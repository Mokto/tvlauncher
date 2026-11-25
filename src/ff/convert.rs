//! Audio the television cannot decode — TrueHD, FLAC, ALAC, Vorbis, LPCM — decoded here with the
//! bundled libavcodec and re-encoded to AC-3, which every webOS decoder plays. A track in one of
//! those formats (`devcaps::CONVERTED_AUDIO_CODECS`) then direct-plays: the server sends the file's
//! own bytes and converts nothing. The idea is SchulzCode/WebOS-Jellyfin-Native's (0BSD).
//!
//! **Output.** Mono, stereo or 5.1 AC-3 at the source's own rate (AC-3 takes 48, 44.1 and 32 kHz
//! only; any other rate is refused): 640 kb/s for 5.1, 256 kb/s for stereo, 192 kb/s for mono. 7.1
//! folds into 5.1 (each back channel joins its side channel at -3 dB). Measured on the webOS
//! 6.5.3 set, 2026-11-17: TrueHD 7.1 -> AC-3 5.1 costs ~14 % of one Cortex-A55 core, FLAC 5.1
//! ~4 %, at 10x and 32x real time.
//!
//! **Refusal is a route decision, not a silence.** A track this module cannot open (an unsupported
//! rate or channel layout, no decoder) is refused before the first audio access unit, through the
//! same `Shared::native_audio_header_refused` door an unfeedable Opus header uses: the open
//! recovery converts that audio on the server instead (`route::open_recovery`).
//!
//! **Timing.** Output time is counted in samples from an anchor: the first decoded audio after an
//! open or a seek takes its packet's timestamp, and every AC-3 frame after it is
//! `anchor + samples emitted / rate`. A packet that lands more than [`REANCHOR_NS`] away from that
//! count (a gap or overlap in the source) re-anchors on it and drops the partial frame in hand.
//!
//! The mixing, sample conversion and framing below are pure and host-tested; only [`Converter`]
//! touches FFmpeg.
use super::*;
use crate::sdl::log;
fn ff() -> &'static Fns {
    lib().expect("FFmpeg loaded before a track is opened")
}

/// The source rates AC-3 can carry.
const AC3_RATES: [i32; 3] = [48_000, 44_100, 32_000];

/// How far a packet's timestamp may stray from the sample count before the count is re-anchored.
pub const REANCHOR_NS: i64 = 100_000_000;

// FFmpeg's `AV_CH_*` channel bits (libavutil/channel_layout.h), stable across majors.
const FL: u64 = 1 << 0;
const FR: u64 = 1 << 1;
const FC: u64 = 1 << 2;
const LFE: u64 = 1 << 3;
const BL: u64 = 1 << 4;
const BR: u64 = 1 << 5;
const FLC: u64 = 1 << 6;
const FRC: u64 = 1 << 7;
const BC: u64 = 1 << 8;
const SL: u64 = 1 << 9;
const SR: u64 = 1 << 10;
/// `AV_CH_LAYOUT_5POINT1` (side surrounds): the AC-3 output's 5.1, in native order FL FR FC LFE SL SR.
const OUT_5_1: u64 = FL | FR | FC | LFE | SL | SR;

/// `AVChannelOrder`: `AV_CHANNEL_ORDER_UNSPEC` and `AV_CHANNEL_ORDER_NATIVE`.
const ORDER_UNSPEC: c_int = 0;
const ORDER_NATIVE: c_int = 1;

/// `AVSampleFormat`, a plain sequential enum unchanged since FFmpeg 1.x.
mod fmt {
    pub const U8: i32 = 0;
    pub const S16: i32 = 1;
    pub const S32: i32 = 2;
    pub const FLT: i32 = 3;
    pub const DBL: i32 = 4;
    pub const U8P: i32 = 5;
    pub const S16P: i32 = 6;
    pub const S32P: i32 = 7;
    pub const FLTP: i32 = 8;
    pub const DBLP: i32 = 9;
    pub const S64: i32 = 10;
    pub const S64P: i32 = 11;
}

