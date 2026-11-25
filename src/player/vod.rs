//! Films and episodes from Jellyfin, in the `--film` process: the server remuxes the title to MPEG-TS HLS (see
//! `film`), and this feeds it to LG's pipeline like a channel, with pause, seeking by restarting the stream at the
//! new place, the resume point and the progress reports.
use super::*;
use crate::film::{self, Clock, Event as Note, Item, Why, TICKS};
use crate::jellyfin::Config;
use crate::subs::{self, Cue};
use std::sync::mpsc::{channel, Receiver};

/// A subtitle track's number and its cues, or why the download failed.
type Subtitles = (i64, Result<Vec<Cue>, String>);

enum Fk {
    Back,
    Toggle,
    Seek(i64),
    Up,
    Down,
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
            (K_UP, _) => return Some(Fk::Up),
            (K_DOWN, _) => return Some(Fk::Down),
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
    /// A status that goes by itself (a failed subtitle download), unlike an error that stays until Back.
    status_until: Option<Instant>,
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
            status_until: None,
            until: Instant::now() + Duration::from_secs(6),
        }
    }

    fn show(&mut self) {
        self.until = Instant::now() + Duration::from_secs(5);
    }

    fn say(&mut self, g: &Gfx, s: Option<&str>) {
        self.status = s.and_then(|s| g.text(g.f_sub, s, 1500));
        self.status_until = None;
        self.show();
    }

    fn note(&mut self, g: &Gfx, s: &str) {
        self.say(g, Some(s));
        self.status_until = Some(Instant::now() + Duration::from_secs(3));
    }

    /// Whether anything was drawn.
    fn draw(&mut self, g: &Gfx, pos_s: i64, paused: bool, seeking: Option<i64>) -> bool {
        if self.status_until.is_some_and(|t| Instant::now() > t) {
            (self.status, self.status_until) = (None, None);
        }
        if !(paused || seeking.is_some() || self.status.is_some() || Instant::now() < self.until) {
            return false;
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
            return true;
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
        true
    }
}

/// What a picture adds to the transparent frame besides the controls.
#[derive(Default)]
struct Over<'a> {
    sub: &'a [Tex],
    menu: Option<&'a Menu>,
}

/// A transparent frame, so the video plane shows, with the subtitle, the controls and the track menu over it.
fn vframe(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<i64>, over: Over) {
    unsafe {
        SDL_SetRenderDrawColor(g.ren, 0, 0, 0, 0);
        SDL_RenderClear(g.ren);
    }
    draw_over(g, osd, pos_s, paused, seeking, over);
    unsafe { SDL_RenderPresent(g.ren) };
}

fn draw_over(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<i64>, over: Over) {
    // The subtitle stands above the controls when they are up.
    let low = if osd.draw(g, pos_s, paused, seeking) {
        H - 300.0
    } else {
        H - 120.0
    };
    if !over.sub.is_empty() {
        let w = over.sub.iter().map(|t| t.w).max().unwrap_or(0) as f32;
        let h: f32 = over.sub.iter().map(|t| t.h as f32).sum();
        g.rounded(
            (W - w) / 2.0 - 20.0,
            low - h - 10.0,
            w + 40.0,
            h + 20.0,
            16.0,
            BLACK,
            0.6,
        );
        let mut y = low - h;
        for t in over.sub {
            g.put(Some(t), (W - t.w as f32) / 2.0, y, TEXT, 1.0);
            y += t.h as f32;
        }
    }
    if let Some(m) = over.menu {
        m.draw(g);
    }
}

