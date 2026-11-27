//! What the viewer sees and presses while a title plays: the remote, the bar and its buttons, the panels and the
//! subtitles (text, and the bitmaps of PGS and DVD tracks).
use super::super::*;
use crate::ff::demux::{Bitmap, SubEv, SubWhat};
use crate::film::extras::{chapter_at, Chapter, SegKind};
use crate::film::{self, Item, TICKS};
use crate::icons::Icon;
use crate::subs::Cue;
use std::ffi::c_int;

pub enum Fk {
    Back,
    Stop,
    /// OK: pauses, or skips what the button on screen offers.
    Toggle,
    /// Dedicated transport key, independent of the focused control.
    Pause,
    /// The next (1) or the previous (-1) chapter.
    Chapter(i32),
    /// Left (-1) or Right (1) went down; held, the key comes again (the caller tells a hold from a tap).
    Step(i64),
    /// Left or Right came up.
    Release,
    /// The rewind and fast-forward buttons: seconds.
    Seek(i64),
    Up,
    Down,
}

pub const AUDIO_PLAY: i32 = (1 << 30) | 261;

/// The remote while a title plays.
pub fn fkey() -> Option<Fk> {
    if STOP.load(Ordering::SeqCst) || crate::lifecycle::RELAUNCH.load(Ordering::SeqCst) {
        return Some(Fk::Stop);
    }
    let mut e = Event::new(0);
    while unsafe { SDL_PollEvent(&mut e) } != 0 {
        if e.kind() == QUIT {
            unsafe { SDL_PushEvent(&mut Event::new(QUIT)) };
            return Some(Fk::Stop);
        }
        let (sym, wcode) = (e.key_sym(), e.key_wcode());
        if e.kind() == KEYUP && (sym == K_LEFT || sym == K_RIGHT) {
            return Some(Fk::Release);
        }
        if e.kind() != KEYDOWN {
            continue;
        }
        if wcode == 413 {
            return Some(Fk::Stop);
        }
        if sym == K_ESCAPE || wcode == WCODE_BACK {
            if e.key_repeat() {
                continue;
            }
            return Some(Fk::Back);
        }
        match (sym, wcode) {
            // 415 and 19 are the remote's Play and Pause
            (K_RETURN | K_KP_ENTER, _) => return Some(Fk::Toggle),
            (AUDIO_PLAY, _) | (_, 415 | 19) => return Some(Fk::Pause),
            (K_PAGEDOWN, _) => return Some(Fk::Chapter(1)),
            (K_PAGEUP, _) => return Some(Fk::Chapter(-1)),
            (K_LEFT, _) => return Some(Fk::Step(-1)),
            (K_RIGHT, _) => return Some(Fk::Step(1)),
            (_, 412) => return Some(Fk::Seek(-30)),
            (_, 417) => return Some(Fk::Seek(30)),
            (K_UP, _) => return Some(Fk::Up),
            (K_DOWN, _) => return Some(Fk::Down),
            _ => log(&format!("film: key {sym} wcode {wcode}")),
        }
    }
    None
}

// ---- the look of subtitles, and the skip step ---------------------------------------------------------------------

/// The choices of Options: subtitle sizes (pixels), how far above the usual place they stand, their colours (pure
/// white is harsh on an HDR picture) and the skip steps.
pub const SIZES: [(&str, c_int); 4] = [("Small", 36), ("Normal", 44), ("Large", 56), ("Huge", 68)];
pub const RAISES: [(&str, f32); 3] = [("Low", 0.0), ("Higher", 90.0), ("Highest", 180.0)];
pub const COLORS: [(&str, u8); 5] = [
    ("White", 255),
    ("Grey 85 %", 217),
    ("Grey 70 %", 179),
    ("Grey 55 %", 140),
    ("Grey 40 %", 102),
];
pub const SKIPS: [i64; 5] = [5, 10, 15, 30, 60];

pub use crate::data::Look;

/// Keeps a look read from a hand-edited file inside the lists.
pub fn clamp_look(l: Look) -> Look {
    Look {
        sub_size: l.sub_size.min(SIZES.len() - 1),
        sub_pos: l.sub_pos.min(RAISES.len() - 1),
        sub_color: l.sub_color.min(COLORS.len() - 1),
        skip: l.skip.min(SKIPS.len() - 1),
        ..l
    }
}

fn sub_ink(l: &Look) -> Color {
    let v = COLORS[l.sub_color.min(COLORS.len() - 1)].1;
    rgb(v, v, v)
}

// ---- seeking ------------------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct SeekTaps {
    dir: i64,
    count: u8,
}

impl SeekTaps {
    pub fn step(&mut self, dir: i64, step: i64) -> (i64, u8) {
        if self.dir != dir {
            *self = Self { dir, count: 0 };
        }
        self.count = (self.count + 1).min(5);
        let level = self.count.saturating_sub(2);
        (dir * (step << level), level + 1)
    }
}

/// A held Left or Right: the place it has reached, in seconds, and since when it is held.
pub struct Scrub {
    pub dir: i64,
    pub at: f64,
    since: Instant,
    last: Instant,
}

impl Scrub {
    pub fn new(dir: i64, at: f64) -> Scrub {
        Scrub {
            dir,
            at,
            since: Instant::now(),
            last: Instant::now(),
        }
    }

    /// Seconds of film a second of holding moves, by how long the key has been held: none at first (a tap), then
    /// faster in steps, so that a whole film takes a quarter of a minute.
    fn rate(held: f64) -> (f64, u8) {
        match held {
            h if h < 0.45 => (0.0, 0),
            h if h < 2.0 => (20.0, 1),
            h if h < 4.0 => (60.0, 2),
            h if h < 6.0 => (180.0, 3),
            _ => (480.0, 4),
        }
    }

    /// Moves on by the time since the last frame, inside `0..=top`. Gives the speed step (0 to 4), or None when the
    /// key has been held so long that its release must have been lost.
    pub fn advance(&mut self, top: i64) -> Option<u8> {
        let held = self.since.elapsed().as_secs_f64();
        if held > 120.0 {
            return None;
        }
        let (rate, level) = Scrub::rate(held);
        self.at = (self.at + self.dir as f64 * rate * self.last.elapsed().as_secs_f64()).clamp(0.0, top as f64);
        self.last = Instant::now();
        Some(level)
    }
}

// ---- the bar ------------------------------------------------------------------------------------------------------

/// Where the bar is on screen: its left edge, width and the top of its panel, and how far along it is (0 to 1).
pub struct Bar {
    x: f32,
    w: f32,
    top: f32,
    frac: f32,
}

/// The buttons on the bar, each opening a panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Btn {
    Subs,
    Audio,
    Chapters,
    Options,
    Info,
}

impl Btn {
    fn label(self) -> &'static str {
        match self {
            Btn::Subs => "Subtitles",
            Btn::Audio => "Audio",
            Btn::Chapters => "Chapters",
            Btn::Options => "Options",
            Btn::Info => "Info",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Btn::Subs => Icon::Subtitles,
            Btn::Audio => Icon::Audio,
            Btn::Chapters => Icon::Chapters,
            Btn::Options => Icon::Options,
            Btn::Info => Icon::Info,
        }
    }
}

/// Where a seek is going: the place in seconds, where it began, and the speed step of a held key (0 to 4).
#[derive(Clone, Copy)]
pub struct Seeking {
    pub to: i64,
    pub from: i64,
    pub level: u8,
}

/// A text and its picture, made again only when the text changes.
#[derive(Default)]
struct Label(String, Option<Tex>);

impl Label {
    fn set(&mut self, g: &Gfx, font: *mut Opaque, s: &str) -> Option<&Tex> {
        if self.0 != s {
            *self = Label(s.to_string(), g.text(font, s, 0));
        }
        self.1.as_ref()
    }
}

/// What the screen says while the title loads: the stage it is at, and whether it covers the screen (the first
/// start) or is a spinner over the bar (a seek, another track).
#[derive(Clone, Copy)]
pub struct Loading<'a> {
    pub stage: &'a str,
    pub full: bool,
}

/// Twelve dots round a circle of radius `r` at `(cx, cy)`, the brightest going round once a second.
fn spinner(g: &Gfx, cx: f32, cy: f32, r: f32, t: f32) {
    let dot = r * 0.22;
    for i in 0..12 {
        let a = i as f32 / 12.0 * std::f32::consts::TAU;
        let behind = (t * 12.0 - i as f32).rem_euclid(12.0) / 12.0;
        let (x, y) = (cx + r * a.sin(), cy - r * a.cos());
        g.rounded(x - dot, y - dot, 2.0 * dot, 2.0 * dot, dot, TEXT, 1.0 - 0.85 * behind);
    }
}

/// The title, the bar, the times and the buttons, at the bottom of the picture for a few seconds, and while paused.
pub struct Osd {
    name: Option<Tex>,
    sub: Option<Tex>,
    big_name: Option<Tex>,
    stage: Label,
    opening: bool,
    opening_fade: Option<Instant>,
    started: Instant,
    /// The title's backdrop from the launcher's cache, read on a thread, behind the loading screen.
    backdrop: Option<std::sync::mpsc::Receiver<Option<Surf>>>,
    backdrop_tex: Option<(Tex, Instant)>,
    total: Option<Tex>,
    total_s: i64,
    at: Label,
    ends: Label,
    delta: Label,
    up_hint: Option<Tex>,
    status: Option<Tex>,
    chapters: Vec<Chapter>,
    /// The chapter whose name is drawn, and the name.
    chapter: (Option<usize>, Option<Tex>),
    skip: Option<(SegKind, Tex)>,
    /// A status that goes by itself (a failed subtitle download), unlike an error that stays until Back.
    status_until: Option<Instant>,
    until: Option<Instant>,
    seek_hint: Option<Tex>,
    buttons: Vec<(Btn, Option<Tex>)>,
    /// The focused button, when the focus is on the buttons rather than on the bar.
    pub focus: Option<usize>,
}

