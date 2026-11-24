//! "Continue watching on Plex": what the signed-in Plex account is in the middle of, from its own servers.
//!
//! Signing in is Plex's PIN flow (the code is entered at plex.tv/link). The servers come from plex.tv; the list is
//! the Continue Watching hub of each server that answers, over the home network when it can. The list and its
//! pictures are cached like the Jellyfin row, so the row shows at once and when the server is asleep (the launcher
//! never wakes it). Opening an item hands its server and rating key to PlxNative, whose launch links are
//! documented in its docs/launch-links.md.
use crate::data::{write_private, Item, Kind, DIR};
use crate::jellyfin::{clip, Qc};
use crate::net;
use crate::sdl::{Color, Event};
use crate::youtube::{device_id, now, sleep_unless};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// The app that opens an item until the user picks another one in Settings.
pub const DEFAULT_APP: &str = "com.beb.plxnative";
const PLEX_TV: &str = "https://plex.tv";
const MAX_ITEMS: usize = 15;
const MAX_SERVERS: usize = 3;
/// Connections tried per server, each for at most this many seconds (a sleeping server never answers).
const MAX_TRIES: usize = 3;
const TRY_SECS: u64 = 5;

/// `<DIR>/plex.json` (mode 600): the account token and the id this TV gave itself when it signed in.
#[derive(Clone)]
pub struct Config {
    token: String,
    client_id: String,
    pub user: String,
}

impl Config {
    pub fn load(root: &str) -> Option<Config> {
        let v: Value = serde_json::from_slice(&std::fs::read(format!("{root}{DIR}/plex.json")).ok()?).ok()?;
        let f = |k: &str| v[k].as_str().map(String::from);
        Some(Config {
            token: f("token")?,
            client_id: f("clientId")?,
            user: f("user").unwrap_or_default(),
        })
    }

    fn save(&self, root: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(format!("{root}{DIR}"))?;
        let body = json!({ "token": self.token, "clientId": self.client_id, "user": self.user });
        write_private(&format!("{root}{DIR}/plex.json"), body.to_string().as_bytes())
    }
}

fn headers(client_id: &str, token: Option<&str>) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("Accept", "application/json".to_string()),
        ("X-Plex-Client-Identifier", client_id.to_string()),
        ("X-Plex-Product", "tvlauncher".into()),
        ("X-Plex-Version", env!("CARGO_PKG_VERSION").into()),
        ("X-Plex-Platform", "webOS".into()),
        ("X-Plex-Device-Name", "LG TV".into()),
    ];
    h.extend(token.map(|t| ("X-Plex-Token", t.to_string())));
    h
}