/// How each source channel reaches the AC-3 output.
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    /// Output channels: 1, 2 or 6.
    pub out_channels: usize,
    /// The output layout's mask, in the same bits.
    pub out_mask: u64,
    /// FFmpeg's name for the output layout, for the encoder's `ch_layout` option.
    pub out_name: &'static str,
    /// Per source channel, in source order: (output channel, gain).
    pub routes: Vec<Vec<(usize, f32)>>,
}

/// The default layout FFmpeg gives a channel count it was told nothing else about.
fn default_mask(channels: i32) -> Option<u64> {
    match channels {
        1 => Some(FC),
        2 => Some(FL | FR),
        6 => Some(OUT_5_1),
        8 => Some(OUT_5_1 | BL | BR),
        _ => None,
    }
}

/// The mix for a source layout: `order`/`channels`/`mask` as `AVChannelLayout` carries them.
/// `None` for a layout the AC-3 output cannot represent without losing a channel (heights, wides,
/// custom maps), and for more than 7.1.
pub fn plan_mix(order: c_int, channels: i32, mask: u64) -> Option<Mix> {
    let mask = match order {
        ORDER_NATIVE if mask.count_ones() as i32 == channels => mask,
        ORDER_UNSPEC => default_mask(channels)?,
        _ => return None,
    };
    if !(1..=8).contains(&channels) {
        return None;
    }
    if mask & !(FL | FR | FC | LFE | BL | BR | FLC | FRC | BC | SL | SR) != 0 {
        return None;
    }
    if mask == FC {
        return Some(Mix {
            out_channels: 1,
            out_mask: FC,
            out_name: "mono",
            routes: vec![vec![(0, 1.0)]],
        });
    }
    if mask == FL | FR {
        return Some(Mix {
            out_channels: 2,
            out_mask: FL | FR,
            out_name: "stereo",
            routes: vec![vec![(0, 1.0)], vec![(1, 1.0)]],
        });
    }
    // Everything else becomes 5.1: fronts to fronts, every surround pair to the side pair.
    let target = |bit: u64| -> &'static [usize] {
        match bit {
            FL | FLC => &[0],
            FR | FRC => &[1],
            FC => &[2],
            LFE => &[3],
            SL | BL => &[4],
            SR | BR => &[5],
            BC => &[4, 5],
            _ => &[],
        }
    };
    let bits: Vec<u64> = (0..64).map(|i| 1u64 << i).filter(|b| mask & b != 0).collect();
    let mut sources = [0u32; 6];
    for b in &bits {
        for &o in target(*b) {
            sources[o] += 1;
        }
    }
    let routes = bits
        .iter()
        .map(|b| {
            let outs = target(*b);
            outs.iter()
                .map(|&o| {
                    // A channel shared out (the back centre) or joining another goes in at -3 dB, so
                    // two full-scale sources summed stay near full scale; the clamp catches the rest.
                    let gain = if outs.len() > 1 || sources[o] > 1 {
                        std::f32::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    (o, gain)
                })
                .collect()
        })
        .collect();
    Some(Mix {
        out_channels: 6,
        out_mask: OUT_5_1,
        out_name: "5.1",
        routes,
    })
}

/// Bytes per sample of a sample format, planar or not; `None` for one this module does not read.
pub fn bytes_per_sample(format: i32) -> Option<usize> {
    match format {
        fmt::U8 | fmt::U8P => Some(1),
        fmt::S16 | fmt::S16P => Some(2),
        fmt::S32 | fmt::S32P | fmt::FLT | fmt::FLTP => Some(4),
        fmt::DBL | fmt::DBLP | fmt::S64 | fmt::S64P => Some(8),
        _ => None,
    }
}

fn is_planar(format: i32) -> bool {
    (fmt::U8P..=fmt::DBLP).contains(&format) || format == fmt::S64P
}

