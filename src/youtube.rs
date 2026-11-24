//! "Recommended on YouTube": the home feed of YouTube's own TV app, for the account the user signs in with.
//!
//! Unofficial, the way SmartTube does it (https://github.com/yuliskov/MediaServiceCore): the sign-in is the
//! device-code flow of YouTube's TV app, whose OAuth client is read from youtube.com/tv at sign-in, and the
//! feed is `youtubei/v1/browse` asked as the TVHTML5 client. YouTube can change either at any time; when the
//! answer is not understood the row simply stays as it was. Nothing is played here: opening a card hands the
//! video id to the TV's YouTube app. HTTPS goes through the TV's `curl`, which keeps TLS out of this binary.
use crate::data::{write_private, Item, Kind, DIR};
use crate::jellyfin::Qc;
use crate::net;
use crate::sdl::{Color, Event};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The TV's YouTube app (the webosbrew ad-free build uses the same id).
pub const APP: &str = "youtube.leanback.v4";
const UA: &str = "Mozilla/5.0 (Linux armeabi-v7a; Android 7.1.2; Fire OS 6.0) Cobalt/22.lts.3.306369-gold (unlike Gecko) v8/8.8.278.8-jit gles Starboard/13, Amazon_ATV_mediatek8695_2019/NS6294 (Amazon, AFTMM, Wireless) com.amazon.firetv.youtube/22.3.r2.v66.0";
const PAGE_UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version";
/// Used only when youtube.com/tv does not say which version it is.
const FALLBACK_VERSION: &str = "7.20261122.19.00";
const SCOPE: &str = "http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content";
const GRANT_DEVICE: &str = "http://oauth.net/grant_type/device/1.0";
const MAX_ITEMS: usize = 20;
/// The feed is asked for at most this often, however often Home comes to the front.
const MIN_AGE: u64 = 30 * 60;

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- HTTPS through curl ------------------------------------------------------------------------------------

