//! Drawing: the renderer, fonts, generated shape textures and the card.
use crate::data::{Item, Kind};
use crate::sdl::*;

pub const W: f32 = 1920.0;
pub const H: f32 = 1080.0;
pub const MARGIN: f32 = 112.0;
pub const GAP: f32 = 32.0;
pub const ROW_Y: f32 = 560.0;
pub const LABEL_H: f32 = 56.0;
const GROW: f32 = 0.1;
/// Radius of the generated corner textures.
const RAD: i32 = 64;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}
pub const BG: Color = rgb(12, 17, 28);
pub const TEXT: Color = rgb(232, 234, 237);
pub const DIM: Color = rgb(154, 160, 166);
pub const WHITE: Color = rgb(255, 255, 255);
pub const BLACK: Color = rgb(0, 0, 0);
pub const YELLOW: Color = rgb(251, 188, 4);
pub const PANEL: Color = rgb(38, 41, 46);

pub fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

/// Moves `v` towards `target`; true while it is still moving, so the loop keeps drawing.
pub fn ease(v: &mut f32, target: f32, dt: f32, k: f32) -> bool {
    let d = target - *v;
    if d.abs() < 0.002 {
        *v = target;
        return false;
    }
    *v += d * (1.0 - (-k * dt).exp());
    true
}

/// Everything that draws: the renderer, the fonts and the generated shape textures.
pub struct Gfx {
    pub root: String,
    pub ren: *mut Opaque,
    pub f_label: *mut Opaque,
    pub f_row: *mut Opaque,
    pub f_hero: *mut Opaque,
    pub f_title: *mut Opaque,
    pub f_sub: *mut Opaque,
    pub f_clock: *mut Opaque,
    pub f_menu: *mut Opaque,
    pub circle: Tex,
    pub soft: Tex,
    pub hgrad: Tex,
    pub vgrad: Tex,
    pub gear: Tex,
    /// `icons::ALL` in order.
    icons: Vec<Tex>,
}

impl Gfx {
    pub fn new(root: &str, ren: *mut Opaque, font: impl Fn(&str, i32) -> *mut Opaque) -> Gfx {
        let semibold = "LG_Smart_UI-SemiBold.ttf";
        let regular = "LG_Smart_UI-Regular.ttf";
        Gfx {
            f_label: font(semibold, 26),
            f_row: font(semibold, 30),
            f_hero: font(semibold, 80),
            f_title: font(semibold, 48),
            f_sub: font(regular, 30),
            f_clock: font(regular, 34),
            f_menu: font(semibold, 34),
            root: root.to_string(),
            ren,
            circle: gen(ren, 2 * RAD, 2 * RAD, f_circle),
            soft: gen(ren, 2 * RAD, 2 * RAD, f_soft),
            hgrad: gen(ren, 256, 1, f_hgrad),
            vgrad: gen(ren, 1, 256, f_vgrad),
            gear: gen(ren, 128, 128, f_gear),
            icons: crate::icons::ALL
                .iter()
                .map(|i| {
                    gen(ren, crate::icons::SIZE, crate::icons::SIZE, |x, y, w, _| {
                        crate::icons::coverage(*i, x, y, w)
                    })
                })
                .collect(),
        }
    }

    /// An icon `size` pixels wide with its top left at `(x, y)`.
    pub fn icon(&self, icon: crate::icons::Icon, x: f32, y: f32, size: f32, c: Color, a: f32) {
        let Some(t) = crate::icons::ALL
            .iter()
            .position(|i| *i == icon)
            .and_then(|i| self.icons.get(i))
        else {
            return;
        };
        self.tint(t, c, a);
        self.copy(t, None, FRect { x, y, w: size, h: size });
    }

    pub fn rp(&self, p: &str) -> String {
        format!("{}{p}", self.root)
    }

    pub fn tint(&self, t: &Tex, c: Color, a: f32) {
        unsafe {
            SDL_SetTextureColorMod(t.ptr(), c.r, c.g, c.b);
            SDL_SetTextureAlphaMod(t.ptr(), (clamp(a, 0.0, 1.0) * 255.0) as u8);
        }
    }

    pub fn copy(&self, t: &Tex, src: Option<Rect>, dst: FRect) {
        let src = src.as_ref().map_or(std::ptr::null(), |r| r as *const Rect);
        unsafe { SDL_RenderCopyF(self.ren, t.ptr(), src, &dst) };
    }

    pub fn fill(&self, c: Color, a: f32, dst: FRect) {
        unsafe {
            SDL_SetRenderDrawColor(self.ren, c.r, c.g, c.b, (clamp(a, 0.0, 1.0) * 255.0) as u8);
            SDL_RenderFillRectF(self.ren, &dst);
        }
    }