impl Osd {
    pub fn new(g: &Gfx, it: &Item) -> Osd {
        let total_s = it.total / TICKS;
        Osd {
            name: g.text(g.f_title, &crate::jellyfin::clip(&it.name, 40), 0),
            sub: g.text(g.f_sub, &it.sub, 1000),
            big_name: g.text(g.f_hero, &crate::jellyfin::clip(&it.name, 76), 1480),
            stage: Label::default(),
            opening: false,
            opening_fade: None,
            started: Instant::now(),
            backdrop: Some({
                let (tx, rx) = std::sync::mpsc::channel();
                let root = super::media_root();
                let (pre, base) = match &it.backend {
                    film::Backend::Jellyfin(_) => (crate::library::JELLYFIN, None),
                    film::Backend::Plex(conn) => (crate::library::PLEX, Some(conn.base.clone())),
                };
                let (id, series) = (it.id.clone(), it.series.clone());
                std::thread::spawn(move || {
                    let art = backdrop_file(&root, pre, base.as_deref(), &id, series.as_deref());
                    let _ = tx.send(art.and_then(|p| Surf::load(&p)).and_then(|s| blur(&s)));
                });
                rx
            }),
            backdrop_tex: None,
            total: g.text(g.f_sub, &film::clock(total_s), 0),
            total_s,
            at: Label::default(),
            ends: Label::default(),
            delta: Label::default(),
            up_hint: g.text(g.f_label, "Up to seek bar", 0),
            status: None,
            chapters: it.chapters.clone(),
            chapter: (None, None),
            skip: None,
            status_until: None,
            until: Some(Instant::now() + Duration::from_secs(6)),
            seek_hint: g.text(g.f_label, "OK to jump   ·   Back to cancel", 0),
            buttons: vec![],
            focus: None,
        }
    }

    pub fn set_buttons(&mut self, g: &Gfx, list: &[Btn]) {
        if self.buttons.iter().map(|b| b.0).ne(list.iter().copied()) {
            self.buttons = list.iter().map(|b| (*b, g.text(g.f_label, b.label(), 0))).collect();
            self.focus = None;
        }
    }

    pub fn button(&self) -> Option<Btn> {
        self.buttons.get(self.focus?).map(|b| b.0)
    }

    /// Down: the focus goes to the first button (or stays where it is).
    pub fn focus_buttons(&mut self) {
        self.show();
        if self.focus.is_none() && !self.buttons.is_empty() {
            self.focus = Some(0);
        }
    }

    pub fn step(&mut self, d: i64) {
        if let Some(f) = self.focus {
            self.focus = Some((f as i64 + d).clamp(0, self.buttons.len() as i64 - 1) as usize);
        }
        self.show();
    }

    pub fn show(&mut self) {
        self.until = Some(Instant::now() + Duration::from_secs(5));
    }

    pub fn controls_visible(&self, paused: bool) -> bool {
        self.until.is_some_and(|until| paused || Instant::now() < until)
            || (self.status.is_some() && self.status_until.is_none_or(|until| Instant::now() < until))
    }

    pub fn hide(&mut self) {
        self.until = None;
        self.focus = None;
        (self.status, self.status_until) = (None, None);
    }

    pub fn say(&mut self, g: &Gfx, s: Option<&str>) {
        self.status = s.and_then(|s| g.text(g.f_sub, s, 1500));
        self.status_until = None;
        self.show();
    }

    pub fn note(&mut self, g: &Gfx, s: &str) {
        self.say(g, Some(s));
        self.status_until = Some(Instant::now() + Duration::from_secs(3));
    }

    pub fn finish_opening(&mut self) {
        if std::mem::take(&mut self.opening) {
            self.opening_fade = Some(Instant::now());
        }
        self.backdrop = None;
    }

    fn draw_loading(&mut self, g: &Gfx, stage: Option<&str>, opacity: f32) {
        if let Some(stage) = stage {
            self.opening = true;
            self.opening_fade = None;
            if let Some(Ok(s)) = self.backdrop.as_ref().map(|r| r.try_recv()) {
                self.backdrop = None;
                self.backdrop_tex = s.and_then(|s| Tex::new(g.ren, &s)).map(|t| (t, Instant::now()));
            }
            let label = if self.started.elapsed() < Duration::from_secs(8) {
                "Starting playback"
            } else {
                stage
            };
            self.stage.set(g, g.f_sub, label);
        }
        let base = rgb(10, 13, 21);
        if let Some((b, arrived)) = &self.backdrop_tex {
            let a = (arrived.elapsed().as_secs_f32() / 0.20).min(1.0);
            if a < 1.0 {
                g.fill(base, opacity * (1.0 - a) / (1.0 - opacity * a), SCREEN);
            }
            g.tint(b, rgb(166, 168, 176), a * opacity);
            g.copy(b, None, SCREEN);
        } else {
            g.fill(base, opacity, SCREEN);
        }
        g.tint(&g.vgrad, BLACK, 0.32 * opacity);
        g.copy(&g.vgrad, None, SCREEN);
        let t = self.started.elapsed().as_secs_f32();
        let a = ((t - 0.12) / 0.18).clamp(0.0, 1.0) * opacity;
        let (nh, sh) = (
            self.big_name.as_ref().map_or(0, |t| t.h),
            self.sub.as_ref().map_or(0, |t| t.h),
        );
        let head_h = (nh + if sh > 0 { sh + 10 } else { 0 }) as f32;
        let top = H / 2.0 - (head_h + 100.0) / 2.0;
        let center = |tex: Option<&Tex>, y, ink| {
            if let Some(tex) = tex {
                g.put(Some(tex), (W - tex.w as f32) / 2.0, y, ink, a);
            }
        };
        center(self.big_name.as_ref(), top, WHITE);
        center(self.sub.as_ref(), top + nh as f32 + 10.0, TEXT);
        let (w, segment) = (176.0, 48.0);
        let x = (W - w) / 2.0;
        let y = top + head_h + 46.0;
        let travel = (1.0 - (t * std::f32::consts::TAU / 1.8).cos()) * 0.5;
        g.rounded(x, y, w, 5.0, 2.5, WHITE, a * 0.22);
        g.rounded(x + (w - segment) * travel, y, segment, 5.0, 2.5, WHITE, a);
        center(self.stage.1.as_ref(), y + 24.0, TEXT);
    }

