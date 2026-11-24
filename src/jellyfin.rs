//! "Continue watching" from a Jellyfin server over plain HTTP on the home network.
//!
//! The list and its pictures are cached on the TV's storage, so the row shows at once and also when the
//! server is asleep (the launcher never wakes it). A background thread refreshes them; opening an item
//! hands its id to Native Jelly (see `Luna::launch`).
use crate::data::{write_private, Item, Kind, DIR};
use crate::net;
use crate::sdl::{Color, Event};
use serde_json::{json, Value};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The app that opens an item until the user picks another one in Settings.
pub const DEFAULT_APP: &str = "com.sostk.nativejelly.nightly";
const CLIENT: &str = "Client=\"tvlauncher\", Device=\"LG C1\", DeviceId=\"tvlauncher-c1\", Version=\"0.1\"";

const CARD_WIDTH: u32 = 640;
const BG_WIDTH: u32 = 1920;
const MAX_ITEMS: usize = 15;
const FIELDS: &str = "Overview,ParentBackdropItemId,ParentBackdropImageTags,ParentThumbItemId,ParentThumbImageTag";

/// `<DIR>/jellyfin.json`, written when the user signs in (see `quick_connect`).
#[derive(Clone)]
pub struct Config {
    pub server: String,
    token: String,
    pub server_id: String,
    pub user_id: String,
    pub user: String,
}

fn auth(token: Option<&str>) -> String {
    match token {
        Some(t) => format!("MediaBrowser Token=\"{t}\", {CLIENT}"),
        None => format!("MediaBrowser {CLIENT}"),
    }
}

/// A request with a 6 s timeout: the server may be asleep, and the launcher never waits long for it.
fn request(server: &str, post: Option<&str>, path: &str, token: Option<&str>) -> Result<Vec<u8>, String> {
    let r = net::send(&format!("{server}{path}"), post, &[("Authorization", auth(token))], 6)?;
    if (200..300).contains(&r.status) {
        Ok(r.body)
    } else {
        Err(format!("HTTP {}", r.status))
    }
}

impl Config {
    pub fn load(root: &str) -> Option<Config> {
        let v: Value = serde_json::from_slice(&std::fs::read(format!("{root}{DIR}/jellyfin.json")).ok()?).ok()?;
        let f = |k: &str| v[k].as_str().map(String::from);
        Some(Config {
            server: f("server")?.trim_end_matches('/').to_string(),
            token: f("token")?,
            server_id: f("serverId")?,
            user_id: f("userId")?,
            user: f("user").unwrap_or_default(),
        })
    }

    /// Owner-only: the file holds the device token.
    fn save(&self, root: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(format!("{root}{DIR}"))?;
        let body = json!({ "server": self.server, "serverId": self.server_id, "userId": self.user_id, "user": self.user, "token": self.token });
        write_private(&format!("{root}{DIR}/jellyfin.json"), body.to_string().as_bytes())
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        request(&self.server, None, path, Some(&self.token))
    }

    /// `app` is the app that opens the item.
    pub fn item(&self, w: &Watch, app: &str) -> Item {
        let mut it = Item::new(
            format!("jf:{}", w.id),
            app.to_string(),
            Kind::Watch,
            Some(w.title.clone()),
        );
        it.sub = Some(w.sub.clone()).filter(|s| !s.is_empty());
        it.bg = w.bg.clone();
        it.icons.extend(w.card.clone());
        it.progress = w.progress;
        it.cover = true;
        it.color = Color {
            r: 40,
            g: 46,
            b: 58,
            a: 255,
        };
        it.params = Some(json!({ "itemId": w.id, "userId": self.user_id, "serverId": self.server_id }));
        it
    }
}

