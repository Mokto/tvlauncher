//! A title's page, opened by OK in a library tab when films play in the launcher: its logo and details, Play or
//! Resume, From the start and Mark as watched; for a series its seasons and episodes; then the cast and crew, related
//! titles, and About with Information, Languages and Accessibility.
//!
//! The page is made from the library's own title at once, then from the server's answers saved the last time
//! (`cache/<lib|plib>-d-<id>-page`), and drawn again only when the server's answer now differs (a thread, see
//! [`load`]). [`prefetch`] saves the pages and their pictures of a whole library ahead of time, right after the server
//! answered for its list, so it never wakes a media server either.
use crate::data::DIR;
use crate::gfx::*;
use crate::jellyfin::clip;
use crate::library::{Source, Title};
use crate::sdl::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::Duration;

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

/// Someone in the cast or the crew. `photo` is a Jellyfin image tag or a Plex address.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Person {
    pub id: String,
    pub name: String,
    /// The character, or the jobs ("Director, Writer").
    pub role: String,
    pub photo: Option<String>,
}

/// A title the server finds similar.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Related {
    pub id: String,
    pub name: String,
    pub poster: Option<String>,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Info {
    pub series: bool,
    pub genres: Vec<String>,
    /// The age rating ("PG-13", "DK-11").
    pub age: String,
    /// (source, score): ("TMDB", "78%"), ("Rotten Tomatoes", "93%").
    pub scores: Vec<(String, String)>,
    /// "13/10/1998".
    pub date: String,
    /// "1 h 52 min", or for a series "10 seasons, 236 episodes".
    pub length: String,
    pub studio: String,
    pub overview: String,
    pub directors: Vec<String>,
    pub cast: Vec<Person>,
    /// The logo's Jellyfin image tag or Plex path.
    pub logo: Option<String>,
    pub related: Vec<Related>,
    /// Of the film, or of a series' next episode.
    pub streams: Streams,
    pub resume_s: i64,
    pub played: bool,
}

/// What the file carries, as a viewer names it.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Streams {
    /// "4K", "1080p".
    pub res: String,
    /// "Dolby Vision", "HDR10".
    pub range: String,
    /// The default track: "Dolby TrueHD Atmos 7.1".
    pub sound: String,
    pub original: String,
    /// Each audio track: "German (Dolby Digital+ 5.1)".
    pub audio: Vec<String>,
    /// The subtitle languages, each once.
    pub subs: Vec<String>,
    /// "SDH", "CC", "AD".
    pub access: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ep {
    pub id: String,
    pub season: i64,
    pub season_name: String,
    pub no: i64,
    pub name: String,
    pub overview: String,
    pub minutes: i64,
    /// "21/09/1994".
    pub date: String,
    pub played: bool,
    pub resume_s: i64,
    pub progress: f32,
    /// The still's Jellyfin image tag or Plex path.
    pub still: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Season {
    pub name: String,
    pub eps: Vec<Ep>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Got {
    /// The title, its seasons (none for a film) and the episode to watch next (season, episode).
    Loaded(Box<Info>, Vec<Season>, (usize, usize)),
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
    /// Open the page of this title (a related one).
    Open(String),
}

fn json(b: Vec<u8>) -> Result<Value, String> {
    serde_json::from_slice(&b).map_err(|e| e.to_string())
}

/// Where the server's answers for the page of `id` are saved. The id comes right after `d`, as
/// `library::prune` reads it, so the file goes when the title leaves the library.
fn page_file(root: &str, pre: &str, id: &str) -> String {
    format!("{root}{DIR}/cache/{pre}-d-{id}-page")
}

/// The page of `id`, on a thread: first the saved copy (when `saved`), then the server's answer when it differs.
/// Each wakes the main loop; the channel closes when the server has answered or failed.
pub fn load(b: Backend, root: String, pre: &'static str, id: String, wake: u32, saved: bool) -> Receiver<Got> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let send = |g: Got| {
            let _ = tx.send(g);
            unsafe { SDL_PushEvent(&mut Event::new(wake)) };
        };
        let file = page_file(&root, pre, &id);
        let old = saved
            .then(|| std::fs::read(&file).ok())
            .flatten()
            .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
            .map(|v| parse(&b, &v));
        if let Some(g) = &old {
            send(g.clone());
        }
        let t = std::time::Instant::now();
        let new = fetch(&b, &id);
        let ms = t.elapsed().as_millis();
        match new {
            Ok(v) => {
                let got = parse(&b, &v);
                if old.as_ref() == Some(&got) {
                    log(&format!("detail: {id} in {ms} ms, unchanged"));
                } else {
                    log(&format!("detail: {id} in {ms} ms"));
                    let _ = std::fs::write(&file, v.to_string());
                    send(got);
                }
            }
            Err(e) if old.is_some() => log(&format!("detail: {id}: {e}, showing the saved copy")),
            Err(e) => send(Got::Failed(e)),
        }
    });
    rx
}

/// Saves the pages of `ids` and all their pictures, on a thread: send it a library right after its server
/// answered. A page saved less than a day ago is left (opening it asks again anyway); the first request that fails
/// ends the library, as the server has gone to sleep.
pub fn prefetch(root: String) -> Sender<(Backend, &'static str, Source, Vec<String>)> {
    const DAY: Duration = Duration::from_secs(24 * 3600);
    let (tx, rx) = channel::<(Backend, &'static str, Source, Vec<String>)>();
    std::thread::spawn(move || {
        while let Ok((b, pre, src, ids)) = rx.recv() {
            let (t, mut pages, mut pics) = (std::time::Instant::now(), 0, 0);
            'library: for id in ids {
                let file = page_file(&root, pre, &id);
                let age = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
                let fresh = age.and_then(|t| t.elapsed().ok()).is_some_and(|a| a < DAY);
                // A fresh page still gets the pictures it misses (a run cut short by a reboot leaves some).
                let saved = fresh
                    .then(|| std::fs::read(&file).ok())
                    .flatten()
                    .and_then(|d| serde_json::from_slice::<Value>(&d).ok());
                let v = match saved {
                    Some(v) => v,
                    None => match fetch(&b, &id) {
                        Ok(v) => {
                            let _ = std::fs::write(&file, v.to_string());
                            pages += 1;
                            v
                        }
                        Err(e) => {
                            log(&format!("prefetch: {id}: {e}"));
                            break;
                        }
                    },
                };
                let Got::Loaded(info, seasons, _) = parse(&b, &v) else {
                    continue;
                };
                for (url, path) in all_pictures(pre, &id, &src, &info, &seasons) {
                    let full = format!("{root}{path}");
                    if std::path::Path::new(&full).exists() {
                        continue;
                    }
                    match crate::net::send(&url, None, &[], 10) {
                        // A picture the server refuses is saved empty, so it is not asked for again.
                        Ok(r) => {
                            let ok = r.status < 400;
                            let _ = std::fs::write(&full, if ok { &r.body[..] } else { b"" });
                            pics += usize::from(ok);
                        }
                        Err(e) => {
                            log(&format!("prefetch: {url}: {e}"));
                            break 'library;
                        }
                    }
                }
            }
            if pages + pics > 0 {
                log(&format!(
                    "prefetch: {pages} pages, {pics} pictures in {} s",
                    t.elapsed().as_secs()
                ));
            }
        }
    });
    tx
}

/// Every picture a page can show: (address, path under the root).
fn all_pictures(pre: &str, id: &str, src: &Source, info: &Info, seasons: &[Season]) -> Vec<(String, String)> {
    let mut out = vec![];
    if let Some(tag) = &info.logo {
        out.extend(src.logo(id, tag).zip(cache_path(pre, id, "logo", tag)));
    }
    for p in &info.cast {
        if let Some(tag) = &p.photo {
            out.extend(
                src.picture(&p.id, tag, FACE_PX)
                    .zip(cache_path(pre, id, &format!("c{}", p.id), tag)),
            );
        }
    }
    for r in &info.related {
        if let Some(tag) = &r.poster {
            out.extend(
                src.picture(&r.id, tag, POSTER_PX)
                    .zip(crate::library::pic_path(pre, 'p', &r.id, tag)),
            );
        }
    }
    for e in seasons.iter().flat_map(|s| &s.eps) {
        if let Some(tag) = &e.still {
            out.extend(src.still(&e.id, tag, STILL_W).zip(cache_path(pre, id, &e.id, tag)));
        }
    }
    out
}