    /// Where the bar is, when it was drawn.
    fn draw(&mut self, g: &Gfx, pos_s: i64, paused: bool, seeking: Option<Seeking>, panel: bool) -> Option<Bar> {
        if self.status_until.is_some_and(|t| Instant::now() > t) {
            (self.status, self.status_until) = (None, None);
        }
        if seeking.is_none() && !self.controls_visible(paused) {
            self.focus = None;
            return None;
        }
        if paused && seeking.is_none() && self.focus.is_none() && !panel {
            let d = 132.0;
            g.rounded((W - d) / 2.0, (H - d) / 2.0 - 60.0, d, d, d / 2.0, PANEL, 0.75);
            g.icon(Icon::Pause, (W - 60.0) / 2.0, (H - 60.0) / 2.0 - 60.0, 60.0, TEXT, 1.0);
        }
        let shown = seeking.map_or(pos_s, |s| s.to);
        let (x, y, w) = (MARGIN, H - 294.0, W - 2.0 * MARGIN);
        let fade = H - 520.0;
        let solid = y + 74.0;
        g.tint(&g.vgrad, BLACK, 0.90);
        g.copy(
            &g.vgrad,
            None,
            FRect {
                x: 0.0,
                y: fade,
                w: W,
                h: solid - fade,
            },
        );
        g.fill(
            BLACK,
            0.90,
            FRect {
                x: 0.0,
                y: solid,
                w: W,
                h: H - solid,
            },
        );
        g.put(self.name.as_ref(), x, y, WHITE, 1.0);
        if seeking.is_none() {
            g.put(self.sub.as_ref(), x, y + 55.0, TEXT, 0.85);
        }
        if seeking.is_none() && !panel && self.focus.is_some() {
            if let Some(t) = &self.up_hint {
                g.put(Some(t), x + w - t.w as f32, y + 64.0, TEXT, 0.90);
            }
        }
        let button_y = y + 192.0;
        let mut bx = x + w;
        for (i, (b, t)) in self.buttons.iter().enumerate().rev() {
            let tw = t.as_ref().map_or(0.0, |t| t.w as f32);
            let (pw, ph, ic) = (tw + 82.0, 64.0, 30.0);
            bx -= pw;
            let on = self.focus == Some(i);
            let active = on && !panel;
            let ink = if active { rgb(20, 22, 25) } else { TEXT };
            if active {
                g.rounded(bx, button_y, pw, ph, 14.0, WHITE, 1.0);
            } else if on {
                g.rounded(bx, button_y, pw, ph, 14.0, WHITE, 0.14);
            }
            let alpha = if panel && !on { 0.56 } else { 1.0 };
            g.icon(b.icon(), bx + 20.0, button_y + (ph - ic) / 2.0, ic, ink, alpha);
            let th = t.as_ref().map_or(0.0, |t| t.h as f32);
            g.put(t.as_ref(), bx + 62.0, button_y + (ph - th) / 2.0, ink, alpha);
            bx -= 8.0;
        }
        let (bx, bw, by) = (x, w, y + 114.0);
        let frac = if self.total_s > 0 {
            (shown as f32 / self.total_s as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bar = Some(Bar {
            x: bx,
            w: bw,
            top: y,
            frac,
        });
        if let Some(s) = self.status.as_ref().filter(|_| seeking.is_none()) {
            g.put(Some(s), x, by + 24.0, TEXT, 1.0);
            return bar;
        }
        let timeline = self.focus.is_none() && !panel;
        g.rounded(bx, by, bw, 6.0, 3.0, WHITE, 0.33);
        if bw * frac >= 6.0 {
            g.rounded(bx, by, bw * frac, 6.0, 3.0, WHITE, if timeline { 1.0 } else { 0.65 });
        }
        if self.total_s > 0 {
            for c in self.chapters.iter().filter(|c| c.start > 0) {
                let f = (c.start / TICKS) as f32 / self.total_s as f32;
                g.rounded(bx + bw * f - 2.0, by - 7.0, 4.0, 20.0, 2.0, WHITE, 0.50);
            }
        }
        let head = bx + bw * frac;
        let (r, ink) = if timeline { (13.0, WHITE) } else { (8.0, DIM) };
        g.rounded(head - r, by + 3.0 - r, 2.0 * r, 2.0 * r, r, ink, 1.0);
        if let Some(s) = seeking {
            let d = s.to - s.from;
            let text = format!("{}{}", if d < 0 { "-" } else { "+" }, film::clock(d.abs()));
            if let Some(t) = self.delta.set(g, g.f_sub, &text) {
                let arrows = f32::from(s.level.max(1));
                let arrow_w = 14.0 * arrows + 12.0;
                let tw = t.w as f32 + arrow_w;
                let x = (head - tw / 2.0).clamp(bx, bx + bw - tw);
                let y = by - 18.0 - t.h as f32;
                let (icon, tx, ax) = if d < 0 {
                    (Icon::Left, x + arrow_w, x)
                } else {
                    (Icon::Right, x, x + t.w as f32 + 12.0)
                };
                g.put(Some(t), tx, y, TEXT, 1.0);
                for i in 0..s.level.max(1) {
                    g.icon(
                        icon,
                        ax + f32::from(i) * 14.0,
                        y + (t.h as f32 - 24.0) / 2.0,
                        24.0,
                        TEXT,
                        1.0,
                    );
                }
            }
        }
        if seeking.is_some() {
            g.put(self.seek_hint.as_ref(), x, button_y + 18.0, TEXT, 1.0);
        }
        if seeking.is_none() {
            let state = if paused { Icon::Pause } else { Icon::Play };
            g.icon(state, bx, by + 28.0, 30.0, TEXT, 1.0);
        }
        let tx = bx + 46.0;
        let at = self.at.set(g, g.f_sub, &film::clock(shown)).map(|t| (t.w as f32, t));
        if let Some((_, t)) = at.filter(|_| seeking.is_none()) {
            g.put(Some(t), tx, by + 24.0, TEXT, 1.0);
        }
        let at_w = at.map_or(0.0, |a| a.0);
        let now = chapter_at(&self.chapters, shown * TICKS);
        if self.chapter.0 != now {
            let name = now.map(|i| crate::jellyfin::clip(&self.chapters[i].name, 60));
            self.chapter = (now, name.and_then(|n| g.text(g.f_sub, &n, 0)));
        }
        if let Some(n) = self.chapter.1.as_ref().filter(|_| seeking.is_none()) {
            g.put(Some(n), tx + at_w + 36.0, by + 24.0, TEXT, 0.82);
        }
        if let Some(t) = &self.total {
            g.put(Some(t), bx + bw - t.w as f32, by + 24.0, TEXT, 1.0);
        }
        if self.total_s > 0 && seeking.is_none() {
            let left = (self.total_s - shown).max(0);
            let now = unsafe { libc::time(std::ptr::null_mut()) } as i64;
            let text = format!("Ends at {}", super::super::hm(now + left));
            if let Some(t) = self.ends.set(g, g.f_small, &text) {
                g.put(Some(t), x, button_y + 18.0, DIM, 1.0);
            }
        }
        bar
    }

    /// The button that offers to skip what is on screen, above the bar when it is up.
    fn draw_skip(&mut self, g: &Gfx, kind: SegKind, bar: Option<&Bar>) {
        if self.skip.as_ref().is_none_or(|s| s.0 != kind) {
            self.skip = g
                .text(g.f_label, &format!("{}   OK", kind.label()), 0)
                .map(|t| (kind, t));
        }
        let Some((_, t)) = &self.skip else { return };
        let (w, h) = (t.w as f32 + 72.0, 84.0);
        let y = bar.map_or(H - 64.0 - h - 24.0, |b| b.top - h - 24.0);
        g.rounded(W - MARGIN - w, y, w, h, 22.0, TEXT, 1.0);
        g.put(
            Some(t),
            W - MARGIN - w + 36.0,
            y + (h - t.h as f32) / 2.0,
            rgb(20, 22, 25),
            1.0,
        );
    }
}

const SCREEN: FRect = FRect {
    x: 0.0,
    y: 0.0,
    w: W,
    h: H,
};

fn backdrop_file(root: &str, pre: &str, plex_base: Option<&str>, id: &str, series: Option<&str>) -> Option<String> {
    use crate::{data::DIR, library};
    let views: Vec<_> = library::cached_views(root, pre)
        .into_iter()
        .filter(|v| match &v.src {
            library::Source::Plex { base, .. } => Some(base.as_str()) == plex_base,
            _ => plex_base.is_none(),
        })
        .collect();
    let cached = if plex_base.is_some() {
        crate::plex::cached(root)
            .into_iter()
            .find(|t| t.rating_key == id && views.iter().any(|v| v.server == t.server))
            .and_then(|t| t.bg)
    } else {
        crate::jellyfin::cached(root)
            .into_iter()
            .find(|t| t.id == id)
            .and_then(|t| t.bg)
    };
    let dir = format!("{DIR}/cache/");
    let art = |path: String| {
        let file = path.strip_prefix(&dir)?;
        if file.is_empty() || file.contains('/') {
            return None;
        }
        let path = format!("{root}{path}");
        std::path::Path::new(&path).is_file().then_some(path)
    };
    if let Some(path) = cached.and_then(art) {
        return Some(path);
    }
    let id = series.unwrap_or(id);
    for v in views {
        if let Some(path) = library::cached_titles(root, pre, &v.id)
            .into_iter()
            .find(|t| t.id == id)
            .and_then(|t| t.key)
            .and_then(|key| art(format!("{dir}t-b-{key}.img")))
        {
            return Some(path);
        }
    }
    let prefix = format!("{pre}-b-{id}-");
    std::fs::read_dir(format!("{root}{dir}"))
        .ok()?
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".img"))
        })
        .and_then(|entry| art(format!("{dir}{}", entry.file_name().to_str()?)))
}

// ---- the next episode ---------------------------------------------------------------------------------------------

/// The card that offers the next episode: during the credits with *Play now* and *Watch credits*, or after the end
/// with *Play now* alone.
pub struct UpCard {
    head: Option<Tex>,
    name: Option<Tex>,
    sub: Option<Tex>,
    play: Option<Tex>,
    watch: Option<Tex>,
    left: Label,
    /// The focused button: 0 *Play now*, 1 *Watch credits*.
    pub focus: usize,
}

impl UpCard {
    pub fn new(g: &Gfx, next: &Item, credits: bool) -> UpCard {
        UpCard {
            head: g.text(g.f_label, "UP NEXT", 0),
            name: g.text(g.f_row, &crate::jellyfin::clip(&next.name, 34), 0),
            sub: g.text(g.f_sub, &crate::jellyfin::clip(&next.sub, 40), 0),
            play: g.text(g.f_label, "Play now", 0),
            watch: credits.then(|| g.text(g.f_label, "Watch credits", 0)).flatten(),
            left: Label::default(),
            focus: 0,
        }
    }

    pub fn step(&mut self, d: i64) {
        if self.watch.is_some() {
            self.focus = (self.focus as i64 + d).clamp(0, 1) as usize;
        }
    }

    /// `left` of `of` seconds remain before the next episode starts. `centre`: alone on a dark screen, after the end.
    fn draw(&mut self, g: &Gfx, left: f32, of: f32, centre: bool, bar: Option<&Bar>) {
        let (w, h) = (660.0, 270.0);
        let (x, y) = if centre {
            ((W - w) / 2.0, (H - h) / 2.0)
        } else {
            (W - MARGIN - w, bar.map_or(H - 64.0 - h, |b| b.top - h - 24.0))
        };
        if centre {
            g.fill(rgb(8, 10, 14), 1.0, SCREEN);
        }
        g.rounded(x, y, w, h, 28.0, PANEL, 0.96);
        g.put(self.head.as_ref(), x + 40.0, y + 30.0, YELLOW, 1.0);
        let secs = format!("in {} s", left.ceil().max(0.0) as i64);
        if let Some(t) = self.left.set(g, g.f_label, &secs) {
            g.put(Some(t), x + w - 40.0 - t.w as f32, y + 30.0, DIM, 1.0);
        }
        g.put(self.name.as_ref(), x + 40.0, y + 72.0, TEXT, 1.0);
        g.put(self.sub.as_ref(), x + 40.0, y + 118.0, DIM, 1.0);
        let (by, bh) = (y + h - 96.0, 64.0);
        let mut bx = x + 40.0;
        for (i, t) in [self.play.as_ref(), self.watch.as_ref()].into_iter().enumerate() {
            let Some(t) = t else { continue };
            let on = self.focus == i;
            let icon = if i == 0 { 34.0 } else { 0.0 };
            let bw = t.w as f32 + 64.0 + if icon > 0.0 { 48.0 } else { 0.0 };
            g.rounded(bx, by, bw, bh, bh / 2.0, if on { TEXT } else { rgb(70, 74, 80) }, 1.0);
            // The countdown fills the *Play now* button.
            if i == 0 && of > 0.0 {
                let f = (1.0 - left / of).clamp(0.0, 1.0);
                if bw * f >= bh {
                    g.rounded(
                        bx,
                        by,
                        bw * f,
                        bh,
                        bh / 2.0,
                        if on { YELLOW } else { rgb(110, 98, 50) },
                        1.0,
                    );
                }
            }
            let ink = if on { rgb(20, 22, 25) } else { TEXT };
            if icon > 0.0 {
                g.icon(Icon::Play, bx + 28.0, by + (bh - icon) / 2.0, icon, ink, 1.0);
            }
            let tx = bx + 32.0 + if icon > 0.0 { 48.0 } else { 0.0 };
            g.put(Some(t), tx, by + (bh - t.h as f32) / 2.0, ink, 1.0);
            bx += bw + 16.0;
        }
    }
}

// ---- subtitles ----------------------------------------------------------------------------------------------------

/// A subtitle track the viewer picked: its stream number, and whether it comes from the server (a file beside the
/// video, or every track while the server converts the film) rather than from the video file itself.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SubSel {
    pub index: i64,
    pub server: bool,
}

/// What the viewer chose, kept across seeks (each seek restarts the stream).
#[derive(Default)]
pub struct Choice {
    /// The first run has picked the tracks the server would start with.
    pub init: bool,
    pub audio: Option<i64>,
    pub sub: Option<SubSel>,
    pub second: Option<SubSel>,
    /// Options: the look (saved) and how much later than the file says subtitles show, in ms (this film only).
    pub look: Look,
    pub delay_ms: i64,
}

