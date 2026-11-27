//! Films and episodes from Jellyfin and Plex, in the `--film` process. The file is read in place (`ff`) and cut into
//! the units LG's pipeline takes, with the subtitles, the audio tracks and the seeking the file allows; what the TV
//! cannot play as it is (10-bit H.264, DTS audio, a server that refuses byte ranges) is converted by the server and
//! read as HLS, as a channel is. Pause, the resume point and the progress reports are the same for both.
mod thumbs;
mod ui;

use super::*;
use crate::ff::ass::Ass;
use crate::ff::demux::{Demux, Feed, Queue, Run, ENDED};
use crate::ff::dolby;
use crate::ff::lossless;
use crate::film::{self, extras, Backend, Clock, Event as Note, Item, Track, Why, TICKS};
use crate::subs::Cue;
use std::sync::mpsc::{channel, Receiver};
use thumbs::Thumbs;
use ui::*;

fn media_root() -> String {
    std::env::var("TVLAUNCHER_PROFILE_ROOT").unwrap_or_else(|_| root())
}

pub use ui::shot;

/// A subtitle track's number and its cues, or why the download failed.
type Subtitles = (SubSel, Result<Vec<Cue>, String>);

enum Vod {
    /// Where it stopped, in ticks, and whether it played to the end.
    Exit(i64, bool),
    /// Start again at this place: a seek, or another track.
    Seek(i64),
    /// Carry on at this place with the server converting the film (the file cannot be played as it is).
    Server(i64, &'static str, String),
    /// The viewer took the next episode during the credits.
    Next,
    /// The viewer asked for the title's page (Info): stopped at this place.
    Page(i64),
}

/// How a viewing ended.
pub enum After {
    Stopped,
    /// Played to the end, the next episode not chosen yet.
    Finished,
    /// The next episode, chosen on its card during the credits.
    PlayNext(Box<Item>),
    /// Stopped, and Home should show the title's page.
    Page,
}

/// The next episode's card during the credits, kept across seeks: the episode once the server has named it, and
/// whether the viewer chose to watch the credits.
#[derive(Default)]
struct Up {
    on: bool,
    asked: Option<Receiver<Option<Item>>>,
    next: Option<Item>,
    card: Option<UpCard>,
    dismissed: bool,
}

/// What one run plays: the title, where it comes from, where it starts, and whether it is a first start (the loading
/// screen covers the screen) rather than a seek.
#[derive(Clone, Copy)]
struct Start<'a> {
    g: &'a Gfx,
    it: &'a Item,
    src: &'a Source,
    start: i64,
    fresh: bool,
}

/// How long the card counts down before the next episode starts.
const UP_SECS: i64 = 10;

