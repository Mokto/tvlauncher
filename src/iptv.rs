//! Live TV channels from the user's own source: an M3U playlist, or an Xtream Codes login (server, user name and
//! password, whose `player_api.php` lists the live channels and whose `xmltv.php` is the guide). An XMLTV guide is
//! read as a stream and only the next day of the playlist's channels is kept, so a guide of a hundred megabytes
//! never sits in memory. The channel list, the guide and the logos are cached like the other rows.
use crate::data::{write_private, DIR};
use crate::net;
use crate::sdl::Event;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{channel, Receiver, Sender};

/// The guide kept: programmes that start within this many seconds, at most `SHOWS` a channel, descriptions cut.
// ponytail: about 1 KB a programme, so a playlist of thousands of channels with a full guide holds tens of MB;
// keep less (or read descriptions from the cache on demand) if that shows on the TV.
const HORIZON: i64 = 24 * 3600;
const SHOWS: usize = 48;
const DESC: usize = 600;
/// The cached list is fetched again when it is older than this, the guide sooner.
const CHANNELS_MAX_AGE: u64 = 12 * 3600;
const GUIDE_MAX_AGE: u64 = 2 * 3600;

#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    M3u(String),
    Xtream { server: String, user: String, pass: String },
}

/// `<DIR>/iptv.json` (mode 600): where the channels come from, and the guide when it is not the source's own.
#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub source: Source,
    pub guide: Option<String>,
}

fn file(root: &str) -> String {
    format!("{root}{DIR}/iptv.json")
}

impl Config {
    pub fn load(root: &str) -> Option<Config> {
        let v: Value = serde_json::from_slice(&std::fs::read(file(root)).ok()?).ok()?;
        let f = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(String::from);
        let source = match f("m3u") {
            Some(url) => Source::M3u(url),
            None => Source::Xtream {
                server: f("server")?,
                user: f("user")?,
                pass: f("password")?,
            },
        };
        Some(Config {
            source,
            guide: f("guide"),
        })
    }

    pub fn save(&self, root: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(format!("{root}{DIR}"))?;
        let mut v = match &self.source {
            Source::M3u(url) => json!({ "m3u": url }),
            Source::Xtream { server, user, pass } => json!({ "server": server, "user": user, "password": pass }),
        };
        if let Some(g) = &self.guide {
            v["guide"] = json!(g);
        }
        write_private(&file(root), v.to_string().as_bytes())
    }

    /// The guide to read: the one set in Settings, else the Xtream server's own.
    fn guide_url(&self, from_playlist: Option<&str>) -> Option<String> {
        if let Some(g) = &self.guide {
            return Some(g.clone());
        }
        match &self.source {
            Source::Xtream { server, user, pass } => Some(format!(
                "{}/xmltv.php?username={}&password={}",
                server.trim_end_matches('/'),
                crate::plex::encode(user),
                crate::plex::encode(pass)
            )),
            Source::M3u(_) => from_playlist.map(String::from),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub group: String,
    pub url: String,
    pub logo: Option<String>,
    /// The guide's id for it (`tvg-id`), else its name.
    pub epg: String,
}

impl Channel {
    /// Stable identity for favourites: the stream address without its host (a provider moves hosts) and the name.
    pub fn key(&self) -> String {
        let path = self.url.split_once("://").map_or(self.url.as_str(), |(_, rest)| {
            rest.split_once('/').map_or(rest, |(_, p)| p)
        });
        format!("tv:{path}")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Show {
    pub title: String,
    pub start: i64,
    pub stop: i64,
    /// The episode's title (`sub-title`), the first `category` and the description; empty when the guide has none.
    pub sub: String,
    pub cat: String,
    pub desc: String,
}

pub type Guide = HashMap<String, Vec<Show>>;

/// A guide id without its feed (iptv-org's `TF1.fr@SD`), so it matches guides that list `TF1.fr`.
fn epg_key(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// A channel's programmes, by start.
pub fn shows<'a>(guide: &'a Guide, epg: &str) -> &'a [Show] {
    guide.get(epg_key(epg)).map_or(&[], Vec::as_slice)
}

/// What is on now on a channel and what follows, at `now` (seconds).
pub fn now_next<'a>(guide: &'a Guide, epg: &str, now: i64) -> (Option<&'a Show>, Option<&'a Show>) {
    let mut it = shows(guide, epg).iter().filter(|s| s.stop > now);
    let first = it.next();
    match first {
        Some(s) if s.start <= now => (Some(s), it.next()),
        _ => (None, first),
    }
}

/// The value of `key="..."` in an `#EXTINF` line.
fn attr<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key}=\""))? + key.len() + 2;
    let rest = &line[at..];
    Some(&rest[..rest.find('"')?]).filter(|v| !v.is_empty())
}

