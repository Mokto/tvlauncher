//! The TV tab: a programme guide. The channels go down the left (Favourites first, then the playlist's order), and
//! two hours of their programmes run across. The focused programme is described above the grid, and OK on one
//! that has not started yet opens its details.
use crate::data::{Item, Kind};
use crate::gfx::*;
use crate::iptv::{logo_path, shows, Channel, Guide, Show};
use crate::sdl::*;
use std::collections::HashMap;

const HEAD_Y: f32 = 150.0;
const HEAD_BOTTOM: f32 = 430.0;
const RULER_Y: f32 = 446.0;
const GRID_Y: f32 = 500.0;
const ROW_H: f32 = 72.0;
const STEP: f32 = ROW_H + 8.0;
const ROWS: usize = 7;
const CH_W: f32 = 380.0;
const LOGO: (f32, f32) = (96.0, 54.0);
const TL_X: f32 = MARGIN + CH_W + 12.0;
const TL_W: f32 = W - MARGIN - TL_X;
/// The time across the grid, and the step the window moves by.
const SPAN: i64 = 2 * 3600;
const HALF: i64 = 1800;
/// Text textures kept for the programme blocks before they are all made again.
const KEEP: usize = 800;

pub enum Out {
    Stay,
    /// Up from the top: to the tabs.
    Top,
    /// Watch this channel (its index in the whole list).
    Ok(usize),
}

/// The focused programme's text: above the grid, and in the details panel.
struct Head {
    key: (usize, i64),
    title: Option<Tex>,
    meta: Option<Tex>,
    sub: Option<Tex>,
    desc: Option<Tex>,
    full: Option<Tex>,
}

pub struct Page {
    /// Channel indices in the order shown, the first `favs` of them Favourites.
    order: Vec<usize>,
    favs: usize,
    row: usize,
    /// The focused programme, by its start; None on a channel without a guide.
    sel: Option<i64>,
    /// The time Up and Down keep the focus near.
    at: i64,
    /// The left edge of the grid, on a half hour, and where it is drawn while it moves there.
    t0: i64,
    t0_drawn: f64,
    scroll: f32,
    pub details: bool,
    logos: HashMap<usize, Item>,
    names: HashMap<usize, (Option<Tex>, Option<Tex>)>,
    titles: HashMap<(usize, i64), Option<Tex>>,
    hours: HashMap<i64, Option<Tex>>,
    head: Option<Head>,
    no_guide: Option<Tex>,
    empty: Option<Tex>,
    hint: Option<Tex>,
}

fn now() -> i64 {
    crate::youtube::now() as i64
}

fn half_hour(t: i64) -> i64 {
    t - t.rem_euclid(HALF)
}

/// The programmes of a channel that have not ended.
fn ahead<'a>(guide: &'a Guide, c: &Channel, now: i64) -> impl Iterator<Item = &'a Show> {
    shows(guide, &c.epg).iter().filter(move |s| s.stop > now)
}

impl Page {
    pub fn new(g: &Gfx, list: &[Channel], favs: &[String]) -> Page {
        let fav: Vec<usize> = favs
            .iter()
            .filter_map(|k| list.iter().position(|c| c.key() == *k))
            .collect();
        let n = fav.len();
        let order = fav
            .iter()
            .copied()
            .chain((0..list.len()).filter(|i| !fav.contains(i)))
            .collect();
        let t = now();
        Page {
            order,
            favs: n,
            row: 0,
            sel: None,
            at: t,
            t0: half_hour(t),
            t0_drawn: half_hour(t) as f64,
            scroll: 0.0,
            details: false,
            logos: HashMap::new(),
            names: HashMap::new(),
            titles: HashMap::new(),
            hours: HashMap::new(),
            head: None,
            no_guide: g.text(g.f_sub, "No programme information", 0),
            empty: g.text(
                g.f_menu,
                "No channels yet. They appear here once the playlist has loaded.",
                1100,
            ),
            hint: g.text(g.f_label, "OK  Watch the channel        Back  Close", 0),
        }
    }