/// Signing out: the device token is revoked on the server (best effort, in the background) and the
/// login and cached list are deleted here.
pub fn sign_out(root: &str, cfg: &Config) {
    let cfg = cfg.clone();
    std::thread::spawn(move || drop(request(&cfg.server, Some("{}"), "/Sessions/Logout", Some(&cfg.token))));
    let _ = std::fs::remove_file(format!("{root}{DIR}/jellyfin.json"));
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if is_ours(&e.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The cache holds YouTube's files too: these are Jellyfin's.
fn is_ours(name: &str) -> bool {
    name == "watching.json" || name == "backdrops.json" || ["card-", "bg-", "home-"].iter().any(|p| name.starts_with(p))
}

/// What the sign-in thread tells the dialog.
pub enum Qc {
    /// Show this code; the user enters it in a Jellyfin app that is signed in.
    Code {
        code: String,
        server: String,
    },
    /// No server was found or the address given does not answer.
    NeedAddress(String),
    Failed(String),
    Done(Config),
    /// YouTube's sign-in finished (see `youtube::device_flow`).
    Linked,
}

/// The base URLs to try for what the user typed. `https://host` is used as typed (port 443, or the one given).
/// `http://host` and a bare `host` get Jellyfin's port 8096 unless one is given; a bare host is also tried as
/// `https://host`, since a server behind a reverse proxy is usually that.
fn candidates(addr: &str) -> Vec<String> {
    let a = addr.trim().trim_end_matches('/');
    if a.starts_with("https://") {
        return vec![a.to_string()];
    }
    let rest = a.strip_prefix("http://").unwrap_or(a);
    let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
    let tail = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    let http = if host.contains(':') {
        format!("http://{host}{tail}")
    } else {
        format!("http://{host}:8096{tail}")
    };
    if a.starts_with("http://") {
        vec![http]
    } else {
        vec![http, format!("https://{host}{tail}")]
    }
}

/// The base URL and name of a Jellyfin server that answers at `addr`.
fn probe(addr: &str) -> Option<(String, String)> {
    for url in candidates(addr) {
        let r = match net::send(&format!("{url}/System/Info/Public"), None, &[], 4) {
            Ok(r) if r.status == 200 => r,
            Ok(r) => {
                crate::sdl::log(&format!("jellyfin {url}: HTTP {}", r.status));
                continue;
            }
            Err(e) => {
                crate::sdl::log(&format!("jellyfin {url}: {e}"));
                continue;
            }
        };
        if let Some(v) = serde_json::from_slice::<Value>(&r.body)
            .ok()
            .filter(|v| v["Id"].is_string())
        {
            return Some((url, v["ServerName"].as_str().unwrap_or("Jellyfin").to_string()));
        }
    }
    None
}

/// Jellyfin answers a UDP broadcast on port 7359 with its address. A server behind a port proxy (as in a
/// WSL or Docker setup) may not receive it; the dialog then asks for the address.
fn discover() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.set_broadcast(true).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    let mut buf = [0u8; 1024];
    for _ in 0..2 {
        s.send_to(b"Who is JellyfinServer?", "255.255.255.255:7359").ok()?;
        if let Ok(n) = s.recv(&mut buf) {
            let v: Value = serde_json::from_slice(&buf[..n]).ok()?;
            return v["Address"].as_str().map(String::from);
        }
    }
    None
}

/// Signs in with Jellyfin's Quick Connect, the way its own apps do: ask the server for a code, show it,
/// and wait until the user enters it in a Jellyfin app that is already signed in. The thread tries `server`
/// first, then (with `discover`) the network. It stops when `cancel` is set, and saves `jellyfin.json` on success.
pub fn quick_connect(
    root: String,
    wake: u32,
    server: Option<String>,
    discover_too: bool,
    cancel: Arc<AtomicBool>,
) -> Receiver<Qc> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let send = |q| {
            let _ = tx.send(q);
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
        };
        match connect(&root, server, discover_too, &cancel, &send) {
            Ok(Some(cfg)) => send(Qc::Done(cfg)),
            Ok(None) => {}
            Err(q) => send(q),
        }
    });
    rx
}

