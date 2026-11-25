//! The settings page: the categories on the left, the entries of the selected one on the right, as in
//! Android TV's settings. The page holds only what is drawn and where the focus is; `App` says what each
//! entry shows and does what a chosen entry asks for.
use crate::gfx::*;
use crate::sdl::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Jellyfin,
    JfApp,
    Youtube,
    Plex,
    PlexApp,
    /// Whether Jellyfin titles play in the launcher's own player.
    JfPlayer,
    /// The language films start in, how their subtitles start, and the subtitles' language.
    AudioLang,
    SubMode,
    SubLang,
    /// How films are delivered (as they are, or by the server), and the most the server's conversion may send.
    Direct,
    Quality,
    /// Whether DTS and TrueHD go to a receiver unconverted.
    Lossless,
    /// Installs or removes the kernel module that the receiver link needs for TrueHD and full-rate DTS-HD.
    Hbr,
    /// The switch of a Continue watching row on Home: WATCH (Jellyfin) or PLEX.
    Watching(usize),
    /// Where the TV channels come from, and the guide.
    TvSource,
    TvGuide,
    /// One group of `debloat.sh`.
    Debloat(&'static str),
    Boot,
    /// A tab after Home, by its place in the order.
    Tab(usize),
}

pub enum Value {
    Text(String),
    Switch(bool),
}

pub struct Entry {
    pub act: Act,
    pub title: String,
    pub desc: String,
    pub value: Value,
}

pub struct Category {
    pub name: String,
    pub entries: Vec<Entry>,
}

pub enum Out {
    Stay,
    Back,
    Do(Act),
}

struct EntryTex {
    act: Act,
    title: Option<Tex>,
    desc: Option<Tex>,
    value: Option<Tex>,
    switch: Option<bool>,
}

struct CatTex {
    name: Option<Tex>,
    entries: Vec<EntryTex>,
}

pub struct Page {
    title: Option<Tex>,
    help: Option<Tex>,
    cats: Vec<CatTex>,
    pub cat: usize,
    pub row: usize,
    /// The focus is on the entries, not on the categories.
    pub pane: bool,
}

const SIDE_X: f32 = MARGIN;
const SIDE_W: f32 = 440.0;
const SIDE_Y: f32 = 250.0;
const SIDE_H: f32 = 84.0;
const PANE_X: f32 = SIDE_X + SIDE_W + 80.0;
const PANE_W: f32 = W - MARGIN - PANE_X;
const ROW_STEP: f32 = 148.0;
const ENTRY_H: f32 = 132.0;
const ON: Color = rgb(52, 168, 83);
/// How many categories and entries fit above the help line; longer lists scroll with the focus.
const CATS_SHOWN: usize = 7;
const ENTRIES_SHOWN: usize = 5;

/// The first item drawn, so that `sel` is on screen.
fn first(sel: usize, shown: usize) -> usize {
    sel.saturating_sub(shown - 1)
}

fn cat_rect(i: usize) -> FRect {
    FRect {
        x: SIDE_X,
        y: SIDE_Y + i as f32 * (SIDE_H + 12.0),
        w: SIDE_W,
        h: SIDE_H,
    }
}

fn entry_rect(i: usize) -> FRect {
    FRect {
        x: PANE_X,
        y: SIDE_Y + i as f32 * ROW_STEP,
        w: PANE_W,
        h: ENTRY_H,
    }
}

impl Page {
    /// `cat`, `row` and `pane` say where the focus was, when the page is built again after a change.
    pub fn cat_count(&self) -> usize {
        self.cats.len()
    }

    pub fn new(g: &Gfx, cats: Vec<Category>, cat: usize, row: usize, pane: bool) -> Page {
        let cats: Vec<CatTex> = cats
            .into_iter()
            .map(|c| CatTex {
                name: g.text(g.f_menu, &c.name, 0),
                entries: c
                    .entries
                    .into_iter()
                    .map(|e| EntryTex {
                        act: e.act,
                        title: g.text(g.f_menu, &e.title, 0),
                        desc: g.text(g.f_label, &e.desc, (PANE_W - 72.0 - 280.0) as u32),
                        switch: match &e.value {
                            Value::Switch(on) => Some(*on),
                            Value::Text(_) => None,
                        },
                        value: match &e.value {
                            Value::Text(t) => g.text(g.f_menu, t, 0),
                            Value::Switch(_) => None,
                        },
                    })
                    .collect(),
            })
            .collect();
        let cat = cat.min(cats.len().saturating_sub(1));
        let row = row.min(cats.get(cat).map_or(0, |c| c.entries.len().saturating_sub(1)));
        Page {
            title: g.text(g.f_hero, "Settings", 0),
            help: g.text(g.f_label, "Up / Down to browse   OK to change   Back to close", 0),
            cats,
            cat,
            row,
            pane,
        }
    }

