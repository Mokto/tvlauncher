//! The FFmpeg the package ships (libavformat, libavcodec and libavutil, LGPL, built by `tools/build-ffmpeg.sh` with a
//! short component list and no network code), bound at run time with `dlopen`: a binary linked against them would
//! not start on a TV where they are missing.
//!
//! The struct layouts and field offsets below are those of FFmpeg 9.0.2 for 32-bit ARM (the TV) and 64-bit (the
//! desktop simulator), taken from Native Jelly's `rust-modules/src/ff.rs` (GPL-3.0), which checks them against the
//! headers the libraries are built from. Majors 63 / 63 / 61 are asserted when the libraries load.
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::OnceLock;

pub mod convert;
pub mod demux;
pub mod lossless;
pub mod range;

pub enum AVInputFormat {}
pub enum AVIOContext {}
pub enum AVDictionary {}
pub enum AVCodec {}
pub enum AVCodecContext {}
pub enum AVStream {}
pub enum AVClass {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AVRational {
    pub num: c_int,
    pub den: c_int,
}

#[repr(C)]
pub struct AVBufferRef {
    _p: [u8; 0],
}

#[repr(C)]
pub struct AVPacket {
    pub buf: *mut AVBufferRef,
    pub pts: i64,
    pub dts: i64,
    pub data: *mut u8,
    pub size: c_int,
    pub stream_index: c_int,
    pub flags: c_int,
    pub side_data: *mut AVPacketSideData,
    pub side_data_elems: c_int,
    pub duration: i64,
    pub pos: i64,
    pub opaque: *mut c_void,
    pub opaque_ref: *mut AVBufferRef,
    pub time_base: AVRational,
}

#[repr(C)]
pub struct AVPacketSideData {
    pub data: *mut u8,
    pub size: usize,
    pub type_: c_int,
}

#[repr(C)]
pub struct AVChannelLayout {
    pub order: c_int,
    pub nb_channels: c_int,
    pub u_mask: u64,
    pub opaque: *mut c_void,
}

#[repr(C)]
pub struct AVCodecParameters {
    pub codec_type: c_int,
    pub codec_id: c_int,
    pub codec_tag: u32,
    pub extradata: *mut u8,
    pub extradata_size: c_int,
    pub coded_side_data: *mut AVPacketSideData,
    pub nb_coded_side_data: c_int,
    pub format: c_int,
    pub bit_rate: i64,
    pub bits_per_coded_sample: c_int,
    pub bits_per_raw_sample: c_int,
    pub profile: c_int,
    pub level: c_int,
    pub width: c_int,
    pub height: c_int,
    pub sample_aspect_ratio: AVRational,
    pub framerate: AVRational,
    pub field_order: c_int,
    pub color_range: c_int,
    pub color_primaries: c_int,
    pub color_trc: c_int,
    pub color_space: c_int,
    pub chroma_location: c_int,
    pub video_delay: c_int,
    pub ch_layout: AVChannelLayout,
    pub sample_rate: c_int,
    pub block_align: c_int,
    pub frame_size: c_int,
    pub initial_padding: c_int,
    pub trailing_padding: c_int,
    pub seek_preroll: c_int,
    pub alpha_mode: c_int,
}

/// Only as far as `streams`: what follows moves between majors.
#[repr(C)]
pub struct AVFormatContext {
    pub av_class: *const AVClass,
    pub iformat: *mut AVInputFormat,
    pub oformat: *mut c_void,
    pub priv_data: *mut c_void,
    pub pb: *mut AVIOContext,
    pub ctx_flags: c_int,
    pub nb_streams: c_uint,
    pub streams: *mut *mut AVStream,
}

#[repr(C)]
pub struct AVSubtitle {
    pub format: u16,
    pub start_display_time: u32,
    pub end_display_time: u32,
    pub num_rects: c_uint,
    pub rects: *mut *mut AVSubtitleRect,
    pub pts: i64,
}

#[repr(C)]
pub struct AVSubtitleRect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
    pub nb_colors: c_int,
    pub data: [*mut u8; 4],
    pub linesize: [c_int; 4],
    pub flags: c_int,
    pub type_: c_int,
    pub text: *mut c_char,
    pub ass: *mut c_char,
}

#[repr(C)]
pub struct AVDictionaryEntry {
    pub key: *mut c_char,
    pub value: *mut c_char,
}

