//! The live TV player: the stream is read by the TV's `curl`, cut into access units by `ts.rs` and fed to LG's
//! StarfishMediaAPIs (libplayerAPIs) in buffer-feed mode. On webOS 5 and later the picture goes to an SDL
//! exported window under the UI, named in the Load payload. The call shapes are those of Native Jelly's
//! `src/starfish.c` (GPL-3.0), which plays this way on webOS 6.
use crate::gfx::*;
use crate::hls;
use crate::iptv::{now_next, Channel, Guide};
use crate::net::curl_stream;
use crate::sdl::*;
use crate::ts::{Au, Codec, Demux, Out};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::Read;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// On the TV, a channel number or a stream address in this file makes the next start of the launcher play it
/// (for testing without the remote).
pub const TRIGGER: &str = "/tmp/tvlauncher-play";
const AHEAD_NS: i64 = 1_600_000_000;
mod vod;
pub use vod::{child as film_child, child_in as film_child_in, film_sim_keys, shot as film_shot};

/// For testing without the remote: `film:<item id>` or a stream address in this file makes the running launcher
/// start the player process for it.
pub const SPAWN: &str = "/tmp/tvlauncher-spawn";
/// What a finished HLS playlist ends with: a film's end, an error for a channel.
const ENDED: &str = "The stream ended.";
/// The pipeline's app id. The media resource manager never gives a decoder to com.webos.app.home.
const APP: &str = "tvlauncher";

enum Msg {
    Streams(Option<Codec>, Option<Codec>),
    Au(Au),
    End(String),
}

/// The thread reading a stream; dropping it stops the stream's `curl`.
struct Reader {
    rx: Receiver<Msg>,
    curl: Arc<Curl>,
}

#[derive(Default)]
struct Curl {
    pid: AtomicU32,
    stop: AtomicBool,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.curl.stop.store(true, Ordering::SeqCst);
        let pid = self.curl.pid.load(Ordering::SeqCst);
        if pid != 0 {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

/// Reads `url` on a thread: one MPEG-TS stream, or the segments of an HLS one in turn, through one demuxer.
/// The bounded channel holds the stream back while the pipeline is full.
fn read(url: &str, skip_s: f64) -> Reader {
    let (tx, rx) = sync_channel(256);
    let curl = Arc::new(Curl::default());
    let (url, shared) = (url.to_string(), curl.clone());
    std::thread::spawn(move || {
        let mut demux = Demux::default();
        // Reads one address to its end: Ok(false) once nobody listens any more.
        let mut pump = |url: &str| -> Result<bool, String> {
            let mut child = curl_stream(url).map_err(|e| format!("curl: {e}"))?;
            shared.pid.store(child.id(), Ordering::SeqCst);
            if shared.stop.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(false);
            }
            let mut stdout = child.stdout.take().ok_or("curl: no stdout")?;
            let (mut buf, mut out) = (vec![0; 64 * 1024], Vec::new());
            loop {
                let n = stdout.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    let _ = child.wait();
                    let err = err.trim().trim_start_matches("curl: ");
                    let stopped = shared.stop.load(Ordering::SeqCst);
                    return if err.is_empty() || stopped {
                        Ok(!stopped)
                    } else {
                        Err(err.to_string())
                    };
                }
                demux.push(&buf[..n], &mut out);
                for o in out.drain(..) {
                    let m = match o {
                        Out::Streams { video, audio } => Msg::Streams(video, audio),
                        Out::Au(a) => Msg::Au(a),
                    };
                    if tx.send(m).is_err() {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(false);
                    }
                }
            }
        };
        let r = if hls::is_hls(&url) {
            read_hls(&url, skip_s, &shared.stop, &mut pump)
        } else {
            pump(&url).and_then(|more| if more { Err(ENDED.into()) } else { Ok(()) })
        };
        if let Err(e) = r {
            let _ = tx.send(Msg::End(e));
        }
    });
    Reader { rx, curl }
}