/// The body of the answer whatever its status, or what curl said when there was no answer. With `fail`, an
/// HTTP error is an error.
fn curl(url: &str, ua: &str, headers: &[(&str, String)], body: Option<&str>, fail: bool) -> Result<Vec<u8>, String> {
    let r = net::curl(url, ua, headers, body, 20)?;
    if fail && r.status >= 400 {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(r.body)
}

fn json_post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value, String> {
    let mut h = vec![("Content-Type", "application/json".to_string())];
    h.extend(headers.iter().map(|(k, v)| (*k, v.clone())));
    let out = curl(url, UA, &h, Some(&body.to_string()), false)?;
    serde_json::from_slice(&out).map_err(|e| format!("not JSON: {e}"))
}

// ---- the saved sign-in -------------------------------------------------------------------------------------

/// `<DIR>/youtube.json` (mode 600): the TV app's OAuth client and the tokens it issued to this TV.
#[derive(Clone, Default)]
pub struct Config {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    access_token: String,
    expires_at: u64,
}

impl Config {
    pub fn load(root: &str) -> Option<Config> {
        let v: Value = serde_json::from_slice(&std::fs::read(format!("{root}{DIR}/youtube.json")).ok()?).ok()?;
        let f = |k: &str| v[k].as_str().map(String::from);
        Some(Config {
            client_id: f("clientId")?,
            client_secret: f("clientSecret")?,
            refresh_token: f("refreshToken")?,
            access_token: f("accessToken").unwrap_or_default(),
            expires_at: v["expiresAt"].as_u64().unwrap_or(0),
        })
    }

    fn save(&self, root: &str) -> std::io::Result<()> {
        let body = json!({
            "clientId": self.client_id, "clientSecret": self.client_secret, "refreshToken": self.refresh_token,
            "accessToken": self.access_token, "expiresAt": self.expires_at,
        });
        write_private(&format!("{root}{DIR}/youtube.json"), body.to_string().as_bytes())
    }

    /// A token that is valid now, asking for a new one when the saved one is about to expire.
    fn token(&mut self, root: &str, force: bool) -> Result<String, String> {
        if !force && !self.access_token.is_empty() && now() + 60 < self.expires_at {
            return Ok(self.access_token.clone());
        }
        let v = json_post(
            "https://www.youtube.com/o/oauth2/token",
            &[],
            &json!({
                "refresh_token": self.refresh_token, "client_id": self.client_id,
                "client_secret": self.client_secret, "grant_type": "refresh_token",
            }),
        )?;
        let Some(t) = v["access_token"].as_str() else {
            let why = v["error"].as_str().unwrap_or("no token");
            return Err(if why == "invalid_grant" {
                "the sign-in was revoked or expired; sign in again".to_string()
            } else {
                format!("token: {why}")
            });
        };
        self.access_token = t.into();
        self.expires_at = now() + v["expires_in"].as_u64().unwrap_or(3600);
        let _ = self.save(root);
        Ok(self.access_token.clone())
    }
}

/// Signing out: the token is revoked at Google (best effort, in the background) and the login, the list
/// and its pictures are deleted here.
pub fn sign_out(root: &str, cfg: &Config) {
    let token = cfg.refresh_token.clone();
    std::thread::spawn(move || {
        drop(curl(
            "https://oauth2.googleapis.com/revoke",
            UA,
            &[],
            Some(&format!("token={token}")),
            false,
        ))
    });
    let _ = std::fs::remove_file(format!("{root}{DIR}/youtube.json"));
    let _ = std::fs::remove_file(cache_file(root));
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with("yt-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ---- sign-in -----------------------------------------------------------------------------------------------

/// The OAuth client id and secret that YouTube's TV app has in its script, where `find_client` looks.
fn find_client(js: &str) -> Option<(String, String)> {
    const DOMAIN: &str = ".apps.googleusercontent.com";
    let mut rest = js;
    while let Some(i) = rest.find("clientId:\"") {
        rest = &rest[i + 10..];
        let Some(end) = rest.find('"') else { break };
        let id = &rest[..end];
        if !id.ends_with(DOMAIN) || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-.".contains(c)) {
            continue;
        }
        // `,` then the name of the secret's field, then `:"secret"`
        let after = rest[end + 1..].trim_start_matches([',', '\n', '\r', ' ']);
        let name_len = after.find(':').unwrap_or(0);
        let value = after[name_len..].strip_prefix(":\"");
        let secret = value.and_then(|v| v.split('"').next()).filter(|s| !s.is_empty());
        if let Some(s) = secret.filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            return Some((id.to_string(), s.to_string()));
        }
    }
    None
}

/// The text between `start` and the next `end`.
fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    Some(&s[i..i + s[i..].find(end)?])
}

fn tv_page() -> Result<String, String> {
    let page = curl(
        "https://www.youtube.com/tv",
        PAGE_UA,
        &[("Referer", "https://www.youtube.com/tv".into())],
        None,
        true,
    )?;
    Ok(String::from_utf8_lossy(&page).into_owned())
}

fn scrape_client() -> Result<(String, String), String> {
    let page = tv_page()?;
    let path =
        between(&page, "id=\"base-js\" src=\"", "\"").ok_or("youtube.com/tv has no script to read the client from")?;
    let js = curl(&format!("https://www.youtube.com{path}"), PAGE_UA, &[], None, true)?;
    find_client(&String::from_utf8_lossy(&js)).ok_or_else(|| "the TV app's sign-in client was not found".to_string())
}

pub(crate) fn device_id() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{:016x}{:08x}", t as u64 ^ 0x9e37_79b9_7f4a_7c15, std::process::id())
}