#[cfg(target_pointer_width = "32")]
mod off {
    pub const STREAM_CODECPAR: usize = 12;
    pub const STREAM_TIME_BASE: usize = 20;
    pub const STREAM_METADATA: usize = 72;
    pub const FMT_DURATION: usize = 64;
    pub const FRAME_NB_SAMPLES: usize = 76;
    pub const FRAME_FORMAT: usize = 80;
    pub const FRAME_PTS: usize = 96;
    pub const FRAME_SAMPLE_RATE: usize = 132;
    pub const FRAME_CH_LAYOUT: usize = 256;
}
#[cfg(target_pointer_width = "64")]
mod off {
    pub const STREAM_CODECPAR: usize = 16;
    pub const STREAM_TIME_BASE: usize = 32;
    pub const STREAM_METADATA: usize = 80;
    pub const FMT_DURATION: usize = 104;
    pub const FRAME_NB_SAMPLES: usize = 112;
    pub const FRAME_FORMAT: usize = 116;
    pub const FRAME_PTS: usize = 136;
    pub const FRAME_SAMPLE_RATE: usize = 180;
    pub const FRAME_CH_LAYOUT: usize = 384;
}
pub(crate) use off::*;

pub const AVMEDIA_TYPE_VIDEO: c_int = 0;
pub const AVMEDIA_TYPE_AUDIO: c_int = 1;
pub const AVMEDIA_TYPE_SUBTITLE: c_int = 3;
pub const AV_PKT_FLAG_KEY: c_int = 1;
pub const AVSEEK_FLAG_BACKWARD: c_int = 1;
pub const AVSEEK_SIZE: c_int = 0x10000;
pub const AV_NOPTS_VALUE: i64 = i64::MIN;
pub const AVERROR_EOF: c_int = -541_478_725;
pub const AVERROR_EXIT: c_int = -1_414_092_869;
pub const NS_TB: AVRational = AVRational {
    num: 1,
    den: 1_000_000_000,
};
pub const AV_PKT_DATA_DOVI_CONF: c_int = 29;

