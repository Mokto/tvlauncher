use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The TV's SDL2 libraries are in the NDK sysroot, which the NDK gcc used as the linker searches by itself.
    if std::env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "arm" {
        for l in ["SDL2", "SDL2_image", "SDL2_ttf"] {
            println!("cargo:rustc-link-lib={l}");
        }
        return;
    }
    let out = Command::new("pkg-config")
        .args(["--libs", "sdl2", "SDL2_image", "SDL2_ttf"])
        .output()
        .expect("pkg-config is needed for the desktop simulator (brew install sdl2 sdl2_image sdl2_ttf)");
    assert!(
        out.status.success(),
        "pkg-config could not find sdl2, SDL2_image and SDL2_ttf"
    );
    for a in String::from_utf8_lossy(&out.stdout).split_whitespace() {
        if let Some(d) = a.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={d}");
        } else if let Some(l) = a.strip_prefix("-l").filter(|l| *l != "SDL2main") {
            println!("cargo:rustc-link-lib={l}");
        }
    }
}