    /// Keeps the focus on the same channel after the list or the favourites changed.
    pub fn keep(&mut self, old: &Page, list: &[Channel], guide: &Guide) {
        if let Some(k) = old.channel().and_then(|i| list.get(i)).map(Channel::key) {
            if let Some(i) = list.iter().position(|c| c.key() == k) {
                self.focus(i, list, guide);
                self.scroll = old.scroll;
            }
        }
    }

    /// The logos of the channels on screen and around them, for the logo thread.
    pub fn logos(&self, list: &[Channel]) -> Vec<(String, String)> {
        let from = self.row.saturating_sub(ROWS);
        self.order
            .iter()
            .skip(from)
            .take(ROWS * 3)
            .filter_map(|&i| list[i].logo.as_ref().map(|l| (l.clone(), logo_path(l))))
            .collect()
    }

    /// A logo arrived (its path): the channels showing it load it again.
    pub fn logo_saved(&mut self, path: &str) {
        for it in self.logos.values_mut() {
            if it.icons.first().is_some_and(|p| p == path) {
                it.icon_tried = false;
            }
        }
    }

    /// The guide changed: its text is made again.
    pub fn forget(&mut self) {
        self.titles.clear();
        self.head = None;
        self.sel = None;
    }

    /// The focused channel, as an index in the whole list.
    pub fn channel(&self) -> Option<usize> {
        self.order.get(self.row).copied()
    }

    pub fn row(&self) -> usize {
        self.row
    }

