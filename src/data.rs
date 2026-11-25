//! What the launcher shows: apps and inputs from the TV's luna services, launching them, and the user's own
//! choices (which apps are hidden, in what order).
use crate::sdl::{Color, FRect, Rect, Tex};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

/// Everything the launcher keeps on the TV's storage lives here (under `LAUNCHER_ROOT` in the simulator).
pub const DIR: &str = "/media/developer/tvlauncher";
/// Writes `bytes` to `path` for the owner only, through a temporary file: the files it is used for hold tokens.
pub fn write_private(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = format!("{path}.tmp");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(bytes)?;
    std::fs::rename(tmp, path)
}

pub const FOCUS_FILE: &str = "/tmp/tvlauncher-focus";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    App,
    Input,
    Watch,
    /// The last card of the Apps row: shows or hides the apps the user hid.
    Toggle,
}

pub struct Item {
    /// Stable identity: the app id, or `jf:<item id>`. Focus memory and the saved order use it.
    pub key: String,
    /// The app that opens it.
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub sub: Option<String>,
    pub bg: Option<String>,
    pub params: Option<Value>,
    pub icons: Vec<String>,
    pub color: Color,
    pub connected: bool,
    /// 0 for none, else the fraction watched.
    pub progress: f32,
    /// The image fills the card (a still from a video) instead of sitting in the middle (an app icon).
    pub cover: bool,
    /// LG's flag: the app can be uninstalled (not a system app).
    pub removable: bool,
    /// Close the app before launching it, for an app that only reads its launch parameters at start.
    pub restart: bool,
    pub hidden: bool,
    pub moving: bool,
    pub tv_index: usize,
    pub icon_tried: bool,
    pub icon: Option<Tex>,
    /// The part of an app icon that holds its logo, so the logo can fill the card instead of floating in padding.
    pub icon_src: Option<Rect>,
    pub label: Option<Tex>,
    pub hero: Option<Tex>,
    pub hero_sub: Option<Tex>,
    pub focus: f32,
    /// Where it was last drawn, for the pointer; zero width when it is off screen.
    pub rect: FRect,
}

impl Item {
    pub fn new(key: String, id: String, kind: Kind, title: Option<String>) -> Item {
        Item {
            title: title.unwrap_or_else(|| id.clone()),
            key,
            id,
            kind,
            sub: None,
            bg: None,
            params: None,
            icons: vec![],
            color: Color {
                r: 60,
                g: 64,
                b: 67,
                a: 255,
            },
            connected: false,
            progress: 0.0,
            cover: false,
            hidden: false,
            moving: false,
            tv_index: 0,
            icon_tried: false,
            removable: false,
            restart: false,
            icon: None,
            icon_src: None,
            label: None,
            hero: None,
            hero_sub: None,
            focus: 0.0,
            rect: Default::default(),
        }
    }
}

pub struct Row {
    pub name: &'static str,
    pub items: Vec<Item>,
    pub col: usize,
    pub scroll: f32,
    pub active: f32,
    pub name_tex: Option<Tex>,
}

impl Row {
    pub fn new(name: &'static str) -> Row {
        Row {
            name,
            items: vec![],
            col: 0,
            scroll: 0.0,
            active: 0.0,
            name_tex: None,
        }
    }
}

/// With a `root`, replies come from `<root>/luna/<method>.json` (see `tools/pull-fixtures.sh`).
/// Makes the TV match the debloat choices in `state.json`. Not in the simulator.
pub fn apply_debloat(root: &str) {
    crate::sdl::log("debloat apply");
    if !root.is_empty() {
        return;
    }
    let child = Command::new("/bin/sh")
        .arg(format!("{DIR}/debloat.sh"))
        .arg("apply")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || drop(c.wait()));
    }
}

const BOOT_HOOK: &str = "/var/lib/webosbrew/init.d/20-tvlauncher-home";

/// Whether the TV starts this launcher as Home at boot (`takeover.sh boot`).
pub fn boot_on(root: &str) -> bool {
    std::fs::symlink_metadata(format!("{root}{BOOT_HOOK}")).is_ok()
}