/// Reads an M3U playlist; also gives the guide it names in its header (`url-tvg` or `x-tvg-url`), if any.
pub fn parse_m3u(r: impl BufRead) -> (Vec<Channel>, Option<String>) {
    let (mut out, mut guide, mut info, mut group) = (vec![], None, None::<String>, None::<String>);
    for line in r.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.starts_with("#EXTM3U") {
            guide = attr(line, "url-tvg")
                .or_else(|| attr(line, "x-tvg-url"))
                .and_then(|g| g.split(',').next())
                .map(String::from);
        } else if line.starts_with("#EXTINF") {
            info = Some(line.to_string());
            group = None;
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            group = Some(g.trim().to_string());
        } else if !line.is_empty() && !line.starts_with('#') {
            let Some(inf) = info.take() else { continue };
            // ponytail: films and series in an Xtream playlist are told apart by their path only.
            if line.contains("/movie/") || line.contains("/series/") {
                continue;
            }
            // The name follows the first comma outside the quoted attributes.
            let mut quoted = false;
            let comma = inf.char_indices().find(|&(_, c)| {
                quoted ^= c == '"';
                c == ',' && !quoted
            });
            let name = comma.map_or("", |(i, _)| inf[i + 1..].trim());
            // Some generated playlists leave a broken attribute after the comma; their name is after the last one.
            let name = if name.contains('"') {
                inf.rsplit(',').next().unwrap_or("").trim()
            } else {
                name
            };
            let name = if name.is_empty() {
                attr(&inf, "tvg-name").unwrap_or(line)
            } else {
                name
            };
            out.push(Channel {
                name: name.to_string(),
                group: attr(&inf, "group-title")
                    .map(String::from)
                    .or_else(|| group.clone())
                    .unwrap_or_else(|| "Channels".into()),
                url: line.to_string(),
                logo: attr(&inf, "tvg-logo").map(String::from),
                epg: attr(&inf, "tvg-id").unwrap_or(name).to_string(),
            });
        }
    }
    (out, guide)
}

/// An Xtream server's live channels, in its own category order.
pub fn parse_xtream(server: &str, user: &str, pass: &str, cats: &Value, streams: &Value) -> Vec<Channel> {
    let server = server.trim_end_matches('/');
    let id = |v: &Value| match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    let names: HashMap<String, (usize, String)> = cats
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, c)| Some((id(&c["category_id"]), (i, c["category_name"].as_str()?.to_string()))))
        .collect();
    let mut out: Vec<(usize, Channel)> = streams
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let (order, group) = names
                .get(&id(&s["category_id"]))
                .cloned()
                .unwrap_or((usize::MAX, "Channels".into()));
            let name = s["name"].as_str()?.trim().to_string();
            Some((
                order,
                Channel {
                    url: format!(
                        "{server}/live/{}/{}/{}.ts",
                        crate::plex::encode(user),
                        crate::plex::encode(pass),
                        id(&s["stream_id"])
                    ),
                    logo: s["stream_icon"].as_str().filter(|l| !l.is_empty()).map(String::from),
                    epg: s["epg_channel_id"]
                        .as_str()
                        .filter(|e| !e.is_empty())
                        .unwrap_or(&name)
                        .to_string(),
                    name,
                    group,
                },
            ))
        })
        .collect();
    out.sort_by_key(|(order, _)| *order);
    out.into_iter().map(|(_, c)| c).collect()
}

