---
name: tvlauncher-screenshots
description: Rebuild or extend tvlauncher's README simulator gallery. Use when asked to refresh screenshots or snapshots, or to show a new launcher or player screen in the README.
---

# tvlauncher screenshots

1. Follow the [screenshot recipe](../../../docs/guide.md#render-the-readme-screenshots) in the checkout that contains the UI change. Use [the demo generator](../../../tools/demo-fixtures.py), not `make shot`: `make shot` uses TV fixtures, while the demo generator builds an isolated root with fictional account and watch data and generated artwork.
2. For a new screen, extend `populate` only if more demo data is needed. Add the navigation keys to `shots` for launcher screens, or use the existing `--film-shot` path for player overlays. Link the image from the [README](../../../README.md) with alt text that describes the screen.
3. Open each affected image. A successful render can still land on the wrong tab or have the wrong item focused. Keep only the intended PNG updates: rerendering can change clocks and programme times even when the UI is unchanged. Do not undo any pre-existing edits.
4. Report these as simulator screenshots, not hardware verification of remote input, app launches, decoding or receiver output.