fn connect(
    root: &str,
    server: Option<String>,
    discover_too: bool,
    cancel: &AtomicBool,
    send: &dyn Fn(Qc),
) -> Result<Option<Config>, Qc> {
    let failed = |e: String| Qc::Failed(e);
    let mut found = server.as_deref().and_then(probe);
    if found.is_none() && discover_too {
        found = discover().and_then(|a| probe(&a));
    }
    let Some((url, name)) = found else {
        return Err(Qc::NeedAddress(match server {
            Some(s) => format!("Could not reach {s}."),
            None => "No Jellyfin server found on the network.".into(),
        }));
    };
    let parse = |b: Vec<u8>| serde_json::from_slice::<Value>(&b).map_err(|e| failed(e.to_string()));
    let init = request(&url, Some("{}"), "/QuickConnect/Initiate", None).map_err(|e| {
        failed(if e == "HTTP 401" {
            "Quick Connect is switched off on the server (Dashboard, General).".into()
        } else {
            e
        })
    })?;
    let init = parse(init)?;
    let (Some(secret), Some(code)) = (init["Secret"].as_str(), init["Code"].as_str()) else {
        return Err(failed("The server sent no code.".into()));
    };
    send(Qc::Code {
        code: code.into(),
        server: name,
    });
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut errors = 0;
    loop {
        for _ in 0..15 {
            if cancel.load(Ordering::SeqCst) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if Instant::now() > deadline {
            return Err(failed("The code expired.".into()));
        }
        match request(&url, None, &format!("/QuickConnect/Connect?secret={secret}"), None) {
            Ok(b) => {
                errors = 0;
                if parse(b)?["Authenticated"] == true {
                    break;
                }
            }
            Err(e) if e == "HTTP 404" => return Err(failed("The code expired.".into())),
            Err(e) => {
                errors += 1;
                if errors >= 5 {
                    return Err(failed(e));
                }
            }
        }
    }
    let body = json!({ "Secret": secret }).to_string();
    let r = parse(request(&url, Some(&body), "/Users/AuthenticateWithQuickConnect", None).map_err(failed)?)?;
    let f = |v: &Value| v.as_str().map(String::from);
    let (Some(token), Some(user_id), Some(server_id)) = (f(&r["AccessToken"]), f(&r["User"]["Id"]), f(&r["ServerId"]))
    else {
        return Err(failed("The server sent no login.".into()));
    };
    let cfg = Config {
        server: url,
        token,
        server_id,
        user_id,
        user: f(&r["User"]["Name"]).unwrap_or_default(),
    };
    cfg.save(root)
        .map_err(|e| failed(format!("Could not save the login: {e}")))?;
    Ok(Some(cfg))
}

/// One entry of the row, as plain data so it can cross threads and be saved.
#[derive(Clone, PartialEq)]
pub struct Watch {
    pub id: String,
    pub title: String,
    pub sub: String,
    pub progress: f32,
    pub card: Option<String>,
    pub bg: Option<String>,
}

fn cache_file(root: &str) -> String {
    format!("{root}{DIR}/cache/watching.json")
}

pub fn cached(root: &str) -> Vec<Watch> {
    let v: Value = std::fs::read(cache_file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some(Watch {
                id: o["id"].as_str()?.into(),
                title: o["title"].as_str()?.into(),
                sub: o["sub"].as_str().unwrap_or("").into(),
                progress: o["progress"].as_f64().unwrap_or(0.0) as f32,
                card: o["card"].as_str().map(String::from),
                bg: o["bg"].as_str().map(String::from),
            })
        })
        .collect()
}

/// Starts the refresh thread: send `()` to refresh; a fresh list arrives on the receiver and `wake` is pushed.
/// `None` means the refresh failed (the server is asleep or unreachable); the cached list stays.
pub fn spawn(root: String, cfg: Config, wake: u32) -> (Sender<()>, Receiver<Option<Vec<Watch>>>) {
    let (req_tx, req_rx) = channel::<()>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        while req_rx.recv().is_ok() {
            while req_rx.try_recv().is_ok() {}
            let list = refresh(&root, &cfg)
                .map_err(|e| crate::sdl::log(&format!("jellyfin: {e}")))
                .ok();
            if res_tx.send(list).is_err() {
                break;
            }
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
            if fetch_backdrops(&root, &cfg) && res_tx.send(None).is_ok() {
                unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
            }
        }
    });
    (req_tx, res_rx)
}

