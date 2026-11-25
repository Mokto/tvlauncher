//! Direct play: a film file (Matroska or MP4) read in place over HTTP, cut into the access units LG's pipeline takes,
//! with a seek by `av_seek_frame`. The video is H.264 or H.265 turned into Annex-B (the parameter sets in front of
//! each keyframe), AAC gets ADTS headers, AC-3 and E-AC-3 go as they are, and audio the TV cannot decode (TrueHD,
//! FLAC, ...) is re-encoded to AC-3 by `convert`. The framing follows Native Jelly's `rust-modules/src/ff.rs`
//! (GPL-3.0).
use super::convert::{self, Converter};
use super::dolby::{self, Dv7, Sei};
use super::lossless::{self, Kind, Mat, Mode, Packer};
use super::range::{Range, Stop};
use super::*;
use crate::sdl::log;
use crate::ts::{Au, Codec};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The end of a file that played to the end (not an error).
pub const ENDED: &str = "The stream ended.";

// ---- what the file holds ------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Dv {
    pub profile: u8,
    pub compat: u8,
    /// A second (enhancement) layer is present: profile 7 discs.
    pub el: bool,
}

#[derive(Clone, Debug)]
pub struct Video {
    pub codec: Codec,
    pub w: i32,
    pub h: i32,
    pub dv: Option<Dv>,
    /// Why the TV's decoder cannot take this picture (a 10-bit H.264 for one), when it cannot.
    pub refused: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum How {
    /// Sent as it is.
    Direct(Codec),
    /// Decoded here and re-encoded to AC-3.
    Convert,
    /// The TV has no decoder for it.
    No,
}

#[derive(Clone, Debug)]
pub struct ATrack {
    pub stream: i32,
    pub name: String,
    pub how: How,
    pub kind: Kind,
    pub lang: String,
}

impl ATrack {
    /// Whether the TV can play it: as it is, converted, or (DTS) unconverted to a receiver.
    pub fn playable(&self) -> bool {
        self.how != How::No || (self.kind == Kind::Dts && lossless::dts_possible())
    }
}

#[derive(Clone, Debug)]
pub struct STrack {
    pub stream: i32,
    pub name: String,
    pub image: bool,
    /// A script libass draws with its styling (when libass is installed), rather than text.
    pub ass: bool,
    pub lang: String,
    pub forced: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub duration_ns: i64,
    pub video: Option<Video>,
    pub audio: Vec<ATrack>,
    pub subs: Vec<STrack>,
    /// The fonts attached to the file, for ASS.
    pub fonts: Fonts,
}

/// Font files attached to a film: name and data. Shared, since a run's header carries them to the player.
#[derive(Clone, Default)]
pub struct Fonts(pub Arc<Vec<(String, Vec<u8>)>>);

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} fonts", self.0.len())
    }
}

impl Info {
    /// The audio track to start with, or why the file cannot be played as it is (and the server has to convert it).
    /// The track asked for is taken if the TV can play it, else the first it can.
    pub fn plan(&self, want: Option<i64>) -> Result<i32, String> {
        let Some(v) = &self.video else {
            return Err("the file has no H.264 or H.265 picture".into());
        };
        if let Some(r) = &v.refused {
            return Err(r.clone());
        }
        let ok = |a: &&ATrack| a.playable();
        want.and_then(|w| self.audio.iter().filter(ok).find(|a| i64::from(a.stream) == w))
            .or_else(|| self.audio.iter().find(ok))
            .map(|a| a.stream)
            .ok_or_else(|| match self.audio.first() {
                Some(a) => format!("the TV cannot decode the audio ({})", a.name),
                None => "the file has no audio".into(),
            })
    }
}

// ---- what comes out -----------------------------------------------------------------------------------------------