    fn show<'a>(&self, list: &[Channel], guide: &'a Guide) -> Option<&'a Show> {
        let c = &list[self.channel()?];
        shows(guide, &c.epg).iter().find(|s| Some(s.start) == self.sel)
    }

    /// Puts the focus on what is on now on a channel (after watching, the one it ended on).
    pub fn focus(&mut self, channel: usize, list: &[Channel], guide: &Guide) {
        if let Some(r) = self.order.iter().position(|&c| c == channel) {
            self.row = r;
            self.at = now();
            self.pick(list, guide);
        }
    }

    /// Focuses the programme on the row that is on at `at`, else the first one after it.
    fn pick(&mut self, list: &[Channel], guide: &Guide) {
        let Some(c) = self.channel().map(|i| &list[i]) else {
            return;
        };
        let mut it = ahead(guide, c, now());
        let first = it.next();
        self.sel = std::iter::once(first)
            .flatten()
            .chain(it)
            .find(|s| s.stop > self.at)
            .or(first)
            .map(|s| s.start);
    }

    pub fn key(&mut self, list: &[Channel], guide: &Guide, dx: i32, dy: i32) -> Out {
        if self.order.is_empty() {
            return if dy < 0 { Out::Top } else { Out::Stay };
        }
        let t = now();
        if dy < 0 && self.row == 0 {
            return Out::Top;
        }
        if dy != 0 {
            self.row = (self.row as i32 + dy).clamp(0, self.order.len() as i32 - 1) as usize;
            self.pick(list, guide);
            return Out::Stay;
        }
        let Some(c) = self.channel().map(|i| &list[i]) else {
            return Out::Stay;
        };
        let row: Vec<&Show> = ahead(guide, c, t).collect();
        let Some(i) = row.iter().position(|s| Some(s.start) == self.sel) else {
            return Out::Stay;
        };
        if let Some(s) = row.get((i as i32 + dx).max(0) as usize) {
            self.sel = Some(s.start);
            self.at = s.start.max(t);
            let start = s.start.max(t);
            if start < self.t0 || start >= self.t0 + SPAN - HALF {
                self.t0 = (half_hour(start) - if dx > 0 { HALF } else { 0 }).max(half_hour(t));
            }
        }
        Out::Stay
    }

    /// OK released on the grid: a programme that is on (or a channel without a guide) plays, a later one shows
    /// its details, and OK in the details plays the channel.
    pub fn tap(&mut self, list: &[Channel], guide: &Guide) -> Out {
        let Some(ch) = self.channel() else { return Out::Stay };
        if !self.details && self.show(list, guide).is_some_and(|s| s.start > now()) {
            self.details = true;
            return Out::Stay;
        }
        self.details = false;
        Out::Ok(ch)
    }

    /// Back on the grid: to what is on now, if the focus is on later programmes. False when it already is.
    pub fn rewind(&mut self, list: &[Channel], guide: &Guide) -> bool {
        let t = now();
        if self.t0 <= half_hour(t) && self.show(list, guide).is_none_or(|s| s.start <= t) {
            return false;
        }
        self.at = t;
        self.t0 = half_hour(t);
        self.pick(list, guide);
        true
    }

    /// Opens the details of the focused programme, if it has any.
    pub fn open_details(&mut self, list: &[Channel], guide: &Guide) {
        self.details = self.show(list, guide).is_some();
    }

    /// Eases the scrolling; true while something moves.
    pub fn animate(&mut self, dt: f32) -> bool {
        let t0 = self.t0.max(half_hour(now()));
        self.t0 = t0;
        let left = t0 as f64 - self.t0_drawn;
        let moving = left.abs() > 1.0;
        self.t0_drawn = if moving {
            self.t0_drawn + left * f64::from((dt * 12.0).min(1.0))
        } else {
            t0 as f64
        };
        let max = self.order.len().saturating_sub(ROWS) as f32;
        let target = (self.row as f32 - 3.0).clamp(0.0, max);
        ease(&mut self.scroll, target, dt, 12.0) || moving
    }

    fn make_head(&mut self, g: &Gfx, list: &[Channel], guide: &Guide) {
        let Some(ch) = self.channel() else { return };
        let key = (ch, self.sel.unwrap_or(0));
        if self.head.as_ref().is_some_and(|h| h.key == key) {
            return;
        }
        let c = &list[ch];
        let head = match self.show(list, guide) {
            Some(s) => {
                let mut meta = format!("{} – {}    {}", clock(s.start), clock(s.stop), short(&c.name));
                if !s.cat.is_empty() {
                    meta += &format!("    {}", s.cat);
                }
                Head {
                    key,
                    title: g.text(g.f_title, &s.title, 1600),
                    meta: g.text(g.f_sub, &meta, 0),
                    sub: (!s.sub.is_empty()).then(|| g.text(g.f_row, &s.sub, 1600)).flatten(),
                    desc: (!s.desc.is_empty())
                        .then(|| g.text(g.f_sub, &cut(&s.desc, 190), 1500))
                        .flatten(),
                    full: None,
                }
            }
            None => Head {
                key,
                title: g.text(g.f_title, short(&c.name), 1600),
                meta: g.text(g.f_sub, &format!("{}    No programme information", c.group), 0),
                sub: None,
                desc: None,
                full: None,
            },
        };
        self.head = Some(head);
    }

    pub fn draw(&mut self, g: &Gfx, list: &[Channel], guide: &Guide, active: bool) {
        if self.order.is_empty() {
            g.put(self.empty.as_ref(), MARGIN, HEAD_Y + 16.0, DIM, 1.0);
            return;
        }
        let t = now();
        // Nothing focused yet, a new guide, or the programme ended: the one on at the focus time.
        if self.show(list, guide).is_none_or(|s| s.stop <= t) {
            self.at = self.at.max(t);
            self.pick(list, guide);
        }
        self.make_head(g, list, guide);
        if let Some(h) = &self.head {
            let mut y = HEAD_Y;
            for (tex, c, gap) in [(&h.title, TEXT, 8.0), (&h.meta, DIM, 14.0), (&h.sub, TEXT, 8.0)] {
                if let Some(tx) = tex {
                    g.put(Some(tx), MARGIN, y, c, 1.0);
                    y += tx.h as f32 + gap;
                }
            }
            clipped(g, h.desc.as_ref(), MARGIN, y, W, HEAD_BOTTOM - y, DIM);
        }

        let pps = TL_W as f64 / SPAN as f64;
        let x_of = |s: i64| TL_X + ((s as f64 - self.t0_drawn) * pps) as f32;
        let first = half_hour(self.t0_drawn as i64);
        for k in 0..=SPAN / HALF + 1 {
            let at = first + k * HALF;
            let x = x_of(at);
            if !(TL_X - 1.0..TL_X + TL_W - 80.0).contains(&x) {
                continue;
            }
            let tex = self.hours.entry(at).or_insert_with(|| g.text(g.f_label, &clock(at), 0));
            g.put(tex.as_ref(), x + 4.0, RULER_Y, DIM, 1.0);
        }
        if self.hours.len() > 64 {
            self.hours.clear();
        }

        let xn = x_of(t);
        if (TL_X..TL_X + TL_W).contains(&xn) {
            let bottom = (GRID_Y + (self.order.len() as f32 - self.scroll) * STEP).min(H);
            g.rounded(xn - 6.0, RULER_Y + 38.0, 12.0, 12.0, 6.0, WHITE, 1.0);
            let line = FRect {
                x: xn - 1.0,
                y: RULER_Y + 44.0,
                w: 2.0,
                h: bottom - RULER_Y - 44.0,
            };
            g.fill(WHITE, 0.8, line);
        }
        if self.titles.len() > KEEP {
            self.titles.clear();
        }
        let top = self.scroll.floor() as usize;
        let sel = (self.row, self.sel);
        for r in top..(top + ROWS + 1).min(self.order.len()) {
            let y = GRID_Y + (r as f32 - self.scroll) * STEP;
            if y > H {
                break;
            }
            let fade = clamp(1.0 - (GRID_Y - y) / STEP, 0.0, 1.0);
            if fade <= 0.0 {
                continue;
            }
            let ci = self.order[r];
            let c = &list[ci];
            let here = r == sel.0;
            if here {
                g.rounded(MARGIN - 16.0, y, CH_W, ROW_H, 14.0, PANEL, fade);
            }
            if r < self.favs {
                g.rounded(MARGIN - 34.0, y + ROW_H / 2.0 - 5.0, 10.0, 10.0, 5.0, YELLOW, fade);
            }
            let (num, name) = self.names.entry(ci).or_insert_with(|| {
                (
                    g.text(g.f_label, &(ci + 1).to_string(), 0),
                    g.text(g.f_row, short(&c.name), 0),
                )
            });
            if let Some(n) = num {
                g.put(
                    Some(n),
                    MARGIN + 52.0 - n.w as f32,
                    y + (ROW_H - n.h as f32) / 2.0,
                    DIM,
                    fade,
                );
            }
            let it = self.logos.entry(ci).or_insert_with(|| {
                let mut it = Item::new(c.key(), String::new(), Kind::Watch, Some(" ".into()));
                it.icons = c.logo.iter().map(|l| logo_path(l)).collect();
                it.color = rgb(58, 62, 70);
                it
            });
            g.card(it, LOGO, MARGIN + 68.0, y + (ROW_H - LOGO.1) / 2.0, fade, 0.0);
            let nx = MARGIN + 68.0 + LOGO.0 + 16.0;
            if let Some(n) = name {
                clipped(
                    g,
                    Some(n),
                    nx,
                    y + (ROW_H - n.h as f32) / 2.0,
                    MARGIN + CH_W - 32.0 - nx,
                    ROW_H,
                    TEXT,
                );
            }

            let row: Vec<&Show> = ahead(guide, c, t).collect();
            if row.is_empty() {
                let on = active && here;
                g.rounded(TL_X, y, TL_W, ROW_H, 12.0, if on { WHITE } else { PANEL }, fade);
                if let Some(n) = &self.no_guide {
                    let c = if on { BLACK } else { DIM };
                    g.put(Some(n), TL_X + 20.0, y + (ROW_H - n.h as f32) / 2.0, c, fade);
                }
                continue;
            }
            for s in row {
                let x0 = x_of(s.start).max(TL_X);
                let x1 = x_of(s.stop).min(TL_X + TL_W);
                if x1 - x0 < 6.0 {
                    continue;
                }
                let on = active && here && sel.1 == Some(s.start);
                let colour = if on {
                    WHITE
                } else if s.start <= t {
                    rgb(58, 62, 70)
                } else {
                    PANEL
                };
                g.rounded(x0 + 2.0, y, x1 - x0 - 4.0, ROW_H, 12.0, colour, fade);
                let tex = self
                    .titles
                    .entry((ci, s.start))
                    .or_insert_with(|| g.text(g.f_sub, &s.title, 0));
                if let Some(tx) = tex {
                    let ty = y + (ROW_H - tx.h as f32) / 2.0;
                    let c = if on { BLACK } else { TEXT };
                    let c = Color {
                        a: (fade * 255.0) as u8,
                        ..c
                    };
                    clipped(g, Some(tx), x0 + 18.0, ty, x1 - x0 - 34.0, ROW_H, c);
                }
            }
        }
        if self.details {
            self.draw_details(g, list, guide);
        }
    }

    fn draw_details(&mut self, g: &Gfx, list: &[Channel], guide: &Guide) {
        let Some(h) = &mut self.head else { return };
        if h.full.is_none() {
            let desc = self.sel.and_then(|_| {
                let c = &list[self.order[self.row]];
                shows(guide, &c.epg).iter().find(|s| Some(s.start) == self.sel)
            });
            h.full = desc.and_then(|s| g.text(g.f_sub, &s.desc, (W - 720.0) as u32));
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
        let text_h: f32 = [&h.title, &h.meta, &h.sub, &h.full]
            .iter()
            .filter_map(|t| t.as_ref().map(|t| t.h as f32 + 16.0))
            .sum();
        let (pw, ph) = (W - 600.0, (text_h + 200.0).clamp(320.0, H - 200.0));
        let (px, py) = (300.0, (H - ph) / 2.0);
        g.rounded(px, py, pw, ph, 28.0, PANEL, 1.0);
        let (x, mut y) = (px + 60.0, py + 56.0);
        for (tex, c, gap) in [(&h.title, TEXT, 8.0), (&h.meta, DIM, 18.0), (&h.sub, TEXT, 14.0)] {
            if let Some(tx) = tex {
                clipped(g, Some(tx), x, y, pw - 120.0, ph, c);
                y += tx.h as f32 + gap;
            }
        }
        clipped(g, h.full.as_ref(), x, y, pw - 120.0, py + ph - 110.0 - y, TEXT);
        g.put(self.hint.as_ref(), x, py + ph - 76.0, DIM, 1.0);
    }
}