fn get(url: &str, h: &[(&str, String)], secs: u64) -> Result<Vec<u8>, String> {
    let r = net::send(url, None, h, secs)?;
    if r.status >= 400 {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(r.body)
}

fn get_json(url: &str, h: &[(&str, String)], secs: u64) -> Result<Value, String> {
    serde_json::from_slice(&get(url, h, secs)?).map_err(|e| format!("not JSON: {e}"))
}

/// Signing out deletes the login and the cache here. The token itself stays valid at Plex until it is removed
/// under Authorized Devices in the account's settings on plex.tv.
pub fn sign_out(root: &str) {
    let _ = std::fs::remove_file(format!("{root}{DIR}/plex.json"));
    let _ = std::fs::remove_file(cache_file(root));
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with("plex-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ---- sign-in -----------------------------------------------------------------------------------------------

/// Asks Plex for a code, shows it (`Qc::Code`, with the page to enter it on), and waits for the user to enter it
/// at plex.tv/link. Saves `plex.json` and says `Qc::Linked`. Stops when `cancel` is set.
pub fn pin_flow(root: String, wake: u32, cancel: Arc<AtomicBool>) -> Receiver<Qc> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let say = |q| {
            let _ = tx.send(q);
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
        };
        let run = || -> Result<Option<Config>, String> {
            let client_id = format!("tvlauncher-{}", device_id());
            let h = headers(&client_id, None);
            let r = net::send(&format!("{PLEX_TV}/api/v2/pins?strong=false"), Some("{}"), &h, 15)?;
            let v: Value = serde_json::from_slice(&r.body).map_err(|e| format!("not JSON: {e}"))?;
            let (Some(id), Some(code)) = (v["id"].as_u64(), v["code"].as_str()) else {
                return Err(format!("no code from Plex (HTTP {})", r.status));
            };
            say(Qc::Code {
                code: code.to_string(),
                server: "plex.tv/link".into(),
            });
            let deadline = now() + v["expiresIn"].as_u64().unwrap_or(900);
            while now() < deadline {
                if !sleep_unless(&cancel, 2) {
                    return Ok(None);
                }
                let pin = match get_json(&format!("{PLEX_TV}/api/v2/pins/{id}"), &h, 15) {
                    Ok(p) => p,
                    Err(e) if e == "HTTP 404" => break,
                    Err(_) => continue,
                };
                if let Some(token) = pin["authToken"].as_str().filter(|t| !t.is_empty()) {
                    let me = get_json(&format!("{PLEX_TV}/api/v2/user"), &headers(&client_id, Some(token)), 15)
                        .unwrap_or_default();
                    let user = ["title", "username", "friendlyName"]
                        .iter()
                        .find_map(|k| me[*k].as_str().filter(|s| !s.is_empty()))
                        .unwrap_or("Plex");
                    return Ok(Some(Config {
                        token: token.into(),
                        client_id,
                        user: user.into(),
                    }));
                }
            }
            Err("The code expired.".into())
        };
        match run() {
            Ok(Some(cfg)) => match cfg.save(&root) {
                Ok(()) => say(Qc::Linked),
                Err(e) => say(Qc::Failed(format!("Could not save the sign-in: {e}"))),
            },
            Ok(None) => {}
            Err(e) => {
                crate::sdl::log(&format!("plex sign-in: {e}"));
                say(Qc::Failed(e));
            }
        }
    });
    rx
}

// ---- the list ----------------------------------------------------------------------------------------------

/// One entry of the row, as plain data so it can cross threads and be saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// The server's `machineIdentifier`.
    pub server: String,
    pub rating_key: String,
    pub title: String,
    pub sub: String,
    pub progress: f32,
    pub card: Option<String>,
    pub bg: Option<String>,
}

impl Tile {
    /// `app` is the app that opens the item.
    pub fn item(&self, app: &str) -> Item {
        let mut it = Item::new(
            format!("plex:{}:{}", self.server, self.rating_key),
            app.to_string(),
            Kind::Watch,
            Some(self.title.clone()),
        );
        it.sub = Some(self.sub.clone()).filter(|s| !s.is_empty());
        it.bg = self.bg.clone();
        it.icons.extend(self.card.clone());
        it.progress = self.progress;
        it.cover = true;
        it.color = Color {
            r: 52,
            g: 44,
            b: 24,
            a: 255,
        };
        it.params = Some(json!({ "plex": { "server": self.server, "ratingKey": self.rating_key, "action": "open" } }));
        // webOS 6 hands a running app's launch parameters to nobody, so the app is closed first.
        it.restart = true;
        it
    }
}

fn cache_file(root: &str) -> String {
    format!("{root}{DIR}/cache/plex.json")
}