/// One sample as a float in [-1, 1].
fn sample(format: i32, b: &[u8]) -> f32 {
    match format {
        fmt::U8 | fmt::U8P => (f32::from(b[0]) - 128.0) / 128.0,
        fmt::S16 | fmt::S16P => f32::from(i16::from_ne_bytes([b[0], b[1]])) / 32_768.0,
        fmt::S32 | fmt::S32P => i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        fmt::FLT | fmt::FLTP => f32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
        fmt::DBL | fmt::DBLP => f64::from_ne_bytes(b[..8].try_into().unwrap()) as f32,
        fmt::S64 | fmt::S64P => {
            (i64::from_ne_bytes(b[..8].try_into().unwrap()) as f64 / 9_223_372_036_854_775_808.0) as f32
        }
        _ => 0.0,
    }
}

/// Convert `samples` frames of decoded audio (`planes` as the decoder left them: one plane per
/// channel when planar, else one interleaved plane) through `mix`, appending to `out`, one `Vec`
/// per output channel. False when the planes are shorter than the frame says.
pub fn mix_into(
    format: i32,
    planes: &[&[u8]],
    channels: usize,
    samples: usize,
    mix: &Mix,
    out: &mut [Vec<f32>],
) -> bool {
    let Some(bps) = bytes_per_sample(format) else {
        return false;
    };
    let planar = is_planar(format);
    let need = if planar {
        samples * bps
    } else {
        samples * channels * bps
    };
    let wanted_planes = if planar { channels } else { 1 };
    if planes.len() < wanted_planes
        || planes[..wanted_planes].iter().any(|p| p.len() < need)
        || mix.routes.len() != channels
    {
        return false;
    }
    let base: Vec<usize> = out.iter().map(Vec::len).collect();
    for o in out.iter_mut() {
        o.resize(o.len() + samples, 0.0);
    }
    for (ch, routes) in mix.routes.iter().enumerate() {
        for i in 0..samples {
            let at = if planar {
                &planes[ch][i * bps..]
            } else {
                &planes[0][(i * channels + ch) * bps..]
            };
            let v = sample(format, at);
            for &(o, gain) in routes {
                out[o][base[o] + i] += v * gain;
            }
        }
    }
    for (o, start) in out.iter_mut().zip(base) {
        for v in &mut o[start..] {
            *v = v.clamp(-1.0, 1.0);
        }
    }
    true
}

/// Mixed samples waiting for a whole AC-3 frame, and the clock that stamps each frame.
#[derive(Debug)]
pub struct Fifo {
    pub chans: Vec<Vec<f32>>,
    rate: i64,
    /// Content time of the first sample counted by `emitted`.
    base_ns: Option<i64>,
    /// Samples handed out since `base_ns`.
    emitted: i64,
}

impl Fifo {
    pub fn new(channels: usize, rate: i32) -> Self {
        Self {
            chans: vec![Vec::new(); channels],
            rate: i64::from(rate),
            base_ns: None,
            emitted: 0,
        }
    }

    fn pending(&self) -> i64 {
        self.chans[0].len() as i64
    }

    fn at(&self, samples: i64) -> i64 {
        let base = self.base_ns.unwrap_or(0);
        base + (i128::from(samples) * 1_000_000_000 / i128::from(self.rate)) as i64
    }

    /// Before appending decoded audio whose packet said `pts_ns`. False while there is no anchor
    /// to count from yet (no timestamp seen since the reset): the caller drops that audio.
    pub fn anchor(&mut self, pts_ns: Option<i64>) -> bool {
        match (self.base_ns, pts_ns) {
            (None, None) => false,
            (None, Some(pts)) => {
                self.base_ns = Some(pts);
                self.emitted = 0;
                true
            }
            (Some(_), None) => true,
            (Some(_), Some(pts)) => {
                let expected = self.at(self.emitted + self.pending());
                if (pts - expected).abs() > REANCHOR_NS {
                    for c in &mut self.chans {
                        c.clear();
                    }
                    self.base_ns = Some(pts);
                    self.emitted = 0;
                }
                true
            }
        }
    }

