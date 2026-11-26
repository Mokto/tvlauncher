use crate::gfx::{rgb, Gfx};
use crate::sdl::{Color, FRect};

pub const NAMES: [&str; crate::profiles::AVATAR_COUNT] = ["Bear", "Fox", "Owl", "Frog", "Robot", "Space explorer"];

const PALETTE: [(Color, Color, Color); NAMES.len()] = [
    (rgb(51, 89, 127), rgb(230, 165, 116), rgb(255, 220, 180)),
    (rgb(93, 59, 133), rgb(250, 140, 84), rgb(255, 224, 178)),
    (rgb(38, 97, 106), rgb(156, 198, 227), rgb(223, 237, 245)),
    (rgb(35, 111, 82), rgb(159, 217, 119), rgb(221, 241, 163)),
    (rgb(134, 65, 96), rgb(177, 207, 228), rgb(231, 245, 252)),
    (rgb(37, 56, 110), rgb(234, 239, 252), rgb(170, 216, 253)),
];

pub fn accent(avatar: usize) -> Color {
    PALETTE[avatar % PALETTE.len()].0
}

pub fn draw(g: &Gfx, avatar: usize, x: f32, y: f32, size: f32) {
    let avatar = avatar % PALETTE.len();
    let (bg, main, light) = PALETTE[avatar];
    let ink = rgb(24, 36, 52);
    let dot = |cx: f32, cy: f32, w: f32, h: f32, c: Color| {
        g.tint(&g.circle, c, 1.0);
        g.copy(
            &g.circle,
            None,
            FRect {
                x: x + (cx - w / 2.0) * size,
                y: y + (cy - h / 2.0) * size,
                w: w * size,
                h: h * size,
            },
        );
    };
    let round = |cx: f32, cy: f32, w: f32, h: f32, r: f32, c: Color| {
        g.rounded(
            x + (cx - w / 2.0) * size,
            y + (cy - h / 2.0) * size,
            w * size,
            h * size,
            r * size,
            c,
            1.0,
        );
    };
    g.rounded(x, y, size, size, size * 0.16, bg, 1.0);
    dot(
        0.5,
        0.5,
        0.79,
        0.79,
        rgb(
            bg.r.saturating_add(17),
            bg.g.saturating_add(17),
            bg.b.saturating_add(17),
        ),
    );

    match avatar {
        0 => {
            for cx in [0.30, 0.70] {
                dot(cx, 0.32, 0.24, 0.23, main);
                dot(cx, 0.32, 0.13, 0.13, light);
            }
            dot(0.5, 0.53, 0.59, 0.52, main);
            dot(0.5, 0.63, 0.30, 0.20, light);
            dot(0.5, 0.61, 0.10, 0.065, ink);
        }
        1 => {
            for cx in [0.30, 0.70] {
                round(cx, 0.38, 0.19, 0.30, 0.084, main);
                dot(cx, 0.32, 0.095, 0.14, light);
            }
            dot(0.5, 0.53, 0.62, 0.46, main);
            dot(0.38, 0.61, 0.30, 0.19, light);
            dot(0.62, 0.61, 0.30, 0.19, light);
            dot(0.50, 0.65, 0.10, 0.07, ink);
        }
        2 => {
            round(0.5, 0.51, 0.59, 0.58, 0.23, main);
            dot(0.37, 0.49, 0.27, 0.26, light);
            dot(0.63, 0.49, 0.27, 0.26, light);
            dot(0.5, 0.625, 0.10, 0.13, rgb(255, 183, 87));
        }
        3 => {
            dot(0.5, 0.58, 0.65, 0.42, main);
            for cx in [0.36, 0.64] {
                dot(cx, 0.44, 0.26, 0.25, main);
                dot(cx, 0.46, 0.15, 0.16, light);
            }
            round(0.5, 0.64, 0.17, 0.038, 0.019, ink);
        }
        4 => {
            round(0.5, 0.3, 0.047, 0.15, 0.02, light);
            dot(0.5, 0.25, 0.105, 0.105, rgb(255, 192, 124));
            round(0.5, 0.55, 0.64, 0.21, 0.07, light);
            round(0.5, 0.55, 0.53, 0.43, 0.10, main);
            round(0.5, 0.49, 0.41, 0.16, 0.075, ink);
            for cx in [0.36, 0.64] {
                dot(cx, 0.49, 0.07, 0.07, rgb(132, 250, 220));
            }
            round(0.5, 0.66, 0.14, 0.035, 0.016, ink);
            return;
        }
        _ => {
            dot(0.23, 0.32, 0.055, 0.055, light);
            dot(0.79, 0.35, 0.045, 0.045, light);
            round(0.5, 0.72, 0.39, 0.14, 0.06, main);
            dot(0.5, 0.50, 0.58, 0.54, main);
            round(0.5, 0.49, 0.45, 0.31, 0.14, ink);
            round(0.45, 0.44, 0.21, 0.04, 0.02, light);
            return;
        }
    }
    let eye_y = if avatar == 3 { 0.46 } else { 0.49 };
    for cx in [0.36, 0.64] {
        dot(cx, eye_y, 0.07, 0.085, ink);
        dot(cx - 0.01, eye_y - 0.015, 0.022, 0.022, light);
    }
}
