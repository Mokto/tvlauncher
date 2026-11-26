//! The sign-in dialog: Jellyfin's Quick Connect, with a number pad for the server address when the
//! server is not found on the network, or YouTube's code screen (the same layout, no address step).
use crate::gfx::*;
use crate::jellyfin::{self, Config, Qc};
use crate::sdl::*;
use crate::{plex, youtube};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// The keyboard for the server address: each row adds up to ten key widths, a key is `(label, widths)`.
const KEYS: [&[(&str, u32)]; 5] = [
    &[
        ("1", 1),
        ("2", 1),
        ("3", 1),
        ("4", 1),
        ("5", 1),
        ("6", 1),
        ("7", 1),
        ("8", 1),
        ("9", 1),
        ("0", 1),
    ],
    &[
        ("q", 1),
        ("w", 1),
        ("e", 1),
        ("r", 1),
        ("t", 1),
        ("y", 1),
        ("u", 1),
        ("i", 1),
        ("o", 1),
        ("p", 1),
    ],
    &[
        ("a", 1),
        ("s", 1),
        ("d", 1),
        ("f", 1),
        ("g", 1),
        ("h", 1),
        ("j", 1),
        ("k", 1),
        ("l", 1),
        ("-", 1),
    ],
    &[
        ("z", 1),
        ("x", 1),
        ("c", 1),
        ("v", 1),
        ("b", 1),
        ("n", 1),
        ("m", 1),
        (".", 1),
        (":", 1),
        ("/", 1),
    ],
    &[("https://", 2), ("http://", 2), ("Delete", 2), ("Connect", 4)],
];
const KEY_W: f32 = 108.0;
const KEY_H: f32 = 68.0;
const KEY_GAP: f32 = 10.0;
const PANEL_W: f32 = 1280.0;

/// The first key width each key of a row starts at, so Up and Down land on the key above or below.
fn starts(row: &[(&str, u32)]) -> Vec<u32> {
    row.iter()
        .scan(0, |at, (_, span)| {
            let s = *at;
            *at += span;
            Some(s)
        })
        .collect()
}

const SCHEMES: [&str; 2] = ["https://", "http://"];

/// What a finished sign-in leaves behind.
pub enum Signed {
    Jellyfin(Config),
    /// The tokens are saved in `youtube.json`.
    Youtube,
    /// The token is saved in `plex.json`.
    Plex,
}

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    Jellyfin,
    Youtube,
    Plex,
}

#[derive(PartialEq)]
enum Step {
    Searching,
    Code,
    Address,
    Failed,
}

pub struct Login {
    provider: Provider,
    step: Step,
    root: String,
    wake: u32,
    rx: Receiver<Qc>,
    cancel: Arc<AtomicBool>,
    /// The address being typed, and the key under the cursor.
    text: String,
    sel: (usize, usize),
    /// What the thread was last asked to try, for "try again".
    server: Option<String>,
    title: Option<Tex>,
    body: Option<Tex>,
    code: Option<Tex>,
    foot: Option<Tex>,
    field: Option<Tex>,
    keys: Vec<Vec<Option<Tex>>>,
}

impl Drop for Login {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Login {
    /// `server` is the address used last time, if any.
    pub fn new(g: &Gfx, root: &str, wake: u32, server: Option<String>) -> Login {
        let mut l = Login::blank(g, root, wake, server.clone());
        l.start(g, server, true);
        l
    }

    fn blank(g: &Gfx, root: &str, wake: u32, server: Option<String>) -> Login {
        let cancel = Arc::new(AtomicBool::new(false));
        Login {
            provider: Provider::Jellyfin,
            step: Step::Searching,
            root: root.to_string(),
            wake,
            rx: std::sync::mpsc::channel().1,
            cancel,
            text: server.clone().unwrap_or_default(),
            sel: (0, 0),
            server: None,
            title: g.text(g.f_label, "Sign in to Jellyfin", 0),
            body: None,
            code: None,
            foot: None,
            field: None,
            keys: KEYS
                .iter()
                .map(|r| r.iter().map(|(k, _)| g.text(g.f_menu, k, 0)).collect())
                .collect(),
        }
    }

