//! Styled ASS/SSA subtitles through libass (`libass-plx`, built by `tools/build-libass.sh` with its own FreeType,
//! FriBidi and HarfBuzz and no font provider), bound at run time with `dlopen` like FFmpeg: without the library the
//! player shows the text of these tracks as plain lines, as before.
//!
//! The demuxer hands over the track's header, the fonts attached to the file and each packet; [`Ass::render`] gives
//! the pictures to put on screen at a time, as colour masks the player turns into textures. The structures read here
//! (`ASS_Image`) are libass's public ones.
use std::ffi::{c_char, c_int, c_longlong, c_void, CString};
use std::sync::OnceLock;

#[repr(C)]
struct AssImage {
    w: c_int,
    h: c_int,
    stride: c_int,
    bitmap: *const u8,
    /// 0xRRGGBBTT, where TT is the transparency (0 is opaque).
    color: u32,
    dst_x: c_int,
    dst_y: c_int,
    next: *const AssImage,
    _kind: c_int,
}

struct Fns {
    library_init: unsafe extern "C" fn() -> *mut c_void,
    library_done: unsafe extern "C" fn(*mut c_void),
    set_message_cb: unsafe extern "C" fn(
        *mut c_void,
        unsafe extern "C" fn(c_int, *const c_char, *mut c_void, *mut c_void),
        *mut c_void,
    ),
    add_font: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int),
    renderer_init: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    renderer_done: unsafe extern "C" fn(*mut c_void),
    set_frame_size: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    set_storage_size: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    set_fonts: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int, *const c_char, c_int),
    new_track: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    free_track: unsafe extern "C" fn(*mut c_void),
    process_codec_private: unsafe extern "C" fn(*mut c_void, *const c_char, c_int),
    process_chunk: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_longlong, c_longlong),
    render_frame: unsafe extern "C" fn(*mut c_void, *mut c_void, c_longlong, *mut c_int) -> *const AssImage,
}

/// A symbol as the function pointer type the field wants.
unsafe fn cast<T: Copy>(p: *mut c_void) -> T {
    std::mem::transmute_copy(&p)
}

static LIB: OnceLock<Option<Fns>> = OnceLock::new();

fn lib() -> Option<&'static Fns> {
    LIB.get_or_init(|| unsafe {
        let mut found = None;
        'dirs: for d in super::dirs() {
            for name in ["libass-plx.so.0", "libass-plx.0.dylib"] {
                let Ok(path) = CString::new(d.join(name).to_string_lossy().into_owned()) else {
                    continue;
                };
                // Local: its own symbols must not meet another library's.
                let h = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if !h.is_null() {
                    found = Some(h);
                    break 'dirs;
                }
            }
        }
        let h = found?;
        let sym = |n: &std::ffi::CStr| {
            let p = libc::dlsym(h, n.as_ptr());
            (!p.is_null()).then_some(p)
        };
        macro_rules! f {
            ($n:literal) => {
                cast(sym($n)?)
            };
        }
        Some(Fns {
            library_init: f!(c"ass_library_init"),
            library_done: f!(c"ass_library_done"),
            set_message_cb: f!(c"ass_set_message_cb"),
            add_font: f!(c"ass_add_font"),
            renderer_init: f!(c"ass_renderer_init"),
            renderer_done: f!(c"ass_renderer_done"),
            set_frame_size: f!(c"ass_set_frame_size"),
            set_storage_size: f!(c"ass_set_storage_size"),
            set_fonts: f!(c"ass_set_fonts"),
            new_track: f!(c"ass_new_track"),
            free_track: f!(c"ass_free_track"),
            process_codec_private: f!(c"ass_process_codec_private"),
            process_chunk: f!(c"ass_process_chunk"),
            render_frame: f!(c"ass_render_frame"),
        })
    })
    .as_ref()
}

/// libass's messages (a font it did not find, a line it could not read) say nothing the player can use. The `va_list`
/// is never read, so a pointer stands in for it.
unsafe extern "C" fn quiet(_level: c_int, _fmt: *const c_char, _args: *mut c_void, _data: *mut c_void) {}

/// Whether libass is installed.
pub fn available() -> bool {
    lib().is_some()
}