pub struct Bitmap {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

pub enum SubWhat {
    Text {
        text: String,
        top: bool,
    },
    Image {
        canvas: (i32, i32),
        rects: Vec<Bitmap>,
    },
    /// What was on screen goes away.
    Clear,
}

pub struct SubEv {
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub what: SubWhat,
}

pub enum Feed {
    /// What the run carries, before the first unit: the picture, the audio as sent to the TV, Dolby Vision.
    Streams {
        video: Option<Codec>,
        audio: Option<Codec>,
        dv: Option<Dv>,
    },
    Au(Au),
    Sub(SubEv),
    /// An ASS track starts: its header, the file's fonts and the picture's size. Then `AssChunk`s follow.
    AssTrack {
        header: Vec<u8>,
        fonts: Fonts,
        video: (i32, i32),
    },
    /// One packet of the ASS track, times in the file.
    AssChunk {
        data: Vec<u8>,
        start_ms: i64,
        dur_ms: i64,
    },
    /// The time in the file, in ns, that this run's time 0 is: the keyframe the picture starts on.
    Base(i64),
    End(String),
}

/// A queue with a limit in bytes, so that a 4K film cannot fill the TV's memory with what it read ahead.
pub struct Queue {
    m: Mutex<QState>,
    cv: Condvar,
    cap: usize,
}

struct QState {
    items: VecDeque<Feed>,
    bytes: usize,
    closed: bool,
}

fn weight(f: &Feed) -> usize {
    match f {
        Feed::Au(a) => a.data.len() + 64,
        Feed::Sub(_) => 4096,
        Feed::AssChunk { data, .. } => data.len() + 256,
        _ => 0,
    }
}

impl Queue {
    pub fn new(cap: usize) -> Arc<Queue> {
        Arc::new(Queue {
            m: Mutex::new(QState {
                items: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            cv: Condvar::new(),
            cap,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QState> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits while the queue is full. False when it was closed.
    pub fn push(&self, f: Feed) -> bool {
        let w = weight(&f);
        let mut s = self.lock();
        while !s.closed && s.bytes > 0 && s.bytes + w > self.cap {
            s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        if s.closed {
            return false;
        }
        s.bytes += w;
        s.items.push_back(f);
        self.cv.notify_all();
        true
    }

    pub fn try_pop(&self) -> Option<Feed> {
        let mut s = self.lock();
        let f = s.items.pop_front()?;
        s.bytes -= weight(&f);
        self.cv.notify_all();
        Some(f)
    }

    pub fn pop_timeout(&self, d: Duration) -> Option<Feed> {
        {
            let s = self.lock();
            if s.items.is_empty() && !s.closed {
                let _ = self.cv.wait_timeout(s, d);
            }
        }
        self.try_pop()
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

/// A run of the film from one place: dropping it ends the run.
pub struct Run {
    pub q: Arc<Queue>,
    stop: Option<Arc<Stop>>,
}

impl Run {
    pub fn new(q: Arc<Queue>, stop: Option<Arc<Stop>>) -> Run {
        Run { q, stop }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.q.close();
        if let Some(s) = &self.stop {
            s.cut();
        }
    }
}

enum Cmd {
    Start {
        at_ns: i64,
        audio: i32,
        sub: Option<i32>,
        lossless: Mode,
        q: Arc<Queue>,
    },
    Quit,
}

pub struct Demux {
    cmd: Sender<Cmd>,
    stop: Arc<Stop>,
    pub info: Info,
    thread: Option<std::thread::JoinHandle<()>>,
}

const QUEUE_BYTES: usize = 12 * 1024 * 1024;

impl Demux {
    /// Opens the file and reads its streams. Err says why it cannot be played this way.
    pub fn open(url: &str) -> Result<Demux, String> {
        lib()?;
        let stop = Arc::new(Stop::default());
        let (cmd, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let (u, s) = (url.to_string(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("demux".into())
            .spawn(move || thread_main(u, s, rx, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(info)) => Ok(Demux {
                cmd,
                stop,
                info,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the demuxer stopped".into()),
        }
    }

    /// Starts reading at `at_ns` (the keyframe at or before it). Ends the run before this one.
    pub fn start(&self, at_ns: i64, audio: i32, sub: Option<i32>, lossless: Mode) -> Run {
        let q = Queue::new(QUEUE_BYTES);
        let _ = self.cmd.send(Cmd::Start {
            at_ns,
            audio,
            sub,
            lossless,
            q: q.clone(),
        });
        Run::new(q, Some(self.stop.clone()))
    }
}

impl Drop for Demux {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        self.stop.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---- the thread ---------------------------------------------------------------------------------------------------

struct Avio {
    range: Range,
}

const IO_BUFFER: usize = 256 * 1024;

extern "C" fn read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    let io = unsafe { &mut *(op as *mut Avio) };
    let buf = unsafe { std::slice::from_raw_parts_mut(dst, n.max(0) as usize) };
    match io.range.read(buf) {
        Ok(0) => AVERROR_EOF,
        Ok(n) => n as c_int,
        Err(_) => AVERROR_EXIT,
    }
}

extern "C" fn seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let io = unsafe { &mut *(op as *mut Avio) };
    let size = io.range.size;
    if whence & AVSEEK_SIZE != 0 {
        return size;
    }
    let to = match whence & 3 {
        0 => offset,
        1 => io.range.position() + offset,
        2 => size + offset,
        _ => return -1,
    };
    if to < 0 {
        return -1;
    }
    io.range.seek(to);
    io.range.position()
}

fn channels_name(n: i32) -> String {
    match n {
        1 => "Mono".into(),
        2 => "Stereo".into(),
        6 => "5.1".into(),
        7 => "6.1".into(),
        8 => "7.1".into(),
        n => format!("{n}ch"),
    }
}

/// "French - AC3 5.1" or, when the file names the track, "French - Director's commentary".
unsafe fn track_name(f: &Fns, s: *mut AVStream, codec: &str, channels: Option<i32>, n: usize) -> String {
    let lang = crate::lang::name(&stream_tag(f, s, c"language").unwrap_or_default());
    let title = stream_tag(f, s, c"title").or_else(|| stream_tag(f, s, c"name"));
    let tech = match channels {
        Some(c) => format!("{} {}", codec.to_uppercase(), channels_name(c)),
        None => codec.to_uppercase(),
    };
    let mut parts: Vec<String> = vec![];
    if !lang.is_empty() {
        parts.push(lang);
    }
    match title {
        Some(t) if !t.eq_ignore_ascii_case(&parts.first().cloned().unwrap_or_default()) => parts.push(t),
        _ => parts.push(tech),
    }
    if parts.len() == 1 && parts[0].starts_with(codec.to_uppercase().as_str()) {
        parts.insert(0, format!("Track {n}"));
    }
    parts.join(" - ")
}

unsafe fn dovi_conf(cp: *const AVCodecParameters) -> Option<Dv> {
    let (list, n) = ((*cp).coded_side_data, (*cp).nb_coded_side_data);
    if list.is_null() || n <= 0 {
        return None;
    }
    for i in 0..n as usize {
        let sd = &*list.add(i);
        if sd.type_ != AV_PKT_DATA_DOVI_CONF || sd.data.is_null() || sd.size < 9 {
            continue;
        }
        let b = std::slice::from_raw_parts(sd.data, sd.size);
        return Some(Dv {
            profile: b[2],
            compat: b[7],
            el: b[5] == 1,
        });
    }
    None
}

/// Video the TV's decoder takes: H.264 up to High profile (8 bit) and H.265 Main and Main 10.
fn video_refusal(name: &str, profile: c_int, bits: c_int) -> Option<String> {
    match name {
        "h264" if matches!(profile, 66 | 77 | 88 | 100 | 578 | 579 | 580 | 581 | 583) || profile < 0 => {
            (bits > 8).then(|| "the TV does not decode 10-bit H.264".to_string())
        }
        "h264" => Some(format!("the TV does not decode H.264 profile {profile}")),
        "hevc" if matches!(profile, 1 | 2 | -99 | -1) => None,
        "hevc" => Some(format!("the TV does not decode H.265 profile {profile}")),
        other => Some(format!("the TV does not decode {other} in a film file")),
    }
}

unsafe fn read_info(f: &Fns, fmt: *mut AVFormatContext) -> Info {
    let mut info = Info {
        duration_ns: fmt_duration(fmt).max(0) * 1000,
        ..Info::default()
    };
    let (mut n_a, mut n_s) = (0, 0);
    let (mut fonts, mut fonts_bytes) = (vec![], 0usize);
    for i in 0..(*fmt).nb_streams as usize {
        let s = *(*fmt).streams.add(i);
        let cp = stream_codecpar(s);
        let name = codec_name(f, (*cp).codec_id);
        match (*cp).codec_type {
            AVMEDIA_TYPE_VIDEO if info.video.is_none() && (*cp).width > 0 => {
                let codec = match name.as_str() {
                    "hevc" => Codec::H265,
                    _ => Codec::H264,
                };
                info.video = Some(Video {
                    codec,
                    w: (*cp).width,
                    h: (*cp).height,
                    dv: dovi_conf(cp),
                    refused: video_refusal(&name, (*cp).profile, (*cp).bits_per_raw_sample),
                });
            }
            AVMEDIA_TYPE_AUDIO => {
                n_a += 1;
                let ch = (*cp).ch_layout.nb_channels;
                let how = match name.as_str() {
                    "aac" => How::Direct(Codec::Aac),
                    "ac3" => How::Direct(Codec::Ac3),
                    "eac3" => How::Direct(Codec::Eac3),
                    "truehd" | "mlp" | "flac" | "alac" | "vorbis" | "pcm_s16le" | "pcm_s16be" | "pcm_s24le"
                    | "pcm_s24be" | "pcm_s32le" | "pcm_bluray" | "pcm_dvd" => {
                        if convert::can_convert(cp) {
                            How::Convert
                        } else {
                            How::No
                        }
                    }
                    _ => How::No,
                };
                let kind = match name.as_str() {
                    "dts" => Kind::Dts,
                    "truehd" => Kind::TrueHd,
                    _ => Kind::Other,
                };
                info.audio.push(ATrack {
                    stream: i as i32,
                    name: track_name(f, s, &name, Some(ch), n_a),
                    how,
                    kind,
                    lang: stream_tag(f, s, c"language").unwrap_or_default(),
                });
            }
            AVMEDIA_TYPE_SUBTITLE => {
                n_s += 1;
                let image = matches!(name.as_str(), "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle");
                let text = matches!(
                    name.as_str(),
                    "subrip" | "srt" | "ass" | "ssa" | "mov_text" | "webvtt" | "text"
                );
                if image || text {
                    info.subs.push(STrack {
                        stream: i as i32,
                        name: track_name(f, s, &name, None, n_s),
                        image,
                        ass: matches!(name.as_str(), "ass" | "ssa") && ass::available(),
                        lang: stream_tag(f, s, c"language").unwrap_or_default(),
                        forced: stream_forced(s),
                    });
                }
            }
            AVMEDIA_TYPE_ATTACHMENT if ass::available() => {
                let cp = stream_codecpar(s);
                let (d, n) = ((*cp).extradata, (*cp).extradata_size);
                let name = stream_tag(f, s, c"filename").unwrap_or_default();
                let lower = name.to_lowercase();
                let font = [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e));
                if font && !d.is_null() && n > 0 && fonts_bytes + n as usize <= MAX_FONT_BYTES {
                    fonts_bytes += n as usize;
                    fonts.push((name, std::slice::from_raw_parts(d, n as usize).to_vec()));
                }
            }
            _ => {}
        }
    }
    info.fonts = Fonts(Arc::new(fonts));
    info
}

/// The most attached font data kept: a pack of fonts for a long series can run to tens of megabytes.
const MAX_FONT_BYTES: usize = 24 << 20;

struct Open {
    fmt: *mut AVFormatContext,
    io: Box<Avio>,
    avio: *mut AVIOContext,
}

unsafe fn open_input(f: &Fns, url: &str, stop: Arc<Stop>) -> Result<Open, String> {
    let range = Range::open(url, stop)?;
    let mut io = Box::new(Avio { range });
    let buf = (f.av_malloc)(IO_BUFFER) as *mut u8;
    if buf.is_null() {
        return Err("out of memory".into());
    }
    let avio = (f.avio_alloc_context)(
        buf,
        IO_BUFFER as c_int,
        0,
        &mut *io as *mut Avio as *mut c_void,
        Some(read_cb),
        None,
        Some(seek_cb),
    );
    if avio.is_null() {
        return Err("out of memory".into());
    }
    let mut fmt = (f.avformat_alloc_context)();
    if fmt.is_null() {
        return Err("out of memory".into());
    }
    (*fmt).pb = avio;
    let r = (f.avformat_open_input)(&mut fmt, c"".as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
    if r < 0 {
        return Err(format!("FFmpeg could not open the file ({r})"));
    }
    let o = Open { fmt, io, avio };
    if (f.avformat_find_stream_info)(o.fmt, std::ptr::null_mut()) < 0 {
        return Err("FFmpeg found no streams in the file".into());
    }
    Ok(o)
}

impl Drop for Open {
    fn drop(&mut self) {
        if let Ok(f) = lib() {
            unsafe {
                let mut fmt = self.fmt;
                (f.avformat_close_input)(&mut fmt);
                // FFmpeg does not free a caller's I/O context: the buffer first, then the context.
                if !self.avio.is_null() {
                    let buf = std::ptr::addr_of_mut!((*(self.avio as *mut AvioHead)).buffer);
                    (f.av_freep)(buf as *mut c_void);
                    let mut p = self.avio;
                    (f.av_freep)(&mut p as *mut *mut AVIOContext as *mut c_void);
                }
            }
        }
        let _ = &self.io;
    }
}

/// The first field of `AVIOContext`: the buffer, which `avio_alloc_context` took from us.
#[repr(C)]
struct AvioHead {
    _class: *const c_void,
    buffer: *mut u8,
}

fn thread_main(url: String, stop: Arc<Stop>, cmds: Receiver<Cmd>, ready: Sender<Result<Info, String>>) {
    let f = match lib() {
        Ok(f) => f,
        Err(e) => return drop(ready.send(Err(e))),
    };
    let open = match unsafe { open_input(f, &url, stop.clone()) } {
        Ok(o) => o,
        Err(e) => return drop(ready.send(Err(e))),
    };
    let info = unsafe { read_info(f, open.fmt) };
    log(&format!(
        "ff: opened, {} s, {} bytes read, video {:?}, audio {:?}, subtitles {:?}",
        info.duration_ns / 1_000_000_000,
        open.io.range.fetched,
        info.video.as_ref().map(|v| (v.codec, v.w, v.h)),
        info.audio.iter().map(|a| a.lang.as_str()).collect::<Vec<_>>(),
        info.subs
            .iter()
            .map(|t| format!("{}{}", t.lang, if t.forced { " forced" } else { "" }))
            .collect::<Vec<_>>()
    ));
    let _ = ready.send(Ok(info.clone()));
    let mut first = true;
    while let Ok(c) = cmds.recv() {
        let Cmd::Start {
            at_ns,
            audio,
            sub,
            lossless,
            q,
        } = c
        else {
            break;
        };
        // A new run must not see the stop of the one before it.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            run(
                f,
                &open,
                &info,
                at_ns,
                audio,
                sub,
                lossless,
                &q,
                first && at_ns == 0,
                &stop,
            )
        }));
        first = false;
        match out {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if !q.is_closed() {
                    log(&format!("ff: {e}"));
                    q.push(Feed::End(e));
                }
            }
            Err(_) => {
                log("ff: the demuxer panicked");
                q.push(Feed::End("The demuxer failed.".into()));
            }
        }
        if stop.flag.load(Ordering::SeqCst) {
            break;
        }
    }
}

// ---- framing ------------------------------------------------------------------------------------------------------

/// The parameter sets of an `avcC` or `hvcC` record as Annex-B, and the length of the NAL length fields.
fn parse_extradata(e: &[u8], hevc: bool) -> (Vec<u8>, usize) {
    let mut blob = Vec::new();
    let sc = [0u8, 0, 0, 1];
    let u16at = |p: usize| ((e[p] as usize) << 8) | e[p + 1] as usize;
    if e.len() < 7 || e[0] != 1 {
        return (blob, 4);
    }
    if hevc {
        if e.len() < 23 {
            return (blob, 4);
        }
        let nls = (e[21] & 3) as usize + 1;
        let mut p = 23usize;
        for _ in 0..e[22] {
            if p + 3 > e.len() {
                break;
            }
            let keep = matches!(e[p] & 0x3f, 32..=34);
            let cnt = u16at(p + 1);
            p += 3;
            for _ in 0..cnt {
                if p + 2 > e.len() {
                    break;
                }
                let nl = u16at(p);
                p += 2;
                if nl == 0 || p + nl > e.len() {
                    break;
                }
                if keep {
                    blob.extend_from_slice(&sc);
                    blob.extend_from_slice(&e[p..p + nl]);
                }
                p += nl;
            }
        }
        (blob, nls)
    } else {
        let nls = (e[4] & 3) as usize + 1;
        let mut p = 6usize;
        let mut lists = vec![(e[5] & 0x1f) as usize];
        for list in 0.. {
            let Some(&n) = lists.get(list) else { break };
            for _ in 0..n {
                if p + 2 > e.len() {
                    break;
                }
                let nl = u16at(p);
                p += 2;
                if nl == 0 || p + nl > e.len() {
                    break;
                }
                blob.extend_from_slice(&sc);
                blob.extend_from_slice(&e[p..p + nl]);
                p += nl;
            }
            if list == 0 && p < e.len() {
                lists.push(e[p] as usize);
                p += 1;
            }
        }
        (blob, nls)
    }
}

fn nal_end(i: usize, nl: usize, size: usize) -> Option<usize> {
    let end = i.checked_add(nl)?;
    (nl > 0 && end <= size).then_some(end)
}

/// Is this H.264 non-IDR slice (header byte included) an I-slice? The slice header starts with two Exp-Golomb
/// fields, `first_mb_in_slice` and `slice_type` (I is 2 or 7).
fn h264_slice_is_intra(nal: &[u8]) -> bool {
    let bits = nal.get(1..).unwrap_or(&[]);
    let mut pos = 0usize;
    let mut ue = || -> Option<u32> {
        let bit = |p: usize| bits.get(p / 8).map(|b| (b >> (7 - p % 8)) & 1);
        let mut zeros = 0;
        while bit(pos)? == 0 {
            zeros += 1;
            pos += 1;
            if zeros > 31 {
                return None;
            }
        }
        pos += 1;
        let mut v = 0u32;
        for _ in 0..zeros {
            v = (v << 1) | u32::from(bit(pos)?);
            pos += 1;
        }
        Some((1u32 << zeros) - 1 + v)
    };
    ue().and_then(|_| ue()).is_some_and(|t| t == 2 || t == 7)
}

/// Calls `f` with each NAL of a length-prefixed packet until it answers true.
fn for_each_nal(d: &[u8], nls: usize, mut f: impl FnMut(&[u8]) -> bool) {
    let mut i = 0usize;
    while i + nls <= d.len() {
        let nl = d[i..i + nls].iter().fold(0usize, |a, b| (a << 8) | *b as usize);
        i += nls;
        let Some(end) = nal_end(i, nl, d.len()) else { return };
        if f(&d[i..end]) {
            return;
        }
        i = end;
    }
}

/// One length-prefixed video packet as Annex-B in `out`, with `param` in front when it is a keyframe. Gives whether it
/// is one. On a profile 7 disc `dv7` decides what happens to the second layer and the RPU (`dolby`); HDR10+ metadata
/// is cut out of every HEVC picture.
fn annexb(d: &[u8], nls: usize, hevc: bool, container_key: bool, dv7: Dv7, param: &[u8], out: &mut Vec<u8>) -> bool {
    out.clear();
    let mut is_key = false;
    let mut sps_seen = false;
    for_each_nal(d, nls, |n| {
        let b0 = n[0];
        let key = if hevc {
            (16..=23).contains(&((b0 >> 1) & 0x3f))
        } else {
            sps_seen |= (b0 & 0x1f) == 7;
            (b0 & 0x1f) == 5 || ((container_key || sps_seen) && (b0 & 0x1f) == 1 && h264_slice_is_intra(n))
        };
        is_key |= key;
        key
    });
    if is_key {
        out.extend_from_slice(param);
    }
    for_each_nal(d, nls, |n| {
        if !hevc {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
            return false;
        }
        let nal: Cow<[u8]> = match (dv7, dolby::nal_type(n)) {
            (Dv7::Off, _) => Cow::Borrowed(n),
            (Dv7::Drop, dolby::NAL_RPU | dolby::NAL_EL) => return false,
            (_, _) if dolby::is_el(n) => return false,
            (Dv7::Convert, dolby::NAL_RPU) => match dolby::rpu_to_p81(n) {
                Some(r) => Cow::Owned(r),
                None => return false,
            },
            _ => Cow::Borrowed(n),
        };
        match dolby::strip_hdr10plus(&nal) {
            Sei::Keep => {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&nal);
            }
            Sei::Rewrite(v) => {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&v);
            }
            Sei::Drop => {}
        }
        false
    });
    is_key
}

fn adts_freq_index(rate: c_int) -> Option<u8> {
    [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ]
    .iter()
    .position(|r| *r == rate)
    .map(|i| i as u8)
}

/// (frequency index, channel configuration) for the ADTS header: from the AudioSpecificConfig when there is one (a
/// HE-AAC stream names its core rate there, not the output rate), else from the parameters.
unsafe fn adts_params(cp: *const AVCodecParameters) -> Option<(u8, u8)> {
    let ch = (*cp).ch_layout.nb_channels;
    let fallback = if (1..=7).contains(&ch) { ch as u8 } else { 2 };
    let (ed, n) = ((*cp).extradata, (*cp).extradata_size);
    if !ed.is_null() && n >= 2 {
        let (b0, b1) = (*ed as u32, *ed.add(1) as u32);
        let aot = (b0 >> 3) & 0x1F;
        let freq = (((b0 & 0x07) << 1) | (b1 >> 7)) as u8;
        if aot != 31 && freq <= 12 {
            let chan = ((b1 >> 3) & 0x0F) as u8;
            return Some((freq, if (1..=7).contains(&chan) { chan } else { fallback }));
        }
    }
    adts_freq_index((*cp).sample_rate).map(|fi| (fi, fallback))
}

fn adts_header(freq: u8, chan: u8, payload: usize) -> [u8; 7] {
    let len = (payload + 7) as u32;
    [
        0xFF,
        0xF1,
        (1 << 6) | (freq << 2) | (chan >> 2),
        ((chan & 3) << 6) | ((len >> 11) as u8 & 0x03),
        ((len >> 3) & 0xFF) as u8,
        (((len & 0x07) as u8) << 5) | 0x1F,
        0xFC,
    ]
}

// ---- subtitles ----------------------------------------------------------------------------------------------------

/// The text of an ASS event as the decoders give it (`ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`)
/// or of a plain text rect, without styling: the override blocks go, `\N` is a line break. True for a line placed at the
/// top of the picture (`\an7` to `\an9`).
pub fn plain_text(ass: &str, is_ass: bool) -> (String, bool) {
    let body = if is_ass {
        ass.splitn(9, ',').nth(8).unwrap_or("")
    } else {
        ass
    };
    let (mut out, mut skip, mut top) = (String::new(), false, false);
    let mut block = String::new();
    for c in body.chars() {
        match (skip, c) {
            (true, '}') => {
                skip = false;
                top |= ["\\an7", "\\an8", "\\an9"].iter().any(|t| block.contains(t));
                block.clear();
            }
            (true, c) => block.push(c),
            (false, '{') => skip = true,
            (false, '<') | (false, '>') if !is_ass => out.push(c),
            _ => out.push(c),
        }
    }
    let t = out
        .replace("\\N", "\n")
        .replace("\\n", "\n")
        .replace("\\h", " ")
        .replace("\r\n", "\n");
    let t = strip_html(&t);
    (
        t.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        top,
    )
}

/// `<i>`, `<b>`, `<font ...>` and their ends, which SRT and WebVTT carry as text.
fn strip_html(s: &str) -> String {
    let (mut out, mut tag) = (String::new(), false);
    for c in s.chars() {
        match (tag, c) {
            (true, '>') => tag = false,
            (true, _) => {}
            (false, '<') => tag = true,
            _ => out.push(c),
        }
    }
    out
}

unsafe fn open_sub_decoder(f: &Fns, cp: *const AVCodecParameters) -> *mut AVCodecContext {
    let codec = (f.avcodec_find_decoder)((*cp).codec_id);
    if codec.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = (f.avcodec_alloc_context3)(codec);
    if ctx.is_null() {
        return ctx;
    }
    if (f.avcodec_parameters_to_context)(ctx, cp) < 0 || (f.avcodec_open2)(ctx, codec, std::ptr::null_mut()) < 0 {
        let mut c = ctx;
        (f.avcodec_free_context)(&mut c);
        return std::ptr::null_mut();
    }
    ctx
}

/// The canvas the bitmaps are drawn for, as the decoder knows it after a decode (PGS declares it in the stream).
unsafe fn sub_canvas(f: &Fns, dec: *mut AVCodecContext, name: &str) -> (i32, i32) {
    let par = (f.avcodec_parameters_alloc)();
    let mut wh = (0, 0);
    if !par.is_null() {
        if (f.avcodec_parameters_from_context)(par, dec) >= 0 {
            wh = ((*par).width, (*par).height);
        }
        let mut p = par;
        (f.avcodec_parameters_free)(&mut p);
    }
    if wh.0 > 0 && wh.1 > 0 {
        wh
    } else if name == "hdmv_pgs_subtitle" {
        (1920, 1080)
    } else {
        (720, 576)
    }
}

unsafe fn rect_to_rgba(r: *const AVSubtitleRect) -> Option<Bitmap> {
    let (x, y, w, h, stride) = ((*r).x, (*r).y, (*r).w, (*r).h, (*r).linesize[0]);
    let (idx, pal) = ((*r).data[0], (*r).data[1] as *const u32);
    if idx.is_null() || pal.is_null() || w <= 0 || h <= 0 || stride < w || w > 8192 || h > 8192 {
        return None;
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h as usize {
        for col in 0..w as usize {
            let p = *pal.add(*idx.add(row * stride as usize + col) as usize);
            rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
        }
    }
    Some(Bitmap { x, y, w, h, rgba })
}

/// Decodes one subtitle packet into an event at `start_ns` (the time in the file, not in the run), if it holds one.
unsafe fn decode_sub(
    f: &Fns,
    dec: *mut AVCodecContext,
    name: &str,
    pkt: *mut AVPacket,
    start_ns: i64,
    dur_ns: i64,
) -> Option<SubEv> {
    let mut sub: AVSubtitle = std::mem::zeroed();
    let mut got: c_int = 0;
    if (f.avcodec_decode_subtitle2)(dec, &mut sub, &mut got, pkt) < 0 || got == 0 {
        return None;
    }
    let shown = i64::from(sub.end_display_time) * 1_000_000;
    let end = if shown > 0 {
        Some(start_ns + shown)
    } else if dur_ns > 0 {
        Some(start_ns + dur_ns)
    } else {
        None
    };
    let ev = if sub.num_rects == 0 {
        Some(SubEv {
            start_ns,
            end_ns: None,
            what: SubWhat::Clear,
        })
    } else {
        let first = *sub.rects;
        match (*first).type_ {
            1 => {
                let rects: Vec<Bitmap> = (0..(sub.num_rects as usize).min(8))
                    .filter_map(|i| rect_to_rgba(*sub.rects.add(i)))
                    .collect();
                (!rects.is_empty()).then(|| SubEv {
                    start_ns,
                    end_ns: end,
                    what: SubWhat::Image {
                        canvas: sub_canvas(f, dec, name),
                        rects,
                    },
                })
            }
            t @ (2 | 3) => {
                let mut text = String::new();
                let mut top = false;
                for i in 0..sub.num_rects as usize {
                    let r = *sub.rects.add(i);
                    let p = if t == 3 { (*r).ass } else { (*r).text };
                    if p.is_null() {
                        continue;
                    }
                    let (s, tp) = plain_text(&CStr::from_ptr(p).to_string_lossy(), t == 3);
                    top |= tp;
                    if !s.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&s);
                    }
                }
                (!text.is_empty()).then(|| SubEv {
                    start_ns,
                    end_ns: Some(end.unwrap_or(start_ns + 4_000_000_000)),
                    what: SubWhat::Text { text, top },
                })
            }
            _ => None,
        }
    };
    (f.avsubtitle_free)(&mut sub);
    ev
}

// ---- a run --------------------------------------------------------------------------------------------------------

unsafe fn pts_ns(f: &Fns, pkt: *const AVPacket, st: *mut AVStream) -> Option<i64> {
    let t = if (*pkt).pts != AV_NOPTS_VALUE {
        (*pkt).pts
    } else {
        (*pkt).dts
    };
    (t != AV_NOPTS_VALUE).then(|| (f.av_rescale_q)(t, stream_time_base(st), NS_TB))
}

/// Reads the file from `at_ns` and queues what it finds, until the queue is closed, the file ends or it fails.
#[allow(clippy::too_many_arguments)]
unsafe fn run(
    f: &Fns,
    open: &Open,
    info: &Info,
    at_ns: i64,
    audio: i32,
    sub: Option<i32>,
    lossless: Mode,
    q: &Arc<Queue>,
    no_seek: bool,
    stop: &Arc<Stop>,
) -> Result<(), String> {
    let fmt = open.fmt;
    let video = info.video.as_ref().ok_or("no picture")?;
    let hevc = video.codec == Codec::H265;
    let streams: Vec<*mut AVStream> = (0..(*fmt).nb_streams as usize)
        .map(|i| *(*fmt).streams.add(i))
        .collect();
    let vstream = (0..streams.len())
        .find(|&i| {
            (*stream_codecpar(streams[i])).codec_type == AVMEDIA_TYPE_VIDEO && (*stream_codecpar(streams[i])).width > 0
        })
        .ok_or("no picture")?;
    let vcp = stream_codecpar(streams[vstream]);
    let ed = if (*vcp).extradata.is_null() || (*vcp).extradata_size <= 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts((*vcp).extradata, (*vcp).extradata_size as usize)
    };
    let (param, nls) = parse_extradata(ed, hevc);
    let dv7 = match &video.dv {
        Some(d) if hevc && d.profile == 7 && d.el && d.compat == 6 && dolby::tv_shows_dv() => Dv7::Convert,
        Some(d) if hevc && d.profile == 7 => Dv7::Drop,
        _ => Dv7::Off,
    };
    let acp = stream_codecpar(*streams.get(audio as usize).ok_or("no such audio track")?);
    let a_how = info
        .audio
        .iter()
        .find(|a| a.stream == audio)
        .map(|a| a.how)
        .ok_or("no such audio track")?;
    let adts = (a_how == How::Direct(Codec::Aac)).then(|| adts_params(acp)).flatten();
    if a_how == How::Direct(Codec::Aac) && adts.is_none() {
        return Err("The AAC audio has a sample rate that ADTS cannot name.".into());
    }
    let mut conv = match a_how {
        How::Convert if lossless != Mode::TrueHd => Some(Converter::open(acp)?),
        _ => None,
    };
    lossless::clear();
    let (mut packer, mut mat) = (Packer::default(), Mat::default());
    if a_how == How::No && lossless == Mode::Off {
        return Err("the TV cannot decode that audio".into());
    }
    if let Some(c) = &conv {
        log(&format!("ff: audio {}", c.desc));
    }
    let sub_info = sub.and_then(|s| info.subs.iter().find(|t| t.stream == s));
    let ass_header = sub_info.filter(|t| t.ass).map(|t| {
        let cp = stream_codecpar(streams[t.stream as usize]);
        let (d, n) = ((*cp).extradata, (*cp).extradata_size);
        if d.is_null() || n <= 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(d, n as usize).to_vec()
        }
    });
    let (sub_dec, sub_name) = match sub_info.filter(|t| !t.ass) {
        Some(t) => {
            let cp = stream_codecpar(streams[t.stream as usize]);
            (open_sub_decoder(f, cp), codec_name(f, (*cp).codec_id))
        }
        None => (std::ptr::null_mut(), String::new()),
    };

    if !no_seek {
        let r = (f.av_seek_frame)(fmt, -1, at_ns / 1000, AVSEEK_FLAG_BACKWARD);
        if r < 0 {
            if q.is_closed() || stop.flag.load(Ordering::SeqCst) {
                return Ok(());
            }
            return Err(format!("The file would not seek ({r})."));
        }
    }

    let pkt = (f.av_packet_alloc)();
    let a_codec = match (lossless, a_how) {
        (Mode::Dts | Mode::TrueHd, _) => Some(Codec::Eac3),
        (Mode::DtsCore, _) => Some(Codec::Pcm),
        (_, How::Direct(c)) => Some(c),
        _ => Some(Codec::Ac3),
    };
    let dv = match dv7 {
        Dv7::Convert => Some(Dv {
            profile: 8,
            compat: 1,
            el: false,
        }),
        Dv7::Drop => None,
        Dv7::Off => video.dv.clone(),
    };
    if !q.push(Feed::Streams {
        video: Some(video.codec),
        audio: a_codec,
        dv,
    }) {
        (f.av_packet_free)(&mut { pkt });
        return Ok(());
    }
    if let Some(header) = ass_header {
        if !q.push(Feed::AssTrack {
            header,
            fonts: info.fonts.clone(),
            video: (video.w, video.h),
        }) {
            (f.av_packet_free)(&mut { pkt });
            return Ok(());
        }
    }
    let ass_stream = sub_info.filter(|t| t.ass).map(|t| t.stream);
    let mut base: Option<i64> = None;
    let mut au_buf: Vec<u8> = Vec::new();
    let mut converted: Vec<(i64, Vec<u8>)> = vec![];
    let (mut videos, mut audios, mut subs_n) = (0u32, 0u32, 0u32);
    let result = loop {
        if q.is_closed() {
            break Ok(());
        }
        let r = (f.av_read_frame)(fmt, pkt);
        if r < 0 {
            if q.is_closed() || stop.flag.load(Ordering::SeqCst) {
                break Ok(());
            }
            break if r == AVERROR_EOF {
                q.push(Feed::End(ENDED.into()));
                Ok(())
            } else {
                Err("The connection to the server ended.".to_string())
            };
        }
        let si = (*pkt).stream_index as usize;
        let st = streams[si];
        let data = if (*pkt).data.is_null() || (*pkt).size <= 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize)
        };
        let pts = pts_ns(f, pkt, st);
        let mut alive = true;
        if si == vstream {
            if let Some(pts) = pts {
                let key = annexb(
                    data,
                    nls,
                    hevc,
                    (*pkt).flags & AV_PKT_FLAG_KEY != 0,
                    dv7,
                    &param,
                    &mut au_buf,
                );
                if base.is_none() && key {
                    base = Some(pts);
                    alive = q.push(Feed::Base(pts));
                }
                // Pictures that come before the keyframe on screen lean on frames that were skipped.
                if let (Some(b), true) = (base, alive && !au_buf.is_empty() && pts >= base.unwrap_or(0)) {
                    videos += 1;
                    alive = q.push(Feed::Au(Au {
                        video: true,
                        key,
                        pts_ns: (pts - b).max(0),
                        data: std::mem::take(&mut au_buf),
                    }));
                }
            }
        } else if let Some(b) = base {
            if si as i32 == audio {
                if let Some(pts) = pts.filter(|p| *p >= b - 40_000_000) {
                    let silent = |t: i64| Au {
                        video: false,
                        key: true,
                        pts_ns: (t - b).max(0),
                        data: lossless::SILENT_EAC3.to_vec(),
                    };
                    match (&mut conv, adts) {
                        _ if lossless == Mode::Dts => {
                            let core = lossless::dts_core_packet(data).map_or(0, |c| c.len());
                            match packer.frame(data, core, pts) {
                                Ok(Some(t)) => {
                                    audios += 1;
                                    alive = q.push(Feed::Au(silent(t)));
                                }
                                Ok(None) => {}
                                Err(()) => break Err("A DTS-HD frame fits no burst of the receiver link.".into()),
                            }
                        }
                        _ if lossless == Mode::TrueHd => match mat.au(data, pts) {
                            Ok(times) => {
                                for t in times {
                                    audios += 1;
                                    alive &= q.push(Feed::Au(silent(t)));
                                }
                            }
                            Err(()) => break Err("A TrueHD unit cannot be packed for the receiver link.".into()),
                        },
                        _ if lossless == Mode::DtsCore => {
                            let Some(burst) = lossless::dts_core_packet(data).and_then(|c| lossless::dts_iec61937(&c))
                            else {
                                break Err("The DTS core does not fit an IEC 61937 burst.".into());
                            };
                            audios += 1;
                            alive = q.push(Feed::Au(Au {
                                video: false,
                                key: true,
                                pts_ns: (pts - b).max(0),
                                data: burst,
                            }));
                        }
                        (Some(c), _) => {
                            converted.clear();
                            if let Err(e) = c.push(pkt, Some(pts), &mut converted) {
                                break Err(e);
                            }
                            for (t, bytes) in converted.drain(..) {
                                audios += 1;
                                alive &= q.push(Feed::Au(Au {
                                    video: false,
                                    key: true,
                                    pts_ns: (t - b).max(0),
                                    data: bytes,
                                }));
                            }
                        }
                        (None, Some((fi, ch))) => {
                            let mut v = adts_header(fi, ch, data.len()).to_vec();
                            v.extend_from_slice(data);
                            audios += 1;
                            alive = q.push(Feed::Au(Au {
                                video: false,
                                key: true,
                                pts_ns: (pts - b).max(0),
                                data: v,
                            }));
                        }
                        (None, None) => {
                            audios += 1;
                            alive = q.push(Feed::Au(Au {
                                video: false,
                                key: true,
                                pts_ns: (pts - b).max(0),
                                data: data.to_vec(),
                            }));
                        }
                    }
                }
            } else if Some(si as i32) == ass_stream {
                if let Some(pts) = pts {
                    let dur = if (*pkt).duration > 0 {
                        (f.av_rescale_q)((*pkt).duration, stream_time_base(st), NS_TB)
                    } else {
                        0
                    };
                    subs_n += 1;
                    alive = q.push(Feed::AssChunk {
                        data: data.to_vec(),
                        start_ms: pts / 1_000_000,
                        dur_ms: dur / 1_000_000,
                    });
                }
            } else if Some(si as i32) == sub && !sub_dec.is_null() {
                if let Some(pts) = pts {
                    let dur = if (*pkt).duration > 0 {
                        (f.av_rescale_q)((*pkt).duration, stream_time_base(st), NS_TB)
                    } else {
                        0
                    };
                    if let Some(ev) = decode_sub(f, sub_dec, &sub_name, pkt, pts, dur) {
                        subs_n += 1;
                        alive = q.push(Feed::Sub(ev));
                    }
                }
            }
        }
        (f.av_packet_unref)(pkt);
        if !alive {
            break Ok(());
        }
    };
    if let Some(s) = dolby::summary() {
        log(&format!("ff: {s}"));
    }
    log(&format!(
        "ff: run from {} s ended: {videos} pictures, {audios} audio frames, {subs_n} subtitles, {} MB read",
        at_ns / 1_000_000_000,
        open.io.range.fetched / 1_000_000
    ));
    let mut p = pkt;
    (f.av_packet_free)(&mut p);
    if !sub_dec.is_null() {
        let mut d = sub_dec;
        (f.avcodec_free_context)(&mut d);
    }
    result
}

/// `--direct <url> [seconds]` on a desktop: opens the file, lists its tracks and counts what a run from `seconds` gives
/// in ten seconds.
pub fn probe_cli(url: &str, at_s: i64, audio: Option<i64>, sub: Option<i64>) {
    let d = match Demux::open(url) {
        Ok(d) => d,
        Err(e) => return println!("open failed: {e}"),
    };
    println!(
        "duration {} s, video {:?}",
        d.info.duration_ns / 1_000_000_000,
        d.info.video
    );
    for a in &d.info.audio {
        println!("audio {} {} {:?}", a.stream, a.name, a.how);
    }
    for s in &d.info.subs {
        println!("sub {} {} image={}", s.stream, s.name, s.image);
    }
    let a = match d.info.plan(audio) {
        Ok(a) => a,
        Err(e) => return println!("refused: {e}"),
    };
    if std::env::var_os("FF_INTERRUPT").is_some() {
        // A run cut off in the middle (a seek while reading), then another from somewhere else.
        let first = d.start(0, a, None, Mode::Off);
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
        println!("first run cut");
    }
    let run = d.start(at_s * 1_000_000_000, a, sub.map(|s| s as i32), Mode::Off);
    let t = std::time::Instant::now();
    let (mut v, mut au, mut keys, mut subs, mut last_v, mut last_a) = (0, 0, 0, 0, 0i64, 0i64);
    let mut base = None;
    while t.elapsed() < Duration::from_secs(10) {
        match run.q.pop_timeout(Duration::from_millis(100)) {
            Some(Feed::Au(x)) if x.video => {
                v += 1;
                keys += x.key as u32;
                last_v = x.pts_ns;
            }
            Some(Feed::Au(x)) => {
                au += 1;
                last_a = x.pts_ns;
            }
            Some(Feed::AssTrack { header, fonts, .. }) => {
                println!("ass track: {} header bytes, {:?}", header.len(), fonts)
            }
            Some(Feed::AssChunk { .. }) => subs += 1,
            Some(Feed::Sub(e)) => {
                subs += 1;
                if subs <= 3 {
                    match e.what {
                        SubWhat::Text { text, .. } => println!("sub at {} ms: {text:?}", e.start_ns / 1_000_000),
                        SubWhat::Image { canvas, rects } => {
                            println!(
                                "sub image at {} ms: {} rects, canvas {canvas:?}",
                                e.start_ns / 1_000_000,
                                rects.len()
                            )
                        }
                        SubWhat::Clear => println!("sub clear at {} ms", e.start_ns / 1_000_000),
                    }
                }
            }
            Some(Feed::Base(b)) => base = Some(b / 1_000_000),
            Some(Feed::Streams { video, audio, dv }) => println!("streams {video:?} {audio:?} {dv:?}"),
            Some(Feed::End(e)) => {
                println!("end: {e}");
                break;
            }
            None => {}
        }
    }
    println!("base {base:?} ms: {v} pictures ({keys} keyframes, last {} ms), {au} audio frames (last {} ms), {subs} subtitles",
        last_v / 1_000_000, last_a / 1_000_000);
    if let Some(s) = dolby::summary() {
        println!("{s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_parameter_sets_become_annexb() {
        // avcC: version 1, profile, compat, level, 0xFF (4-byte lengths), 0xE1 (one SPS), SPS, one PPS.
        let mut e = vec![1, 100, 0, 40, 0xFF, 0xE1, 0, 3, 0x67, 1, 2, 1, 0, 2, 0x68, 9];
        let (blob, nls) = parse_extradata(&e, false);
        assert_eq!(nls, 4);
        assert_eq!(blob, [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 9]);
        e[0] = 0;
        assert!(parse_extradata(&e, false).0.is_empty(), "not an avcC");
    }

    #[test]
    fn hvcc_keeps_only_vps_sps_pps() {
        let mut e = vec![1u8; 22];
        e[0] = 1;
        e[21] = 3;
        e.push(3); // three arrays
        for (t, nal) in [(32u8, 0x40u8), (39, 0x4e), (34, 0x44)] {
            e.extend_from_slice(&[0x80 | t, 0, 1, 0, 2, nal, 1]);
        }
        let (blob, nls) = parse_extradata(&e, true);
        assert_eq!(nls, 4);
        assert_eq!(
            blob,
            [0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x44, 1],
            "the SEI array (39) is left out"
        );
    }

    #[test]
    fn keyframes_get_their_parameter_sets_and_start_codes() {
        let param = [0, 0, 0, 1, 0x67, 1];
        let mut out = vec![];
        // an IDR slice (type 5), 4-byte length
        let idr = [0, 0, 0, 3, 0x65, 0xAA, 0xBB];
        assert!(annexb(&idr, 4, false, true, Dv7::Off, &param, &mut out));
        assert_eq!(out, [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x65, 0xAA, 0xBB]);
        // a P slice (type 1) is no keyframe and gets no parameter sets; first_mb ue=0 ('1'), type ue=0 ('1')
        let p = [0, 0, 0, 3, 0x41, 0xC0, 0];
        assert!(!annexb(&p, 4, false, true, Dv7::Off, &param, &mut out));
        assert_eq!(out, [0, 0, 0, 1, 0x41, 0xC0, 0]);
    }

    #[test]
    fn a_dolby_vision_disc_loses_its_second_layer() {
        // HEVC NAL header byte 0 holds type<<1: IDR_W_RADL = 19, RPU = 62, EL = 63.
        let pkt = [0, 0, 0, 2, 19 << 1, 1, 0, 0, 0, 2, 62 << 1, 1, 0, 0, 0, 2, 63 << 1, 1];
        let mut out = vec![];
        assert!(annexb(&pkt, 4, true, true, Dv7::Drop, &[], &mut out));
        assert_eq!(out, [0, 0, 0, 1, 19 << 1, 1]);
        annexb(&pkt, 4, true, true, Dv7::Off, &[], &mut out);
        assert_eq!(out.len(), 18, "kept when not profile 7");
    }

    /// A length-prefixed packet (4-byte lengths) of these NALs.
    fn packet(nals: &[&[u8]]) -> Vec<u8> {
        nals.iter()
            .flat_map(|n| (n.len() as u32).to_be_bytes().into_iter().chain(n.iter().copied()))
            .collect()
    }

    #[test]
    fn a_profile_7_picture_keeps_its_base_and_gets_an_8_1_rpu() {
        let mel = &dolby::tests::nal(include_bytes!("dovi-assets/dovi-p7-mel-rpu.bin"));
        let want = &dolby::tests::nal(include_bytes!("dovi-assets/dovi-p7-mel-to-81-rpu.bin"));
        let base = [19 << 1, 1, 0xAA];
        let pkt = packet(&[&base, mel, &[63 << 1, 1, 0xBB], &[2, 9, 0xCC]]);
        let mut out = vec![];
        annexb(&pkt, 4, true, true, Dv7::Convert, &[], &mut out);
        let mut expect = vec![0, 0, 0, 1, 19 << 1, 1, 0xAA, 0, 0, 0, 1];
        expect.extend_from_slice(want);
        assert_eq!(out, expect);
        let junk = packet(&[&base, &[0x7c, 1, 0, 0, 0]]);
        annexb(&junk, 4, true, true, Dv7::Convert, &[], &mut out);
        assert_eq!(
            out,
            [0, 0, 0, 1, 19 << 1, 1, 0xAA],
            "an RPU that cannot convert is dropped"
        );
    }

    #[test]
    fn hdr10_plus_is_cut_from_every_hevc_picture_but_not_h264() {
        let plus = [39 << 1, 1, 4, 7, 0xb5, 0, 0x3c, 0, 1, 4, 1, 0x80];
        let pkt = packet(&[&plus, &[19 << 1, 1, 0xAA]]);
        let mut out = vec![];
        annexb(&pkt, 4, true, true, Dv7::Off, &[], &mut out);
        assert_eq!(out, [0, 0, 0, 1, 19 << 1, 1, 0xAA]);
        annexb(&pkt, 4, false, true, Dv7::Off, &[], &mut out);
        assert_eq!(out.len(), 4 + plus.len() + 4 + 3, "an H.264 stream is left alone");
    }

    #[test]
    fn adts_header_names_the_frame_length() {
        let h = adts_header(3, 2, 100);
        assert_eq!(h[0..2], [0xFF, 0xF1]);
        let len = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | (h[5] >> 5) as usize;
        assert_eq!(len, 107);
        assert_eq!(adts_freq_index(48000), Some(3));
        assert_eq!(adts_freq_index(1234), None);
    }

    #[test]
    fn ass_events_lose_their_styling() {
        let (t, top) = plain_text("12,0,Default,,0,0,0,,{\\an8}Bonjour,{\\i1} le\\Nmonde{\\i0}", true);
        assert_eq!(t, "Bonjour, le\nmonde");
        assert!(top);
        let (t, top) = plain_text("<i>Hello</i>\n<font color=red>there</font>", false);
        assert_eq!(t, "Hello\nthere");
        assert!(!top);
    }

    #[test]
    fn video_the_tv_cannot_decode_is_refused() {
        assert_eq!(video_refusal("h264", 100, 8), None);
        assert!(video_refusal("h264", 110, 10).is_some());
        assert!(video_refusal("h264", 100, 10).is_some());
        assert_eq!(video_refusal("hevc", 2, 10), None);
        assert!(video_refusal("vp9", 0, 8).is_some());
    }

    #[test]
    fn the_queue_holds_back_a_full_reader_and_ends_on_close() {
        let q = Queue::new(1000);
        let au = |n| {
            Feed::Au(Au {
                video: true,
                key: false,
                pts_ns: 0,
                data: vec![0; n],
            })
        };
        assert!(q.push(au(100)));
        let q2 = q.clone();
        let t = std::thread::spawn(move || q2.push(au(5000)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!t.is_finished(), "5000 bytes wait for the first to be taken");
        assert!(q.try_pop().is_some());
        assert!(t.join().unwrap());
        q.close();
        assert!(!q.push(au(1)));
    }
}
