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
    /// OK: pauses, or skips what the button on screen offers.
    Toggle,
    /// The remote's Play and Pause keys: always pause.
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

/// The remote while a title plays.
pub fn fkey() -> Option<Fk> {
    if STOP.load(Ordering::SeqCst) || crate::lifecycle::RELAUNCH.load(Ordering::SeqCst) {
        return Some(Fk::Back);
    }
    let mut e = Event::new(0);
    while unsafe { SDL_PollEvent(&mut e) } != 0 {
        if e.kind() == QUIT {
            unsafe { SDL_PushEvent(&mut Event::new(QUIT)) };
            return Some(Fk::Back);
        }
        let (sym, wcode) = (e.key_sym(), e.key_wcode());
        if e.kind() == KEYUP && (sym == K_LEFT || sym == K_RIGHT) {
            return Some(Fk::Release);
        }
        if e.kind() != KEYDOWN {
            continue;
        }
        // 413 is the remote's Stop
        if sym == K_ESCAPE || wcode == WCODE_BACK || wcode == 413 {
            return Some(Fk::Back);
        }
        match (sym, wcode) {
            // 415 and 19 are the remote's Play and Pause
            (K_RETURN | K_KP_ENTER, _) => return Some(Fk::Toggle),
            (_, 415 | 19) => return Some(Fk::Pause),
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
    /// The title and its line while loading, larger and in the middle.
    big_name: Option<Tex>,
    stage: Label,
    started: Instant,
    /// The title's backdrop from the launcher's cache, read on a thread, behind the loading screen.
    backdrop: Option<std::sync::mpsc::Receiver<Option<Surf>>>,
    backdrop_tex: Option<Tex>,
    total: Option<Tex>,
    total_s: i64,
    at: Label,
    ends: Label,
    delta: Label,
    status: Option<Tex>,
    chapters: Vec<Chapter>,
    /// The chapter whose name is drawn, and the name.
    chapter: (Option<usize>, Option<Tex>),
    skip: Option<(SegKind, Tex)>,
    /// A status that goes by itself (a failed subtitle download), unlike an error that stays until Back.
    status_until: Option<Instant>,
    until: Instant,
    buttons: Vec<(Btn, Option<Tex>)>,
    /// The focused button, when the focus is on the buttons rather than on the bar.
    pub focus: Option<usize>,
}

impl Osd {
    pub fn new(g: &Gfx, it: &Item) -> Osd {
        let total_s = it.total / TICKS;
        Osd {
            name: g.text(g.f_row, &crate::jellyfin::clip(&it.name, 40), 0),
            sub: g.text(g.f_sub, &it.sub, 1000),
            big_name: g.text(g.f_title, &crate::jellyfin::clip(&it.name, 48), 0),
            stage: Label::default(),
            started: Instant::now(),
            backdrop: backdrop_file(it).map(|path| {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(Surf::load(&path));
                });
                rx
            }),
            backdrop_tex: None,
            total: g.text(g.f_sub, &film::clock(total_s), 0),
            total_s,
            at: Label::default(),
            ends: Label::default(),
            delta: Label::default(),
            status: None,
            chapters: it.chapters.clone(),
            chapter: (None, None),
            skip: None,
            status_until: None,
            until: Instant::now() + Duration::from_secs(6),
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
        self.until = Instant::now() + Duration::from_secs(5);
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

    /// The first start: the title in the middle of a dark screen, a spinner, and what it is waiting for.
    fn draw_loading(&mut self, g: &Gfx, stage: &str) {
        g.fill(rgb(8, 10, 14), 1.0, SCREEN);
        if let Some(Ok(s)) = self.backdrop.as_ref().map(|r| r.try_recv()) {
            self.backdrop = None;
            self.backdrop_tex = s.and_then(|s| Tex::new(g.ren, &s));
        }
        if let Some(b) = &self.backdrop_tex {
            // Cropped to fill the screen, dimmed so the title reads over it.
            let (bw, bh) = (b.w as f32, b.h as f32);
            let (cw, ch) = if bw * H > bh * W {
                (bh * W / H, bh)
            } else {
                (bw, bw * H / W)
            };
            let src = Rect {
                x: ((bw - cw) / 2.0) as i32,
                y: ((bh - ch) / 2.0) as i32,
                w: cw as i32,
                h: ch as i32,
            };
            g.tint(b, rgb(255, 255, 255), 1.0);
            g.copy(b, Some(src), SCREEN);
            g.fill(rgb(8, 10, 14), 0.72, SCREEN);
        }
        let t = self.started.elapsed().as_secs_f32();
        // Fades in, so a start that is quick does not flash.
        let a = ((t - 0.15) * 4.0).clamp(0.0, 1.0);
        spinner(g, W / 2.0, H / 2.0 - 120.0, 44.0, t);
        let mut y = H / 2.0 - 30.0;
        if let Some(n) = &self.big_name {
            g.put(Some(n), (W - n.w as f32) / 2.0, y, TEXT, a);
            y += n.h as f32 + 12.0;
        }
        if let Some(s) = &self.sub {
            g.put(Some(s), (W - s.w as f32) / 2.0, y, DIM, a);
            y += s.h as f32 + 40.0;
        }
        if let Some(s) = self.status.as_ref() {
            return g.put(Some(s), (W - s.w as f32) / 2.0, y, TEXT, 1.0);
        }
        // A server that takes long is usually waking up from sleep.
        let stage = if t > 12.0 {
            format!("{stage}   ·   the server may be waking up")
        } else {
            stage.to_string()
        };
        if let Some(s) = self.stage.set(g, g.f_sub, &stage) {
            g.put(Some(s), (W - s.w as f32) / 2.0, y, DIM, a);
        }
    }

    /// Where the bar is, when it was drawn.
    fn draw(&mut self, g: &Gfx, pos_s: i64, paused: bool, seeking: Option<Seeking>) -> Option<Bar> {
        if self.status_until.is_some_and(|t| Instant::now() > t) {
            (self.status, self.status_until) = (None, None);
        }
        if paused && seeking.is_none() && self.focus.is_none() {
            let d = 132.0;
            g.rounded((W - d) / 2.0, (H - d) / 2.0 - 60.0, d, d, d / 2.0, PANEL, 0.75);
            g.icon(Icon::Pause, (W - 60.0) / 2.0, (H - 60.0) / 2.0 - 60.0, 60.0, TEXT, 1.0);
        }
        if !(paused || seeking.is_some() || self.status.is_some() || Instant::now() < self.until) {
            self.focus = None;
            return None;
        }
        let shown = seeking.map_or(pos_s, |s| s.to);
        let (x, w, h) = (MARGIN, W - 2.0 * MARGIN, 230.0);
        let y = H - h - 64.0;
        g.rounded(x, y, w, h, 28.0, PANEL, 0.94);
        g.put(self.name.as_ref(), x + 48.0, y + 28.0, TEXT, 1.0);
        g.put(self.sub.as_ref(), x + 48.0, y + 88.0, DIM, 1.0);
        let mut bx = x + w - 48.0;
        for (i, (b, t)) in self.buttons.iter().enumerate().rev() {
            let tw = t.as_ref().map_or(0.0, |t| t.w as f32);
            let (pw, ph, ic) = (tw + 104.0, 64.0, 34.0);
            bx -= pw;
            let on = self.focus == Some(i);
            let ink = if on { rgb(20, 22, 25) } else { TEXT };
            g.rounded(
                bx,
                y + 26.0,
                pw,
                ph,
                ph / 2.0,
                if on { TEXT } else { rgb(70, 74, 80) },
                1.0,
            );
            g.icon(b.icon(), bx + 26.0, y + 26.0 + (ph - ic) / 2.0, ic, ink, 1.0);
            let th = t.as_ref().map_or(0.0, |t| t.h as f32);
            g.put(t.as_ref(), bx + 74.0, y + 26.0 + (ph - th) / 2.0, ink, 1.0);
            bx -= 14.0;
        }
        let (bx, bw, by) = (x + 48.0, w - 96.0, y + h - 76.0);
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
        if let Some(s) = &self.status {
            g.put(Some(s), x + 48.0, y + 146.0, DIM, 1.0);
            return bar;
        }
        g.rounded(bx, by, bw, 8.0, 4.0, BLACK, 0.5);
        if bw * frac >= 8.0 {
            g.rounded(bx, by, bw * frac, 8.0, 4.0, TEXT, 1.0);
        }
        if self.total_s > 0 {
            for c in self.chapters.iter().filter(|c| c.start > 0) {
                let f = (c.start / TICKS) as f32 / self.total_s as f32;
                g.rounded(bx + bw * f - 2.0, by - 6.0, 4.0, 20.0, 2.0, DIM, 1.0);
            }
        }
        if let Some(s) = seeking {
            let head = bx + bw * frac;
            g.rounded(head - 12.0, by - 8.0, 24.0, 24.0, 12.0, TEXT, 1.0);
            let d = s.to - s.from;
            let text = format!("{}{}", if d < 0 { "-" } else { "+" }, film::clock(d.abs()));
            let dots = if s.level > 0 { 4.0 * 18.0 + 12.0 } else { 0.0 };
            if let Some(t) = self.delta.set(g, g.f_sub, &text) {
                let tw = t.w as f32 + dots;
                let tx = (head - tw / 2.0).clamp(bx, bx + bw - tw);
                let ty = by - 18.0 - t.h as f32;
                g.put(Some(t), tx, ty, TEXT, 1.0);
                for i in 0..if s.level > 0 { 4u8 } else { 0 } {
                    let on = i < s.level;
                    let dx = tx + t.w as f32 + 12.0 + f32::from(i) * 18.0;
                    g.rounded(
                        dx,
                        ty + t.h as f32 / 2.0 - 6.0,
                        12.0,
                        12.0,
                        6.0,
                        if on { YELLOW } else { DIM },
                        1.0,
                    );
                }
            }
        }
        let state = if paused { Icon::Pause } else { Icon::Play };
        g.icon(state, bx, by + 28.0, 30.0, TEXT, 1.0);
        let tx = bx + 46.0;
        let at = self.at.set(g, g.f_sub, &film::clock(shown)).map(|t| (t.w as f32, t));
        if let Some((_, t)) = at {
            g.put(Some(t), tx, by + 24.0, TEXT, 1.0);
        }
        let at_w = at.map_or(0.0, |a| a.0);
        let now = chapter_at(&self.chapters, shown * TICKS);
        if self.chapter.0 != now {
            let name = now.map(|i| crate::jellyfin::clip(&self.chapters[i].name, 60));
            self.chapter = (now, name.and_then(|n| g.text(g.f_sub, &n, 0)));
        }
        if let Some(n) = &self.chapter.1 {
            g.put(Some(n), tx + at_w + 40.0, by + 24.0, DIM, 1.0);
        }
        let total_w = self.total.as_ref().map_or(0.0, |t| t.w as f32);
        if let Some(t) = &self.total {
            g.put(Some(t), bx + bw - total_w, by + 24.0, DIM, 1.0);
        }
        if self.total_s > 0 {
            let left = (self.total_s - shown).max(0);
            let now = unsafe { libc::time(std::ptr::null_mut()) } as i64;
            let text = format!("Ends at {}", super::super::hm(now + left));
            if let Some(t) = self.ends.set(g, g.f_sub, &text) {
                g.put(Some(t), bx + bw - total_w - 40.0 - t.w as f32, by + 24.0, DIM, 1.0);
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

/// The backdrop the launcher's library tab cached for the film, or for an episode's series (`library::Page::path`).
fn backdrop_file(it: &Item) -> Option<String> {
    let pre = match it.backend {
        film::Backend::Jellyfin(_) => crate::library::JELLYFIN,
        film::Backend::Plex(_) => crate::library::PLEX,
    };
    let start = format!("{pre}-b-{}-", it.series.as_deref().unwrap_or(&it.id));
    let dir = format!("{}{}/cache", root(), crate::data::DIR);
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with(&start))
        .map(|n| format!("{dir}/{n}"))
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
    title: Option<Tex>,
    hint: Option<Tex>,
    pub rows: Vec<Row>,
    pub sel: Option<usize>,
}

/// What a panel's row says: its name, its value (Options and Info), what it does, and whether it is the current one.
type RowSpec = (String, Option<String>, Pick, bool);

/// The subtitle tracks under their languages: `lead` (the language the viewer reads) first, then the others in the
/// order of the file; in each, the full track, SDH, forced and the rest.
fn sub_rows(subs: &[SubRow], lead: &str, cur: Option<i64>) -> Vec<RowSpec> {
    let mut langs: Vec<&str> = vec![];
    for s in subs {
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
    let mut rows = vec![("Off".to_string(), None, Pick::Sub(None), cur.is_none())];
    for l in langs {
        let name = match crate::lang::name(l) {
            n if n.is_empty() => "Other".to_string(),
            n => n,
        };
        rows.push((name, None, Pick::Line, false));
        let mut list: Vec<&SubRow> = subs.iter().filter(|s| s.lang == l).collect();
        list.sort_by_key(|s| rank(s));
        for s in list {
            let sel = SubSel {
                index: s.index,
                server: s.server,
            };
            let format = (!s.format.is_empty()).then(|| s.format.clone());
            rows.push((s.label.clone(), format, Pick::Sub(Some(sel)), cur == Some(s.index)));
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
            title: g.text(g.f_label, btn.label(), 0),
            hint: (!hint.is_empty()).then(|| g.text(g.f_label, hint, 0)).flatten(),
            rows,
            sel,
        }
    }

    pub fn subs(g: &Gfx, t: &Tracks, c: &Choice, lead: &str) -> Menu {
        let rows = sub_rows(&t.subs, lead, c.sub.map(|s| s.index));
        Menu::build(g, Btn::Subs, "", rows, None)
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
        Menu::build(g, Btn::Options, "Left and Right change it", rows, sel.or(Some(0)))
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
        const ROW: f32 = 64.0;
        let shown = self.rows.len().clamp(1, 10);
        let at = self.sel.unwrap_or(0);
        // The rows around the focus; Info's facts stay in view below its two actions.
        let first = if self.btn == Btn::Info {
            0
        } else {
            (at + 1)
                .saturating_sub(shown - 1)
                .min(self.rows.len().saturating_sub(shown))
        };
        let head = 84.0;
        let foot = if self.hint.is_some() { 52.0 } else { 0.0 };
        let w = if self.btn == Btn::Info { 1040.0 } else { 820.0 };
        let h = head + shown as f32 * ROW + foot + 24.0;
        let (x, y) = (W - MARGIN - w, ((H - 340.0 - h) / 2.0).max(40.0));
        g.rounded(x, y, w, h, 28.0, PANEL, 0.96);
        g.icon(self.btn.icon(), x + 36.0, y + 26.0, 36.0, DIM, 1.0);
        let th = self.title.as_ref().map_or(0.0, |t| t.h as f32);
        g.put(self.title.as_ref(), x + 88.0, y + 26.0 + (36.0 - th) / 2.0, DIM, 1.0);
        for (n, row) in self.rows.iter().enumerate().skip(first).take(shown) {
            let ry = y + head + (n - first) as f32 * ROW;
            let on = self.sel == Some(n);
            if on {
                g.rounded(x + 16.0, ry + 4.0, w - 32.0, ROW - 8.0, 16.0, TEXT, 1.0);
            }
            let ink = if on { rgb(20, 22, 25) } else { TEXT };
            if row.current {
                g.icon(
                    Icon::Check,
                    x + 32.0,
                    ry + (ROW - 28.0) / 2.0,
                    28.0,
                    if on { ink } else { YELLOW },
                    1.0,
                );
            }
            if let Some(i) = row.icon {
                g.icon(i, x + 32.0, ry + (ROW - 30.0) / 2.0, 30.0, ink, 1.0);
            }
            let th = row.tex.as_ref().map_or(0, |t| t.h) as f32;
            let heading = matches!(row.pick, Pick::Line) && row.value.is_none();
            let (tx, name_ink) = match (heading, &row.pick) {
                (true, _) => (x + 36.0, YELLOW),
                (false, Pick::Line) => (x + 76.0, DIM),
                _ => (x + 76.0, ink),
            };
            g.put(row.tex.as_ref(), tx, ry + (ROW - th) / 2.0, name_ink, 1.0);
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
        if let Some(t) = &self.hint {
            g.put(Some(t), x + 36.0, y + h - foot - 8.0, DIM, 1.0);
        }
    }
}

// ---- a frame ------------------------------------------------------------------------------------------------------

/// What a picture adds to the transparent frame besides the controls.
#[derive(Default)]
pub struct Over<'a> {
    pub shown: Option<&'a Shown>,
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

fn draw_over(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<Seeking>, over: Over) {
    if let Some(l) = over.loading.filter(|l| l.full) {
        return osd.draw_loading(g, l.stage);
    }
    // The subtitle stands above the controls when they are up.
    let geom = osd.draw(g, pos_s, paused, seeking);
    let bar = geom.is_some();
    if over.loading.is_some() {
        spinner(g, W / 2.0, H / 2.0 - 60.0, 30.0, osd.started.elapsed().as_secs_f32());
    }
    if let (Some(t), Some(b), true) = (over.thumb, &geom, seeking.is_some()) {
        let w = 420.0;
        let h = w * t.h as f32 / t.w.max(1) as f32;
        let x = (b.x + b.w * b.frac - w / 2.0).clamp(MARGIN, W - MARGIN - w);
        let y = b.top - h - 28.0;
        g.rounded(x - 6.0, y - 6.0, w + 12.0, h + 12.0, 14.0, TEXT, 1.0);
        g.copy(t, None, FRect { x, y, w, h });
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
        for (t, dst) in &a.pics {
            g.tint(t, ink, 1.0);
            g.copy(t, None, *dst);
        }
    }
    let raise = RAISES[look.sub_pos].1;
    if let Some(s) = over.shown.filter(|_| seeking.is_none()) {
        // A picture subtitle grows with the size from the bottom middle of the screen, as text does.
        let k = SIZES[look.sub_size].1 as f32 / 44.0;
        for (t, d) in &s.images {
            let dst = FRect {
                x: W / 2.0 + (d.x - W / 2.0) * k,
                y: (H - (H - d.y) * k - raise).max(0.0),
                w: d.w * k,
                h: d.h * k,
            };
            g.tint(t, ink, 1.0);
            g.copy(t, None, dst);
        }
        if !s.lines.is_empty() {
            let w = s.lines.iter().map(|t| t.w).max().unwrap_or(0) as f32;
            let h: f32 = s.lines.iter().map(|t| t.h as f32).sum();
            let top = if s.top {
                70.0
            } else {
                (if bar { H - 320.0 } else { H - 120.0 - raise }) - h
            };
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

/// `--film-shot out.bmp [what] [backdrop]` on a desktop: the controls over a stand-in picture (no player). `what` is `seek` (a
/// held seek), `options`, `subs`, `info`, `next` (the next episode's card) or `load` (the first start).
pub fn shot(g: &Gfx, what: &str, backdrop: Option<&Tex>) {
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
        "options" => {
            focus(&mut osd, 3);
            Some(Menu::options(g, &look, 300, Some(4)))
        }
        "subs" => {
            focus(&mut osd, 0);
            Some(Menu::subs(g, &tracks, &choice, "eng"))
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
            text: "Bonjour, le monde.\nIl est tard.".into(),
            top: false,
        },
    });
    let mut shown = Shown::default();
    shown.update(g, &events, 1_234_000);
    if let Some(tex) = backdrop {
        g.copy(tex, None, SCREEN);
    } else {
        g.fill(rgb(54, 78, 112), 1.0, SCREEN);
    }
    let (w, h) = (320, 180);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 120, 255])
        .collect();
    let thumb = Tex::rgba(g.ren, w, h, &px);
    let seeking = (what == "seek").then_some(Seeking {
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
    if what == "next" {
        osd.until = Instant::now();
    }
    osd.started -= Duration::from_secs(2);
    let over = Over {
        shown: Some(&shown),
        menu: menu.as_ref(),
        skip: (what == "seek" && backdrop.is_none()).then_some(SegKind::Intro),
        thumb: backdrop.or(thumb.as_ref()),
        ass: None,
        look,
        loading: (what == "load").then_some(Loading {
            stage: "Reading the file",
            full: true,
        }),
        up: (what == "next").then_some((&mut card, 6.4, 10.0)),
    };
    draw_over(g, &mut osd, 1234, what == "options", seeking, over);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rows = sub_rows(&subs, "en", Some(6));
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
