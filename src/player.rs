//! The live TV player: the stream is read by the TV's `curl`, cut into access units by `ts.rs` and fed to LG's
//! StarfishMediaAPIs (libplayerAPIs) in buffer-feed mode. On webOS 5 and later the picture goes to an SDL
//! exported window under the UI, named in the Load payload. The call shapes are those of Native Jelly's
//! `src/starfish.c` (GPL-3.0), which plays this way on webOS 6.
use crate::net::curl_stream;
use crate::sdl::*;
use crate::ts::{Au, Codec, Demux, Out};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::Read;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// On the TV, a URL in this file makes the next start of the launcher play it instead of showing Home.
pub const TRIGGER: &str = "/tmp/tvlauncher-play";
const AHEAD_NS: i64 = 1_600_000_000;
/// The pipeline's app id. The media resource manager never gives a decoder to com.webos.app.home.
const APP: &str = "tvlauncher";

enum Msg {
    Streams(Option<Codec>, Option<Codec>),
    Au(Au),
    End(String),
}

/// Reads `url` on a thread. The bounded channel holds the stream back while the pipeline is full.
fn read(url: &str) -> Receiver<Msg> {
    let (tx, rx) = sync_channel(256);
    let url = url.to_string();
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            let mut child = curl_stream(&url).map_err(|e| format!("curl: {e}"))?;
            let mut stdout = child.stdout.take().ok_or("curl: no stdout")?;
            let (mut demux, mut buf, mut out) = (Demux::default(), vec![0; 64 * 1024], Vec::new());
            loop {
                let n = stdout.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    let _ = child.wait();
                    return Err(format!("the stream ended {}", err.trim()));
                }
                demux.push(&buf[..n], &mut out);
                for o in out.drain(..) {
                    let m = match o {
                        Out::Streams { video, audio } => Msg::Streams(video, audio),
                        Out::Au(a) => Msg::Au(a),
                    };
                    if tx.send(m).is_err() {
                        let _ = child.kill();
                        return Ok(());
                    }
                }
            }
        };
        if let Err(e) = run() {
            let _ = tx.send(Msg::End(e));
        }
    });
    rx
}

/// `--play <url>` on a desktop: reads ten seconds of the stream and says what the TV would be fed.
pub fn probe(url: &str) -> ExitCode {
    let rx = read(url);
    let end = Instant::now() + Duration::from_secs(10);
    let (mut n, mut keys, mut last) = ([0u32; 2], 0, [0i64; 2]);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Msg::Streams(v, a)) => println!("streams: video {v:?}, audio {a:?}"),
            Ok(Msg::Au(au)) => {
                n[au.video as usize] += 1;
                keys += u32::from(au.key && au.video);
                last[au.video as usize] = au.pts_ns / 1_000_000;
            }
            Ok(Msg::End(e)) => {
                println!("{e}");
                break;
            }
            Err(_) => break,
        }
    }
    println!(
        "video: {} pictures, {keys} keyframes, last at {} ms; audio: {} frames, last at {} ms",
        n[1], last[1], n[0], last[0]
    );
    if n[1] > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn run(ren: *mut Opaque, url: &str) -> ExitCode {
    log("play: start");
    match play(ren, url) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("play: {e}"));
            ExitCode::FAILURE
        }
    }
}

type Obj = *mut c_void;
type EventFn = extern "C" fn(c_int, i64, *const c_char);

/// One StarfishMediaAPIs object. Its size is not published, so it is built in place in a generous buffer
/// that is never freed (the C++ side may still call back into it after Unload).
struct Smp {
    obj: Obj,
    load: unsafe extern "C" fn(Obj, *const c_char, EventFn) -> c_int,
    // Returns a std::string through the hidden pointer in the first argument.
    feed: unsafe extern "C" fn(*mut c_void, Obj, *const c_char),
    play: unsafe extern "C" fn(Obj) -> c_int,
    unload: unsafe extern "C" fn(Obj) -> c_int,
    loaded: unsafe extern "C" fn(Obj) -> c_int,
    delete: unsafe extern "C" fn(*mut c_void),
}

