//! Films and episodes from Jellyfin, in the `--film` process: the server remuxes the title to MPEG-TS HLS (see
//! `film`), and this feeds it to LG's pipeline like a channel, with pause, seeking by restarting the stream at the
//! new place, the resume point and the progress reports.
use super::*;
use crate::film::{self, Clock, Event as Note, Item, Why, TICKS};
use crate::jellyfin::Config;

enum Fk {
    Back,
    Toggle,
    Seek(i64),
    Show,
}

/// The remote while a title plays. Seconds are what a key seeks by: Left and Right 10, the rewind and fast-forward
/// buttons 30.
fn fkey() -> Option<Fk> {
    if STOP.load(Ordering::SeqCst) || crate::lifecycle::RELAUNCH.load(Ordering::SeqCst) {
        return Some(Fk::Back);
    }
    let mut e = Event::new(0);
    while unsafe { SDL_PollEvent(&mut e) } != 0 {
        if e.kind() == QUIT {
            unsafe { SDL_PushEvent(&mut Event::new(QUIT)) };
            return Some(Fk::Back);
        }
        if e.kind() != KEYDOWN {
            continue;
        }
        let (sym, wcode) = (e.key_sym(), e.key_wcode());
        if sym == K_ESCAPE || wcode == WCODE_BACK {
            return Some(Fk::Back);
        }
        match (sym, wcode) {
            // 415 and 19 are the remote's Play and Pause
            (K_RETURN | K_KP_ENTER, _) | (_, 415 | 19) => return Some(Fk::Toggle),
            (K_LEFT, _) => return Some(Fk::Seek(-10)),
            (K_RIGHT, _) => return Some(Fk::Seek(10)),
            (_, 412) => return Some(Fk::Seek(-30)),
            (_, 417) => return Some(Fk::Seek(30)),
            (K_UP | K_DOWN, _) => return Some(Fk::Show),
            _ => log(&format!("film: key {sym} wcode {wcode}")),
        }
    }
    None
}

enum Vod {
    /// Where it stopped, in ticks, and whether it played to the end.
    Exit(i64, bool),
    /// Start again at this place: a seek.
    Seek(i64),
}

/// The title, the bar and the times, at the bottom of the picture for a few seconds, and while paused.
struct Osd {
    name: Option<Tex>,
    sub: Option<Tex>,
    paused: Option<Tex>,
    total: Option<Tex>,
    total_s: i64,
    at: (String, Option<Tex>),
    status: Option<Tex>,
    until: Instant,
}

impl Osd {
    fn new(g: &Gfx, it: &Item) -> Osd {
        let total_s = it.total / TICKS;
        Osd {
            name: g.text(g.f_row, &it.name, 1500),
            sub: g.text(g.f_sub, &it.sub, 1500),
            paused: g.text(g.f_sub, "Paused", 0),
            total: g.text(g.f_sub, &film::clock(total_s), 0),
            total_s,
            at: (String::new(), None),
            status: None,
            until: Instant::now() + Duration::from_secs(6),
        }
    }

    fn show(&mut self) {
        self.until = Instant::now() + Duration::from_secs(5);
    }

    fn say(&mut self, g: &Gfx, s: Option<&str>) {
        self.status = s.and_then(|s| g.text(g.f_sub, s, 1500));
        self.show();
    }