pub fn cached(root: &str) -> Vec<Tile> {
    let v: Value = std::fs::read(cache_file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some(Tile {
                server: o["server"].as_str()?.into(),
                rating_key: o["ratingKey"].as_str()?.into(),
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
/// `None` means the refresh failed (no server answered); the cached list stays.
pub fn spawn(root: String, cfg: Config, wake: u32) -> (Sender<()>, Receiver<Option<Vec<Tile>>>) {
    let (req_tx, req_rx) = channel::<()>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        while req_rx.recv().is_ok() {
            while req_rx.try_recv().is_ok() {}
            let list = refresh(&root, &cfg)
                .map_err(|e| crate::sdl::log(&format!("plex: {e}")))
                .ok();
            if res_tx.send(list).is_err() {
                break;
            }
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
        }
    });
    (req_tx, res_rx)
}

struct Server {
    id: String,
    token: String,
    urls: Vec<String>,
}

/// The addresses to try, best first: on the home network and plain HTTP (a server that requires secure
/// connections refuses it, and the next one is tried), then Plex's own https names; relays are never used,
/// because they are slow and metered.
fn urls(resource: &Value) -> Vec<String> {
    let mut c: Vec<&Value> = resource["connections"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["relay"] != true && c["IPv6"] != true)
        .collect();
    c.sort_by_key(|c| (c["local"] != true, c["protocol"] == "https"));
    let mut out: Vec<String> = vec![];
    for c in c {
        let u = match (c["protocol"].as_str(), c["address"].as_str(), c["port"].as_u64()) {
            (Some("http"), Some(a), Some(p)) => format!("http://{a}:{p}"),
            _ => match c["uri"].as_str() {
                Some(u) => u.trim_end_matches('/').to_string(),
                None => continue,
            },
        };
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out.truncate(MAX_TRIES);
    out
}

/// The servers of the account: its own first, then shared ones.
fn servers(cfg: &Config) -> Result<Vec<Server>, String> {
    let v = get_json(
        &format!("{PLEX_TV}/api/v2/resources?includeHttps=1&includeRelay=0&includeIPv6=0"),
        &headers(&cfg.client_id, Some(&cfg.token)),
        15,
    )?;
    let mut all: Vec<&Value> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["provides"]
                .as_str()
                .is_some_and(|p| p.split(',').any(|x| x == "server"))
        })
        .collect();
    all.sort_by_key(|r| r["owned"] != true);
    Ok(all
        .into_iter()
        .filter_map(|r| {
            Some(Server {
                id: r["clientIdentifier"].as_str()?.into(),
                token: r["accessToken"].as_str().unwrap_or(&cfg.token).into(),
                urls: urls(r),
            })
        })
        .take(MAX_SERVERS)
        .collect())
}

/// A tile before its pictures are fetched, with where they come from.
struct Entry {
    tile: Tile,
    last: u64,
    card_src: Option<String>,
    bg_src: Option<String>,
}

fn entry(server: &str, o: &Value) -> Option<Entry> {
    let episode = match o["type"].as_str()? {
        "episode" => true,
        "movie" => false,
        _ => return None,
    };
    let name = o["title"].as_str().unwrap_or("");
    let title = if episode {
        o["grandparentTitle"].as_str().unwrap_or(name)
    } else {
        name
    };
    let head = if episode {
        match (o["parentIndex"].as_i64(), o["index"].as_i64()) {
            (Some(s), Some(e)) => format!("S{s} · E{e} — {name}"),
            _ => name.to_string(),
        }
    } else {
        o["year"].as_i64().map(|y| y.to_string()).unwrap_or_default()
    };
    let sub = [head, clip(o["summary"].as_str().unwrap_or(""), 220)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let (pos, total) = (
        o["viewOffset"].as_f64().unwrap_or(0.0),
        o["duration"].as_f64().unwrap_or(0.0),
    );
    let s = |k: &str| o[k].as_str().filter(|s| !s.is_empty()).map(String::from);
    let rating_key = o["ratingKey"]
        .as_str()
        .map(String::from)
        .or_else(|| o["ratingKey"].as_u64().map(|n| n.to_string()))?;
    Some(Entry {
        tile: Tile {
            server: server.into(),
            rating_key,
            title: title.into(),
            sub,
            progress: if total > 0.0 {
                (pos / total).clamp(0.0, 1.0) as f32
            } else {
                0.0
            },
            card: None,
            bg: None,
        },
        last: o["lastViewedAt"].as_u64().unwrap_or(0),
        card_src: if episode {
            s("thumb").or_else(|| s("art"))
        } else {
            s("art").or_else(|| s("thumb"))
        },
        bg_src: s("grandparentArt").or_else(|| s("art")),
    })
}

fn entries(server: &str, body: &Value) -> Vec<Entry> {
    body["MediaContainer"]["Metadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| entry(server, o))
        .collect()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The picture is cached under a name that changes when Plex's picture does (its path ends in a timestamp).
/// Where a server's pictures come from: the address that answered, its token and its id.
struct Source<'a> {
    base: &'a str,
    token: &'a str,
    server: &'a str,
}

fn picture(root: &str, from: &Source, rk: &str, kind: &str, src: &str, size: (u32, u32)) -> Option<String> {
    let stamp: String = src
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    let path = format!(
        "{DIR}/cache/plex-{kind}-{}-{rk}-{stamp}.img",
        from.server.chars().take(8).collect::<String>()
    );
    if std::path::Path::new(&format!("{root}{path}")).exists() {
        return Some(path);
    }
    let url = format!(
        "{}/photo/:/transcode?width={}&height={}&minSize=1&upscale=1&url={}",
        from.base,
        size.0,
        size.1,
        encode(src)
    );
    match get(&url, &[("X-Plex-Token", from.token.to_string())], 20) {
        Ok(bytes) => std::fs::write(format!("{root}{path}"), bytes).ok().map(|()| path),
        Err(e) => {
            crate::sdl::log(&format!("plex picture {rk}: {e}"));
            None
        }
    }
}

fn refresh(root: &str, cfg: &Config) -> Result<Vec<Tile>, String> {
    std::fs::create_dir_all(format!("{root}{DIR}/cache")).map_err(|e| e.to_string())?;
    // (server, address that answered, entries)
    let mut found: Vec<(Server, String, Vec<Entry>)> = vec![];
    for server in servers(cfg)? {
        let h = [
            ("Accept", "application/json".to_string()),
            ("X-Plex-Token", server.token.clone()),
        ];
        let size = format!("X-Plex-Container-Start=0&X-Plex-Container-Size={MAX_ITEMS}");
        let hit = server.urls.iter().find_map(|base| {
            ["/hubs/continueWatching/items", "/library/onDeck"]
                .iter()
                .find_map(|p| get_json(&format!("{base}{p}?{size}"), &h, TRY_SECS).ok())
                .map(|v| (base.clone(), entries(&server.id, &v)))
        });
        match hit {
            Some((base, list)) => found.push((server, base, list)),
            None => crate::sdl::log(&format!(
                "plex: server {} did not answer",
                server.id.chars().take(8).collect::<String>()
            )),
        }
    }
    if found.is_empty() {
        return Err("no server answered".into());
    }
    let mut all: Vec<(usize, Entry)> = vec![];
    for (i, (_, _, list)) in found.iter_mut().enumerate() {
        all.extend(std::mem::take(list).into_iter().map(|e| (i, e)));
    }
    all.sort_by_key(|(_, e)| std::cmp::Reverse(e.last));
    all.truncate(MAX_ITEMS);
    let mut out = vec![];
    for (i, mut e) in all {
        let (server, base, _) = &found[i];
        let rk = e.tile.rating_key.clone();
        let from = Source {
            base,
            token: &server.token,
            server: &server.id,
        };
        let pic =
            |kind, src: &Option<String>, size| src.as_deref().and_then(|s| picture(root, &from, &rk, kind, s, size));
        e.tile.card = pic("c", &e.card_src, (640, 360));
        e.tile.bg = pic("b", &e.bg_src, (1920, 1080));
        out.push(e.tile);
    }
    save(root, &out);
    Ok(out)
}

/// Writes the list, and removes pictures that no entry uses any more.
fn save(root: &str, list: &[Tile]) {
    let items: Vec<Value> = list
        .iter()
        .map(|t| {
            json!({ "server": t.server, "ratingKey": t.rating_key, "title": t.title, "sub": t.sub,
                "progress": t.progress, "card": t.card, "bg": t.bg })
        })
        .collect();
    let _ = std::fs::write(cache_file(root), json!({ "items": items }).to_string());
    let used: Vec<&str> = list
        .iter()
        .flat_map(|t| [t.card.as_deref(), t.bg.as_deref()])
        .flatten()
        .collect();
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("plex-") && name.ends_with(".img") && !used.iter().any(|u| u.ends_with(&name)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episodes_and_movies_become_tiles() {
        let v = json!({ "MediaContainer": { "Metadata": [
            { "type": "episode", "ratingKey": "101", "title": "The Pilot", "grandparentTitle": "Some Show",
              "parentIndex": 1, "index": 2, "summary": "A  long\nstory.", "viewOffset": 600000, "duration": 2400000,
              "lastViewedAt": 1700000500, "thumb": "/library/metadata/101/thumb/1699", "art": "/library/metadata/101/art/5",
              "grandparentArt": "/library/metadata/100/art/7" },
            { "type": "movie", "ratingKey": 202, "title": "A Film", "year": 2024, "viewOffset": 0, "duration": 0,
              "thumb": "/library/metadata/202/thumb/1", "art": "/library/metadata/202/art/2" },
            { "type": "track", "ratingKey": "9", "title": "A Song" }
        ] } });
        let e = entries("srv", &v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].tile.title, "Some Show");
        assert_eq!(e[0].tile.sub, "S1 · E2 — The Pilot\nA long story.");
        assert!((e[0].tile.progress - 0.25).abs() < 1e-6);
        assert_eq!(e[0].card_src.as_deref(), Some("/library/metadata/101/thumb/1699"));
        assert_eq!(e[0].bg_src.as_deref(), Some("/library/metadata/100/art/7"));
        assert_eq!((e[0].last, e[1].last), (1700000500, 0));
        assert_eq!((e[1].tile.rating_key.as_str(), e[1].tile.sub.as_str()), ("202", "2024"));
        assert_eq!(e[1].card_src.as_deref(), Some("/library/metadata/202/art/2"));
        assert_eq!(e[1].tile.progress, 0.0);
    }

    #[test]
    fn the_item_carries_plxnatives_launch_link() {
        let t = Tile {
            server: "abc".into(),
            rating_key: "7".into(),
            title: "T".into(),
            sub: String::new(),
            progress: 0.0,
            card: None,
            bg: None,
        };
        let it = t.item("com.beb.plxnative");
        assert_eq!(it.key, "plex:abc:7");
        assert_eq!(
            it.params.unwrap(),
            json!({ "plex": { "server": "abc", "ratingKey": "7", "action": "open" } })
        );
        assert!(it.restart);
    }

    #[test]
    fn local_plain_http_is_tried_first_and_relays_never() {
        let r = json!({ "connections": [
            { "protocol": "https", "address": "1.2.3.4", "port": 32400, "uri": "https://1-2-3-4.x.plex.direct:32400", "local": false },
            { "protocol": "https", "address": "10.0.0.2", "port": 32400, "uri": "https://10-0-0-2.x.plex.direct:32400", "local": true },
            { "protocol": "http", "address": "10.0.0.2", "port": 32400, "uri": "http://10.0.0.2:32400", "local": true },
            { "protocol": "https", "address": "r", "port": 443, "uri": "https://relay.plex.tv", "local": false, "relay": true },
            { "protocol": "https", "address": "::1", "port": 1, "uri": "https://v6", "local": true, "IPv6": true }
        ] });
        assert_eq!(
            urls(&r),
            [
                "http://10.0.0.2:32400",
                "https://10-0-0-2.x.plex.direct:32400",
                "https://1-2-3-4.x.plex.direct:32400"
            ]
        );
    }

    #[test]
    fn urls_are_encoded_for_the_picture_request() {
        assert_eq!(
            encode("/library/metadata/1/art/2"),
            "%2Flibrary%2Fmetadata%2F1%2Fart%2F2"
        );
    }
}
