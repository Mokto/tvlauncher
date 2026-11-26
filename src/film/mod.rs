//! Titles for the launcher's own player (`player::film`): what to play and where it comes from. A title is a Jellyfin
//! or a Plex item (`jellyfin`, `plex`); both give the address of the file itself, which the player reads in place
//! (`ff`), and of the server's remuxed HLS stream, which stands in when the file cannot be played as it is.
use serde_json::Value;
use std::time::{Duration, Instant};

pub mod extras;
pub mod jellyfin;
pub mod pick;
pub mod plex;

pub const TICKS: i64 = 10_000_000;
/// The exit status of the player process that tells Home to open the title in the media app instead.
pub const ELSEWHERE: u8 = 3;

/// An audio or a subtitle track, as the server names it (for example "French - AC3 5.1 - Default"). `index` is
/// the stream number in the file, which is the number FFmpeg counts too.
#[derive(Clone, Debug)]
pub struct Track {
    pub index: i64,
    pub name: String,
    /// A subtitle file beside the video, which only the server can hand over.
    pub external: bool,
    /// The language code the server gives, and whether the track holds only the lines of foreign speech.
    pub lang: String,
    pub forced: bool,
    /// Plex: where the server serves the track (`/library/streams/<id>`).
    pub key: String,
    /// Subtitles for the deaf and hard of hearing; the track's own title, and its format ("SRT").
    pub sdh: bool,
    pub title: String,
    pub format: String,
}

impl Track {
    pub fn new(index: i64, name: impl Into<String>) -> Track {
        Track {
            index,
            name: name.into(),
            external: false,
            lang: String::new(),
            forced: false,
            key: String::new(),
            sdh: false,
            title: String::new(),
            format: String::new(),
        }
    }
}

/// A subtitle codec as the menu shows it, from FFmpeg's, Jellyfin's or Plex's name for it.
pub fn sub_format(codec: &str) -> String {
    match codec.to_lowercase().as_str() {
        "subrip" | "srt" => "SRT",
        "ass" | "ssa" => "ASS",
        "hdmv_pgs_subtitle" | "pgssub" | "pgs" => "PGS",
        "dvd_subtitle" | "dvdsub" | "vobsub" => "VobSub",
        "dvb_subtitle" | "dvbsub" => "DVB",
        "webvtt" | "vtt" => "WebVTT",
        "" => "",
        _ => "Text",
    }
    .into()
}

#[derive(Clone)]
pub enum Backend {
    Jellyfin(crate::jellyfin::Config),
    Plex(plex::Conn),
}

/// The viewer's language settings, read when the player starts (`vod::child_in`).
pub static LANGS: std::sync::Mutex<pick::Langs> = std::sync::Mutex::new(pick::Langs {
    audio: String::new(),
    sub_mode: String::new(),
    sub_lang: String::new(),
});

/// How a film is delivered: 0 as it is when it can be and by the server otherwise, 1 always by the server, 2 only as it
/// is (no fallback).
pub static DIRECT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// The most the server's conversion may send, in kilobits per second; 0 for no limit.
pub static MAX_KBPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn direct_mode() -> u8 {
    DIRECT.load(std::sync::atomic::Ordering::Relaxed)
}

/// What is played: a film or one episode.
pub struct Item {
    pub backend: Backend,
    pub audio: Vec<Track>,
    /// Text subtitles only: the server can hand those over as SRT; picture subtitles (PGS, VobSub) only the file
    /// itself carries.
    pub subs: Vec<Track>,
    /// The tracks the server would start with.
    pub default_audio: Option<i64>,
    pub default_sub: Option<i64>,
    pub chapters: Vec<extras::Chapter>,
    /// What the viewer may skip: an intro, a recap, the credits.
    pub segments: Vec<extras::Segment>,
    /// The pictures shown while seeking, when the server made them.
    pub trick: Option<extras::Trick>,
    pub id: String,
    /// The series an episode belongs to, to find the next one.
    pub series: Option<String>,
    /// Jellyfin: the media source. Plex: the part.
    pub source: String,
    /// Where the file is, ready to be read with byte ranges.
    pub direct: String,
    pub name: String,
    pub sub: String,
    /// Length and the place to resume from, in 100 ns ticks.
    pub total: i64,
    pub resume: i64,
    pub session: String,
}

pub enum Why {
    /// A collection, a library: only the media app shows those.
    Elsewhere,
    Failed(String),
}

/// The item to play for what Home passes to the player process: a Jellyfin item id, or `plex:<server>:<rating key>`.
pub fn resolve(root: &str, spec: &str) -> Result<Item, Why> {
    // `<spec>@<seconds>` starts there (a test hook for the spawn file). Playing reports progress, so the server's resume point moves.
    if let Some((spec, at)) = spec.rsplit_once('@') {
        let at: i64 = at.parse().map_err(|_| Why::Failed("Bad start time.".into()))?;
        return resolve(root, spec).map(|it| Item {
            resume: at * TICKS,
            ..it
        });
    }
    match spec.strip_prefix("plex:").and_then(|r| r.split_once(':')) {
        Some((server, key)) => {
            let cfg = crate::plex::Config::load(root).ok_or_else(|| Why::Failed("Not signed in to Plex.".into()))?;
            plex::resolve(root, &cfg, server, key)
        }
        None => {
            let cfg =
                crate::jellyfin::Config::load(root).ok_or_else(|| Why::Failed("Not signed in to Jellyfin.".into()))?;
            jellyfin::resolve(&cfg, spec)
        }
    }
}

