//! MPEG-TS to the access units LG's buffer-feed pipeline takes: Annex-B video pictures and ADTS or AC-3
//! audio frames, with PTS in nanoseconds counted from the first video keyframe.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    H264,
    H265,
    Aac,
    Ac3,
    Eac3,
}

impl Codec {
    /// The name in the Load payload's `codec` object.
    pub fn payload_name(self) -> &'static str {
        match self {
            Codec::H264 => "H264",
            Codec::H265 => "H265",
            Codec::Aac => "AAC",
            Codec::Ac3 => "AC3",
            Codec::Eac3 => "AC3 PLUS",
        }
    }
}

pub struct Au {
    pub video: bool,
    pub key: bool,
    pub pts_ns: i64,
    pub data: Vec<u8>,
}

pub enum Out {
    Streams { video: Option<Codec>, audio: Option<Codec> },
    Au(Au),
}

#[derive(Default)]
pub struct Demux {
    carry: Vec<u8>,
    pmt: Option<u16>,
    video: Option<(u16, Codec)>,
    audio: Option<(u16, Codec)>,
    streams_known: bool,
    vbuf: Vec<u8>,
    abuf: Vec<u8>,
    base: Option<u64>,
    last_vpts: u64,
}

const PTS_WRAP: u64 = 1 << 33;

impl Demux {
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<Out>) {
        self.carry.extend_from_slice(chunk);
        let mut i = 0;
        while self.carry.len() - i >= 188 {
            if self.carry[i] != 0x47 {
                i += 1;
                continue;
            }
            let mut p = [0u8; 188];
            p.copy_from_slice(&self.carry[i..i + 188]);
            self.packet(&p, out);
            i += 188;
        }
        self.carry.drain(..i);
    }

    fn packet(&mut self, p: &[u8; 188], out: &mut Vec<Out>) {
        let pusi = p[1] & 0x40 != 0;
        let pid = u16::from(p[1] & 0x1f) << 8 | u16::from(p[2]);
        let afc = p[3] >> 4 & 3;
        let off = if afc & 2 != 0 { 5 + p[4] as usize } else { 4 };
        if afc & 1 == 0 || off >= 188 {
            return;
        }
        let pl = &p[off..];
        if pid == 0 && pusi && self.pmt.is_none() {
            self.pmt = section(pl).and_then(pat);
        } else if Some(pid) == self.pmt && pusi && !self.streams_known {
            if let Some((v, a)) = section(pl).map(pmt) {
                (self.video, self.audio, self.streams_known) = (v, a, true);
                out.push(Out::Streams {
                    video: v.map(|s| s.1),
                    audio: a.map(|s| s.1),
                });
            }
        } else if let Some((_, codec)) = self.video.filter(|v| v.0 == pid) {
            if let Some(pes) = gather(&mut self.vbuf, pl, pusi) {
                self.video_pes(&pes, codec, out);
            }
        } else if let Some((_, codec)) = self.audio.filter(|a| a.0 == pid) {
            if let Some(pes) = gather(&mut self.abuf, pl, pusi) {
                self.audio_pes(&pes, codec, out);
            }
        }
    }

    /// Nanoseconds since the first keyframe; negative for what came before it.
    fn rel_ns(&self, pts: u64) -> i64 {
        let d = pts.wrapping_sub(self.base.unwrap_or(pts)) % PTS_WRAP;
        let ticks = if d > PTS_WRAP / 2 {
            d as i64 - PTS_WRAP as i64
        } else {
            d as i64
        };
        ticks * 100_000 / 9
    }

    fn video_pes(&mut self, pes: &[u8], codec: Codec, out: &mut Vec<Out>) {
        let Some((pts, data)) = pes_payload(pes) else { return };
        let key = is_key(codec, data);
        if self.base.is_none() {
            if !key || pts.is_none() {
                return;
            }
            self.base = pts;
        }
        let pts = pts.unwrap_or(self.last_vpts);
        self.last_vpts = pts;
        out.push(Out::Au(Au {
            video: true,
            key,
            pts_ns: self.rel_ns(pts),
            data: data.to_vec(),
        }));
    }

    fn audio_pes(&mut self, pes: &[u8], codec: Codec, out: &mut Vec<Out>) {
        let Some((Some(pts), data)) = pes_payload(pes) else {
            return;
        };
        if self.base.is_none() {
            return;
        }
        let start = self.rel_ns(pts);
        let frames = if codec == Codec::Aac { adts_frames(data) } else { vec![] };
        if frames.is_empty() {
            if start >= 0 {
                out.push(Out::Au(Au {
                    video: false,
                    key: true,
                    pts_ns: start,
                    data: data.to_vec(),
                }));
            }
            return;
        }
        let mut t = start;
        for (f, dur) in frames {
            if t >= 0 {
                out.push(Out::Au(Au {
                    video: false,
                    key: true,
                    pts_ns: t,
                    data: f.to_vec(),
                }));
            }
            t += dur;
        }
    }
}