/// How long the saved set of background pictures is used before a new random set is asked for.
const BACKDROP_AGE: u64 = 12 * 3600;
const BACKDROPS: usize = 12;

fn backdrop_file(root: &str) -> String {
    format!("{root}{DIR}/cache/backdrops.json")
}

/// A picture that can be the background of Home, with what the hero area says about its movie or series.
#[derive(Clone, PartialEq, Debug)]
pub struct Backdrop {
    pub path: String,
    pub title: String,
    pub sub: String,
}

/// The pictures that can be the background of Home: random movie and series backdrops that are on disk.
pub fn backdrops(root: &str) -> Vec<Backdrop> {
    let v: Value = std::fs::read(backdrop_file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let text = |k: &str| o[k].as_str().unwrap_or("").to_string();
            Some(Backdrop {
                path: o["path"].as_str()?.to_string(),
                title: text("title"),
                sub: text("sub"),
            })
        })
        .filter(|b| std::path::Path::new(&format!("{root}{}", b.path)).exists())
        .collect()
}

/// Picks random movies and series that have a backdrop and saves the pictures; true when the set changed.
fn fetch_backdrops(root: &str, cfg: &Config) -> bool {
    let fresh = std::fs::metadata(backdrop_file(root))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
        .is_some_and(|a| a.as_secs() < BACKDROP_AGE);
    // A set saved before the hero text existed has no titles: fetch it again.
    if fresh && backdrops(root).iter().any(|b| !b.title.is_empty()) {
        return false;
    }
    let q = format!(
        "/Users/{}/Items?IncludeItemTypes=Movie,Series&Recursive=true&SortBy=Random&Limit={}&ImageTypes=Backdrop&Fields=BackdropImageTags,Overview,ProductionYear&EnableImageTypes=Backdrop&ImageTypeLimit=1&EnableTotalRecordCount=false",
        cfg.user_id,
        BACKDROPS * 2
    );
    let list = match cfg
        .get(&q)
        .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            crate::sdl::log(&format!("jellyfin backdrops: {e}"));
            return false;
        }
    };
    let mut found: Vec<Value> = vec![];
    for o in list["Items"].as_array().into_iter().flatten() {
        if found.len() >= BACKDROPS {
            break;
        }
        let (Some(id), Some(tag)) = (o["Id"].as_str(), tag0(o, "BackdropImageTags")) else {
            continue;
        };
        let img = Img {
            item: id.into(),
            kind: "Backdrop",
            tag,
        };
        let Some(path) = fetch_img(root, cfg, &img, "home", BG_WIDTH) else {
            continue;
        };
        let year = o["ProductionYear"].as_i64().map(|y| y.to_string()).unwrap_or_default();
        let sub = [year, clip(o["Overview"].as_str().unwrap_or(""), 200)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        found.push(json!({ "path": path, "title": o["Name"].as_str().unwrap_or(""), "sub": sub }));
    }
    if found.is_empty() {
        return false;
    }
    let paths: Vec<String> = found
        .iter()
        .filter_map(|f| f["path"].as_str().map(String::from))
        .collect();
    let _ = std::fs::write(backdrop_file(root), json!(found).to_string());
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("home-") && !paths.iter().any(|p| p.ends_with(&name)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    true
}

struct Img {
    item: String,
    kind: &'static str,
    tag: String,
}

fn tag0(o: &Value, k: &str) -> Option<String> {
    o[k].as_array()?.first()?.as_str().map(String::from)
}

fn tag(o: &Value, k: &str) -> Option<String> {
    o["ImageTags"][k].as_str().map(String::from)
}

/// A 16:9 still for the card: the episode's own picture, else a thumb or backdrop, else the poster.
fn card_img(o: &Value) -> Option<Img> {
    let id = o["Id"].as_str()?;
    let own = |kind, tag: Option<String>| {
        tag.map(|tag| Img {
            item: id.into(),
            kind,
            tag,
        })
    };
    let parent = |kind, id: &str, tag: Option<String>| {
        tag.map(|tag| Img {
            item: id.into(),
            kind,
            tag,
        })
    };
    let episode = o["Type"] == "Episode";
    (if episode {
        own("Primary", tag(o, "Primary"))
    } else {
        None
    })
    .or_else(|| own("Thumb", tag(o, "Thumb")))
    .or_else(|| own("Backdrop", tag0(o, "BackdropImageTags")))
    .or_else(|| own("Primary", tag(o, "Primary")))
    .or_else(|| {
        parent(
            "Thumb",
            o["ParentThumbItemId"].as_str()?,
            o["ParentThumbImageTag"].as_str().map(String::from),
        )
    })
    .or_else(|| {
        parent(
            "Backdrop",
            o["ParentBackdropItemId"].as_str()?,
            tag0(o, "ParentBackdropImageTags"),
        )
    })
}

fn bg_img(o: &Value) -> Option<Img> {
    let id = o["Id"].as_str()?;
    if let Some(tag) = tag0(o, "BackdropImageTags") {
        return Some(Img {
            item: id.into(),
            kind: "Backdrop",
            tag,
        });
    }
    let parent = o["ParentBackdropItemId"].as_str()?;
    Some(Img {
        item: parent.into(),
        kind: "Backdrop",
        tag: tag0(o, "ParentBackdropImageTags")?,
    })
}

/// Downloads the picture unless it is already cached; returns its path (without the root).
fn fetch_img(root: &str, cfg: &Config, img: &Img, prefix: &str, width: u32) -> Option<String> {
    let path = format!(
        "{DIR}/cache/{prefix}-{}-{}.img",
        img.item,
        img.tag.chars().take(8).collect::<String>()
    );
    if std::path::Path::new(&format!("{root}{path}")).exists() {
        return Some(path);
    }
    let sub = if img.kind == "Backdrop" { "Backdrop/0" } else { img.kind };
    let url = format!(
        "/Items/{}/Images/{sub}?maxWidth={width}&quality=80&tag={}",
        img.item, img.tag
    );
    match cfg.get(&url) {
        Ok(bytes) => std::fs::write(format!("{root}{path}"), bytes).ok().map(|()| path),
        Err(e) => {
            crate::sdl::log(&format!("jellyfin image {}: {e}", img.item));
            None
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a))
}

fn watch(root: &str, cfg: &Config, o: &Value) -> Option<Watch> {
    let id = o["Id"].as_str()?.to_string();
    let name = o["Name"].as_str().unwrap_or("");
    let episode = o["Type"] == "Episode";
    let title = if episode {
        o["SeriesName"].as_str().unwrap_or(name)
    } else {
        name
    }
    .to_string();
    let head = if episode {
        match (o["ParentIndexNumber"].as_i64(), o["IndexNumber"].as_i64()) {
            (Some(s), Some(e)) => format!("S{s} · E{e} — {name}"),
            _ => name.to_string(),
        }
    } else {
        o["ProductionYear"].as_i64().map(|y| y.to_string()).unwrap_or_default()
    };
    let overview = clip(o["Overview"].as_str().unwrap_or(""), 220);
    let sub = [head, overview]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let ud = &o["UserData"];
    let progress = ud["PlayedPercentage"].as_f64().map(|p| p / 100.0).unwrap_or_else(|| {
        let (pos, total) = (
            ud["PlaybackPositionTicks"].as_f64().unwrap_or(0.0),
            o["RunTimeTicks"].as_f64().unwrap_or(0.0),
        );
        if total > 0.0 {
            pos / total
        } else {
            0.0
        }
    });
    Some(Watch {
        card: card_img(o).and_then(|i| fetch_img(root, cfg, &i, "card", CARD_WIDTH)),
        bg: bg_img(o).and_then(|i| fetch_img(root, cfg, &i, "bg", BG_WIDTH)),
        id,
        title,
        sub,
        progress: progress.clamp(0.0, 1.0) as f32,
    })
}

fn refresh(root: &str, cfg: &Config) -> Result<Vec<Watch>, String> {
    std::fs::create_dir_all(format!("{root}{DIR}/cache")).map_err(|e| e.to_string())?;
    let q = format!("Limit={MAX_ITEMS}&Fields={FIELDS}&EnableImageTypes=Primary,Backdrop,Thumb&ImageTypeLimit=1&EnableTotalRecordCount=false");
    let parse = |b: Vec<u8>| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string());
    let resume = parse(cfg.get(&format!("/Users/{}/Items/Resume?MediaTypes=Video&{q}", cfg.user_id))?)?;
    let next = parse(cfg.get(&format!("/Shows/NextUp?UserId={}&{q}", cfg.user_id))?)?;
    let mut seen_series: Vec<String> = vec![];
    let mut out = vec![];
    let resumed = resume["Items"].as_array().into_iter().flatten().map(|o| (o, true));
    let upnext = next["Items"].as_array().into_iter().flatten().map(|o| (o, false));
    for (o, resumed) in resumed.chain(upnext) {
        let series = o["SeriesId"].as_str().map(String::from);
        if out.len() >= MAX_ITEMS || series.as_ref().is_some_and(|s| !resumed && seen_series.contains(s)) {
            continue;
        }
        seen_series.extend(series);
        out.extend(watch(root, cfg, o));
    }
    save(root, &out);
    Ok(out)
}