/// Turns the start at boot on or off. Not in the simulator.
pub fn set_boot(root: &str, on: bool) {
    crate::sdl::log(&format!("start at boot: {on}"));
    if root.is_empty() {
        let _ = Command::new("/bin/sh")
            .arg(format!("{DIR}/takeover.sh"))
            .args(["boot", if on { "on" } else { "off" }])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[derive(Clone)]
pub struct Luna {
    pub root: String,
}

const APPS_URI: &str = "luna://com.webos.applicationManager/listLaunchPoints";
const INPUTS_URI: &str = "luna://com.webos.service.eim/getAllInputStatus";

impl Luna {
    /// Raw reply from a luna call, or a fixture file in the simulator.
    pub fn fetch(&self, uri: &str) -> Vec<u8> {
        if self.root.is_empty() {
            Command::new("/usr/bin/luna-send")
                .args(["-n", "1", "-w", "5000", uri, "{}"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default()
        } else {
            std::fs::read(format!(
                "{}/luna/{}.json",
                self.root,
                uri.rsplit('/').next().unwrap_or("")
            ))
            .unwrap_or_default()
        }
    }

    /// Asks the app installer to remove an app, then waits (up to 60 s) until it is gone from the launch points.
    /// The receiver gets a message when that is over, whether it worked or not. None in the simulator.
    pub fn uninstall(&self, id: &str, wake: u32) -> Option<std::sync::mpsc::Receiver<()>> {
        if !self.root.is_empty() {
            return None;
        }
        crate::sdl::log(&format!("uninstall {id}"));
        let (tx, rx) = std::sync::mpsc::channel();
        let id = id.to_string();
        std::thread::spawn(move || {
            let luna = |uri: &str, body: &str| {
                Command::new("/usr/bin/luna-send")
                    .args(["-n", "1", "-w", "5000", uri, body])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default()
            };
            let reply = luna(
                "luna://com.webos.appInstallService/remove",
                &json!({ "id": id }).to_string(),
            );
            crate::sdl::log(&format!(
                "uninstall {id}: {}",
                reply.split_whitespace().collect::<String>()
            ));
            let needle = format!("\"id\":\"{id}\"");
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let list = luna("luna://com.webos.applicationManager/listLaunchPoints", "{}");
                if !list.is_empty() && !list.contains(&needle) {
                    break;
                }
            }
            let _ = tx.send(());
            unsafe { crate::sdl::SDL_PushEvent(&mut crate::sdl::Event::new(wake)) };
        });
        Some(rx)
    }

    pub fn launch(&self, it: &Item) {
        let mut req = json!({ "id": it.id });
        if let Some(p) = &it.params {
            req["params"] = p.clone();
        }
        crate::sdl::log(&format!("launch {req}"));
        if !self.root.is_empty() {
            return;
        }
        // The app manager starts Home afresh on the Home key, so the focus survives in a file.
        let _ = std::fs::write(FOCUS_FILE, &it.key);
        let close = it.restart.then(|| json!({ "id": it.id }).to_string());
        let req = req.to_string();
        std::thread::spawn(move || {
            let call = |method: &str, body: &str| {
                drop(
                    Command::new("/usr/bin/luna-send")
                        .args(["-n", "1", "-w", "5000"])
                        .arg(format!("luna://com.webos.applicationManager/{method}"))
                        .arg(body)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status(),
                )
            };
            if let Some(body) = close {
                call("closeByAppId", &body);
                // PlxNative measured 600 ms as enough for the close to finish.
                std::thread::sleep(std::time::Duration::from_millis(600));
            }
            call("launch", &req);
        });
    }
}

fn s(o: &Value, k: &str) -> Option<String> {
    o[k].as_str().filter(|s| !s.is_empty()).map(String::from)
}

fn hex(s: Option<&str>, def: Color) -> Color {
    let digits = s.and_then(|s| s.split_once('#')).and_then(|(_, r)| r.get(..6));
    match digits.and_then(|d| u32::from_str_radix(d, 16).ok()) {
        Some(v) => Color {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            a: 255,
        },
        None => def,
    }
}

/// Apps that must stay: CleanYourTV is the Home that comes back when this launcher is off, and the Homebrew Channel
/// keeps the TV rooted.
const KEEP: [&str; 2] = ["com.cleanyourtv.", "org.webosbrew.hbchannel"];

fn id_of(o: &Value) -> String {
    s(o, "id").unwrap_or_default()
}

/// Apps and inputs from the bus. On the TV the two luna calls run together.
pub fn launch_lists(luna: &Luna) -> (Vec<Item>, Vec<Item>) {
    if !luna.root.is_empty() {
        return lists_from_bytes(&luna.fetch(APPS_URI), &luna.fetch(INPUTS_URI));
    }
    let luna2 = luna.clone();
    let (apps, inputs) = std::thread::scope(|s| {
        let t1 = s.spawn(|| luna.fetch(APPS_URI));
        let t2 = s.spawn(|| luna2.fetch(INPUTS_URI));
        (t1.join().unwrap_or_default(), t2.join().unwrap_or_default())
    });
    lists_from_bytes(&apps, &inputs)
}

/// Turns raw luna replies into items on the main thread (items carry SDL textures).
pub fn lists_from_bytes(apps: &[u8], inputs: &[u8]) -> (Vec<Item>, Vec<Item>) {
    (apps_from_bytes(apps), inputs_from_bytes(inputs))
}

/// Starts fetching apps and inputs on a thread; the bytes are parsed when they arrive.
pub fn fetch_launch_lists(luna: Luna) -> std::sync::mpsc::Receiver<(Vec<u8>, Vec<u8>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if luna.root.is_empty() {
            let luna2 = luna.clone();
            let (apps, inputs) = std::thread::scope(|s| {
                let t1 = s.spawn(move || luna.fetch(APPS_URI));
                let t2 = s.spawn(move || luna2.fetch(INPUTS_URI));
                (t1.join().unwrap_or_default(), t2.join().unwrap_or_default())
            });
            let _ = tx.send((apps, inputs));
        } else {
            let _ = tx.send((luna.fetch(APPS_URI), luna.fetch(INPUTS_URI)));
        }
    });
    rx
}

fn apps_from_bytes(out: &[u8]) -> Vec<Item> {
    let j = serde_json::from_slice::<Value>(out).ok();
    let Some(points) = j.as_ref().and_then(|j| j["launchPoints"].as_array()) else {
        return vec![];
    };
    points
        .iter()
        .filter_map(|o| {
            let id = s(o, "id")?;
            let mut it = Item::new(id.clone(), id, Kind::App, s(o, "title"));
            it.sub = s(o, "appDescription");
            it.removable = o["removable"].as_bool().unwrap_or(false) && !KEEP.iter().any(|k| id_of(o).starts_with(k));
            for k in ["extraLargeIcon", "mediumLargeIcon", "largeIcon", "icon"] {
                it.icons.extend(s(o, k));
            }
            let c = s(o, "bgColor")
                .filter(|c| c.contains('#'))
                .or_else(|| s(o, "iconColor"));
            it.color = hex(c.as_deref(), it.color);
            it.params = Some(o["params"].clone()).filter(|p| p.as_object().is_some_and(|m| !m.is_empty()));
            Some(it)
        })
        .enumerate()
        .map(|(i, mut it)| {
            it.tv_index = i;
            it
        })
        .collect()
}

fn inputs_from_bytes(out: &[u8]) -> Vec<Item> {
    let j = serde_json::from_slice::<Value>(out).ok();
    let Some(devices) = j.as_ref().and_then(|j| j["devices"].as_array()) else {
        return vec![];
    };
    devices
        .iter()
        .filter_map(|o| {
            let id = s(o, "appId")?;
            let mut it = Item::new(id.clone(), id, Kind::Input, s(o, "label"));
            it.connected = o["connected"].as_bool().unwrap_or(false);
            it.color = Color {
                r: 45,
                g: 48,
                b: 54,
                a: 255,
            };
            let port = o["port"].as_i64().unwrap_or(0);
            let mut sub = format!(
                "HDMI {port} · {}",
                if it.connected { "Connected" } else { "Nothing connected" }
            );
            for d in o["subList"].as_array().into_iter().flatten() {
                if let Some(n) = s(d, "osdName").filter(|n| *n != it.title && !sub.contains(n.as_str())) {
                    sub += &format!(" · {n}");
                }
            }
            it.sub = Some(sub);
            if let Some(icon) = o["icon"].as_str() {
                it.icons
                    .push(format!("/usr/palm/plugins/inputapps/assets/2k/medium/{icon}"));
                it.icons
                    .push(format!("{}{icon}", o["iconPrefix"].as_str().unwrap_or("")));
            }
            Some(it)
        })
        .collect()
}

/// The user's choices about the Apps row, kept in `<DIR>/state.json`.
#[derive(Default)]
pub struct Prefs {
    path: String,
    pub hidden: Vec<String>,
    /// App ids in the order the user arranged them; apps not listed follow in the TV's own order.
    pub order: Vec<String>,
    /// Apps pinned to the Pinned apps row, in the order of that row.
    pub pinned: Vec<String>,
    /// The Jellyfin server signed in to last, offered again the next time.
    pub jf_server: Option<String>,
    /// The app that opens a Jellyfin item.
    pub jf_app: Option<String>,
    pub plex_app: Option<String>,
    /// Groups of LG's ads and telemetry kept off while this is Home (see `debloat.sh`).
    pub debloat: Vec<String>,
    /// Channels in the TV tab's Favourites, by `iptv::Channel::key`.
    pub tv_favs: Vec<String>,
    /// The tabs after Home in the user's order ("tv" or a library's id), and the ones hidden.
    pub tabs: Vec<String>,
    pub tabs_off: Vec<String>,
    /// Names the user gave tabs, by the same keys.
    pub tab_names: BTreeMap<String, String>,
    /// The Continue watching rows turned off: "jellyfin", "plex".
    pub rows_off: Vec<String>,
    /// Jellyfin titles play in the launcher's own player rather than the Jellyfin app.
    pub jf_player: bool,
    /// How the library tabs are sorted: "name" or "added".
    pub lib_sort: String,
}

/// The groups `debloat.sh` knows, with what each stops.
pub const DEBLOAT: [(&str, &str); 4] = [
    ("ads", "Block ads"),
    ("tracking", "Block viewing tracking"),
    ("logs", "Stop telemetry logs"),
    ("suggestions", "Stop suggestions"),
];

impl Prefs {
    pub fn load(root: &str) -> Prefs {
        let path = format!("{root}{DIR}/state.json");
        let v: Value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let list = |k: &str| {
            v[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        };
        Prefs {
            hidden: list("hidden"),
            order: list("order"),
            pinned: list("pinned"),
            jf_server: v["jfServer"].as_str().map(String::from),
            jf_app: v["jfApp"].as_str().map(String::from),
            plex_app: v["plexApp"].as_str().map(String::from),
            debloat: list("debloat"),
            tv_favs: list("tvFavs"),
            tabs: list("tabs"),
            tabs_off: list("tabsOff"),
            rows_off: list("rowsOff"),
            jf_player: v["jfPlayer"].as_bool().unwrap_or(false),
            tab_names: serde_json::from_value(v["tabNames"].clone()).unwrap_or_default(),
            lib_sort: v["libSort"].as_str().unwrap_or("name").into(),
            path,
        }
    }

    pub fn save(&self) {
        let body =
            json!({ "hidden": self.hidden, "order": self.order, "pinned": self.pinned, "jfServer": self.jf_server, "jfApp": self.jf_app, "plexApp": self.plex_app, "debloat": self.debloat, "tvFavs": self.tv_favs, "tabs": self.tabs, "tabsOff": self.tabs_off, "tabNames": self.tab_names, "rowsOff": self.rows_off, "jfPlayer": self.jf_player, "libSort": self.lib_sort })
                .to_string();
        let tmp = format!("{}.tmp", self.path);
        if std::fs::write(&tmp, body)
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .is_err()
        {
            crate::sdl::log(&format!("could not save {}", self.path));
        }
    }
}