    pub fn put(&self, t: Option<&Tex>, x: f32, y: f32, c: Color, a: f32) {
        if let Some(t) = t.filter(|_| a > 0.0) {
            self.tint(t, c, a);
            self.copy(
                t,
                None,
                FRect {
                    x,
                    y,
                    w: t.w as f32,
                    h: t.h as f32,
                },
            );
        }
    }

    /// A rounded rectangle of corner radius `r` from a `2 * RAD` circle texture, as a nine-slice.
    #[allow(clippy::too_many_arguments)]
    pub fn nine(&self, t: &Tex, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, a: f32) {
        if a <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let r = r.min(w.min(h) / 2.0);
        self.tint(t, c, a);
        let (so, ss) = ([0, RAD - 1, RAD], [RAD, 2, RAD]);
        let (dx, dw) = ([x, x + r, x + w - r], [r, w - 2.0 * r, r]);
        let (dy, dh) = ([y, y + r, y + h - r], [r, h - 2.0 * r, r]);
        for j in 0..3 {
            for i in 0..3 {
                if dw[i] > 0.0 && dh[j] > 0.0 {
                    let src = Rect {
                        x: so[i],
                        y: so[j],
                        w: ss[i],
                        h: ss[j],
                    };
                    self.copy(
                        t,
                        Some(src),
                        FRect {
                            x: dx[i],
                            y: dy[j],
                            w: dw[i],
                            h: dh[j],
                        },
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rounded(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, a: f32) {
        self.nine(&self.circle, x, y, w, h, r, c, a);
    }

    pub fn text(&self, font: *mut Opaque, s: &str, wrap: u32) -> Option<Tex> {
        if font.is_null() {
            return None;
        }
        let s = cstr(s);
        let su = unsafe {
            if wrap > 0 {
                TTF_RenderUTF8_Blended_Wrapped(font, s.as_ptr(), WHITE, wrap)
            } else {
                TTF_RenderUTF8_Blended(font, s.as_ptr(), WHITE)
            }
        };
        if su.is_null() {
            return None;
        }
        Tex::new(self.ren, &Surf(su))
    }

    /// Loads the card picture: an icon as it is, a still cropped to the card's shape with rounded corners.
    fn load_icon(&self, it: &mut Item) -> Option<Tex> {
        let s = it.icons.iter().find_map(|p| Surf::load(&self.rp(p)))?;
        if it.cover {
            // A poster stays upright; anything else becomes a 16:9 still.
            let (w, h, r) = if s.size().1 > s.size().0 {
                (300, 450, 22)
            } else {
                (640, 360, 28)
            };
            cover(&s, w, h, r).and_then(|c| Tex::new(self.ren, &c))
        } else {
            if it.kind == Kind::App {
                if let Some((src, bg)) = logo_box(&s) {
                    it.icon_src = Some(src);
                    it.color = bg;
                }
            }
            Tex::new(self.ren, &s)
        }
    }

    /// `dim` darkens the card by laying the background colour over it: a card made translucent instead would
    /// show its own layers through each other.
    pub fn card(&self, it: &mut Item, (cw, ch): (f32, f32), x: f32, y: f32, a: f32, dim: f32) {
        let s = 1.0 + GROW * it.focus * if it.moving { 1.6 } else { 1.0 };
        let (w, h) = (cw * s, ch * s);
        let (x0, y0) = (x + cw / 2.0 - w / 2.0, y + ch / 2.0 - h / 2.0);
        it.rect = FRect { x: x0, y: y0, w, h };
        let f = it.focus * a;
        let dim = if it.hidden { dim.max(0.55) } else { dim };
        self.nine(
            &self.soft,
            x0 - 28.0,
            y0 - 10.0,
            w + 56.0,
            h + 64.0,
            56.0,
            BLACK,
            0.7 * f,
        );
        self.rounded(
            x0 - 5.0,
            y0 - 5.0,
            w + 10.0,
            h + 10.0,
            19.0,
            if it.moving { YELLOW } else { WHITE },
            f,
        );
        self.rounded(x0, y0, w, h, 14.0, it.color, a);
        if !it.icon_tried {
            it.icon_tried = true;
            it.icon = self.load_icon(it);
        }
        if let Some(icon) = &it.icon {
            let (iw, ih) = it
                .icon_src
                .map_or((icon.w as f32, icon.h as f32), |r| (r.w as f32, r.h as f32));
            self.tint(icon, WHITE, a);
            if it.cover {
                self.copy(icon, None, FRect { x: x0, y: y0, w, h });
            } else {
                let tall = match (it.kind == Kind::Input, it.icon_src.is_some()) {
                    (true, _) => 0.42,
                    (_, true) => 0.46,
                    _ => 0.62,
                };
                let k = (h * tall / ih).min(w * 0.64 / iw);
                let iy = if it.kind == Kind::Input {
                    y0 + h * 0.38 - ih * k / 2.0
                } else {
                    y0 + (h - ih * k) / 2.0
                };
                self.copy(
                    icon,
                    it.icon_src,
                    FRect {
                        x: x0 + (w - iw * k) / 2.0,
                        y: iy,
                        w: iw * k,
                        h: ih * k,
                    },
                );
            }
        }
        if it.progress > 0.0 {
            let (bx, by, bw) = (x0 + 14.0, y0 + h - 16.0, w - 28.0);
            self.rounded(bx, by, bw, 6.0, 3.0, BLACK, 0.55 * a);
            self.rounded(bx, by, (bw * it.progress).max(6.0), 6.0, 3.0, WHITE, a);
        }
        if it.label.is_none() {
            it.label = self.text(self.f_label, &it.title, 0);
        }
        let (lw, lh) = it.label.as_ref().map_or((0.0, 0.0), |t| (t.w as f32, t.h as f32));
        let label = it.label.as_ref();
        if it.kind == Kind::Input {
            self.put(label, x0 + (w - lw) / 2.0, y0 + h * 0.72 - lh / 2.0, TEXT, a);
            let dot = if it.connected {
                rgb(52, 168, 83)
            } else {
                rgb(95, 99, 104)
            };
            self.rounded(x0 + w - 32.0, y0 + 18.0, 14.0, 14.0, 7.0, dot, a);
        } else if it.icon.is_none() {
            self.put(label, x0 + (w - lw) / 2.0, y0 + (h - lh) / 2.0, WHITE, a);
        }
        self.rounded(x0, y0, w, h, 14.0, BG, dim * a);
        // A poster carries its own title, and the page shows it above.
        if !matches!(it.kind, Kind::Input | Kind::Toggle) && cw >= ch {
            self.put(label, x0, y0 + h + 16.0, TEXT, f);
        }
    }
}

/// A white texture whose alpha is `f(x, y, w, h)`.
fn gen(ren: *mut Opaque, w: i32, h: i32, f: impl Fn(f32, f32, f32, f32) -> f32) -> Tex {
    let s = Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888) });
    let stride = unsafe { (*s.0).pitch as usize / 4 };
    let px = unsafe { (*s.0).pixels as *mut u32 };
    for y in 0..h {
        for x in 0..w {
            let a = (clamp(f(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32), 0.0, 1.0) * 255.0 + 0.5) as u32;
            unsafe { *px.add(y as usize * stride + x as usize) = a << 24 | 0xFF_FFFF };
        }
    }
    let t = Tex::new(ren, &s).expect("texture");
    unsafe { SDL_SetTextureBlendMode(t.ptr(), BLENDMODE_BLEND) };
    t
}

/// The middle of `s` (YouTube thumbnails often have a frame) averaged down to 32 x 18 pixels and darkened. Stretched over the screen with smooth scaling that is a soft glow of its
/// colours: a small YouTube thumbnail enlarged to 1080p would otherwise be blocky and its text would fight the title.
pub fn blur(s: &Surf) -> Option<Surf> {
    const OUT: (i32, i32) = (32, 18);
    let (w, h) = s.size();
    let (mx, my) = (w / 10, h / 10);
    let mk = |w, h| Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888) });
    let (src, dst) = (mk(w, h), mk(OUT.0, OUT.1));
    if src.0.is_null()
        || dst.0.is_null()
        || unsafe { SDL_UpperBlitScaled(s.0, std::ptr::null(), src.0, std::ptr::null_mut()) } != 0
    {
        return None;
    }
    let (sp, dp) = unsafe { ((*src.0).pitch as usize / 4, (*dst.0).pitch as usize / 4) };
    let (sx, dx) = unsafe { ((*src.0).pixels as *const u32, (*dst.0).pixels as *mut u32) };
    for oy in 0..OUT.1 {
        for ox in 0..OUT.0 {
            let (iw, ih) = (w - 2 * mx, h - 2 * my);
            let (x0, x1) = (
                mx + ox * iw / OUT.0,
                mx + ((ox + 1) * iw / OUT.0).max(ox * iw / OUT.0 + 1),
            );
            let (y0, y1) = (
                my + oy * ih / OUT.1,
                my + ((oy + 1) * ih / OUT.1).max(oy * ih / OUT.1 + 1),
            );
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = unsafe { *sx.add(y as usize * sp + x as usize) };
                    for (i, c) in sum.iter_mut().enumerate() {
                        *c += (p >> (16 - 8 * i)) & 255;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let c = |i: usize| sum[i] / n * 7 / 10;
            let px = 255 << 24 | c(0) << 16 | c(1) << 8 | c(2);
            unsafe { *dx.add(oy as usize * dp + ox as usize) = px };
        }
    }
    Some(dst)
}