/// `--film-shot` on a desktop: the controls, a subtitle and the track menu over a stand-in picture (no player).
pub fn shot(g: &Gfx, menu: bool) {
    let it = film::item_from(
        &serde_json::json!({ "Id": "m1", "Name": "The Silent Harbour", "Type": "Episode",
        "SeriesName": "Harbour Lights", "ParentIndexNumber": 2, "IndexNumber": 5, "RunTimeTicks": 5_400i64 * TICKS,
        "MediaSources": [{ "Id": "s", "MediaStreams": [
            { "Type": "Audio", "Index": 1, "DisplayTitle": "French - AC3 5.1 - Default" },
            { "Type": "Audio", "Index": 2, "DisplayTitle": "English - AAC Stereo" },
            { "Type": "Subtitle", "Index": 3, "DisplayTitle": "French - SRT", "IsTextSubtitleStream": true },
            { "Type": "Subtitle", "Index": 4, "DisplayTitle": "English - SRT", "IsTextSubtitleStream": true },
        ] }] }),
    )
    .expect("item");
    let mut osd = Osd::new(g, &it);
    let choice = Choice {
        sub: Some(3),
        ..Choice::default()
    };
    let menu = menu.then(|| Menu::new(g, &it, &choice)).flatten();
    let sub = lines(g, "Bonjour, le monde.\nIl est tard.");
    g.fill(
        rgb(54, 78, 112),
        1.0,
        FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: H,
        },
    );
    let over = Over {
        sub: &sub,
        menu: menu.as_ref(),
    };
    draw_over(g, &mut osd, 1234, true, None, over);
}

/// A subtitle as one picture per line, so each can be centred.
fn lines(g: &Gfx, text: &str) -> Vec<Tex> {
    text.lines().filter_map(|l| g.text(g.f_row, l, 0)).collect()
}

/// What the viewer chose, kept across seeks (each seek restarts the stream).
#[derive(Default)]
struct Choice {
    audio: Option<i64>,
    sub: Option<i64>,
    cues: Vec<Cue>,
}

enum Pick {
    Head,
    Sub(Option<i64>),
    Audio(i64),
}

/// Subtitles and audio tracks to choose from, with Down.
struct Menu {
    rows: Vec<(Option<Tex>, Pick, bool)>,
    sel: usize,
}

impl Menu {
    /// None when there is nothing to choose: no text subtitles and at most one audio track.
    fn new(g: &Gfx, it: &Item, c: &Choice) -> Option<Menu> {
        let mut rows = vec![];
        let mut row = |name: &str, pick, current| {
            rows.push((g.text(g.f_sub, &crate::jellyfin::clip(name, 46), 0), pick, current))
        };
        if !it.subs.is_empty() {
            row("Subtitles", Pick::Head, false);
            row("Off", Pick::Sub(None), c.sub.is_none());
            for t in &it.subs {
                row(&t.name, Pick::Sub(Some(t.index)), c.sub == Some(t.index));
            }
        }
        if it.audio.len() > 1 {
            row("Audio", Pick::Head, false);
            for (n, t) in it.audio.iter().enumerate() {
                row(
                    &t.name,
                    Pick::Audio(t.index),
                    c.audio == Some(t.index) || (c.audio.is_none() && n == 0),
                );
            }
        }
        let sel = rows.iter().position(|r| r.2 && !matches!(r.1, Pick::Head))?;
        Some(Menu { rows, sel })
    }

    fn step(&mut self, d: i32) {
        let mut i = self.sel as i32;
        loop {
            i += d;
            match self.rows.get(i as usize).filter(|_| i >= 0) {
                None => return,
                Some((_, Pick::Head, _)) => {}
                Some(_) => return self.sel = i as usize,
            }
        }
    }

    fn draw(&self, g: &Gfx) {
        const ROW: f32 = 64.0;
        let shown = self.rows.len().min(9);
        let first = (self.sel + 1).saturating_sub(shown).min(self.rows.len() - shown);
        let (w, h) = (760.0, shown as f32 * ROW + 32.0);
        let (x, y) = (W - MARGIN - w, (H - h) / 2.0);
        g.rounded(x, y, w, h, 28.0, PANEL, 0.96);
        for (n, (tex, pick, current)) in self.rows.iter().enumerate().skip(first).take(shown) {
            let ry = y + 16.0 + (n - first) as f32 * ROW;
            let on = n == self.sel;
            if on {
                g.rounded(x + 16.0, ry + 4.0, w - 32.0, ROW - 8.0, 16.0, TEXT, 1.0);
            }
            let ink = if on {
                rgb(20, 22, 25)
            } else if matches!(pick, Pick::Head) {
                DIM
            } else {
                TEXT
            };
            if *current {
                g.rounded(x + 36.0, ry + ROW / 2.0 - 7.0, 14.0, 14.0, 7.0, ink, 1.0);
            }
            let th = tex.as_ref().map_or(0, |t| t.h) as f32;
            g.put(tex.as_ref(), x + 68.0, ry + (ROW - th) / 2.0, ink, 1.0);
        }
    }
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
        let (mut at, mut choice) = (it.resume, Choice::default());
        film::report(cfg, it, Note::Start, at, false);
        let (pos, done) = loop {
            match self.vod(g, cfg, it, at, &mut osd, &mut choice) {
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
            vframe(g, osd, at / TICKS, false, None, Over::default());
        }
    }