/// Appends a packet's payload to its PES buffer and returns the PES when one is complete: when the next one
/// starts, or as soon as a PES with a declared length is whole.
fn gather(buf: &mut Vec<u8>, pl: &[u8], pusi: bool) -> Option<Vec<u8>> {
    let mut done = None;
    if pusi {
        if !buf.is_empty() {
            done = Some(std::mem::take(buf));
        }
        buf.extend_from_slice(pl);
    } else if !buf.is_empty() {
        buf.extend_from_slice(pl);
    }
    let len = buf.get(4..6).map_or(0, |b| usize::from(b[0]) << 8 | usize::from(b[1]));
    if done.is_none() && len > 0 && buf.len() >= 6 + len {
        buf.truncate(6 + len);
        done = Some(std::mem::take(buf));
    }
    done
}

/// The PSI section after the pointer field, cut to its declared length.
fn section(pl: &[u8]) -> Option<&[u8]> {
    let s = pl.get(1 + *pl.first()? as usize..)?;
    let len = (usize::from(*s.get(1)? & 0x0f) << 8 | usize::from(*s.get(2)?)) + 3;
    s.get(..len)
}

/// The PMT PID of the first program.
fn pat(s: &[u8]) -> Option<u16> {
    s.get(8..s.len().checked_sub(4)?)?
        .as_chunks::<4>()
        .0
        .iter()
        .find(|e| e[0] != 0 || e[1] != 0)
        .map(|e| u16::from(e[2] & 0x1f) << 8 | u16::from(e[3]))
}

type Stream = Option<(u16, Codec)>;

/// The first video and the first audio stream this player can feed.
fn pmt(s: &[u8]) -> (Stream, Stream) {
    let (mut video, mut audio) = (None, None);
    let end = s.len().saturating_sub(4);
    let mut j = 12 + (usize::from(s.get(10).unwrap_or(&0) & 0x0f) << 8 | usize::from(*s.get(11).unwrap_or(&0)));
    while j + 5 <= end {
        let pid = u16::from(s[j + 1] & 0x1f) << 8 | u16::from(s[j + 2]);
        let esl = usize::from(s[j + 3] & 0x0f) << 8 | usize::from(s[j + 4]);
        let desc = s.get(j + 5..(j + 5 + esl).min(end)).unwrap_or(&[]);
        let codec = match s[j] {
            0x1b => Some(Codec::H264),
            0x24 => Some(Codec::H265),
            0x0f => Some(Codec::Aac),
            0x81 => Some(Codec::Ac3),
            0x87 => Some(Codec::Eac3),
            0x06 => dvb_audio(desc),
            _ => None,
        };
        match codec {
            Some(c @ (Codec::H264 | Codec::H265)) => video = video.or(Some((pid, c))),
            Some(c) => audio = audio.or(Some((pid, c))),
            None => {}
        }
        j += 5 + esl;
    }
    (video, audio)
}

/// DVB carries AC-3 as private data, named by a descriptor.
fn dvb_audio(mut d: &[u8]) -> Option<Codec> {
    while d.len() >= 2 {
        match d[0] {
            0x6a => return Some(Codec::Ac3),
            0x7a => return Some(Codec::Eac3),
            _ => d = d.get(2 + d[1] as usize..)?,
        }
    }
    None
}

/// The PES's PTS (90 kHz) and its elementary-stream bytes.
fn pes_payload(b: &[u8]) -> Option<(Option<u64>, &[u8])> {
    if b.get(..3)? != [0, 0, 1] {
        return None;
    }
    let p = b.get(9..14);
    let pts = p.filter(|_| b[7] & 0x80 != 0).map(|p| {
        u64::from(p[0] >> 1 & 7) << 30
            | u64::from(p[1]) << 22
            | u64::from(p[2] >> 1) << 15
            | u64::from(p[3]) << 7
            | u64::from(p[4] >> 1)
    });
    Some((pts, b.get(9 + *b.get(8)? as usize..)?))
}

/// True when the picture can start decoding: an IDR or a parameter set (H.264), an IRAP or a VPS (H.265).
fn is_key(codec: Codec, d: &[u8]) -> bool {
    d.windows(4).any(|w| {
        w[..3] == [0, 0, 1]
            && match codec {
                Codec::H265 => matches!(w[3] >> 1 & 0x3f, 16..=23 | 32),
                _ => matches!(w[3] & 0x1f, 5 | 7),
            }
    })
}