/// Seconds since 1970 of an XMLTV time, `20261124150000 +0100`.
fn xmltv_time(s: &str) -> Option<i64> {
    let d = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, da, h, mi, se) = (
        d(0, 4)?,
        d(4, 6)?,
        d(6, 8)?,
        d(8, 10)?,
        d(10, 12)?,
        d(12, 14).unwrap_or(0),
    );
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + da - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let mut t = days * 86400 + h * 3600 + mi * 60 + se;
    if let Some(z) = s[14..].trim().strip_prefix(['+', '-']) {
        let off = z.get(0..2)?.parse::<i64>().ok()? * 3600 + z.get(2..4)?.parse::<i64>().ok()? * 60;
        t -= if s[14..].trim().starts_with('-') { -off } else { off };
    }
    Some(t)
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e < 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Reads an XMLTV guide as it arrives and keeps, for the channels in `wanted`, the programmes from the one on
/// at `now` on, for the next `HORIZON` (at most `SHOWS` each).
pub fn parse_xmltv(mut r: impl Read, wanted: &HashSet<String>, now: i64) -> Guide {
    let mut guide = Guide::new();
    let (mut buf, mut chunk) = (Vec::<u8>::new(), vec![0u8; 256 * 1024]);
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut used = 0;
        while let Some(s) = find(&buf[used..], b"<programme") {
            let start = used + s;
            let Some(e) = find(&buf[start..], b"</programme>") else {
                break;
            };
            let end = start + e + 12;
            if let Ok(p) = std::str::from_utf8(&buf[start..end]) {
                programme(p, wanted, now, &mut guide);
            }
            used = end;
        }
        // Keep only an unfinished programme; anything else before it (channel lists, icons) is dropped.
        let keep = find(&buf[used..], b"<programme").map_or(buf.len().saturating_sub(10), |i| used + i);
        buf.drain(..keep.max(used).min(buf.len()));
    }
    for shows in guide.values_mut() {
        shows.sort_by_key(|s| s.start);
    }
    guide
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn programme(p: &str, wanted: &HashSet<String>, now: i64, guide: &mut Guide) {
    let head = &p[..p.find('>').unwrap_or(p.len())];
    let Some(ch) = attr(head, "channel").map(|c| epg_key(&unescape(c)).to_string()) else {
        return;
    };
    if !wanted.contains(&ch) {
        return;
    }
    let (Some(start), Some(stop)) = (
        attr(head, "start").and_then(xmltv_time),
        attr(head, "stop").and_then(xmltv_time),
    ) else {
        return;
    };
    if stop <= now || start > now + HORIZON {
        return;
    }
    let Some(title) = tag(p, "title") else { return };
    let mut desc = tag(p, "desc").unwrap_or_default();
    if let Some((cut, _)) = desc.char_indices().nth(DESC) {
        desc.truncate(cut);
        desc.push('…');
    }
    let shows = guide.entry(ch).or_default();
    shows.push(Show {
        title,
        start,
        stop,
        sub: tag(p, "sub-title").unwrap_or_default(),
        cat: tag(p, "category").unwrap_or_default(),
        desc,
    });
    if shows.len() > SHOWS * 2 {
        shows.sort_by_key(|s| s.start);
        shows.truncate(SHOWS);
    }
}

/// The text of the first `<name ...>...</name>` in a programme.
fn tag(p: &str, name: &str) -> Option<String> {
    let after = &p[p.find(&format!("<{name}"))?..];
    let open = after.find('>')?;
    if after[..open].ends_with('/') {
        return None;
    }
    let close = after.find(&format!("</{name}>"))?;
    let mut t = after.get(open + 1..close)?.trim();
    if let Some(c) = t.strip_prefix("<![CDATA[").and_then(|c| c.strip_suffix("]]>")) {
        t = c;
    }
    Some(unescape(t)).filter(|t| !t.is_empty())
}

fn cache(root: &str, name: &str) -> String {
    format!("{root}{DIR}/cache/iptv-{name}.json")
}

fn age(path: &str) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(u64::MAX, |d| d.as_secs())
}

pub fn cached_channels(root: &str) -> Vec<Channel> {
    let v: Value = std::fs::read(cache(root, "channels"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let f = |k: &str| c[k].as_str().map(String::from);
            Some(Channel {
                name: f("name")?,
                group: f("group")?,
                url: f("url")?,
                logo: f("logo"),
                epg: f("epg")?,
            })
        })
        .collect()
}

