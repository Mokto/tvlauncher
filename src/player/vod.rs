//! Films and episodes from Jellyfin and Plex, in the `--film` process. The file is read in place (`ff`) and cut into
//! the units LG's pipeline takes, with the subtitles, the audio tracks and the seeking the file allows; what the TV
//! cannot play as it is (10-bit H.264, DTS audio, a server that refuses byte ranges) is converted by the server and
//! read as HLS, as a channel is. Pause, the resume point and the progress reports are the same for both.
mod ui;

use super::*;
use crate::ff::demux::{Demux, Feed, How, Queue, Run, ENDED};
use crate::film::{self, Backend, Clock, Event as Note, Item, Why, TICKS};
use crate::subs::Cue;
use std::sync::mpsc::{channel, Receiver};
use ui::*;

pub use ui::shot;

/// A subtitle track's number and its cues, or why the download failed.
type Subtitles = (SubSel, Result<Vec<Cue>, String>);

enum Vod {
    /// Where it stopped, in ticks, and whether it played to the end.
    Exit(i64, bool),
    /// Start again at this place: a seek, or another track.
    Seek(i64),
    /// Carry on at this place with the server converting the film (the file cannot be played as it is).
    Server(i64, String),
}

/// How the film comes to the TV.
enum Source {
    /// The file, read in place; `audio` is the stream the player starts with.
    File(Demux),
    Server,
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

/// Whether this TV shows Dolby Vision (`tv.config.supportDolbyHDRContents`): the picture is declared Dolby Vision to
/// the pipeline only then.
fn dolby_vision_tv() -> bool {
    static DV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DV.get_or_init(|| {
        std::process::Command::new("/usr/bin/luna-send")
            .args([
                "-n",
                "1",
                "-w",
                "3000",
                "luna://com.webos.service.config/getConfigs",
                r#"{"configNames":["tv.config.supportDolbyHDRContents"]}"#,
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(r#""tv.config.supportDolbyHDRContents":true"#))
            .unwrap_or(false)
    })
}

/// The server's HLS stream as a run: a thread carries what `read` finds into a queue.
fn hls_run(url: &str) -> Run {
    let q = Queue::new(12 * 1024 * 1024);
    let reader = read(url);
    let out = q.clone();
    std::thread::spawn(move || {
        while !out.is_closed() {
            let Ok(m) = reader.rx.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            let ok = match m {
                Msg::Streams(Some(v), a) => out.push(Feed::Streams {
                    video: Some(v),
                    audio: a,
                    dv: None,
                }),
                Msg::Streams(None, _) => out.push(Feed::End("The server sent no H.264 or H.265 picture.".into())),
                Msg::Au(a) => out.push(Feed::Au(a)),
                Msg::End(e) => out.push(Feed::End(e)),
            };
            if !ok {
                break;
            }
        }
    });
    Run::new(q, None)
}

impl Player {
    /// Plays `it` full screen until Back. Gives where it stopped, in ticks, and whether it played to the end.
    pub fn film(&mut self, g: &Gfx, it: &Item) -> (i64, bool) {
        let mut osd = Osd::new(g, it);
        osd.say(g, Some("Loading..."));
        let mut src = match self.open_file(g, it, &mut osd) {
            Ok(Some(d)) => Source::File(d),
            Ok(None) => return (it.resume, false),
            Err(why) => {
                log(&format!("film: the server will convert it: {why}"));
                Source::Server
            }
        };
        let mut choice = Choice::default();
        let mut events = Events::default();
        let mut at = it.resume;
        film::report(it, Note::Start, at, false, matches!(src, Source::File(_)), false);
        let (pos, done) = loop {
            let out = self.vod(g, it, &src, at, &mut osd, &mut choice, &mut events);
            match out {
                Vod::Seek(t) => at = t,
                Vod::Server(t, why) => {
                    log(&format!("film: the server converts it from here: {why}"));
                    src = Source::Server;
                    events.clear();
                    (choice.sub, at) = (choice.sub.filter(|s| s.server), t);
                }
                Vod::Exit(pos, done) => break (pos, done),
            }
        };
        film::report(
            it,
            Note::Stop,
            if done { it.total } else { pos },
            false,
            matches!(src, Source::File(_)),
            done,
        );
        (pos, done)
    }

    /// Opens the file while the viewer waits. Ok(None) when Back was pressed meanwhile, Err(why) when the file
    /// cannot be played as it is.
    fn open_file(&mut self, g: &Gfx, it: &Item, osd: &mut Osd) -> Result<Option<Demux>, String> {
        let url = it.direct.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Demux::open(&url));
        });
        let t = Instant::now();
        let d = loop {
            if matches!(fkey(), Some(Fk::Back)) {
                return Ok(None);
            }
            match rx.try_recv() {
                Ok(r) => break r?,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => return Err("the demuxer stopped".into()),
            }
            if t.elapsed() > Duration::from_secs(45) {
                return Err("the server did not answer in time".into());
            }
            vframe(g, osd, it.resume / TICKS, false, None, Over::default());
        };
        d.info.plan(it.default_audio)?;
        Ok(Some(d))
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

    /// The tracks the viewer can choose from.
    fn tracks(&self, it: &Item, src: &Source) -> Tracks {
        match src {
            Source::File(d) => {
                let mut t = Tracks::default();
                for a in &d.info.audio {
                    let note = if a.how == How::No {
                        " (the server converts it)"
                    } else {
                        ""
                    };
                    t.audio.push(AudioRow {
                        index: i64::from(a.stream),
                        name: format!("{}{note}", a.name),
                    });
                }
                for s in &d.info.subs {
                    t.subs.push(SubRow {
                        index: i64::from(s.stream),
                        name: s.name.clone(),
                        server: false,
                    });
                }
                for s in it.subs.iter().filter(|s| s.external) {
                    t.subs.push(SubRow {
                        index: s.index,
                        name: s.name.clone(),
                        server: true,
                    });
                }
                t
            }
            Source::Server => Tracks {
                audio: it
                    .audio
                    .iter()
                    .map(|a| AudioRow {
                        index: a.index,
                        name: a.name.clone(),
                    })
                    .collect(),
                subs: it
                    .subs
                    .iter()
                    .map(|s| SubRow {
                        index: s.index,
                        name: s.name.clone(),
                        server: true,
                    })
                    .collect(),
            },
        }
    }

    /// One run from `start` ticks, until Back, the end, or a seek.
    #[allow(clippy::too_many_arguments)]
    fn vod(
        &mut self,
        g: &Gfx,
        it: &Item,
        src: &Source,
        start: i64,
        osd: &mut Osd,
        choice: &mut Choice,
        events: &mut Events,
    ) -> Vod {
        let direct = matches!(src, Source::File(_));
        let tracks = self.tracks(it, src);
        let mut shown = Shown::default();
        // The first run picks what the server would start with.
        if !choice.init {
            choice.init = true;
            choice.audio = match src {
                Source::File(d) => d.info.plan(it.default_audio).ok().map(i64::from),
                Source::Server => it.default_audio,
            };
            if let Some(ds) = it.default_sub {
                choice.sub = tracks.subs.iter().find(|s| s.index == ds).map(|s| SubSel {
                    index: s.index,
                    server: s.server,
                });
            }
        }
        let mut loading: Option<Receiver<Subtitles>> = None;
        let fetch = |sel: SubSel| -> Receiver<Subtitles> {
            let (tx, rx) = channel();
            let (backend, id, source) = (it.backend.clone(), it.id.clone(), it.source.clone());
            let track = it.subs.iter().find(|t| t.index == sel.index).cloned();
            std::thread::spawn(move || {
                let got = match track {
                    Some(t) => film::subtitles(&backend, &id, &source, &t),
                    None => Err("the server has no such track".to_string()),
                };
                let _ = tx.send((sel, got));
            });
            rx
        };
        // A server-served subtitle chosen before this run is loaded once and kept; the file's own come with the run.
        if choice.sub.is_some_and(|s| s.server) && events.is_empty() {
            loading = choice.sub.map(&fetch);
        }
        if !choice.sub.is_some_and(|s| s.server) {
            events.clear();
        }

        let start_s = start / TICKS;
        let run = match src {
            Source::File(d) => {
                let audio = choice.audio.map_or(-1, |a| a as i32);
                let sub = choice.sub.filter(|s| !s.server).map(|s| s.index as i32);
                d.start(start * 100, audio, sub)
            }
            Source::Server => hls_run(&film::hls_url(it, start, choice.audio)),
        };
        let t = Instant::now();
        let (video, audio, dv) = loop {
            if matches!(fkey(), Some(Fk::Back)) {
                return Vod::Exit(start, false);
            }
            match run.q.pop_timeout(Duration::from_millis(16)) {
                Some(Feed::Streams {
                    video: Some(v),
                    audio,
                    dv,
                }) => break (v, audio, dv),
                Some(Feed::End(e)) if direct => return Vod::Server(start, e),
                Some(Feed::End(e)) => return self.vod_fail(g, osd, &e, start),
                _ => {}
            }
            if t.elapsed() > Duration::from_secs(60) {
                return if direct {
                    Vod::Server(start, "the file gave no picture in a minute".into())
                } else {
                    self.vod_fail(g, osd, "The server did not start the video within a minute.", start)
                };
            }
            vframe(g, osd, start_s, false, None, Over::default());
        };
        log(&format!("film: video {video:?}, audio {audio:?}, dolby vision {dv:?}"));
        *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let window = match self.window() {
            Ok(w) => w,
            Err(e) => return self.vod_fail(g, osd, &e, start),
        };
        let smp = &self.smp.fresh();
        let profile = dv.as_ref().filter(|d| matches!(d.profile, 5 | 8)).map(|d| d.profile);
        let profile = profile.filter(|_| dolby_vision_tv());
        let p = cstr(&payload_dv(APP, &window.0.to_string_lossy(), video, audio, profile));
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
        let (mut pending, mut refused) = (None::<crate::ts::Au>, 0);
        let (mut last, mut tick) = ([0i64; 2], Instant::now());
        let (mut paused, mut eof) = (false, false);
        let mut seek: Option<(i64, Instant)> = None;
        let mut menu: Option<Menu> = None;
        // The time in the file of the run's time 0: what was asked for, then the keyframe it began on.
        let mut origin = start;
        let end = loop {
            let pos = origin + (clock.now().as_nanos() / 100) as i64;
            if tick.elapsed() > Duration::from_secs(10) {
                tick = Instant::now();
                if clock.running() {
                    film::report(it, Note::Progress, pos, false, direct, false);
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
                        Some(Pick::Sub(None)) => {
                            let embedded = choice.sub.is_some_and(|s| !s.server);
                            choice.sub = None;
                            events.clear();
                            shown.reset();
                            if embedded {
                                smp.retire();
                                return Vod::Seek(pos);
                            }
                        }
                        Some(Pick::Sub(Some(sel))) if sel.server => {
                            osd.note(g, "Loading subtitles...");
                            loading = Some(fetch(sel));
                        }
                        Some(Pick::Sub(Some(sel))) => {
                            // A track of the file is read with the video: the run starts again here with it.
                            choice.sub = Some(sel);
                            events.clear();
                            smp.retire();
                            return Vod::Seek(pos);
                        }
                        Some(Pick::Audio(i)) if choice.audio != Some(i) => {
                            choice.audio = Some(i);
                            smp.retire();
                            let can = match src {
                                Source::File(d) => d
                                    .info
                                    .audio
                                    .iter()
                                    .any(|a| i64::from(a.stream) == i && a.how != How::No),
                                Source::Server => true,
                            };
                            return if can {
                                Vod::Seek(pos)
                            } else {
                                Vod::Server(pos, "the TV cannot decode that audio track".into())
                            };
                        }
                        _ => {}
                    }
                }
                (_, true) => {}
                (Some(Fk::Back), false) => break Vod::Exit(pos, false),
                (Some(Fk::Up), false) => osd.show(),
                (Some(Fk::Down), false) => match Menu::new(g, &tracks, choice) {
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
                    film::report(it, Note::Progress, pos, paused, direct, false);
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
            if let Some((sel, got)) = loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
                loading = None;
                match got {
                    Ok(c) if !c.is_empty() => {
                        *events = Events::from_cues(c);
                        choice.sub = Some(sel);
                        shown.reset();
                        osd.say(g, None);
                    }
                    Ok(_) => osd.note(g, "These subtitles are empty"),
                    Err(e) => osd.note(g, &format!("Subtitles: {e}")),
                }
            }
            loop {
                let au = match pending.take() {
                    Some(a) => a,
                    None => match run.q.try_pop() {
                        Some(Feed::Au(a)) => a,
                        Some(Feed::Base(b)) => {
                            origin = b / 100;
                            continue;
                        }
                        Some(Feed::Sub(ev)) => {
                            if choice.sub.is_some_and(|s| !s.server) {
                                events.insert(ev);
                            }
                            continue;
                        }
                        Some(Feed::Streams { .. }) => continue,
                        Some(Feed::End(e)) if e == ENDED => {
                            eof = true;
                            break;
                        }
                        Some(Feed::End(e)) if direct && !clock.running() => {
                            smp.retire();
                            return Vod::Server(pos, e);
                        }
                        Some(Feed::End(e)) => {
                            smp.retire();
                            return self.vod_fail(g, osd, &e, pos);
                        }
                        None => break,
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
                film::report(it, Note::Progress, pos, false, direct, false);
            }
            let size = SIZE.load(Ordering::SeqCst);
            if size != placed {
                placed = place(size);
            }
            if eof && pending.is_none() && clock.now().as_nanos() as i64 + 300_000_000 >= last[1] {
                break Vod::Exit(it.total, true);
            }
            let ms = pos / 10_000;
            if choice.sub.is_some_and(|s| !s.server) {
                events.prune(ms);
            }
            if choice.sub.is_some() {
                shown.update(g, events, ms);
            }
            let over = Over {
                shown: choice.sub.map(|_| &shown),
                menu: menu.as_ref(),
            };
            vframe(g, osd, pos / TICKS, paused, seek.map(|s| s.0), over);
            if !clock.running() && !paused && t.elapsed() > Duration::from_secs(40) {
                smp.retire();
                return if direct {
                    Vod::Server(pos, "the file gave no pictures".into())
                } else {
                    self.vod_fail(g, osd, "The server sent no pictures.", start)
                };
            }
        };
        smp.retire();
        end
    }
}

/// `--film <item>`: resolves the item, opens its own window and plays until Back. Exits with `film::ELSEWHERE` (or
/// 1) before opening anything when Home should open the title in the media app instead.
pub fn child(spec: &str) -> ExitCode {
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as usize) };
    let it = match film::resolve(spec) {
        Ok(it) => it,
        Err(Why::Elsewhere) => return ExitCode::from(film::ELSEWHERE),
        Err(Why::Failed(e)) => {
            log(&format!("film: {e}"));
            return ExitCode::from(1);
        }
    };
    log(&format!(
        "film: {} ({}) on {}, resume at {} s",
        it.name,
        it.sub,
        match it.backend {
            Backend::Jellyfin(_) => "Jellyfin",
            Backend::Plex(_) => "Plex",
        },
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
    let (pos, done) = p.film(&g, &it);
    log(&format!(
        "film: stopped at {} s{}",
        pos / TICKS,
        if done { " (finished)" } else { "" }
    ));
    ExitCode::SUCCESS
}
