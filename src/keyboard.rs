//! A dialog that asks for one line of text with an on-screen keyboard: addresses, user names, passwords. The
//! remote's number keys and a USB keyboard type too.
use crate::avatars;
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
const SEARCH_KEYS: [&[(&str, u32)]; 5] = [
    KEYS[0],
    KEYS[1],
    KEYS[2],
    &[
        ("z", 1),
        ("x", 1),
        ("c", 1),
        ("v", 1),
        ("b", 1),
        ("n", 1),
        ("m", 1),
        ("æ", 1),
        ("ø", 1),
        ("å", 1),
    ],
    &[("Space", 4), ("Delete", 3), ("Results", 3)],
];
const PIN_KEYS: [&[(&str, u32)]; 4] = [
    &[("1", 1), ("2", 1), ("3", 1)],
    &[("4", 1), ("5", 1), ("6", 1)],
    &[("7", 1), ("8", 1), ("9", 1)],
    &[("Delete", 1), ("0", 1)],
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

enum Layout {
    Text,
    Pin(usize, usize),
    Search,
}

pub struct Prompt {
    title: Option<Tex>,
    body: Option<Tex>,
    hint: String,
    text: String,
    /// Shown as stars.
    secret: bool,
    search: bool,
    pin: Option<(usize, usize)>,
    shift: bool,
    sel: (usize, usize),
    field: Option<Tex>,
    keys: Vec<Vec<Option<Tex>>>,
}

impl Prompt {
    pub fn new(g: &Gfx, title: &str, body: &str, hint: &str, text: &str, secret: bool) -> Prompt {
        Self::make(g, title, body, hint, text, secret, Layout::Text)
    }

    pub fn pin(g: &Gfx, title: &str, body: &str, hint: &str, avatar: usize, digits: usize) -> Self {
        Self::make(g, title, body, hint, "", true, Layout::Pin(avatar, digits))
    }

    pub(crate) fn search(g: &Gfx) -> Self {
        Self::make(g, "", "", "Search films and shows", "", false, Layout::Search)
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub fn is_pin(&self) -> bool {
        self.pin.is_some()
    }

    fn make(g: &Gfx, title: &str, body: &str, hint: &str, text: &str, secret: bool, layout: Layout) -> Self {
        let keypad = match layout {
            Layout::Pin(avatar, digits) => Some((avatar, digits)),
            _ => None,
        };
        let pin = keypad.is_some();
        let mut p = Prompt {
            title: (!title.is_empty())
                .then(|| g.text(if pin { g.f_title } else { g.f_label }, title, 0))
                .flatten(),
            body: g.text(
                if pin { g.f_sub } else { g.f_menu },
                body,
                if pin { 800 } else { PANEL_W as u32 - 96 },
            ),
            hint: hint.to_string(),
            text: text.to_string(),
            secret,
            search: matches!(layout, Layout::Search),
            pin: keypad,
            shift: false,
            sel: (usize::from(!pin), 0),
            field: None,
            keys: vec![],
        };
        p.relabel(g);
        p.field(g);
        p
    }

    fn layout(&self) -> &'static [&'static [(&'static str, u32)]] {
        if self.pin.is_some() {
            &PIN_KEYS
        } else if self.search {
            &SEARCH_KEYS
        } else {
            &KEYS
        }
    }

    fn label(&self, k: &str) -> String {
        if self.shift && k.len() == 1 {
            k.to_uppercase()
        } else {
            k.to_string()
        }
    }

    fn relabel(&mut self, g: &Gfx) {
        self.keys = self
            .layout()
            .iter()
            .map(|r| {
                r.iter()
                    .map(|(k, _)| {
                        let font = if self.pin.is_none() || k.len() == 1 {
                            g.f_menu
                        } else {
                            g.f_label
                        };
                        g.text(font, &self.label(k), 0)
                    })
                    .collect()
            })
            .collect();
    }

    fn field(&mut self, g: &Gfx) {
        if self.pin.is_some() {
            self.field = if self.text.is_empty() {
                g.text(g.f_sub, &self.hint, 0)
            } else {
                g.text(g.f_title, "* ".repeat(self.text.len()).trim_end(), 0)
            };
            return;
        }
        let shown = if self.secret && !self.text.is_empty() {
            "*".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        };
        // Long addresses show their end, where the typing is.
        let tail: String = {
            let c: Vec<char> = shown.chars().collect();
            c[c.len().saturating_sub(if self.search { 28 } else { 56 })..]
                .iter()
                .collect()
        };
        let font = if self.search { g.f_title } else { g.f_menu };
        self.field = if self.text.is_empty() {
            g.text(font, &self.hint, 0)
        } else {
            g.text(font, &format!("{tail}_"), 0)
        };
    }

    fn press(&mut self, g: &Gfx, key: &str) -> Typed {
        match key {
            "Done" | "Results" => return Typed::Done(self.text.trim().to_string()),
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
        self.typed(g)
    }

    fn typed(&mut self, g: &Gfx) -> Typed {
        if self.pin.is_some_and(|(_, digits)| self.text.len() == digits) {
            return Typed::Done(std::mem::take(&mut self.text));
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
            .and_then(char::from_u32)
            .filter(|c| (' '..='~').contains(c) || (self.search && c.is_alphanumeric()));
        if let Some(c) = typed {
            if self.text.len() < 400 && (self.pin.is_none() || c.is_ascii_digit()) {
                self.text.push(c);
                return self.typed(g);
            }
        } else if sym == 8 {
            return self.press(g, "Delete");
        } else if ok {
            return self.press(g, self.layout()[self.sel.0][self.sel.1].0);
        } else if dx != 0 {
            let n = self.layout()[self.sel.0].len() as i32;
            self.sel.1 = (self.sel.1 as i32 + dx).clamp(0, n - 1) as usize;
        } else if dy != 0 {
            let layout = self.layout();
            let at = starts(layout[self.sel.0])[self.sel.1];
            let row = (self.sel.0 as i32 + dy).clamp(0, layout.len() as i32 - 1) as usize;
            let col = starts(layout[row]).iter().rposition(|s| *s <= at).unwrap_or(0);
            self.sel = (row, col);
        }
        Typed::Stay
    }

    fn draw_pin(&self, g: &Gfx, avatar: usize) {
        const WIDTH: f32 = 116.0;
        const HEIGHT: f32 = 80.0;
        const GAP: f32 = 12.0;
        const AVATAR: f32 = 136.0;
        let wide = 3.0 * WIDTH + 2.0 * GAP;
        let left = (W - wide) / 2.0;
        let center = |t: Option<&Tex>, cx: f32, cy: f32, color| {
            if let Some(t) = t {
                g.put(Some(t), cx - t.w as f32 / 2.0, cy - t.h as f32 / 2.0, color, 1.0);
            }
        };
        avatars::backdrop(g);

        avatars::draw(g, avatar, (W - AVATAR) / 2.0, 182.0, AVATAR);
        let field_y = if let Some(title) = &self.title {
            center(Some(title), W / 2.0, 360.0, TEXT);
            center(self.body.as_ref(), W / 2.0, 418.0, TEXT);
            465.0
        } else {
            center(self.body.as_ref(), W / 2.0, 354.0, TEXT);
            402.0
        };
        g.rounded(left, field_y, wide, 68.0, 34.0, BLACK, 0.40);
        center(self.field.as_ref(), W / 2.0, field_y + 34.0, TEXT);
        for (r, row) in self.keys.iter().enumerate() {
            for (i, key) in row.iter().enumerate() {
                let x = left + i as f32 * (WIDTH + GAP);
                let y = field_y + 88.0 + r as f32 * (HEIGHT + GAP);
                let on = self.sel == (r, i);
                let delete = r == 3 && i == 0;
                if on || !delete {
                    let fill = if on { WHITE } else { BLACK };
                    g.rounded(x, y, WIDTH, HEIGHT, HEIGHT / 2.0, fill, if on { 1.0 } else { 0.25 });
                }
                center(
                    key.as_ref(),
                    x + WIDTH / 2.0,
                    y + HEIGHT / 2.0,
                    if on { BLACK } else { TEXT },
                );
            }
        }
    }

    pub fn draw(&self, g: &Gfx) {
        if let Some((avatar, _)) = self.pin {
            return self.draw_pin(g, avatar);
        }
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
        let ph = 250.0 + body_h + self.layout().len() as f32 * (KEY_H + KEY_GAP);
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
        self.draw_keys(g, (px + (PANEL_W - gw) / 2.0, fy + 110.0), (KEY_W, KEY_H), true);
    }

    pub(crate) fn draw_search_text(&self, g: &Gfx, x: f32, cy: f32, a: f32) {
        if let Some(t) = &self.field {
            g.put(
                Some(t),
                x,
                cy - t.h as f32 / 2.0,
                if self.text.is_empty() { DIM } else { TEXT },
                a,
            );
        }
    }

    pub(crate) fn at_right_edge(&self) -> bool {
        self.sel.1 + 1 == self.layout()[self.sel.0].len()
    }

    pub(crate) fn draw_keys(&self, g: &Gfx, origin: (f32, f32), size: (f32, f32), active: bool) {
        let (kw, kh) = size;
        for (r, row) in self.layout().iter().enumerate() {
            let y = origin.1 + r as f32 * (kh + KEY_GAP);
            let mut x = origin.0;
            for (i, (k, span)) in row.iter().enumerate() {
                let w = *span as f32 * kw + (*span as f32 - 1.0) * KEY_GAP;
                let on = active && self.sel == (r, i);
                let lit = *k == "Shift" && self.shift;
                let fill = if on {
                    WHITE
                } else if lit {
                    DIM
                } else {
                    if self.search {
                        PANEL
                    } else {
                        BG
                    }
                };
                g.rounded(x, y, w, kh, if self.search { 12.0 } else { 16.0 }, fill, 1.0);
                if let Some(t) = &self.keys[r][i] {
                    g.put(
                        Some(t),
                        x + (w - t.w as f32) / 2.0,
                        y + (kh - t.h as f32) / 2.0,
                        if on { BLACK } else { TEXT },
                        1.0,
                    );
                }
                x += w + KEY_GAP;
            }
        }
    }
}