pub fn cached_guide(root: &str) -> Guide {
    let v: Value = std::fs::read(cache(root, "guide"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v.as_object()
        .into_iter()
        .flatten()
        .map(|(k, shows)| {
            let shows = shows
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| {
                    let f = |i: usize| s[i].as_str().unwrap_or_default().to_string();
                    Some(Show {
                        title: s[0].as_str()?.into(),
                        start: s[1].as_i64()?,
                        stop: s[2].as_i64()?,
                        sub: f(3),
                        cat: f(4),
                        desc: f(5),
                    })
                })
                .collect();
            (k.clone(), shows)
        })
        .collect()
}

/// Removes the login, the cached lists and the logos.
pub fn sign_out(root: &str) {
    let _ = std::fs::remove_file(file(root));
    sign_out_cache(root);
}

/// Removes the cached lists and the logos (the source changed).
pub fn sign_out_cache(root: &str) {
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with("iptv-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub enum Got {
    Channels(Vec<Channel>),
    Guide(Guide),
    Failed(String),
    /// The refresh is over.
    Done,
}

/// A body read through the TV's `curl` (any scheme), decompressed by `gzip` when the address ends in `.gz`.
fn open(url: &str) -> Result<Box<dyn Read>, String> {
    let mut child = net::curl_stream(url).map_err(|e| format!("curl: {e}"))?;
    let out = child.stdout.take().ok_or("curl: no output")?;
    if url.split('?').next().is_some_and(|p| p.ends_with(".gz")) {
        let gz = std::process::Command::new("gzip")
            .arg("-dc")
            .stdin(out)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("gzip: {e}"))?;
        return Ok(Box::new(gz.stdout.ok_or("gzip: no output")?));
    }
    Ok(Box::new(out))
}

fn fetch_channels(cfg: &Config) -> Result<(Vec<Channel>, Option<String>), String> {
    match &cfg.source {
        Source::M3u(url) => {
            let (list, guide) = parse_m3u(BufReader::new(open(url)?));
            if list.is_empty() {
                return Err("the playlist has no channels".into());
            }
            Ok((list, guide))
        }
        Source::Xtream { server, user, pass } => {
            let api = |action: &str| -> Result<Value, String> {
                let url = format!(
                    "{}/player_api.php?username={}&password={}&action={action}",
                    server.trim_end_matches('/'),
                    crate::plex::encode(user),
                    crate::plex::encode(pass)
                );
                let r = net::curl(&url, "tvlauncher", &[], None, 60)?;
                if r.status >= 400 {
                    return Err(format!("HTTP {}", r.status));
                }
                serde_json::from_slice(&r.body).map_err(|_| "the server did not answer as Xtream does".into())
            };
            let list = parse_xtream(
                server,
                user,
                pass,
                &api("get_live_categories")?,
                &api("get_live_streams")?,
            );
            if list.is_empty() {
                return Err("the login has no live channels".into());
            }
            Ok((list, None))
        }
    }
}

fn save_channels(root: &str, list: &[Channel]) {
    let v: Vec<Value> = list
        .iter()
        .map(|c| json!({ "name": c.name, "group": c.group, "url": c.url, "logo": c.logo, "epg": c.epg }))
        .collect();
    let _ = std::fs::create_dir_all(format!("{root}{DIR}/cache"));
    let _ = write_private(&cache(root, "channels"), Value::from(v).to_string().as_bytes());
}

fn save_guide(root: &str, guide: &Guide) {
    let v: serde_json::Map<String, Value> = guide
        .iter()
        .map(|(k, shows)| {
            let s: Vec<Value> = shows
                .iter()
                .map(|s| json!([s.title, s.start, s.stop, s.sub, s.cat, s.desc]))
                .collect();
            (k.clone(), Value::from(s))
        })
        .collect();
    let _ = std::fs::write(cache(root, "guide"), Value::from(v).to_string());
}

fn refresh(root: &str, cfg: &Config, force: bool, send: &dyn Fn(Got) -> bool) {
    let mut list = cached_channels(root);
    let mut playlist_guide = None;
    if force || list.is_empty() || age(&cache(root, "channels")) > CHANNELS_MAX_AGE {
        match fetch_channels(cfg) {
            Ok((l, g)) => {
                crate::sdl::log(&format!("iptv: {} channels", l.len()));
                save_channels(root, &l);
                list = l;
                playlist_guide = g;
                send(Got::Channels(list.clone()));
            }
            Err(e) => {
                crate::sdl::log(&format!("iptv: {e}"));
                send(Got::Failed(e));
                return;
            }
        }
    }
    let Some(url) = cfg.guide_url(playlist_guide.as_deref()) else {
        return;
    };
    if !force && age(&cache(root, "guide")) <= GUIDE_MAX_AGE {
        return;
    }
    let wanted: HashSet<String> = list.iter().map(|c| epg_key(&c.epg).to_string()).collect();
    match open(&url) {
        Ok(r) => {
            let guide = parse_xmltv(r, &wanted, crate::youtube::now() as i64);
            crate::sdl::log(&format!("iptv: guide for {} channels", guide.len()));
            if !guide.is_empty() {
                save_guide(root, &guide);
                send(Got::Guide(guide));
            }
        }
        Err(e) => crate::sdl::log(&format!("iptv guide: {e}")),
    }
}

/// Starts the refresh thread. Send `true` to fetch everything now, `false` to fetch only what is older than its
/// maximum age. The channels, then the guide, arrive on the receiver, each followed by `wake`.
pub fn spawn(root: String, cfg: Config, wake: u32) -> (Sender<bool>, Receiver<Got>) {
    let (req_tx, req_rx) = channel::<bool>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        let send = |g: Got| {
            let ok = res_tx.send(g).is_ok();
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
            ok
        };
        while let Ok(mut force) = req_rx.recv() {
            while let Ok(f) = req_rx.try_recv() {
                force |= f;
            }
            refresh(&root, &cfg, force, &send);
            if !send(Got::Done) {
                return;
            }
        }
    });
    (req_tx, res_rx)
}