    pub fn youtube(g: &Gfx, root: &str, wake: u32) -> Login {
        let mut l = Login::blank(g, root, wake, None);
        l.provider = Provider::Youtube;
        l.title = g.text(g.f_label, "Sign in to YouTube", 0);
        l.start(g, None, false);
        l
    }

    pub fn plex(g: &Gfx, root: &str, wake: u32) -> Login {
        let mut l = Login::blank(g, root, wake, None);
        l.provider = Provider::Plex;
        l.title = g.text(g.f_label, "Sign in to Plex", 0);
        l.start(g, None, false);
        l
    }

    fn start(&mut self, g: &Gfx, server: Option<String>, discover: bool) {
        self.cancel.store(true, Ordering::SeqCst);
        self.cancel = Arc::new(AtomicBool::new(false));
        match self.provider {
            Provider::Youtube => {
                self.rx = youtube::device_flow(self.root.clone(), self.wake, self.cancel.clone());
                self.step = Step::Searching;
                return self.say(g, "Asking YouTube for a code...", None, None);
            }
            Provider::Plex => {
                self.rx = plex::pin_flow(self.root.clone(), self.wake, self.cancel.clone());
                self.step = Step::Searching;
                return self.say(g, "Asking Plex for a code...", None, None);
            }
            Provider::Jellyfin => {}
        }
        self.rx = jellyfin::quick_connect(
            self.root.clone(),
            self.wake,
            server.clone(),
            discover,
            self.cancel.clone(),
        );
        self.server = server;
        self.step = Step::Searching;
        self.say(g, "Looking for your Jellyfin server...", None, None);
    }

    fn say(&mut self, g: &Gfx, body: &str, code: Option<&str>, foot: Option<&str>) {
        self.body = g.text(g.f_menu, body, PANEL_W as u32 - 96);
        self.code = code.and_then(|c| g.text(g.f_hero, c, 0));
        self.foot = foot.and_then(|f| g.text(g.f_label, f, 0));
    }

    /// True while the sign-in thread has not said anything yet.
    pub fn busy(&self) -> bool {
        self.step == Step::Searching
    }

    /// Takes what the thread has to say; the new login once the user has entered the code.
    pub fn poll(&mut self, g: &Gfx) -> Option<Signed> {
        while let Ok(q) = self.rx.try_recv() {
            match q {
                Qc::Code { code, server } if self.provider != Provider::Jellyfin => {
                    self.step = Step::Code;
                    let body = format!("On your phone or computer, go to {server} and enter this code:");
                    self.say(g, &body, Some(&code), Some("Waiting for you to enter it"));
                }
                Qc::Code { code, server } => {
                    self.step = Step::Code;
                    let spaced = format!("{} {}", &code[..code.len() / 2], &code[code.len() / 2..]);
                    self.say(
                        g,
                        "On your phone or computer, open Jellyfin, go to Settings, then Quick Connect, and enter this code:",
                        Some(&spaced),
                        Some(&format!("{server}  -  waiting for you to enter it")),
                    );
                }
                Qc::NeedAddress(why) => {
                    self.step = Step::Address;
                    self.say(g, &format!("{why} Type the server's address:"), None, None);
                    self.field(g);
                }
                Qc::Failed(e) => {
                    self.step = Step::Failed;
                    self.say(g, &e, None, Some("OK to try again   Back to close"));
                }
                Qc::Done(cfg) => return Some(Signed::Jellyfin(*cfg)),
                Qc::Linked if self.provider == Provider::Plex => return Some(Signed::Plex),
                Qc::Linked => return Some(Signed::Youtube),
            }
        }
        None
    }

    fn field(&mut self, g: &Gfx) {
        self.field = if self.text.is_empty() {
            g.text(g.f_menu, "jellyfin.example.com or 192.168.1.10", 0)
        } else {
            g.text(g.f_menu, &format!("{}_", self.text), 0)
        };
    }

    fn press(&mut self, g: &Gfx, key: &str) {
        match key {
            "Delete" => drop(self.text.pop()),
            k if SCHEMES.contains(&k) => {
                let bare = SCHEMES
                    .iter()
                    .fold(self.text.as_str(), |t, s| t.strip_prefix(s).unwrap_or(t));
                self.text = format!("{k}{bare}");
            }
            "Connect" => {
                if !self.text.is_empty() {
                    self.start(g, Some(self.text.clone()), false);
                }
                return;
            }
            k => self.text.push_str(k),
        }
        self.text.truncate(80);
        self.field(g);
    }