/// Where a picture of the page of `id` is cached. Named after the title, so the library's clean-up keeps it while
/// the title is in the library.
fn cache_path(pre: &str, id: &str, key: &str, tag: &str) -> Option<String> {
    let end: String = tag
        .rsplit('/')
        .next()?
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let key: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    Some(format!(
        "{DIR}/cache/{pre}-e-{id}-{key}{}.img",
        &end[end.len().saturating_sub(8)..]
    ))
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

/// The server's answers for a page, as sent: `item`, `similar`, and for a series `episodes` and `next` (its next
/// episode, for the languages). The extras are left null when the server does not have them.
fn fetch(b: &Backend, id: &str) -> Result<Value, String> {
    let mut v = serde_json::json!({});
    match b {
        Backend::Jellyfin(c) => {
            let user = &c.user_id;
            v["item"] = json(c.get_within(&format!("/Users/{user}/Items/{id}"), 15)?)?;
            v["similar"] = c
                .get_within(&format!("/Items/{id}/Similar?userId={user}&limit=16"), 10)
                .and_then(json)
                .unwrap_or_default();
            if v["item"]["Type"] == "Series" {
                v["episodes"] = json(c.get_within(
                    &format!(
                        "/Shows/{id}/Episodes?userId={user}&Fields=Overview&EnableImageTypes=Primary&ImageTypeLimit=1"
                    ),
                    20,
                )?)?;
                if let Some(next) = next_id(b, &v) {
                    v["next"] = c
                        .get_within(&format!("/Users/{user}/Items/{next}"), 10)
                        .and_then(json)
                        .unwrap_or_default();
                }
            }
        }
        Backend::Plex(conn, _) => {
            v["item"] = conn.get_json(&format!("/library/metadata/{id}"), 15)?;
            v["similar"] = conn
                .get_json(&format!("/library/metadata/{id}/similar?count=16"), 10)
                .unwrap_or_default();
            if v["item"]["MediaContainer"]["Metadata"][0]["type"] == "show" {
                v["episodes"] = conn.get_json(
                    &format!("/library/metadata/{id}/allLeaves?X-Plex-Container-Start=0&X-Plex-Container-Size=2000"),
                    20,
                )?;
                if let Some(next) = next_id(b, &v) {
                    v["next"] = conn
                        .get_json(&format!("/library/metadata/{next}"), 10)
                        .unwrap_or_default();
                }
            }
        }
    }
    Ok(v)
}

fn episodes(b: &Backend, v: &Value) -> Vec<Ep> {
    let (list, ep): (&Value, fn(&Value) -> Option<Ep>) = match b {
        Backend::Jellyfin(_) => (&v["episodes"]["Items"], jf_ep),
        Backend::Plex(..) => (&v["episodes"]["MediaContainer"]["Metadata"], plex_ep),
    };
    list.as_array().into_iter().flatten().filter_map(ep).collect()
}

fn next_id(b: &Backend, v: &Value) -> Option<String> {
    let (seasons, (s, e)) = group(episodes(b, v));
    Some(seasons.get(s)?.eps.get(e)?.id.clone())
}

/// The page, from the server's answers (see [`fetch`]).
fn parse(b: &Backend, v: &Value) -> Got {
    let (o, similar, next) = match b {
        Backend::Jellyfin(_) => (&v["item"], &v["similar"]["Items"], &v["next"]),
        Backend::Plex(..) => (
            &v["item"]["MediaContainer"]["Metadata"][0],
            &v["similar"]["MediaContainer"]["Metadata"],
            &v["next"]["MediaContainer"]["Metadata"][0],
        ),
    };
    let jf = matches!(b, Backend::Jellyfin(_));
    let kind = if jf { &o["Type"] } else { &o["type"] };
    let series = match kind.as_str().unwrap_or_default() {
        "Series" | "show" => true,
        "Movie" | "Episode" | "Video" | "MusicVideo" | "movie" | "episode" | "clip" => false,
        _ => return Got::Elsewhere,
    };
    let mut info = if jf { jf_info(o) } else { plex_info(o) };
    let related = if jf { jf_related } else { plex_related };
    info.related = similar.as_array().into_iter().flatten().filter_map(related).collect();
    if !series {
        return Got::Loaded(Box::new(info), vec![], (0, 0));
    }
    let (seasons, next_ep) = group(episodes(b, v));
    info.length = series_length(&seasons);
    if next.is_object() {
        info.streams = if jf { jf_streams(next) } else { plex_streams(next) };
    }
    Got::Loaded(Box::new(info), seasons, next_ep)
}

fn res_name(width: i64) -> &'static str {
    match width {
        w if w >= 3200 => "4K",
        w if w >= 1800 => "1080p",
        w if w >= 1200 => "720p",
        0 => "",
        _ => "SD",
    }
}

/// A sound track as a viewer names it: "Dolby TrueHD Atmos 7.1".
fn sound_name(codec: &str, profile: &str, ch: i64, atmos: bool) -> String {
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
}

/// One audio or subtitle track, for [`languages`].
struct Track<'a> {
    lang: &'a str,
    title: String,
    codec: &'a str,
    sound: String,
    sdh: bool,
}

/// The audio tracks, the subtitle languages and the accessibility marks.
fn languages(audio: &[Track], subs: &[Track]) -> (Vec<String>, Vec<String>, Vec<&'static str>) {
    let name = |l: &str| match crate::lang::name(l) {
        n if n.is_empty() => "Unknown".to_string(),
        n => n,
    };
    let mut a: Vec<String> = vec![];
    for t in audio {
        let s = format!("{} ({})", name(t.lang), t.sound);
        if !a.contains(&s) {
            a.push(s);
        }
    }
    let mut s: Vec<String> = vec![];
    for t in subs {
        let n = name(t.lang);
        if !s.contains(&n) {
            s.push(n);
        }
    }
    let words = |t: &Track| t.title.to_lowercase();
    let mut access = vec![];
    if subs.iter().any(|t| t.sdh || words(t).contains("sdh")) {
        access.push("SDH");
    }
    if subs
        .iter()
        .any(|t| t.codec.contains("608") || t.codec.contains("708") || words(t).contains("closed caption"))
    {
        access.push("CC");
    }
    if audio.iter().any(|t| words(t).contains("descri")) {
        access.push("AD");
    }
    (a, s, access)
}

fn jf_streams(o: &Value) -> Streams {
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
    let tracks = |t: &'static str| {
        of(t)
            .map(|a| {
                let s = |k: &str| a[k].as_str().unwrap_or_default();
                let title = format!("{} {}", s("Title"), s("DisplayTitle"));
                let atmos = s("Profile").contains("Atmos") || title.contains("Atmos");
                Track {
                    lang: s("Language"),
                    codec: s("Codec"),
                    sound: sound_name(s("Codec"), s("Profile"), a["Channels"].as_i64().unwrap_or(0), atmos),
                    sdh: a["IsHearingImpaired"] == true,
                    title,
                }
            })
            .collect::<Vec<Track<'_>>>()
    };
    let (audio, subs) = (tracks("Audio"), tracks("Subtitle"));
    let default = of("Audio").position(|s| s["IsDefault"] == true).unwrap_or(0);
    let (a, s, access) = languages(&audio, &subs);
    Streams {
        res: res_name(video.and_then(|v| v["Width"].as_i64()).unwrap_or(0)).into(),
        range: range.into(),
        sound: audio.get(default).map(|t| t.sound.clone()).unwrap_or_default(),
        original: o["OriginalLanguage"]
            .as_str()
            .map(crate::lang::name)
            .unwrap_or_default(),
        audio: a,
        subs: s,
        access,
    }
}

fn plex_streams(o: &Value) -> Streams {
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
    let tracks = |t: i64| {
        of(t)
            .map(|a| {
                let s = |k: &str| a[k].as_str().unwrap_or_default();
                let title = format!("{} {} {}", s("title"), s("displayTitle"), s("extendedDisplayTitle"));
                let atmos = title.contains("Atmos");
                Track {
                    lang: s("languageCode"),
                    codec: s("codec"),
                    sound: sound_name(s("codec"), s("profile"), a["channels"].as_i64().unwrap_or(0), atmos),
                    sdh: a["hearingImpaired"] == true,
                    title,
                }
            })
            .collect::<Vec<Track<'_>>>()
    };
    let (audio, subs) = (tracks(2), tracks(3));
    let default = of(2).position(|s| s["selected"] == true).unwrap_or(0);
    let width = video
        .and_then(|v| v["width"].as_i64())
        .or_else(|| o["Media"][0]["width"].as_i64())
        .unwrap_or(0);
    let (a, s, access) = languages(&audio, &subs);
    Streams {
        res: res_name(width).into(),
        range: range.into(),
        sound: audio.get(default).map(|t| t.sound.clone()).unwrap_or_default(),
        original: String::new(),
        audio: a,
        subs: s,
        access,
    }
}

/// "2024-10-17..." as "17/10/2024".
fn date(s: &str) -> String {
    match (s.get(..4), s.get(5..7), s.get(8..10)) {
        (Some(y), Some(m), Some(d)) if s.len() >= 10 => format!("{d}/{m}/{y}"),
        _ => String::new(),
    }
}

fn length(minutes: i64) -> String {
    match minutes {
        m if m <= 0 => String::new(),
        m if m >= 60 => format!("{} h {:02} min", m / 60, m % 60),
        m => format!("{m} min"),
    }
}

fn series_length(seasons: &[Season]) -> String {
    let real: Vec<&Season> = seasons.iter().filter(|s| s.eps[0].season != 0).collect();
    let eps: usize = real.iter().map(|s| s.eps.len()).sum();
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    format!("{}, {}", plural(real.len(), "season"), plural(eps, "episode"))
}

/// The cast, then the crew with their jobs merged: (name, kind, role, id, photo) in the server's order.
fn crew(people: Vec<(String, String, String, String, Option<String>)>) -> (Vec<String>, Vec<Person>) {
    let directors = people
        .iter()
        .filter(|p| p.1 == "Director")
        .map(|p| p.0.clone())
        .collect();
    let mut out: Vec<Person> = people
        .iter()
        .filter(|p| matches!(p.1.as_str(), "Actor" | "GuestStar"))
        .take(20)
        .map(|p| Person {
            name: p.0.clone(),
            role: p.2.clone(),
            id: p.3.clone(),
            photo: p.4.clone(),
        })
        .collect();
    for p in people.iter().filter(|p| matches!(p.1.as_str(), "Director" | "Writer")) {
        let crew = |o: &Person| o.name == p.0 && (o.role.contains("Director") || o.role.contains("Writer"));
        match out.iter().position(crew) {
            Some(i) => out[i].role += &format!(", {}", p.1),
            None if out.len() < 26 => out.push(Person {
                name: p.0.clone(),
                role: p.1.clone(),
                id: p.3.clone(),
                photo: p.4.clone(),
            }),
            _ => {}
        }
    }
    (directors, out)
}