/// The ADTS frames in an AAC PES, each with its duration in nanoseconds.
fn adts_frames(d: &[u8]) -> Vec<(&[u8], i64)> {
    const RATES: [i64; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let mut v = Vec::new();
    let mut i = 0;
    while i + 7 <= d.len() && d[i] == 0xff && d[i + 1] & 0xf0 == 0xf0 {
        let len = usize::from(d[i + 3] & 3) << 11 | usize::from(d[i + 4]) << 3 | usize::from(d[i + 5] >> 5);
        let Some(&rate) = RATES.get(usize::from(d[i + 2] >> 2 & 0x0f)) else {
            break;
        };
        if len < 7 || i + len > d.len() {
            break;
        }
        v.push((&d[i..i + len], 1024 * 1_000_000_000 / rate));
        i += len;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cuts a payload into 188-byte packets of one PID, padding the last with an adaptation field.
    fn packets(pid: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, c) in payload.chunks(184).enumerate() {
            let pusi = if n == 0 { 0x40 } else { 0 };
            out.extend_from_slice(&[0x47, pusi | (pid >> 8) as u8, pid as u8]);
            if c.len() == 184 {
                out.push(0x10);
            } else {
                let fill = 184 - c.len() - 1;
                out.push(0x30);
                out.push(fill as u8);
                if fill > 0 {
                    out.push(0);
                    out.extend(std::iter::repeat_n(0xff, fill - 1));
                }
            }
            out.extend_from_slice(c);
        }
        out
    }

    fn psi(table: u8, body: &[u8]) -> Vec<u8> {
        let len = body.len() + 5 + 4;
        let mut s = vec![0, table, 0xb0 | (len >> 8) as u8, len as u8, 0, 1, 0xc1, 0, 0];
        s.extend_from_slice(body);
        s.extend_from_slice(&[0; 4]);
        s
    }

    fn pes(stream_id: u8, pts: u64, data: &[u8], sized: bool) -> Vec<u8> {
        let p = [
            0x21 | ((pts >> 29) & 0x0e) as u8,
            (pts >> 22) as u8,
            ((pts >> 14) | 1) as u8,
            (pts >> 7) as u8,
            ((pts << 1) | 1) as u8,
        ];
        let len = if sized { data.len() + 8 } else { 0 };
        let mut v = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x80, 0x80, 5];
        v.extend_from_slice(&p);
        v.extend_from_slice(data);
        v
    }

    fn adts(payload: usize) -> Vec<u8> {
        let len = payload + 7;
        let mut f = vec![
            0xff,
            0xf1,
            0x40 | (3 << 2),
            0x80 | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ];
        f.extend(std::iter::repeat_n(0xaa, payload));
        f
    }

    #[test]
    fn demuxes_video_and_audio_from_the_first_keyframe() {
        let mut ts = packets(0, &psi(0, &[0, 1, 0xe1, 0x00]));
        ts.extend(packets(
            0x100,
            &psi(
                2,
                &[
                    0xe1, 0x01, 0xf0, 0, 0x1b, 0xe1, 0x01, 0xf0, 0, 0x06, 0xe1, 0x02, 0xf0, 3, 0x6a, 1, 0,
                ],
            ),
        ));
        let base = PTS_WRAP - 9000; // wraps between the two pictures
        let non_idr = [0, 0, 0, 1, 0x41, 1, 2, 3];
        let idr = [&[0, 0, 0, 1, 0x67, 9, 9][..], &[0, 0, 1, 0x65][..], &[7; 400][..]].concat();
        ts.extend(packets(0x101, &pes(0xe0, base - 3000, &non_idr, false)));
        ts.extend(packets(0x102, &pes(0xc0, base - 1000, &[1; 50], true)));
        ts.extend(packets(0x101, &pes(0xe0, base, &idr, false)));
        ts.extend(packets(0x101, &pes(0xe0, base + 18000, &non_idr, false)));
        ts.extend(packets(0x102, &pes(0xc0, base + 900, &[2; 300], true)));
        ts.extend(packets(0x101, &pes(0xe0, 0, &non_idr, false)));

        let mut d = Demux::default();
        let mut out = Vec::new();
        for c in ts.chunks(1000) {
            d.push(c, &mut out);
        }
        let Out::Streams { video, audio } = &out[0] else {
            panic!("no streams first")
        };
        assert_eq!((*video, *audio), (Some(Codec::H264), Some(Codec::Ac3)));
        let aus: Vec<_> = out[1..]
            .iter()
            .map(|o| match o {
                Out::Au(a) => (a.video, a.key, a.pts_ns, a.data.len()),
                _ => panic!("streams twice"),
            })
            .collect();
        assert_eq!(
            aus,
            [
                (true, true, 0, idr.len()),
                (false, true, 10_000_000, 300),
                (true, false, 200_000_000, 8)
            ]
        );
    }

    #[test]
    fn splits_adts_frames_with_their_times() {
        let d = [adts(10), adts(20)].concat();
        let f = adts_frames(&d);
        assert_eq!(
            f.iter().map(|(b, t)| (b.len(), *t)).collect::<Vec<_>>(),
            [(17, 21_333_333), (27, 21_333_333)]
        );
    }

    #[test]
    fn finds_h265_keyframes() {
        assert!(is_key(Codec::H265, &[0, 0, 1, 0x40, 1]));
        assert!(is_key(Codec::H265, &[0, 0, 1, 0x26, 1]));
        assert!(!is_key(Codec::H265, &[0, 0, 1, 0x02, 1]));
    }
}