    /// Take one frame of `n` samples per channel, if there is one: its content time and samples.
    pub fn pop(&mut self, n: usize) -> Option<(i64, Vec<Vec<f32>>)> {
        if n == 0 || self.chans[0].len() < n {
            return None;
        }
        let pts = self.at(self.emitted);
        self.emitted += n as i64;
        Some((pts, self.chans.iter_mut().map(|c| c.drain(..n).collect()).collect()))
    }
}

/// The AC-3 bit rate for an output width.
pub fn ac3_bitrate(channels: usize) -> i64 {
    match channels {
        1 => 192_000,
        2 => 256_000,
        _ => 640_000,
    }
}

/// Whether `Converter::open` will take this track: a rate and a channel layout the AC-3 encoder can carry.
pub unsafe fn can_convert(cp: *const AVCodecParameters) -> bool {
    let layout = &(*cp).ch_layout;
    AC3_RATES.contains(&(*cp).sample_rate) && plan_mix(layout.order, layout.nb_channels, layout.u_mask).is_some()
}

/// The decoder, the AC-3 encoder and the mix between them, for one opened track.
pub struct Converter {
    dec: *mut AVCodecContext,
    enc: *mut AVCodecContext,
    dec_frame: *mut c_void,
    enc_frame: *mut c_void,
    pkt: *mut AVPacket,
    mix: Mix,
    fifo: Fifo,
    frame_size: usize,
    rate: i32,
    in_channels: i32,
    /// Packets the decoder rejected, for one log line per run of them.
    bad_packets: u32,
    /// "TRUEHD 8ch -> AC3 5.1 640k", for the log.
    pub desc: String,
}

// The contexts are only ever touched by the demux thread that owns the Converter.
unsafe impl Send for Converter {}

