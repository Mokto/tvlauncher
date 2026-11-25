//! A title's page, opened by OK in a library tab when films play in the launcher: what it is (year, length, picture
//! and sound, cast), Play or Resume, From the start and Mark as watched; for a series, its seasons and their episodes.
//!
//! The page is made from the library's own title at once, and filled in when the server answers (a thread, see
//! [`load`]). It never wakes a media server by itself: it asks only when the viewer opened it.
use crate::data::{Item, Kind, DIR};
use crate::gfx::*;
use crate::jellyfin::clip;
use crate::library::{Source, Title};
use crate::sdl::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};

/// Where the title lives, and how to reach it.
#[derive(Clone)]
pub enum Backend {
    Jellyfin(crate::jellyfin::Config),
    /// The server's `machineIdentifier`, and the address and token its library tab reached it at.
    Plex(crate::plex::Conn, String),
}

impl Backend {
    /// What the film player takes (`film::resolve`): a Jellyfin item id or `plex:<server>:<rating key>`, and
    /// `@0` to start at the beginning instead of the resume point.
    fn spec(&self, id: &str, from_start: bool) -> String {
        let at = if from_start { "@0" } else { "" };
        match self {
            Backend::Jellyfin(_) => format!("{id}{at}"),
            Backend::Plex(_, server) => format!("plex:{server}:{id}{at}"),
        }
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Info {
    pub series: bool,
    /// The picture and the sound: "4K   ·   Dolby Vision   ·   Dolby TrueHD Atmos 7.1   ·   Subtitles: 9".
    pub tech: String,
    /// "Directed by …   ·   With …".
    pub people: String,
    pub resume_s: i64,
    pub played: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ep {
    pub id: String,
    pub season: i64,
    pub season_name: String,
    pub no: i64,
    pub name: String,
    pub overview: String,
    /// Length and air date, ready to show.
    pub meta: String,
    pub played: bool,
    pub resume_s: i64,
    pub progress: f32,
    /// The still's Jellyfin image tag or Plex path.
    pub still: Option<String>,
}

pub struct Season {
    pub name: String,
    pub eps: Vec<Ep>,
}

pub enum Got {
    /// The title, its seasons (none for a film) and the episode to watch next (season, episode).
    Loaded(Info, Vec<Season>, (usize, usize)),
    /// Not a film or a series (a collection): the media app shows it.
    Elsewhere,
    Failed(String),
}

/// What the viewer asked for.
#[derive(Clone)]
pub enum Act {
    /// Play this spec (see [`Backend::spec`]).
    Play(String),
    /// Mark this id (a film, an episode or a whole series) watched (true) or not.
    Mark(String, bool),
}

fn json(b: Vec<u8>) -> Result<Value, String> {
    serde_json::from_slice(&b).map_err(|e| e.to_string())
}

/// Fetches the title and, for a series, all its episodes, on a thread; wakes the main loop when done.
pub fn load(b: Backend, id: String, wake: u32) -> Receiver<Got> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let got = match &b {
            Backend::Jellyfin(c) => jf_load(c, &id),
            Backend::Plex(c, _) => plex_load(c, &id),
        };
        log(&format!("detail: {id} in {} ms", t.elapsed().as_millis()));
        let _ = tx.send(got.unwrap_or_else(Got::Failed));
        unsafe { SDL_PushEvent(&mut Event::new(wake)) };
    });
    rx
}

/// Marks `id` watched or not on the server, on a thread. The page shows the change at once.
pub fn mark(b: Backend, id: String, played: bool) {
    std::thread::spawn(move || {
        let r = match &b {
            Backend::Jellyfin(c) => {
                let path = format!("/Users/{}/PlayedItems/{id}", c.user_id);
                if played {
                    c.post(&path)
                } else {
                    c.delete(&path)
                }
            }
            Backend::Plex(conn, _) => {
                let what = if played { "scrobble" } else { "unscrobble" };
                conn.get_bytes(
                    &format!("/:/{what}?key={id}&identifier=com.plexapp.plugins.library"),
                    10,
                )
                .map(drop)
            }
        };
        if let Err(e) = r {
            log(&format!("detail: marking {id}: {e}"));
        }
    });
}

fn jf_load(c: &crate::jellyfin::Config, id: &str) -> Result<Got, String> {
    let o = json(c.get_within(&format!("/Users/{}/Items/{id}", c.user_id), 15)?)?;
    let series = match o["Type"].as_str().unwrap_or_default() {
        "Series" => true,
        "Movie" | "Episode" | "Video" | "MusicVideo" => false,
        _ => return Ok(Got::Elsewhere),
    };
    let info = jf_info(&o);
    if !series {
        return Ok(Got::Loaded(info, vec![], (0, 0)));
    }
    let v = json(c.get_within(
        &format!(
            "/Shows/{id}/Episodes?userId={}&Fields=Overview&EnableImageTypes=Primary&ImageTypeLimit=1",
            c.user_id
        ),
        20,
    )?)?;
    let eps: Vec<Ep> = v["Items"].as_array().into_iter().flatten().filter_map(jf_ep).collect();
    let (seasons, next) = group(eps);
    Ok(Got::Loaded(info, seasons, next))
}

