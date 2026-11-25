//! A dialog that asks for one line of text with an on-screen keyboard: addresses, user names, passwords. The
//! remote's number keys and a USB keyboard type too.
use crate::gfx::*;
use crate::sdl::*;

/// Each row adds up to ten key widths; a key is `(label, widths)`.
const KEYS: [&[(&str, u32)]; 6] = [
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
    &[
        ("_", 1),
        ("?", 1),
        ("=", 1),
        ("&", 1),
        ("%", 1),
        ("@", 1),
        ("~", 1),
        ("+", 1),
        ("#", 1),
        ("!", 1),
    ],
    &[("Shift", 2), ("Space", 2), ("http://", 2), ("Delete", 2), ("Done", 2)],
];
const KEY_W: f32 = 108.0;
const KEY_H: f32 = 64.0;
const KEY_GAP: f32 = 10.0;
const PANEL_W: f32 = 1280.0;

fn starts(row: &[(&str, u32)]) -> Vec<u32> {
    row.iter()
        .scan(0, |at, (_, span)| {
            let s = *at;
            *at += span;
            Some(s)
        })
        .collect()
}

pub enum Typed {
    Stay,
    Cancel,
    Done(String),
}

pub struct Prompt {
    title: Option<Tex>,
    body: Option<Tex>,
    hint: String,
    text: String,
    /// Shown as stars but for the last character.
    secret: bool,
    shift: bool,
    sel: (usize, usize),
    field: Option<Tex>,
    keys: Vec<Vec<Option<Tex>>>,
}

impl Prompt {
    pub fn new(g: &Gfx, title: &str, body: &str, hint: &str, text: &str, secret: bool) -> Prompt {
        let mut p = Prompt {
            title: g.text(g.f_label, title, 0),
            body: g.text(g.f_menu, body, PANEL_W as u32 - 96),
            hint: hint.to_string(),
            text: text.to_string(),
            secret,
            shift: false,
            sel: (1, 0),
            field: None,
            keys: vec![],
        };
        p.relabel(g);
        p.field(g);
        p
    }

    fn label(&self, k: &str) -> String {
        if self.shift && k.len() == 1 {
            k.to_uppercase()
        } else {
            k.to_string()
        }
    }

    fn relabel(&mut self, g: &Gfx) {
        self.keys = KEYS
            .iter()
            .map(|r| r.iter().map(|(k, _)| g.text(g.f_menu, &self.label(k), 0)).collect())
            .collect();
    }

    fn field(&mut self, g: &Gfx) {
        let shown = if self.secret && !self.text.is_empty() {
            let n = self.text.chars().count();
            "*".repeat(n - 1) + &self.text.chars().last().unwrap_or_default().to_string()
        } else {
            self.text.clone()
        };
        // Long addresses show their end, where the typing is.
        let tail: String = {
            let c: Vec<char> = shown.chars().collect();
            c[c.len().saturating_sub(56)..].iter().collect()
        };
        self.field = if self.text.is_empty() {
            g.text(g.f_menu, &self.hint, 0)
        } else {
            g.text(g.f_menu, &format!("{tail}_"), 0)
        };
    }

    fn press(&mut self, g: &Gfx, key: &str) -> Typed {
        match key {
            "Done" => return Typed::Done(self.text.trim().to_string()),
            "Delete" => drop(self.text.pop()),
            "Space" => self.text.push(' '),
            "Shift" => {
                self.shift = !self.shift;
                self.relabel(g);
                return Typed::Stay;
            }
            k => {
                let k = self.label(k);
                self.text.push_str(&k);
            }
        }
        if self.text.len() > 400 {
            self.text.pop();
        }
        self.field(g);
        Typed::Stay
    }

    pub fn key(&mut self, g: &Gfx, sym: i32, ok: bool, back: bool, dx: i32, dy: i32) -> Typed {
        if back {
            return Typed::Cancel;
        }
        let typed = u32::try_from(sym)
            .ok()
            .filter(|c| (32..127).contains(c))
            .and_then(char::from_u32);
        if let Some(c) = typed {
            self.text.push(c);
            self.field(g);
        } else if sym == 8 {
            return self.press(g, "Delete");
        } else if ok {
            return self.press(g, KEYS[self.sel.0][self.sel.1].0);
        } else if dx != 0 {
            let n = KEYS[self.sel.0].len() as i32;
            self.sel.1 = (self.sel.1 as i32 + dx).clamp(0, n - 1) as usize;
        } else if dy != 0 {
            let at = starts(KEYS[self.sel.0])[self.sel.1];
            let row = (self.sel.0 as i32 + dy).clamp(0, KEYS.len() as i32 - 1) as usize;
            let col = starts(KEYS[row]).iter().rposition(|s| *s <= at).unwrap_or(0);
            self.sel = (row, col);
        }
        Typed::Stay
    }

    pub fn draw(&self, g: &Gfx) {
        g.fill(
            BLACK,
            0.7,
            FRect {
                x: 0.0,
                y: 0.0,
                w: W,
                h: H,
            },
        );
        let body_h = self.body.as_ref().map_or(0.0, |b| b.h as f32);
        let ph = 250.0 + body_h + KEYS.len() as f32 * (KEY_H + KEY_GAP);
        let (px, py) = ((W - PANEL_W) / 2.0, (H - ph) / 2.0);
        g.rounded(px, py, PANEL_W, ph, 28.0, PANEL, 1.0);
        g.put(self.title.as_ref(), px + 48.0, py + 40.0, DIM, 1.0);
        g.put(self.body.as_ref(), px + 48.0, py + 100.0, TEXT, 1.0);
        let fy = py + 120.0 + body_h;
        g.rounded(px + 48.0, fy, PANEL_W - 96.0, 84.0, 18.0, BG, 1.0);
        let fh = self.field.as_ref().map_or(0.0, |f| f.h as f32);
        let c = if self.text.is_empty() { DIM } else { TEXT };
        g.put(self.field.as_ref(), px + 80.0, fy + (84.0 - fh) / 2.0, c, 1.0);
        let gw = 10.0 * KEY_W + 9.0 * KEY_GAP;
        for (r, row) in KEYS.iter().enumerate() {
            let y = fy + 110.0 + r as f32 * (KEY_H + KEY_GAP);
            let mut x = px + (PANEL_W - gw) / 2.0;
            for (i, (k, span)) in row.iter().enumerate() {
                let w = *span as f32 * KEY_W + (*span as f32 - 1.0) * KEY_GAP;
                let on = self.sel == (r, i);
                let lit = *k == "Shift" && self.shift;
                let fill = if on {
                    WHITE
                } else if lit {
                    DIM
                } else {
                    BG
                };
                g.rounded(x, y, w, KEY_H, 16.0, fill, 1.0);
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
    }
}