impl Converter {
    /// Open a converter for the stream `cp` describes, or say why not.
    pub unsafe fn open(cp: *const AVCodecParameters) -> Result<Converter, String> {
        let name = std::ffi::CStr::from_ptr((ff().avcodec_get_name)((*cp).codec_id))
            .to_string_lossy()
            .into_owned();
        let rate = (*cp).sample_rate;
        if !AC3_RATES.contains(&rate) {
            return Err(format!("{name} at {rate} Hz: AC-3 carries 48, 44.1 and 32 kHz only"));
        }
        let layout = &(*cp).ch_layout;
        let Some(mix) = plan_mix(layout.order, layout.nb_channels, layout.u_mask) else {
            return Err(format!(
                "{name} layout order={} channels={} mask={:#x} has no AC-3 mix",
                layout.order, layout.nb_channels, layout.u_mask
            ));
        };
        let codec = (ff().avcodec_find_decoder)((*cp).codec_id);
        if codec.is_null() {
            return Err(format!("no {name} decoder in this build"));
        }
        let mut c = Converter {
            dec: (ff().avcodec_alloc_context3)(codec),
            enc: std::ptr::null_mut(),
            dec_frame: (ff().av_frame_alloc)(),
            enc_frame: (ff().av_frame_alloc)(),
            pkt: (ff().av_packet_alloc)(),
            fifo: Fifo::new(mix.out_channels, rate),
            frame_size: 0,
            rate,
            in_channels: layout.nb_channels,
            bad_packets: 0,
            desc: String::new(),
            mix,
        };
        if c.dec.is_null() || c.dec_frame.is_null() || c.enc_frame.is_null() || c.pkt.is_null() {
            return Err("allocation failed".into());
        }
        if (ff().avcodec_parameters_to_context)(c.dec, cp) < 0
            || (ff().avcodec_open2)(c.dec, codec, std::ptr::null_mut()) < 0
        {
            return Err(format!("{name} decoder would not open"));
        }
        let ac3 = (ff().avcodec_find_encoder_by_name)(c"ac3".as_ptr());
        if ac3.is_null() {
            return Err("no AC-3 encoder in this build".into());
        }
        c.enc = (ff().avcodec_alloc_context3)(ac3);
        if c.enc.is_null() {
            return Err("allocation failed".into());
        }
        let bitrate = ac3_bitrate(c.mix.out_channels);
        // Format, rate and layout go in through a library-allocated AVCodecParameters (its fields
        // are modelled and asserted; AVCodecContext's are not, and FFmpeg 9 has no `sample_fmt`
        // option): read the fresh context out, set the three, copy it back. The bit rate is an
        // ordinary option, set after, since the copy carries the parameters' own (zero) rate.
        let par = (ff().avcodec_parameters_alloc)();
        if par.is_null() {
            return Err("allocation failed".into());
        }
        let copied = (ff().avcodec_parameters_from_context)(par, c.enc) >= 0 && {
            (*par).format = fmt::FLTP;
            (*par).sample_rate = rate;
            (*par).ch_layout = AVChannelLayout {
                order: ORDER_NATIVE,
                nb_channels: c.mix.out_channels as c_int,
                u_mask: c.mix.out_mask,
                opaque: std::ptr::null_mut(),
            };
            (ff().avcodec_parameters_to_context)(c.enc, par) >= 0
        };
        (ff().avcodec_parameters_free)(&mut { par });
        if !copied {
            return Err("AC-3 encoder parameters would not copy".into());
        }
        let b = std::ffi::CString::new(bitrate.to_string()).unwrap();
        let r = (ff().av_opt_set)(c.enc as *mut c_void, c"b".as_ptr(), b.as_ptr(), 0);
        if r < 0 {
            return Err(format!("AC-3 encoder refused b={bitrate} ({r})"));
        }
        let r = (ff().avcodec_open2)(c.enc, ac3, std::ptr::null_mut());
        if r < 0 {
            return Err(format!("AC-3 encoder would not open ({r})"));
        }
        // Read the encoder's settings back through the library's own copy (sizes are its
        // problem): the rate, width, format and frame size it actually opened with.
        let par = (ff().avcodec_parameters_alloc)();
        if par.is_null() {
            return Err("allocation failed".into());
        }
        let ok = (ff().avcodec_parameters_from_context)(par, c.enc) >= 0
            && (*par).sample_rate == rate
            && (*par).ch_layout.nb_channels as usize == c.mix.out_channels
            && (*par).format == fmt::FLTP
            && (*par).frame_size > 0;
        let frame_size = (*par).frame_size;
        (ff().avcodec_parameters_free)(&mut { par });
        if !ok {
            return Err("AC-3 encoder self-check failed".into());
        }
        c.frame_size = frame_size as usize;
        let f = c.enc_frame;
        poke_i32(f, FRAME_NB_SAMPLES, frame_size);
        poke_i32(f, FRAME_FORMAT, fmt::FLTP);
        poke_i32(f, FRAME_SAMPLE_RATE, rate);
        *((f as *mut u8).add(FRAME_CH_LAYOUT) as *mut AVChannelLayout) = AVChannelLayout {
            order: ORDER_NATIVE,
            nb_channels: c.mix.out_channels as c_int,
            u_mask: c.mix.out_mask,
            opaque: std::ptr::null_mut(),
        };
        if (ff().av_frame_get_buffer)(f, 0) < 0 {
            return Err("AC-3 frame buffer allocation failed".into());
        }
        c.desc = format!(
            "{} {}ch -> AC3 {} {}k",
            name.to_uppercase(),
            c.in_channels,
            c.mix.out_name,
            bitrate / 1000
        );
        Ok(c)
    }