fn jf_info(o: &Value) -> Info {
    let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
    let people = o["People"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
            Some((
                p["Name"].as_str()?.into(),
                s("Type"),
                s("Role"),
                s("Id"),
                p["PrimaryImageTag"].as_str().map(String::from),
            ))
        })
        .collect();
    let (directors, cast) = crew(people);
    let mut scores = vec![];
    if let Some(r) = o["CriticRating"].as_f64() {
        scores.push(("Rotten Tomatoes".into(), format!("{r:.0}%")));
    }
    if let Some(r) = o["CommunityRating"].as_f64() {
        let src = if o["ProviderIds"]["Tmdb"].is_string() {
            "TMDB"
        } else {
            "Rating"
        };
        scores.push((src.into(), format!("{:.0}%", r * 10.0)));
    }
    let ud = &o["UserData"];
    let series = o["Type"] == "Series";
    Info {
        series,
        genres: o["Genres"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| Some(g.as_str()?.to_string()))
            .collect(),
        age: s("OfficialRating"),
        scores,
        date: date(&s("PremiereDate")),
        length: length(o["RunTimeTicks"].as_i64().unwrap_or(0) / 600_000_000),
        studio: o["Studios"][0]["Name"].as_str().unwrap_or_default().into(),
        overview: s("Overview"),
        directors,
        cast,
        logo: o["ImageTags"]["Logo"].as_str().map(String::from),
        related: vec![],
        streams: jf_streams(o),
        resume_s: ud["PlaybackPositionTicks"].as_i64().unwrap_or(0) / 10_000_000,
        played: ud["Played"] == true,
    }
}

/// A Plex rating and the picture that says whose it is.
fn plex_score(image: &str, r: f64, audience: bool) -> Option<(String, String)> {
    let (src, pct) = match image.split("://").next()? {
        "rottentomatoes" if audience => ("Audience", true),
        "rottentomatoes" => ("Rotten Tomatoes", true),
        "themoviedb" => ("TMDB", true),
        "imdb" => ("IMDb", false),
        _ => return None,
    };
    Some((
        src.into(),
        if pct {
            format!("{:.0}%", r * 10.0)
        } else {
            format!("{r:.1}")
        },
    ))
}