/// Draws the top left of a text, at most `w` by `h`.
fn clipped(g: &Gfx, t: Option<&Tex>, x: f32, y: f32, w: f32, h: f32, c: Color) {
    let Some(t) = t else { return };
    let (w, h) = ((t.w as f32).min(w).floor(), (t.h as f32).min(h).floor());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    g.tint(t, c, f32::from(c.a) / 255.0);
    let src = Rect {
        x: 0,
        y: 0,
        w: w as i32,
        h: h as i32,
    };
    g.copy(t, Some(src), FRect { x, y, w, h });
}

/// A channel name without the notes playlists add, such as "(1080p)" or "[Not 24/7]".
fn short(name: &str) -> &str {
    let mut n = name.trim_end();
    if let Some((rest, _)) = n.strip_suffix(']').and_then(|n| n.rsplit_once(" [")) {
        n = rest;
    }
    match n.strip_suffix("p)").and_then(|n| n.rsplit_once(" (")) {
        Some((rest, size)) if size.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => n,
    }
}

/// At most about `n` characters of `s`, cut at a word.
fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", s[..at].rsplit_once(' ').map_or(&s[..at], |(a, _)| a)),
        None => s.into(),
    }
}

pub fn clock(t: i64) -> String {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&(t as libc::time_t), &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_descriptions_are_shortened() {
        assert_eq!(short("TF1 (1080p)"), "TF1");
        assert_eq!(short("Antenne Reunion (720p) [Not 24/7]"), "Antenne Reunion");
        assert_eq!(short("France 24 (English)"), "France 24 (English)");
        assert_eq!(short("M6"), "M6");
        assert_eq!(cut("one two three", 9), "one two…");
        assert_eq!(cut("short", 9), "short");
    }
}