    fn entries(&self) -> usize {
        self.cats.get(self.cat).map_or(0, |c| c.entries.len())
    }

    pub fn key(&mut self, dx: i32, dy: i32, ok: bool, back: bool) -> Out {
        if back {
            return Out::Back;
        }
        if self.pane {
            if dx < 0 {
                self.pane = false;
            } else if dy != 0 {
                self.row = (self.row as i32 + dy).clamp(0, self.entries() as i32 - 1) as usize;
            } else if ok {
                return Out::Do(self.cats[self.cat].entries[self.row].act);
            }
        } else if dy != 0 {
            self.cat = (self.cat as i32 + dy).clamp(0, self.cats.len() as i32 - 1) as usize;
            self.row = 0;
        } else if (dx > 0 || ok) && self.entries() > 0 {
            self.pane = true;
        }
        Out::Stay
    }

    pub fn draw(&self, g: &Gfx) {
        let full = FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: H,
        };
        g.fill(BG, 1.0, full);
        g.put(self.title.as_ref(), MARGIN, 76.0, TEXT, 1.0);
        let c0 = first(self.cat, CATS_SHOWN);
        for (i, c) in self.cats.iter().enumerate().skip(c0).take(CATS_SHOWN) {
            let r = cat_rect(i - c0);
            let (on, here) = (i == self.cat && !self.pane, i == self.cat);
            if here {
                g.rounded(r.x, r.y, r.w, r.h, 20.0, if on { WHITE } else { PANEL }, 1.0);
            }
            if let Some(t) = &c.name {
                let colour = if on {
                    BLACK
                } else if here {
                    TEXT
                } else {
                    DIM
                };
                g.put(Some(t), r.x + 36.0, r.y + (r.h - t.h as f32) / 2.0, colour, 1.0);
            }
        }
        let Some(cat) = self.cats.get(self.cat) else { return };
        let e0 = first(self.row, ENTRIES_SHOWN);
        for (i, e) in cat.entries.iter().enumerate().skip(e0).take(ENTRIES_SHOWN) {
            let r = entry_rect(i - e0);
            let on = self.pane && i == self.row;
            g.rounded(
                r.x,
                r.y,
                r.w,
                r.h,
                20.0,
                if on { WHITE } else { PANEL },
                if on { 1.0 } else { 0.92 },
            );
            let (head, sub) = if on { (BLACK, rgb(84, 88, 95)) } else { (TEXT, DIM) };
            let th = e.title.as_ref().map_or(0.0, |t| t.h as f32);
            let dh = e.desc.as_ref().map_or(0.0, |t| t.h as f32);
            let top = r.y + (r.h - th - dh - 8.0) / 2.0;
            g.put(e.title.as_ref(), r.x + 36.0, top, head, 1.0);
            g.put(e.desc.as_ref(), r.x + 36.0, top + th + 8.0, sub, 1.0);
            if let Some(t) = &e.value {
                g.put(
                    Some(t),
                    r.x + r.w - 36.0 - t.w as f32,
                    r.y + (r.h - t.h as f32) / 2.0,
                    sub,
                    1.0,
                );
            }
            if let Some(state) = e.switch {
                let (sx, sy) = (r.x + r.w - 36.0 - 96.0, r.y + (r.h - 52.0) / 2.0);
                let track = match (state, on) {
                    (true, _) => ON,
                    (false, true) => rgb(176, 180, 186),
                    (false, false) => rgb(86, 91, 98),
                };
                g.rounded(sx, sy, 96.0, 52.0, 26.0, track, 1.0);
                let kx = if state { sx + 96.0 - 46.0 } else { sx + 6.0 };
                g.rounded(kx, sy + 6.0, 40.0, 40.0, 20.0, WHITE, 1.0);
            }
        }
        g.put(self.help.as_ref(), MARGIN, H - 96.0, DIM, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lists_scroll_and_stay_above_the_help_line() {
        assert_eq!((first(0, 7), first(6, 7), first(7, 7), first(9, 7)), (0, 0, 1, 3));
        let help = H - 96.0;
        let c = cat_rect(CATS_SHOWN - 1);
        let e = entry_rect(ENTRIES_SHOWN - 1);
        assert!(c.y + c.h < help && e.y + e.h < help);
        assert!(
            cat_rect(CATS_SHOWN).y + SIDE_H >= help - 16.0,
            "one more category would reach the help line"
        );
    }
}