macro_rules! table {
    ($(fn $name:ident($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        pub struct Fns {
            $(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }
        impl Fns {
            unsafe fn load(libs: &[*mut c_void]) -> Result<Fns, String> {
                Ok(Fns {$(
                    $name: {
                        let n = concat!(stringify!($name), "\0");
                        let n = CStr::from_bytes_with_nul_unchecked(n.as_bytes());
                        let p = libs.iter().map(|l| libc::dlsym(*l, n.as_ptr())).find(|p| !p.is_null());
                        match p {
                            Some(p) => std::mem::transmute_copy(&p),
                            None => return Err(format!("{} is missing from FFmpeg", n.to_string_lossy())),
                        }
                    },
                )*})
            }
        }
    };
}

table! {
    fn avformat_version() -> c_uint;
    fn avcodec_version() -> c_uint;
    fn avutil_version() -> c_uint;
    fn avformat_alloc_context() -> *mut AVFormatContext;
    fn avformat_open_input(*mut *mut AVFormatContext, *const c_char, *mut AVInputFormat, *mut *mut AVDictionary) -> c_int;
    fn avformat_find_stream_info(*mut AVFormatContext, *mut *mut AVDictionary) -> c_int;
    fn av_read_frame(*mut AVFormatContext, *mut AVPacket) -> c_int;
    fn av_seek_frame(*mut AVFormatContext, c_int, i64, c_int) -> c_int;
    fn avformat_close_input(*mut *mut AVFormatContext);
    fn avio_alloc_context(
        *mut u8, c_int, c_int, *mut c_void,
        Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        Option<extern "C" fn(*mut c_void, i64, c_int) -> i64>
    ) -> *mut AVIOContext;
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_free(*mut *mut AVPacket);
    fn av_packet_unref(*mut AVPacket);
    fn avcodec_get_name(c_int) -> *const c_char;
    fn avcodec_find_decoder(c_int) -> *const AVCodec;
    fn avcodec_find_encoder_by_name(*const c_char) -> *const AVCodec;
    fn avcodec_alloc_context3(*const AVCodec) -> *mut AVCodecContext;
    fn avcodec_parameters_to_context(*mut AVCodecContext, *const AVCodecParameters) -> c_int;
    fn avcodec_parameters_from_context(*mut AVCodecParameters, *const AVCodecContext) -> c_int;
    fn avcodec_parameters_alloc() -> *mut AVCodecParameters;
    fn avcodec_parameters_free(*mut *mut AVCodecParameters);
    fn avcodec_open2(*mut AVCodecContext, *const AVCodec, *mut *mut AVDictionary) -> c_int;
    fn avcodec_free_context(*mut *mut AVCodecContext);
    fn avcodec_decode_subtitle2(*mut AVCodecContext, *mut AVSubtitle, *mut c_int, *mut AVPacket) -> c_int;
    fn avsubtitle_free(*mut AVSubtitle);
    fn avcodec_send_packet(*mut AVCodecContext, *const AVPacket) -> c_int;
    fn avcodec_receive_frame(*mut AVCodecContext, *mut c_void) -> c_int;
    fn avcodec_send_frame(*mut AVCodecContext, *const c_void) -> c_int;
    fn avcodec_receive_packet(*mut AVCodecContext, *mut AVPacket) -> c_int;
    fn av_malloc(usize) -> *mut c_void;
    fn av_freep(*mut c_void);
    fn av_rescale_q(i64, AVRational, AVRational) -> i64;
    fn av_frame_alloc() -> *mut c_void;
    fn av_frame_free(*mut *mut c_void);
    fn av_frame_get_buffer(*mut c_void, c_int) -> c_int;
    fn av_frame_make_writable(*mut c_void) -> c_int;
    fn av_frame_unref(*mut c_void);
    fn av_opt_set(*mut c_void, *const c_char, *const c_char, c_int) -> c_int;
    fn av_dict_get(*const AVDictionary, *const c_char, *const AVDictionaryEntry, c_int) -> *const AVDictionaryEntry;
}

/// Where the libraries are looked for: `FFMPEG_DIR`, then `lib/` beside the program and in the install folder (the
/// package), then the desktop build `build/ffmpeg-host` (`tools/build-ffmpeg.sh host`).
fn dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = std::env::var_os("FFMPEG_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("lib")))
    {
        v.push(d);
    }
    // The program runs as a bind mount over LG's Home program, so its own path says nothing of where it came from.
    v.push(PathBuf::from(format!("{}/lib", crate::data::DIR)));
    v.push(PathBuf::from("build/ffmpeg-host"));
    v
}

unsafe fn open_lib(stem: &str, major: u32) -> Option<*mut c_void> {
    for d in dirs() {
        for name in [
            format!("lib{stem}-plx.so.{major}"),
            format!("lib{stem}-plx.{major}.dylib"),
        ] {
            let Ok(path) = CString::new(d.join(&name).to_string_lossy().into_owned()) else {
                continue;
            };
            let h = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            if !h.is_null() {
                return Some(h);
            }
        }
    }
    None
}

static FF: OnceLock<Result<Fns, String>> = OnceLock::new();

/// The loaded libraries, or why there are none. Loaded once.
pub fn lib() -> Result<&'static Fns, String> {
    FF.get_or_init(|| unsafe {
        // The dependency order: each library names the previous by SONAME, which only resolves when it is loaded.
        let util = open_lib("avutil", 61).ok_or("libavutil-plx is not installed")?;
        let codec = open_lib("avcodec", 63).ok_or("libavcodec-plx is not installed")?;
        let format = open_lib("avformat", 63).ok_or("libavformat-plx is not installed")?;
        let f = Fns::load(&[format, codec, util])?;
        let (a, b, c) = (
            (f.avformat_version)() >> 16,
            (f.avcodec_version)() >> 16,
            (f.avutil_version)() >> 16,
        );
        if (a, b, c) != (63, 63, 61) {
            return Err(format!("FFmpeg majors {a}/{b}/{c}, this build is for 63/63/61"));
        }
        Ok(f)
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub unsafe fn stream_codecpar(s: *mut AVStream) -> *mut AVCodecParameters {
    *((s as *const u8).add(STREAM_CODECPAR) as *const *mut AVCodecParameters)
}

pub unsafe fn stream_time_base(s: *mut AVStream) -> AVRational {
    *((s as *const u8).add(STREAM_TIME_BASE) as *const AVRational)
}

pub unsafe fn stream_tag(f: &Fns, s: *mut AVStream, key: &CStr) -> Option<String> {
    let dict = *((s as *const u8).add(STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() {
        return None;
    }
    let e = (f.av_dict_get)(dict, key.as_ptr(), std::ptr::null(), 0);
    if e.is_null() || (*e).value.is_null() {
        return None;
    }
    Some(CStr::from_ptr((*e).value).to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// `AV_DISPOSITION_FORCED`, in `AVStream.disposition`: the field sits 16 bytes before `metadata` (disposition, discard
/// and the 8-byte sample_aspect_ratio come between), which the offsets above agree with on both layouts.
pub unsafe fn stream_forced(s: *mut AVStream) -> bool {
    *((s as *const u8).add(STREAM_METADATA - 16) as *const c_int) & 0x40 != 0
}

pub unsafe fn fmt_duration(fmt: *const AVFormatContext) -> i64 {
    *((fmt as *const u8).add(FMT_DURATION) as *const i64)
}

pub unsafe fn codec_name(f: &Fns, id: c_int) -> String {
    CStr::from_ptr((f.avcodec_get_name)(id)).to_string_lossy().into_owned()
}

pub unsafe fn peek_i32(base: *const c_void, off: usize) -> i32 {
    *((base as *const u8).add(off) as *const i32)
}

pub unsafe fn poke_i32(base: *mut c_void, off: usize, v: i32) {
    *((base as *mut u8).add(off) as *mut i32) = v;
}

pub unsafe fn poke_i64(base: *mut c_void, off: usize, v: i64) {
    *((base as *mut u8).add(off) as *mut i64) = v;
}

pub unsafe fn frame_plane(frame: *const c_void, i: usize) -> *mut u8 {
    *((frame as *const u8).add(i * std::mem::size_of::<*mut u8>()) as *const *mut u8)
}