pub(crate) fn sleep_unless(cancel: &AtomicBool, secs: u64) -> bool {
    for _ in 0..secs * 4 {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    !cancel.load(Ordering::SeqCst)
}

/// Asks for a code, shows it (`Qc::Code`, the second field being the page to enter it on), and waits for the
/// user to enter it on a phone. Saves `youtube.json` and says `Qc::Linked`. Stops when `cancel` is set.
pub fn device_flow(root: String, wake: u32, cancel: Arc<AtomicBool>) -> Receiver<Qc> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let say = |q| {
            let _ = tx.send(q);
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
        };
        let run = || -> Result<Option<Config>, String> {
            let (client_id, client_secret) = scrape_client()?;
            let v = json_post(
                "https://www.youtube.com/o/oauth2/device/code",
                &[],
                &json!({ "client_id": client_id, "device_id": device_id(), "model_name": "ytlr::", "scope": SCOPE }),
            )?;
            let (Some(device), Some(code)) = (v["device_code"].as_str(), v["user_code"].as_str()) else {
                return Err(format!(
                    "no code from YouTube ({})",
                    v["error"].as_str().unwrap_or("unknown answer")
                ));
            };
            let url = v["verification_url"].as_str().unwrap_or("youtube.com/activate");
            say(Qc::Code {
                code: code.to_string(),
                server: url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .to_string(),
            });
            let mut every = v["interval"].as_u64().filter(|i| *i > 0).unwrap_or(5);
            let deadline = now() + v["expires_in"].as_u64().unwrap_or(600);
            while now() < deadline {
                if !sleep_unless(&cancel, every) {
                    return Ok(None);
                }
                let t = json_post(
                    "https://www.youtube.com/o/oauth2/token",
                    &[],
                    &json!({
                        "code": device, "client_id": client_id, "client_secret": client_secret,
                        "grant_type": GRANT_DEVICE,
                    }),
                )?;
                match (t["refresh_token"].as_str(), t["error"].as_str()) {
                    (Some(r), _) => {
                        return Ok(Some(Config {
                            client_id,
                            client_secret,
                            refresh_token: r.into(),
                            access_token: t["access_token"].as_str().unwrap_or("").into(),
                            expires_at: now() + t["expires_in"].as_u64().unwrap_or(0),
                        }))
                    }
                    (_, Some("authorization_pending")) => {}
                    (_, Some("slow_down")) => every += 5,
                    (_, Some("access_denied")) => return Err("The sign-in was refused.".into()),
                    (_, Some(e)) => return Err(format!("YouTube said: {e}")),
                    _ => return Err("YouTube gave no token".into()),
                }
            }
            Err("The code expired.".into())
        };
        match run() {
            Ok(Some(cfg)) => match std::fs::create_dir_all(format!("{root}{DIR}")).and_then(|()| cfg.save(&root)) {
                Ok(()) => say(Qc::Linked),
                Err(e) => say(Qc::Failed(format!("Could not save the sign-in: {e}"))),
            },
            Ok(None) => {}
            Err(e) => {
                crate::sdl::log(&format!("youtube sign-in: {e}"));
                say(Qc::Failed(e));
            }
        }
    });
    rx
}

// ---- the feed ----------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub id: String,
    pub title: String,
    pub sub: String,
    pub progress: f32,
    /// The picture's path in the cache (without the root).
    pub card: Option<String>,
    thumb: Option<String>,
}

impl Tile {
    pub fn item(&self) -> Item {
        let mut it = Item::new(
            format!("yt:{}", self.id),
            APP.into(),
            Kind::Watch,
            Some(self.title.clone()),
        );
        it.sub = Some(self.sub.clone()).filter(|s| !s.is_empty());
        it.bg = self.card.clone();
        it.icons.extend(self.card.clone());
        it.progress = self.progress;
        it.cover = true;
        it.color = Color {
            r: 58,
            g: 34,
            b: 40,
            a: 255,
        };
        // The app is asked both ways: webosbrew's youtube-webos reads `contentTarget`, Home Assistant sends `contentId`.
        let v = format!("v={}", self.id);
        it.params = Some(json!({ "contentTarget": v, "contentId": v }));
        it
    }
}

fn cache_file(root: &str) -> String {
    format!("{root}{DIR}/cache/youtube.json")
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
                id: o["id"].as_str()?.into(),
                title: o["title"].as_str()?.into(),
                sub: o["sub"].as_str().unwrap_or("").into(),
                progress: o["progress"].as_f64().unwrap_or(0.0) as f32,
                card: o["card"].as_str().map(String::from),
                thumb: None,
            })
        })
        .collect()
}