/// Writes the list, and removes pictures that no entry uses any more.
fn save(root: &str, list: &[Watch]) {
    let items: Vec<Value> = list
        .iter()
        .map(|w| json!({ "id": w.id, "title": w.title, "sub": w.sub, "progress": w.progress, "card": w.card, "bg": w.bg }))
        .collect();
    let _ = std::fs::write(cache_file(root), json!({ "items": items }).to_string());
    let used: Vec<&str> = list
        .iter()
        .flat_map(|w| [w.card.as_deref(), w.bg.as_deref()])
        .flatten()
        .collect();
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_ours(&name)
            && !name.starts_with("home-")
            && name.ends_with(".img")
            && !used.iter().any(|u| u.ends_with(&name))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            server: "http://127.0.0.1:1".into(),
            token: "t".into(),
            server_id: "s".into(),
            user_id: "u".into(),
            user: "n".into(),
        }
    }

    #[test]
    fn episode_uses_series_title_and_its_own_picture() {
        let o = json!({
            "Id": "e1", "Type": "Episode", "Name": "Episode 8", "SeriesName": "The Bridge", "ParentIndexNumber": 1, "IndexNumber": 8,
            "Overview": "A  long\nstory", "RunTimeTicks": 100.0, "UserData": { "PlaybackPositionTicks": 25.0 },
            "ImageTags": { "Primary": "abcdef123456" }, "BackdropImageTags": [],
            "ParentBackdropItemId": "series", "ParentBackdropImageTags": ["bd"]
        });
        let card = card_img(&o).unwrap();
        assert_eq!(
            (card.item.as_str(), card.kind, card.tag.as_str()),
            ("e1", "Primary", "abcdef123456")
        );
        let bg = bg_img(&o).unwrap();
        assert_eq!(
            (bg.item.as_str(), bg.kind, bg.tag.as_str()),
            ("series", "Backdrop", "bd")
        );
        let w = watch("/nonexistent", &cfg(), &o).unwrap();
        assert_eq!(
            (w.title.as_str(), w.sub.as_str(), w.progress),
            ("The Bridge", "S1 · E8 — Episode 8\nA long story", 0.25)
        );
        assert_eq!(cfg().item(&w, "app").params.unwrap()["itemId"], "e1");
    }

    #[test]
    fn movie_prefers_thumb_then_backdrop() {
        let o = json!({ "Id": "m1", "Type": "Movie", "ImageTags": { "Primary": "p", "Thumb": "t" }, "BackdropImageTags": ["b"] });
        assert_eq!(card_img(&o).unwrap().kind, "Thumb");
        let o = json!({ "Id": "m1", "Type": "Movie", "ImageTags": { "Primary": "p" }, "BackdropImageTags": ["b"] });
        assert_eq!(card_img(&o).unwrap().kind, "Backdrop");
    }

    #[test]
    fn only_backdrops_that_are_on_disk_can_be_the_background() {
        let root = std::env::temp_dir().join(format!("tvl-bd-{}", std::process::id()));
        let cache = root.join(DIR.trim_start_matches('/')).join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("home-a-1.img"), b"x").unwrap();
        let (have, gone) = (format!("{DIR}/cache/home-a-1.img"), format!("{DIR}/cache/home-b-2.img"));
        let list =
            json!([{ "path": have, "title": "A", "sub": "2020\nText" }, { "path": gone, "title": "B", "sub": "" }]);
        std::fs::write(cache.join("backdrops.json"), list.to_string()).unwrap();
        let root_s = root.to_string_lossy().into_owned();
        let got = backdrops(&root_s);
        assert_eq!(
            (got.len(), got[0].path.as_str(), got[0].title.as_str()),
            (1, have.as_str(), "A")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clip_cuts_at_a_word() {
        assert_eq!(clip("one two three four", 9), "one two…");
        assert_eq!(clip("short", 9), "short");
    }

    #[test]
    fn addresses_get_a_scheme_and_the_default_port() {
        assert_eq!(candidates("http://nas.local"), ["http://nas.local:8096"]);
        assert_eq!(candidates(" http://192.168.1.10:8920/ "), ["http://192.168.1.10:8920"]);
        assert_eq!(
            candidates("192.168.1.10"),
            ["http://192.168.1.10:8096", "https://192.168.1.10"]
        );
        assert_eq!(candidates("https://jf.example.com/"), ["https://jf.example.com"]);
        assert_eq!(
            candidates("https://jf.example.com:8920"),
            ["https://jf.example.com:8920"]
        );
        assert_eq!(
            candidates("host/jellyfin"),
            ["http://host:8096/jellyfin", "https://host/jellyfin"]
        );
    }

    /// Needs a server reachable over HTTPS: `JF_HTTPS=https://jellyfin.example.com cargo test --release -- --ignored probe_https`.
    #[test]
    #[ignore]
    fn probe_https() {
        let addr = std::env::var("JF_HTTPS").expect("JF_HTTPS");
        let host = addr.trim_start_matches("https://");
        for typed in [addr.as_str(), host] {
            let (url, name) = probe(typed).expect("server");
            assert!(url.starts_with("https://"), "{url}");
            println!("{typed} -> {url} ({name})");
        }
    }

    /// Needs the real server and a login to authorize with: `JF_FIXTURES=<dir with media/developer/tvlauncher/jellyfin.json>
    /// cargo test --release -- --ignored quick_connect`. The device id is fixed, so signing in again replaces that
    /// login on the server and signing out revokes the new one: sign in again afterwards.
    #[test]
    #[ignore]
    fn quick_connect_end_to_end() {
        let fx = std::env::var("JF_FIXTURES").expect("JF_FIXTURES");
        let me = Config::load(&fx).expect("a signed-in jellyfin.json in JF_FIXTURES");
        let root = format!("{}/qc-test-{}", std::env::temp_dir().display(), std::process::id());
        let cancel = Arc::new(AtomicBool::new(false));
        let rx = quick_connect(root.clone(), 0, Some(me.server.clone()), false, cancel.clone());
        let Ok(Qc::Code { code, .. }) = rx.recv_timeout(Duration::from_secs(10)) else {
            panic!("no code")
        };
        request(
            &me.server,
            Some("{}"),
            &format!("/QuickConnect/Authorize?code={code}"),
            Some(&me.token),
        )
        .unwrap();
        let Ok(Qc::Done(cfg)) = rx.recv_timeout(Duration::from_secs(20)) else {
            panic!("not signed in")
        };
        assert_eq!(cfg.user_id, me.user_id);
        assert_eq!(Config::load(&root).unwrap().token, cfg.token);
        let mode = std::fs::metadata(format!("{root}{DIR}/jellyfin.json")).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o600
        );
        sign_out(&root, &cfg);
        std::thread::sleep(Duration::from_secs(1));
        assert!(Config::load(&root).is_none());
        assert!(cfg.get("/Users/Me").is_err(), "the token was revoked on the server");
        let _ = std::fs::remove_dir_all(root);
    }
}
