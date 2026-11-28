//! Lossless audio to an AV receiver: DTS, DTS-HD, DTS:X and Dolby TrueHD / Atmos, which the TV cannot decode, sent on
//! unconverted through LG's own Dolby Digital Plus pass-through route.
//!
//! Ported from Native Jelly's `rust-modules/src/player/es_carrier.rs` and `src/es_swap.c` (GPL-3.0, GLinnik21's
//! PLX Native and Native Jelly's pull request 46), where it was measured on an LG C1 with a Sony STR-DN1080. The
//! carrier is theirs, and so are the burst layouts; what is ours is the way it is wired into this player.
//!
//! The C1 has no DTS path, but it does pass DD+ through to an eARC receiver: LG's audio libraries run inside this
//! process, and liblxa packs each E-AC-3 frame into an IEC 61937 burst (24576 bytes at the 192 kHz stereo carrier)
//! that it writes into LG's ES renderer with `ioctl(fd, 0x40285207, arg)` (word 0 = buffer, word 2 = size, word 8 = 1
//! for the ES renderer). The executable exports `ioctl` (`.cargo/config.toml`), so that call lands in [`ioctl`]
//! below, which hands such a write to [`es_swap`].
//!
//! The player declares the title `AC3 PLUS`; the demuxer feeds one silent E-AC-3 frame per 32 ms on contiguous
//! timestamps and queues the real track for the same 32 ms: three DTS-HD frames (3 x 512 samples) as IEC 61937 type
//! IV bursts ([`Packer`]), or TrueHD as MAT bursts cut into carrier chunks ([`Mat`]). [`es_swap`] points each LG write
//! at the next queued chunk, so the receiver gets the bitstream while LG keeps timing, A/V sync, pause and seek as
//! for a DD+ title. With the HBR lease ([`set_on`]) LG runs the link at 8 channels x 192 kHz and every chunk is four
//! times larger, which TrueHD and full-rate DTS-HD need (`hbr/`, the earc_route kernel module).
//!
//! The older route, [`dts_core_packet`] + [`dts_iec61937`], sends the DTS core alone as IEC 61937 bursts declared
//! `PCM`, which any receiver on optical or ARC takes.
#![cfg_attr(not(target_arch = "arm"), allow(dead_code))] // the ES hook is ARM only

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// LG's DD+ burst on the 192 kHz stereo carrier: 1536 samples x 4 (rate) x 4 bytes.
pub const CHUNK: usize = 24576;
/// One DTS-HD frame's burst at the same carrier: 512 samples x 4 x 4 bytes (IEC 61937 type IV,
/// repetition period 2048, subtype 2).
const BURST: usize = 8192;
/// HBR (8 ch x 192 kHz, 4x the stereo carrier): a 32768-byte burst per DTS-HD frame (repetition
/// period 8192, subtype 4) and 98304 bytes per LG write. Needs the earc_route kernel module
/// (the earc_route release in `hbr/`), whose hooks widen LG's eARC link to 8 channels while this process holds
/// its lease.
const HBR: usize = 4;
/// A silent 48 kHz stereo E-AC-3 frame (1536 samples, 96 kb/s), made with
/// `ffmpeg -f lavfi -i anullsrc=r=48000:cl=stereo -c:a eac3 -b:a 96k -f eac3`; every frame of
/// that encode is identical, so one frame repeats for the whole title.
pub static SILENT_EAC3: &[u8] = include_bytes!("silent-eac3.bin");

static ON: AtomicBool = AtomicBool::new(false);
/// Bursts and writes are HBR-sized (x [`HBR`]).
static WIDE: AtomicBool = AtomicBool::new(false);
/// `/dev/earc_route`, open while HBR is on: writing "2" leases eARC to renderer index 1 (LG's main
/// decoder for this app); closing the file releases it.
static LEASE: Mutex<Option<std::fs::File>> = Mutex::new(None);
/// One queued chunk: `(offset, bytes)` segments, every other byte of the chunk is zero. Stored
/// without the zero padding (a DTS-HD MA chunk carries about 7.7 KB, a TrueHD one at 5 Mb/s about
/// 20 KB), so two minutes of read-ahead stay in the tens of MB.
type Chunk = Vec<(u32, Vec<u8>)>;
static QUEUE: Mutex<VecDeque<Chunk>> = Mutex::new(VecDeque::new());
/// The chunk LG's current write points at: kept alive until the next write replaces it.
static CURRENT: Mutex<Option<Box<[u8]>>> = Mutex::new(None);
static DROPPED: AtomicU32 = AtomicU32::new(0);
static SWAPPED: AtomicU32 = AtomicU32::new(0);
static MISSED: AtomicU32 = AtomicU32::new(0);
static OTHER: AtomicU32 = AtomicU32::new(0);
/// The renderer refused the last ES write (`-1`, buffer full); liblxa retries the same write, which
/// must carry the same chunk again rather than the next one.
static LAST_FAILED: AtomicBool = AtomicBool::new(false);
static RETRIED: AtomicU32 = AtomicU32::new(0);
static CORE_ONLY: AtomicU32 = AtomicU32::new(0);

/// The DD+ route never reached LG's ES renderer in this app run (see [`silent_route`]): the
/// carrier stays off until the app restarts, and DTS/TrueHD use the PCM core and the AC-3 converter.
static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// May this playback use the carrier at all?
pub fn available() -> bool {
    !UNAVAILABLE.load(Ordering::Acquire)
}

/// Is the carrier on for the playback being started?
#[cfg(test)]
pub fn on() -> bool {
    ON.load(Ordering::Acquire)
}

/// HBR is possible when the HBR support Native Jelly installs on rooted TVs has put a valid
/// `earc_route` lease node in this app's sandbox.
pub fn hbr_available() -> bool {
    lease_node_matches().is_ok()
}

