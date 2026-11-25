# tvlauncher

A native Rust + SDL2 Home screen for rooted LG TVs (webOS 6). The [README](README.md) describes what it does and how to install it; this file holds what a change needs to get right.

## This repo is public

- Keep personal data out of every commit: LAN IPs, hostnames, tokens, Jellyfin/Plex/YouTube content, real posters or thumbnails. `jellyfin.json`, `youtube.json`, `plex.json`, `fixtures/`, `build/`, `target/` and `dist/` must stay untracked.
- Screenshots in the repo come from generated artwork. Re-render `docs/home.png` with `rm -rf /tmp/demo && cp -R fixtures /tmp/demo && rm -f /tmp/demo/media/developer/tvlauncher/state.json && python3 tools/demo-fixtures.py /tmp/demo && LAUNCHER_ROOT=/tmp/demo target/release/tvlauncher --shot build/demo.bmp "R"`, then `sips -s format png --resampleWidth 1600 build/demo.bmp --out docs/home.png`. Don't commit LG, Netflix or other vendors' artwork.
- Before pushing, run `git grep -nI "192\.168\|/Users/" -- . ':!Cargo.lock'`. The README's example IP `192.168.1.10` is intended.

## Build and check

- Run `export PATH=$HOME/.cargo/bin:$PATH` first: Homebrew's `cargo` shadows rustup's, so `cargo +nightly` fails. Never set `RUSTFLAGS` (it replaces the flags in `.cargo/config.toml`, and the TV binary then crashes with SIGILL).
- Before a commit, run `cargo fmt --check`, `cargo clippy --release --all-targets -- -D warnings` and `cargo test --release`. CI runs the same, plus `shellcheck` on the `.sh` files and an ARM build with a check that the binary has no `p15` instruction.
- `takeover.sh`, `boot.sh`, `debloat.sh` and `install.sh` must stay plain `sh` that busybox runs (no `sleep 0.5`, no bashisms). Check with `sh -n`.
- `make shot KEYS=…` renders the simulator to `build/shot.png`; `make qemu` runs the ARM binary under QEMU. Neither uses LG's SDL, so they can't show key codes, the Home key, the cursor or real app launches. Say so when you report a UI change as checked, and test those on the TV.

## The TV

- Deploy with `make deploy off on` (the bind mount pins the old binary, so `off` then `on` is needed after every rebuild), then `ssh lg-c1 'head -5 /tmp/tvlauncher.log'`. Ask before you run anything that changes the TV beyond `/media/developer/tvlauncher`, such as turning debloat switches on or uninstalling an app.
- Recovery if the launcher hangs: `ssh lg-c1 sh /media/developer/tvlauncher/takeover.sh off`, or reboot. A crash is handled by the watcher, a hang isn't.
- Over SSH, `luna-send` returns nothing unless stdin is redirected: always end with `</dev/null`.

## Decisions to keep

- No TLS library in the binary. HTTPS goes through the TV's `curl` (`src/net.rs`), with the config on stdin so tokens stay out of the process list.
- The Magic Remote pointer stays off: no cursor, no hover, and a click counts as OK.
- Opening an app's menu is a hold of OK (`HOLD_MS`), not a colour button.
- The launcher never wakes a media server: rows show the cache and the refresh times out.
- Start at boot is an option that is off by default, and debloat switches stay off by default.
- Native Jelly is `com.sostk.nativejelly.nightly` and PlxNative `com.beb.plxnative`; both take the item through launch parameters (see `Item.params` in `src/data.rs`).
- The film player (`--film`, `src/player/vod.rs`, `src/ff/`) is a process of its own, so a crash or a leak in it never reaches Home. FFmpeg is three separate shared libraries in `lib/`, LGPL, built by `tools/build-ffmpeg.sh` from the unmodified release tarball (a release attaches the tarball): never link them statically or patch FFmpeg, and keep the component list short (it sets the size). They are bound with `dlopen`, and the structure offsets in `src/ff/mod.rs` are for FFmpeg 9.0.2 only: a version bump means re-deriving them. libass (`src/ff/ass.rs`, built by `tools/build-libass.sh`) is `dlopen`ed like FFmpeg and holds FreeType, HarfBuzz and FriBidi statically: keep the export list to `ass_*`, check the build with `readelf --dyn-syms` and that only libc and libm are NEEDED, and attach `libass-sources.tar.xz` to a release. Lossless audio (`src/ff/lossless.rs`) exports `ioctl` from the executable (`--export-dynamic-symbol=ioctl` in `.cargo/config.toml`) to swap LG's ES renderer writes: keep the export and check `readelf --dyn-syms` after a linker change. `vendor/earc_route` is a GPL-2.0-only kernel module that stays a separate program (never linked, source shipped with the package). What the TV cannot play as it is goes to the server's HLS (the same reader as Live TV); never transcode on the TV beyond the AC-3 re-encode.
- Check the film player without a TV with `sh tools/film-sim.sh` (fake Jellyfin and fake player library) and `tvlauncher --direct <url>`; say that LG's decoder, Dolby Vision, picture subtitles and the Home key during a film can only be checked on the TV.
- Don't claim speed against other launchers, or that this is the first native launcher (Luma is also Rust and SDL2). Claim only what was measured.

## Commits and releases

- Use Conventional Commits. `feat:` and `fix:` make release-please open a release PR (it bumps `Cargo.toml` and `CHANGELOG.md`); `docs:`, `ci:` and `chore:` don't. Merging the release PR tags `vX.Y.Z` and the called `build.yml` attaches the ARM package.
- Don't edit `Cargo.toml`'s version or `.release-please-manifest.json` by hand.
- Open a PR for features and fixes. The `Mokto` account on github.com owns this repo.
- When a change touches behaviour, update the README section for it in the same commit.
