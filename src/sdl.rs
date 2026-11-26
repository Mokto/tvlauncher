//! The few SDL2, SDL2_image and SDL2_ttf calls the launcher needs, declared by hand.
//!
//! No binding crate: LG's SDL fork changes `SDL_KeyboardEvent` (a webOS `inputSource` field shifts the
//! key symbol), and a crate built for stock SDL2 would read the wrong bytes. Mouse, window and surface
//! layouts are the same in both.
#![allow(non_snake_case, dead_code)]
use std::ffi::{c_char, c_int, c_void, CString};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[repr(C)]
pub struct Surface {
    flags: u32,
    format: *mut c_void,
    pub w: c_int,
    pub h: c_int,
    pub pitch: c_int,
    pub pixels: *mut c_void,
}

#[repr(C)]
pub struct RendererInfo {
    pub name: *const c_char,
    flags: u32,
    num_texture_formats: u32,
    texture_formats: [u32; 16],
    max_texture_width: c_int,
    max_texture_height: c_int,
}

#[repr(C)]
pub struct Opaque([u8; 0]);

/// SDL's 56-byte event union, read at fixed offsets.
#[repr(C, align(8))]
pub struct Event(pub [u8; 56]);

impl Event {
    pub fn new(kind: u32) -> Self {
        let mut e = Event([0; 56]);
        e.0[..4].copy_from_slice(&kind.to_ne_bytes());
        e
    }
    pub fn u32_at(&self, off: usize) -> u32 {
        u32::from_ne_bytes(self.0[off..off + 4].try_into().unwrap())
    }
    pub fn kind(&self) -> u32 {
        self.u32_at(0)
    }
    pub fn i32_at(&self, off: usize) -> i32 {
        self.u32_at(off) as i32
    }
    /// The SDL keycode of a key event. LG's fork writes scancode@20 and sym@24; stock SDL2 (the
    /// desktop simulator) writes scancode@16 and sym@20.
    pub fn key_sym(&self) -> i32 {
        self.i32_at(if cfg!(target_arch = "arm") { 24 } else { 20 })
    }
    /// LG's remote buttons above SDL's own range (BACK 482, colours 486-489) arrive as a scancode with no keycode.
    /// Stock SDL2 has no such thing, so the simulator never sees one.
    pub fn key_wcode(&self) -> u32 {
        if cfg!(target_arch = "arm") {
            self.u32_at(20)
        } else {
            0
        }
    }
    /// True for the auto-repeat of a held key: bit 0x100 of the state word on LG's fork, `repeat` byte on stock SDL2.
    pub fn key_repeat(&self) -> bool {
        if cfg!(target_arch = "arm") {
            self.u32_at(16) & 0x100 != 0
        } else {
            self.0[13] != 0
        }
    }
}

pub const INIT_VIDEO: u32 = 0x20;
pub const WINDOW_FULLSCREEN: u32 = 1;
pub const WINDOW_RESIZABLE: u32 = 0x20;
pub const RENDERER_ACCELERATED: u32 = 2;
pub const RENDERER_PRESENTVSYNC: u32 = 4;
pub const BLENDMODE_BLEND: c_int = 1;
pub const PIXELFORMAT_ARGB8888: u32 = 0x1636_2004;
pub const FLIP_VERTICAL: c_int = 2;
pub const IMG_INIT_JPG: c_int = 1;
pub const IMG_INIT_PNG: c_int = 2;
pub const LOG_CATEGORY_APPLICATION: c_int = 0;
pub const QUIT: u32 = 0x100;
pub const WINDOWEVENT: u32 = 0x200;
pub const WINDOWEVENT_FOCUS_GAINED: u8 = 12;
pub const KEYDOWN: u32 = 0x300;
pub const KEYUP: u32 = 0x301;
pub const WCODE_BACK: u32 = 482;
pub const WCODE_YELLOW: u32 = 488;
pub const K_M: i32 = 109;
pub const MOUSEMOTION: u32 = 0x400;
pub const MOUSEBUTTONDOWN: u32 = 0x401;
pub const MOUSEBUTTONUP: u32 = 0x402;
pub const BUTTON_LEFT: u8 = 1;
pub const K_RETURN: i32 = 13;
pub const K_ESCAPE: i32 = 27;
pub const K_KP_ENTER: i32 = 0x4000_0058;
pub const K_RIGHT: i32 = 0x4000_004f;
pub const K_LEFT: i32 = 0x4000_0050;
pub const K_DOWN: i32 = 0x4000_0051;
pub const K_PAGEUP: i32 = 0x4000_004B;
pub const K_PAGEDOWN: i32 = 0x4000_004E;
pub const K_UP: i32 = 0x4000_0052;