    /// One run of the stream from `start` ticks, until Back, the end, or a seek.
    fn vod(&mut self, g: &Gfx, cfg: &Config, it: &Item, start: i64, osd: &mut Osd, choice: &mut Choice) -> Vod {
        let start_s = start / TICKS;
        osd.say(g, Some("Loading..."));
        let reader = read(&film::master_url(cfg, it, start, choice.audio));
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
            vframe(g, osd, start_s, false, None, Over::default());
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
            vframe(g, osd, start_s, false, None, Over::default());
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
        let mut menu: Option<Menu> = None;
        let mut loading: Option<Receiver<Subtitles>> = None;
        let mut cue: (Option<usize>, Vec<Tex>) = (None, vec![]);
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
            match (fkey(), menu.is_some()) {
                (Some(Fk::Back), true) => menu = None,
                (Some(Fk::Up), true) => menu.iter_mut().for_each(|m| m.step(-1)),
                (Some(Fk::Down), true) => menu.iter_mut().for_each(|m| m.step(1)),
                (Some(Fk::Toggle), true) => {
                    let pick = menu.take().and_then(|m| m.rows.into_iter().nth(m.sel)).map(|r| r.1);
                    match pick {
                        Some(Pick::Sub(None)) => (choice.sub, choice.cues) = (None, vec![]),
                        Some(Pick::Sub(Some(i))) => {
                            osd.note(g, "Loading subtitles...");
                            let (tx, rx) = channel();
                            let (cfg, id, source) = (cfg.clone(), it.id.clone(), it.source.clone());
                            std::thread::spawn(move || {
                                let _ = tx.send((i, film::subtitles(&cfg, &id, &source, i)));
                            });
                            loading = Some(rx);
                        }
                        Some(Pick::Audio(i)) if choice.audio != Some(i) => {
                            choice.audio = Some(i);
                            smp.retire();
                            return Vod::Seek(pos);
                        }
                        _ => {}
                    }
                }
                (_, true) => {}
                (Some(Fk::Back), false) => break Vod::Exit(pos, false),
                (Some(Fk::Up), false) => osd.show(),
                (Some(Fk::Down), false) => match Menu::new(g, it, choice) {
                    Some(m) => menu = Some(m),
                    None => osd.note(g, "No subtitles or other audio tracks"),
                },
                (Some(Fk::Toggle), false) if clock.running() || paused => {
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
                (Some(Fk::Seek(d)), false) => {
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
            if let Some((to, _)) = seek.filter(|s| s.1.elapsed() > Duration::from_millis(600)) {
                smp.retire();
                return Vod::Seek(to * TICKS);
            }
            if let Some(e) = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take() {
                smp.retire();
                return self.vod_fail(g, osd, &e, pos);
            }
            if let Some((i, got)) = loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
                loading = None;
                match got {
                    Ok(c) if !c.is_empty() => {
                        (choice.sub, choice.cues, cue) = (Some(i), c, (None, vec![]));
                        osd.say(g, None);
                    }
                    Ok(_) => osd.note(g, "These subtitles are empty"),
                    Err(e) => osd.note(g, &format!("Subtitles: {e}")),
                }
            }
            let want = choice.sub.and_then(|_| subs::at(&choice.cues, pos / 10_000));
            if want != cue.0 {
                cue = (want, want.map_or(vec![], |i| lines(g, &choice.cues[i].text)));
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
            let over = Over {
                sub: &cue.1,
                menu: menu.as_ref(),
            };
            vframe(g, osd, pos / TICKS, paused, seek.map(|s| s.0), over);
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
