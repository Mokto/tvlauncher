# Compatibility

A check applies only to the build and TV listed. **Hardware** means tested on that TV. **Simulator only** means the result doesn’t cover LG’s SDL, real remote events, app launches, picture or sound. **Untested** means there’s no hardware result for that build. See the [historical notes](guide.md#verified) for earlier development builds.

| Build | TV / firmware | Home, D-pad, app and HDMI launches | Film playback and fallback | Search and diagnostics | Rollback |
| --- | --- | --- | --- | --- | --- |
| `v0.4.0` | LG C1 / 03.53.45 | Untested on the tagged build | Untested on the tagged build | Not in release | Untested on the tagged build |
| Next release (development) | LG C1 / 03.53.45 | Untested | Simulator only | Simulator only | Untested |
| Any build | Other models or firmware | Untested | Untested | Untested | Untested |

Dolby Vision, picture subtitles and eARC need separate hardware checks. Receiver results from Native Jelly are not hardware results for this launcher.

## C1 smoke test

Start with **Start as Home** off and keep an SSH session open. Note the release tag or commit, plus the model and firmware from **Settings → This TV → Diagnostics** (or LG’s TV Information on older releases).

1. **Home and navigation:** Activate the launcher for this boot. Press Home on the physical remote, navigate with the D-pad, tap OK, and hold OK on an app to check that its menu opens without launching it.
2. **Apps and HDMI:** Open an installed app and return with Home. Switch to a connected HDMI input and return with Home. Check that focus is usable after each return.
3. **Playback:** Play a known-good H.264/AAC title. Check picture and sound, pause, seek and return to Home. Open **Info** on a title playing directly, then on a converted title. Check **Why** for both a forced server conversion (**Direct Play → Off**) and an automatic fallback. Restore the playback setting when done.
4. **Search and diagnostics:** Warm the caches, then try Search with the server offline. Check the source labels and Back navigation. If two profiles are configured, confirm they show only their own results. Check that Diagnostics shows the model and firmware and that an unsuccessful refresh doesn’t change the last completed time.
5. **Rollback:** Run `ssh root@<tv-ip> sh /media/developer/tvlauncher/takeover.sh off` and press Home. LG’s Home should return. If testing boot takeover separately, disable **Start as Home** before finishing. See [installation and rollback](../README.md#install-on-your-tv).

Report the outcome of each step separately. For playback, include the video and audio formats, and the receiver’s reported input format if testing eARC. Don’t include server addresses, credentials or raw logs.