pub struct Ev {
    start_ms: i64,
    end_ms: Option<i64>,
    what: SubWhat,
}

/// The subtitle events known so far, in time order.
#[derive(Default)]
pub struct Events {
    list: Vec<Ev>,
}

impl Events {
    pub fn clear(&mut self) {
        self.list.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn insert(&mut self, e: SubEv) {
        let ev = Ev {
            start_ms: e.start_ns / 1_000_000,
            end_ms: e.end_ns.map(|n| n / 1_000_000),
            what: e.what,
        };
        let at = self.list.partition_point(|x| x.start_ms <= ev.start_ms);
        self.list.insert(at, ev);
    }

    pub fn from_cues(cues: Vec<Cue>) -> Events {
        let mut e = Events::default();
        for c in cues {
            e.insert(SubEv {
                start_ns: c.start_ms * 1_000_000,
                end_ns: Some(c.end_ms * 1_000_000),
                what: SubWhat::Text {
                    text: c.text,
                    top: false,
                },
            });
        }
        e
    }

    /// The index of the event on screen at `ms`: the latest that has begun, unless it was cleared or has ended. A
    /// long one can still run while a later one has begun: a few are looked back through.
    fn active(&self, ms: i64) -> Option<usize> {
        let after = self.list.partition_point(|e| e.start_ms <= ms);
        for i in (after.saturating_sub(6)..after).rev() {
            match (&self.list[i].what, self.list[i].end_ms) {
                (SubWhat::Clear, _) => return None,
                (_, Some(end)) if end <= ms => {}
                _ => return Some(i),
            }
        }
        None
    }

    /// Forgets what is long past, so that a film of bitmap subtitles does not keep every one in memory.
    pub fn prune(&mut self, ms: i64) {
        let old = self.list.partition_point(|e| e.start_ms < ms - 30_000);
        let drop = old.saturating_sub(6);
        self.list.drain(..drop);
    }
}

/// The subtitle on screen: lines of text (each its own picture, so each is centred) or bitmaps with where they go.
#[derive(Default)]
pub struct Shown {
    key: Option<(i64, usize)>,
    lines: Vec<Tex>,
    top: bool,
    images: Vec<(Tex, FRect)>,
}

impl Shown {
    /// Makes what is on screen at `ms` the picture it should be.
    pub fn update(&mut self, g: &Gfx, ev: &Events, ms: i64) {
        let at = ev.active(ms);
        let key = at.map(|i| (ev.list[i].start_ms, i));
        if key.map(|k| k.0) == self.key.map(|k| k.0) && at.is_some() == self.key.is_some() {
            return;
        }
        self.key = key;
        log(&format!(
            "film: subtitle {} at {ms} ms",
            if at.is_some() { "on" } else { "off" }
        ));
        self.lines.clear();
        self.images.clear();
        self.top = false;
        let Some(i) = at else { return };
        match &ev.list[i].what {
            SubWhat::Text { text, top } => {
                self.lines = lines(g, text);
                self.top = *top;
            }
            SubWhat::Image { canvas, rects } => {
                let (sx, sy) = (W / canvas.0.max(1) as f32, H / canvas.1.max(1) as f32);
                for Bitmap { x, y, w, h, rgba } in rects {
                    if let Some(t) = Tex::rgba(g.ren, *w, *h, rgba) {
                        let dst = FRect {
                            x: *x as f32 * sx,
                            y: *y as f32 * sy,
                            w: *w as f32 * sx,
                            h: *h as f32 * sy,
                        };
                        self.images.push((t, dst));
                    }
                }
            }
            SubWhat::Clear => {}
        }
    }

    pub fn reset(&mut self) {
        *self = Shown::default();
    }
}

/// A styled (ASS) subtitle on screen: libass's pictures as textures, where they go on the frame.
#[derive(Default)]
pub struct AssLayer {
    pics: Vec<(Tex, FRect)>,
}

impl AssLayer {
    /// Makes the pictures libass gave at `(ox, oy)` on the frame.
    pub fn set(&mut self, g: &Gfx, pics: &[crate::ff::ass::Picture], ox: i32, oy: i32) {
        self.pics = pics
            .iter()
            .filter_map(|p| {
                let t = Tex::rgba(g.ren, p.w, p.h, &p.rgba)?;
                let dst = FRect {
                    x: (p.x + ox) as f32,
                    y: (p.y + oy) as f32,
                    w: p.w as f32,
                    h: p.h as f32,
                };
                Some((t, dst))
            })
            .collect();
    }

    pub fn clear(&mut self) {
        self.pics.clear();
    }
}

/// Where the subtitles go for a picture of `video` pixels shown at the largest size that fits the 1920 x 1080 screen:
/// the frame libass works on, and its offset on the screen.
pub fn ass_frame(video: (i32, i32)) -> ((i32, i32), (i32, i32)) {
    let (vw, vh) = (video.0.max(1) as i64, video.1.max(1) as i64);
    if vw * 1080 > vh * 1920 {
        let h = (1920 * vh / vw) as i32;
        ((1920, h), (0, (1080 - h) / 2))
    } else {
        let w = (1080 * vw / vh) as i32;
        ((w, 1080), ((1920 - w) / 2, 0))
    }
}

/// The font libass falls back to for a name that no attachment gives: the UI's own.
pub fn ass_default_font() -> Option<String> {
    let root = FONT_ROOT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let p = format!("{root}/usr/share/fonts/LG_Smart_UI-SemiBold.ttf");
    std::path::Path::new(&p).exists().then_some(p)
}

/// A subtitle as one picture per line, so each can be centred. A line the UI font has no glyphs for (Chinese,
/// Arabic, ...) is drawn with another font of the TV that has them.
pub fn lines(g: &Gfx, text: &str) -> Vec<Tex> {
    text.lines().filter_map(|l| g.text(font_for(g, l), l, 1700)).collect()
}

// ---- fonts for scripts the UI font lacks --------------------------------------------------------------------------

struct Fonts {
    /// The font files of the TV, and the ones opened so far.
    files: Vec<std::path::PathBuf>,
    open: Vec<(usize, *mut Opaque)>,
}

// The fonts are only touched by the main thread of the player process; the lock is for the static.
unsafe impl Send for Fonts {}

static FONTS: std::sync::Mutex<Option<Fonts>> = std::sync::Mutex::new(None);
static FONT_ROOT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Where the TV's fonts are looked for (`<root>/usr/share/fonts`).
pub fn set_font_root(root: &str) {
    *FONT_ROOT.lock().unwrap_or_else(|e| e.into_inner()) = root.to_string();
    let f = crate::font(root, "LG_Smart_UI-SemiBold.ttf", SUB_PX.load(Ordering::SeqCst));
    MAIN.store(f, Ordering::SeqCst);
}

/// The root the player runs under: where `state.json` is.
pub fn root() -> String {
    FONT_ROOT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The text subtitles' size in pixels (Options): the fonts are opened again at it.
pub fn set_sub_size(px: c_int) {
    if SUB_PX.swap(px, Ordering::SeqCst) != px {
        set_font_root(&root());
        *FONTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Subtitles are bigger than the UI's text: they are read from the sofa.
static SUB_PX: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(44);
static MAIN: std::sync::atomic::AtomicPtr<Opaque> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// The font of subtitles: the UI font at `SUB_SIZE`, or the UI's row font where there is none (a desktop shot).
fn main_font(g: &Gfx) -> *mut Opaque {
    let f = MAIN.load(Ordering::SeqCst);
    if f.is_null() {
        g.f_row
    } else {
        f
    }
}

/// `TTF_GlyphIsProvided`, looked up at run time: a library that lacks it only costs the fallback.
fn glyph_fn() -> Option<unsafe extern "C" fn(*mut Opaque, u16) -> c_int> {
    static F: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let p = (*F.get_or_init(|| {
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"TTF_GlyphIsProvided".as_ptr()) };
        (!p.is_null()).then_some(p as usize)
    }))?;
    Some(unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*mut Opaque, u16) -> c_int>(p) })
}

fn font_files(dir: &std::path::Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if p.is_dir() && depth < 3 {
            font_files(&p, depth + 1, out);
        } else if (name.ends_with(".ttf") || name.ends_with(".otf") || name.ends_with(".ttc"))
            && !name.starts_with("LG_Smart_UI")
            && !["Bold", "Italic", "Light", "Thin", "Black"]
                .iter()
                .any(|w| name.contains(w))
        {
            out.push(p);
        }
    }
}

/// How many of `chars` the font has.
fn coverage(f: *mut Opaque, has: unsafe extern "C" fn(*mut Opaque, u16) -> c_int, chars: &[u16]) -> usize {
    chars.iter().filter(|c| unsafe { has(f, **c) } != 0).count()
}

/// The font to draw `line` with: the UI font when it has every glyph, else the font of the TV with the most.
fn font_for(g: &Gfx, line: &str) -> *mut Opaque {
    let main = main_font(g);
    let Some(has) = glyph_fn() else { return main };
    let chars: Vec<u16> = line
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .filter_map(|c| u16::try_from(c as u32).ok())
        .collect();
    let base = coverage(main, has, &chars);
    if base == chars.len() {
        return main;
    }
    let mut guard = FONTS.lock().unwrap_or_else(|e| e.into_inner());
    let fonts = guard.get_or_insert_with(|| {
        let root = FONT_ROOT.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut files = vec![];
        font_files(std::path::Path::new(&format!("{root}/usr/share/fonts")), 0, &mut files);
        files.truncate(40);
        Fonts { files, open: vec![] }
    });
    let (mut best, mut best_n) = (main, base);
    for i in 0..fonts.files.len() {
        let cached = fonts.open.iter().find(|o| o.0 == i).map(|o| o.1);
        let f = match cached {
            Some(f) => f,
            None => {
                let px = SUB_PX.load(Ordering::SeqCst);
                let f = unsafe { TTF_OpenFont(cstr(&fonts.files[i].to_string_lossy()).as_ptr(), px) };
                fonts.open.push((i, f));
                f
            }
        };
        if f.is_null() {
            continue;
        }
        let n = coverage(f, has, &chars);
        if n > best_n {
            (best, best_n) = (f, n);
        }
        if best_n == chars.len() {
            break;
        }
    }
    best
}

