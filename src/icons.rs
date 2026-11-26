//! Icons for the player and Settings, drawn from distance functions as the cog is: white on transparent, tinted when
//! they are drawn. No picture files and no icon font: the TV's fonts have no symbols.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Play,
    Pause,
    Subtitles,
    Audio,
    Chapters,
    Options,
    Info,
    Restart,
    Next,
    Library,
    Check,
    Tabs,
    Server,
    Tv,
    Video,
    Lock,
    Power,
    Person,
}

pub const ALL: [Icon; 18] = [
    Icon::Play,
    Icon::Pause,
    Icon::Subtitles,
    Icon::Audio,
    Icon::Chapters,
    Icon::Options,
    Icon::Info,
    Icon::Restart,
    Icon::Next,
    Icon::Library,
    Icon::Check,
    Icon::Tabs,
    Icon::Server,
    Icon::Tv,
    Icon::Video,
    Icon::Lock,
    Icon::Power,
    Icon::Person,
];

/// The size the icons are made at, in pixels; they are drawn at this size or a little smaller.
pub const SIZE: i32 = 64;

type P = (f32, f32);

fn len(p: P) -> f32 {
    p.0.hypot(p.1)
}
fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn dot(a: P, b: P) -> f32 {
    a.0 * b.0 + a.1 * b.1
}
fn circle(p: P, c: P, r: f32) -> f32 {
    len(sub(p, c)) - r
}
fn ring(p: P, c: P, r: f32, t: f32) -> f32 {
    circle(p, c, r).abs() - t
}
/// A box of half size `h` with corners of radius `r`.
fn rbox(p: P, c: P, h: P, r: f32) -> f32 {
    let q = ((p.0 - c.0).abs() - h.0 + r, (p.1 - c.1).abs() - h.1 + r);
    len((q.0.max(0.0), q.1.max(0.0))) + q.0.max(q.1).min(0.0) - r
}
/// A line from `a` to `b`, `t` thick on each side, with round ends.
fn seg(p: P, a: P, b: P, t: f32) -> f32 {
    let (pa, ba) = (sub(p, a), sub(b, a));
    let h = (dot(pa, ba) / dot(ba, ba)).clamp(0.0, 1.0);
    len(sub(pa, (ba.0 * h, ba.1 * h))) - t
}
fn tri(p: P, a: P, b: P, c: P) -> f32 {
    let (e0, e1, e2) = (sub(b, a), sub(c, b), sub(a, c));
    let (v0, v1, v2) = (sub(p, a), sub(p, b), sub(p, c));
    let near = |v: P, e: P| {
        let k = (dot(v, e) / dot(e, e)).clamp(0.0, 1.0);
        sub(v, (e.0 * k, e.1 * k))
    };
    let (q0, q1, q2) = (near(v0, e0), near(v1, e1), near(v2, e2));
    let s = (e0.0 * e2.1 - e0.1 * e2.0).signum();
    let d0 = (dot(q0, q0), s * (v0.0 * e0.1 - v0.1 * e0.0));
    let d1 = (dot(q1, q1), s * (v1.0 * e1.1 - v1.1 * e1.0));
    let d2 = (dot(q2, q2), s * (v2.0 * e2.1 - v2.1 * e2.0));
    let d = (d0.0.min(d1.0).min(d2.0), d0.1.min(d1.1).min(d2.1));
    -d.0.sqrt() * d.1.signum()
}