    /// False when the dialog should close.
    pub fn key(&mut self, g: &Gfx, sym: i32, ok: bool, back: bool, dx: i32, dy: i32) -> bool {
        if back {
            return false;
        }
        match self.step {
            Step::Failed if ok => {
                let server = self.server.clone();
                self.start(g, server, true);
            }
            Step::Address => {
                let typed = u32::try_from(sym)
                    .ok()
                    .and_then(char::from_u32)
                    .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-./:".contains(*c));
                if let Some(c) = typed {
                    self.press(g, &c.to_string());
                } else if sym == 8 {
                    self.press(g, "Delete");
                } else if ok {
                    self.press(g, KEYS[self.sel.0][self.sel.1].0);
                } else if dx != 0 {
                    let n = KEYS[self.sel.0].len() as i32;
                    self.sel.1 = (self.sel.1 as i32 + dx).clamp(0, n - 1) as usize;
                } else if dy != 0 {
                    let at = starts(KEYS[self.sel.0])[self.sel.1];
                    let row = (self.sel.0 as i32 + dy).clamp(0, KEYS.len() as i32 - 1) as usize;
                    let col = starts(KEYS[row]).iter().rposition(|s| *s <= at).unwrap_or(0);
                    self.sel = (row, col);
                }
            }
            _ => {}
        }
        true
    }

    pub fn draw(&self, g: &Gfx) {
        let rect = |x, y, w, h| FRect { x, y, w, h };
        g.fill(BLACK, 0.7, rect(0.0, 0.0, W, H));
        let ph = if self.step == Step::Address { 740.0 } else { 540.0 };
        let (px, py) = ((W - PANEL_W) / 2.0, (H - ph) / 2.0);
        g.rounded(px, py, PANEL_W, ph, 28.0, PANEL, 1.0);
        g.put(self.title.as_ref(), px + 48.0, py + 40.0, DIM, 1.0);
        let body_h = self.body.as_ref().map_or(0.0, |b| b.h as f32);
        g.put(self.body.as_ref(), px + 48.0, py + 110.0, TEXT, 1.0);
        if self.step == Step::Address {
            let fy = py + 120.0 + body_h;
            g.rounded(px + 48.0, fy, PANEL_W - 96.0, 84.0, 18.0, BG, 1.0);
            let fh = self.field.as_ref().map_or(0.0, |f| f.h as f32);
            let c = if self.text.is_empty() { DIM } else { TEXT };
            g.put(self.field.as_ref(), px + 80.0, fy + (84.0 - fh) / 2.0, c, 1.0);
            let gw = 10.0 * KEY_W + 9.0 * KEY_GAP;
            for (r, row) in KEYS.iter().enumerate() {
                let y = fy + 104.0 + r as f32 * (KEY_H + KEY_GAP);
                let mut x = px + (PANEL_W - gw) / 2.0;
                for (i, (_, span)) in row.iter().enumerate() {
                    let w = *span as f32 * KEY_W + (*span as f32 - 1.0) * KEY_GAP;
                    let on = self.sel == (r, i);
                    g.rounded(x, y, w, KEY_H, 16.0, if on { WHITE } else { BG }, 1.0);
                    if let Some(t) = &self.keys[r][i] {
                        g.put(
                            Some(t),
                            x + (w - t.w as f32) / 2.0,
                            y + (KEY_H - t.h as f32) / 2.0,
                            if on { BLACK } else { TEXT },
                            1.0,
                        );
                    }
                    x += w + KEY_GAP;
                }
            }
        } else {
            let mut y = py + 140.0 + body_h;
            if let Some(c) = &self.code {
                g.put(Some(c), px + (PANEL_W - c.w as f32) / 2.0, y + 20.0, WHITE, 1.0);
                y += c.h as f32 + 40.0;
            }
            if let Some(f) = &self.foot {
                g.put(Some(f), px + 48.0, (py + ph - 80.0).max(y), DIM, 1.0);
            }
        }
    }
}