    /// Decode one demuxed packet and append the finished AC-3 frames to `out` as (content time ns,
    /// bytes). `pts_ns` is the packet's own timestamp. An undecodable packet is skipped; `Err` is
    /// a stream that changed shape mid-play, which the caller treats as unfeedable.
    pub unsafe fn push(
        &mut self,
        pkt: *mut AVPacket,
        pts_ns: Option<i64>,
        out: &mut Vec<(i64, Vec<u8>)>,
    ) -> Result<(), String> {
        if (ff().avcodec_send_packet)(self.dec, pkt) < 0 {
            self.bad_packets += 1;
            if self.bad_packets == 1 {
                log("ff: audio convert: the decoder rejected a packet; skipping");
            }
            return Ok(());
        }
        self.bad_packets = 0;
        let f = self.dec_frame;
        let mut first = true;
        while (ff().avcodec_receive_frame)(self.dec, f) == 0 {
            let samples = peek_i32(f, FRAME_NB_SAMPLES).max(0) as usize;
            let format = peek_i32(f, FRAME_FORMAT);
            let rate = peek_i32(f, FRAME_SAMPLE_RATE);
            let channels = peek_i32(f, FRAME_CH_LAYOUT + 4);
            if rate != self.rate || channels != self.in_channels {
                (ff().av_frame_unref)(f);
                return Err(format!("source changed to {rate} Hz / {channels}ch mid-stream"));
            }
            // Only the packet's first frame carries its timestamp; the rest continue the count.
            let anchored = self.fifo.anchor(if first { pts_ns } else { None });
            first = false;
            if anchored {
                let Some(bps) = bytes_per_sample(format) else {
                    (ff().av_frame_unref)(f);
                    return Err(format!(
                        "decoder sample format {format} is not one this converter reads"
                    ));
                };
                let ch = channels as usize;
                let (n_planes, len) = if is_planar(format) {
                    (ch, samples * bps)
                } else {
                    (1, samples * ch * bps)
                };
                let planes: Vec<&[u8]> = (0..n_planes)
                    .map(|i| {
                        let p = frame_plane(f, i);
                        if p.is_null() {
                            &[][..]
                        } else {
                            std::slice::from_raw_parts(p, len)
                        }
                    })
                    .collect();
                if !mix_into(format, &planes, ch, samples, &self.mix, &mut self.fifo.chans) {
                    (ff().av_frame_unref)(f);
                    return Err("decoded frame is shorter than it says".into());
                }
            }
            (ff().av_frame_unref)(f);
            self.encode(out)?;
        }
        Ok(())
    }