/// The distance from `p` (in -1..1, y down) to the icon's shape: negative inside.
fn shape(icon: Icon, p: P) -> f32 {
    const T: f32 = 0.085;
    let (x, y) = p;
    match icon {
        Icon::Play => tri(p, (-0.5, -0.72), (0.72, 0.0), (-0.5, 0.72)) - 0.06,
        Icon::Pause => rbox(p, (-0.34, 0.0), (0.17, 0.68), 0.07).min(rbox(p, (0.34, 0.0), (0.17, 0.68), 0.07)),
        Icon::Subtitles => {
            let frame = rbox(p, (0.0, 0.0), (0.86, 0.64), 0.2).abs() - T;
            let lines = seg(p, (-0.48, 0.24), (0.02, 0.24), T)
                .min(seg(p, (0.26, 0.24), (0.48, 0.24), T))
                .min(seg(p, (-0.48, -0.1), (-0.22, -0.1), T))
                .min(seg(p, (0.02, -0.1), (0.48, -0.1), T));
            frame.min(lines)
        }
        Icon::Audio => {
            let body = rbox(p, (-0.62, 0.0), (0.16, 0.24), 0.05)
                .min(tri(p, (-0.56, -0.24), (0.0, -0.68), (0.0, 0.68)) - 0.03)
                .min(tri(p, (-0.56, -0.24), (0.0, 0.68), (-0.56, 0.24)) - 0.03);
            let wedge = |r: f32| ring(p, (0.0, 0.0), r, T * 0.9).max(y.abs() - x * 1.1);
            body.min(wedge(0.42)).min(wedge(0.76))
        }
        Icon::Chapters => [-0.52f32, 0.0, 0.52]
            .iter()
            .map(|&ly| circle(p, (-0.64, ly), 0.12).min(seg(p, (-0.3, ly), (0.74, ly), T)))
            .fold(f32::MAX, f32::min),
        Icon::Options => [(-0.52f32, -0.3f32), (0.0, 0.34), (0.52, -0.08)]
            .iter()
            .map(|&(ly, kx)| seg(p, (-0.76, ly), (0.76, ly), T * 0.75).min(circle(p, (kx, ly), 0.17)))
            .fold(f32::MAX, f32::min),
        Icon::Info => ring(p, (0.0, 0.0), 0.8, T).min(circle(p, (0.0, -0.38), 0.11)).min(seg(
            p,
            (0.0, -0.08),
            (0.0, 0.44),
            T * 1.1,
        )),
        Icon::Restart => {
            seg(p, (-0.6, -0.6), (-0.6, 0.6), T * 1.1).min(tri(p, (0.66, -0.62), (-0.34, 0.0), (0.66, 0.62)) - 0.05)
        }
        Icon::Next => {
            seg(p, (0.6, -0.6), (0.6, 0.6), T * 1.1).min(tri(p, (-0.66, -0.62), (0.34, 0.0), (-0.66, 0.62)) - 0.05)
        }
        Icon::Library => {
            let sq = |c: P| rbox(p, c, (0.3, 0.3), 0.1);
            sq((-0.38, -0.38))
                .min(sq((0.38, -0.38)))
                .min(sq((-0.38, 0.38)))
                .min(sq((0.38, 0.38)))
        }
        Icon::Check => seg(p, (-0.62, 0.02), (-0.18, 0.46), T * 1.3).min(seg(p, (-0.18, 0.46), (0.66, -0.44), T * 1.3)),
        Icon::Tabs => {
            let back = rbox(p, (0.0, 0.16), (0.84, 0.56), 0.16).abs() - T;
            back.min(rbox(p, (-0.42, -0.5), (0.42, 0.16), 0.1))
        }
        Icon::Server => {
            let unit =
                |cy: f32| (rbox(p, (0.0, cy), (0.82, 0.3), 0.14).abs() - T * 0.9).min(circle(p, (-0.46, cy), 0.09));
            unit(-0.4).min(unit(0.4))
        }
        Icon::Tv => (rbox(p, (0.0, -0.12), (0.86, 0.56), 0.14).abs() - T).min(seg(p, (-0.36, 0.78), (0.36, 0.78), T)),
        Icon::Video => (rbox(p, (0.0, 0.0), (0.86, 0.62), 0.22).abs() - T)
            .min(tri(p, (-0.2, -0.3), (0.34, 0.0), (-0.2, 0.3)) - 0.04),
        Icon::Lock => {
            let shackle = ring(p, (0.0, -0.3), 0.36, T)
                .max(y + 0.3)
                .min(seg(p, (-0.36, -0.3), (-0.36, 0.0), T))
                .min(seg(p, (0.36, -0.3), (0.36, 0.0), T));
            let body = rbox(p, (0.0, 0.3), (0.62, 0.46), 0.14).max(-circle(p, (0.0, 0.26), 0.12));
            shackle.min(body)
        }
        Icon::Person => {
            let shoulders = rbox(p, (0.0, 0.62), (0.66, 0.36), 0.34).max(y - 0.8);
            circle(p, (0.0, -0.3), 0.34).min(shoulders.max(-circle(p, (0.0, -0.3), 0.46)))
        }
        Icon::Power => {
            let arc = ring(p, (0.0, 0.08), 0.66, T).max(-(x.abs() - 0.3).max(y + 0.1));
            arc.min(seg(p, (0.0, -0.82), (0.0, -0.06), T))
        }
    }
}

/// How much of the pixel at `(px, py)` of an icon `size` pixels wide the shape covers (0 to 1).
pub fn coverage(icon: Icon, px: f32, py: f32, size: f32) -> f32 {
    let unit = 2.0 / size;
    let p = (px * unit - 1.0, py * unit - 1.0);
    (0.5 - shape(icon, p) / unit).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_ink_and_space_around_it() {
        let n = SIZE as usize;
        for icon in ALL {
            let at = |x: usize, y: usize| coverage(icon, x as f32 + 0.5, y as f32 + 0.5, SIZE as f32);
            let ink: f32 = (0..n * n).map(|i| at(i % n, i / n)).sum();
            let share = ink / (n * n) as f32;
            assert!((0.08..0.75).contains(&share), "{icon:?} covers {share}");
            let edge: f32 = (0..n).map(|i| at(i, 0) + at(0, i) + at(i, n - 1) + at(n - 1, i)).sum();
            assert!(edge < 1.0, "{icon:?} touches the edge");
        }
        assert_eq!(coverage(Icon::Play, 32.0, 32.0, 64.0), 1.0);
        assert_eq!(coverage(Icon::Info, 32.0, 32.0 + 8.0, 64.0), 1.0, "the stem of the i");
    }
}