pub type LogFn = extern "C" fn(*mut c_void, c_int, c_int, *const c_char);

extern "C" {
    pub fn SDL_Init(flags: u32) -> c_int;
    pub fn SDL_GetError() -> *const c_char;
    pub fn SDL_Log(fmt: *const c_char, ...);
    pub fn SDL_LogSetOutputFunction(f: LogFn, userdata: *mut c_void);
    pub fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    pub fn SDL_GetTicks() -> u32;
    pub fn SDL_RegisterEvents(n: c_int) -> u32;
    pub fn SDL_PollEvent(e: *mut Event) -> c_int;
    pub fn SDL_WaitEventTimeout(e: *mut Event, ms: c_int) -> c_int;
    pub fn SDL_PushEvent(e: *mut Event) -> c_int;

    pub fn SDL_CreateWindow(title: *const c_char, x: c_int, y: c_int, w: c_int, h: c_int, flags: u32) -> *mut Opaque;
    pub fn SDL_CreateRenderer(win: *mut Opaque, index: c_int, flags: u32) -> *mut Opaque;
    pub fn SDL_GetRendererInfo(r: *mut Opaque, info: *mut RendererInfo) -> c_int;
    pub fn SDL_GetRendererOutputSize(r: *mut Opaque, w: *mut c_int, h: *mut c_int) -> c_int;
    pub fn SDL_RenderSetLogicalSize(r: *mut Opaque, w: c_int, h: c_int) -> c_int;
    pub fn SDL_SetRenderDrawBlendMode(r: *mut Opaque, mode: c_int) -> c_int;
    pub fn SDL_SetRenderDrawColor(r: *mut Opaque, r_: u8, g: u8, b: u8, a: u8) -> c_int;
    pub fn SDL_RenderClear(r: *mut Opaque) -> c_int;
    pub fn SDL_RenderPresent(r: *mut Opaque);
    pub fn SDL_RaiseWindow(win: *mut Opaque);
    pub fn SDL_ShowWindow(win: *mut Opaque);
    pub fn SDL_HideWindow(win: *mut Opaque);
    pub fn SDL_ShowCursor(toggle: c_int) -> c_int;
    pub fn SDL_RenderFillRectF(r: *mut Opaque, rect: *const FRect) -> c_int;
    pub fn SDL_RenderCopyF(r: *mut Opaque, t: *mut Opaque, src: *const Rect, dst: *const FRect) -> c_int;
    pub fn SDL_RenderCopyExF(
        r: *mut Opaque,
        t: *mut Opaque,
        src: *const Rect,
        dst: *const FRect,
        angle: f64,
        center: *const c_void,
        flip: c_int,
    ) -> c_int;
    pub fn SDL_RenderReadPixels(
        r: *mut Opaque,
        rect: *const Rect,
        format: u32,
        pixels: *mut c_void,
        pitch: c_int,
    ) -> c_int;

    pub fn SDL_CreateTextureFromSurface(r: *mut Opaque, s: *mut Surface) -> *mut Opaque;
    pub fn SDL_DestroyTexture(t: *mut Opaque);
    pub fn SDL_SetTextureColorMod(t: *mut Opaque, r: u8, g: u8, b: u8) -> c_int;
    pub fn SDL_SetTextureAlphaMod(t: *mut Opaque, a: u8) -> c_int;
    pub fn SDL_SetTextureBlendMode(t: *mut Opaque, mode: c_int) -> c_int;

    pub fn SDL_CreateRGBSurfaceWithFormat(flags: u32, w: c_int, h: c_int, depth: c_int, format: u32) -> *mut Surface;
    pub fn SDL_FreeSurface(s: *mut Surface);
    pub fn SDL_UpperBlitScaled(src: *mut Surface, srcrect: *const Rect, dst: *mut Surface, dstrect: *mut Rect)
        -> c_int;
    pub fn SDL_RWFromConstMem(mem: *const c_void, size: c_int) -> *mut Opaque;
    pub fn SDL_RWFromFile(file: *const c_char, mode: *const c_char) -> *mut Opaque;
    pub fn SDL_SaveBMP_RW(s: *mut Surface, dst: *mut Opaque, freedst: c_int) -> c_int;

    pub fn IMG_Init(flags: c_int) -> c_int;
    pub fn IMG_Load_RW(src: *mut Opaque, freesrc: c_int) -> *mut Surface;
    pub fn IMG_Load(file: *const c_char) -> *mut Surface;

    pub fn TTF_Init() -> c_int;
    pub fn TTF_OpenFont(file: *const c_char, size: c_int) -> *mut Opaque;
    pub fn TTF_RenderUTF8_Blended(f: *mut Opaque, text: *const c_char, fg: Color) -> *mut Surface;
    pub fn TTF_RenderUTF8_Blended_Wrapped(f: *mut Opaque, text: *const c_char, fg: Color, wrap: u32) -> *mut Surface;
}