// ---- the panels ---------------------------------------------------------------------------------------------------

pub struct AudioRow {
    pub index: i64,
    pub name: String,
    pub lang: String,
}

pub struct SubRow {
    pub index: i64,
    pub server: bool,
    /// The language code, and what the track is: "Full", "SDH", "Forced", "Commentary" or its own title.
    pub lang: String,
    pub label: String,
    pub format: String,
}

impl SubRow {
    pub fn selection(&self) -> SubSel {
        SubSel {
            index: self.index,
            server: self.server,
        }
    }

    fn plain_text(&self) -> bool {
        matches!(self.format.as_str(), "SRT" | "WebVTT" | "Text")
    }

    /// `title` is the track's own title, which often only repeats the language or the kind.
    pub fn new(index: i64, server: bool, lang: &str, forced: bool, sdh: bool, title: &str, format: &str) -> SubRow {
        let t = title.to_lowercase();
        let kind = if forced || t.contains("forced") {
            "Forced"
        } else if sdh || t.contains("sdh") || t.contains("hearing") {
            "SDH"
        } else if t.contains("comment") {
            "Commentary"
        } else {
            "Full"
        };
        let language = crate::lang::name(lang).to_lowercase();
        let words: Vec<&str> = t
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        // A title that says only the language (often in that language: "Français"), the kind or the format adds
        // nothing.
        let plain = words.len() <= 1
            || words.iter().all(|w| {
                [
                    "full",
                    "sdh",
                    "forced",
                    "cc",
                    "subtitles",
                    "subs",
                    "default",
                    "complete",
                ]
                .contains(w)
                    || language.split(' ').any(|l| l == *w)
                    || lang.eq_ignore_ascii_case(w)
                    || format.eq_ignore_ascii_case(w)
            });
        let label = match (kind, plain || kind == "Commentary") {
            (k, true) => k.to_string(),
            ("Full", false) => title.trim().to_string(),
            (k, false) => format!("{k} · {}", title.trim()),
        };
        SubRow {
            index,
            server,
            lang: lang.to_string(),
            label,
            format: format.to_string(),
        }
    }

    /// The language and the kind, for Info.
    pub fn describe(&self) -> String {
        match crate::lang::name(&self.lang) {
            l if l.is_empty() => self.label.clone(),
            l => format!("{l} · {}", self.label),
        }
    }
}

#[derive(Default)]
pub struct Tracks {
    pub audio: Vec<AudioRow>,
    pub subs: Vec<SubRow>,
}

impl Tracks {
    pub fn subtitle(&self, sel: SubSel) -> Option<&SubRow> {
        self.subs.iter().find(|s| s.selection() == sel)
    }

    pub fn allows_second(&self, primary: Option<SubSel>, second: SubSel) -> bool {
        primary.is_some_and(|p| p != second && self.subtitle(p).is_some_and(SubRow::plain_text))
            && self.subtitle(second).is_some_and(SubRow::plain_text)
    }
}

/// A row of Options: Left and Right change it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Opt {
    Size,
    Raise,
    Color,
    Bg,
    Delay,
    Skip,
}

pub enum Pick {
    /// A line that is shown and not chosen: a heading, or a fact of Info.
    Line,
    Sub(Option<SubSel>),
    SecondMenu,
    SecondSub(Option<SubSel>),
    Audio(i64),
    /// A chapter's start, in ticks.
    Chapter(i64),
    Opt(Opt),
    /// Info: play from the start, or leave for the title's page (the series of an episode).
    Restart,
    Page,
}

pub struct Row {
    tex: Option<Tex>,
    value: Option<Tex>,
    icon: Option<Icon>,
    pub pick: Pick,
    current: bool,
}

/// The panel a button opens, at the right of the picture.
pub struct Menu {
    pub btn: Btn,
    pub second: bool,
    title: Option<Tex>,
    hint: Option<Tex>,
    pub rows: Vec<Row>,
    pub sel: Option<usize>,
}

/// What a panel's row says: its name, its value (Options and Info), what it does, and whether it is the current one.
type RowSpec = (String, Option<String>, Pick, bool);

/// The subtitle tracks under their languages: `lead` (the language the viewer reads) first, then the others in the
/// order of the file; in each, the full track, SDH, forced and the rest.
fn sub_rows<'a>(
    subs: impl Iterator<Item = &'a SubRow> + Clone,
    lead: &str,
    cur: Option<SubSel>,
    second: bool,
) -> Vec<RowSpec> {
    let pick: fn(Option<SubSel>) -> Pick = if second { Pick::SecondSub } else { Pick::Sub };
    let mut langs: Vec<&str> = vec![];
    for s in subs.clone() {
        if !langs.contains(&s.lang.as_str()) {
            langs.push(&s.lang);
        }
    }
    let same = |a: &str, b: &str| !a.is_empty() && crate::lang::name(a) == crate::lang::name(b);
    langs.sort_by_key(|l| !same(l, lead));
    let rank = |s: &SubRow| match s.label.split(' ').next().unwrap_or_default() {
        "Full" => 0,
        "SDH" => 1,
        "Forced" => 2,
        "Commentary" => 4,
        _ => 3,
    };
    let mut rows = vec![("Off".to_string(), None, pick(None), cur.is_none())];
    for l in langs {
        let name = match crate::lang::name(l) {
            n if n.is_empty() => "Other".to_string(),
            n => n,
        };
        rows.push((name, None, Pick::Line, false));
        let mut list: Vec<&SubRow> = subs.clone().filter(|s| s.lang == l).collect();
        list.sort_by_key(|s| rank(s));
        for (n, s) in list.iter().enumerate() {
            let sel = s.selection();
            let format = (!s.format.is_empty()).then(|| s.format.clone());
            let same = |other: &&SubRow| other.label == s.label && other.format == s.format;
            let label = if list.iter().filter(|other| same(other)).take(2).count() > 1 {
                format!(
                    "{} {}",
                    s.label,
                    list[..n].iter().filter(|other| same(other)).count() + 1
                )
            } else {
                s.label.clone()
            };
            rows.push((label, format, pick(Some(sel)), cur == Some(sel)));
        }
    }
    rows
}

impl Menu {
    fn build(g: &Gfx, btn: Btn, hint: &str, rows: Vec<RowSpec>, sel: Option<usize>) -> Menu {
        let rows: Vec<Row> = rows
            .into_iter()
            .map(|(name, value, pick, current)| Row {
                tex: g.text(
                    if matches!(pick, Pick::Line) && value.is_none() {
                        g.f_label
                    } else {
                        g.f_sub
                    },
                    &crate::jellyfin::clip(&name, 46),
                    0,
                ),
                value: value.and_then(|v| g.text(g.f_sub, &crate::jellyfin::clip(&v, 60), 0)),
                icon: match pick {
                    Pick::Restart => Some(Icon::Restart),
                    Pick::Page => Some(Icon::Library),
                    _ => None,
                },
                pick,
                current,
            })
            .collect();
        let can = |i: usize| rows.get(i).is_some_and(|r| !matches!(r.pick, Pick::Line));
        let sel = sel
            .filter(|i| can(*i))
            .or_else(|| (0..rows.len()).find(|i| can(*i) && rows[*i].current))
            .or_else(|| (0..rows.len()).find(|i| can(*i)));
        Menu {
            btn,
            second: false,
            title: g.text(g.f_menu, btn.label(), 0),
            hint: g.text(g.f_small, if hint.is_empty() { "Back to close" } else { hint }, 0),
            rows,
            sel,
        }
    }

    pub fn subs(g: &Gfx, t: &Tracks, c: &Choice, lead: &str, sel: Option<usize>) -> Menu {
        let mut rows = sub_rows(t.subs.iter(), lead, c.sub, false);
        if t.subs.iter().any(|s| t.allows_second(c.sub, s.selection())) {
            let value = c
                .second
                .and_then(|s| t.subtitle(s))
                .map_or_else(|| "Off".into(), SubRow::describe);
            rows.insert(0, ("Second subtitles".into(), Some(value), Pick::SecondMenu, false));
        }
        Menu::build(g, Btn::Subs, "", rows, sel)
    }

    pub fn second_subs(g: &Gfx, t: &Tracks, c: &Choice, lead: &str) -> Menu {
        let candidates = t.subs.iter().filter(|s| t.allows_second(c.sub, s.selection()));
        let mut m = Menu::build(
            g,
            Btn::Subs,
            "Shown at the top   ·   Back to subtitles",
            sub_rows(candidates, lead, c.second, true),
            None,
        );
        m.title = g.text(g.f_menu, "Second subtitles", 0);
        m.second = true;
        m
    }

    pub fn audio(g: &Gfx, t: &Tracks, c: &Choice) -> Menu {
        let rows = t
            .audio
            .iter()
            .enumerate()
            .map(|(n, a)| {
                let on = c.audio == Some(a.index) || (c.audio.is_none() && n == 0);
                (a.name.clone(), None, Pick::Audio(a.index), on)
            })
            .collect();
        Menu::build(g, Btn::Audio, "", rows, None)
    }

    pub fn chapters(g: &Gfx, chapters: &[Chapter], pos: i64) -> Menu {
        let now = chapter_at(chapters, pos);
        let rows = chapters
            .iter()
            .enumerate()
            .map(|(n, ch)| {
                let time = film::clock(ch.start / TICKS);
                (ch.name.clone(), Some(time), Pick::Chapter(ch.start), now == Some(n))
            })
            .collect();
        Menu::build(g, Btn::Chapters, "", rows, None)
    }