/// How the film comes to the TV.
enum Source {
    /// The file, read in place; `audio` is the stream the player starts with.
    File(Demux),
    Server(&'static str),
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

/// The server's HLS stream as a run: a thread carries what `read` finds into a queue.
fn hls_run(url: &str, skip_s: f64) -> Run {
    let q = Queue::new(12 * 1024 * 1024);
    let reader = read(url, skip_s);
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
    /// Plays `it` full screen until Back. Gives where it stopped, in ticks, and how it ended. `countdown`: the next
    /// episode's card comes up when the credits begin.
    pub fn film(&mut self, g: &Gfx, it: &Item, countdown: bool) -> (i64, After) {
        let mut osd = Osd::new(g, it);
        let mode = film::direct_mode();
        let forced = |why: &str| format!("Direct Play is forced, and this file cannot be played as it is: {why}");
        let mut src = if mode == 1 {
            Source::Server("Direct Play is off in this profile")
        } else {
            match self.open_file(g, it, &mut osd) {
                Ok(Some(d)) => Source::File(d),
                Ok(None) => return (it.resume, After::Stopped),
                Err((_, why)) if mode == 2 => {
                    let p = match self.vod_fail(g, &mut osd, &forced(&why), it.resume) {
                        Vod::Exit(p, _) => p,
                        _ => it.resume,
                    };
                    return (p, After::Stopped);
                }
                Err((reason, why)) => {
                    log(&format!("film: the server will convert it: {why}"));
                    Source::Server(reason)
                }
            }
        };
        let mut choice = Choice {
            look: clamp_look(crate::data::Prefs::load(&media_root()).look),
            ..Choice::default()
        };
        set_sub_size(SIZES[choice.look.sub_size].1);
        let mut events = Events::default();
        let mut thumbs = Thumbs::new(it.trick.clone());
        let mut at = it.resume;
        let mut up = Up {
            on: countdown && it.series.is_some(),
            ..Up::default()
        };
        let mut fresh = true;
        film::report(it, Note::Start, at, false, matches!(src, Source::File(_)), false);
        let (pos, after) = loop {
            let run = Start {
                g,
                it,
                src: &src,
                start: at,
                fresh,
            };
            let out = self.vod(run, &mut osd, &mut choice, &mut events, &mut thumbs, &mut up);
            fresh = false;
            match out {
                Vod::Seek(t) => at = t,
                Vod::Server(t, _, why) if mode == 2 => {
                    break match self.vod_fail(g, &mut osd, &forced(&why), t) {
                        Vod::Exit(p, _) => (p, After::Stopped),
                        _ => (t, After::Stopped),
                    };
                }
                Vod::Server(t, reason, why) => {
                    log(&format!("film: the server converts it from here: {why}"));
                    src = Source::Server(reason);
                    events.clear();
                    (choice.sub, at, fresh) = (choice.sub.filter(|s| s.server), t, true);
                }
                Vod::Exit(pos, true) => break (pos, After::Finished),
                Vod::Exit(pos, false) => break (pos, After::Stopped),
                Vod::Next => match up.next.take() {
                    Some(n) => break (it.total, After::PlayNext(Box::new(n))),
                    None => break (it.total, After::Finished),
                },
                Vod::Page(pos) => break (pos, After::Page),
            }
        };
        let done = matches!(after, After::Finished | After::PlayNext(_));
        film::report(
            it,
            Note::Stop,
            if done { it.total } else { pos },
            false,
            matches!(src, Source::File(_)),
            done,
        );
        (pos, after)
    }

    fn open_file(&mut self, g: &Gfx, it: &Item, osd: &mut Osd) -> Result<Option<Demux>, (&'static str, String)> {
        const CANNOT_OPEN: &str = "Direct Play could not open the file";
        let url = it.direct.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Demux::open(&url));
        });
        let t = Instant::now();
        let d = loop {
            if matches!(fkey(), Some(Fk::Back | Fk::Stop)) {
                return Ok(None);
            }
            match rx.try_recv() {
                Ok(r) => break r.map_err(|e| (CANNOT_OPEN, e))?,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => return Err((CANNOT_OPEN, "the demuxer stopped".into())),
            }
            if t.elapsed() > Duration::from_secs(45) {
                return Err((
                    "The server took too long to send the file",
                    "the server did not answer in time".into(),
                ));
            }
            let loading = Some(Loading {
                stage: "Opening the file",
                full: true,
            });
            vframe(
                g,
                osd,
                it.resume / TICKS,
                false,
                None,
                Over {
                    loading,
                    ..Over::default()
                },
            );
        };
        d.info.plan(it.default_audio).map_err(|e| {
            let reason = if d.info.video.as_ref().is_none_or(|v| v.refused.is_some()) {
                "No compatible video track"
            } else {
                "No compatible audio track"
            };
            (reason, e)
        })?;
        Ok(Some(d))
    }

    /// A failure of the file played as it is hands the film to the server; a failure of that ends the viewing.
    fn or_server(&self, g: &Gfx, osd: &mut Osd, direct: bool, why: &str, at: i64) -> Vod {
        if direct {
            Vod::Server(at, "The TV’s player could not play the file", why.to_string())
        } else {
            self.vod_fail(g, osd, why, at)
        }
    }

    /// Shows why the title does not play until the viewer leaves.
    fn vod_fail(&self, g: &Gfx, osd: &mut Osd, why: &str, at: i64) -> Vod {
        log(&format!("film: {why}"));
        osd.say(g, Some(why));
        loop {
            if matches!(fkey(), Some(Fk::Back | Fk::Stop)) {
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
                    let note = if !a.playable() { " (the server converts it)" } else { "" };
                    t.audio.push(AudioRow {
                        index: i64::from(a.stream),
                        name: format!("{}{note}", a.name),
                        lang: a.lang.clone(),
                    });
                }
                for s in &d.info.subs {
                    let i = i64::from(s.stream);
                    t.subs
                        .push(SubRow::new(i, false, &s.lang, s.forced, s.sdh, &s.title, &s.format));
                }
                for s in it.subs.iter().filter(|s| s.external) {
                    t.subs.push(SubRow::new(
                        s.index, true, &s.lang, s.forced, s.sdh, &s.title, &s.format,
                    ));
                }
                t
            }
            Source::Server(_) => Tracks {
                audio: it
                    .audio
                    .iter()
                    .map(|a| AudioRow {
                        index: a.index,
                        name: a.name.clone(),
                        lang: a.lang.clone(),
                    })
                    .collect(),
                subs: it
                    .subs
                    .iter()
                    .map(|s| SubRow::new(s.index, true, &s.lang, s.forced, s.sdh, &s.title, &s.format))
                    .collect(),
            },
        }
    }

    /// One run from `start` ticks, until Back, the end, or a seek.
    fn vod(
        &mut self,
        run_of: Start,
        osd: &mut Osd,
        choice: &mut Choice,
        events: &mut Events,
        thumbs: &mut Thumbs,
        up: &mut Up,
    ) -> Vod {
        let Start {
            g,
            it,
            src,
            start,
            fresh,
        } = run_of;
        let direct = matches!(src, Source::File(_));
        let tracks = self.tracks(it, src);
        let mut btns = vec![];
        if !tracks.subs.is_empty() {
            btns.push(Btn::Subs);
        }
        if tracks.audio.len() > 1 {
            btns.push(Btn::Audio);
        }
        if !it.chapters.is_empty() {
            btns.push(Btn::Chapters);
        }
        btns.extend([Btn::Options, Btn::Info]);
        osd.set_buttons(g, &btns);
        let mut shown = Shown::default();
        // The first run picks the tracks the viewer's language settings and the server's choice lead to.
        if !choice.init {
            choice.init = true;
            let langs = film::LANGS.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cand = |index: i64, t: Option<&Track>| film::pick::Cand {
                index,
                lang: t.map(|t| t.lang.clone()).unwrap_or_default(),
                forced: t.is_some_and(|t| t.forced),
            };
            let (audio_cands, sub_cands): (Vec<_>, Vec<_>) = match src {
                Source::File(d) => (
                    d.info
                        .audio
                        .iter()
                        .filter(|a| a.playable())
                        .map(|a| film::pick::Cand {
                            index: i64::from(a.stream),
                            lang: a.lang.clone(),
                            forced: false,
                        })
                        .collect(),
                    d.info
                        .subs
                        .iter()
                        .map(|s| film::pick::Cand {
                            index: i64::from(s.stream),
                            lang: s.lang.clone(),
                            forced: s.forced,
                        })
                        .chain(it.subs.iter().filter(|s| s.external).map(|s| cand(s.index, Some(s))))
                        .collect(),
                ),
                Source::Server(_) => (
                    it.audio.iter().map(|a| cand(a.index, Some(a))).collect(),
                    it.subs.iter().map(|s| cand(s.index, Some(s))).collect(),
                ),
            };
            let want = film::pick::audio(&langs, &audio_cands, it.default_audio);
            choice.audio = match src {
                Source::File(d) => d.info.plan(want).ok().map(i64::from),
                Source::Server(_) => want,
            };
            let audio_lang = audio_cands
                .iter()
                .find(|c| Some(c.index) == choice.audio)
                .map(|c| c.lang.clone())
                .unwrap_or_default();
            if let Some(ds) = film::pick::sub(&langs, &audio_lang, &sub_cands, it.default_sub) {
                choice.sub = tracks.subs.iter().find(|s| s.index == ds).map(|s| SubSel {
                    index: s.index,
                    server: s.server,
                });
            }
            log(&format!(
                "film: starts with audio {:?} ({audio_lang:?}) and subtitle {:?}",
                choice.audio,
                choice.sub.map(|s| s.index)
            ));
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
        // Before the pipeline loads: the receiver link is set up as it connects.
        let (lossless_mode, _armed) = match src {
            Source::File(d) => {
                let kind = d
                    .info
                    .audio
                    .iter()
                    .find(|a| Some(i64::from(a.stream)) == choice.audio)
                    .map_or(lossless::Kind::Other, |a| a.kind);
                lossless::arm(kind)
            }
            Source::Server(_) => lossless::arm(lossless::Kind::Other),
        };
        let run = match src {
            Source::File(d) => {
                let audio = choice.audio.map_or(-1, |a| a as i32);
                let sub = choice.sub.filter(|s| !s.server).map(|s| s.index as i32);
                d.start(start * 100, audio, sub, lossless_mode)
            }
            Source::Server(_) => {
                let (url, skip) = film::hls_url(it, start, choice.audio);
                hls_run(&url, skip as f64 / TICKS as f64)
            }
        };
        let t = Instant::now();
        let (video, audio, dv) = loop {
            if matches!(fkey(), Some(Fk::Back | Fk::Stop)) {
                return Vod::Exit(start, false);
            }
            match run.q.pop_timeout(Duration::from_millis(16)) {
                Some(Feed::Streams {
                    video: Some(v),
                    audio,
                    dv,
                }) => break (v, audio, dv),
                Some(Feed::End(e)) if direct => return Vod::Server(start, "The file could not be read", e),
                Some(Feed::End(e)) => return self.vod_fail(g, osd, &e, start),
                _ => {}
            }
            if t.elapsed() > Duration::from_secs(60) {
                return if direct {
                    Vod::Server(
                        start,
                        "The file gave no picture",
                        "the file gave no picture in a minute".into(),
                    )
                } else {
                    self.vod_fail(g, osd, "The server did not start the video within a minute.", start)
                };
            }
            let stage = if direct {
                "Reading the file"
            } else {
                "The server is preparing the video"
            };
            let loading = Some(Loading { stage, full: fresh });
            vframe(
                g,
                osd,
                start_s,
                false,
                None,
                Over {
                    loading,
                    ..Over::default()
                },
            );
        };
        log(&format!("film: video {video:?}, audio {audio:?}, dolby vision {dv:?}"));
        *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let window = match self.window() {
            Ok(w) => w,
            Err(e) => return self.vod_fail(g, osd, &e, start),
        };
        let smp = &self.smp.fresh();
        let profile = dv
            .as_ref()
            .filter(|d| matches!(d.profile, 5 | 8 | 10))
            .map(|d| d.profile);
        let profile = profile.filter(|_| dolby::tv_shows_dv());
        let p = cstr(&payload_dv(APP, &window.0.to_string_lossy(), video, audio, profile));
        if unsafe { (smp.load)(smp.obj, p.as_ptr(), on_event) } & 0xff == 0 {
            return self.or_server(g, osd, direct, "The TV's player refused the video.", start);
        }
        foreground();
        let t = Instant::now();
        while unsafe { (smp.loaded)(smp.obj) } & 0xff == 0 {
            let k = fkey();
            let err = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take();
            if t.elapsed() > Duration::from_secs(10) || err.is_some() || matches!(k, Some(Fk::Back | Fk::Stop)) {
                smp.retire();
                return match k {
                    Some(Fk::Back | Fk::Stop) => Vod::Exit(start, false),
                    _ => {
                        let why = err.unwrap_or("The TV's player did not start.".into());
                        self.or_server(g, osd, direct, &why, start)
                    }
                };
            }
            let loading = Some(Loading {
                stage: "Starting the TV's player",
                full: fresh,
            });
            vframe(
                g,
                osd,
                start_s,
                false,
                None,
                Over {
                    loading,
                    ..Over::default()
                },
            );
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
        // The pipeline's own buffer filled with pictures: a very high bit rate reaches that before 0.7 s of them.
        let mut video_full = false;
        let (mut paused, mut eof) = (false, false);
        let mut seek: Option<(i64, Instant)> = None;
        let mut menu: Option<Menu> = None;
        // A held Left or Right, its speed step, where the seek began, and when Info was last made.
        let (mut scrub, mut level, mut seek_from) = (None::<Scrub>, 0u8, 0i64);
        let mut taps = SeekTaps::default();
        let mut info_at = Instant::now();
        let page_name = if it.series.is_some() {
            "Go to series"
        } else {
            "Go to the film's page"
        };
        // The language the subtitle panel lists first: the one asked for in Settings, else the audio's.
        let lead = {
            let wanted = film::LANGS.lock().unwrap_or_else(|e| e.into_inner()).sub_lang.clone();
            let audio = tracks
                .audio
                .iter()
                .find(|a| Some(a.index) == choice.audio)
                .map(|a| a.lang.clone());
            if wanted.is_empty() {
                audio.unwrap_or_default()
            } else {
                wanted
            }
        };
        // The time in the file of the run's time 0: what was asked for, then the keyframe it began on.
        let mut origin = start;
        let mut ass: Option<Ass> = None;
        let mut ass_layer = AssLayer::default();
        let (mut ass_off, mut last_ass) = ((0, 0), Instant::now());
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
            let skip = extras::segment_at(&it.segments, pos).filter(|_| clock.running());
            // The next episode: asked of the server a minute before the credits, offered on a card once they begin.
            let credits = if up.on { credits_at(it) } else { None };
            if let Some(c) = credits {
                if up.asked.is_none() && up.next.is_none() && pos + 60 * TICKS >= c {
                    let (tx, rx) = channel();
                    let (b, series, id) = (it.backend.clone(), it.series.clone().unwrap_or_default(), it.id.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(film::next_after(&b, &series, &id));
                    });
                    up.asked = Some(rx);
                }
                if let Some(got) = up.asked.as_ref().and_then(|r| r.try_recv().ok()) {
                    up.asked = None;
                    match got {
                        Some(n) => {
                            log(&format!("film: up next is {} ({})", n.name, n.sub));
                            up.card = Some(UpCard::new(g, &n, true));
                            up.next = Some(n);
                        }
                        None => up.on = false,
                    }
                }
                // Back before the credits: the card comes again.
                if pos + 2 * TICKS < c {
                    up.dismissed = false;
                }
            }
            let up_left = credits
                .filter(|c| pos >= *c && up.card.is_some() && !up.dismissed)
                .map(|c| (c + UP_SECS * TICKS - pos) as f32 / TICKS as f32);
            if up_left.is_some_and(|l| l <= 0.0) && clock.running() {
                log("film: the next episode starts");
                smp.retire();
                return Vod::Next;
            }
            let top = if it.total > 0 {
                (it.total / TICKS - 5).max(0)
            } else {
                i64::MAX / 4
            };
            let had_seek = seek.is_some();
            let key = fkey();
            if !had_seek || !matches!(key, None | Some(Fk::Step(_) | Fk::Release)) {
                taps = SeekTaps::default();
                if scrub.is_none() {
                    level = 0;
                }
            }
            if menu.is_some() {
                osd.show();
            }
            let toggle = matches!(key, Some(Fk::Pause))
                || (matches!(key, Some(Fk::Toggle))
                    && menu.is_none()
                    && osd.focus.is_none()
                    && seek.is_none()
                    && up_left.is_none()
                    && skip.is_none());
            if matches!(key, Some(Fk::Stop)) {
                break Vod::Exit(pos, false);
            } else if matches!(key, Some(Fk::Back)) {
                if seek.is_some() {
                    (seek, scrub, level) = (None, None, 0);
                    osd.show();
                } else if menu.take().is_some() {
                    osd.show();
                } else if osd.controls_visible(paused) {
                    osd.hide();
                } else if up_left.is_some() {
                    up.dismissed = true;
                } else {
                    break Vod::Exit(pos, false);
                }
            } else if toggle {
                if clock.running() || paused {
                    paused = !paused;
                    log(&format!(
                        "film: {} at {} s",
                        if paused { "paused" } else { "resumed" },
                        pos / TICKS
                    ));
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
            } else if let Some(m) = menu.as_mut() {
                let opt = match m.picked() {
                    Some(Pick::Opt(o)) => Some(*o),
                    _ => None,
                };
                // OK on an option goes round its choices; Left and Right stop at the ends.
                let change = match (key.as_ref(), opt) {
                    (Some(Fk::Step(d)), Some(o)) => Some((o, *d, false)),
                    (Some(Fk::Toggle), Some(o)) => Some((o, 1, true)),
                    _ => None,
                };
                match (key, change) {
                    (_, Some((o, d, wrap))) => {
                        let l = &mut choice.look;
                        let next = |v: usize, n: usize| {
                            let v = v as i64 + d;
                            (if wrap {
                                v.rem_euclid(n as i64)
                            } else {
                                v.clamp(0, n as i64 - 1)
                            }) as usize
                        };
                        match o {
                            Opt::Size => l.sub_size = next(l.sub_size, SIZES.len()),
                            Opt::Raise => l.sub_pos = next(l.sub_pos, RAISES.len()),
                            Opt::Color => l.sub_color = next(l.sub_color, COLORS.len()),
                            Opt::Bg => l.sub_bg = !l.sub_bg,
                            Opt::Skip => l.skip = next(l.skip, SKIPS.len()),
                            Opt::Delay if wrap => choice.delay_ms = 0,
                            Opt::Delay => choice.delay_ms = (choice.delay_ms + d * 100).clamp(-30_000, 30_000),
                        }
                        if o == Opt::Size {
                            set_sub_size(SIZES[choice.look.sub_size].1);
                            shown.reset();
                        }
                        if o != Opt::Delay && std::env::var_os("TVLAUNCHER_READ_ONLY").is_none() {
                            crate::data::Prefs::save_look(&media_root(), choice.look);
                        }
                        *m = Menu::options(g, &choice.look, choice.delay_ms, m.sel);
                    }
                    (Some(Fk::Step(-1)), _) => menu = None,
                    (Some(Fk::Up), _) => m.step(-1),
                    (Some(Fk::Down), _) => m.step(1),
                    (Some(Fk::Toggle), _) => {
                        let pick = menu.take().and_then(|m| m.rows.into_iter().nth(m.sel?)).map(|r| r.pick);
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
                            Some(Pick::Chapter(t)) => {
                                smp.retire();
                                return Vod::Seek(t);
                            }
                            Some(Pick::Restart) => {
                                log("film: from the beginning");
                                smp.retire();
                                return Vod::Seek(0);
                            }
                            Some(Pick::Page) => {
                                smp.retire();
                                return Vod::Page(pos);
                            }
                            Some(Pick::Audio(i)) if choice.audio != Some(i) => {
                                choice.audio = Some(i);
                                smp.retire();
                                let can = match src {
                                    Source::File(d) => {
                                        d.info.audio.iter().any(|a| i64::from(a.stream) == i && a.playable())
                                    }
                                    Source::Server(_) => true,
                                };
                                return if can {
                                    Vod::Seek(pos)
                                } else {
                                    Vod::Server(
                                        pos,
                                        "The selected audio track is not supported",
                                        "the TV cannot decode that audio track".into(),
                                    )
                                };
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                if matches!(menu.as_ref().map(|m| m.btn), Some(Btn::Info)) && info_at.elapsed() > Duration::from_secs(1)
                {
                    info_at = Instant::now();
                    let ahead = (last[1] - clock.now().as_nanos() as i64) as f64 / 1e9;
                    let sel = menu.as_ref().and_then(|m| m.sel);
                    let lines = info_lines(src, &tracks, choice, video, profile, lossless_mode, ahead);
                    menu = Some(Menu::info(g, page_name, lines, sel));
                }
            } else if osd.focus.is_some() {
                match key {
                    Some(Fk::Step(d)) => osd.step(d),
                    Some(Fk::Up) => {
                        osd.focus = None;
                        osd.show();
                    }
                    Some(Fk::Toggle) => {
                        info_at = Instant::now();
                        let ahead = (last[1] - clock.now().as_nanos() as i64) as f64 / 1e9;
                        menu = match osd.button() {
                            Some(Btn::Subs) => Some(Menu::subs(g, &tracks, choice, &lead)),
                            Some(Btn::Audio) => Some(Menu::audio(g, &tracks, choice)),
                            Some(Btn::Chapters) => Some(Menu::chapters(g, &it.chapters, pos)),
                            Some(Btn::Options) => Some(Menu::options(g, &choice.look, choice.delay_ms, None)),
                            Some(Btn::Info) => Some(Menu::info(
                                g,
                                page_name,
                                info_lines(src, &tracks, choice, video, profile, lossless_mode, ahead),
                                None,
                            )),
                            None => None,
                        };
                    }
                    _ => {}
                }
            } else if let (Some(card), true) = (up.card.as_mut().filter(|_| up_left.is_some()), seek.is_none()) {
                match key {
                    Some(Fk::Toggle) if card.focus == 0 => {
                        smp.retire();
                        return Vod::Next;
                    }
                    Some(Fk::Toggle) => {
                        log("film: watches the credits");
                        up.dismissed = true;
                    }
                    Some(Fk::Step(d)) => card.step(d),
                    Some(Fk::Up) => osd.show(),
                    Some(Fk::Down) => osd.focus_buttons(),
                    _ => {}
                }
            } else {
                match key {
                    Some(Fk::Up) => osd.show(),
                    Some(Fk::Down) if seek.is_none() => osd.focus_buttons(),
                    Some(Fk::Chapter(d)) if !it.chapters.is_empty() => {
                        let cur = seek.map_or(pos, |s| s.0 * TICKS);
                        let now = extras::chapter_at(&it.chapters, cur);
                        // Back goes to the start of this chapter, unless it began just now.
                        let to = match (d, now) {
                            (d, Some(i)) if d < 0 && cur - it.chapters[i].start > 4 * TICKS => Some(i),
                            (d, Some(i)) if d < 0 => i.checked_sub(1),
                            (_, i) => Some(i.map_or(0, |i| i + 1)).filter(|i| *i < it.chapters.len()),
                        };
                        if let Some(c) = to.map(|i| &it.chapters[i]) {
                            seek = Some((c.start / TICKS, Instant::now()));
                            osd.show();
                        }
                    }
                    Some(Fk::Toggle) if seek.is_some() => {
                        let to = scrub.as_ref().map_or(seek.map_or(0, |s| s.0), |s| s.at as i64);
                        log(&format!("film: seek from {} s to {to} s (OK)", pos / TICKS));
                        smp.retire();
                        return Vod::Seek(to * TICKS);
                    }
                    Some(Fk::Toggle) if skip.is_some() => {
                        if let Some(s) = skip {
                            log(&format!("film: skips {:?} to {} s", s.kind, s.end / TICKS));
                            let near_end = it.total > 0 && s.end + 3 * TICKS >= it.total;
                            if near_end {
                                break Vod::Exit(it.total, true);
                            }
                            seek = Some((s.end / TICKS, Instant::now()));
                            osd.show();
                        }
                    }
                    // The key comes again while it is held: the hold is already scrubbing.
                    Some(Fk::Step(d)) if scrub.as_ref().is_some_and(|s| s.dir == d) => {}
                    Some(Fk::Step(d)) => {
                        let base = seek.map_or(pos / TICKS, |s| s.0);
                        let (step, speed) = taps.step(d, SKIPS[choice.look.skip]);
                        level = speed;
                        let to = (base + step).clamp(0, top);
                        seek = Some((to, Instant::now()));
                        scrub = Some(Scrub::new(d, to as f64));
                        osd.show();
                    }
                    Some(Fk::Release) => {
                        scrub = None;
                        if let Some(s) = seek.as_mut() {
                            s.1 = Instant::now();
                        }
                    }
                    Some(Fk::Seek(d)) => {
                        let base = seek.map_or(pos / TICKS, |s| s.0);
                        seek = Some(((base + d).clamp(0, top), Instant::now()));
                        osd.show();
                    }
                    _ => {}
                }
            }
            if let Some(s) = scrub.as_mut() {
                match s.advance(top) {
                    Some(l) => {
                        if l > 0 {
                            level = l;
                            taps = SeekTaps::default();
                        }
                        seek = Some((s.at as i64, Instant::now()));
                    }
                    None => (scrub, level) = (None, 0),
                }
            }
            if seek.is_some() && !had_seek {
                seek_from = pos / TICKS;
            }
            if let Some((to, _)) = seek.filter(|s| scrub.is_none() && s.1.elapsed() > Duration::from_millis(600)) {
                log(&format!("film: seek from {} s to {to} s", pos / TICKS));
                smp.retire();
                return Vod::Seek(to * TICKS);
            }
            if let Some(e) = ERROR.lock().unwrap_or_else(|e| e.into_inner()).take() {
                smp.retire();
                return self.or_server(g, osd, direct, &e, pos);
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
                        Some(Feed::AssTrack { header, fonts, video }) => {
                            let (frame, off) = ass_frame(video);
                            ass_off = off;
                            ass = choice
                                .sub
                                .filter(|s| !s.server)
                                .and_then(|_| Ass::new(&header, &fonts.0, ass_default_font().as_deref(), frame, video));
                            ass_layer.clear();
                            continue;
                        }
                        Some(Feed::AssChunk { data, start_ms, dur_ms }) => {
                            if let Some(a) = ass.as_mut() {
                                a.chunk(&data, start_ms, dur_ms);
                            }
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
                            return Vod::Server(pos, "The file could not be read", e);
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
                        video_full |= au.video;
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
            let enough = last[1] >= 700_000_000 && (audio.is_none() || last[0] >= 300_000_000);
            if !clock.running() && !paused && (enough || video_full) {
                clock.run();
                let ok = unsafe { (smp.play)(smp.obj) } & 0xff;
                log(&format!("film: Play {ok}"));
                osd.say(g, None);
                film::report(it, Note::Progress, pos, false, direct, false);
            }
            // LG made no write to its ES renderer: its DD+ route is not in pass-through on this output (TV speakers, or
            // a receiver without DD+). Start again without the carrier.
            if clock.running() && !paused && lossless::silent_route((clock.now().as_millis() as i64 * 24 / 1000) as i32)
            {
                log("film: the receiver link did not take the carrier; starting again without it");
                smp.retire();
                return Vod::Seek(pos);
            }
            let size = SIZE.load(Ordering::SeqCst);
            if size != placed {
                placed = place(size);
            }
            if eof && pending.is_none() && clock.now().as_nanos() as i64 + 300_000_000 >= last[1] {
                break if up_left.is_some() && !up.dismissed {
                    Vod::Next
                } else {
                    Vod::Exit(it.total, true)
                };
            }
            let ms = pos / 10_000 - choice.delay_ms;
            if choice.sub.is_some_and(|s| !s.server) {
                events.prune(ms);
            }
            if choice.sub.is_some() {
                shown.update(g, events, ms);
            }
            if let Some(a) = ass.as_mut() {
                // A script's own timing: look at most every 33 ms.
                if last_ass.elapsed() >= Duration::from_millis(33) {
                    last_ass = Instant::now();
                    if let Some(pics) = a.render(ms) {
                        if !pics.is_empty() {
                            log(&format!("film: ass {} pictures at {ms} ms", pics.len()));
                        }
                        ass_layer.set(g, &pics, ass_off.0, ass_off.1);
                    }
                }
            }
            let thumb = seek.and_then(|s| thumbs.at(g, s.0 * 1000));
            let over = Over {
                shown: choice.sub.map(|_| &shown),
                menu: menu.as_ref(),
                skip: skip.map(|s| s.kind).filter(|_| seek.is_none()),
                thumb,
                ass: ass.as_ref().map(|_| &ass_layer),
                look: choice.look,
                loading: (!clock.running() && !paused).then_some(Loading {
                    stage: "Buffering",
                    full: fresh,
                }),
                up: match (up.card.as_mut(), up_left, seek) {
                    (Some(c), Some(l), None) if !up.dismissed => Some((c, l, UP_SECS as f32)),
                    _ => None,
                },
            };
            let seeking = seek.map(|s| Seeking {
                to: s.0,
                from: seek_from,
                level,
            });
            vframe(g, osd, pos / TICKS, paused, seeking, over);
            if !clock.running() && !paused && t.elapsed() > Duration::from_secs(40) {
                smp.retire();
                return if direct {
                    Vod::Server(pos, "The file gave no picture", "the file gave no pictures".into())
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
    child_in("", spec)
}

/// The same under `root`: a desktop run (`--film-sim`) with the fake player library of `tools/fake-starfish.c`.
pub fn child_in(root: &str, spec: &str) -> ExitCode {
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as usize) };
    let profile_root = std::env::var("TVLAUNCHER_PROFILE_ROOT").unwrap_or_else(|_| root.to_string());
    let it = match film::resolve(&profile_root, spec) {
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
    let Some((_win, ren)) = crate::start(root, false) else {
        return ExitCode::from(1);
    };
    let g = Gfx::new(root, ren, |name, size| crate::font(root, name, size));
    set_font_root(root);
    let prefs = crate::data::Prefs::load(&profile_root);
    lossless::configure(prefs.lossless);
    film::DIRECT.store(
        match prefs.direct.as_str() {
            "off" => 1,
            "force" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
    film::MAX_KBPS.store(prefs.max_mbps * 1000, Ordering::Relaxed);
    *film::LANGS.lock().unwrap_or_else(|e| e.into_inner()) = film::pick::Langs {
        audio: prefs.audio_lang,
        sub_mode: prefs.sub_mode,
        sub_lang: prefs.sub_lang,
    };
    let mut p = match Player::new() {
        Ok(p) => p,
        Err(e) => {
            log(&format!("film: {e}"));
            return ExitCode::from(1);
        }
    };
    let mut it = it;
    loop {
        let (pos, after) = p.film(&g, &it, prefs.next_ep == "countdown");
        log(&format!(
            "film: stopped at {} s{}",
            pos / TICKS,
            if matches!(after, After::Stopped | After::Page) {
                ""
            } else {
                " (finished)"
            }
        ));
        match after {
            After::Stopped => break,
            After::Page => {
                let page = it.series.as_deref().unwrap_or(&it.id);
                let _ = std::fs::write(PAGE, page);
                break;
            }
            After::PlayNext(n) => it = *n,
            After::Finished if prefs.next_ep == "off" => break,
            After::Finished => match film::next(&it).filter(|n| p.next_up(&g, n)) {
                Some(n) => it = n,
                None => break,
            },
        }
    }
    ExitCode::SUCCESS
}

impl Player {
    /// After an episode: the next one starts in five seconds, or at once with OK; Back ends the viewing. Gives
    /// whether to play it.
    fn next_up(&self, g: &Gfx, next: &Item) -> bool {
        let mut card = UpCard::new(g, next, false);
        let t = Instant::now();
        loop {
            match fkey() {
                Some(Fk::Back | Fk::Stop) => return false,
                Some(Fk::Toggle) => return true,
                _ => {}
            }
            let left = 5.0 - t.elapsed().as_secs_f32();
            if left <= 0.0 {
                return true;
            }
            up_frame(g, &mut card, left, 5.0);
        }
    }
}

/// `FILM_KEYS="3:enter,6:right,9:down"` on a desktop run: presses these keys that many seconds in (Back is `esc`,
/// `rightup` lets go of Right and `play` tests the dedicated Play/Pause key).
pub fn film_sim_keys() {
    let script = std::env::var("FILM_KEYS").unwrap_or_default();
    let keys: Vec<(u64, i32)> = script
        .split(',')
        .filter_map(|k| {
            let (t, name) = k.split_once(':')?;
            let sym = match name {
                "enter" => K_RETURN,
                "play" => AUDIO_PLAY,
                "left" => K_LEFT,
                "right" => K_RIGHT,
                "up" => K_UP,
                "down" => K_DOWN,
                "esc" => K_ESCAPE,
                "leftup" => -K_LEFT,
                "rightup" => -K_RIGHT,
                "pgdn" => K_PAGEDOWN,
                "pgup" => K_PAGEUP,
                _ => return None,
            };
            Some((t.parse().ok()?, sym))
        })
        .collect();
    std::thread::spawn(move || {
        let t = Instant::now();
        for (at, sym) in keys {
            std::thread::sleep(Duration::from_secs(at).saturating_sub(t.elapsed()));
            let mut e = Event::new(if sym < 0 { KEYUP } else { KEYDOWN });
            e.0[20..24].copy_from_slice(&sym.abs().to_ne_bytes());
            unsafe { SDL_PushEvent(&mut e) };
            log(&format!("sim: key {sym} at {at} s"));
        }
    });
}

/// Where the next episode's card comes up: the start of the credits the server marked in the second half, else 20
/// seconds before the end of an episode longer than two minutes.
fn credits_at(it: &Item) -> Option<i64> {
    let marked = it
        .segments
        .iter()
        .find(|s| s.kind == extras::SegKind::Credits && s.start > it.total / 2)
        .map(|s| s.start);
    marked.or((it.total > 120 * TICKS).then(|| it.total - 20 * TICKS))
}

/// What Info says about what plays: how it comes, the picture, the sound and its way to the speakers, the subtitles
/// and how far ahead the TV has it.
fn info_lines(
    src: &Source,
    tracks: &Tracks,
    choice: &Choice,
    video: crate::ts::Codec,
    dv_profile: Option<u8>,
    mode: lossless::Mode,
    ahead_s: f64,
) -> Vec<(String, String)> {
    let size = SIZE.load(Ordering::SeqCst);
    let hdr = HDR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let range = match (dv_profile, hdr.as_str()) {
        (Some(p), _) => format!("   ·   Dolby Vision profile {p}"),
        (None, "" | "none") => String::new(),
        (None, h) => format!("   ·   {h}"),
    };
    let codec = match video {
        crate::ts::Codec::H265 => "H.265",
        crate::ts::Codec::Av1 => "AV1",
        crate::ts::Codec::Vp9 => "VP9",
        _ => "H.264",
    };
    let audio_name = tracks
        .audio
        .iter()
        .find(|a| Some(a.index) == choice.audio)
        .or(tracks.audio.first())
        .map_or("None".to_string(), |a| a.name.clone());
    let route = match (src, mode) {
        (Source::Server(_), _) => "as the server sends it",
        (_, lossless::Mode::TrueHd) => "unconverted to the receiver (TrueHD as MAT)",
        (_, lossless::Mode::Dts) => "unconverted to the receiver (DTS)",
        (_, lossless::Mode::DtsCore) => "the DTS core, unconverted to the receiver",
        (Source::File(d), _) => match d.info.audio.iter().find(|a| Some(i64::from(a.stream)) == choice.audio) {
            Some(a) if a.how == crate::ff::demux::How::Convert => "converted to Dolby Digital on the TV",
            _ => "as it is",
        },
    };
    let sub = choice
        .sub
        .and_then(|s| tracks.subs.iter().find(|t| t.index == s.index))
        .map_or("Off".to_string(), SubRow::describe);
    let timing = match choice.delay_ms {
        0 => String::new(),
        d => format!("   ·   {:+.1} s", d as f64 / 1000.0),
    };
    let (delivery, reason) = match src {
        Source::File(_) => ("the file as it is (Direct Play)", None),
        Source::Server(reason) => ("converted by the server", Some(*reason)),
    };
    let mut lines = vec![("Playing".into(), delivery.into())];
    if let Some(reason) = reason {
        lines.push(("Why".into(), reason.into()));
    }
    lines.extend([
        (
            "Picture".into(),
            format!("{codec}   ·   {}x{}{range}", size >> 16, size & 0xffff),
        ),
        ("Sound".into(), audio_name),
        ("Sent".into(), route.into()),
        ("Subtitles".into(), format!("{sub}{timing}")),
        ("Buffered".into(), format!("{:.1} s ahead", ahead_s.max(0.0))),
    ]);
    lines
}