/// For an app icon that is a flat colour with a logo in the middle: the box around the logo (a little loose) and
/// the colour. The card takes that colour, so the padding disappears and the logo can be drawn larger.
fn logo_box(s: &Surf) -> Option<(Rect, Color)> {
    let (w, h) = s.size();
    let d = Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888) });
    if d.0.is_null() || unsafe { SDL_UpperBlitScaled(s.0, std::ptr::null(), d.0, std::ptr::null_mut()) } != 0 {
        return None;
    }
    let stride = unsafe { (*d.0).pitch as usize / 4 };
    let px = |x: i32, y: i32| unsafe { *((*d.0).pixels as *const u32).add(y as usize * stride + x as usize) };
    let far = |a: u32, b: u32| (0..3).any(|i| ((a >> (8 * i)) & 255).abs_diff((b >> (8 * i)) & 255) > 40);
    let bg = px(0, 0);
    if bg >> 24 != 255
        || [px(w - 1, 0), px(0, h - 1), px(w - 1, h - 1)]
            .iter()
            .any(|&c| far(c, bg))
    {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, -1, -1);
    for y in 0..h {
        for x in 0..w {
            if far(px(x, y), bg) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x1 < x0 || (x1 - x0) * 5 < w / 2 && (y1 - y0) * 5 < h / 2 {
        return None;
    }
    let (x0, y0, x1, y1) = ((x0 - 2).max(0), (y0 - 2).max(0), (x1 + 3).min(w), (y1 + 3).min(h));
    let c = |i: u32| ((bg >> i) & 255) as u8;
    Some((
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        },
        Color {
            r: c(16),
            g: c(8),
            b: c(0),
            a: 255,
        },
    ))
}

