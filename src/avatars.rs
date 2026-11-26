use crate::gfx::{rgb, Gfx, H, W, WHITE};
use crate::sdl::{FRect, Opaque, Rect, Surf, Tex};

const ATLAS: &[u8] = include_bytes!("avatars/midnight-matinee.png");
const SIZE: i32 = 160;
const COLS: i32 = 7;

// Profile files store indexes into this list. Keep the names and atlas cells in the same order.
pub const NAMES: [&str; 38] = [
    "Bear",
    "Fox",
    "Owl",
    "Frog",
    "Robot",
    "Space explorer",
    "Elephant",
    "Panda",
    "Axolotl",
    "Cat",
    "T. rex",
    "Capybara",
    "Penguin",
    "Red panda",
    "Quokka",
    "Otter",
    "Bat",
    "Bumblebee",
    "Shark",
    "Hedgehog",
    "Octopus",
    "Chameleon",
    "Snow leopard",
    "Rabbit",
    "Llama",
    "Gecko",
    "Tarantula",
    "Sloth",
    "Crow",
    "Hermit crab",
    "Narwhal",
    "Goat",
    "Puffin",
    "Duck",
    "Turtle",
    "Moth",
    "Koala",
    "Crocodile",
];

pub fn backdrop(g: &Gfx) {
    g.fill(
        rgb(35, 32, 30),
        1.0,
        FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: H,
        },
    );
    for (color, x, y, w, h) in [
        (rgb(84, 83, 89), -1160.0, -160.0, 3340.0, 2220.0),
        (rgb(103, 70, 44), 275.0, 195.0, 2690.0, 1890.0),
        (rgb(101, 92, 75), -1110.0, -1020.0, 2300.0, 1920.0),
    ] {
        g.tint(&g.soft, color, 0.85);
        g.copy(&g.soft, None, FRect { x, y, w, h });
    }
}

fn load(ren: *mut Opaque) -> Option<Tex> {
    let surface = Surf::from_bytes(ATLAS)?;
    let rows = NAMES.len().div_ceil(COLS as usize) as i32;
    (surface.size() == (SIZE * COLS, SIZE * rows))
        .then(|| Tex::new(ren, &surface))
        .flatten()
}

pub fn draw(g: &Gfx, avatar: usize, x: f32, y: f32, size: f32) {
    let dst = FRect { x, y, w: size, h: size };
    let Some(atlas) = g.avatar_atlas.get_or_init(|| {
        let atlas = load(g.ren);
        if atlas.is_none() {
            crate::sdl::log("Could not load the profile picture atlas");
        }
        atlas
    }) else {
        g.tint(&g.circle, WHITE, 0.4);
        g.copy(&g.circle, None, dst);
        return;
    };
    let id = (avatar % NAMES.len()) as i32;
    let src = Rect {
        x: id % COLS * SIZE,
        y: id / COLS * SIZE,
        w: SIZE,
        h: SIZE,
    };
    g.copy(atlas, Some(src), dst);
}