/// An HLS stream: the variant with the most bandwidth, from three segments before the live edge (or from the
/// start for a complete playlist), then each new segment as the playlist lists it.
// ponytail: no switching between variants on a slow network, and a gap or a discontinuity (an inserted
// advert) jumps the timestamps, which the feed throttle then waits out.
fn read_hls(
    url: &str,
    skip_s: f64,
    stop: &AtomicBool,
    pump: &mut impl FnMut(&str) -> Result<bool, String>,
) -> Result<(), String> {
    let mut url = url.to_string();
    let (mut next, mut masters, mut failed) = (None::<u64>, 0, 0);
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let t = Instant::now();
        let reply = crate::net::curl(&url, crate::net::BROWSER_UA, &[], None, 10)?;
        if reply.status != 200 {
            return Err(format!("The playlist answered HTTP {}.", reply.status));
        }
        url = reply.url;
        match hls::parse(&url, &String::from_utf8_lossy(&reply.body))? {
            hls::Playlist::Master(v) => {
                masters += 1;
                // The first of equal bandwidths: Jellyfin 12 gives every variant the same figure, best first.
                url = v
                    .into_iter()
                    .reduce(|a, b| if b.0 > a.0 { b } else { a })
                    .ok_or("The playlist has no stream.")?
                    .1;
                if masters > 3 {
                    return Err("The playlist leads nowhere.".into());
                }
            }
            hls::Playlist::Media {
                seq,
                target_s,
                segs,
                durs,
                end,
            } => {
                let last = seq + segs.len() as u64;
                // A film's playlist is complete from the start: play it from the segment that holds `skip_s`
                // seconds in (the server's playlists take no start time; Jellyfin 12 refuses one).
                let from = next
                    .unwrap_or(if end {
                        seq + hls::index_at(&durs, skip_s) as u64
                    } else {
                        last.saturating_sub(3)
                    })
                    .max(seq);
                for (n, seg) in (seq..).zip(&segs).skip((from - seq) as usize) {
                    match pump(seg) {
                        Ok(true) => failed = 0,
                        Ok(false) => return Ok(()),
                        Err(e) if failed < 3 => {
                            failed += 1;
                            log(&format!("play: segment {n} skipped: {e}"));
                        }
                        Err(e) => return Err(e),
                    }
                    next = Some(n + 1);
                }
                if end && next.unwrap_or(0) >= last {
                    return Err(ENDED.into());
                }
                let wait = Duration::from_millis(target_s.clamp(1, 10) * 500);
                if let Some(rest) = wait.checked_sub(t.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
        }
    }
}

/// `--play <url>` on a desktop: reads ten seconds of the stream and says what the TV would be fed.
pub fn probe(url: &str) -> ExitCode {
    let rx = read(url, 0.0);
    let rx = &rx.rx;
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

type Obj = *mut c_void;
type EventFn = extern "C" fn(c_int, i64, *const c_char);

/// One StarfishMediaAPIs object. Its size is not published, so it is built in place in a generous buffer
/// that is never freed (the C++ side may still call back into it after Unload).
#[derive(Clone, Copy)]
struct Smp {
    obj: Obj,
    ctor: unsafe extern "C" fn(Obj, *const c_char),
    dtor: unsafe extern "C" fn(Obj),
    foreground: unsafe extern "C" fn(Obj),
    load: unsafe extern "C" fn(Obj, *const c_char, EventFn) -> c_int,
    // Returns a std::string through the hidden pointer in the first argument.
    feed: unsafe extern "C" fn(*mut c_void, Obj, *const c_char),
    play: unsafe extern "C" fn(Obj) -> c_int,
    pause: unsafe extern "C" fn(Obj) -> c_int,
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
/// The pipeline's last error (event 18), for the session to show.
static ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Set by UNLOADCOMPLETED (event 23): the object may be destroyed.
static UNLOADED: AtomicBool = AtomicBool::new(false);
/// The picture's size, from the video info (event 4), as width << 16 | height.
static SIZE: AtomicU32 = AtomicU32::new(0);

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
                .take(600)
                .collect()
        };
        log(&format!("smp event {kind} {num} {s}"));
    }
    if kind == 23 {
        UNLOADED.store(true, Ordering::SeqCst);
    }
    if kind == 4 && !s.is_null() {
        let info = unsafe { CStr::from_ptr(s) }.to_string_lossy();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&info) {
            let (w, h) = (v["video"]["width"].as_u64(), v["video"]["height"].as_u64());
            if let (Some(w @ 1..=8192), Some(h @ 1..=8192)) = (w, h) {
                SIZE.store((w as u32) << 16 | h as u32, Ordering::SeqCst);
            }
        }
    }
    if kind == 18 {
        let s = if s.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
        };
        *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("The TV's player failed: {s} ({num})."));
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
            Ok(Smp {
                obj: std::ptr::null_mut(),
                ctor: sym(lib, c"_ZN17StarfishMediaAPIsC1EPKc")?,
                dtor: sym(lib, c"_ZN17StarfishMediaAPIsD1Ev")?,
                foreground: sym(lib, c"_ZN17StarfishMediaAPIs16notifyForegroundEv")?,
                load: sym(lib, c"_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E")?,
                feed: sym(lib, c"_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc")?,
                play: sym(lib, c"_ZN17StarfishMediaAPIs4PlayEv")?,
                pause: sym(lib, c"_ZN17StarfishMediaAPIs5PauseEv")?,
                unload: sym(lib, c"_ZN17StarfishMediaAPIs6UnloadEv")?,
                loaded: sym(lib, c"_ZN17StarfishMediaAPIs15isLoadCompletedEv")?,
                delete: sym(libc::RTLD_DEFAULT, c"_ZdlPv")?,
            })
        }
    }

    /// A new object for one Load. Its memory is never freed (a late callback may still reach it).
    fn fresh(&self) -> Smp {
        let mut s = *self;
        s.obj = Box::into_raw(Box::new(Storage([0; 65536]))).cast();
        UNLOADED.store(false, Ordering::SeqCst);
        unsafe {
            // A null id registers on the media server's pre-authorised namespace.
            (s.ctor)(s.obj, std::ptr::null());
            (s.foreground)(s.obj);
        }
        s
    }

    /// Unloads, and destroys the object once the pipeline says the unload is complete; else leaves it.
    fn retire(&self) {
        unsafe { (self.unload)(self.obj) };
        let t = Instant::now();
        while !UNLOADED.load(Ordering::SeqCst) {
            if t.elapsed() > Duration::from_secs(3) {
                return log("play: no UNLOADCOMPLETED; the object is left");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { (self.dtor)(self.obj) };
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
    payload_dv(app, window, video, audio, None)
}

/// The same with Dolby Vision declared: `profile` is the stream's profile (5 or 8), which the pipeline then shows in
/// Dolby Vision. The node sits in `contents`, as Native Jelly sends it (`with_dolby_hdr_info`).
fn payload_dv(app: &str, window: &str, video: Codec, audio: Option<Codec>, profile: Option<u8>) -> String {
    let p = payload_base(app, window, video, audio);
    match profile.filter(|_| video == Codec::H265 || video == Codec::H264) {
        Some(n) => p.replacen(
            r#""provider":"tvlauncher""#,
            &format!(r#""provider":"tvlauncher","DolbyHdrInfo":{{"trackType":"single","encryptionType":"clear","profileId":{n}}}"#),
            1,
        ),
        None => p,
    }
}

fn payload_base(app: &str, window: &str, video: Codec, audio: Option<Codec>) -> String {
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
        r#"{{"args":[{{"mediaTransportType":"BUFFERSTREAM","option":{{"appId":"{app}","windowId":"{window}","externalStreamingInfo":{{"contents":{{"codec":{{{codec}}},"esInfo":{{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true}},"format":"RAW","provider":"tvlauncher"{pcm}}},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{{"minimum":1,"maximum":1048576}},"srcBufferLevelVideo":{{"minimum":1,"maximum":8388608}}}}}},"needAudio":{},"queryPosition":false,"lowDelayMode":false,"transmission":{{"contentsType":"LIVE"}},"adaptiveStreaming":{{"audioOnly":false,"maxWidth":{w},"maxHeight":{h},"maxFrameRate":60}}}}}}]}}"#,
        audio.is_some(),
        pcm = if audio == Some(Codec::Pcm) {
            r#","pcmInfo":{"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16}"#
        } else {
            ""
        },
    )
}

enum Key {
    Back,
    Step(i64),
    Show,
}

const K_PAGEUP: i32 = 0x4000_004b;
const K_PAGEDOWN: i32 = 0x4000_004e;

/// The remote, while a channel plays: Back (or Home, or quitting) leaves, Up and Down change channel, OK shows
/// the banner. A quit is put back for the main loop.
fn key() -> Option<Key> {
    if STOP.load(Ordering::SeqCst) {
        return Some(Key::Back);
    }
    let mut e = Event::new(0);
    while unsafe { SDL_PollEvent(&mut e) } != 0 {
        if e.kind() == QUIT {
            unsafe { SDL_PushEvent(&mut Event::new(QUIT)) };
            return Some(Key::Back);
        }
        if e.kind() != KEYDOWN {
            continue;
        }
        let (sym, wcode) = (e.key_sym(), e.key_wcode());
        if sym == K_ESCAPE || wcode == WCODE_BACK {
            return Some(Key::Back);
        }
        match sym {
            K_UP | K_PAGEUP => return Some(Key::Step(1)),
            K_DOWN | K_PAGEDOWN => return Some(Key::Step(-1)),
            K_RETURN | K_KP_ENTER => return Some(Key::Show),
            _ => {}
        }
    }
    crate::lifecycle::RELAUNCH.load(Ordering::SeqCst).then_some(Key::Back)
}

/// A fully transparent frame, so the video plane under the UI shows, with the banner over it.
fn frame(g: &Gfx, banner: &Banner) {
    unsafe {
        SDL_SetRenderDrawColor(g.ren, 0, 0, 0, 0);
        SDL_RenderClear(g.ren);
    }
    banner.draw(g);
    unsafe { SDL_RenderPresent(g.ren) };
}

fn hm(t: i64) -> String {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&(t as libc::time_t), &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// The channel's name and what is on, at the bottom of the picture for a few seconds.
struct Banner {
    name: Option<Tex>,
    now: Option<Tex>,
    next: Option<Tex>,
    status: Option<Tex>,
    show: Option<(i64, i64)>,
    until: Instant,
}

impl Banner {
    fn new(g: &Gfx, ch: &Channel, number: usize, guide: &Guide) -> Banner {
        let t = crate::youtube::now() as i64;
        let (on, next) = now_next(guide, &ch.epg, t);
        Banner {
            name: g.text(g.f_row, &format!("{number}   {}", ch.name), 0),
            now: on.and_then(|s| {
                g.text(
                    g.f_sub,
                    &format!("{} - {}   {}", hm(s.start), hm(s.stop), s.title),
                    1500,
                )
            }),
            next: next.and_then(|s| g.text(g.f_sub, &format!("Next   {}   {}", hm(s.start), s.title), 1500)),
            status: g.text(g.f_sub, "Tuning...", 0),
            show: on.map(|s| (s.start, s.stop)),
            until: Instant::now() + Duration::from_secs(6),
        }
    }

    fn say(&mut self, g: &Gfx, s: Option<&str>) {
        self.status = s.and_then(|s| g.text(g.f_sub, s, 1500));
        self.until = Instant::now() + Duration::from_secs(6);
    }

    fn draw(&self, g: &Gfx) {
        if self.status.is_none() && Instant::now() > self.until {
            return;
        }
        let (x, w, h) = (MARGIN, W - 2.0 * MARGIN, 230.0);
        let y = H - h - 64.0;
        g.rounded(x, y, w, h, 28.0, PANEL, 0.94);
        g.put(self.name.as_ref(), x + 48.0, y + 32.0, TEXT, 1.0);
        if let Some(s) = &self.status {
            g.put(Some(s), x + 48.0, y + 100.0, DIM, 1.0);
            return;
        }
        g.put(self.now.as_ref(), x + 48.0, y + 92.0, TEXT, 1.0);
        if let Some((a, b)) = self.show {
            let f = ((crate::youtube::now() as i64 - a) as f32 / (b - a).max(1) as f32).clamp(0.0, 1.0);
            g.rounded(x + 48.0, y + 140.0, 520.0, 6.0, 3.0, BLACK, 0.5);
            g.rounded(x + 48.0, y + 140.0, (520.0 * f).max(6.0), 6.0, 3.0, WHITE, 1.0);
        }
        g.put(self.next.as_ref(), x + 48.0, y + 162.0, DIM, 1.0);
    }
}

enum End {
    Exit,
    Step(i64),
}

/// LG's player, loaded once. Each channel gets an object and an exported window of its own, as in Native Jelly:
/// the video output never shows a second pipeline on the same object or window.
pub struct Player {
    smp: Smp,
    create: unsafe extern "C" fn(c_int) -> *const c_char,
    place: unsafe extern "C" fn(*const c_char, *mut Rect, *mut Rect) -> c_int,
    destroy: unsafe extern "C" fn(*const c_char),
}

/// An exported window, destroyed when dropped.
struct Window<'a>(std::ffi::CString, &'a Player);

impl Drop for Window<'_> {
    fn drop(&mut self) {
        unsafe { (self.1.destroy)(self.0.as_ptr()) };
    }
}

impl Player {
    pub fn new() -> Result<Player, String> {
        unsafe {
            Ok(Player {
                create: sym(libc::RTLD_DEFAULT, c"SDL_webOSCreateExportedWindow")?,
                place: sym(libc::RTLD_DEFAULT, c"SDL_webOSSetExportedWindow")?,
                destroy: sym(libc::RTLD_DEFAULT, c"SDL_webOSDestroyExportedWindow")?,
                smp: Smp::new()?,
            })
        }
    }

    fn window(&self) -> Result<Window<'_>, String> {
        let id = unsafe { (self.create)(0) };
        if id.is_null() {
            return Err("The TV gave no video window.".into());
        }
        Ok(Window(unsafe { CStr::from_ptr(id) }.to_owned(), self))
    }

    /// Plays `list[at]` full screen until Back or Home; Up and Down change channel. Gives the channel it ended on.
    pub fn watch(&mut self, g: &Gfx, list: &[Channel], mut at: usize, guide: &Guide) -> usize {
        loop {
            let mut banner = Banner::new(g, &list[at], at + 1, guide);
            match self.session(g, &list[at], &mut banner) {
                End::Exit => return at,
                End::Step(d) => at = (at as i64 + d).rem_euclid(list.len() as i64) as usize,
            }
        }
    }

    /// Shows why a channel does not play until the viewer leaves or changes channel.
    fn fail(&self, g: &Gfx, banner: &mut Banner, why: &str) -> End {
        log(&format!("play: {why}"));
        banner.say(g, Some(why));
        loop {
            match key() {
                Some(Key::Back) => return End::Exit,
                Some(Key::Step(d)) => return End::Step(d),
                _ => frame(g, banner),
            }
        }
    }

    fn session(&mut self, g: &Gfx, ch: &Channel, banner: &mut Banner) -> End {
        log(&format!("play: {}", ch.name));
        let reader = read(&ch.url, 0.0);
        let t = Instant::now();
        let (video, audio) = loop {
            match key() {
                Some(Key::Back) => return End::Exit,
                Some(Key::Step(d)) => return End::Step(d),
                _ => {}
            }
            match reader.rx.recv_timeout(Duration::from_millis(16)) {
                Ok(Msg::Streams(Some(v), a)) => break (v, a),
                Ok(Msg::Streams(None, _)) => {
                    return self.fail(g, banner, "This channel has no H.264 or H.265 picture.")
                }
                Ok(Msg::End(e)) => return self.fail(g, banner, &e),
                Ok(Msg::Au(_)) | Err(_) => {}
            }
            if t.elapsed() > Duration::from_secs(15) {
                return self.fail(
                    g,
                    banner,
                    "This channel did not send an MPEG-TS stream within 15 seconds.",
                );
            }
            frame(g, banner);
        };
        log(&format!("play: video {video:?}, audio {audio:?}"));
        *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let window = match self.window() {
            Ok(w) => w,
            Err(e) => return self.fail(g, banner, &e),
        };
        let smp = &self.smp.fresh();
        let p = cstr(&payload(APP, &window.0.to_string_lossy(), video, audio));
        if unsafe { (smp.load)(smp.obj, p.as_ptr(), on_event) } & 0xff == 0 {
            return self.fail(g, banner, "The TV's player refused the channel.");
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
            let k = key();
            let err = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take();
            if t.elapsed() > Duration::from_secs(10) || err.is_some() || matches!(k, Some(Key::Back | Key::Step(_))) {
                smp.retire();
                return match k {
                    Some(Key::Back) => End::Exit,
                    Some(Key::Step(d)) => End::Step(d),
                    _ => self.fail(g, banner, &err.unwrap_or("The TV's player did not start.".into())),
                };
            }
            frame(g, banner);
        }
        log(&format!("play: loaded in {} ms", t.elapsed().as_millis()));
        // Placed as 1080p until the pipeline says the picture's size, then again.
        SIZE.store(1920 << 16 | 1080, Ordering::SeqCst);
        let place = |size: u32| {
            let mut src = Rect {
                x: 0,
                y: 0,
                w: (size >> 16) as i32,
                h: (size & 0xffff) as i32,
            };
            let mut dst = Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            };
            let rv = unsafe { (self.place)(window.0.as_ptr(), &mut src, &mut dst) };
            log(&format!("play: window placed for {}x{} ({rv})", src.w, src.h));
            size
        };
        let mut placed = place(SIZE.load(Ordering::SeqCst));

        let (mut pending, mut fed, mut refused) = (None::<Au>, [0u64; 2], 0);
        let (mut last, mut tick, mut started) = ([0i64; 2], Instant::now(), None::<Instant>);
        let end = loop {
            if tick.elapsed() > Duration::from_secs(10) {
                tick = Instant::now();
                log(&format!(
                    "play: fed {}/{} up to {}/{} ms",
                    fed[1],
                    fed[0],
                    last[1] / 1_000_000,
                    last[0] / 1_000_000
                ));
            }
            match key() {
                Some(Key::Back) => break End::Exit,
                Some(Key::Step(d)) => break End::Step(d),
                Some(Key::Show) => banner.say(g, None),
                None => {}
            }
            if let Some(e) = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take() {
                smp.retire();
                return self.fail(g, banner, &e);
            }
            loop {
                let au = match pending.take() {
                    Some(a) => a,
                    None => match reader.rx.try_recv() {
                        Ok(Msg::Au(a)) => a,
                        Ok(Msg::Streams(..)) => continue,
                        Ok(Msg::End(e)) => {
                            smp.retire();
                            return self.fail(g, banner, &e);
                        }
                        Err(_) => break,
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
                        fed[au.video as usize] += 1;
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
                banner.say(g, None);
            }
            let size = SIZE.load(Ordering::SeqCst);
            if size != placed {
                placed = place(size);
            }
            frame(g, banner);
            if started.is_none() && t.elapsed() > Duration::from_secs(20) {
                smp.retire();
                return self.fail(g, banner, "The channel sent no pictures.");
            }
        };
        smp.retire();
        log(&format!(
            "play: stopped after {} pictures and {} audio frames",
            fed[1], fed[0]
        ));
        end
    }
}
/// Set by SIGTERM in the `--player` process: Home wants the screen back, and playback ends as on Back.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// POC: the `--player` process. Opens its own window and plays `url` until Back.
pub fn child(url: &str) -> ExitCode {
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as usize) };
    let Some((_win, ren)) = crate::start("", false) else {
        return ExitCode::FAILURE;
    };
    let g = Gfx::new("", ren, |name, size| crate::font("", name, size));
    let mut p = match Player::new() {
        Ok(p) => p,
        Err(e) => {
            log(&format!("player: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let ch = Channel {
        name: "Test".into(),
        group: String::new(),
        url: url.into(),
        logo: None,
        epg: String::new(),
    };
    p.watch(&g, &[ch], 0, &Guide::new());
    log("player: done");
    ExitCode::SUCCESS
}

/// Starts this program as the player process with `args` and waits for it, swallowing the launcher window's events
/// meanwhile. The Home key (a relaunch, once registered) stops it: SIGTERM first, so LG's pipeline is released and the
/// server hears where it stopped, and a kill after eight seconds. Gives its exit status, None when a signal ended it. The launcher's window is raised again
/// afterwards: the TV leaves it hidden.
pub fn run_child(win: *mut Opaque, wake: u32, args: &[&str]) -> Option<i32> {
    let exe = std::env::current_exe().unwrap_or_default();
    let t = Instant::now();
    // Its own app id: while the player's window carries Home's id the app manager takes Home to be in front already and
    // the Home key does nothing (checked on the TV); with another id the key launches Home, which arrives as a relaunch.
    let mut cmd = std::process::Command::new(&exe);
    cmd.env("APPID", "tvlauncher.player");
    let mut child = match cmd.args(args).spawn() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("player: {}: {e}", exe.display()));
            return Some(1);
        }
    };
    log(&format!(
        "player: pid {} for {}",
        child.id(),
        args.first().unwrap_or(&"")
    ));
    // Only a registered Home hears the Home key (as a relaunch) while another window is in front.
    crate::lifecycle::RELAUNCH.store(false, Ordering::SeqCst);
    crate::lifecycle::register(wake);
    let mut asked: Option<Instant> = None;
    let status = loop {
        if asked.is_none() && crate::lifecycle::RELAUNCH.load(Ordering::SeqCst) {
            log("player: Home key, stopping the player");
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            asked = Some(Instant::now());
        }
        // The player needs up to 3 s to release LG's pipeline and 3 s more to tell the server where it stopped.
        if asked.is_some_and(|t| t.elapsed() > Duration::from_secs(8)) {
            log("player: it did not stop, killing it");
            let _ = child.kill();
        }
        let mut e = Event::new(0);
        while unsafe { SDL_PollEvent(&mut e) } != 0 {}
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    log(&format!("player: ended {status} after {} ms", t.elapsed().as_millis()));
    unsafe {
        SDL_ShowWindow(win);
        SDL_RaiseWindow(win);
    }
    status.code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dts_core_is_declared_as_lgs_stereo_48k_pcm_input() {
        let p = payload("a", "w", Codec::H264, Some(Codec::Pcm));
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        let c = &v["args"][0]["option"]["externalStreamingInfo"]["contents"];
        assert_eq!(c["codec"]["audio"], "PCM");
        assert_eq!(
            c["pcmInfo"],
            serde_json::json!({"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16})
        );
        for a in [Codec::Ac3, Codec::Eac3, Codec::Aac] {
            assert!(!payload("a", "w", Codec::H264, Some(a)).contains("pcmInfo"));
        }
    }

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