    /// `delay_ms`: how much later than the file says subtitles are shown (earlier when negative).
    pub fn options(g: &Gfx, look: &Look, delay_ms: i64, sel: Option<usize>) -> Menu {
        let delay = match delay_ms {
            0 => "0.0 s".to_string(),
            d => format!("{}{:.1} s", if d > 0 { "+" } else { "-" }, d.abs() as f64 / 1000.0),
        };
        let rows = vec![
            (
                "Subtitle size".into(),
                Some(SIZES[look.sub_size].0.into()),
                Pick::Opt(Opt::Size),
                false,
            ),
            (
                "Subtitle position".into(),
                Some(RAISES[look.sub_pos].0.into()),
                Pick::Opt(Opt::Raise),
                false,
            ),
            (
                "Subtitle colour".into(),
                Some(COLORS[look.sub_color].0.into()),
                Pick::Opt(Opt::Color),
                false,
            ),
            (
                "Subtitle background".into(),
                Some(if look.sub_bg { "Box" } else { "Outline" }.into()),
                Pick::Opt(Opt::Bg),
                false,
            ),
            ("Subtitle timing".into(), Some(delay), Pick::Opt(Opt::Delay), false),
            (
                "Skip by".into(),
                Some(format!("{} s", SKIPS[look.skip])),
                Pick::Opt(Opt::Skip),
                false,
            ),
        ];
        Menu::build(
            g,
            Btn::Options,
            "Left / Right to change   ·   Back to close",
            rows,
            sel.or(Some(0)),
        )
    }

    /// What can be done (`page`: the name of the title's page, "Go to series"), then facts about what plays.
    pub fn info(g: &Gfx, page: &str, lines: Vec<(String, String)>, sel: Option<usize>) -> Menu {
        let mut rows: Vec<RowSpec> = vec![("Play from the beginning".into(), None, Pick::Restart, false)];
        rows.push((page.into(), None, Pick::Page, false));
        rows.extend(lines.into_iter().map(|(k, v)| (k, Some(v), Pick::Line, false)));
        Menu::build(g, Btn::Info, "", rows, sel)
    }

    pub fn picked(&self) -> Option<&Pick> {
        self.rows.get(self.sel?).map(|r| &r.pick)
    }

    pub fn step(&mut self, d: i32) {
        let Some(mut i) = self.sel.map(|s| s as i32) else {
            return;
        };
        loop {
            i += d;
            match self.rows.get(i as usize).filter(|_| i >= 0) {
                None => return,
                Some(Row { pick: Pick::Line, .. }) => {}
                Some(_) => return self.sel = Some(i as usize),
            }
        }
    }

    fn draw(&self, g: &Gfx) {
        const ROW: f32 = 58.0;
        let info = self.btn == Btn::Info;
        let shown = self.rows.len().clamp(1, if info { 10 } else { 7 });
        let first = if info {
            0
        } else {
            self.sel
                .unwrap_or(0)
                .saturating_sub(shown / 2)
                .min(self.rows.len().saturating_sub(shown))
        };
        let (head, foot) = (84.0, 56.0);
        let w = match self.btn {
            Btn::Info => 1040.0,
            Btn::Options => 760.0,
            _ => 660.0,
        };
        let h = head + shown as f32 * ROW + foot;
        let (x, y) = (W - MARGIN - w, (H - 438.0 - h).max(40.0));
        g.rounded(x, y, w, h, 24.0, rgb(18, 21, 26), 0.98);
        g.icon(self.btn.icon(), x + 28.0, y + 23.0, 34.0, TEXT, 1.0);
        g.fill(
            WHITE,
            0.14,
            FRect {
                x: x + 28.0,
                y: y + 73.0,
                w: w - 56.0,
                h: 1.0,
            },
        );
        let th = self.title.as_ref().map_or(0.0, |t| t.h as f32);
        g.put(self.title.as_ref(), x + 78.0, y + 40.0 - th / 2.0, WHITE, 1.0);
        for (n, row) in self.rows.iter().enumerate().skip(first).take(shown) {
            let ry = y + head + (n - first) as f32 * ROW;
            let on = self.sel == Some(n);
            if on {
                g.rounded(x + 16.0, ry + 3.0, w - 40.0, ROW - 6.0, 12.0, TEXT, 1.0);
            }
            let ink = if on { rgb(20, 22, 25) } else { TEXT };
            if row.current {
                g.icon(Icon::Check, x + 32.0, ry + (ROW - 28.0) / 2.0, 28.0, ink, 1.0);
            }
            if let Some(i) = row.icon {
                g.icon(i, x + 32.0, ry + (ROW - 30.0) / 2.0, 30.0, ink, 1.0);
            }
            let heading = matches!(row.pick, Pick::Line) && row.value.is_none();
            let tx = x + if heading { 32.0 } else { 68.0 };
            let value_w = row.value.as_ref().map_or(0.0, |t| t.w as f32 + 18.0);
            if let Some(t) = &row.tex {
                let width = (t.w as f32).min(x + w - 48.0 - tx - value_w);
                if width >= 1.0 {
                    g.tint(t, if matches!(row.pick, Pick::Line) { DIM } else { ink }, 1.0);
                    g.copy(
                        t,
                        Some(Rect {
                            x: 0,
                            y: 0,
                            w: width as i32,
                            h: t.h,
                        }),
                        FRect {
                            x: tx,
                            y: ry + (ROW - t.h as f32) / 2.0,
                            w: width,
                            h: t.h as f32,
                        },
                    );
                }
            }
            if let Some(v) = &row.value {
                let vink = match (on, &row.pick) {
                    (true, _) => ink,
                    (false, Pick::Line) => TEXT,
                    _ => DIM,
                };
                g.put(
                    Some(v),
                    x + w - 48.0 - v.w as f32,
                    ry + (ROW - v.h as f32) / 2.0,
                    vink,
                    1.0,
                );
            }
        }
        if self.rows.len() > shown {
            let step = shown as f32 * ROW / self.rows.len() as f32;
            let track = x + w - 14.0;
            g.rounded(track, y + head, 3.0, shown as f32 * ROW, 1.5, WHITE, 0.12);
            g.rounded(
                track,
                y + head + first as f32 * step,
                3.0,
                shown as f32 * step,
                1.5,
                WHITE,
                0.55,
            );
        }
        if let Some(t) = &self.hint {
            g.put(Some(t), x + 32.0, y + h - (foot + t.h as f32) / 2.0 + 2.0, DIM, 1.0);
        }
    }
}

// ---- a frame ------------------------------------------------------------------------------------------------------

/// What a picture adds to the transparent frame besides the controls.
#[derive(Default)]
pub struct Over<'a> {
    pub shown: Option<&'a Shown>,
    pub second: Option<&'a Shown>,
    pub menu: Option<&'a Menu>,
    /// What the skip button offers now, and the picture to show above the bar while seeking.
    pub skip: Option<SegKind>,
    pub thumb: Option<&'a Tex>,
    pub ass: Option<&'a AssLayer>,
    pub look: Look,
    pub loading: Option<Loading<'a>>,
    /// The next episode's card, with the seconds left and the seconds it counts down from.
    pub up: Option<(&'a mut UpCard, f32, f32)>,
}

/// A transparent frame, so the video plane shows, with the subtitle, the controls and the panel over it.
pub fn vframe(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<Seeking>, over: Over) {
    unsafe {
        SDL_SetRenderDrawColor(g.ren, 0, 0, 0, 0);
        SDL_RenderClear(g.ren);
    }
    draw_over(g, osd, pos_s, paused, seeking, over);
    unsafe { SDL_RenderPresent(g.ren) };
}

fn subtitle_lift(bar: Option<&Bar>, rects: impl Iterator<Item = FRect>) -> f32 {
    let Some(bar) = bar else { return 0.0 };
    let (top, bottom) = rects.fold((H, 0.0_f32), |(top, bottom), r| (top.min(r.y), bottom.max(r.y + r.h)));
    (bottom - (bar.top - 26.0)).max(0.0).min(top.max(0.0))
}