/// Where a logo is kept, named after its address.
pub fn logo_path(url: &str) -> String {
    let h = url.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{DIR}/cache/iptv-logo-{h:016x}.img")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_channels_groups_and_guide() {
        let m3u = "#EXTM3U url-tvg=\"http://g/epg.xml\"\n\
            #EXTINF:-1 tvg-id=\"one.uk\" tvg-logo=\"http://l/1.png\" group-title=\"News, World\",One, the news\n\
            http://p/live/u/p/1.ts\n\
            #EXTINF:-1,Two\n#EXTGRP:Sport\nhttp://p/2\n\
            #EXTINF:-1 group-title=\"Films\",A film\nhttp://p/movie/u/p/9.mkv\n\
            #EXTINF:-1 group-title=\"Music\",like Gecko) Chrome/1 Safari/5\" group-title=\"Music\",Trace (1080p)\nhttp://p/3\n";
        let (list, guide) = parse_m3u(m3u.as_bytes());
        assert_eq!(guide.as_deref(), Some("http://g/epg.xml"));
        assert_eq!(list.len(), 3, "films are left out");
        assert_eq!(list[2].name, "Trace (1080p)");
        assert_eq!(
            (list[0].name.as_str(), list[0].group.as_str(), list[0].epg.as_str()),
            ("One, the news", "News, World", "one.uk")
        );
        assert_eq!(list[0].logo.as_deref(), Some("http://l/1.png"));
        assert_eq!((list[1].name.as_str(), list[1].group.as_str()), ("Two", "Sport"));
        assert_eq!(list[0].key(), "tv:live/u/p/1.ts");
    }

    #[test]
    fn xtream_streams_in_category_order() {
        let cats =
            json!([{"category_id": "7", "category_name": "News"}, {"category_id": "3", "category_name": "Sport"}]);
        let streams = json!([
            {"name": "S1", "stream_id": 11, "category_id": "3", "epg_channel_id": "s1.de", "stream_icon": ""},
            {"name": "N1", "stream_id": 12, "category_id": "7", "epg_channel_id": null, "stream_icon": "http://i/n.png"}
        ]);
        let list = parse_xtream("http://x:80/", "me", "p&w", &cats, &streams);
        assert_eq!(list[0].name, "N1");
        assert_eq!(list[0].url, "http://x:80/live/me/p%26w/12.ts");
        assert_eq!((list[0].epg.as_str(), list[0].logo.is_some()), ("N1", true));
        assert_eq!((list[1].group.as_str(), list[1].epg.as_str()), ("Sport", "s1.de"));
    }

    #[test]
    fn xmltv_keeps_now_and_next_of_wanted_channels() {
        let now = xmltv_time("20261124153000 +0000").unwrap();
        assert_eq!(xmltv_time("20261124163000 +0100"), Some(now));
        assert_eq!(xmltv_time("19700101000000 +0000"), Some(0));
        let xml = r#"<?xml version="1.0"?><tv><channel id="a"><display-name>A</display-name></channel>
            <programme start="20261124140000 +0000" stop="20261124150000 +0000" channel="a"><title>Over</title></programme>
            <programme start="20261124150000 +0000" stop="20261124160000 +0000" channel="a"><title lang="en">News &amp; weather</title><sub-title>Late</sub-title><desc lang="en">Rain &lt;soon&gt;.</desc><category lang="en">News</category><category>Weather</category><icon src="x"/></programme>
            <programme start="20261124160000 +0000" stop="20261124170000 +0000" channel="a@HD"><title><![CDATA[Film]]></title></programme>
            <programme start="20261124150000 +0000" stop="20261124160000 +0000" channel="b"><title>Not wanted</title></programme>
            </tv>"#;
        // Read in small pieces, so programmes are cut across reads.
        struct Slow<'a>(&'a [u8]);
        impl Read for Slow<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(7).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let wanted = HashSet::from(["a".to_string()]);
        let guide = parse_xmltv(Slow(xml.as_bytes()), &wanted, now);
        assert_eq!(guide.len(), 1);
        let (on, next) = now_next(&guide, "a@SD", now);
        assert_eq!(on.map(|s| s.title.as_str()), Some("News & weather"));
        let on = on.unwrap();
        assert_eq!(
            (on.sub.as_str(), on.cat.as_str(), on.desc.as_str()),
            ("Late", "News", "Rain <soon>.")
        );
        assert_eq!(next.map(|s| s.title.as_str()), Some("Film"));
    }
}