/// Pictures to wait for before deciding LG's DD+ route is not in pass-through (about 3 s at 24 fps;
/// the first ES write comes with the first audio, well before that).
const SILENT_ROUTE_FRAMES: i32 = 75;

/// Called every pump tick with the pictures shown so far: `true` once when the carrier is on, the
/// title has shown [`SILENT_ROUTE_FRAMES`] pictures, and LG has made no ES write at all. LG then
/// plays our silent E-AC-3 itself (TV speakers, or a receiver without DD+ on its eARC/ARC), so the
/// caller reloads at the same position and the carrier stays off for this app run.
pub fn silent_route(frames: i32) -> bool {
    if !ON.load(Ordering::Acquire) || frames < SILENT_ROUTE_FRAMES {
        return false;
    }
    let writes = SWAPPED.load(Ordering::Relaxed) + MISSED.load(Ordering::Relaxed) + RETRIED.load(Ordering::Relaxed);
    if writes > 0 || UNAVAILABLE.swap(true, Ordering::AcqRel) {
        return false;
    }
    set_on(false);
    true
}

/// The app sandbox has its own `/dev`; root-lg adds an `earc_route` node there, but the module's
/// misc minor changes with every load, so a stale node could name another device. Only a node
/// whose number the kernel names `earc_route` (`/sys/dev/char/<major>:<minor>/uevent`, which the
/// sandbox can read) is opened.
fn lease_node_matches() -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let rdev = std::fs::metadata("/dev/earc_route")?.rdev();
    let (major, minor) = ((rdev >> 8) & 0xfff, (rdev & 0xff) | ((rdev >> 12) & 0xfff00));
    let uevent = std::fs::read_to_string(format!("/sys/dev/char/{major}:{minor}/uevent"))?;
    if uevent.lines().any(|l| l == "DEVNAME=earc_route") {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "/dev/earc_route ({major}:{minor}) is not the earc_route device"
        )))
    }
}

/// Start or stop swapping LG's ES writes; both clear the queue and the counters. With HBR
/// requested, takes the earc_route lease BEFORE the Load (LG configures eARC when it connects);
/// if the lease cannot be taken, stays on the 192 kHz stereo carrier. Returns whether HBR is on.
pub fn set_on(on: bool) -> bool {
    use std::io::Write as _;
    clear();
    for c in [&SWAPPED, &MISSED, &OTHER, &CORE_ONLY, &DROPPED, &RETRIED] {
        LAST_FAILED.store(false, Ordering::Relaxed);
        c.store(0, Ordering::Relaxed);
    }
    let mut lease = LEASE.lock().unwrap_or_else(|e| e.into_inner());
    *lease = None;
    let mut wide = false;
    if on && hbr_available() {
        match lease_node_matches()
            .and_then(|()| std::fs::OpenOptions::new().write(true).open("/dev/earc_route"))
            .and_then(|mut f| {
                f.write_all(b"2")?;
                Ok(f)
            }) {
            Ok(f) => {
                *lease = Some(f);
                wide = true;
                crate::sdl::log("es_carrier: HBR on (earc_route lease for index 1)");
            }
            Err(e) => crate::sdl::log(&format!("es_carrier: HBR lease failed ({e}); 192 kHz stereo carrier")),
        }
    }
    WIDE.store(wide, Ordering::Release);
    ON.store(on, Ordering::Release);
    wide
}

/// Drop everything queued (a seek: the bursts belong to a position that no longer exists).
pub fn clear() {
    if let Ok(mut q) = QUEUE.lock() {
        q.clear();
    }
}