    fn draw(&mut self, g: &Gfx, pos_s: i64, paused: bool, seeking: Option<i64>) {
        if !(paused || seeking.is_some() || self.status.is_some() || Instant::now() < self.until) {
            return;
        }
        let shown = seeking.unwrap_or(pos_s);
        let label = film::clock(shown);
        if self.at.0 != label {
            self.at = (label.clone(), g.text(g.f_sub, &label, 0));
        }
        let (x, w, h) = (MARGIN, W - 2.0 * MARGIN, 210.0);
        let y = H - h - 64.0;
        g.rounded(x, y, w, h, 28.0, PANEL, 0.94);
        g.put(self.name.as_ref(), x + 48.0, y + 28.0, TEXT, 1.0);
        g.put(self.sub.as_ref(), x + 48.0, y + 82.0, DIM, 1.0);
        if let Some(s) = &self.status {
            g.put(Some(s), x + 48.0, y + 132.0, DIM, 1.0);
            return;
        }
        if let Some(p) = self.paused.as_ref().filter(|_| paused) {
            g.put(Some(p), x + w - 48.0 - p.w as f32, y + 34.0, TEXT, 1.0);
        }
        let (bx, bw, by) = (x + 48.0, w - 96.0, y + h - 72.0);
        g.rounded(bx, by, bw, 8.0, 4.0, BLACK, 0.5);
        let f = if self.total_s > 0 {
            (shown as f32 / self.total_s as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if bw * f >= 8.0 {
            g.rounded(bx, by, bw * f, 8.0, 4.0, TEXT, 1.0);
        }
        g.put(self.at.1.as_ref(), bx, by + 22.0, TEXT, 1.0);
        if let Some(t) = &self.total {
            g.put(Some(t), bx + bw - t.w as f32, by + 22.0, DIM, 1.0);
        }
    }
}

/// A transparent frame, so the video plane shows, with the controls over it.
fn vframe(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<i64>) {
    unsafe {
        SDL_SetRenderDrawColor(g.ren, 0, 0, 0, 0);
        SDL_RenderClear(g.ren);
    }
    osd.draw(g, pos_s, paused, seeking);
    unsafe { SDL_RenderPresent(g.ren) };
}

/// The media server only connects the decoder to the screen for an app it believes is in front.
fn foreground() {
    let _ = std::process::Command::new("/usr/bin/luna-send")
        .args(["-n", "1", "-w", "3000", "luna://com.webos.media/notifyAppLifeStatus"])
        .arg(format!(
            r#"{{"appId":"{APP}","status":"foreground","windowType":"card"}}"#
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
}

impl Player {
    /// Plays `it` full screen until Back. Gives where it stopped, in ticks, and whether it played to the end.
    pub fn film(&mut self, g: &Gfx, cfg: &Config, it: &Item) -> (i64, bool) {
        let mut osd = Osd::new(g, it);
        let mut at = it.resume;
        film::report(cfg, it, Note::Start, at, false);
        let (pos, done) = loop {
            match self.vod(g, cfg, it, at, &mut osd) {
                Vod::Seek(t) => at = t,
                Vod::Exit(pos, done) => break (pos, done),
            }
        };
        film::report(cfg, it, Note::Stop, if done { it.total } else { pos }, false);
        (pos, done)
    }

    /// Shows why the title does not play until the viewer leaves.
    fn vod_fail(&self, g: &Gfx, osd: &mut Osd, why: &str, at: i64) -> Vod {
        log(&format!("film: {why}"));
        osd.say(g, Some(why));
        loop {
            if matches!(fkey(), Some(Fk::Back)) {
                return Vod::Exit(at, false);
            }
            vframe(g, osd, at / TICKS, false, None);
        }
    }

    /// One run of the stream from `start` ticks, until Back, the end, or a seek.
    fn vod(&mut self, g: &Gfx, cfg: &Config, it: &Item, start: i64, osd: &mut Osd) -> Vod {
        let start_s = start / TICKS;
        osd.say(g, Some("Loading..."));
        let reader = read(&film::master_url(cfg, it, start));
        let t = Instant::now();
        let (video, audio) = loop {
            if matches!(fkey(), Some(Fk::Back)) {
                return Vod::Exit(start, false);
            }
            match reader.rx.recv_timeout(Duration::from_millis(16)) {
                Ok(Msg::Streams(Some(v), a)) => break (v, a),
                Ok(Msg::Streams(None, _)) => {
                    return self.vod_fail(g, osd, "The server sent no H.264 or H.265 picture.", start)
                }
                Ok(Msg::End(e)) => return self.vod_fail(g, osd, &e, start),
                Ok(Msg::Au(_)) | Err(_) => {}
            }
            if t.elapsed() > Duration::from_secs(60) {
                return self.vod_fail(g, osd, "The server did not start the video within a minute.", start);
            }
            vframe(g, osd, start_s, false, None);
        };
        log(&format!("film: video {video:?}, audio {audio:?}"));
        *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let window = match self.window() {
            Ok(w) => w,
            Err(e) => return self.vod_fail(g, osd, &e, start),
        };
        let smp = &self.smp.fresh();
        let p = cstr(&payload(APP, &window.0.to_string_lossy(), video, audio));
        if unsafe { (smp.load)(smp.obj, p.as_ptr(), on_event) } & 0xff == 0 {
            return self.vod_fail(g, osd, "The TV's player refused the video.", start);
        }
        foreground();
        let t = Instant::now();
        while unsafe { (smp.loaded)(smp.obj) } & 0xff == 0 {
            let k = fkey();
            let err = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take();
            if t.elapsed() > Duration::from_secs(10) || err.is_some() || matches!(k, Some(Fk::Back)) {
                smp.retire();
                return match k {
                    Some(Fk::Back) => Vod::Exit(start, false),
                    _ => self.vod_fail(g, osd, &err.unwrap_or("The TV's player did not start.".into()), start),
                };
            }
            vframe(g, osd, start_s, false, None);
        }
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
            log(&format!("film: window placed for {}x{} ({rv})", src.w, src.h));
            size
        };
        let mut placed = place(SIZE.load(Ordering::SeqCst));

        let mut clock = Clock::default();
        let (mut pending, mut refused) = (None::<Au>, 0);
        let (mut last, mut tick) = ([0i64; 2], Instant::now());
        let (mut paused, mut eof) = (false, false);
        let mut seek: Option<(i64, Instant)> = None;
        let ticks = |clock: &Clock| start + (clock.now().as_nanos() / 100) as i64;
        let end = loop {
            let pos = ticks(&clock);
            if tick.elapsed() > Duration::from_secs(10) {
                tick = Instant::now();
                if clock.running() {
                    film::report(cfg, it, Note::Progress, pos, false);
                }
                log(&format!(
                    "film: at {} s, fed up to {}/{} ms",
                    pos / TICKS,
                    last[1] / 1_000_000,
                    last[0] / 1_000_000
                ));
            }
            match fkey() {
                Some(Fk::Back) => break Vod::Exit(pos, false),
                Some(Fk::Show) => osd.show(),
                Some(Fk::Toggle) if clock.running() || paused => {
                    paused = !paused;
                    if paused {
                        unsafe { (smp.pause)(smp.obj) };
                        clock.hold();
                    } else {
                        unsafe { (smp.play)(smp.obj) };
                        clock.run();
                    }
                    film::report(cfg, it, Note::Progress, pos, paused);
                    osd.show();
                }
                Some(Fk::Seek(d)) => {
                    let top = if it.total > 0 {
                        (it.total / TICKS - 5).max(0)
                    } else {
                        i64::MAX
                    };
                    let base = seek.map_or(pos / TICKS, |s| s.0);
                    seek = Some(((base + d).clamp(0, top), Instant::now()));
                    osd.show();
                }
                _ => {}
            }
            if let Some((to, since)) = seek.filter(|s| s.1.elapsed() > Duration::from_millis(600)) {
                let _ = since;
                smp.retire();
                return Vod::Seek(to * TICKS);
            }
            if let Some(e) = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take() {
                smp.retire();
                return self.vod_fail(g, osd, &e, pos);
            }
            loop {
                let au = match pending.take() {
                    Some(a) => a,
                    None => match reader.rx.try_recv() {
                        Ok(Msg::Au(a)) => a,
                        Ok(Msg::Streams(..)) => continue,
                        Ok(Msg::End(e)) if e == ENDED => {
                            eof = true;
                            break;
                        }
                        Ok(Msg::End(e)) => {
                            smp.retire();
                            return self.vod_fail(g, osd, &e, pos);
                        }
                        Err(_) => break,
                    },
                };
                // Never more than AHEAD_NS past the playhead, which stands still while paused.
                if au.pts_ns > clock.now().as_nanos() as i64 + AHEAD_NS {
                    pending = Some(au);
                    break;
                }
                match smp.feed(&au) {
                    Fed::Ok => last[au.video as usize] = au.pts_ns,
                    Fed::Full => {
                        pending = Some(au);
                        break;
                    }
                    Fed::Refused(r) => {
                        refused += 1;
                        if refused <= 5 {
                            log(&format!("film: Feed answered {r:?}"));
                        }
                    }
                }
            }
            if !clock.running() && !paused && last[1] >= 700_000_000 && (audio.is_none() || last[0] >= 300_000_000) {
                clock.run();
                let ok = unsafe { (smp.play)(smp.obj) } & 0xff;
                log(&format!("film: Play {ok}"));
                osd.say(g, None);
                film::report(cfg, it, Note::Progress, pos, false);
            }
            let size = SIZE.load(Ordering::SeqCst);
            if size != placed {
                placed = place(size);
            }
            if eof && pending.is_none() && clock.now().as_nanos() as i64 + 300_000_000 >= last[1] {
                break Vod::Exit(it.total, true);
            }
            vframe(g, osd, pos / TICKS, paused, seek.map(|s| s.0));
            if !clock.running() && !paused && t.elapsed() > Duration::from_secs(40) {
                smp.retire();
                return self.vod_fail(g, osd, "The server sent no pictures.", start);
            }
        };
        smp.retire();
        end
    }
}

/// `--film <item id>`: resolves the item, opens its own window and plays until Back. Exits with `film::ELSEWHERE`
/// (or 1) before opening anything when Home should open the title in the Jellyfin app instead.
pub fn child(id: &str) -> ExitCode {
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as usize) };
    let Some(cfg) = Config::load("") else {
        log("film: not signed in to Jellyfin");
        return ExitCode::from(1);
    };
    let it = match film::resolve(&cfg, id) {
        Ok(it) => it,
        Err(Why::Elsewhere) => return ExitCode::from(film::ELSEWHERE),
        Err(Why::Failed(e)) => {
            log(&format!("film: {e}"));
            return ExitCode::from(1);
        }
    };
    log(&format!(
        "film: {} ({}), resume at {} s",
        it.name,
        it.sub,
        it.resume / TICKS
    ));
    let Some((_win, ren)) = crate::start("", false) else {
        return ExitCode::from(1);
    };
    let g = Gfx::new("", ren, |name, size| crate::font("", name, size));
    let mut p = match Player::new() {
        Ok(p) => p,
        Err(e) => {
            log(&format!("film: {e}"));
            return ExitCode::from(1);
        }
    };
    let (pos, done) = p.film(&g, &cfg, &it);
    log(&format!(
        "film: stopped at {} s{}",
        pos / TICKS,
        if done { " (finished)" } else { "" }
    ));
    ExitCode::SUCCESS
}