/// One mask of a frame: where it goes on the frame and its pixels as R, G, B, A (the colour already applied to the
/// mask).
pub struct Picture {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

/// A renderer with one track, for one run of the film.
pub struct Ass {
    f: &'static Fns,
    library: *mut c_void,
    renderer: *mut c_void,
    track: *mut c_void,
}

// Used by the player thread alone.
unsafe impl Send for Ass {}

impl Ass {
    /// `header` is the track's codec private data (the script up to its events), `fonts` the attachments of the
    /// file, `default_font` a font file for what no attachment covers, `frame` the picture the subtitles are placed
    /// on and `storage` the video's own size, which the script's coordinates relate to.
    pub fn new(
        header: &[u8],
        fonts: &[(String, Vec<u8>)],
        default_font: Option<&str>,
        frame: (i32, i32),
        storage: (i32, i32),
    ) -> Option<Ass> {
        let f = lib()?;
        unsafe {
            let library = (f.library_init)();
            if library.is_null() {
                return None;
            }
            (f.set_message_cb)(library, quiet, std::ptr::null_mut());
            for (name, data) in fonts {
                if let Ok(n) = CString::new(name.as_str()) {
                    (f.add_font)(library, n.as_ptr(), data.as_ptr().cast(), data.len() as c_int);
                }
            }
            let renderer = (f.renderer_init)(library);
            let track = (f.new_track)(library);
            if renderer.is_null() || track.is_null() {
                if !track.is_null() {
                    (f.free_track)(track);
                }
                if !renderer.is_null() {
                    (f.renderer_done)(renderer);
                }
                (f.library_done)(library);
                return None;
            }
            (f.set_frame_size)(renderer, frame.0, frame.1);
            (f.set_storage_size)(renderer, storage.0.max(1), storage.1.max(1));
            let font = default_font.and_then(|p| CString::new(p).ok());
            // 0 is no font provider: only the attachments and the default font are known.
            (f.set_fonts)(
                renderer,
                font.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                c"sans-serif".as_ptr(),
                0,
                std::ptr::null(),
                1,
            );
            (f.process_codec_private)(track, header.as_ptr().cast(), header.len() as c_int);
            Some(Ass {
                f,
                library,
                renderer,
                track,
            })
        }
    }

    /// One packet of the track: Matroska's `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`.
    pub fn chunk(&mut self, data: &[u8], start_ms: i64, dur_ms: i64) {
        unsafe {
            (self.f.process_chunk)(
                self.track,
                data.as_ptr().cast(),
                data.len() as c_int,
                start_ms,
                dur_ms.max(1),
            )
        }
    }

    /// The pictures at `now_ms`, or None when they are the same as at the last call.
    pub fn render(&mut self, now_ms: i64) -> Option<Vec<Picture>> {
        let mut changed: c_int = 0;
        let mut p = unsafe { (self.f.render_frame)(self.renderer, self.track, now_ms, &mut changed) };
        if changed == 0 {
            return None;
        }
        let mut out = vec![];
        while !p.is_null() {
            let im = unsafe { &*p };
            if im.w > 0 && im.h > 0 {
                out.push(picture(im));
            }
            p = im.next;
        }
        Some(out)
    }
}

fn picture(im: &AssImage) -> Picture {
    let (r, g, b) = ((im.color >> 24) as u8, (im.color >> 16) as u8, (im.color >> 8) as u8);
    let opaque = 255 - (im.color & 0xff);
    let mut rgba = Vec::with_capacity((im.w * im.h * 4) as usize);
    for y in 0..im.h as usize {
        let row = unsafe { std::slice::from_raw_parts(im.bitmap.add(y * im.stride as usize), im.w as usize) };
        for &m in row {
            rgba.extend_from_slice(&[r, g, b, (u32::from(m) * opaque / 255) as u8]);
        }
    }
    Picture {
        x: im.dst_x,
        y: im.dst_y,
        w: im.w,
        h: im.h,
        rgba,
    }
}

impl Drop for Ass {
    fn drop(&mut self) {
        unsafe {
            (self.f.free_track)(self.track);
            (self.f.renderer_done)(self.renderer);
            (self.f.library_done)(self.library);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs `build/libass-host` (`sh tools/build-libass.sh host`) and a font: `ASS_TEST_FONT=/path/font.ttf`.
    #[test]
    #[ignore = "needs libass-host and ASS_TEST_FONT"]
    fn a_styled_line_is_drawn() {
        let font = std::env::var("ASS_TEST_FONT").expect("ASS_TEST_FONT");
        let header = b"[Script Info]\nScriptType: v4.00+\nPlayResX: 640\nPlayResY: 360\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Sans,40,&H0000FF00,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
        let mut a = Ass::new(header, &[], Some(&font), (1920, 1080), (640, 360)).expect("libass");
        a.chunk(b"0,0,Default,,0,0,0,,Hello", 1000, 2000);
        assert!(a.render(500).is_none_or(|p| p.is_empty()), "nothing before it starts");
        let pics = a.render(1500).expect("a frame");
        if let Ok(out) = std::env::var("ASS_TEST_OUT") {
            // 1920x1080 over mid grey, as a PPM
            let mut px = vec![90u8; 1920 * 1080 * 3];
            for p in &pics {
                for y in 0..p.h {
                    for x in 0..p.w {
                        let (sx, sy) = (p.x + x, p.y + y);
                        if sx < 0 || sy < 0 || sx >= 1920 || sy >= 1080 {
                            continue;
                        }
                        let s = &p.rgba[((y * p.w + x) * 4) as usize..][..4];
                        let d = &mut px[((sy * 1920 + sx) * 3) as usize..][..3];
                        for i in 0..3 {
                            d[i] = ((u32::from(s[i]) * u32::from(s[3]) + u32::from(d[i]) * (255 - u32::from(s[3])))
                                / 255) as u8;
                        }
                    }
                }
            }
            let mut f = b"P6\n1920 1080\n255\n".to_vec();
            f.extend(px);
            std::fs::write(out, f).unwrap();
        }
        assert!(!pics.is_empty());
        let green = pics
            .iter()
            .any(|p| p.rgba.chunks(4).any(|px| px[0] == 0 && px[1] == 255 && px[3] > 200));
        assert!(green, "the style's green fill is in a picture");
        assert!(pics.iter().all(|p| p.y > 540), "bottom aligned");
    }
}