fn plex_info(o: &Value) -> Info {
    let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
    let tags = |k: &str| -> Vec<String> {
        o[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some(t["tag"].as_str()?.to_string()))
            .collect()
    };
    let mut people = vec![];
    for (k, kind) in [("Role", "Actor"), ("Director", "Director"), ("Writer", "Writer")] {
        for p in o[k].as_array().into_iter().flatten() {
            let Some(name) = p["tag"].as_str() else { continue };
            let id = p["id"].as_i64().map_or_else(|| name.to_string(), |n| n.to_string());
            people.push((
                name.to_string(),
                kind.to_string(),
                p["role"].as_str().unwrap_or_default().to_string(),
                id,
                p["thumb"].as_str().map(String::from),
            ));
        }
    }
    let (directors, cast) = crew(people);
    let num = |k: &str| o[k].as_f64();
    let scores = [
        num("rating").and_then(|r| plex_score(&s("ratingImage"), r, false)),
        num("audienceRating").and_then(|r| plex_score(&s("audienceRatingImage"), r, true)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let logo = o["Image"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["type"] == "clearLogo")
        .and_then(|i| i["url"].as_str())
        .map(String::from);
    Info {
        series: o["type"] == "show",
        genres: tags("Genre"),
        age: s("contentRating"),
        scores,
        date: date(&s("originallyAvailableAt")),
        length: length(o["duration"].as_i64().unwrap_or(0) / 60_000),
        studio: s("studio"),
        overview: s("summary"),
        directors,
        cast,
        logo,
        related: vec![],
        streams: plex_streams(o),
        resume_s: o["viewOffset"].as_i64().unwrap_or(0) / 1000,
        played: o["viewCount"].as_i64().unwrap_or(0) > 0,
    }
}

fn jf_related(o: &Value) -> Option<Related> {
    Some(Related {
        id: o["Id"].as_str()?.into(),
        name: o["Name"].as_str()?.into(),
        poster: o["ImageTags"]["Primary"].as_str().map(String::from),
    })
}

fn plex_related(o: &Value) -> Option<Related> {
    Some(Related {
        id: o["ratingKey"].as_str()?.into(),
        name: o["title"].as_str()?.into(),
        poster: o["thumb"].as_str().map(String::from),
    })
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
        minutes: o["RunTimeTicks"].as_i64().unwrap_or(0) / 600_000_000,
        date: date(&s("PremiereDate")),
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
        minutes: duration / 60_000,
        date: date(&s("originallyAvailableAt")),
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
//
// One long page that scrolls: the hero over the backdrop (logo, details, buttons), then for a series its seasons and
// episodes, the cast and crew, related titles, and About with Information, Languages and Accessibility. All y
// positions below are on the page; the screen shows it from `scroll` down.

const LOGO: (f32, f32) = (660.0, 230.0);
const LOGO_BOTTOM: f32 = 560.0;
const Y_LINE: f32 = 590.0;
const Y_SCORE: f32 = 642.0;
const Y_OVERVIEW: f32 = 692.0;
const Y_META: f32 = 816.0;
const Y_BUTTONS: f32 = 870.0;
const BTN_H: f32 = 72.0;
/// Where the hero ends and the first row starts.
const FOLD: f32 = 1000.0;
/// A section heading and the space under it.
const HEAD: f32 = 66.0;
const SEASON_H: f32 = 56.0;
const STILL: (f32, f32) = (420.0, 236.0);
const EP_PITCH: f32 = 452.0;
const EP_BLOCK: f32 = 660.0;
const FACE: f32 = 176.0;
const FACE_PITCH: f32 = 232.0;
const CAST_BLOCK: f32 = HEAD + FACE + 130.0;
const POSTER: (f32, f32) = (240.0, 360.0);
const POSTER_PITCH: f32 = 272.0;
const REL_BLOCK: f32 = HEAD + POSTER.1 + 110.0;
const ABOUT_W: f32 = 760.0;
const COLS: [f32; 3] = [MARGIN, MARGIN + 640.0, MARGIN + 1200.0];
const STILL_W: u32 = 480;
const FACE_PX: u32 = 240;
const POSTER_PX: u32 = 300;
/// Where a focused section's top sits on the screen once the page has scrolled to it.
const SECTION_TOP: f32 = 150.0;
/// A light layer: the unfocused buttons and pills, the focused text panels.
const SHEER: Color = rgb(255, 255, 255);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Focus {
    Buttons,
    Seasons,
    Eps,
    Cast,
    Related,
    About,
}

#[derive(Clone, Copy)]
enum Shape {
    Round(i32),
    Card(i32, i32, i32),
    Fit,
}

struct Btn {
    icon: crate::icons::Icon,
    /// The pill's words (Play, Resume); the round buttons have a caption, shown while focused.
    label: Option<Tex>,
    caption: Option<Tex>,
    act: Act,
}

/// The texts of the page, made again when what they show changes.
#[derive(Default)]
struct Texts {
    title: Option<Tex>,
    line: Option<Tex>,
    badges: Vec<Option<Tex>>,
    scores: Vec<(Option<Tex>, Option<Tex>)>,
    overview: Option<Tex>,
    meta: Option<Tex>,
    people: Vec<(Option<Tex>, Option<Tex>)>,
    seasons: Vec<Option<Tex>>,
    /// Cast & Crew, Related, About, Information, Languages, Accessibility.
    heads: Vec<Option<Tex>>,
    cast: Vec<(Option<Tex>, Option<Tex>, Option<Tex>)>,
    related: Vec<Option<Tex>>,
    about_title: Option<Tex>,
    about_genres: Option<Tex>,
    about_text: Option<Tex>,
    more: Option<Tex>,
    cols: [Vec<(Option<Tex>, Option<Tex>)>; 3],
    hint: Option<Tex>,
}

struct EpTex {
    label: Option<Tex>,
    name: Option<Tex>,
    overview: Option<Tex>,
    date: Option<Tex>,
    length: Option<Tex>,
}

/// Where the sections start on the page, for what it holds now.
struct Lay {
    seasons: f32,
    eps: f32,
    cast: f32,
    related: f32,
    about: f32,
    cols: f32,
    end: f32,
}

pub struct Page {
    pub backend: Backend,
    root: String,
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
    cast: usize,
    rel: usize,
    scroll: f32,
    /// The rows' sideways scroll: seasons, episodes, cast, related.
    rows_x: [f32; 4],
    /// Grows the focused card in, after each move.
    pop: f32,
    expanded: bool,
    /// The tabs or the cog have the focus: the page shows none.
    pub away: bool,
    buttons: Vec<Btn>,
    tex: Texts,
    eps_tex: HashMap<(usize, usize), EpTex>,
    pics: HashMap<String, Option<Tex>>,
    /// Set by a change the page cannot see: the texts and buttons are made again.
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

/// The sideways scroll that keeps item `i` of `n` (each `pitch` apart) on screen, one before it showing.
fn row_x(i: usize, n: usize, pitch: f32) -> f32 {
    let room = W - 2.0 * MARGIN;
    let most = (n as f32 * pitch - room).max(0.0);
    (i.saturating_sub(1) as f32 * pitch).min(most)
}

fn texh(t: &Option<Tex>) -> f32 {
    t.as_ref().map_or(0.0, |t| t.h as f32)
}

fn texw(t: &Option<Tex>) -> f32 {
    t.as_ref().map_or(0.0, |t| t.w as f32)
}

impl Page {
    pub fn new(backend: Backend, title: Title, pre: &'static str, src: Source, wake: u32, root: String) -> Page {
        let id = title.id.clone();
        Page {
            rx: Some(load(backend.clone(), root.clone(), pre, id.clone(), wake, true)),
            root,
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
            cast: 0,
            rel: 0,
            scroll: 0.0,
            rows_x: [0.0; 4],
            pop: 1.0,
            expanded: false,
            away: false,
            buttons: vec![],
            tex: Texts::default(),
            eps_tex: HashMap::new(),
            pics: HashMap::new(),
            stale: true,
            wake,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Asks the server again (after a film was played, its resume point moved).
    pub fn reload(&mut self) {
        let b = self.backend.clone();
        self.rx = Some(load(b, self.root.clone(), self.pre, self.id.clone(), self.wake, false));
    }

    /// What the server sent: `Some(true)` when the title is not one this page shows (the media app should),
    /// `Some(false)` when the page has new details.
    pub fn poll(&mut self, g: &Gfx) -> Option<bool> {
        let got = match self.rx.as_ref()?.try_recv() {
            Ok(g) => g,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                return None;
            }
        };
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
                self.info = Some(*info);
                self.seasons = seasons;
                self.note = None;
                self.stale = true;
            }
        }
        Some(false)
    }

    /// Drops the related titles Home cannot open (not in any library tab).
    pub fn keep_related(&mut self, known: impl Fn(&str) -> bool) {
        if let Some(i) = &mut self.info {
            i.related.retain(|r| known(&r.id));
            self.rel = self.rel.min(i.related.len().saturating_sub(1));
            self.stale = true;
        }
    }

    fn ep_ref(&self) -> Option<&Ep> {
        self.seasons.get(self.season)?.eps.get(self.ep)
    }

    /// The rows the focus can be on, top to bottom.
    fn rows(&self) -> Vec<Focus> {
        let Some(info) = &self.info else {
            return vec![Focus::Buttons];
        };
        let mut r = vec![Focus::Buttons];
        if self.seasons.len() > 1 {
            r.push(Focus::Seasons);
        }
        if !self.seasons.is_empty() {
            r.push(Focus::Eps);
        }
        if !info.cast.is_empty() {
            r.push(Focus::Cast);
        }
        if !info.related.is_empty() {
            r.push(Focus::Related);
        }
        r.push(Focus::About);
        r
    }

    fn layout(&self) -> Lay {
        let mut y = FOLD;
        let seasons = y;
        if self.seasons.len() > 1 {
            y += SEASON_H + 34.0;
        }
        let eps = y;
        if !self.seasons.is_empty() {
            y += EP_BLOCK;
        } else {
            y = seasons;
        }
        let info = self.info.as_ref();
        let cast = y;
        if info.is_some_and(|i| !i.cast.is_empty()) {
            y += CAST_BLOCK;
        }
        let related = y;
        if info.is_some_and(|i| !i.related.is_empty()) {
            y += REL_BLOCK;
        }
        let about = y;
        let t = &self.tex;
        let card = 36.0
            + texh(&t.about_title)
            + 8.0
            + texh(&t.about_genres)
            + 18.0
            + texh(&t.about_text)
            + if t.more.is_some() { 44.0 } else { 0.0 }
            + 36.0;
        let cols = about + HEAD + card + 60.0;
        let col_h = t
            .cols
            .iter()
            .map(|c| c.iter().map(|(l, v)| texh(l) + texh(v) + 22.0).sum::<f32>())
            .fold(0.0, f32::max);
        Lay {
            seasons,
            eps,
            cast,
            related,
            about,
            cols,
            end: cols + HEAD + col_h + 80.0,
        }
    }

    /// The buttons, for what is known now.
    fn make_buttons(&mut self, g: &Gfx) {
        use crate::icons::Icon;
        let Some(info) = &self.info else {
            self.buttons.clear();
            return;
        };
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
        let played = if info.series {
            self.seasons.iter().flat_map(|s| &s.eps).all(|e| e.played)
        } else {
            info.played
        };
        let mut b = vec![];
        if !target.is_empty() {
            let (label, act) = if resume > 0 {
                (
                    format!("Resume{name} · {}", clock(resume)),
                    Act::Play(self.backend.spec(&target, false)),
                )
            } else {
                (format!("Play{name}"), Act::Play(self.backend.spec(&target, true)))
            };
            b.push(Btn {
                icon: Icon::Play,
                label: g.text(g.f_menu, &label, 0),
                caption: None,
                act,
            });
            if resume > 0 {
                b.push(Btn {
                    icon: Icon::Restart,
                    label: None,
                    caption: g.text(g.f_label, "From the start", 0),
                    act: Act::Play(self.backend.spec(&target, true)),
                });
            }
        }
        b.push(Btn {
            icon: Icon::Check,
            label: None,
            caption: g.text(
                g.f_label,
                if played {
                    "Watched · mark as unwatched"
                } else {
                    "Mark as watched"
                },
                0,
            ),
            act: Act::Mark(self.id.clone(), !played),
        });
        self.buttons = b;
        self.btn = self.btn.min(self.buttons.len().saturating_sub(1));
    }

    fn make_texts(&mut self, g: &Gfx) {
        let opt = |font, s: &str, wrap| g.text(font, s, wrap).filter(|_| !s.is_empty());
        let t = &self.title;
        let mut x = Texts {
            title: g.text(g.f_hero, &clip(&t.name, 30), 0),
            ..Texts::default()
        };
        let Some(i) = &self.info else {
            x.line = opt(g.f_sub, &t.meta, 0);
            x.overview = opt(g.f_sub, &clip(&t.overview, 210), 1000);
            self.tex = x;
            return;
        };
        let kind = if i.series { "TV Show" } else { "Movie" };
        let line: Vec<&str> = std::iter::once(kind)
            .chain(i.genres.iter().take(2).map(String::as_str))
            .chain((!i.age.is_empty()).then_some(i.age.as_str()))
            .collect();
        x.line = opt(g.f_sub, &line.join("  ·  "), 0);
        x.badges = [&i.streams.res, &i.streams.range]
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| g.text(g.f_label, s, 0))
            .collect();
        x.scores = i
            .scores
            .iter()
            .map(|(src, v)| (g.text(g.f_label, src, 0), g.text(g.f_row, v, 0)))
            .collect();
        let overview = if i.overview.is_empty() {
            &t.overview
        } else {
            &i.overview
        };
        x.overview = opt(g.f_sub, &clip(overview, 210), 1000);
        let sound = if i.series { "" } else { i.streams.sound.as_str() };
        let meta: Vec<&str> = [i.date.as_str(), i.length.as_str(), sound]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        x.meta = opt(g.f_sub, &meta.join("  ·  "), 0);
        let actors: Vec<&str> = i
            .cast
            .iter()
            .filter(|p| !p.role.contains("Director") && !p.role.contains("Writer"))
            .take(3)
            .map(|p| p.name.as_str())
            .collect();
        for (label, names) in [("Directed by", i.directors.join(", ")), ("Starring", actors.join(", "))] {
            if !names.is_empty() {
                x.people
                    .push((g.text(g.f_small, label, 0), g.text(g.f_label, &clip(&names, 56), 0)));
            }
        }
        x.seasons = self.seasons.iter().map(|s| g.text(g.f_label, &s.name, 0)).collect();
        x.heads = [
            "Cast & Crew",
            "Related",
            "About",
            "Information",
            "Languages",
            "Accessibility",
        ]
        .iter()
        .map(|h| g.text(g.f_menu, h, 0))
        .collect();
        x.cast = i
            .cast
            .iter()
            .map(|p| {
                (
                    g.text(g.f_small, &clip(&p.name, 17), 0),
                    opt(g.f_small, &clip(&p.role, 19), 0),
                    g.text(g.f_label, &clip(&p.name, 17), 0),
                )
            })
            .collect();
        x.related = i
            .related
            .iter()
            .map(|r| g.text(g.f_label, &clip(&r.name, 20), 0))
            .collect();
        x.about_title = g.text(g.f_menu, &clip(&t.name, 40), 0);
        x.about_genres = opt(g.f_small, &i.genres.join(", "), 0);
        let limit = if self.expanded { 3000 } else { 300 };
        x.about_text = opt(g.f_sub, &clip(overview, limit), (ABOUT_W - 64.0) as u32);
        x.more = (overview.chars().count() > 300 && !self.expanded)
            .then(|| g.text(g.f_label, "MORE", 0))
            .flatten();
        let row = |l: &str, v: &str| (g.text(g.f_small, l, 0), g.text(g.f_label, v, 520));
        let s = &i.streams;
        let mut info_rows = vec![];
        for (l, v) in [
            ("Released", &i.date),
            (if i.series { "Seasons" } else { "Run Time" }, &i.length),
            ("Rated", &i.age),
            ("Studio", &i.studio),
        ] {
            if !v.is_empty() {
                info_rows.push(row(l, v));
            }
        }
        let mut lang_rows = vec![];
        if !s.original.is_empty() {
            lang_rows.push(row("Original Audio", &s.original));
        }
        if !s.audio.is_empty() {
            let mut a: Vec<String> = s.audio.iter().take(4).cloned().collect();
            if s.audio.len() > 4 {
                a.push(format!("and {} more", s.audio.len() - 4));
            }
            lang_rows.push(row("Audio", &a.join("\n")));
        }
        if !s.subs.is_empty() {
            lang_rows.push(row("Subtitles", &clip(&s.subs.join(", "), 110)));
        }
        let access_rows = if s.access.is_empty() {
            vec![(None, g.text(g.f_label, "—", 0))]
        } else {
            s.access
                .iter()
                .map(|a| {
                    let what = match *a {
                        "SDH" => "Subtitles for the deaf and hard of hearing",
                        "CC" => "Closed captions",
                        _ => "Audio description",
                    };
                    (g.text(g.f_label, a, 0), g.text(g.f_small, what, 440))
                })
                .collect()
        };
        x.cols = [info_rows, lang_rows, access_rows];
        x.hint = g.text(g.f_small, "Hold OK for more", 0);
        self.tex = x;
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

    /// The focus is on the top row: Up from here goes to the tabs.
    pub fn at_top(&self) -> bool {
        self.focus == Focus::Buttons
    }

    /// Moves the focus.
    pub fn key(&mut self, dx: i32, dy: i32) {
        let before = (self.focus, self.btn, self.season, self.ep, self.cast, self.rel);
        if dy != 0 {
            let rows = self.rows();
            let at = rows.iter().position(|r| *r == self.focus).unwrap_or(0) as i32;
            self.focus = rows[(at + dy.signum()).clamp(0, rows.len() as i32 - 1) as usize];
        }
        let step = |v: usize, n: usize| (v as i32 + dx).clamp(0, n.saturating_sub(1) as i32) as usize;
        let (n_cast, n_rel) = self.info.as_ref().map_or((0, 0), |i| (i.cast.len(), i.related.len()));
        if dx != 0 {
            match self.focus {
                Focus::Buttons => self.btn = step(self.btn, self.buttons.len()),
                Focus::Seasons => {
                    let s = step(self.season, self.seasons.len());
                    if s != self.season {
                        // The stills of the season left go: a long series would otherwise keep them all.
                        let left: Vec<String> = self.seasons[self.season]
                            .eps
                            .iter()
                            .filter_map(|e| self.still_path(e))
                            .collect();
                        for p in left {
                            self.pics.remove(&p);
                        }
                        self.season = s;
                        let eps = &self.seasons[s].eps;
                        self.ep = eps.iter().position(|e| !e.played).unwrap_or(0);
                        self.eps_tex.clear();
                    }
                }
                Focus::Eps => self.ep = step(self.ep, self.seasons.get(self.season).map_or(0, |s| s.eps.len())),
                Focus::Cast => self.cast = step(self.cast, n_cast),
                Focus::Related => self.rel = step(self.rel, n_rel),
                Focus::About => {}
            }
        }
        if before != (self.focus, self.btn, self.season, self.ep, self.cast, self.rel) {
            self.pop = 0.0;
        }
    }

    /// OK: the focused button's action, play the focused episode, open a related title, or show all of About.
    pub fn tap(&mut self) -> Option<Act> {
        match self.focus {
            Focus::Buttons => self.buttons.get(self.btn).map(|b| b.act.clone()),
            Focus::Eps => self.ep_ref().map(|e| Act::Play(self.backend.spec(&e.id, e.played))),
            Focus::Related => self
                .info
                .as_ref()?
                .related
                .get(self.rel)
                .map(|r| Act::Open(r.id.clone())),
            Focus::About => {
                self.expanded = !self.expanded;
                self.stale = true;
                None
            }
            Focus::Seasons | Focus::Cast => None,
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
        // The saved copy is out of date now: the next visit waits for the server rather than show it.
        let _ = std::fs::remove_file(page_file(&self.root, self.pre, &self.id));
        let whole = id == self.id;
        if whole {
            if let Some(i) = &mut self.info {
                i.played = played;
                i.resume_s = 0;
            }
        }
        for e in self.seasons.iter_mut().flat_map(|s| &mut s.eps) {
            if whole || e.id == id {
                e.played = played;
                e.resume_s = 0;
                e.progress = 0.0;
            }
        }
        self.stale = true;
    }

    fn cache(&self, key: &str, tag: &str) -> Option<String> {
        cache_path(self.pre, &self.id, key, tag)
    }

    fn still_path(&self, e: &Ep) -> Option<String> {
        self.cache(&e.id, e.still.as_ref()?)
    }

    fn face_path(&self, p: &Person) -> Option<String> {
        self.cache(&format!("c{}", p.id), p.photo.as_ref()?)
    }

    fn poster_path(&self, r: &Related) -> Option<String> {
        crate::library::pic_path(self.pre, 'p', &r.id, r.poster.as_ref()?)
    }

    fn logo_path(&self) -> Option<String> {
        self.cache("logo", self.info.as_ref()?.logo.as_ref()?)
    }

    /// The pictures to fetch: the logo, then the focused row's around the focus, then the other rows' first ones.
    pub fn wanted(&self) -> Vec<(String, String)> {
        let Some(i) = &self.info else { return vec![] };
        let src = &self.src;
        let mut out: Vec<(String, String)> = vec![];
        if let (Some(tag), Some(path)) = (&i.logo, self.logo_path()) {
            out.extend(src.logo(&self.id, tag).map(|u| (u, path)));
        }
        let stills: Vec<(String, String)> = self
            .seasons
            .get(self.season)
            .into_iter()
            .flat_map(|s| s.eps.iter().skip(self.ep.saturating_sub(1)).take(6))
            .filter_map(|e| Some((src.still(&e.id, e.still.as_ref()?, STILL_W)?, self.still_path(e)?)))
            .collect();
        let faces: Vec<(String, String)> = i
            .cast
            .iter()
            .skip(self.cast.saturating_sub(1))
            .take(10)
            .filter_map(|p| Some((src.picture(&p.id, p.photo.as_ref()?, FACE_PX)?, self.face_path(p)?)))
            .collect();
        let posters: Vec<(String, String)> = i
            .related
            .iter()
            .skip(self.rel.saturating_sub(1))
            .take(8)
            .filter_map(|r| Some((src.picture(&r.id, r.poster.as_ref()?, POSTER_PX)?, self.poster_path(r)?)))
            .collect();
        let mut rows = [stills, faces, posters];
        let first = match self.focus {
            Focus::Cast => 1,
            Focus::Related | Focus::About => 2,
            _ => 0,
        };
        rows.swap(0, first);
        out.extend(rows.into_iter().flatten());
        out
    }

    /// A key that changes when [`wanted`] would.
    pub fn wanted_key(&self) -> String {
        let i = self.info.as_ref();
        format!(
            "detail/{}/{}/{}/{:?}/{}/{}/{}/{}",
            self.id,
            i.is_some(),
            i.map_or(0, |i| i.related.len()),
            self.focus,
            self.season,
            self.ep,
            self.cast,
            self.rel
        )
    }

    pub fn saved(&mut self, path: &str) {
        self.pics.remove(path);
    }

    /// The picture at `path`, made to `shape`: None until it is on disk (or if it cannot be read).
    fn pic(&mut self, g: &Gfx, path: &str, shape: Shape) -> Option<&Tex> {
        if !self.pics.contains_key(path) {
            let t = Surf::load(&g.rp(path))
                .and_then(|s| match shape {
                    Shape::Round(d) => cover(&s, d, d, d / 2),
                    Shape::Card(w, h, r) => cover(&s, w, h, r),
                    Shape::Fit => visible(&s),
                })
                .and_then(|s| Tex::new(g.ren, &s));
            self.pics.insert(path.to_string(), t);
        }
        self.pics.get(path)?.as_ref()
    }

    pub fn animate(&mut self, dt: f32) -> bool {
        let mut anim = self.rx.is_some();
        let lay = self.layout();
        let target = match self.focus {
            Focus::Buttons => 0.0,
            Focus::Seasons | Focus::Eps => lay.seasons - SECTION_TOP,
            Focus::Cast => lay.cast - SECTION_TOP,
            Focus::Related => lay.related - SECTION_TOP,
            Focus::About => lay.about - SECTION_TOP,
        };
        let target = target.min(lay.end - H).max(0.0);
        anim |= ease(&mut self.scroll, target, dt, 10.0);
        let (n_cast, n_rel) = self.info.as_ref().map_or((0, 0), |i| (i.cast.len(), i.related.len()));
        let n_eps = self.seasons.get(self.season).map_or(0, |s| s.eps.len());
        let season_w: Vec<f32> = self.tex.seasons.iter().map(|t| texw(t) + 60.0).collect();
        let season_x = season_w[..self.season.min(season_w.len())].iter().sum::<f32>();
        let season_x = (season_x - 300.0)
            .min(season_w.iter().sum::<f32>() - (W - 2.0 * MARGIN))
            .max(0.0);
        let targets = [
            season_x,
            row_x(self.ep, n_eps, EP_PITCH),
            row_x(self.cast, n_cast, FACE_PITCH),
            row_x(self.rel, n_rel, POSTER_PITCH),
        ];
        for (x, t) in self.rows_x.iter_mut().zip(targets) {
            anim |= ease(x, t, dt, 12.0);
        }
        anim |= ease(&mut self.pop, 1.0, dt, 12.0);
        anim
    }

    pub fn draw(&mut self, g: &Gfx) {
        if self.stale {
            self.stale = false;
            self.make_buttons(g);
            self.make_texts(g);
            self.eps_tex.clear();
        }
        let sy = self.scroll;
        // The backdrop shows behind the hero; below it the page is the plain background.
        let fold = FOLD - 110.0 - sy;
        g.tint(&g.vgrad, BG, 1.0);
        g.copy(
            &g.vgrad,
            None,
            FRect {
                x: 0.0,
                y: fold - 420.0,
                w: W,
                h: 420.0,
            },
        );
        g.fill(
            BG,
            1.0,
            FRect {
                x: 0.0,
                y: fold,
                w: W,
                h: (H - fold).max(0.0),
            },
        );
        if sy < FOLD {
            self.draw_hero(g, -sy);
        }
        if self.info.is_none() {
            return;
        }
        let lay = self.layout();
        let on_screen = |top: f32, h: f32| top - sy < H && top + h - sy > 0.0;
        if self.seasons.len() > 1 && on_screen(lay.seasons, SEASON_H) {
            self.draw_seasons(g, lay.seasons - sy);
        }
        if !self.seasons.is_empty() && on_screen(lay.eps, EP_BLOCK) {
            self.draw_eps(g, lay.eps - sy);
        }
        if self.info.as_ref().is_some_and(|i| !i.cast.is_empty()) && on_screen(lay.cast, CAST_BLOCK) {
            self.draw_cast(g, lay.cast - sy);
        }
        if self.info.as_ref().is_some_and(|i| !i.related.is_empty()) && on_screen(lay.related, REL_BLOCK) {
            self.draw_related(g, lay.related - sy);
        }
        if on_screen(lay.about, lay.end - lay.about) {
            self.draw_about(g, lay.about - sy, lay.cols - sy);
        }
        // Once the page has scrolled, the tabs and the clock sit on a band of the background, not on the rows.
        if sy > 1.0 {
            let a = (sy / 240.0).min(1.0);
            let band = FRect {
                x: 0.0,
                y: 0.0,
                w: W,
                h: 112.0,
            };
            g.fill(BG, a, band);
            g.tint(&g.vgrad, BG, a);
            let fade = FRect {
                x: 0.0,
                y: band.h,
                w: W,
                h: 48.0,
            };
            unsafe {
                SDL_RenderCopyExF(
                    g.ren,
                    g.vgrad.ptr(),
                    std::ptr::null(),
                    &fade,
                    0.0,
                    std::ptr::null(),
                    FLIP_VERTICAL,
                )
            };
        }
    }

    fn draw_hero(&mut self, g: &Gfx, dy: f32) {
        let a = 1.0;
        let logo = self
            .logo_path()
            .and_then(|p| self.pic(g, &p, Shape::Fit).map(|t| (t.w as f32, t.h as f32)));
        if let (Some((w, h)), Some(p)) = (logo, self.logo_path()) {
            let k = (LOGO.0 / w).min(LOGO.1 / h);
            let (lw, lh) = (w * k, h * k);
            if let Some(t) = self.pic(g, &p, Shape::Fit) {
                g.tint(t, WHITE, a);
                g.copy(
                    t,
                    None,
                    FRect {
                        x: MARGIN,
                        y: LOGO_BOTTOM - lh + dy,
                        w: lw,
                        h: lh,
                    },
                );
            }
        } else {
            let th = texh(&self.tex.title);
            g.put(self.tex.title.as_ref(), MARGIN, LOGO_BOTTOM - th + dy, TEXT, a);
        }
        let t = &self.tex;
        g.put(t.line.as_ref(), MARGIN, Y_LINE + dy, DIM, a);
        let mut x = MARGIN + texw(&t.line) + 20.0;
        for b in &t.badges {
            let (bw, bh) = (texw(b), texh(b));
            g.rounded(x, Y_LINE + dy, bw + 24.0, 40.0, 8.0, SHEER, 0.2 * a);
            g.put(b.as_ref(), x + 12.0, Y_LINE + dy + (40.0 - bh) / 2.0, TEXT, a);
            x += bw + 36.0;
        }
        let mut x = MARGIN;
        for (src, v) in &t.scores {
            g.put(src.as_ref(), x, Y_SCORE + dy + 6.0, DIM, a);
            x += texw(src) + 10.0;
            g.put(v.as_ref(), x, Y_SCORE + dy, TEXT, a);
            x += texw(v) + 36.0;
        }
        let y = if t.scores.is_empty() { Y_SCORE } else { Y_OVERVIEW };
        g.put(t.overview.as_ref(), MARGIN, y + dy, TEXT, a);
        g.put(t.meta.as_ref(), MARGIN, Y_META + dy, DIM, a);
        let mut y = Y_BUTTONS - 4.0 + dy;
        for (label, names) in &t.people {
            let right = W - MARGIN;
            g.put(names.as_ref(), right - texw(names), y, TEXT, a);
            g.put(
                label.as_ref(),
                right - texw(names) - 12.0 - texw(label),
                y + (texh(names) - texh(label)) / 2.0,
                DIM,
                a,
            );
            y += 42.0;
        }
        if let Some(n) = &self.note {
            g.put(Some(n), MARGIN, Y_BUTTONS + 16.0 + dy, DIM, a);
            return;
        }
        let mut x = MARGIN;
        let y = Y_BUTTONS + dy;
        for (i, b) in self.buttons.iter().enumerate() {
            let on = !self.away && self.focus == Focus::Buttons && i == self.btn;
            let (bg, ba, fg) = if on { (WHITE, 1.0, BLACK) } else { (SHEER, 0.2, TEXT) };
            let w = if b.label.is_some() {
                34.0 + 34.0 + 14.0 + texw(&b.label) + 40.0
            } else {
                BTN_H
            };
            g.rounded(x, y, w, BTN_H, BTN_H / 2.0, bg, ba * a);
            if b.label.is_some() {
                g.icon(b.icon, x + 30.0, y + (BTN_H - 34.0) / 2.0, 34.0, fg, a);
                g.put(b.label.as_ref(), x + 78.0, y + (BTN_H - texh(&b.label)) / 2.0, fg, a);
            } else {
                g.icon(b.icon, x + (BTN_H - 34.0) / 2.0, y + (BTN_H - 34.0) / 2.0, 34.0, fg, a);
            }
            x += w + 18.0;
        }
        if let Some(b) = self
            .buttons
            .get(self.btn)
            .filter(|_| !self.away && self.focus == Focus::Buttons)
        {
            g.put(
                b.caption.as_ref(),
                x + 8.0,
                y + (BTN_H - texh(&b.caption)) / 2.0,
                DIM,
                a,
            );
        }
    }

    fn draw_seasons(&mut self, g: &Gfx, y: f32) {
        let mut x = MARGIN - self.rows_x[0];
        for (i, t) in self.tex.seasons.iter().enumerate() {
            let w = texw(t) + 48.0;
            if x > W {
                break;
            }
            if x + w > 0.0 {
                let here = i == self.season;
                let on = here && self.focus == Focus::Seasons;
                let (bg, ba, fg) = match (on, here) {
                    (true, _) => (WHITE, 1.0, BLACK),
                    (_, true) => (SHEER, 0.28, TEXT),
                    _ => (SHEER, 0.1, DIM),
                };
                g.rounded(x, y, w, SEASON_H, SEASON_H / 2.0, bg, ba);
                g.put(t.as_ref(), x + 24.0, y + (SEASON_H - texh(t)) / 2.0, fg, 1.0);
            }
            x += w + 12.0;
        }
        if self.focus == Focus::Eps {
            let h = &self.tex.hint;
            g.put(
                h.as_ref(),
                W - MARGIN - texw(h),
                y + (SEASON_H - texh(h)) / 2.0,
                DIM,
                1.0,
            );
        }
    }

    fn draw_eps(&mut self, g: &Gfx, y: f32) {
        let si = self.season;
        let Some(n) = self.seasons.get(si).map(|s| s.eps.len()) else {
            return;
        };
        let first = (self.rows_x[1] / EP_PITCH) as usize;
        for i in first.saturating_sub(1)..n.min(first + 6) {
            let x = MARGIN + i as f32 * EP_PITCH - self.rows_x[1];
            if x > W || x + STILL.0 < 0.0 {
                continue;
            }
            let e = self.seasons[si].eps[i].clone();
            let on = self.focus == Focus::Eps && i == self.ep;
            self.eps_tex.entry((si, i)).or_insert_with(|| {
                let opt = |font, s: &str, wrap| g.text(font, s, wrap).filter(|_| !s.is_empty());
                EpTex {
                    label: g.text(g.f_label, &format!("EPISODE {}", e.no), 0),
                    name: opt(g.f_row, &clip(&e.name, 46), 390),
                    overview: opt(g.f_small, &clip(&e.overview, 190), 390),
                    date: opt(g.f_small, &e.date, 0),
                    length: (e.minutes > 0)
                        .then(|| g.text(g.f_small, &format!("{} min", e.minutes), 0))
                        .flatten(),
                }
            });
            let s = if on { 1.0 + 0.04 * self.pop } else { 1.0 };
            let (w, h) = (STILL.0 * s, STILL.1 * s);
            let (sx, sy) = (x - (w - STILL.0) / 2.0, y - (h - STILL.1) / 2.0);
            if on {
                g.nine(&g.soft, sx - 28.0, sy - 10.0, w + 56.0, h + 64.0, 56.0, BLACK, 0.7);
                g.rounded(sx - 5.0, sy - 5.0, w + 10.0, h + 10.0, 19.0, WHITE, 1.0);
            }
            g.rounded(sx, sy, w, h, 14.0, rgb(40, 46, 58), 1.0);
            if let Some(p) = self.still_path(&e) {
                if let Some(t) = self.pic(g, &p, Shape::Card(STILL.0 as i32, STILL.1 as i32, 14)) {
                    g.tint(t, WHITE, 1.0);
                    g.copy(t, None, FRect { x: sx, y: sy, w, h });
                }
            }
            if e.played {
                g.rounded(sx + w - 56.0, sy + 14.0, 42.0, 42.0, 21.0, WHITE, 0.92);
                g.icon(crate::icons::Icon::Check, sx + w - 47.0, sy + 23.0, 24.0, BLACK, 1.0);
            }
            let et = &self.eps_tex[&(si, i)];
            if let Some(l) = &et.length {
                let (lw, lh) = (l.w as f32, l.h as f32);
                g.rounded(sx + 14.0, sy + h - 54.0, lw + 56.0, 40.0, 20.0, BLACK, 0.55);
                g.icon(crate::icons::Icon::Play, sx + 26.0, sy + h - 44.0, 20.0, YELLOW, 1.0);
                g.put(Some(l), sx + 54.0, sy + h - 54.0 + (40.0 - lh) / 2.0, WHITE, 1.0);
            }
            if e.progress > 0.0 {
                let (bx, by, bw) = (sx + 14.0, sy + h - 10.0, w - 28.0);
                g.rounded(bx, by, bw, 6.0, 3.0, BLACK, 0.55);
                g.rounded(bx, by, (bw * e.progress).max(6.0), 6.0, 3.0, WHITE, 1.0);
            }
            let ty = y + STILL.1 + 28.0;
            let text_h = texh(&et.label) + 6.0 + texh(&et.name) + 12.0 + texh(&et.overview) + 16.0 + texh(&et.date);
            if on {
                g.rounded(x - 4.0, ty - 18.0, STILL.0 + 8.0, text_h + 36.0, 14.0, SHEER, 0.09);
            }
            let tx = x + 16.0;
            let mut cy = ty;
            for (t, c, gap) in [
                (&et.label, DIM, 6.0),
                (&et.name, TEXT, 12.0),
                (&et.overview, DIM, 16.0),
                (&et.date, DIM, 0.0),
            ] {
                g.put(t.as_ref(), tx, cy, c, 1.0);
                cy += texh(t) + if t.is_some() { gap } else { 0.0 };
            }
        }
    }

    fn draw_cast(&mut self, g: &Gfx, y: f32) {
        g.put(self.tex.heads[0].as_ref(), MARGIN, y, TEXT, 1.0);
        let people = self.info.as_ref().map(|i| i.cast.clone()).unwrap_or_default();
        let y = y + HEAD;
        for (i, p) in people.iter().enumerate() {
            let x = MARGIN + i as f32 * FACE_PITCH - self.rows_x[2];
            if x > W || x + FACE_PITCH < 0.0 {
                continue;
            }
            let on = self.focus == Focus::Cast && i == self.cast;
            let s = if on { 1.0 + 0.06 * self.pop } else { 1.0 };
            let d = FACE * s;
            let (cx, cy) = (x + (FACE_PITCH - d) / 2.0 - 28.0, y + (FACE - d) / 2.0);
            if on {
                g.rounded(cx - 6.0, cy - 6.0, d + 12.0, d + 12.0, d / 2.0 + 6.0, WHITE, 1.0);
            }
            let face = self
                .face_path(p)
                .and_then(|path| self.pic(g, &path, Shape::Round(FACE as i32)).map(|_| path));
            match face {
                Some(path) => {
                    if let Some(t) = self.pic(g, &path, Shape::Round(FACE as i32)) {
                        g.tint(t, WHITE, 1.0);
                        g.copy(
                            t,
                            None,
                            FRect {
                                x: cx,
                                y: cy,
                                w: d,
                                h: d,
                            },
                        );
                    }
                }
                None => {
                    g.rounded(cx, cy, d, d, d / 2.0, PANEL, 1.0);
                    g.icon(
                        crate::icons::Icon::Person,
                        cx + d * 0.3,
                        cy + d * 0.3,
                        d * 0.4,
                        DIM,
                        1.0,
                    );
                }
            }
            let Some((name, role, bold)) = self.tex.cast.get(i) else {
                continue;
            };
            let mid = cx + d / 2.0;
            let name = if on { bold } else { name };
            let ny = y + FACE + 22.0;
            g.put(
                name.as_ref(),
                mid - texw(name) / 2.0,
                ny,
                if on { WHITE } else { TEXT },
                1.0,
            );
            g.put(role.as_ref(), mid - texw(role) / 2.0, ny + texh(name) + 2.0, DIM, 1.0);
        }
    }

    fn draw_related(&mut self, g: &Gfx, y: f32) {
        g.put(self.tex.heads[1].as_ref(), MARGIN, y, TEXT, 1.0);
        let list = self.info.as_ref().map(|i| i.related.clone()).unwrap_or_default();
        let y = y + HEAD;
        let mut focused = None;
        for (i, r) in list.iter().enumerate() {
            let x = MARGIN + i as f32 * POSTER_PITCH - self.rows_x[3];
            if x > W || x + POSTER.0 < 0.0 {
                continue;
            }
            let on = self.focus == Focus::Related && i == self.rel;
            if on {
                focused = Some((i, x));
                continue;
            }
            self.draw_poster(g, i, r, x, y, 1.0);
        }
        if let Some((i, x)) = focused {
            let s = 1.0 + 0.06 * self.pop;
            let (w, h) = (POSTER.0 * s, POSTER.1 * s);
            let (px, py) = (x - (w - POSTER.0) / 2.0, y - (h - POSTER.1) / 2.0);
            g.nine(&g.soft, px - 28.0, py - 10.0, w + 56.0, h + 64.0, 56.0, BLACK, 0.7);
            g.rounded(px - 5.0, py - 5.0, w + 10.0, h + 10.0, 21.0, WHITE, 1.0);
            self.draw_poster(g, i, &list[i], x, y, s);
            if let Some(t) = self.tex.related.get(i) {
                g.put(t.as_ref(), x + (POSTER.0 - texw(t)) / 2.0, py + h + 22.0, TEXT, 1.0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_poster(&mut self, g: &Gfx, i: usize, r: &Related, x: f32, y: f32, s: f32) {
        let (w, h) = (POSTER.0 * s, POSTER.1 * s);
        let (px, py) = (x - (w - POSTER.0) / 2.0, y - (h - POSTER.1) / 2.0);
        g.rounded(px, py, w, h, 16.0, rgb(40, 46, 58), 1.0);
        let pic = self
            .poster_path(r)
            .and_then(|p| self.pic(g, &p, Shape::Card(POSTER.0 as i32, POSTER.1 as i32, 16)));
        match pic {
            Some(t) => {
                g.tint(t, WHITE, 1.0);
                g.copy(t, None, FRect { x: px, y: py, w, h });
            }
            None => {
                let t = self.tex.related.get(i).and_then(Option::as_ref);
                g.put(t, px + 16.0, py + 20.0, TEXT, 1.0);
            }
        }
    }

    fn draw_about(&mut self, g: &Gfx, y: f32, cols_y: f32) {
        let t = &self.tex;
        g.put(t.heads[2].as_ref(), MARGIN, y, TEXT, 1.0);
        let (cx, cy) = (MARGIN - 24.0, y + HEAD);
        let card_h = cols_y - 60.0 - cy;
        if self.focus == Focus::About {
            g.nine(
                &g.soft,
                cx - 28.0,
                cy - 10.0,
                ABOUT_W + 56.0,
                card_h + 64.0,
                56.0,
                BLACK,
                0.6,
            );
            g.rounded(cx, cy, ABOUT_W, card_h, 16.0, SHEER, 0.12);
        }
        let mut ty = cy + 36.0;
        let tx = cx + 32.0;
        for (tex, c, gap) in [
            (&t.about_title, TEXT, 8.0),
            (&t.about_genres, DIM, 18.0),
            (&t.about_text, TEXT, 0.0),
        ] {
            g.put(tex.as_ref(), tx, ty, c, 1.0);
            ty += texh(tex) + gap;
        }
        if let Some(m) = &t.more {
            g.put(
                Some(m),
                cx + ABOUT_W - 32.0 - m.w as f32,
                cy + card_h - 36.0 - m.h as f32,
                DIM,
                1.0,
            );
        }
        for (c, rows) in t.cols.iter().enumerate() {
            let x = COLS[c];
            g.put(t.heads[3 + c].as_ref(), x, cols_y, TEXT, 1.0);
            let mut ry = cols_y + HEAD;
            for (label, value) in rows {
                if c == 2 && label.is_some() {
                    // An accessibility mark: the letters in a box, what they mean under it.
                    let (lw, lh) = (texw(label), texh(label));
                    g.rounded(x, ry, lw + 20.0, lh + 8.0, 6.0, SHEER, 0.2);
                    g.put(label.as_ref(), x + 10.0, ry + 4.0, TEXT, 1.0);
                    ry += lh + 14.0;
                    g.put(value.as_ref(), x, ry, DIM, 1.0);
                } else {
                    g.put(label.as_ref(), x, ry, DIM, 1.0);
                    ry += texh(label) + 2.0;
                    g.put(value.as_ref(), x, ry, TEXT, 1.0);
                }
                ry += texh(value) + 22.0;
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
            minutes: 0,
            date: String::new(),
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
        assert_eq!(series_length(&s), "2 seasons, 3 episodes");
        let (_, next) = group(vec![ep(1, 1, true, 0), ep(1, 2, false, 0), ep(1, 3, false, 600)]);
        assert_eq!(next, (0, 2), "a begun episode first");
        let (_, next) = group(vec![ep(1, 1, false, 0), ep(1, 2, true, 0), ep(1, 3, false, 0)]);
        assert_eq!(next, (0, 2), "after the last watched, not the first gap");
        let (s, next) = group(vec![ep(1, 1, true, 0)]);
        assert_eq!(next, (0, 0), "all watched: from the first");
        assert_eq!(series_length(&s), "1 season, 1 episode");
    }

    #[test]
    fn jellyfin_film_and_episode() {
        let o = json!({
            "Type": "Movie", "Name": "Nemo", "Genres": ["Animation", "Family"], "OfficialRating": "G",
            "CommunityRating": 7.84, "ProviderIds": { "Tmdb": "12" }, "PremiereDate": "2003-05-30T00:00:00Z",
            "RunTimeTicks": 60_000_000_000i64, "OriginalLanguage": "en", "ImageTags": { "Logo": "lg" },
            "UserData": { "PlaybackPositionTicks": 6_000_000_000i64, "Played": false },
            "People": [
                { "Name": "Andrew Stanton", "Type": "Director", "Id": "p1" },
                { "Name": "Albert Brooks", "Type": "Actor", "Role": "Marlin", "Id": "p2", "PrimaryImageTag": "t2" },
                { "Name": "Andrew Stanton", "Type": "Writer", "Id": "p1" }
            ],
            "MediaStreams": [
                { "Type": "Video", "Width": 3840, "VideoRangeType": "DOVIWithHDR10" },
                { "Type": "Audio", "Language": "eng", "Codec": "eac3", "Channels": 6, "DisplayTitle": "English - Dolby Digital+ Atmos", "IsDefault": true },
                { "Type": "Audio", "Language": "eng", "Codec": "ac3", "Channels": 2, "Title": "Audio Description" },
                { "Type": "Subtitle", "Language": "eng", "IsHearingImpaired": true },
                { "Type": "Subtitle", "Language": "fre" }, { "Type": "Subtitle", "Language": "eng" }
            ]
        });
        let i = jf_info(&o);
        assert_eq!(
            (i.date.as_str(), i.length.as_str(), i.age.as_str()),
            ("30/05/2003", "1 h 40 min", "G")
        );
        assert_eq!(i.scores, [("TMDB".to_string(), "78%".to_string())]);
        assert_eq!(i.directors, ["Andrew Stanton"]);
        let crew: Vec<(&str, &str)> = i.cast.iter().map(|p| (p.name.as_str(), p.role.as_str())).collect();
        assert_eq!(
            crew,
            [("Albert Brooks", "Marlin"), ("Andrew Stanton", "Director, Writer")]
        );
        assert_eq!(i.cast[0].photo.as_deref(), Some("t2"));
        let s = &i.streams;
        assert_eq!((s.res.as_str(), s.range.as_str()), ("4K", "Dolby Vision"));
        assert_eq!(s.sound, "Dolby Digital+ Atmos 5.1");
        assert_eq!(s.original, "English");
        assert_eq!(
            s.audio,
            ["English (Dolby Digital+ Atmos 5.1)", "English (Dolby Digital Stereo)"]
        );
        assert_eq!(s.subs, ["English", "French"]);
        assert_eq!(s.access, ["SDH", "AD"]);
        assert_eq!(
            (i.resume_s, i.played, i.series, i.logo.as_deref()),
            (600, false, false, Some("lg"))
        );
        let e = jf_ep(&json!({
            "Id": "e1", "Name": "Pilot", "IndexNumber": 1, "ParentIndexNumber": 2, "SeasonName": "Season 2",
            "RunTimeTicks": 29_400_000_000i64, "PremiereDate": "2024-03-01T00:00:00Z",
            "UserData": { "PlayedPercentage": 25.0, "PlaybackPositionTicks": 7_350_000_000i64 },
            "ImageTags": { "Primary": "abc" }
        }))
        .unwrap();
        assert_eq!((e.season, e.no, e.minutes, e.date.as_str()), (2, 1, 49, "01/03/2024"));
        assert_eq!((e.resume_s, e.progress, e.still.as_deref()), (735, 0.25, Some("abc")));
    }

    #[test]
    fn plex_film_and_episode() {
        let o = json!({
            "type": "movie", "viewOffset": 90_000, "viewCount": 1, "duration": 8_340_000,
            "rating": 9.3, "ratingImage": "rottentomatoes://image.rating.ripe",
            "audienceRating": 8.4, "audienceRatingImage": "rottentomatoes://image.rating.upright",
            "Director": [{ "tag": "Sean Baker", "id": 5 }],
            "Role": [{ "tag": "Mikey Madison", "role": "Ani", "id": 7, "thumb": "https://example.org/p.jpg" }],
            "Image": [{ "type": "background", "url": "/a" }, { "type": "clearLogo", "url": "/library/metadata/83/clearLogo/1" }],
            "Media": [{ "Part": [{ "Stream": [
                { "streamType": 1, "width": 3840, "colorTrc": "smpte2084" },
                { "streamType": 2, "codec": "truehd", "channels": 8, "languageCode": "eng", "extendedDisplayTitle": "English (TrueHD Atmos 7.1)", "selected": true },
                { "streamType": 3, "languageCode": "eng", "codec": "srt" }
            ] }] }]
        });
        let i = plex_info(&o);
        assert_eq!(i.streams.sound, "Dolby TrueHD Atmos 7.1");
        assert_eq!((i.streams.res.as_str(), i.streams.range.as_str()), ("4K", "HDR10"));
        assert_eq!(
            i.scores,
            [
                ("Rotten Tomatoes".to_string(), "93%".to_string()),
                ("Audience".to_string(), "84%".to_string())
            ]
        );
        assert_eq!(i.length, "2 h 19 min");
        assert_eq!(i.logo.as_deref(), Some("/library/metadata/83/clearLogo/1"));
        assert_eq!(
            (i.cast[0].id.as_str(), i.cast[0].role.as_str(), i.cast[1].role.as_str()),
            ("7", "Ani", "Director")
        );
        assert_eq!((i.resume_s, i.played), (90, true));
        let e = plex_ep(&json!({
            "ratingKey": "28", "title": "Episode 1", "index": 1, "parentIndex": 1, "parentTitle": "Season 1",
            "duration": 2_842_000, "viewOffset": 1_421_000, "thumb": "/library/metadata/28/thumb/1795"
        }))
        .unwrap();
        assert_eq!((e.id.as_str(), e.resume_s, e.progress), ("28", 1421, 0.5));
        assert_eq!(sound_name("dca", "ma", 8, false), "DTS-HD MA 7.1");
    }

    #[test]
    fn a_saved_page_is_read_back_and_kept_while_its_title_is_in_the_library() {
        let b = Backend::Jellyfin(crate::jellyfin::Config {
            server: String::new(),
            token: String::new(),
            server_id: String::new(),
            user_id: String::new(),
            user: String::new(),
        });
        let ep = |n: i64, played: bool| json!({ "Id": format!("e{n}"), "IndexNumber": n, "ParentIndexNumber": 1, "UserData": { "Played": played } });
        let v = json!({
            "item": { "Type": "Series", "Name": "Show" },
            "similar": { "Items": [{ "Id": "r1", "Name": "Other", "ImageTags": { "Primary": "t" } }] },
            "episodes": { "Items": [ep(1, true), ep(2, false)] },
            "next": { "MediaStreams": [{ "Type": "Audio", "Language": "fre", "Codec": "aac", "Channels": 2 }] }
        });
        assert_eq!(next_id(&b, &v).as_deref(), Some("e2"));
        let saved: Value = serde_json::from_str(&v.to_string()).unwrap();
        let got = parse(&b, &saved);
        assert_eq!(got, parse(&b, &v), "the saved copy draws the same page");
        let Got::Loaded(info, seasons, next) = got else {
            panic!()
        };
        assert_eq!((seasons[0].eps.len(), next), (2, (0, 1)));
        assert_eq!(info.related[0].id, "r1");
        assert_eq!(info.streams.audio, ["French (AAC Stereo)"]);
        assert_eq!(parse(&b, &json!({ "item": { "Type": "BoxSet" } })), Got::Elsewhere);

        let root = std::env::temp_dir().join(format!("tvl-page-{}", std::process::id()));
        let root = root.to_str().unwrap();
        std::fs::create_dir_all(format!("{root}{DIR}/cache")).unwrap();
        for id in ["keep", "gone"] {
            std::fs::write(page_file(root, "lib", id), "{}").unwrap();
        }
        crate::library::prune(root, "lib", &["keep".to_string()].into_iter().collect());
        assert!(std::path::Path::new(&page_file(root, "lib", "keep")).exists());
        assert!(!std::path::Path::new(&page_file(root, "lib", "gone")).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rows_scroll_to_keep_the_focus_in_view() {
        assert_eq!(row_x(0, 20, 100.0), 0.0);
        assert_eq!(row_x(5, 40, 100.0), 400.0);
        let most = 20.0 * 100.0 - (W - 2.0 * MARGIN);
        assert_eq!(row_x(19, 20, 100.0), most, "the last item ends at the margin");
        assert_eq!(row_x(3, 4, 100.0), 0.0, "a short row does not scroll");
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