pub fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

pub fn error() -> String {
    unsafe { std::ffi::CStr::from_ptr(SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

/// Goes through `SDL_Log`, whose output function in `main.rs` keeps only the application's own lines.
pub fn log(msg: &str) {
    unsafe { SDL_Log(c"%s".as_ptr(), cstr(msg).as_ptr()) }
}

/// An SDL surface, freed on drop.
pub struct Surf(pub *mut Surface);

// A surface is plain pixel memory; the loader thread hands its decode result to the main thread.
unsafe impl Send for Surf {}

impl Surf {
    pub fn load(path: &str) -> Option<Surf> {
        let s = unsafe { IMG_Load(cstr(path).as_ptr()) };
        (!s.is_null()).then_some(Surf(s))
    }
    pub fn from_bytes(bytes: &[u8]) -> Option<Surf> {
        let rw = unsafe { SDL_RWFromConstMem(bytes.as_ptr().cast(), bytes.len().try_into().ok()?) };
        if rw.is_null() {
            return None;
        }
        let s = unsafe { IMG_Load_RW(rw, 1) };
        (!s.is_null()).then_some(Surf(s))
    }

    pub fn size(&self) -> (i32, i32) {
        unsafe { ((*self.0).w, (*self.0).h) }
    }
}

impl Drop for Surf {
    fn drop(&mut self) {
        unsafe { SDL_FreeSurface(self.0) }
    }
}

/// A texture with the size of the surface it came from, destroyed on drop.
pub struct Tex {
    p: *mut Opaque,
    pub w: i32,
    pub h: i32,
}

impl Tex {
    pub fn new(ren: *mut Opaque, s: &Surf) -> Option<Tex> {
        let p = unsafe { SDL_CreateTextureFromSurface(ren, s.0) };
        let (w, h) = s.size();
        (!p.is_null()).then_some(Tex { p, w, h })
    }
    pub fn ptr(&self) -> *mut Opaque {
        self.p
    }

    /// A texture of `w` x `h` pixels from R, G, B, A bytes (a subtitle bitmap), blended over what is behind it.
    pub fn rgba(ren: *mut Opaque, w: i32, h: i32, px: &[u8]) -> Option<Tex> {
        if w <= 0 || h <= 0 || px.len() < (w * h * 4) as usize {
            return None;
        }
        let s = Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888) });
        if s.0.is_null() {
            return None;
        }
        let stride = unsafe { (*s.0).pitch as usize / 4 };
        let dst = unsafe { (*s.0).pixels as *mut u32 };
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = &px[(y * w as usize + x) * 4..][..4];
                let v = (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32;
                unsafe { *dst.add(y * stride + x) = v };
            }
        }
        let t = Tex::new(ren, &s)?;
        unsafe { SDL_SetTextureBlendMode(t.ptr(), BLENDMODE_BLEND) };
        Some(t)
    }
}

impl Drop for Tex {
    fn drop(&mut self) {
        unsafe { SDL_DestroyTexture(self.p) }
    }
}