fn plex_load(conn: &crate::plex::Conn, id: &str) -> Result<Got, String> {
    let v = conn.get_json(&format!("/library/metadata/{id}"), 15)?;
    let o = &v["MediaContainer"]["Metadata"][0];
    let series = match o["type"].as_str().unwrap_or_default() {
        "show" => true,
        "movie" | "episode" | "clip" => false,
        _ => return Ok(Got::Elsewhere),
    };
    let info = plex_info(o);
    if !series {
        return Ok(Got::Loaded(info, vec![], (0, 0)));
    }
    let v = conn.get_json(
        &format!("/library/metadata/{id}/allLeaves?X-Plex-Container-Start=0&X-Plex-Container-Size=2000"),
        20,
    )?;
    let eps: Vec<Ep> = v["MediaContainer"]["Metadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(plex_ep)
        .collect();
    let (seasons, next) = group(eps);
    Ok(Got::Loaded(info, seasons, next))
}

/// The resolution, the dynamic range and the sound, as a viewer names them.
fn tech(width: i64, range: &str, audio: Option<(&str, &str, i64, bool)>, subs: usize) -> String {
    let res = match width {
        w if w >= 3200 => "4K",
        w if w >= 1800 => "1080p",
        w if w >= 1200 => "720p",
        0 => "",
        _ => "SD",
    };
    let sound = audio.map(|(codec, profile, ch, atmos)| {
        let p = profile.to_ascii_lowercase();
        let name = match codec.to_ascii_lowercase().as_str() {
            "eac3" => "Dolby Digital+",
            "ac3" => "Dolby Digital",
            "truehd" => "Dolby TrueHD",
            "dts" | "dca" if p.contains("x") => "DTS:X",
            "dts" | "dca" if p.contains("ma") => "DTS-HD MA",
            "dts" | "dca" => "DTS",
            "aac" => "AAC",
            "flac" => "FLAC",
            "opus" => "Opus",
            "mp3" => "MP3",
            other => return other.to_uppercase(),
        };
        let ch = match ch {
            8 => " 7.1",
            6 => " 5.1",
            2 => " Stereo",
            1 => " Mono",
            _ => "",
        };
        format!("{name}{}{ch}", if atmos { " Atmos" } else { "" })
    });
    let subs = (subs > 0).then(|| format!("Subtitles: {subs}"));
    [Some(res.to_string()), Some(range.to_string()), sound, subs]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("   ·   ")
}

fn people(directors: &[&str], cast: &[&str]) -> String {
    let d = (!directors.is_empty()).then(|| format!("Directed by {}", directors.join(", ")));
    let c = (!cast.is_empty()).then(|| format!("With {}", cast.join(", ")));
    [d, c].into_iter().flatten().collect::<Vec<_>>().join("   ·   ")
}

fn jf_info(o: &Value) -> Info {
    let streams = o["MediaStreams"].as_array().cloned().unwrap_or_default();
    let of = |t: &'static str| streams.iter().filter(move |s| s["Type"] == t);
    let video = of("Video").next();
    let range = match video.and_then(|v| v["VideoRangeType"].as_str()).unwrap_or_default() {
        r if r.starts_with("DOVI") => "Dolby Vision",
        "HDR10Plus" => "HDR10+",
        "HDR10" => "HDR10",
        "HLG" => "HLG",
        _ => "",
    };
    let audio = of("Audio")
        .find(|s| s["IsDefault"] == true)
        .or_else(|| of("Audio").next());
    let audio = audio.map(|a| {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        let atmos = s("Profile").contains("Atmos") || s("DisplayTitle").contains("Atmos");
        (s("Codec"), s("Profile"), a["Channels"].as_i64().unwrap_or(0), atmos)
    });
    let langs = of("Subtitle").count();
    let names = |t: &str, n: usize| -> Vec<&str> {
        o["People"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["Type"] == t)
            .filter_map(|p| p["Name"].as_str())
            .take(n)
            .collect()
    };
    let ud = &o["UserData"];
    Info {
        series: o["Type"] == "Series",
        tech: tech(
            video.and_then(|v| v["Width"].as_i64()).unwrap_or(0),
            range,
            audio,
            langs,
        ),
        people: people(&names("Director", 2), &names("Actor", 4)),
        resume_s: ud["PlaybackPositionTicks"].as_i64().unwrap_or(0) / 10_000_000,
        played: ud["Played"] == true,
    }
}

fn plex_info(o: &Value) -> Info {
    let streams: Vec<Value> = o["Media"][0]["Part"][0]["Stream"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let of = |t: i64| streams.iter().filter(move |s| s["streamType"].as_i64() == Some(t));
    let video = of(1).next();
    let range = match video {
        Some(v) if v["DOVIPresent"] == true => "Dolby Vision",
        Some(v) if v["colorTrc"] == "smpte2084" => "HDR10",
        Some(v) if v["colorTrc"] == "arib-std-b67" => "HLG",
        _ => "",
    };
    let audio = of(2).find(|s| s["selected"] == true).or_else(|| of(2).next());
    let audio = audio.map(|a| {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        let atmos = s("extendedDisplayTitle").contains("Atmos") || s("displayTitle").contains("Atmos");
        (s("codec"), s("profile"), a["channels"].as_i64().unwrap_or(0), atmos)
    });
    let width = video
        .and_then(|v| v["width"].as_i64())
        .or_else(|| o["Media"][0]["width"].as_i64())
        .unwrap_or(0);
    let tags = |k: &str, n: usize| -> Vec<&str> {
        o[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["tag"].as_str())
            .take(n)
            .collect()
    };
    Info {
        series: o["type"] == "show",
        tech: tech(width, range, audio, of(3).count()),
        people: people(&tags("Director", 2), &tags("Role", 4)),
        resume_s: o["viewOffset"].as_i64().unwrap_or(0) / 1000,
        played: o["viewCount"].as_i64().unwrap_or(0) > 0,
    }
}

fn ep_meta(minutes: i64, date: &str) -> String {
    let len = (minutes > 0).then(|| format!("{minutes} min"));
    let date = date.get(..10).filter(|d| d.len() == 10).map(String::from);
    [len, date].into_iter().flatten().collect::<Vec<_>>().join("   ·   ")
}

fn jf_ep(o: &Value) -> Option<Ep> {
    let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
    let ud = &o["UserData"];
    let played = ud["Played"] == true;
    Some(Ep {
        id: o["Id"].as_str()?.into(),
        season: o["ParentIndexNumber"].as_i64().unwrap_or(1),
        season_name: s("SeasonName"),
        no: o["IndexNumber"].as_i64().unwrap_or(0),
        name: s("Name"),
        overview: s("Overview"),
        meta: ep_meta(
            o["RunTimeTicks"].as_i64().unwrap_or(0) / 600_000_000,
            &s("PremiereDate"),
        ),
        played,
        resume_s: ud["PlaybackPositionTicks"].as_i64().unwrap_or(0) / 10_000_000,
        progress: if played {
            0.0
        } else {
            ud["PlayedPercentage"].as_f64().unwrap_or(0.0) as f32 / 100.0
        },
        still: o["ImageTags"]["Primary"].as_str().map(String::from),
    })
}

fn plex_ep(o: &Value) -> Option<Ep> {
    let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
    let num = |k: &str| o[k].as_i64().unwrap_or(0);
    let played = num("viewCount") > 0;
    let (offset, duration) = (num("viewOffset"), num("duration"));
    Some(Ep {
        id: o["ratingKey"].as_str()?.into(),
        season: o["parentIndex"].as_i64().unwrap_or(1),
        season_name: s("parentTitle"),
        no: num("index"),
        name: s("title"),
        overview: s("summary"),
        meta: ep_meta(duration / 60_000, &s("originallyAvailableAt")),
        played,
        resume_s: offset / 1000,
        progress: if played || duration <= 0 {
            0.0
        } else {
            (offset as f32 / duration as f32).clamp(0.0, 1.0)
        },
        still: o["thumb"].as_str().map(String::from),
    })
}

/// Episodes by season, in order with the specials (season 0) last, and the one to watch next.
fn group(mut eps: Vec<Ep>) -> (Vec<Season>, (usize, usize)) {
    eps.sort_by_key(|e| (e.season == 0, e.season, e.no));
    let mut seasons: Vec<Season> = vec![];
    for e in eps {
        match seasons.last_mut() {
            Some(s) if s.eps[0].season == e.season => s.eps.push(e),
            _ => seasons.push(Season {
                name: if e.season_name.is_empty() {
                    if e.season == 0 {
                        "Specials".into()
                    } else {
                        format!("Season {}", e.season)
                    }
                } else {
                    e.season_name.clone()
                },
                eps: vec![e],
            }),
        }
    }
    let flat: Vec<(usize, usize, &Ep)> = seasons
        .iter()
        .enumerate()
        .filter(|(_, s)| s.eps[0].season != 0)
        .flat_map(|(si, s)| s.eps.iter().enumerate().map(move |(ei, e)| (si, ei, e)))
        .collect();
    let last_seen = flat.iter().rposition(|(_, _, e)| e.played);
    let next = flat
        .iter()
        .find(|(_, _, e)| e.resume_s > 0 && !e.played)
        .or_else(|| {
            flat.iter()
                .skip(last_seen.map_or(0, |i| i + 1))
                .find(|(_, _, e)| !e.played)
        })
        .or_else(|| flat.iter().find(|(_, _, e)| !e.played))
        .or(flat.first())
        .map_or((0, 0), |(s, e, _)| (*s, *e));
    (seasons, next)
}

// ---- the page ------------------------------------------------------------------------------------------------------

const Y_TITLE: f32 = 140.0;
const Y_BUTTONS: f32 = 560.0;
const BTN_H: f32 = 72.0;
const Y_SEASONS: f32 = 668.0;
const SEASON_H: f32 = 56.0;
const Y_EPS: f32 = 756.0;
const EP: (f32, f32) = (384.0, 216.0);
const STILL_W: u32 = 480;

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Buttons,
    Seasons,
    Eps,
}

/// The texts of the hero: made again when what they show changes.
struct Hero {
    key: String,
    title: Option<Tex>,
    line: Option<Tex>,
    meta: Option<Tex>,
    tech: Option<Tex>,
    overview: Option<Tex>,
    people: Option<Tex>,
}

pub struct Page {
    pub backend: Backend,
    id: String,
    pre: &'static str,
    src: Source,
    title: Title,
    rx: Option<Receiver<Got>>,
    info: Option<Info>,
    seasons: Vec<Season>,
    note: Option<Tex>,
    focus: Focus,
    btn: usize,
    season: usize,
    ep: usize,
    ep_scroll: f32,
    season_scroll: f32,
    buttons: Vec<(String, Act, Option<Tex>)>,
    season_tex: Vec<Option<Tex>>,
    cards: HashMap<(usize, usize), Item>,
    hero: Option<Hero>,
    hero_a: f32,
    hint: Option<Tex>,
    /// Set by a change the page cannot see: the episode cards and buttons are made again.
    stale: bool,
    wake: u32,
}

fn clock(s: i64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Page {
    pub fn new(backend: Backend, title: Title, pre: &'static str, src: Source, wake: u32) -> Page {
        let id = title.id.clone();
        Page {
            rx: Some(load(backend.clone(), id.clone(), wake)),
            backend,
            id,
            pre,
            src,
            title,
            info: None,
            seasons: vec![],
            note: None,
            focus: Focus::Buttons,
            btn: 0,
            season: 0,
            ep: 0,
            ep_scroll: 0.0,
            season_scroll: 0.0,
            buttons: vec![],
            season_tex: vec![],
            cards: HashMap::new(),
            hero: None,
            hero_a: 1.0,
            hint: None,
            stale: true,
            wake,
        }
    }

    /// Asks the server again (after a film was played, its resume point moved).
    pub fn reload(&mut self) {
        self.rx = Some(load(self.backend.clone(), self.id.clone(), self.wake));
    }

    /// What the server sent: `Some(true)` when the title is not one this page shows (the media app should).
    pub fn poll(&mut self, g: &Gfx) -> Option<bool> {
        let got = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        match got {
            Got::Elsewhere => return Some(true),
            Got::Failed(e) => {
                log(&format!("detail: {e}"));
                if self.info.is_none() {
                    self.note = g.text(g.f_sub, &format!("The server did not answer ({e})."), 1100);
                }
            }
            Got::Loaded(info, seasons, next) => {
                let first = self.info.is_none();
                let same = seasons.len() == self.seasons.len()
                    && seasons
                        .iter()
                        .zip(&self.seasons)
                        .all(|(a, b)| a.eps.len() == b.eps.len());
                if first || !same {
                    (self.season, self.ep) = next;
                }
                self.info = Some(info);
                self.seasons = seasons;
                self.note = None;
                self.stale = true;
            }
        }
        Some(false)
    }

    fn ep_ref(&self) -> Option<&Ep> {
        self.seasons.get(self.season)?.eps.get(self.ep)
    }

    /// The buttons, for what is known now.
    fn make_buttons(&mut self, g: &Gfx) {
        let Some(info) = &self.info else {
            self.buttons.clear();
            return;
        };
        let mut b: Vec<(String, Act)> = vec![];
        let (target, resume, name) = if info.series {
            match self.next_up() {
                Some(e) => (
                    e.id.clone(),
                    if e.played { 0 } else { e.resume_s },
                    format!(" S{} E{}", e.season, e.no),
                ),
                None => (String::new(), 0, String::new()),
            }
        } else {
            (
                self.id.clone(),
                if info.played { 0 } else { info.resume_s },
                String::new(),
            )
        };
        if !target.is_empty() {
            if resume > 0 {
                b.push((
                    format!("Resume{name} from {}", clock(resume)),
                    Act::Play(self.backend.spec(&target, false)),
                ));
                b.push(("From the start".into(), Act::Play(self.backend.spec(&target, true))));
            } else {
                b.push((format!("Play{name}"), Act::Play(self.backend.spec(&target, true))));
            }
        }
        let played = if info.series {
            self.seasons.iter().flat_map(|s| &s.eps).all(|e| e.played)
        } else {
            info.played
        };
        b.push((
            if played { "Mark as unwatched" } else { "Mark as watched" }.into(),
            Act::Mark(self.id.clone(), !played),
        ));
        self.buttons = b
            .into_iter()
            .map(|(label, act)| {
                let t = g.text(g.f_menu, &label, 0);
                (label, act, t)
            })
            .collect();
        self.btn = self.btn.min(self.buttons.len().saturating_sub(1));
    }

    /// The episode Play goes to: the one the page opened on, as the server's state says now.
    fn next_up(&self) -> Option<&Ep> {
        let flat: Vec<&Ep> = self
            .seasons
            .iter()
            .flat_map(|s| &s.eps)
            .filter(|e| e.season != 0)
            .collect();
        let last_seen = flat.iter().rposition(|e| e.played);
        flat.iter()
            .find(|e| e.resume_s > 0 && !e.played)
            .or_else(|| flat.iter().skip(last_seen.map_or(0, |i| i + 1)).find(|e| !e.played))
            .or_else(|| flat.iter().find(|e| !e.played))
            .or(flat.first())
            .copied()
    }

    /// Moves the focus. Up from the buttons does nothing: Back leaves the page.
    pub fn key(&mut self, dx: i32, dy: i32) {
        let has_eps = !self.seasons.is_empty();
        if dy != 0 {
            self.focus = match (self.focus, dy > 0, has_eps) {
                (Focus::Buttons, true, true) if self.seasons.len() > 1 => Focus::Seasons,
                (Focus::Buttons, true, true) => Focus::Eps,
                (Focus::Seasons, true, _) => Focus::Eps,
                (Focus::Seasons, false, _) => Focus::Buttons,
                (Focus::Eps, false, _) if self.seasons.len() > 1 => Focus::Seasons,
                (Focus::Eps, false, _) => Focus::Buttons,
                (f, _, _) => f,
            };
        }
        let step = |v: usize, n: usize| (v as i32 + dx).clamp(0, n.saturating_sub(1) as i32) as usize;
        if dx != 0 {
            match self.focus {
                Focus::Buttons => self.btn = step(self.btn, self.buttons.len()),
                Focus::Seasons => {
                    let s = step(self.season, self.seasons.len());
                    if s != self.season {
                        self.season = s;
                        let eps = &self.seasons[s].eps;
                        self.ep = eps.iter().position(|e| !e.played).unwrap_or(0);
                        self.cards.clear();
                    }
                }
                Focus::Eps => self.ep = step(self.ep, self.seasons.get(self.season).map_or(0, |s| s.eps.len())),
            }
        }
    }

    /// OK: the focused button's action, or play the focused episode.
    pub fn tap(&self) -> Option<Act> {
        match self.focus {
            Focus::Buttons => self.buttons.get(self.btn).map(|(_, a, _)| a.clone()),
            Focus::Seasons => None,
            Focus::Eps => self.ep_ref().map(|e| Act::Play(self.backend.spec(&e.id, e.played))),
        }
    }

    /// What a hold of OK offers for the focused episode: (label, action).
    pub fn options(&self) -> Option<(String, Vec<(String, Act)>)> {
        if self.focus != Focus::Eps {
            return None;
        }
        let e = self.ep_ref()?;
        Some((
            format!("S{} E{}   {}", e.season, e.no, e.name),
            vec![
                ("From the start".into(), Act::Play(self.backend.spec(&e.id, true))),
                (
                    if e.played {
                        "Mark as unwatched"
                    } else {
                        "Mark as watched"
                    }
                    .into(),
                    Act::Mark(e.id.clone(), !e.played),
                ),
            ],
        ))
    }

    /// Shows a change of watched state at once; the server is told by [`mark`].
    pub fn marked(&mut self, id: &str, played: bool) {
        if id == self.id {
            if let Some(i) = &mut self.info {
                i.played = played;
                i.resume_s = 0;
            }
        }
        let whole = id == self.id;
        for e in self.seasons.iter_mut().flat_map(|s| &mut s.eps) {
            if whole || e.id == id {
                e.played = played;
                e.resume_s = 0;
                e.progress = 0.0;
            }
        }
        self.stale = true;
    }

    fn still_path(&self, e: &Ep) -> Option<String> {
        let tag = e.still.as_ref()?;
        let end: String = tag
            .rsplit('/')
            .next()?
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        // Named after the series, so the library's clean-up keeps it while the series is in the library.
        Some(format!(
            "{DIR}/cache/{}-e-{}-{}{}.img",
            self.pre,
            self.id,
            e.id,
            &end[end.len().saturating_sub(8)..]
        ))
    }

    /// The stills to fetch: the focused season's, around the focus.
    pub fn wanted(&self) -> Vec<(String, String)> {
        let Some(s) = self.seasons.get(self.season) else {
            return vec![];
        };
        let from = self.ep.saturating_sub(1);
        s.eps
            .iter()
            .skip(from)
            .take(7)
            .filter_map(|e| Some((self.src.still(&e.id, e.still.as_ref()?, STILL_W)?, self.still_path(e)?)))
            .collect()
    }

    /// A key that changes when [`wanted`] would.
    pub fn wanted_key(&self) -> String {
        format!("detail/{}/{}/{}/{}", self.id, self.seasons.len(), self.season, self.ep)
    }

    pub fn saved(&mut self, path: &str) {
        for it in self.cards.values_mut() {
            if it.icons.first().is_some_and(|p| p == path) {
                it.icon_tried = false;
            }
        }
    }

    pub fn animate(&mut self, dt: f32) -> bool {
        let mut anim = self.rx.is_some();
        anim |= ease(&mut self.hero_a, 1.0, dt, 9.0);
        anim |= ease(
            &mut self.ep_scroll,
            self.ep.saturating_sub(1) as f32 * (EP.0 + GAP),
            dt,
            12.0,
        );
        anim |= ease(&mut self.season_scroll, 0.0, dt, 12.0);
        for ((s, e), it) in &mut self.cards {
            let on = self.focus == Focus::Eps && *s == self.season && *e == self.ep;
            anim |= ease(&mut it.focus, f32::from(on), dt, 14.0);
        }
        anim
    }

    pub fn draw(&mut self, g: &Gfx) {
        if self.stale {
            self.stale = false;
            self.make_buttons(g);
            self.season_tex = self.seasons.iter().map(|s| g.text(g.f_menu, &s.name, 0)).collect();
            self.cards.clear();
            self.hero = None;
        }
        g.fill(
            BLACK,
            0.35,
            FRect {
                x: 0.0,
                y: 0.0,
                w: W,
                h: H,
            },
        );
        self.draw_hero(g);
        let y = Y_BUTTONS;
        match (&self.note, &self.info) {
            (Some(n), _) => g.put(Some(n), MARGIN, y + 16.0, DIM, 1.0),
            (None, None) => {}
            (None, Some(_)) => {
                let mut x = MARGIN;
                for (i, (_, _, t)) in self.buttons.iter().enumerate() {
                    let tw = t.as_ref().map_or(0.0, |t| t.w as f32);
                    let w = tw + 72.0;
                    let on = self.focus == Focus::Buttons && i == self.btn;
                    g.rounded(
                        x,
                        y,
                        w,
                        BTN_H,
                        BTN_H / 2.0,
                        if on { WHITE } else { PANEL },
                        if on { 1.0 } else { 0.85 },
                    );
                    let th = t.as_ref().map_or(0.0, |t| t.h as f32);
                    g.put(
                        t.as_ref(),
                        x + 36.0,
                        y + (BTN_H - th) / 2.0,
                        if on { BLACK } else { TEXT },
                        1.0,
                    );
                    x += w + 20.0;
                }
            }
        }
        if self.seasons.is_empty() {
            return;
        }
        if self.seasons.len() > 1 {
            let mut x = MARGIN - self.season_scroll;
            let first = self.season.saturating_sub(5);
            for (i, t) in self.season_tex.iter().enumerate().skip(first) {
                let tw = t.as_ref().map_or(0.0, |t| t.w as f32);
                let w = tw + 48.0;
                if x > W {
                    break;
                }
                let here = i == self.season;
                let on = here && self.focus == Focus::Seasons;
                if here {
                    g.rounded(
                        x,
                        Y_SEASONS,
                        w,
                        SEASON_H,
                        SEASON_H / 2.0,
                        if on { WHITE } else { PANEL },
                        1.0,
                    );
                }
                let th = t.as_ref().map_or(0.0, |t| t.h as f32);
                let c = if on {
                    BLACK
                } else if here {
                    TEXT
                } else {
                    DIM
                };
                g.put(t.as_ref(), x + 24.0, Y_SEASONS + (SEASON_H - th) / 2.0, c, 1.0);
                x += w + 12.0;
            }
        }
        if self.focus == Focus::Eps {
            if self.hint.is_none() {
                self.hint = g.text(g.f_label, "Hold OK for more", 0);
            }
            let w = self.hint.as_ref().map_or(0.0, |t| t.w as f32);
            g.put(self.hint.as_ref(), W - MARGIN - w, Y_SEASONS + 14.0, DIM, 1.0);
        }
        let si = self.season;
        let Some(season) = self.seasons.get(si) else { return };
        let from = self.ep.saturating_sub(2);
        let mut focused = None;
        for (i, e) in season.eps.iter().enumerate().skip(from).take(7) {
            let x = MARGIN + i as f32 * (EP.0 + GAP) - self.ep_scroll;
            if x > W || x + EP.0 < 0.0 {
                continue;
            }
            let path = self.still_path(e);
            let it = self.cards.entry((si, i)).or_insert_with(|| {
                let mut it = Item::new(format!("ep:{}", e.id), String::new(), Kind::Watch, Some(e.name.clone()));
                it.icons.extend(path);
                it.cover = true;
                it.progress = e.progress;
                it.color = rgb(40, 46, 58);
                let mark = if e.played { "   ·   Watched" } else { "" };
                it.label = g.text(
                    g.f_label,
                    &clip(&format!("{}. {}{mark}", e.no, e.name), 44),
                    EP.0 as u32,
                );
                it
            });
            if it.focus > 0.01 {
                focused = Some((i, x));
            } else {
                g.card(it, EP, x, Y_EPS, 1.0, if e.played { 0.35 } else { 0.0 });
            }
        }
        if let Some((i, x)) = focused {
            let played = season.eps[i].played;
            if let Some(it) = self.cards.get_mut(&(si, i)) {
                g.card(it, EP, x, Y_EPS, 1.0, if played { 0.35 } else { 0.0 });
            }
        }
    }

    fn draw_hero(&mut self, g: &Gfx) {
        let on_ep = self.focus != Focus::Buttons;
        let ep = if on_ep { self.ep_ref().cloned() } else { None };
        let ep = ep.as_ref();
        let key = ep.map_or_else(|| String::from("title"), |e| e.id.clone());
        if self.hero.as_ref().is_none_or(|h| h.key != key) {
            let t = &self.title;
            let info = self.info.clone().unwrap_or_default();
            let (line, meta, overview) = match ep {
                Some(e) => (
                    g.text(
                        g.f_title,
                        &clip(&format!("S{} E{}   {}", e.season, e.no, e.name), 60),
                        0,
                    ),
                    e.meta.clone(),
                    e.overview.clone(),
                ),
                None => (None, t.meta.clone(), t.overview.clone()),
            };
            let opt = |font, s: &str, wrap| g.text(font, s, wrap).filter(|_| !s.is_empty());
            self.hero_a = if self.hero.is_some() { 0.0 } else { 1.0 };
            self.hero = Some(Hero {
                key,
                title: g.text(g.f_hero, &clip(&t.name, 36), 0),
                line,
                meta: opt(g.f_sub, &meta, 0),
                tech: if ep.is_some() {
                    None
                } else {
                    opt(g.f_sub, &info.tech, 0)
                },
                overview: opt(g.f_sub, &clip(&overview, if ep.is_some() { 260 } else { 330 }), 1150),
                people: if ep.is_some() {
                    None
                } else {
                    opt(g.f_label, &info.people, 1150)
                },
            });
        }
        let Some(h) = &self.hero else { return };
        let (dx, a) = ((1.0 - self.hero_a) * 24.0, self.hero_a);
        let mut y = Y_TITLE;
        g.put(h.title.as_ref(), MARGIN, y, TEXT, 1.0);
        y += h.title.as_ref().map_or(0.0, |t| t.h as f32) + 4.0;
        for (t, c, gap) in [
            (&h.line, TEXT, 6.0),
            (&h.meta, TEXT, 8.0),
            (&h.tech, YELLOW, 18.0),
            (&h.overview, DIM, 14.0),
            (&h.people, DIM, 0.0),
        ] {
            if let Some(t) = t {
                if y + t.h as f32 > Y_BUTTONS - 16.0 {
                    break;
                }
                g.put(Some(t), MARGIN + dx, y, c, a);
                y += t.h as f32 + gap;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ep(season: i64, no: i64, played: bool, resume_s: i64) -> Ep {
        Ep {
            id: format!("{season}x{no}"),
            season,
            season_name: String::new(),
            no,
            name: String::new(),
            overview: String::new(),
            meta: String::new(),
            played,
            resume_s,
            progress: 0.0,
            still: None,
        }
    }

    #[test]
    fn seasons_in_order_specials_last_and_the_next_episode() {
        let (s, next) = group(vec![
            ep(2, 1, false, 0),
            ep(0, 1, false, 0),
            ep(1, 2, true, 0),
            ep(1, 1, true, 0),
        ]);
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Season 1", "Season 2", "Specials"]);
        assert_eq!(next, (1, 0), "after the last watched, specials aside");
        let (_, next) = group(vec![ep(1, 1, true, 0), ep(1, 2, false, 0), ep(1, 3, false, 600)]);
        assert_eq!(next, (0, 2), "a begun episode first");
        let (_, next) = group(vec![ep(1, 1, false, 0), ep(1, 2, true, 0), ep(1, 3, false, 0)]);
        assert_eq!(next, (0, 2), "after the last watched, not the first gap");
        let (_, next) = group(vec![ep(1, 1, true, 0), ep(1, 2, true, 0)]);
        assert_eq!(next, (0, 0), "all watched: from the first");
    }

    #[test]
    fn jellyfin_film_and_episode() {
        let o = json!({
            "Type": "Movie",
            "UserData": { "PlaybackPositionTicks": 6_000_000_000i64, "Played": false },
            "People": [
                { "Name": "Andrew Stanton", "Type": "Director" },
                { "Name": "Albert Brooks", "Type": "Actor" },
                { "Name": "Ellen DeGeneres", "Type": "Actor" }
            ],
            "MediaStreams": [
                { "Type": "Video", "Width": 3840, "VideoRangeType": "DOVIWithHDR10" },
                { "Type": "Audio", "Codec": "eac3", "Channels": 6, "DisplayTitle": "English - Dolby Digital+ Atmos", "IsDefault": true },
                { "Type": "Subtitle" }, { "Type": "Subtitle" }
            ]
        });
        let i = jf_info(&o);
        assert_eq!(
            i.tech,
            "4K   ·   Dolby Vision   ·   Dolby Digital+ Atmos 5.1   ·   Subtitles: 2"
        );
        assert_eq!(
            i.people,
            "Directed by Andrew Stanton   ·   With Albert Brooks, Ellen DeGeneres"
        );
        assert_eq!((i.resume_s, i.played, i.series), (600, false, false));
        let e = jf_ep(&json!({
            "Id": "e1", "Name": "Pilot", "IndexNumber": 1, "ParentIndexNumber": 2, "SeasonName": "Season 2",
            "RunTimeTicks": 29_400_000_000i64, "PremiereDate": "2024-03-01T00:00:00Z",
            "UserData": { "PlayedPercentage": 25.0, "PlaybackPositionTicks": 7_350_000_000i64 },
            "ImageTags": { "Primary": "abc" }
        }))
        .unwrap();
        assert_eq!((e.season, e.no, e.meta.as_str()), (2, 1, "49 min   ·   2024-03-01"));
        assert_eq!((e.resume_s, e.progress, e.still.as_deref()), (735, 0.25, Some("abc")));
    }

    #[test]
    fn plex_film_and_episode() {
        let o = json!({
            "type": "movie", "viewOffset": 90_000, "viewCount": 1,
            "Director": [{ "tag": "Sean Baker" }], "Role": [{ "tag": "Mikey Madison" }],
            "Media": [{ "Part": [{ "Stream": [
                { "streamType": 1, "width": 3840, "colorTrc": "smpte2084" },
                { "streamType": 2, "codec": "truehd", "channels": 8, "extendedDisplayTitle": "English (TrueHD Atmos 7.1)", "selected": true },
                { "streamType": 3 }
            ] }] }]
        });
        let i = plex_info(&o);
        assert_eq!(i.tech, "4K   ·   HDR10   ·   Dolby TrueHD Atmos 7.1   ·   Subtitles: 1");
        assert_eq!(i.people, "Directed by Sean Baker   ·   With Mikey Madison");
        assert_eq!((i.resume_s, i.played), (90, true));
        let e = plex_ep(&json!({
            "ratingKey": "28", "title": "Episode 1", "index": 1, "parentIndex": 1, "parentTitle": "Season 1",
            "duration": 2_842_000, "viewOffset": 1_421_000, "thumb": "/library/metadata/28/thumb/1795"
        }))
        .unwrap();
        assert_eq!((e.id.as_str(), e.resume_s, e.progress), ("28", 1421, 0.5));
        let dts = tech(1920, "", Some(("dca", "ma", 8, false)), 0);
        assert_eq!(dts, "1080p   ·   DTS-HD MA 7.1");
    }

    #[test]
    fn specs_for_the_player() {
        let c = crate::plex::Conn {
            base: String::new(),
            token: String::new(),
            client_id: String::new(),
        };
        let b = Backend::Plex(c, "abc".into());
        assert_eq!(b.spec("7", false), "plex:abc:7");
        assert_eq!(b.spec("7", true), "plex:abc:7@0");
    }
}