fn draw_over(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<Seeking>, over: Over) {
    if let Some(l) = over.loading.filter(|l| l.full) {
        return osd.draw_loading(g, Some(l.stage), 1.0);
    }
    if let Some(start) = osd.opening_fade {
        let remaining = 1.0 - start.elapsed().as_secs_f32() / 0.18;
        if remaining > 0.0 {
            return osd.draw_loading(g, None, remaining);
        }
        osd.opening_fade = None;
    }
    let geom = osd.draw(g, pos_s, paused, seeking, over.menu.is_some());
    let bar = geom.as_ref();
    if over.loading.is_some() {
        spinner(g, W / 2.0, H / 2.0 - 60.0, 30.0, osd.started.elapsed().as_secs_f32());
    }
    if let (Some(b), true) = (bar, seeking.is_some()) {
        let w = 420.0;
        let (pad, label_h) = (8.0, 60.0);
        let image_h = over
            .thumb
            .map_or(0.0, |t| (w - 2.0 * pad) * t.h as f32 / t.w.max(1) as f32);
        let x = (b.x + b.w * b.frac - w / 2.0).clamp(MARGIN, W - MARGIN - w);
        let y = b.top - image_h - label_h - 32.0;
        g.rounded(x, y, w, image_h + label_h + pad, 12.0, rgb(20, 23, 27), 1.0);
        if let Some(t) = over.thumb {
            g.copy(
                t,
                None,
                FRect {
                    x: x + pad,
                    y: y + pad,
                    w: w - 2.0 * pad,
                    h: image_h,
                },
            );
        }
        let label_y = y + image_h + pad;
        if let Some(t) = &osd.at.1 {
            let tw = t.w as f32;
            let tx = x + if osd.chapter.1.is_some() { 24.0 } else { (w - tw) / 2.0 };
            g.put(Some(t), tx, label_y + (label_h - t.h as f32) / 2.0, WHITE, 1.0);
            if let Some(ch) = &osd.chapter.1 {
                let cx = tx + tw + 32.0;
                let cw = ch.w as f32;
                let k = ((x + w - 24.0 - cx) / cw.max(1.0)).clamp(0.0, 1.0);
                let ch_h = ch.h as f32 * k;
                g.fill(
                    WHITE,
                    0.20,
                    FRect {
                        x: cx - 16.0,
                        y: label_y + 16.0,
                        w: 1.0,
                        h: 28.0,
                    },
                );
                g.tint(ch, TEXT, 0.88);
                g.copy(
                    ch,
                    None,
                    FRect {
                        x: cx,
                        y: label_y + (label_h - ch_h) / 2.0,
                        w: cw * k,
                        h: ch_h,
                    },
                );
            }
        }
    }
    let card = over.up.is_some();
    if let (Some((c, left, of)), None) = (over.up, over.menu) {
        c.draw(g, left, of, false, geom.as_ref());
    }
    if let (Some(kind), None, false) = (over.skip, over.menu, card) {
        osd.draw_skip(g, kind, geom.as_ref());
    }
    let look = clamp_look(over.look);
    let ink = sub_ink(&look);
    if let Some(a) = over.ass.filter(|_| seeking.is_none()) {
        let lift = subtitle_lift(bar, a.pics.iter().map(|(_, dst)| *dst));
        for (t, dst) in &a.pics {
            g.tint(t, ink, 1.0);
            g.copy(
                t,
                None,
                FRect {
                    y: dst.y - lift,
                    ..*dst
                },
            );
        }
    }
    let raise = RAISES[look.sub_pos].1;
    for (s, second) in [over.shown, over.second].into_iter().zip([false, true]) {
        let Some(s) = s.filter(|_| seeking.is_none()) else {
            continue;
        };
        // A picture subtitle grows with the size from the bottom middle of the screen, as text does.
        let k = SIZES[look.sub_size].1 as f32 / 44.0;
        let image_rect = |d: &FRect| FRect {
            x: W / 2.0 + (d.x - W / 2.0) * k,
            y: (H - (H - d.y) * k - raise).max(0.0),
            w: d.w * k,
            h: d.h * k,
        };
        let lift = subtitle_lift(bar, s.images.iter().map(|(_, d)| image_rect(d)));
        for (t, d) in &s.images {
            let mut dst = image_rect(d);
            dst.y -= lift;
            g.tint(t, ink, 1.0);
            g.copy(t, None, dst);
        }
        if !s.lines.is_empty() {
            let w = s.lines.iter().map(|t| t.w).max().unwrap_or(0) as f32;
            let h: f32 = s.lines.iter().map(|t| t.h as f32).sum();
            let y = if second || (s.top && over.second.is_none()) {
                70.0
            } else {
                H - 120.0 - raise - h
            };
            let rect = FRect {
                x: (W - w) / 2.0,
                y,
                w,
                h,
            };
            let top = y - subtitle_lift(bar, std::iter::once(rect));
            if look.sub_bg {
                g.rounded((W - w) / 2.0 - 20.0, top - 10.0, w + 40.0, h + 20.0, 16.0, BLACK, 0.6);
            }
            let mut y = top;
            for t in &s.lines {
                let x = (W - t.w as f32) / 2.0;
                if !look.sub_bg {
                    for (dx, dy) in [
                        (-3.0, 0.0),
                        (3.0, 0.0),
                        (0.0, -3.0),
                        (0.0, 3.0),
                        (-2.0, -2.0),
                        (2.0, 2.0),
                        (-2.0, 2.0),
                        (2.0, -2.0),
                    ] {
                        g.put(Some(t), x + dx, y + dy, BLACK, 1.0);
                    }
                }
                g.put(Some(t), x, y, ink, 1.0);
                y += t.h as f32;
            }
        }
    }
    if let Some(m) = over.menu {
        m.draw(g);
    }
}

/// The card after an episode, alone on a dark screen.
pub fn up_frame(g: &Gfx, card: &mut UpCard, left: f32, of: f32) {
    card.draw(g, left, of, true, None);
    unsafe { SDL_RenderPresent(g.ren) };
}

