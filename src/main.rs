//! tvlauncher: an Android TV-style Home for rooted LG webOS TVs. GPL-3.0-or-later, see LICENSE.
//!
//! Runs as LG's Home program (takeover.sh), so it is root and outside the app jail: it reads the launch
//! points and inputs over luna-send and the icons straight from the app folders.
//! `LAUNCHER_ROOT=<dir>` runs it on a desktop against files from tools/pull-fixtures.sh.
mod app;
#[cfg(target_arch = "arm")]
mod compat;
mod data;
mod gfx;
mod hls;
mod iptv;
mod jellyfin;
mod keyboard;
mod library;
mod lifecycle;
mod login;
mod net;
mod player;
mod plex;
mod sdl;
mod settings;
mod ts;
mod tv;
mod youtube;

use app::{App, Background, Jelly};
use data::{Luna, DIR};
use gfx::{Gfx, H, W};
use sdl::*;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;

extern "C" fn log_line(_: *mut c_void, category: c_int, _: c_int, msg: *const c_char) {
    if category == LOG_CATEGORY_APPLICATION {
        eprintln!(
            "{} {}",
            unsafe { SDL_GetTicks() },
            unsafe { CStr::from_ptr(msg) }.to_string_lossy()
        );
    }
}

fn font(root: &str, name: &str, size: c_int) -> *mut Opaque {
    let path = format!("{root}/usr/share/fonts/{name}");
    let f = unsafe { TTF_OpenFont(cstr(&path).as_ptr(), size) };
    if f.is_null() {
        log(&format!("font {path}: {}", error()));
    }
    f
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let shot = (args.len() > 2 && args[1] == "--shot").then(|| args[2].clone());
    let root = std::env::var("LAUNCHER_ROOT").unwrap_or_default();
    let on_tv = root.is_empty();
    if !on_tv && args.len() > 2 && args[1] == "--play" {
        return player::probe(&args[2]);
    }
    if on_tv {
        if let Ok(f) = std::fs::File::create("/tmp/tvlauncher.log") {
            unsafe {
                libc::dup2(f.as_raw_fd(), 2);
                libc::dup2(f.as_raw_fd(), 1)
            };
        }
        if std::env::var_os("APPID").is_none() {
            std::env::set_var("APPID", "com.webos.app.home");
        }
        std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
        lifecycle::install_crash_counter();
    }
    unsafe { SDL_LogSetOutputFunction(log_line, std::ptr::null_mut()) };
    log(&format!("tvlauncher start: {}", args.get(1).map_or("", String::as_str)));
    let started = start(&root, shot.is_some());
    let Some((win, ren)) = started else {
        if on_tv {
            lifecycle::note_failed_start();
        }
        return ExitCode::FAILURE;
    };

    let g = Gfx::new(&root, ren, |name, size| font(&root, name, size));
    // Threads wake the loop with this event when they have something for it.
    let wake = unsafe { SDL_RegisterEvents(1) };

    // Decoding a 1080p background takes longer than a frame, so it happens on a thread. Only the latest request is decoded.
    let (bg_req, req_rx) = channel::<(u64, Option<String>)>();
    let (res_tx, bg_res) = channel();
    std::thread::spawn(move || {
        while let Ok(mut req) = req_rx.recv() {
            while let Ok(newer) = req_rx.try_recv() {
                req = newer;
            }
            let surf = req.1.and_then(|p| {
                let s = Surf::load(&p)?;
                // YouTube's pictures are small; everything else is a 1080p backdrop.
                if p.contains("/cache/yt-") {
                    return Some(gfx::blur(&s).unwrap_or(s));
                }
                Some(s)
            });
            if res_tx.send((req.0, surf)).is_err() {
                break;
            }
            unsafe { SDL_PushEvent(&mut Event::new(wake)) };
        }
    });

    let jf = jellyfin::Config::load(&root).map(|cfg| {
        let (refresh, res) = jellyfin::spawn(root.clone(), cfg.clone(), wake);
        Jelly { cfg, refresh, res }
    });
    if on_tv && std::path::Path::new(&format!("{DIR}/register")).exists() {
        lifecycle::register(wake);
    }
    let mut app = App::new(
        g,
        Luna { root },
        win,
        Background {
            req: bg_req,
            res: bg_res,
        },
        jf,
        wake,
    );

    if on_tv {
        app.play_file();
    }

    if let Some(file) = shot {
        // After each key, draw until nothing moves and nothing is loading, so a dialog is ready for the next key.
        let settle = |app: &mut App| {
            for _ in 0..3000 {
                let mut e = Event::new(0);
                while unsafe { SDL_PollEvent(&mut e) } != 0 {}
                app.frame(1.0 / 60.0);
                if !app.animating && !app.busy() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        settle(&mut app);
        for k in args.get(3).map_or("", String::as_str).chars() {
            app.handle(&key_event(k));
            // 'H' holds OK (settle waits for the hold to open the options); 'O' taps it
            if k == 'O' {
                let mut up = key_event(k);
                up.0[0..4].copy_from_slice(&KEYUP.to_ne_bytes());
                app.handle(&up);
            }
            settle(&mut app);
        }
        return save_bmp(ren, &file);
    }

    let mut last = unsafe { SDL_GetTicks() };
    loop {
        let mut e = Event::new(0);
        let mut any = unsafe {
            if app.animating {
                SDL_PollEvent(&mut e)
            } else {
                SDL_WaitEventTimeout(&mut e, 1000)
            }
        } != 0;
        while any {
            if !app.handle(&e) {
                return ExitCode::SUCCESS;
            }
            any = unsafe { SDL_PollEvent(&mut e) } != 0;
        }
        if lifecycle::RELAUNCH.swap(false, Ordering::SeqCst) {
            app.relaunch();
        }
        let now = unsafe { SDL_GetTicks() };
        app.frame((now.wrapping_sub(last) as f32 / 1000.0).min(0.1));
        last = now;
        unsafe { SDL_RenderPresent(ren) };
    }
}

/// Initialises SDL and opens the window and renderer.
fn start(root: &str, shot: bool) -> Option<(*mut Opaque, *mut Opaque)> {
    let on_tv = root.is_empty();
    unsafe {
        SDL_SetHint(c"SDL_RENDER_SCALE_QUALITY".as_ptr(), c"1".as_ptr());
        if SDL_Init(INIT_VIDEO) != 0 || TTF_Init() < 0 {
            log(&format!("init: {}", error()));
            return None;
        }
        IMG_Init(IMG_INIT_PNG | IMG_INIT_JPG);
    }
    let windowed = !on_tv && !shot;
    let flags = if on_tv {
        WINDOW_FULLSCREEN
    } else if windowed {
        WINDOW_RESIZABLE
    } else {
        0
    };
    let (ww, wh) = if windowed {
        (1280, 720)
    } else {
        (W as c_int, H as c_int)
    };
    let (win, ren) = unsafe {
        SDL_ShowCursor(0);
        let win = SDL_CreateWindow(c"tvlauncher".as_ptr(), 0, 0, ww, wh, flags);
        let vsync = if shot { 0 } else { RENDERER_PRESENTVSYNC };
        let ren = if win.is_null() {
            win
        } else {
            SDL_CreateRenderer(win, -1, RENDERER_ACCELERATED | vsync)
        };
        (win, ren)
    };
    if ren.is_null() {
        log(&format!("window: {}", error()));
        return None;
    }
    unsafe {
        let mut info: RendererInfo = std::mem::zeroed();
        SDL_GetRendererInfo(ren, &mut info);
        log(&format!("renderer {}", CStr::from_ptr(info.name).to_string_lossy()));
        SDL_RenderSetLogicalSize(ren, W as c_int, H as c_int);
        SDL_SetRenderDrawBlendMode(ren, BLENDMODE_BLEND);
    }
    Some((win, ren))
}

/// A key press for `--shot`: R/L/U/D move, O is OK, M the options menu, B back, digits and a dot type.
fn key_event(k: char) -> Event {
    let sym = match k {
        'R' => K_RIGHT,
        'L' => K_LEFT,
        'U' => K_UP,
        'D' => K_DOWN,
        'O' | 'H' => K_RETURN,
        'M' => K_M,
        '0'..='9' | '.' | ':' => k as i32,
        _ => K_ESCAPE,
    };
    let mut e = Event::new(KEYDOWN);
    // Where stock SDL2 (the simulator) and LG's fork each keep the key symbol.
    e.0[20..24].copy_from_slice(&sym.to_ne_bytes());
    e.0[24..28].copy_from_slice(&sym.to_ne_bytes());
    e
}

fn save_bmp(ren: *mut Opaque, file: &str) -> ExitCode {
    unsafe {
        let (mut w, mut h) = (0, 0);
        SDL_GetRendererOutputSize(ren, &mut w, &mut h);
        let s = Surf(SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888));
        SDL_RenderReadPixels(ren, std::ptr::null(), PIXELFORMAT_ARGB8888, (*s.0).pixels, (*s.0).pitch);
        let out = SDL_RWFromFile(cstr(file).as_ptr(), c"wb".as_ptr());
        if SDL_SaveBMP_RW(s.0, out, 1) == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}