#[repr(C, align(16))]
struct Storage([u8; 65536]);

enum Fed {
    Ok,
    Full,
    Refused(String),
}

unsafe fn sym<T: Copy>(lib: *mut c_void, name: &CStr) -> Result<T, String> {
    let p = libc::dlsym(lib, name.as_ptr());
    if p.is_null() {
        return Err(format!("{} is missing", name.to_string_lossy()));
    }
    Ok(std::mem::transmute_copy(&p))
}

static EVENTS: [AtomicU32; 64] = [const { AtomicU32::new(0) }; 64];

/// The pipeline's events, from its own threads: the first few of each type go to the log.
extern "C" fn on_event(kind: c_int, num: i64, s: *const c_char) {
    let seen = EVENTS
        .get(kind as usize & 63)
        .map_or(0, |c| c.fetch_add(1, Ordering::Relaxed));
    if seen < 5 {
        let s = if s.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(s) }
                .to_string_lossy()
                .chars()
                .take(300)
                .collect()
        };
        log(&format!("smp event {kind} {num} {s}"));
    }
}

impl Smp {
    fn new() -> Result<Smp, String> {
        unsafe {
            let lib = libc::dlopen(c"libplayerAPIs.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            if lib.is_null() {
                return Err(format!(
                    "libplayerAPIs: {}",
                    CStr::from_ptr(libc::dlerror()).to_string_lossy()
                ));
            }
            let ctor: unsafe extern "C" fn(Obj, *const c_char) = sym(lib, c"_ZN17StarfishMediaAPIsC1EPKc")?;
            let foreground: unsafe extern "C" fn(Obj) = sym(lib, c"_ZN17StarfishMediaAPIs16notifyForegroundEv")?;
            let smp = Smp {
                obj: Box::into_raw(Box::new(Storage([0; 65536]))).cast(),
                load: sym(lib, c"_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E")?,
                feed: sym(lib, c"_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc")?,
                play: sym(lib, c"_ZN17StarfishMediaAPIs4PlayEv")?,
                unload: sym(lib, c"_ZN17StarfishMediaAPIs6UnloadEv")?,
                loaded: sym(lib, c"_ZN17StarfishMediaAPIs15isLoadCompletedEv")?,
                delete: sym(libc::RTLD_DEFAULT, c"_ZdlPv")?,
            };
            // A null id registers on the media server's pre-authorised namespace.
            ctor(smp.obj, std::ptr::null());
            foreground(smp.obj);
            Ok(smp)
        }
    }

    fn feed(&self, au: &Au) -> Fed {
        let j = cstr(&format!(
            r#"{{"bufferAddr":"{:p}","bufferSize":{},"pts":{},"esData":{}}}"#,
            au.data.as_ptr(),
            au.data.len(),
            au.pts_ns,
            if au.video { 1 } else { 2 }
        ));
        // A libstdc++ std::string: data pointer, length, then a 16-byte inline buffer that short replies use.
        let mut ret = [0usize; 8];
        unsafe {
            (self.feed)(ret.as_mut_ptr().cast(), self.obj, j.as_ptr());
            let (s, len) = (ret[0] as *mut u8, ret[1]);
            let reply = if s.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(s, len)).into_owned()
            };
            if !s.is_null() && s != ret.as_mut_ptr().add(2).cast() {
                (self.delete)(s.cast());
            }
            if reply.contains("BufferFull") {
                Fed::Full
            } else if reply.contains("Ok") {
                Fed::Ok
            } else {
                Fed::Refused(reply)
            }
        }
    }
}