/// `--film-shot out.bmp [what] [backdrop]` draws the controls over a stand-in picture (no player).
/// Modes: `seek`, `seek-no-thumb`, `buttons`, `options`, `subs`, `info`, `next`, `load`, `load-fade`, `bitmap`, `bitmap-hidden`.
pub fn shot(g: &Gfx, what: &str, surface: Option<&Surf>) {
    let it = Item {
        backend: film::Backend::Jellyfin(crate::jellyfin::Config::default()),
        audio: vec![],
        subs: vec![],
        default_audio: None,
        default_sub: None,
        chapters: [
            (0, "Opening"),
            (610, "The harbour"),
            (1750, "Chapter 3"),
            (3300, "Low tide"),
        ]
        .iter()
        .map(|(s, n)| Chapter {
            start: s * TICKS,
            name: n.to_string(),
        })
        .collect(),
        segments: vec![],
        trick: None,
        id: "m1".into(),
        series: None,
        source: "s".into(),
        direct: String::new(),
        name: "The Martian".into(),
        sub: String::new(),
        total: 8640 * TICKS,
        resume: 0,
        session: String::new(),
    };
    let mut osd = Osd::new(g, &it);
    if matches!(what, "load" | "load-fade") {
        osd.backdrop = None;
        let demo = || Surf::load(&format!("{}{}/cache/bg-demo1.img", g.root, crate::data::DIR));
        osd.backdrop_tex = surface
            .and_then(blur)
            .or_else(|| demo().and_then(|s| blur(&s)))
            .and_then(|s| Tex::new(g.ren, &s))
            .map(|t| (t, Instant::now() - Duration::from_secs(1)));
        osd.started -= Duration::from_secs(2);
        if what == "load-fade" {
            if let Some(t) = surface.and_then(|s| Tex::new(g.ren, s)) {
                g.copy(&t, None, SCREEN);
            }
            osd.draw_loading(g, Some("Reading the file"), 0.5);
        } else {
            osd.draw_loading(g, Some("Reading the file"), 1.0);
        }
        return;
    }
    let picture = surface.and_then(|s| Tex::new(g.ren, s));
    let backdrop = picture.as_ref();
    osd.set_buttons(g, &[Btn::Subs, Btn::Audio, Btn::Chapters, Btn::Options, Btn::Info]);
    let look = Look {
        sub_bg: what != "options",
        sub_color: if what == "options" { 1 } else { 0 },
        ..Look::default()
    };
    let tracks = Tracks {
        audio: vec![],
        subs: vec![
            SubRow::new(3, false, "fre", false, false, "", "PGS"),
            SubRow::new(4, false, "eng", false, false, "English", "SRT"),
            SubRow::new(5, false, "eng", false, true, "English (SDH)", "SRT"),
            SubRow::new(9, false, "eng", false, true, "SDH", "SRT"),
            SubRow::new(10, false, "eng", false, true, "STRIPPED_SDH", "SRT"),
            SubRow::new(11, false, "dan", false, false, "", "SRT"),
            SubRow::new(12, false, "ger", false, false, "", "SRT"),
            SubRow::new(6, false, "eng", true, false, "Forced", "PGS"),
            SubRow::new(7, false, "eng", false, false, "Director's commentary", "SRT"),
            SubRow::new(8, false, "spa", false, false, "Latin America", "SRT"),
        ],
    };
    let choice = Choice {
        sub: Some(SubSel {
            index: 4,
            server: false,
        }),
        ..Choice::default()
    };
    let focus = |osd: &mut Osd, n: i64| {
        osd.focus_buttons();
        osd.step(n);
    };
    let menu = match what {
        "buttons" => {
            focus(&mut osd, 0);
            None
        }
        "options" => {
            focus(&mut osd, 3);
            Some(Menu::options(g, &look, 300, Some(4)))
        }
        "subs" => {
            focus(&mut osd, 0);
            Some(Menu::subs(g, &tracks, &choice, "eng", None))
        }
        "second-subs" => {
            focus(&mut osd, 0);
            Some(Menu::second_subs(g, &tracks, &choice, "dan"))
        }
        "info-fallback" => {
            focus(&mut osd, 4);
            let lines = super::info_lines(
                &super::Source::Server("No compatible audio track"),
                &tracks,
                &choice,
                crate::ts::Codec::H265,
                None,
                crate::ff::lossless::Mode::Off,
                1.5,
            );
            Some(Menu::info(g, "Go to series", lines, None))
        }
        "info" => {
            focus(&mut osd, 4);
            let facts = [
                ("Playing", "the file as it is (Direct Play)"),
                ("Picture", "H.265   ·   3840x2160   ·   Dolby Vision profile 8"),
                ("Sound", "English - EAC3 5.1"),
                ("Sent", "as it is"),
                ("Subtitles", "English · Full"),
                ("Buffered", "1.5 s ahead"),
            ];
            let lines = facts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            Some(Menu::info(g, "Go to series", lines, None))
        }
        _ => None,
    };
    let mut events = Events::default();
    events.insert(SubEv {
        start_ns: 0,
        end_ns: Some(3_600_000_000_000),
        what: SubWhat::Text {
            text: if what == "dual-subs" {
                "Det er sent."
            } else {
                "Bonjour, le monde.\nIl est tard."
            }
            .into(),
            top: false,
        },
    });
    let mut shown = Shown::default();
    shown.update(g, &events, 1_234_000);
    let mut second = Shown::default();
    if what == "dual-subs" {
        let cues = Events::from_cues(vec![Cue {
            start_ms: 0,
            end_ms: 2_000_000,
            text: "It’s late.".into(),
        }]);
        second.update(g, &cues, 1_234_000);
    }
    let demo = |i| {
        Surf::load(&format!("{}{}/cache/bg-demo{i}.img", g.root, crate::data::DIR)).and_then(|s| Tex::new(g.ren, &s))
    };
    let demo_bg = backdrop.is_none().then(|| demo(1)).flatten();
    if let Some(t) = backdrop.or(demo_bg.as_ref()) {
        g.copy(t, None, SCREEN);
    } else {
        g.fill(rgb(54, 78, 112), 1.0, SCREEN);
    }
    let demo_thumb = (what == "seek" && backdrop.is_none())
        .then(|| {
            demo(2).or_else(|| {
                let (w, h) = (320, 180);
                let px: Vec<u8> = (0..w * h)
                    .flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 120, 255])
                    .collect();
                Tex::rgba(g.ren, w, h, &px)
            })
        })
        .flatten();
    let seeking = matches!(what, "seek" | "seek-no-thumb").then_some(Seeking {
        to: 2410,
        from: 1234,
        level: 2,
    });
    let next = Item {
        name: "The Lighthouse Keeper".into(),
        sub: "Harbour Lights   S2 E6".into(),
        backend: film::Backend::Jellyfin(crate::jellyfin::Config::default()),
        audio: vec![],
        subs: vec![],
        default_audio: None,
        default_sub: None,
        chapters: vec![],
        segments: vec![],
        trick: None,
        id: "m2".into(),
        series: None,
        source: "s".into(),
        direct: String::new(),
        total: 0,
        resume: 0,
        session: String::new(),
    };
    let mut card = UpCard::new(g, &next, true);
    if matches!(what, "next" | "bitmap-hidden" | "dual-subs") {
        osd.hide();
    }
    if matches!(what, "bitmap" | "bitmap-hidden") {
        let mut y = H - 150.0;
        for t in shown.lines.drain(..) {
            let (w, h) = (t.w as f32, t.h as f32);
            shown.images.push((
                t,
                FRect {
                    x: (W - w) / 2.0,
                    y,
                    w,
                    h,
                },
            ));
            y += h;
        }
    }
    osd.started -= Duration::from_secs(2);
    let over = Over {
        shown: Some(&shown),
        second: (what == "dual-subs").then_some(&second),
        menu: menu.as_ref(),
        skip: None,
        thumb: backdrop.or(demo_thumb.as_ref()).filter(|_| what == "seek"),
        ass: None,
        look,
        loading: None,
        up: (what == "next").then_some((&mut card, 6.4, 10.0)),
    };
    draw_over(g, &mut osd, 1234, what == "options", seeking, over);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_uses_cached_art_for_the_matching_title() -> std::io::Result<()> {
        let root = std::env::temp_dir().join(format!("tvlauncher-opening-{}", std::process::id()));
        let cache = format!("{}/cache", crate::data::DIR);
        let cache_dir = root.join(cache.trim_start_matches('/'));
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::write(cache_dir.join("bg-episode.img"), [])?;
        std::fs::write(cache_dir.join("t-b-tmdbtv55.img"), [])?;
        std::fs::write(
            cache_dir.join("watching.json"),
            format!(r#"{{"items":[{{"id":"episode","title":"Episode","bg":"{cache}/bg-episode.img"}}]}}"#),
        )?;
        std::fs::write(cache_dir.join("lib-views.json"), r#"[{"id":"shows","name":"Shows"}]"#)?;
        std::fs::write(
            cache_dir.join("lib-shows.json"),
            r#"[{"id":"series","name":"Series","key":"tmdbtv55"}]"#,
        )?;
        let art = |id, series| backdrop_file(root.to_str().unwrap(), crate::library::JELLYFIN, None, id, series);
        let paths = (
            art("episode", Some("series")),
            art("next", Some("series")),
            art("other", None),
        );
        std::fs::remove_dir_all(&root)?;
        assert_eq!(paths.0.as_deref(), cache_dir.join("bg-episode.img").to_str());
        assert_eq!(paths.1.as_deref(), cache_dir.join("t-b-tmdbtv55.img").to_str());
        assert_eq!(paths.2, None);
        Ok(())
    }

    #[test]
    fn second_subtitles_require_two_different_plain_text_tracks() {
        let tracks = Tracks {
            audio: vec![],
            subs: vec![
                SubRow::new(1, false, "dan", false, false, "", "SRT"),
                SubRow::new(1, true, "eng", false, false, "", "SRT"),
                SubRow::new(2, false, "eng", false, false, "", "PGS"),
            ],
        };
        let (dan, eng, pgs) = (
            tracks.subs[0].selection(),
            tracks.subs[1].selection(),
            tracks.subs[2].selection(),
        );
        assert!(tracks.allows_second(Some(dan), eng));
        assert!(!tracks.allows_second(Some(dan), dan));
        assert!(!tracks.allows_second(Some(dan), pgs));
        assert!(!tracks.allows_second(Some(pgs), eng));
        assert!(!tracks.allows_second(None, eng));
        let rows = sub_rows(
            tracks
                .subs
                .iter()
                .filter(|s| tracks.allows_second(Some(dan), s.selection())),
            "eng",
            Some(eng),
            true,
        );
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[2], (_, _, Pick::SecondSub(Some(s)), true) if s == eng));
    }

    #[test]
    fn identical_subtitle_tracks_have_distinct_labels() {
        let tracks = [
            SubRow::new(1, false, "eng", false, true, "", "SRT"),
            SubRow::new(2, false, "eng", false, true, "", "SRT"),
        ];
        let rows = sub_rows(tracks.iter(), "eng", Some(tracks[1].selection()), false);
        assert_eq!(rows[2].0, "SDH 1");
        assert_eq!(rows[3].0, "SDH 2");
        assert!(rows[3].3);
    }

    #[test]
    fn rapid_seek_taps_ramp_and_reverse_at_normal_speed() {
        let mut taps = SeekTaps::default();
        let steps: Vec<_> = (0..6).map(|_| taps.step(1, 10)).collect();
        assert_eq!(steps, [(10, 1), (10, 1), (20, 2), (40, 3), (80, 4), (80, 4)]);
        assert_eq!(taps.step(-1, 10), (-10, 1));
    }

    #[test]
    fn subtitle_group_lifts_only_to_clear_visible_controls() {
        let bar = Bar {
            x: 0.0,
            w: W,
            top: 786.0,
            frac: 0.0,
        };
        let rect = |y| FRect {
            x: 0.0,
            y,
            w: 240.0,
            h: 40.0,
        };
        let group = [rect(880.0), rect(940.0)];
        assert_eq!(subtitle_lift(None, group.into_iter()), 0.0);
        assert_eq!(subtitle_lift(Some(&bar), group.into_iter()), 220.0);
        assert_eq!(subtitle_lift(Some(&bar), [rect(70.0)].into_iter()), 0.0);
        assert_eq!(subtitle_lift(Some(&bar), [rect(50.0), rect(940.0)].into_iter()), 50.0);
    }

    fn text(start: i64, end: Option<i64>, s: &str) -> SubEv {
        SubEv {
            start_ns: start * 1_000_000,
            end_ns: end.map(|e| e * 1_000_000),
            what: SubWhat::Text {
                text: s.into(),
                top: false,
            },
        }
    }

    fn clear(start: i64) -> SubEv {
        SubEv {
            start_ns: start * 1_000_000,
            end_ns: None,
            what: SubWhat::Clear,
        }
    }

    fn text_at(e: &Events, ms: i64) -> Option<String> {
        e.active(ms).map(|i| match &e.list[i].what {
            SubWhat::Text { text, .. } => text.clone(),
            _ => "?".into(),
        })
    }

    #[test]
    fn subtitles_group_by_language_reader_first_full_before_sdh_and_forced() {
        let subs = [
            SubRow::new(3, false, "fre", false, false, "Français", "PGS"),
            SubRow::new(4, false, "eng", true, false, "", "PGS"),
            SubRow::new(5, false, "eng", false, false, "English SDH", "SRT"),
            SubRow::new(6, false, "eng", false, false, "English", "SRT"),
            SubRow::new(7, true, "eng", false, false, "Signs & Songs", "ASS"),
        ];
        let rows = sub_rows(
            subs.iter(),
            "en",
            Some(SubSel {
                index: 6,
                server: false,
            }),
            false,
        );
        let names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "Off",
                "English",
                "Full",
                "SDH",
                "Forced",
                "Signs & Songs",
                "French",
                "Full"
            ]
        );
        assert!(rows[2].3, "the current track is marked");
        assert_eq!(rows[2].1.as_deref(), Some("SRT"));
        assert!(matches!(rows[1].2, Pick::Line), "a language is a heading");
        assert_eq!(subs[0].describe(), "French · Full");
    }

    #[test]
    fn a_hold_speeds_up_and_stays_inside_the_film() {
        assert_eq!(Scrub::rate(0.2), (0.0, 0), "a tap does not scrub");
        let rates: Vec<f64> = [1.0, 3.0, 5.0, 9.0].iter().map(|h| Scrub::rate(*h).0).collect();
        assert!(rates.windows(2).all(|w| w[0] < w[1]), "{rates:?}");
        let mut s = Scrub::new(1, 100.0);
        s.since -= Duration::from_secs(8);
        s.last -= Duration::from_millis(500);
        assert_eq!(s.advance(150), Some(4));
        assert_eq!(s.at, 150.0, "stops at the end");
        let mut s = Scrub::new(-1, 10.0);
        s.since -= Duration::from_secs(3);
        s.last -= Duration::from_secs(1);
        s.advance(150);
        assert_eq!(s.at, 0.0, "stops at the start");
        s.since -= Duration::from_secs(200);
        assert_eq!(s.advance(150), None, "a lost release ends the hold");
    }

    #[test]
    fn events_arrive_out_of_order_and_show_by_time() {
        let mut e = Events::default();
        e.insert(text(5000, Some(7000), "second"));
        e.insert(text(1000, Some(3000), "first"));
        assert_eq!(text_at(&e, 500), None);
        assert_eq!(text_at(&e, 1000).as_deref(), Some("first"));
        assert_eq!(text_at(&e, 3000), None, "ends at its end");
        assert_eq!(text_at(&e, 6000).as_deref(), Some("second"));
    }

    #[test]
    fn a_bitmap_stays_until_the_clear() {
        let mut e = Events::default();
        e.insert(text(1000, None, "bitmap stand-in"));
        e.insert(clear(4000));
        assert_eq!(text_at(&e, 3999).as_deref(), Some("bitmap stand-in"));
        assert_eq!(text_at(&e, 4000), None);
    }

    #[test]
    fn a_long_cue_survives_a_short_one_inside_it() {
        let mut e = Events::default();
        e.insert(text(1000, Some(20_000), "long"));
        e.insert(text(2000, Some(3000), "short"));
        assert_eq!(text_at(&e, 2500).as_deref(), Some("short"));
        assert_eq!(text_at(&e, 10_000).as_deref(), Some("long"));
    }

    #[test]
    fn old_events_are_forgotten_but_the_last_few_stay() {
        let mut e = Events::default();
        for i in 0..100 {
            e.insert(text(i * 1000, Some(i * 1000 + 500), "x"));
        }
        e.prune(99_000);
        assert!(e.list.len() < 100 && e.list.len() >= 6);
        assert_eq!(e.list.last().unwrap().start_ms, 99_000);
    }
}