fn push(chunk: Chunk) {
    if let Ok(mut q) = QUEUE.lock() {
        // ponytail: a fixed cap instead of PTS matching. The demuxer reads 10-20 s ahead of the
        // playhead, so the cap must cover that (4096 chunks = 131 s); a drop skips audio LG has not
        // played and the receiver loses lock. Upgrade path: key chunks by the write's timestamp
        // (words 4-5, ns) and bound the queue by the demuxer's own audio lane.
        if q.len() >= 4096 {
            q.pop_front();
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(chunk);
    }
}

/// Called from [`ioctl`] for every write to LG's ES renderer (`w` is the ioctl argument).
pub fn es_swap(w: *mut u32) {
    if w.is_null() || !ON.load(Ordering::Acquire) {
        return;
    }
    let scale = if WIDE.load(Ordering::Acquire) { HBR } else { 1 };
    if LAST_FAILED.swap(false, Ordering::AcqRel) {
        // A retry of a refused write: send the chunk it carried, not the next one.
        if let Ok(current) = CURRENT.lock() {
            if let Some(buf) = current.as_deref() {
                // SAFETY: as below; CURRENT still owns the buffer.
                unsafe {
                    *w = buf.as_ptr() as usize as u32;
                    *w.add(2) = buf.len() as u32;
                }
                RETRIED.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }
    // SAFETY: the C side passes libkadaptor's 40-byte renderer-write argument, words 0..9.
    let size = unsafe { *w.add(2) } as usize;
    if size != CHUNK {
        OTHER.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let next = QUEUE.lock().ok().and_then(|mut q| q.pop_front());
    let Some(chunk) = next else {
        MISSED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let Ok(mut current) = CURRENT.lock() else { return };
    if current.as_ref().is_some_and(|b| b.len() != CHUNK * scale) {
        *current = None;
    }
    let buf = current.get_or_insert_with(|| vec![0u8; CHUNK * scale].into_boxed_slice());
    buf.fill(0);
    for (at, part) in &chunk {
        let at = *at as usize;
        if at + part.len() <= buf.len() {
            buf[at..at + part.len()].copy_from_slice(part);
        }
    }
    // SAFETY: 32-bit ARM, so the buffer address fits word 0; CURRENT keeps the buffer until the
    // next write, i.e. past the real ioctl that copies it (writes come from one liblxa thread).
    // On HBR the write grows 4x; LG's ES renderer takes it (measured with the shim on the C1).
    unsafe {
        *w = buf.as_ptr() as usize as u32;
        *w.add(2) = (CHUNK * scale) as u32;
    }
    let n = SWAPPED.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 || n.is_multiple_of(250) {
        crate::sdl::log(&format!(
            "es_carrier: {n} DD+ bursts replaced (queued {}, dropped {}, missed {}, retried {}, other sizes {}, core-only frames {})",
            QUEUE.lock().map_or(0, |q| q.len()),
            DROPPED.load(Ordering::Relaxed),
            MISSED.load(Ordering::Relaxed),
            RETRIED.load(Ordering::Relaxed),
            OTHER.load(Ordering::Relaxed),
            CORE_ONLY.load(Ordering::Relaxed)
        ));
    }
}

/// Called after the real ioctl of every ES-renderer write. A refused write (`-1`: the renderer's
/// buffer is full) is retried by liblxa with the same argument block; [`es_swap`] then resends
/// the same chunk. Measured on HBR, where about 0.6 retries follow each replaced write.
pub fn es_swap_done(w: *const u32, ret: i32) {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    if w.is_null() || !ON.load(Ordering::Acquire) {
        return;
    }
    if ret < 0 {
        LAST_FAILED.store(true, Ordering::Release);
    }
    // SAFETY: the same 40-byte argument the C side passed to `es_swap`.
    let size = unsafe { *w.add(2) } as usize;
    if size != CHUNK && size != CHUNK * HBR && SEEN.fetch_add(1, Ordering::Relaxed) < 10 {
        crate::sdl::log(&format!("es_carrier: unexpected ES write size {size} ret {ret}"));
    }
}

/// One DTS-HD frame as an IEC 61937 type IV burst for the 192 kHz stereo carrier, big-endian, as
/// FFmpeg's spdif muxer writes it with `-dtshd_rate 192000 -spdif_flags be`. A frame too large for
/// the burst sends its core alone (`core_len`), for this frame only. `None` when not even the core
/// fits.
fn dtshd_burst(frame: &[u8], core_len: usize, wide: bool) -> Option<Vec<u8>> {
    const START: [u8; 10] = [0x01, 0, 0, 0, 0, 0, 0, 0, 0xfe, 0xfe];
    let period = if wide { BURST * HBR } else { BURST };
    let fits = |n: usize| START.len() + 2 + n <= period - 8;
    let payload = if fits(frame.len()) {
        frame
    } else if core_len > 0 && core_len <= frame.len() && fits(core_len) {
        CORE_ONLY.fetch_add(1, Ordering::Relaxed);
        &frame[..core_len]
    } else {
        return None;
    };
    let out_bytes = START.len() + 2 + payload.len();
    let length_code = (out_bytes + 8).div_ceil(16) * 16 - 8;
    let mut out = vec![0u8; period];
    out[..4].copy_from_slice(&[0xf8, 0x72, 0x4e, 0x1f]);
    out[4..6].copy_from_slice(&(if wide { 0x0411u16 } else { 0x0211 }).to_be_bytes());
    out[6..8].copy_from_slice(&(length_code as u16).to_be_bytes());
    out[8..18].copy_from_slice(&START);
    out[18..20].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    out[20..20 + payload.len()].copy_from_slice(payload);
    Some(out)
}

/// Groups DTS-HD frames three at a time into one queued chunk; the demuxer feeds a silent E-AC-3
/// frame with the first frame's timestamp whenever [`Packer::frame`] answers `Some`.
/// One carrier frame: 1536 samples at 48 kHz.
const FRAME_NS: i64 = 32_000_000;
/// A source timestamp further than this from the contiguous one restarts the carrier timeline.
const REANCHOR_NS: i64 = 100_000_000;

#[derive(Default)]
pub struct Packer {
    acc: Vec<Vec<u8>>,
    first_pts: i64,
    /// The timestamp the next carrier frame gets when the source agrees within [`REANCHOR_NS`].
    /// Contiguous carrier timestamps matter: LG schedules each 32 ms E-AC-3 frame at its PTS, and a
    /// gap between frames (container rounding, or a file whose timestamps are stretched) becomes a
    /// gap in the IEC 61937 stream, where the receiver drops the DTS-HD lock.
    next_pts: Option<i64>,
}

impl Packer {
    /// Add one 512-sample DTS-HD frame. `Err` when it cannot be packed at all.
    pub fn frame(&mut self, frame: &[u8], core_len: usize, pts: i64) -> Result<Option<i64>, ()> {
        let mut burst = dtshd_burst(frame, core_len, WIDE.load(Ordering::Acquire)).ok_or(())?;
        let used = 20 + usize::from(u16::from_be_bytes([burst[18], burst[19]]));
        burst.truncate(used);
        if self.acc.is_empty() {
            self.first_pts = pts;
        }
        self.acc.push(burst);
        if self.acc.len() < 3 {
            return Ok(None);
        }
        let period = BURST * if WIDE.load(Ordering::Acquire) { HBR } else { 1 };
        push(
            std::mem::take(&mut self.acc)
                .into_iter()
                .enumerate()
                .map(|(i, b)| ((i * period) as u32, b))
                .collect(),
        );
        let pts = match self.next_pts {
            Some(next) if (self.first_pts - next).abs() <= REANCHOR_NS => next,
            _ => self.first_pts,
        };
        self.next_pts = Some(pts + FRAME_NS);
        Ok(Some(pts))
    }
}

/// A continuous IEC 61937 byte stream cut into carrier chunks (HBR size) as it is written: TrueHD's
/// 61440-byte MAT bursts last 20 ms and do not line up with the 32 ms carrier frames, and the
/// receiver only sees the concatenation (the command-line test fed exactly that and showed TrueHD).
#[derive(Default)]
struct Stream {
    chunk: Chunk,
    filled: usize,
    done: usize,
}

impl Stream {
    const SIZE: usize = CHUNK * HBR;

    fn bytes(&mut self, mut b: &[u8]) {
        while !b.is_empty() {
            let n = (Self::SIZE - self.filled).min(b.len());
            match self.chunk.last_mut() {
                Some((at, v)) if *at as usize + v.len() == self.filled => v.extend_from_slice(&b[..n]),
                _ => self.chunk.push((self.filled as u32, b[..n].to_vec())),
            }
            self.filled += n;
            b = &b[n..];
            self.roll();
        }
    }

    fn zeros(&mut self, mut n: usize) {
        while n > 0 {
            let k = (Self::SIZE - self.filled).min(n);
            self.filled += k;
            n -= k;
            self.roll();
        }
    }

    /// Bytes with long zero runs left out of the stored segments.
    fn sparse(&mut self, b: &[u8]) {
        let mut i = 0;
        while i < b.len() {
            let z = b[i..].iter().take_while(|x| **x == 0).count();
            if z >= 64 || i + z == b.len() {
                self.zeros(z);
                i += z;
                continue;
            }
            let start = i;
            i += z;
            while i < b.len() && !b[i..].iter().take(64).all(|x| *x == 0) {
                i += 1;
            }
            self.bytes(&b[start..i]);
        }
    }

    fn roll(&mut self) {
        if self.filled == Self::SIZE {
            push(std::mem::take(&mut self.chunk));
            self.filled = 0;
            self.done += 1;
        }
    }
}

const MAT_FRAME: usize = 61424;
const MAT_PKT: usize = 61440;
const MAT_START: [u8; 20] = [
    0x07, 0x9E, 0x00, 0x03, 0x84, 0x01, 0x01, 0x01, 0x80, 0x00, 0x56, 0xA5, 0x3B, 0xF4, 0x81, 0x83, 0x49, 0x80, 0x77,
    0xE0,
];
const MAT_MIDDLE: [u8; 12] = [0xC3, 0xC1, 0x42, 0x49, 0x3B, 0xFA, 0x82, 0x83, 0x49, 0x80, 0x77, 0xE0];
const MAT_END: [u8; 16] = [0xC3, 0xC2, 0xC0, 0xC4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x97, 0x11];
const MAT_CODES: [(usize, &[u8]); 3] = [
    (0, &MAT_START),
    (30708, &MAT_MIDDLE),
    (MAT_FRAME - MAT_END.len(), &MAT_END),
];

/// TrueHD access units into MAT frames and IEC 61937 type 22 bursts (big-endian, HBR), a port of
/// FFmpeg 9.0's `spdif_header_truehd`: each unit is padded to its nominal 2560-byte slot from its
/// `input_timing`, with the MAT start/middle/end codes at fixed offsets. The bursts go into a
/// [`Stream`]; [`Mat::au`] answers the carrier timestamps of the chunks it completed.
#[derive(Default)]
pub struct Mat {
    buf: Vec<u8>,
    filled: usize,
    prev_time: u16,
    prev_size: usize,
    spf: usize,
    stream: Stream,
    anchor: Option<i64>,
}

impl Mat {
    pub fn au(&mut self, pkt: &[u8], pts: i64) -> Result<Vec<i64>, ()> {
        if pkt.len() < 10 {
            return Err(());
        }
        if self.buf.is_empty() {
            self.buf = vec![0; MAT_FRAME];
        }
        if pkt[4..7] == [0xf8, 0x72, 0x6f] {
            let ratebits = match pkt[7] {
                0xba => pkt[8] >> 4,
                0xbb => pkt[9] >> 4,
                _ => return Err(()),
            };
            self.spf = 40 << (ratebits & 3);
        }
        if self.spf == 0 {
            return Err(());
        }
        let anchor = *self.anchor.get_or_insert(pts);
        let input_timing = u16::from_be_bytes([pkt[2], pkt[3]]);
        let mut padding = 0usize;
        if self.prev_size > 0 {
            let delta_bytes = usize::from(input_timing.wrapping_sub(self.prev_time)) * 2560 / self.spf;
            padding = match delta_bytes.checked_sub(self.prev_size) {
                Some(p) if p < MAT_FRAME / 2 => p,
                _ => 0,
            };
        }
        let mut total = pkt.len();
        let mut next = MAT_CODES.iter().position(|c| self.filled <= c.0).ok_or(())?;
        let mut data = pkt;
        let before = self.stream.done;
        while padding > 0 || !data.is_empty() || MAT_CODES[next].0 == self.filled {
            if MAT_CODES[next].0 == self.filled {
                let code = MAT_CODES[next].1;
                self.buf[self.filled..self.filled + code.len()].copy_from_slice(code);
                self.filled += code.len();
                let mut code_left = code.len();
                next += 1;
                if next == MAT_CODES.len() {
                    next = 0;
                    self.stream.bytes(&[0xf8, 0x72, 0x4e, 0x1f, 0x00, 0x16, 0xef, 0xf0]);
                    let frame = std::mem::take(&mut self.buf);
                    self.stream.sparse(&frame);
                    self.buf = frame;
                    self.stream.zeros(MAT_PKT - 8 - MAT_FRAME);
                    self.filled = 0;
                    code_left += MAT_PKT - MAT_FRAME;
                }
                if padding > 0 {
                    let c = padding.min(code_left);
                    padding -= c;
                    code_left -= c;
                }
                total += code_left;
            }
            if padding > 0 {
                let n = (MAT_CODES[next].0 - self.filled).min(padding);
                self.buf[self.filled..self.filled + n].fill(0);
                self.filled += n;
                padding -= n;
                if padding > 0 {
                    continue;
                }
            }
            if !data.is_empty() {
                let n = (MAT_CODES[next].0 - self.filled).min(data.len());
                self.buf[self.filled..self.filled + n].copy_from_slice(&data[..n]);
                self.filled += n;
                data = &data[n..];
            }
        }
        self.prev_size = total;
        self.prev_time = input_timing;
        Ok((before..self.stream.done)
            .map(|k| anchor + k as i64 * FRAME_NS)
            .collect())
    }
}

// ---- the DTS core as PCM ------------------------------------------------------------------------------------------

/// Extracts one DTS core access unit, dropping the DTS-HD extension substream. The core size is the 14-bit FSIZE + 1
/// from its normalised header (ETSI TS 102 114). The four core sync formats are normalised to 16-bit big endian before
/// measuring. Extension-only, truncated and malformed packets are rejected instead of sending HD data to the core
/// decoder.
pub fn dts_core_packet(packet: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    let sync: [u8; 4] = packet.get(..4)?.try_into().ok()?;
    let normalized = match sync {
        [0x7f, 0xfe, 0x80, 0x01] => Cow::Borrowed(packet),
        [0xfe, 0x7f, 0x01, 0x80] => {
            let mut bytes = packet.to_vec();
            for word in bytes.as_chunks_mut::<2>().0 {
                word.swap(0, 1);
            }
            Cow::Owned(bytes)
        }
        [0x1f, 0xff, 0xe8, 0x00] | [0xff, 0x1f, 0x00, 0xe8] => {
            let little = sync[0] == 0xff;
            let mut bytes = Vec::with_capacity(packet.len() * 7 / 8);
            let (mut bits, mut count) = (0u32, 0u32);
            for word in packet.as_chunks::<2>().0 {
                let word = if little {
                    u16::from_le_bytes([word[0], word[1]])
                } else {
                    u16::from_be_bytes([word[0], word[1]])
                };
                bits = (bits << 14) | u32::from(word & 0x3fff);
                count += 14;
                while count >= 8 {
                    count -= 8;
                    bytes.push((bits >> count) as u8);
                }
            }
            Cow::Owned(bytes)
        }
        _ => return None,
    };
    let header = normalized.get(..10)?;
    let size =
        (((usize::from(header[5]) & 3) << 12) | (usize::from(header[6]) << 4) | (usize::from(header[7]) >> 4)) + 1;
    // The standard's minimum core frame length is 96 bytes.
    if size < 96 || size > normalized.len() {
        return None;
    }
    Some(match normalized {
        Cow::Borrowed(bytes) => Cow::Borrowed(&bytes[..size]),
        Cow::Owned(mut bytes) => {
            bytes.truncate(size);
            Cow::Owned(bytes)
        }
    })
}

/// One 48 kHz DTS core frame as an IEC 61937 burst, little-endian S16 stereo: the bytes FFmpeg's spdif muxer writes. A
/// receiver on optical or ARC sees the original DTS bitstream; nothing is decoded. `None` when the core is not 48 kHz,
/// has an unusual block count, or does not fit its period (a 1536 kb/s core needs 2044 of 2040 bytes).
pub fn dts_iec61937(core: &[u8]) -> Option<Vec<u8>> {
    if core.len() < 9 || core[..4] != [0x7f, 0xfe, 0x80, 0x01] || (core[8] >> 2) & 0x0f != 13 {
        return None;
    }
    let blocks = ((usize::from(core[4]) << 8 | usize::from(core[5])) >> 2 & 0x7f) + 1;
    let kind: u16 = match blocks {
        16 => 0x0b,
        32 => 0x0c,
        64 => 0x0d,
        _ => return None,
    };
    let period = blocks << 7;
    let len = core.len();
    if len + (len & 1) > period - 8 {
        return None;
    }
    let mut out = vec![0u8; period];
    out[..2].copy_from_slice(&0xf872u16.to_le_bytes());
    out[2..4].copy_from_slice(&0x4e1fu16.to_le_bytes());
    out[4..6].copy_from_slice(&kind.to_le_bytes());
    out[6..8].copy_from_slice(&(((len + 1) & !1) as u16 * 8).to_le_bytes());
    for (i, pair) in core.chunks(2).enumerate() {
        out[8 + 2 * i] = pair.get(1).copied().unwrap_or(0);
        out[9 + 2 * i] = pair[0];
    }
    Some(out)
}

// ---- which route a title takes ------------------------------------------------------------------------------------

/// How a title's audio goes to the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Not at all: the audio is sent or converted as for any film.
    Off,
    /// DTS, DTS-HD and DTS:X as type IV bursts on the DD+ carrier.
    Dts,
    /// The DTS core as IEC 61937 in a stereo PCM stream.
    DtsCore,
    /// TrueHD (Atmos) as MAT bursts on the HBR carrier.
    TrueHd,
}

/// What a track is, as far as this module cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Other,
    Dts,
    TrueHd,
}

static SETTING: AtomicBool = AtomicBool::new(false);
static RECEIVER: AtomicBool = AtomicBool::new(false);

/// Sets the owner's switch, and reads the TV's sound output now: the audio only goes unconverted to ARC or optical.
/// TV speakers, headphones and Bluetooth would play the bursts as full-scale noise, so nothing counts as a receiver
/// until the TV has said so.
pub fn configure(setting: bool) {
    SETTING.store(setting, Ordering::Release);
    RECEIVER.store(setting && sound_output_is_receiver(), Ordering::Release);
}

fn sound_output_is_receiver() -> bool {
    if std::env::var_os("FILM_RECEIVER").is_some() {
        return true; // a desktop run (tools/film-sim.sh)
    }
    let out = std::process::Command::new("/usr/bin/luna-send")
        .args([
            "-n",
            "1",
            "-w",
            "3000",
            "luna://com.webos.service.audio/getSoundOutput",
            "{}",
        ])
        .stdin(std::process::Stdio::null())
        .output();
    let reply = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let found = receiver_in(&reply);
    crate::sdl::log(&format!(
        "lossless: sound output {}",
        if found {
            "is a receiver"
        } else {
            "is not an ARC or optical receiver"
        }
    ));
    found
}

/// `{"returnValue":true,"soundOutput":"external_arc",...}`: HDMI ARC / eARC or optical, the links that carry an IEC 61937
/// bitstream to an external decoder.
fn receiver_in(reply: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) else {
        return false;
    };
    v["returnValue"] == true && matches!(v["soundOutput"].as_str(), Some("external_arc" | "external_optical"))
}

/// Whether a DTS track can be played at all (it is not decoded by the TV, so otherwise the server converts the film).
pub fn dts_possible() -> bool {
    RECEIVER.load(Ordering::Acquire)
}

/// The route for a track, before the run starts. `Dts` and `TrueHd` take the carrier (and, with the HBR lease, widen it),
/// which switches the swapping on: call it before the pipeline loads, and drop the guard when the run ends.
pub fn arm(kind: Kind) -> (Mode, Armed) {
    let receiver = RECEIVER.load(Ordering::Acquire);
    let mode = match kind {
        Kind::Dts if receiver && available() => Mode::Dts,
        Kind::Dts if receiver => Mode::DtsCore,
        Kind::TrueHd if receiver && available() && hbr_available() => Mode::TrueHd,
        _ => Mode::Off,
    };
    let wide = matches!(mode, Mode::Dts | Mode::TrueHd) && set_on(true);
    if mode == Mode::TrueHd && !wide {
        // The lease failed: MAT does not fit the 192 kHz stereo link.
        set_on(false);
        return (Mode::Off, Armed);
    }
    if mode != Mode::Off {
        crate::sdl::log(&format!(
            "lossless: {mode:?}{}",
            if wide { " on the HBR carrier" } else { "" }
        ));
    }
    (mode, Armed)
}

/// Releases the lease and stops swapping when the run ends.
pub struct Armed;

impl Drop for Armed {
    fn drop(&mut self) {
        set_on(false);
    }
}

/// `hbr.sh status` says whether the kernel module that HBR needs is installed and active (see `hbr/hbr.sh`).
pub fn hbr_status(dir: &str) -> String {
    std::process::Command::new("/bin/sh")
        .arg(format!("{dir}/hbr/hbr.sh"))
        .arg("status")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ---- the ES renderer hook -----------------------------------------------------------------------------------------

/// Exported from the executable (`--export-dynamic-symbol=ioctl` in `.cargo/config.toml`), so that LG's libkadaptor, which
/// writes its bursts into the ES renderer with `ioctl(fd, 0x40285207, arg)`, binds here. Every call is forwarded with the
/// raw syscall; an ES-renderer write (word 8 == 1) is first offered to [`es_swap`], which may point word 0 at a queued
/// burst. Declared with three fixed arguments: on 32-bit ARM a variadic third argument travels as a fixed one.
#[cfg(all(target_arch = "arm", target_os = "linux"))]
#[no_mangle]
pub unsafe extern "C" fn ioctl(
    fd: std::ffi::c_int,
    req: std::ffi::c_ulong,
    arg: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    let es = req == 0x4028_5207 && !arg.is_null() && unsafe { *arg.cast::<u32>().add(8) } == 1;
    if es {
        es_swap(arg.cast());
    }
    let ret = unsafe { libc::syscall(libc::SYS_ioctl, fd, req, arg) } as std::ffi::c_int;
    if es {
        es_swap_done(arg.cast(), ret);
    }
    ret
}

#[cfg(test)]
mod tests {
    /// The carrier is process-global state (one LG ES renderer per process); tests that touch it
    /// take this lock so cargo's parallel runner cannot interleave them.
    static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn globals() -> std::sync::MutexGuard<'static, ()> {
        GLOBALS.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn burst_matches_ffmpeg_spdif_dtshd_192k_be() {
        // First burst of `ffmpeg -i dtshd.mkv -c copy -f spdif -spdif_flags be -dtshd_rate 192000`
        // on a 2568-byte DTS-HD MA frame: header, start code, size, then the frame verbatim.
        let mut frame = vec![0x5a; 2568];
        frame[..4].copy_from_slice(&[0x7f, 0xfe, 0x80, 0x01]);
        let out = super::dtshd_burst(&frame, 1000, false).unwrap();
        assert_eq!(out.len(), 8192);
        assert_eq!(
            out[..20],
            [0xf8, 0x72, 0x4e, 0x1f, 0x02, 0x11, 0x0a, 0x18, 1, 0, 0, 0, 0, 0, 0, 0, 0xfe, 0xfe, 0x0a, 0x08]
        );
        assert_eq!(out[20..20 + 2568], frame[..]);
        assert!(out[20 + 2568..].iter().all(|b| *b == 0));
        // 2560 bytes: the same length code (rounded so the low nibble is 8), size 0x0a00.
        let out = super::dtshd_burst(&frame[..2560], 1000, false).unwrap();
        assert_eq!(out[6..8], [0x0a, 0x18]);
        assert_eq!(out[18..20], [0x0a, 0x00]);
    }

    #[test]
    fn oversized_frame_sends_core_and_unfit_core_fails() {
        let _g = globals();
        let frame = vec![0u8; 9000];
        let out = super::dtshd_burst(&frame, 2000, false).unwrap();
        assert_eq!(out[18..20], 2000u16.to_be_bytes());
        assert!(super::dtshd_burst(&frame, 8180, false).is_none());
        // HBR: the whole 9000-byte frame fits a 32768-byte type IV burst, subtype 4.
        let out = super::dtshd_burst(&frame, 2000, true).unwrap();
        assert_eq!(
            (out.len(), out[4..6].to_vec(), out[18..20].to_vec()),
            (32768, vec![0x04, 0x11], 9000u16.to_be_bytes().to_vec())
        );
    }

    #[test]
    fn swap_rebuilds_the_padded_chunk_and_points_the_write_at_it() {
        let _g = globals();
        super::set_on(true);
        let mut p = super::Packer::default();
        let frame = vec![7u8; 300];
        for t in 0..3 {
            p.frame(&frame, 0, t).unwrap();
        }
        let mut w = [0u32; 10];
        w[2] = super::CHUNK as u32;
        w[8] = 1;
        super::es_swap(w.as_mut_ptr());
        // SAFETY: the test reads the buffer CURRENT keeps alive (32-bit address in word 0 on the
        // TV; on a 64-bit host the low half is not a pointer, so compare through CURRENT instead).
        let cur = super::CURRENT.lock().unwrap();
        let buf = cur.as_deref().unwrap();
        for i in 0..3 {
            let b = &buf[i * super::BURST..(i + 1) * super::BURST];
            assert_eq!(b[..4], [0xf8, 0x72, 0x4e, 0x1f]);
            assert_eq!(b[20..320], frame[..]);
            assert!(b[320..].iter().all(|x| *x == 0));
        }
        assert_eq!(super::SWAPPED.load(std::sync::atomic::Ordering::Relaxed), 1);
        drop(cur);
        super::set_on(false);
    }

    #[test]
    fn a_refused_write_is_retried_with_the_same_chunk() {
        let _g = globals();
        super::set_on(true);
        let mut p = super::Packer::default();
        for t in 0..6 {
            p.frame(&[t as u8; 50], 0, t).unwrap();
        }
        let mut w = [0u32; 10];
        w[2] = super::CHUNK as u32;
        w[8] = 1;
        super::es_swap(w.as_mut_ptr());
        let first = super::CURRENT.lock().unwrap().as_deref().unwrap()[20];
        super::es_swap_done(w.as_ptr(), -1);
        // liblxa retries with the size it was given; the queue must not advance.
        super::es_swap(w.as_mut_ptr());
        assert_eq!(super::CURRENT.lock().unwrap().as_deref().unwrap()[20], first);
        super::es_swap_done(w.as_ptr(), 0);
        w[2] = super::CHUNK as u32;
        super::es_swap(w.as_mut_ptr());
        assert_ne!(super::CURRENT.lock().unwrap().as_deref().unwrap()[20], first);
        super::set_on(false);
    }

    #[test]
    fn a_route_lg_never_writes_to_turns_the_carrier_off_once() {
        let _g = globals();
        super::UNAVAILABLE.store(false, std::sync::atomic::Ordering::Relaxed);
        super::set_on(true);
        assert!(!super::silent_route(10), "too early to judge");
        assert!(super::silent_route(super::SILENT_ROUTE_FRAMES));
        assert!(!super::on() && !super::available());
        assert!(!super::silent_route(500), "fires once");
        // A route that does write is never judged silent.
        super::UNAVAILABLE.store(false, std::sync::atomic::Ordering::Relaxed);
        super::set_on(true);
        super::SWAPPED.store(1, std::sync::atomic::Ordering::Relaxed);
        assert!(!super::silent_route(500));
        super::set_on(false);
    }

    #[test]
    fn packer_emits_one_chunk_per_three_frames() {
        let _g = globals();
        super::clear();
        let mut p = super::Packer::default();
        let frame = vec![1u8; 100];
        assert_eq!(p.frame(&frame, 0, 10), Ok(None));
        assert_eq!(p.frame(&frame, 0, 20), Ok(None));
        assert_eq!(p.frame(&frame, 0, 30), Ok(Some(10)));
        // Stretched or rounded source timestamps: the carrier stays contiguous...
        for t in [44_000_000, 55_000_000, 66_000_000] {
            p.frame(&frame, 0, t).ok();
        }
        assert_eq!(p.frame(&frame, 0, 0).ok(), Some(None));
        assert_eq!(p.frame(&frame, 0, 0).ok(), Some(None));
        assert_eq!(p.frame(&frame, 0, 0).ok(), Some(Some(32_000_010 + 32_000_000)));
        // ...until the source jumps (a seek or a discontinuity).
        for t in [900_000_000, 910_000_000] {
            p.frame(&frame, 0, t).ok();
        }
        assert_eq!(p.frame(&frame, 0, 920_000_000), Ok(Some(900_000_000)));
        let q = super::QUEUE.lock().unwrap();
        assert_eq!(
            q.back().map(|c| c.iter().map(|(_, v)| v.len()).sum::<usize>()),
            Some(3 * (20 + 100))
        );
    }

    fn render(chunk: &super::Chunk, size: usize) -> Vec<u8> {
        let mut out = vec![0u8; size];
        for (at, v) in chunk {
            out[*at as usize..*at as usize + v.len()].copy_from_slice(v);
        }
        out
    }

    /// Byte-for-byte against FFmpeg 9.0.2: `ffmpeg -i x.mkv -map 0:a -c copy -f truehd x.thd` and
    /// `ffmpeg -i x.thd -c copy -f spdif -spdif_flags be x.spdif` (root-lg dts-hd-research).
    #[test]
    #[ignore = "needs /tmp/thd-ref.thd and /tmp/thd-ref.spdif"]
    fn mat_matches_ffmpeg_spdif() {
        let _g = globals();
        let thd = std::fs::read("/tmp/thd-ref.thd").unwrap();
        let want = std::fs::read("/tmp/thd-ref.spdif").unwrap();
        super::clear();
        let mut mat = super::Mat::default();
        let (mut i, mut t) = (0, 0i64);
        while i + 4 <= thd.len() && super::QUEUE.lock().unwrap().len() < 40 {
            let n = usize::from(u16::from_be_bytes([thd[i], thd[i + 1]]) & 0xfff) * 2;
            mat.au(&thd[i..i + n], t).unwrap();
            i += n;
            t += 833_333;
        }
        let q = super::QUEUE.lock().unwrap();
        let got: Vec<u8> = q.iter().flat_map(|c| render(c, super::Stream::SIZE)).collect();
        assert_eq!(got.len(), 40 * super::Stream::SIZE);
        let first_diff = got.iter().zip(&want).position(|(a, b)| a != b);
        assert_eq!(first_diff, None, "differs from ffmpeg at byte {first_diff:?}");
    }

    #[test]
    fn mat_chunks_get_contiguous_carrier_timestamps() {
        let _g = globals();
        super::clear();
        let mut mat = super::Mat::default();
        // A major-sync unit (48 kHz: ratebits 0 -> 40 samples per unit) then minor units, each in
        // its 2560-byte slot: 98304 / 2560 = 38.4 units per carrier chunk.
        let mut major = vec![0u8; 64];
        major[4..8].copy_from_slice(&[0xf8, 0x72, 0x6f, 0xba]);
        let mut pts = Vec::new();
        // 130 units: 5 complete MAT frames (24 units each) = 307200 bytes = 3 carrier chunks.
        for k in 0..130u16 {
            let mut au = if k == 0 { major.clone() } else { vec![0x11u8; 64] };
            au[2..4].copy_from_slice(&(k * 40).to_be_bytes());
            pts.extend(mat.au(&au, 5_000 + i64::from(k) * 833_333).unwrap());
        }
        assert_eq!(pts, vec![5_000, 5_000 + 32_000_000, 5_000 + 64_000_000]);
    }

    #[test]
    fn dts_core_burst_matches_ffmpeg_spdif() {
        // 512-sample, 48 kHz core of 1003 bytes (odd length exercises the swap padding).
        let mut core = vec![0u8; 1003];
        core[..4].copy_from_slice(&[0x7f, 0xfe, 0x80, 0x01]);
        core[4] = 0xfc;
        core[5] = 0x3c;
        core[8] = 13 << 2;
        core[1002] = 0xab;
        let out = super::dts_iec61937(&core).unwrap();
        assert_eq!(out.len(), 2048);
        assert_eq!(out[..8], [0x72, 0xf8, 0x1f, 0x4e, 0x0b, 0x00, 0x60, 0x1f]);
        assert_eq!(out[8..12], [0xfe, 0x7f, 0x01, 0x80]);
        assert_eq!((out[8 + 1002], out[9 + 1002]), (0, 0xab));
        assert!(super::dts_iec61937(&vec![0u8; 2041]).is_none());
        let mut khz44 = core.clone();
        khz44[8] = 12 << 2;
        assert!(super::dts_iec61937(&khz44).is_none());
    }

    #[test]
    fn the_core_is_cut_out_of_a_dts_hd_packet() {
        let mut p = vec![0u8; 3000];
        p[..10].copy_from_slice(&[0x7f, 0xfe, 0x80, 0x01, 0xfc, 0x00, 0x07, 0xf0, 0, 0]);
        // FSIZE = (0 << 12 | 0x07 << 4 | 0xf0 >> 4) = 0x7f, so 128 bytes
        assert_eq!(super::dts_core_packet(&p).map(|c| c.len()), Some(128));
        assert!(super::dts_core_packet(&p[..100]).is_none(), "truncated");
        assert!(super::dts_core_packet(&[0u8; 200]).is_none(), "no sync");
    }

    #[test]
    fn only_arc_and_optical_are_a_receiver() {
        assert!(super::receiver_in(
            r#"{"returnValue":true,"soundOutput":"external_arc","lastSoundOutput":"tv_speaker"}"#
        ));
        assert!(super::receiver_in(
            r#"{"returnValue":true,"soundOutput":"external_optical"}"#
        ));
        for no in [
            r#"{"returnValue":true,"soundOutput":"tv_speaker"}"#,
            r#"{"returnValue":true,"soundOutput":"bt_soundbar"}"#,
            r#"{"returnValue":false,"errorText":"Denied method call"}"#,
            r#"{"soundOutput":"external_arc"}"#,
            "not json",
        ] {
            assert!(!super::receiver_in(no), "{no}");
        }
    }

    #[test]
    fn a_track_takes_the_route_its_receiver_allows() {
        let _g = globals();
        super::UNAVAILABLE.store(false, std::sync::atomic::Ordering::Relaxed);
        super::RECEIVER.store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(super::arm(super::Kind::Dts).0, super::Mode::Off, "no receiver");
        super::RECEIVER.store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let (m, _a) = super::arm(super::Kind::Dts);
            assert_eq!(m, super::Mode::Dts);
            assert!(super::on());
        }
        assert!(!super::on(), "the guard stops the swapping");
        assert_eq!(
            super::arm(super::Kind::TrueHd).0,
            super::Mode::Off,
            "no HBR lease node on a desktop"
        );
        assert_eq!(super::arm(super::Kind::Other).0, super::Mode::Off);
        super::UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            super::arm(super::Kind::Dts).0,
            super::Mode::DtsCore,
            "the carrier proved not to work"
        );
        super::UNAVAILABLE.store(false, std::sync::atomic::Ordering::Relaxed);
        super::RECEIVER.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