/// The Load payload (the shape of ss4s and Native Jelly). `windowId` binds the picture to our exported window.
fn payload(app: &str, window: &str, video: Codec, audio: Option<Codec>) -> String {
    let (w, h) = if video == Codec::H265 {
        (3840, 2160)
    } else {
        (1920, 1080)
    };
    let mut codec = format!(r#""video":"{}""#, video.payload_name());
    if let Some(a) = audio {
        codec.push_str(&format!(r#","audio":"{}""#, a.payload_name()));
    }
    format!(
        r#"{{"args":[{{"mediaTransportType":"BUFFERSTREAM","option":{{"appId":"{app}","windowId":"{window}","externalStreamingInfo":{{"contents":{{"codec":{{{codec}}},"esInfo":{{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true}},"format":"RAW","provider":"tvlauncher"}},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{{"minimum":1,"maximum":1048576}},"srcBufferLevelVideo":{{"minimum":1,"maximum":8388608}}}}}},"needAudio":{},"queryPosition":false,"lowDelayMode":false,"transmission":{{"contentsType":"LIVE"}},"adaptiveStreaming":{{"audioOnly":false,"maxWidth":{w},"maxHeight":{h},"maxFrameRate":60}}}}}}]}}"#,
        audio.is_some()
    )
}

/// True when Back (or Escape) was pressed or the app was told to quit. Also keeps the window serviced.
fn back_pressed() -> bool {
    let mut e = Event::new(0);
    let mut back = false;
    while unsafe { SDL_PollEvent(&mut e) } != 0 {
        back |= e.kind() == QUIT || e.kind() == KEYDOWN && (e.key_wcode() == WCODE_BACK || e.key_sym() == K_ESCAPE);
    }
    back
}

/// A fully transparent frame, so the video plane under the UI shows.
fn clear(ren: *mut Opaque) {
    unsafe {
        SDL_SetRenderDrawColor(ren, 0, 0, 0, 0);
        SDL_RenderClear(ren);
        SDL_RenderPresent(ren);
    }
}

fn play(ren: *mut Opaque, url: &str) -> Result<(), String> {
    let rx = read(url);
    let (video, audio) = loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Msg::Streams(Some(v), a)) => break (v, a),
            Ok(Msg::Streams(None, _)) => return Err("no H.264 or H.265 video in the stream".into()),
            Ok(Msg::End(e)) => return Err(e),
            Ok(Msg::Au(_)) => {}
            Err(_) => return Err("no programme table within 15 s".into()),
        }
    };
    log(&format!("play: video {video:?}, audio {audio:?}"));

    let (create, place) = unsafe {
        let create: unsafe extern "C" fn(c_int) -> *const c_char =
            sym(libc::RTLD_DEFAULT, c"SDL_webOSCreateExportedWindow")?;
        let place: unsafe extern "C" fn(*const c_char, *mut Rect, *mut Rect) -> c_int =
            sym(libc::RTLD_DEFAULT, c"SDL_webOSSetExportedWindow")?;
        (create, place)
    };
    let id = unsafe { create(0) };
    if id.is_null() {
        return Err("SDL_webOSCreateExportedWindow gave no window".into());
    }
    let window = unsafe { CStr::from_ptr(id) }.to_owned();
    log(&format!("play: exported window {}", window.to_string_lossy()));

    clear(ren);
    let smp = Smp::new()?;
    let p = cstr(&payload(APP, &window.to_string_lossy(), video, audio));
    if unsafe { (smp.load)(smp.obj, p.as_ptr(), on_event) } & 0xff == 0 {
        return Err("Load refused".into());
    }
    // The media server only connects the decoder to the screen for an app it believes is in front, and
    // the app manager never reports Home as such.
    let _ = std::process::Command::new("/usr/bin/luna-send")
        .args(["-n", "1", "-w", "3000", "luna://com.webos.media/notifyAppLifeStatus"])
        .arg(format!(
            r#"{{"appId":"{APP}","status":"foreground","windowType":"card"}}"#
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
    let t = Instant::now();
    while unsafe { (smp.loaded)(smp.obj) } & 0xff == 0 {
        if t.elapsed() > Duration::from_secs(10) || back_pressed() {
            unsafe { (smp.unload)(smp.obj) };
            return Err("Load did not complete within 10 s".into());
        }
        clear(ren);
    }
    log(&format!("play: loaded in {} ms", t.elapsed().as_millis()));
    // ponytail: the source rect assumes 1080p; a 720p stream may need its size from the SPS.
    let rv = unsafe {
        place(
            window.as_ptr(),
            &mut Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            &mut Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
        )
    };
    log(&format!("play: window placed ({rv})"));

    let (mut pending, mut fed, mut refused, mut ended) = (None::<Au>, [0u64; 2], 0, false);
    let (mut last, mut tick, mut started) = ([0i64; 2], Instant::now(), None::<Instant>);
    let result = 'run: loop {
        if tick.elapsed() > Duration::from_secs(5) {
            tick = Instant::now();
            log(&format!(
                "play: fed {}/{} up to {}/{} ms, waiting: {}",
                fed[1],
                fed[0],
                last[1] / 1_000_000,
                last[0] / 1_000_000,
                pending
                    .as_ref()
                    .map_or("-", |a| if a.video { "video" } else { "audio" })
            ));
        }
        if back_pressed() {
            break Ok(());
        }
        while !ended {
            let au = match pending.take() {
                Some(a) => a,
                None => match rx.try_recv() {
                    Ok(Msg::Au(a)) => a,
                    Ok(Msg::Streams(..)) => continue,
                    Ok(Msg::End(e)) => {
                        log(&format!("play: {e}"));
                        ended = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'run Err("the reader stopped".into()),
                },
            };
            // Native Jelly's throttle: never more than AHEAD_NS past the playhead (zero until Play), or the
            // pipeline stops pulling and the clock never starts.
            let playhead = started.map_or(0, |t| t.elapsed().as_nanos() as i64);
            if au.pts_ns > playhead + AHEAD_NS {
                pending = Some(au);
                break;
            }
            match smp.feed(&au) {
                Fed::Ok => {
                    last[au.video as usize] = au.pts_ns;
                    let n = &mut fed[au.video as usize];
                    *n += 1;
                    if *n == 1 || *n % 500 == 0 {
                        let lane = if au.video { "video" } else { "audio" };
                        log(&format!("play: fed {n} {lane}, pts {} ms", au.pts_ns / 1_000_000));
                    }
                }
                Fed::Full => {
                    pending = Some(au);
                    break;
                }
                Fed::Refused(r) => {
                    refused += 1;
                    if refused <= 5 {
                        log(&format!("play: Feed answered {r:?}"));
                    }
                }
            }
        }
        // Like Native Jelly: Play once the decoder has some of each lane, not on an empty pipeline.
        if started.is_none() && last[1] >= 700_000_000 && (audio.is_none() || last[0] >= 300_000_000) {
            started = Some(Instant::now());
            let ok = unsafe { (smp.play)(smp.obj) } & 0xff;
            log(&format!("play: Play {ok}"));
        }
        clear(ren);
    };
    unsafe { (smp.unload)(smp.obj) };
    log(&format!(
        "play: stopped after {} pictures and {} audio frames",
        fed[1], fed[0]
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_json_with_the_window_and_codecs() {
        let p = payload("com.webos.app.home", "_Window_Id_3", Codec::H264, Some(Codec::Aac));
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        let o = &v["args"][0]["option"];
        assert_eq!(o["windowId"], "_Window_Id_3");
        assert_eq!(o["externalStreamingInfo"]["contents"]["codec"]["audio"], "AAC");
        assert_eq!(o["needAudio"], true);
        let p = payload("a", "w", Codec::H265, None);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["args"][0]["option"]["adaptiveStreaming"]["maxWidth"], 3840);
        assert_eq!(v["args"][0]["option"]["needAudio"], false);
    }
}