/// The episode that follows `it` in its series, if it is an episode and not the last.
pub fn next(it: &Item) -> Option<Item> {
    next_after(&it.backend, it.series.as_deref()?, &it.id)
}

/// The episode after `id` of `series`.
pub fn next_after(backend: &Backend, series: &str, id: &str) -> Option<Item> {
    match backend {
        Backend::Jellyfin(c) => jellyfin::next(c, series, id),
        Backend::Plex(c) => plex::next(c, series, id),
    }
}

/// The playlist of the server's remux of the film, and how many ticks of it to skip: Jellyfin's playlist always starts
/// at the beginning, Plex's starts at `start`.
pub fn hls_url(it: &Item, start: i64, audio: Option<i64>) -> (String, i64) {
    match &it.backend {
        Backend::Jellyfin(c) => (jellyfin::master_url(c, it, audio), start),
        Backend::Plex(c) => (plex::hls_url(c, it, start, audio), 0),
    }
}

/// A subtitle track as cues, from the server.
pub fn subtitles(backend: &Backend, id: &str, source: &str, t: &Track) -> Result<Vec<crate::subs::Cue>, String> {
    match backend {
        Backend::Jellyfin(c) => jellyfin::subtitles(c, id, source, t.index),
        Backend::Plex(c) => plex::subtitles(c, t),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Event {
    Start,
    Progress,
    Stop,
}

/// Tells the server where the viewer is (the resume point, the watched mark and the dashboard's "now playing").
/// The last one is waited for so the process does not exit first; the others go out on a thread. `direct` says how
/// the title is played, `done` that it played to the end.
pub fn report(it: &Item, event: Event, ticks: i64, paused: bool, direct: bool, done: bool) {
    let (item, backend) = (it.summary(), it.backend.clone());
    let wait = event == Event::Stop;
    let send = move || {
        let r = match &backend {
            Backend::Jellyfin(c) => jellyfin::report(c, &item, event, ticks, paused, direct),
            Backend::Plex(c) => plex::report(c, &item, event, ticks, paused, done),
        };
        if let Err(e) = r {
            crate::sdl::log(&format!("film: report: {e}"));
        }
    };
    if wait {
        send();
    } else {
        std::thread::spawn(send);
    }
}

/// What a report needs of an item, copied so it can go to a thread.
#[derive(Clone)]
pub struct Summary {
    pub id: String,
    pub source: String,
    pub session: String,
    pub total: i64,
}

impl Item {
    pub fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            source: self.source.clone(),
            session: self.session.clone(),
            total: self.total,
        }
    }
}

/// A value that tells one play session from another.
pub fn session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}")
}

/// Resume only between 30 s in and 95 %: what is nearly finished starts again.
pub fn resume_point(at: i64, total: i64) -> i64 {
    if at > 30 * TICKS && (total == 0 || at < total / 100 * 95) {
        at
    } else {
        0
    }
}

pub fn text(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

/// The playhead: how far the pipeline has been let run, which stops while paused.
#[derive(Default)]
pub struct Clock {
    before: Duration,
    since: Option<Instant>,
}

impl Clock {
    pub fn run(&mut self) {
        self.since.get_or_insert_with(Instant::now);
    }

    pub fn hold(&mut self) {
        if let Some(t) = self.since.take() {
            self.before += t.elapsed();
        }
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    pub fn now(&self) -> Duration {
        self.before + self.since.map_or(Duration::ZERO, |t| t.elapsed())
    }
}

/// `1:05:09` or `5:09`.
pub fn clock(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_stops_while_held() {
        let mut c = Clock::default();
        assert_eq!(c.now(), Duration::ZERO);
        c.run();
        std::thread::sleep(Duration::from_millis(30));
        c.hold();
        let held = c.now();
        assert!(held >= Duration::from_millis(30) && !c.running());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(c.now(), held);
        c.run();
        assert!(c.running());
    }

    #[test]
    fn times_read_as_clocks() {
        assert_eq!(
            (clock(309).as_str(), clock(3909).as_str(), clock(-4).as_str()),
            ("5:09", "1:05:09", "0:00")
        );
    }

    #[test]
    fn resume_only_in_the_middle() {
        assert_eq!(resume_point(1200 * TICKS, 6000 * TICKS), 1200 * TICKS);
        assert_eq!(resume_point(10 * TICKS, 6000 * TICKS), 0, "too early to resume");
        assert_eq!(
            resume_point(5900 * TICKS, 6000 * TICKS),
            0,
            "nearly finished starts again"
        );
    }
}