    unsafe fn encode(&mut self, out: &mut Vec<(i64, Vec<u8>)>) -> Result<(), String> {
        while let Some((pts, planes)) = self.fifo.pop(self.frame_size) {
            let f = self.enc_frame;
            if (ff().av_frame_make_writable)(f) < 0 {
                return Err("AC-3 frame not writable".into());
            }
            for (c, plane) in planes.iter().enumerate() {
                std::ptr::copy_nonoverlapping(plane.as_ptr(), frame_plane(f, c) as *mut f32, self.frame_size);
            }
            poke_i64(
                f,
                FRAME_PTS,
                (i128::from(pts) * i128::from(self.rate) / 1_000_000_000) as i64,
            );
            if (ff().avcodec_send_frame)(self.enc, f) < 0 {
                return Err("AC-3 encoder refused a frame".into());
            }
            while (ff().avcodec_receive_packet)(self.enc, self.pkt) == 0 {
                let p = self.pkt;
                if !(*p).data.is_null() && (*p).size > 0 {
                    out.push((pts, std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec()));
                }
                (ff().av_packet_unref)(p);
            }
        }
        Ok(())
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        unsafe {
            (ff().av_packet_free)(&mut self.pkt);
            (ff().av_frame_free)(&mut self.enc_frame);
            (ff().av_frame_free)(&mut self.dec_frame);
            if !self.enc.is_null() {
                (ff().avcodec_free_context)(&mut self.enc);
            }
            if !self.dec.is_null() {
                (ff().avcodec_free_context)(&mut self.dec);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE: c_int = ORDER_NATIVE;

    #[test]
    fn mono_and_stereo_pass_straight_through() {
        let mono = plan_mix(NATIVE, 1, FC).unwrap();
        assert_eq!((mono.out_channels, mono.out_name), (1, "mono"));
        let stereo = plan_mix(NATIVE, 2, FL | FR).unwrap();
        assert_eq!((stereo.out_channels, stereo.out_name), (2, "stereo"));
        assert_eq!(stereo.routes, vec![vec![(0, 1.0)], vec![(1, 1.0)]]);
    }

    #[test]
    fn five_one_maps_one_to_one_whichever_surround_pair_it_names() {
        let side = plan_mix(NATIVE, 6, OUT_5_1).unwrap();
        assert_eq!(side.out_name, "5.1");
        assert_eq!(side.routes, (0..6).map(|o| vec![(o, 1.0)]).collect::<Vec<_>>());
        // 5.1(back) in native order: FL FR FC LFE BL BR — the backs become the sides.
        let back = plan_mix(NATIVE, 6, FL | FR | FC | LFE | BL | BR).unwrap();
        assert_eq!(back.routes, (0..6).map(|o| vec![(o, 1.0)]).collect::<Vec<_>>());
    }

    #[test]
    fn seven_one_folds_each_back_channel_into_its_side_at_minus_3_db() {
        // Native order: FL FR FC LFE BL BR SL SR.
        let m = plan_mix(NATIVE, 8, OUT_5_1 | BL | BR).unwrap();
        let h = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(m.out_channels, 6);
        assert_eq!(
            m.routes,
            vec![
                vec![(0, 1.0)],
                vec![(1, 1.0)],
                vec![(2, 1.0)],
                vec![(3, 1.0)],
                vec![(4, h)],
                vec![(5, h)],
                vec![(4, h)],
                vec![(5, h)],
            ]
        );
    }

    #[test]
    fn an_unspecified_order_takes_ffmpegs_default_for_the_count() {
        assert_eq!(plan_mix(ORDER_UNSPEC, 2, 0).unwrap().out_name, "stereo");
        assert_eq!(plan_mix(ORDER_UNSPEC, 8, 0).unwrap().routes.len(), 8);
        assert!(plan_mix(ORDER_UNSPEC, 5, 0).is_none(), "no default this module trusts");
    }

    #[test]
    fn layouts_without_an_honest_ac3_mix_are_refused() {
        const TOP_CENTER: u64 = 1 << 11;
        assert!(
            plan_mix(NATIVE, 7, OUT_5_1 | TOP_CENTER).is_none(),
            "a height channel would be lost"
        );
        assert!(plan_mix(NATIVE, 3, FL | FR).is_none(), "mask and count disagree");
        assert!(plan_mix(2, 2, 0).is_none(), "custom channel maps");
        assert!(
            plan_mix(NATIVE, 10, OUT_5_1 | BL | BR | FLC | FRC).is_none(),
            "past 7.1"
        );
    }

    #[test]
    fn three_point_oh_and_quad_become_5_1_with_silent_gaps() {
        let surround = plan_mix(NATIVE, 3, FL | FR | FC).unwrap();
        assert_eq!(surround.routes, vec![vec![(0, 1.0)], vec![(1, 1.0)], vec![(2, 1.0)]]);
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let four = plan_mix(NATIVE, 4, FL | FR | FC | BC).unwrap();
        assert_eq!(four.routes[3], vec![(4, h), (5, h)], "the back centre feeds both sides");
    }

    fn bytes<T: Copy, const N: usize>(values: &[T], to: impl Fn(T) -> [u8; N]) -> Vec<u8> {
        values.iter().flat_map(|v| to(*v)).collect()
    }

    #[test]
    fn interleaved_integer_samples_are_scaled_and_deinterleaved() {
        let mix = plan_mix(NATIVE, 2, FL | FR).unwrap();
        let s16 = bytes(&[16_384i16, -32_768, 0, 32_767], i16::to_ne_bytes);
        let mut out = vec![Vec::new(), Vec::new()];
        assert!(mix_into(fmt::S16, &[&s16], 2, 2, &mix, &mut out));
        assert_eq!(out[0], vec![0.5, 0.0]);
        assert_eq!(out[1][0], -1.0);
        assert!((out[1][1] - 1.0).abs() < 1e-4);
        // TrueHD's own output: 24-bit audio in the top of an s32.
        let s32 = bytes(&[i32::MIN, 1 << 30], i32::to_ne_bytes);
        let mut out = vec![Vec::new(), Vec::new()];
        assert!(mix_into(fmt::S32, &[&s32], 2, 1, &mix, &mut out));
        assert_eq!((out[0][0], out[1][0]), (-1.0, 0.5));
    }

    #[test]
    fn planar_float_and_u8_read_one_plane_per_channel() {
        let mix = plan_mix(NATIVE, 2, FL | FR).unwrap();
        let left = bytes(&[0.25f32, -0.25], f32::to_ne_bytes);
        let right = bytes(&[0.75f32, 2.0], f32::to_ne_bytes);
        let mut out = vec![Vec::new(), Vec::new()];
        assert!(mix_into(fmt::FLTP, &[&left, &right], 2, 2, &mix, &mut out));
        assert_eq!(out, vec![vec![0.25, -0.25], vec![0.75, 1.0]], "out of range clamps");
        let u8s = [128u8, 0];
        let mut out = vec![Vec::new()];
        let mono = plan_mix(NATIVE, 1, FC).unwrap();
        assert!(mix_into(fmt::U8P, &[&u8s], 1, 2, &mono, &mut out));
        assert_eq!(out[0], vec![0.0, -1.0]);
    }

    #[test]
    fn a_fold_sums_into_the_side_channel_and_appends_after_what_is_queued() {
        let m = plan_mix(NATIVE, 8, OUT_5_1 | BL | BR).unwrap();
        // One sample: every channel at 0.5.
        let frame = bytes(&[0.5f32; 8], f32::to_ne_bytes);
        let mut out = vec![vec![9.0]; 6];
        assert!(mix_into(fmt::FLT, &[&frame], 8, 1, &m, &mut out));
        assert_eq!(out[0], vec![9.0, 0.5], "appended, the queued sample untouched");
        let side = out[4][1];
        assert!((side - 0.5 * 2.0 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn short_planes_and_unknown_formats_are_refused() {
        let mix = plan_mix(NATIVE, 2, FL | FR).unwrap();
        let mut out = vec![Vec::new(), Vec::new()];
        assert!(!mix_into(fmt::S16, &[&[0u8; 6]], 2, 2, &mix, &mut out));
        assert!(!mix_into(99, &[&[0u8; 64]], 2, 2, &mix, &mut out));
        assert!(
            !mix_into(fmt::S16P, &[&[0u8; 4]], 2, 2, &mix, &mut out),
            "one plane for two channels"
        );
        assert!(out.iter().all(Vec::is_empty), "nothing appended on refusal");
    }

    fn fill(f: &mut Fifo, n: usize) {
        for c in &mut f.chans {
            c.extend(std::iter::repeat_n(0.0, n));
        }
    }

    #[test]
    fn frames_are_stamped_by_sample_count_from_the_first_timestamp() {
        let mut f = Fifo::new(2, 48_000);
        assert!(!f.anchor(None), "nothing to count from yet");
        assert!(f.anchor(Some(10_000_000_000)));
        fill(&mut f, 1536 * 2 + 10);
        assert_eq!(f.pop(1536).unwrap().0, 10_000_000_000);
        assert_eq!(
            f.pop(1536).unwrap().0,
            10_032_000_000,
            "1536 samples at 48 kHz are 32 ms"
        );
        assert!(f.pop(1536).is_none(), "10 samples are not a frame");
        assert_eq!(f.chans[0].len(), 10);
    }

    #[test]
    fn a_timestamp_close_to_the_count_keeps_it_and_a_far_one_reanchors() {
        let mut f = Fifo::new(1, 48_000);
        f.anchor(Some(0));
        fill(&mut f, 4800); // 100 ms queued
        assert!(f.anchor(Some(100_000_000 + REANCHOR_NS / 2)), "jitter keeps the count");
        assert_eq!(f.chans[0].len(), 4800);
        assert!(f.anchor(Some(5_000_000_000)));
        assert!(f.chans[0].is_empty(), "a gap drops the partial frame");
        fill(&mut f, 1536);
        assert_eq!(f.pop(1536).unwrap().0, 5_000_000_000);
    }

    #[test]
    fn bitrates_follow_the_output_width() {
        assert_eq!(
            (ac3_bitrate(1), ac3_bitrate(2), ac3_bitrate(6)),
            (192_000, 256_000, 640_000)
        );
    }
}