/// `src` scaled to fill `w` x `h` (cropped in the middle), with corners of radius `r` made transparent.
fn cover(src: &Surf, w: i32, h: i32, r: i32) -> Option<Surf> {
    let (sw, sh) = src.size();
    let (cw, ch) = if sw * h > sh * w {
        (sh * w / h, sh)
    } else {
        (sw, sw * h / w)
    };
    let crop = Rect {
        x: (sw - cw) / 2,
        y: (sh - ch) / 2,
        w: cw,
        h: ch,
    };
    let dst = Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, w, h, 32, PIXELFORMAT_ARGB8888) });
    if dst.0.is_null() || unsafe { SDL_UpperBlitScaled(src.0, &crop, dst.0, std::ptr::null_mut()) } != 0 {
        return None;
    }
    let stride = unsafe { (*dst.0).pitch as usize / 4 };
    let px = unsafe { (*dst.0).pixels as *mut u32 };
    for (cx, cy, ox, oy) in [
        (r, r, 0, 0),
        (w - r, r, w - r, 0),
        (r, h - r, 0, h - r),
        (w - r, h - r, w - r, h - r),
    ] {
        for y in oy..oy + r {
            for x in ox..ox + r {
                let d = ((x as f32 + 0.5 - cx as f32).hypot(y as f32 + 0.5 - cy as f32)) - r as f32;
                let keep = clamp(0.5 - d, 0.0, 1.0);
                let p = unsafe { &mut *px.add(y as usize * stride + x as usize) };
                *p = (((*p >> 24) as f32 * keep) as u32) << 24 | (*p & 0xFF_FFFF);
            }
        }
    }
    Some(dst)
}

fn f_circle(x: f32, y: f32, w: f32, h: f32) -> f32 {
    w / 2.0 - (x - w / 2.0).hypot(y - h / 2.0) + 0.5
}
fn f_soft(x: f32, y: f32, w: f32, h: f32) -> f32 {
    let a = clamp(1.0 - (x - w / 2.0).hypot(y - h / 2.0) / (w / 2.0), 0.0, 1.0);
    a * a * (3.0 - 2.0 * a)
}
/// A cog: a disc with eight teeth and a hole.
fn f_gear(x: f32, y: f32, w: f32, h: f32) -> f32 {
    let (dx, dy) = (x - w / 2.0, y - h / 2.0);
    let r = dx.hypot(dy) / (w / 2.0);
    let tooth = ((8.0 * dy.atan2(dx)).cos() * 3.0).clamp(-0.5, 0.5) + 0.5;
    let px = 2.0 / w;
    (((0.72 + 0.22 * tooth) - r) / px + 0.5).min((r - 0.3) / px + 0.5)
}
fn f_hgrad(x: f32, _: f32, w: f32, _: f32) -> f32 {
    1.0 - x / w
}
fn f_vgrad(_: f32, y: f32, _: f32, h: f32) -> f32 {
    y / h
}