/// Text of a YouTube `{"simpleText": ..}` or `{"runs": [{"text": ..}]}`.
fn text(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str() {
        return s.to_string();
    }
    v["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["text"].as_str())
        .collect()
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

fn tile(t: &Value) -> Option<Tile> {
    if t["contentType"] != "TILE_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let id = t["contentId"].as_str().filter(|i| i.len() == 11)?;
    // Shorts and other kinds open with another endpoint, which the app is not asked for here.
    t["onSelectCommand"]["watchEndpoint"]["videoId"].as_str()?;
    let meta = &t["metadata"]["tileMetadataRenderer"];
    let title = text(&meta["title"]);
    if title.is_empty() {
        return None;
    }
    let lines: Vec<String> = meta["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            l["lineRenderer"]["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| text(&i["lineItemRenderer"]["text"]))
                .filter(|s| !s.is_empty() && s != "\u{2022}")
                .collect::<Vec<_>>()
                .join(" \u{b7} ")
        })
        .filter(|l| !l.is_empty())
        .collect();
    let header = &t["header"]["tileHeaderRenderer"];
    let percent = find_key(&header["thumbnailOverlays"], "percentDurationWatched").and_then(|p| p.as_f64());
    let thumb = header["thumbnail"]["thumbnails"]
        .as_array()
        .and_then(|a| a.iter().max_by_key(|x| x["width"].as_u64().unwrap_or(0)))
        .and_then(|x| x["url"].as_str())
        .map(String::from);
    Some(Tile {
        id: id.into(),
        title,
        sub: lines.join("\n"),
        progress: (percent.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32,
        card: None,
        thumb,
    })
}

/// Every video tile in a browse answer, in order, each video once.
fn parse_tiles(v: &Value) -> Vec<Tile> {
    fn walk(v: &Value, out: &mut Vec<Tile>) {
        match v {
            Value::Object(m) => {
                if let Some(t) = m.get("tileRenderer").and_then(tile) {
                    if !out.iter().any(|o| o.id == t.id) {
                        out.push(t);
                    }
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    walk(v, &mut out);
    out.truncate(MAX_ITEMS);
    out
}

/// `logged_in` as the answer reports it: "1" when YouTube accepted the sign-in.
fn logged_in(v: &Value) -> Option<String> {
    v["responseContext"]["serviceTrackingParams"]
        .as_array()?
        .iter()
        .flat_map(|s| s["params"].as_array().into_iter().flatten())
        .find(|p| p["key"] == "logged_in")
        .and_then(|p| p["value"].as_str().map(String::from))
}

fn browse(token: &str, version: &str, visitor: Option<&str>) -> Result<Value, String> {
    let mut client = json!({
        "clientName": "TVHTML5", "clientVersion": version, "clientScreen": "WATCH", "userAgent": UA,
        "tvAppInfo": { "appQuality": "TV_APP_QUALITY_FULL_ANIMATION", "zylonLeftNav": true },
        "webpSupport": false, "animatedWebpSupport": true, "acceptLanguage": "en",
    });
    if let Some(v) = visitor {
        client["visitorData"] = v.into();
    }
    let body = json!({
        "context": { "client": client, "user": { "enableSafetyMode": false, "lockedSafetyMode": false } },
        "racyCheckOk": true, "contentCheckOk": true, "browseId": "default",
    });
    let h = [
        ("Authorization", format!("Bearer {token}")),
        ("Referer", "https://www.youtube.com/tv".to_string()),
    ];
    json_post("https://www.youtube.com/youtubei/v1/browse", &h, &body)
}

/// Starts the refresh thread: send `true` to ask now, `false` to ask unless the saved list is younger than
/// 30 minutes. A new list arrives on the receiver and `wake` is pushed; `None` means nothing changed.
pub fn spawn(root: String, cfg: Config, wake: u32) -> (Sender<bool>, Receiver<Option<Vec<Tile>>>) {
    let (req_tx, req_rx) = channel::<bool>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        let mut cfg = cfg;
        while let Ok(mut force) = req_rx.recv() {
            while let Ok(f) = req_rx.try_recv() {
                force |= f;
            }
            let list = refresh(&root, &mut cfg, force)
                .map_err(|e| crate::sdl::log(&format!("youtube: {e}")))
                .ok()
                .flatten();
            if res_tx.send(list).is_err() {
                break;
            }
            unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
        }
    });
    (req_tx, res_rx)
}

fn age(path: &str) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(m).ok().map(|d| d.as_secs())
}

fn refresh(root: &str, cfg: &mut Config, force: bool) -> Result<Option<Vec<Tile>>, String> {
    if !force && age(&cache_file(root)).is_some_and(|a| a < MIN_AGE) {
        return Ok(None);
    }
    std::fs::create_dir_all(format!("{root}{DIR}/cache")).map_err(|e| e.to_string())?;
    let page = tv_page().unwrap_or_default();
    let version = between(&page, "\"INNERTUBE_CONTEXT_CLIENT_VERSION\":\"", "\"").unwrap_or(FALLBACK_VERSION);
    let visitor = between(&page, "\"visitorData\":\"", "\"");
    let mut answer = browse(&cfg.token(root, false)?, version, visitor)?;
    if answer["error"]["code"] == 401 {
        answer = browse(&cfg.token(root, true)?, version, visitor)?;
    }
    let mut tiles = parse_tiles(&answer);
    if tiles.is_empty() {
        let status = answer["error"]["message"].as_str().map(String::from);
        return Err(format!(
            "no videos in the answer (logged_in {:?}, error {:?}, keys {:?})",
            logged_in(&answer),
            status,
            answer.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>())
        ));
    }
    for t in &mut tiles {
        let path = format!("{DIR}/cache/yt-{}.img", t.id);
        if std::path::Path::new(&format!("{root}{path}")).exists() {
            t.card = Some(path);
        } else if let Some(url) = &t.thumb {
            match curl(url, UA, &[], None, true) {
                Ok(bytes) => {
                    if std::fs::write(format!("{root}{path}"), bytes).is_ok() {
                        t.card = Some(path);
                    }
                }
                Err(e) => crate::sdl::log(&format!("youtube picture {}: {e}", t.id)),
            }
        }
    }
    save(root, &tiles);
    Ok(Some(tiles))
}

/// Writes the list, and removes pictures that no entry uses any more.
fn save(root: &str, list: &[Tile]) {
    let items: Vec<Value> = list
        .iter()
        .map(|t| json!({ "id": t.id, "title": t.title, "sub": t.sub, "progress": t.progress, "card": t.card }))
        .collect();
    let _ = std::fs::write(cache_file(root), json!({ "items": items }).to_string());
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("yt-") && !list.iter().any(|t| name == format!("yt-{}.img", t.id)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../tests/tv-browse-sample.json");

    #[test]
    fn tiles_come_from_a_real_browse_answer() {
        let v: Value = serde_json::from_str(SAMPLE).unwrap();
        let t = parse_tiles(&v);
        // the sample has two videos, a playlist (skipped) and a Short-style tile without a watch endpoint (skipped)
        assert_eq!(
            t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["fcnDmrtj6Sk", "kJQP7kiw5Fk"]
        );
        assert_eq!(t[0].title, "Dai Dai");
        assert_eq!(t[0].sub, "Shakira\n1.4bn views \u{b7} 6 months ago");
        assert!(t[0]
            .thumb
            .as_deref()
            .unwrap()
            .starts_with("https://i.ytimg.com/vi/fcnDmrtj6Sk/"));
        assert_eq!(t[0].progress, 0.0);
        assert!((t[1].progress - 0.42).abs() < 1e-6);
        assert_eq!(logged_in(&v).as_deref(), Some("1"));
    }

    #[test]
    fn a_tile_opens_the_youtube_app_with_the_video() {
        let v: Value = serde_json::from_str(SAMPLE).unwrap();
        let it = parse_tiles(&v)[0].item();
        assert_eq!(it.id, "youtube.leanback.v4");
        assert_eq!(it.params.unwrap()["contentTarget"], "v=fcnDmrtj6Sk");
    }

    #[test]
    fn the_client_is_read_from_the_tv_script() {
        let js = "x={clientId:\"nope\",y:\"z\"},w={clientId:\"861-d6d.apps.googleusercontent.com\",\n$q:\"SeCr3tAbC\"}";
        assert_eq!(
            find_client(js),
            Some(("861-d6d.apps.googleusercontent.com".into(), "SeCr3tAbC".into()))
        );
    }

    #[test]
    fn page_values_are_cut_out() {
        let page = "..\"INNERTUBE_CONTEXT_CLIENT_VERSION\":\"7.1\",\"x\"";
        assert_eq!(
            between(page, "\"INNERTUBE_CONTEXT_CLIENT_VERSION\":\"", "\""),
            Some("7.1")
        );
        assert_eq!(between(page, "nothing", "\""), None);
    }
}
