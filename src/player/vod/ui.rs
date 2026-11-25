//! What the viewer sees and presses while a title plays: the remote, the bar at the bottom, the track menu and the
//! subtitles (text, and the bitmaps of PGS and DVD tracks).
use super::super::*;
use crate::ff::demux::{Bitmap, SubEv, SubWhat};
use crate::film::{self, Item, TICKS};
use crate::subs::Cue;

pub enum Fk {
    Back,
    Toggle,
    Seek(i64),
    Up,
    Down,
}

/// The remote while a title plays. Seconds are what a key seeks by: Left and Right 10, the rewind and fast-forward
/// buttons 30.
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
        if e.kind() != KEYDOWN {
            continue;
        }
        let (sym, wcode) = (e.key_sym(), e.key_wcode());
        if sym == K_ESCAPE || wcode == WCODE_BACK {
            return Some(Fk::Back);
        }
        match (sym, wcode) {
            // 415 and 19 are the remote's Play and Pause
            (K_RETURN | K_KP_ENTER, _) | (_, 415 | 19) => return Some(Fk::Toggle),
            (K_LEFT, _) => return Some(Fk::Seek(-10)),
            (K_RIGHT, _) => return Some(Fk::Seek(10)),
            (_, 412) => return Some(Fk::Seek(-30)),
            (_, 417) => return Some(Fk::Seek(30)),
            (K_UP, _) => return Some(Fk::Up),
            (K_DOWN, _) => return Some(Fk::Down),
            _ => log(&format!("film: key {sym} wcode {wcode}")),
        }
    }
    None
}

// ---- the bar ------------------------------------------------------------------------------------------------------

/// The title, the bar and the times, at the bottom of the picture for a few seconds, and while paused.
pub struct Osd {
    name: Option<Tex>,
    sub: Option<Tex>,
    paused: Option<Tex>,
    total: Option<Tex>,
    total_s: i64,
    at: (String, Option<Tex>),
    status: Option<Tex>,
    /// A status that goes by itself (a failed subtitle download), unlike an error that stays until Back.
    status_until: Option<Instant>,
    until: Instant,
}

impl Osd {
    pub fn new(g: &Gfx, it: &Item) -> Osd {
        let total_s = it.total / TICKS;
        Osd {
            name: g.text(g.f_row, &it.name, 1500),
            sub: g.text(g.f_sub, &it.sub, 1500),
            paused: g.text(g.f_sub, "Paused", 0),
            total: g.text(g.f_sub, &film::clock(total_s), 0),
            total_s,
            at: (String::new(), None),
            status: None,
            status_until: None,
            until: Instant::now() + Duration::from_secs(6),
        }
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

    /// Whether anything was drawn.
    fn draw(&mut self, g: &Gfx, pos_s: i64, paused: bool, seeking: Option<i64>) -> bool {
        if self.status_until.is_some_and(|t| Instant::now() > t) {
            (self.status, self.status_until) = (None, None);
        }
        if !(paused || seeking.is_some() || self.status.is_some() || Instant::now() < self.until) {
            return false;
        }
        let shown = seeking.unwrap_or(pos_s);
        let label = film::clock(shown);
        if self.at.0 != label {
            self.at = (label.clone(), g.text(g.f_sub, &label, 0));
        }
        let (x, w, h) = (MARGIN, W - 2.0 * MARGIN, 210.0);
        let y = H - h - 64.0;
        g.rounded(x, y, w, h, 28.0, PANEL, 0.94);
        g.put(self.name.as_ref(), x + 48.0, y + 28.0, TEXT, 1.0);
        g.put(self.sub.as_ref(), x + 48.0, y + 82.0, DIM, 1.0);
        if let Some(s) = &self.status {
            g.put(Some(s), x + 48.0, y + 132.0, DIM, 1.0);
            return true;
        }
        if let Some(p) = self.paused.as_ref().filter(|_| paused) {
            g.put(Some(p), x + w - 48.0 - p.w as f32, y + 34.0, TEXT, 1.0);
        }
        let (bx, bw, by) = (x + 48.0, w - 96.0, y + h - 72.0);
        g.rounded(bx, by, bw, 8.0, 4.0, BLACK, 0.5);
        let f = if self.total_s > 0 {
            (shown as f32 / self.total_s as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if bw * f >= 8.0 {
            g.rounded(bx, by, bw * f, 8.0, 4.0, TEXT, 1.0);
        }
        g.put(self.at.1.as_ref(), bx, by + 22.0, TEXT, 1.0);
        if let Some(t) = &self.total {
            g.put(Some(t), bx + bw - t.w as f32, by + 22.0, DIM, 1.0);
        }
        true
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

/// A subtitle as one picture per line, so each can be centred.
pub fn lines(g: &Gfx, text: &str) -> Vec<Tex> {
    text.lines().filter_map(|l| g.text(g.f_row, l, 0)).collect()
}

// ---- the track menu -----------------------------------------------------------------------------------------------

pub struct AudioRow {
    pub index: i64,
    pub name: String,
}

pub struct SubRow {
    pub index: i64,
    pub name: String,
    pub server: bool,
}

#[derive(Default)]
pub struct Tracks {
    pub audio: Vec<AudioRow>,
    pub subs: Vec<SubRow>,
}

pub enum Pick {
    Head,
    Sub(Option<SubSel>),
    Audio(i64),
}

/// Subtitles and audio tracks to choose from, with Down.
pub struct Menu {
    pub rows: Vec<(Option<Tex>, Pick, bool)>,
    pub sel: usize,
}

impl Menu {
    /// None when there is nothing to choose: no subtitles and at most one audio track.
    pub fn new(g: &Gfx, t: &Tracks, c: &Choice) -> Option<Menu> {
        let mut rows = vec![];
        let mut row = |name: &str, pick, current| {
            rows.push((g.text(g.f_sub, &crate::jellyfin::clip(name, 46), 0), pick, current))
        };
        if !t.subs.is_empty() {
            row("Subtitles", Pick::Head, false);
            row("Off", Pick::Sub(None), c.sub.is_none());
            for s in &t.subs {
                let sel = SubSel {
                    index: s.index,
                    server: s.server,
                };
                row(&s.name, Pick::Sub(Some(sel)), c.sub.is_some_and(|x| x.index == s.index));
            }
        }
        if t.audio.len() > 1 {
            row("Audio", Pick::Head, false);
            for (n, a) in t.audio.iter().enumerate() {
                row(
                    &a.name,
                    Pick::Audio(a.index),
                    c.audio == Some(a.index) || (c.audio.is_none() && n == 0),
                );
            }
        }
        let sel = rows.iter().position(|r| r.2 && !matches!(r.1, Pick::Head))?;
        Some(Menu { rows, sel })
    }

    pub fn step(&mut self, d: i32) {
        let mut i = self.sel as i32;
        loop {
            i += d;
            match self.rows.get(i as usize).filter(|_| i >= 0) {
                None => return,
                Some((_, Pick::Head, _)) => {}
                Some(_) => return self.sel = i as usize,
            }
        }
    }

    fn draw(&self, g: &Gfx) {
        const ROW: f32 = 64.0;
        let shown = self.rows.len().min(9);
        let first = (self.sel + 1).saturating_sub(shown).min(self.rows.len() - shown);
        let (w, h) = (760.0, shown as f32 * ROW + 32.0);
        let (x, y) = (W - MARGIN - w, (H - h) / 2.0);
        g.rounded(x, y, w, h, 28.0, PANEL, 0.96);
        for (n, (tex, pick, current)) in self.rows.iter().enumerate().skip(first).take(shown) {
            let ry = y + 16.0 + (n - first) as f32 * ROW;
            let on = n == self.sel;
            if on {
                g.rounded(x + 16.0, ry + 4.0, w - 32.0, ROW - 8.0, 16.0, TEXT, 1.0);
            }
            let ink = if on {
                rgb(20, 22, 25)
            } else if matches!(pick, Pick::Head) {
                DIM
            } else {
                TEXT
            };
            if *current {
                g.rounded(x + 36.0, ry + ROW / 2.0 - 7.0, 14.0, 14.0, 7.0, ink, 1.0);
            }
            let th = tex.as_ref().map_or(0, |t| t.h) as f32;
            g.put(tex.as_ref(), x + 68.0, ry + (ROW - th) / 2.0, ink, 1.0);
        }
    }
}

// ---- a frame ------------------------------------------------------------------------------------------------------

/// What a picture adds to the transparent frame besides the controls.
#[derive(Default)]
pub struct Over<'a> {
    pub shown: Option<&'a Shown>,
    pub menu: Option<&'a Menu>,
}

/// A transparent frame, so the video plane shows, with the subtitle, the controls and the track menu over it.
pub fn vframe(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<i64>, over: Over) {
    unsafe {
        SDL_SetRenderDrawColor(g.ren, 0, 0, 0, 0);
        SDL_RenderClear(g.ren);
    }
    draw_over(g, osd, pos_s, paused, seeking, over);
    unsafe { SDL_RenderPresent(g.ren) };
}

fn draw_over(g: &Gfx, osd: &mut Osd, pos_s: i64, paused: bool, seeking: Option<i64>, over: Over) {
    // The subtitle stands above the controls when they are up.
    let bar = osd.draw(g, pos_s, paused, seeking);
    if let Some(s) = over.shown {
        for (t, dst) in &s.images {
            g.tint(t, rgb(255, 255, 255), 1.0);
            g.copy(t, None, *dst);
        }
        if !s.lines.is_empty() {
            let w = s.lines.iter().map(|t| t.w).max().unwrap_or(0) as f32;
            let h: f32 = s.lines.iter().map(|t| t.h as f32).sum();
            let top = if s.top {
                70.0
            } else {
                (if bar { H - 300.0 } else { H - 120.0 }) - h
            };
            g.rounded((W - w) / 2.0 - 20.0, top - 10.0, w + 40.0, h + 20.0, 16.0, BLACK, 0.6);
            let mut y = top;
            for t in &s.lines {
                g.put(Some(t), (W - t.w as f32) / 2.0, y, TEXT, 1.0);
                y += t.h as f32;
            }
        }
    }
    if let Some(m) = over.menu {
        m.draw(g);
    }
}

/// `--film-shot` on a desktop: the controls, a subtitle and the track menu over a stand-in picture (no player).
pub fn shot(g: &Gfx, menu: bool) {
    let it = Item {
        backend: film::Backend::Jellyfin(crate::jellyfin::Config::default()),
        audio: vec![],
        subs: vec![],
        default_audio: None,
        default_sub: None,
        id: "m1".into(),
        series: None,
        source: "s".into(),
        direct: String::new(),
        name: "The Silent Harbour".into(),
        sub: "Harbour Lights   S2 E5".into(),
        total: 5400 * TICKS,
        resume: 0,
        session: String::new(),
    };
    let mut osd = Osd::new(g, &it);
    let tracks = Tracks {
        audio: vec![
            AudioRow {
                index: 1,
                name: "French - AC3 5.1".into(),
            },
            AudioRow {
                index: 2,
                name: "English - AAC Stereo".into(),
            },
        ],
        subs: vec![
            SubRow {
                index: 3,
                name: "French - SUBRIP".into(),
                server: false,
            },
            SubRow {
                index: 4,
                name: "English - PGS".into(),
                server: false,
            },
        ],
    };
    let choice = Choice {
        sub: Some(SubSel {
            index: 3,
            server: false,
        }),
        ..Choice::default()
    };
    let menu = menu.then(|| Menu::new(g, &tracks, &choice)).flatten();
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
    g.fill(
        rgb(54, 78, 112),
        1.0,
        FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: H,
        },
    );
    let over = Over {
        shown: Some(&shown),
        menu: menu.as_ref(),
    };
    draw_over(g, &mut osd, 1234, true, None, over);
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
