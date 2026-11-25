# tvlauncher

**An Android TV-style Home screen for rooted LG TVs. Native, not another web wrapper.**

Most custom launchers for webOS are web pages in LG's browser engine. tvlauncher is a single native program, written in Rust, that draws straight onto the TV's own SDL2 and OpenGL ES libraries. There is no browser, no Node.js and no web view in the path: the whole thing is a binary of about 1.8 MB (plus 1.5 MB of FFmpeg libraries for the film player) that uses about 40 MB of memory on Home. It starts as Home in a fraction of a second, and it has no page to load or script engine to warm up.

![Home screen: a hero with title and description, a Continue watching row and a Recommended on YouTube row, all made-up demo content](docs/home.png)

*A simulator render with generated demo artwork (`tools/demo-fixtures.py`), so it shows no one's posters or thumbnails.*

- **The Android TV layout:** a big hero with the focused item's title, description and backdrop, and rows of cards under it: Pinned apps, Continue watching, Continue watching on Plex, Recommended on YouTube, Apps, Inputs.
- **Movement that feels native:** focus, scrolling and background changes are animated frame by frame on the GPU, with smooth easing and crossfades. Moving around feels like the TV's own interface, with no page redraws, layout reflows or script pauses to cause stutter.
- **Your library on Home:** *Continue watching* from your own Jellyfin server, signed in on the TV with Quick Connect (nothing is typed with the remote), and a changing backdrop with title and synopsis behind everything else.
- **Continue watching on Plex,** signed in with a code you enter at plex.tv/link. PlxNative opens the title itself.
- **Recommended on YouTube** (unofficial, opt-in), with the YouTube app doing the playback.
- **Jellyfin and Plex library tabs** (*Movies*, *Shows*, …): rows of posters by genre, recently added and unwatched, under the focused title's backdrop and details, opening the title in Native Jelly or PlxNative.
- **Live TV from your own M3U/Xtream source,** in a Live TV tab: a programme guide grid with programme details, Favourites, and a built-in player on the TV's own hardware decoder.
- **Your apps, your way:** pin, hide, reorder and uninstall from the options menu, opened by holding OK as on LG's own Home.
- **Optional blocking of LG's ads and telemetry,** four switches, all off by default.
- **Safe by design:** it changes nothing permanent until you choose *Start as Home* (a reboot puts LG's Home back before that), and three crashes in a minute hand Home back to LG's.

Built and tried on an LG C1 (webOS 6.5.3). It is early software: expect rough edges, and read [Install on your TV](#install-on-your-tv) before you start. GPL-3.0-or-later ([LICENSE](LICENSE)).

### How it is built

Rust, with `serde_json`, `libc` and `minreq` as the only dependencies. [src/sdl.rs](src/sdl.rs) declares the SDL calls by hand because LG's SDL fork shifts the key symbol in `SDL_KeyboardEvent`, which a stock-SDL binding would read wrongly. Everything is drawn by the app: cards, text, focus animations, the on-screen keyboard and the settings page. HTTPS goes through the TV's own `curl`, so there is no TLS stack in the binary. The details are in the sections below.

It shows a hero area with the focused card's name, description and background image, and up to six rows:

- **Pinned apps**: the apps you pinned (hold OK on an app, *Pin to Home*), in the order you give them. A pinned app is shown here instead of in Apps. The row is absent until something is pinned.
- **Continue watching**: Jellyfin's resume and next-up items, with a picture and a progress bar. See [Jellyfin](#jellyfin).
- **Continue watching on Plex**: the Continue Watching hub of your Plex servers. See [Plex](#plex).
- **Recommended on YouTube**: see [YouTube](#youtube-recommendations).
- **Apps**, from `listLaunchPoints`. All the other apps. Hold OK on one to pin, hide or reorder it. See [Hiding and moving apps](#hiding-and-moving-apps).
- **Inputs**, from `getAllInputStatus`.

App icons are flat-colour squares with a logo in the middle, so the launcher finds the logo's box, gives the card the icon's colour and draws the logo large (`logo_box` in `src/gfx.rs`); an icon that isn't a flat colour keeps the old placement.

**Uninstalling:** the hold-OK menu has *Uninstall* for apps LG marks removable (`removable` in `listLaunchPoints`), then asks to confirm, with Cancel preselected. It calls `luna://com.webos.appInstallService/remove`, drops the card at once and reads the app list again when the app is gone from it (or after 60 s). CleanYourTV (the Home that comes back when this launcher is off) and the Homebrew Channel (keeps the TV rooted) are never offered. The call was checked with an id that doesn't exist; no real app has been removed through the launcher yet.

The rows are in that order: Pinned apps, Continue watching, YouTube, Apps, Inputs. As on Android TV, the two rows of videos have large cards (384×216) and the apps and inputs small ones (256×144).


The cog at the top right (next to the clock) opens **Settings**, a page of its own: categories on the left (*Jellyfin*, *YouTube*, *Ads and privacy*) and the entries of the selected one on the right, each with a description and either its value or a switch (Up and Down browse, Right or OK enters a category, Left goes back to the list, Back closes the page). An account entry opens a small menu or the sign-in dialog, and closing either returns to the page. See [Jellyfin](#jellyfin), [YouTube](#youtube-recommendations) and the debloat switches below. The D-pad moves the focus; OK launches. The Magic Remote's pointer is switched off: the cursor is hidden, moving it does nothing, and its click counts as OK. Opening a card saves its key to `/tmp/tvlauncher-focus` so the next Home start focuses it again.

## Jellyfin

Sign in on the TV, from the **cog** at the top right of Home (press Up from the top row, then OK). Nothing is set up from the Mac.

1. In Settings, Jellyfin, open **Account**. The launcher looks for a server with Jellyfin's UDP broadcast (port 7359), trying the address used last time first.
2. If none answers (a server behind a port proxy, as in WSL or Docker, does not get the broadcast), an on-screen keyboard asks for the address. Type a host or IP (`192.168.1.10`; the port is 8096 unless you add `:port`) or a full URL, with the **https://** and **http://** keys to set the scheme. A bare host is tried as `http://host:8096` and then as `https://host`, so `jellyfin.example.com` behind a reverse proxy just works. The remote's number keys and a keyboard's letters work too. **HTTPS goes through the TV's `curl`** (this binary has no TLS), so the server's certificate must be trusted by the TV: a self-signed one is refused.
3. The dialog shows a **Quick Connect** code. On a phone or computer, open a Jellyfin app that is signed in, go to Settings, then Quick Connect, and enter it. Quick Connect must be on in the server (Dashboard, General).
4. The TV then receives its own device login (named `tvlauncher`) and the **Continue watching** row appears. Back closes the dialog at any step.

- The login is saved in `/media/developer/tvlauncher/jellyfin.json` (mode 600: server, user and device token); the last server address is kept in `state.json`. The **Account** entry in Settings offers *Refresh now* and *Sign out*, which revokes the token on the server and deletes the file and the cache. The device id is fixed, so signing in again replaces the earlier login of this TV.
- **Open videos in** in Settings picks the app that opens an item: it lists the installed apps whose id or name contains "jelly", "moonfin" or "moonfish" (Native Jelly in any build, Moonfin, the official app). The default is `com.sostk.nativejelly.nightly`. The choice is kept in `state.json`.
- **Continue watching**, a switch under Settings → *Jellyfin*, hides the row from Home (on by default); the library tabs and the background pictures stay. It is kept in `state.json` (`rowsOff`).
- **Home background:** when the focused card has no picture of its own (an app, an input), the background is a random movie or series backdrop from the signed-in user's library, as on Android TV. The refresh asks Jellyfin for 12 random ones (`SortBy=Random`, only items with a backdrop), saves them as `cache/home-*.img` with the list in `cache/backdrops.json`, and renews the set every 12 hours. The launcher picks one at random each time Home starts or comes to the front, and every 30 seconds while one is showing (`FEATURED_MS`), never the one it just showed; it doesn't change while a menu is open. A YouTube video's picture is only 640 px wide, so as a background it is cut to its middle, averaged down to 32 × 18 pixels and darkened (`blur` in `src/gfx.rs`), which gives a soft glow of its colours instead of a blocky enlargement; the hero title is cut at about 35 characters. LG's own background images for apps are ignored, since they are inconsistent (a dull preview for YouTube, a logo on flat colour for Disney+). The hero area then says what the picture is, as on Android TV: the title of that movie or series, and its year and a short description (about 200 characters), saved with the picture in `backdrops.json`; the focused app's name is the label under its card. A set saved by an earlier version has no text and is fetched again. Cards with their own picture (Continue watching, YouTube) still crossfade to it. The picture shows through the rows at 16 % and through the left gradient at 18 %. Signed out, or with no backdrops, the background is plain dark. Sign-out deletes the pictures. Only the Jellyfin user's own libraries are used, so nothing from the kids library appears for `theo`.
- The list (Jellyfin's resume and next-up items) and its pictures are cached in `/media/developer/tvlauncher/cache` and shown at once; a background thread refreshes them on every start, on coming back to the front and on the Home key. The launcher never wakes the tower: when the server is asleep the refresh times out after 6 s and the cached row stays.
- Opening an item starts the chosen app with `{"itemId", "userId", "serverId"}` as launch parameters. Native Jelly's `feat/deep-link-launch` branch (see its `docs/deep-link.md`) turns that into the item's detail page; a build without it, or another app, just opens the app (whether Moonfin reads these parameters is unchecked). The detail page never starts playback itself, and the user must already be signed in inside that app.
- The simulator signs in the same way and writes `fixtures/media/developer/tvlauncher/jellyfin.json`, which is ignored by Git.

### Library tabs

Each film, series or collection library of the signed-in user gets a tab after Home (*Movies*, *Shows*, …); music, books, photo and live TV libraries are left out. A tab shows the focused title's backdrop behind the page, its name, year (a series' years), length, rating, age rating and genres, and the start of its overview, above rows of posters as on Google TV: *Continue watching*, *Recently added*, *Unwatched* (best rated first), a row for each of the eight largest genres, and last *All*, every title in a grid of six to a row (A to Z, or newest first). The rows come from the cached list, so they need no extra requests; a library of fewer than 24 titles is only the grid, and an empty row is left out. Left and Right move along a row (it scrolls, and keeps its place when you come back to it), Up and Down move between rows. A bar on a poster shows how far a film is watched. OK opens the title in the app from *Open videos in*: Native Jelly opens its page, with the seasons, episodes and Play, so nothing plays in the launcher itself. Holding OK offers *Sort by name* or *Sort by date added* (for all library tabs, kept in `state.json`) and *Refresh the libraries*. Back goes to the first row, then to Home.

- The libraries (`/Users/{id}/Views`) and their titles are cached as `cache/lib-views.json` and `cache/lib-<library>.json` and shown at once; the refresh at start, on coming back to the front, on the Home key and on switching to a library tab asks for new lists when they are more than 5 minutes old, so a title removed on the server is gone the next time you open the tab. The first request has the same 6 s timeout as Continue watching, so a sleeping server is never waited for; a whole library then has 60 s. Posters (300 px wide) are fetched only for the rows on screen and the one below, and the backdrop (1280 px) only for the focused title, as `cache/lib-p-*.img` and `cache/lib-b-*.img`. A refresh deletes the pictures of titles that are gone; sign-out deletes them all.
- Only the posters of the rows on screen are kept as textures, and those of a tab that isn't shown are freed: on the C1, with a library of 400 titles from a fake server, the launcher stayed at about 53 MB after four rounds of scrolling 15 rows down and back, and at about 60 MB with real posters from a 26-film library.

## Tabs

Tabs appear at the top once there is more than Home: the Jellyfin libraries, the Plex ones, then **Live TV** when live TV is set up. Up from the first row reaches them, Left and Right switch tabs (Right past the last one reaches the cog), and Back on a tab returns to its start and then to Home. Settings → **Tabs** lists them with a switch each; OK on one offers *Hide* or *Show*, *Rename*, *Move left* and *Move right*. *Rename* takes a name on the on-screen keyboard; an empty one puts the library's own name back. The order, the hidden tabs and the names given are kept in `state.json`, by library id, so renaming a library on the server keeps its place.

## Plex

A **Continue watching on Plex** row with what your Plex account is in the middle of, from its own servers.

1. Cog → **Plex** → **Account**. The dialog shows a code. On a phone or computer, open **plex.tv/link**, sign in and enter it (this is Plex's own PIN sign-in, the one Plex's TV apps use). Nothing is typed with the remote.
2. The launcher asks plex.tv for the account's servers (its own first, then shared ones, at most three), then asks each for its Continue Watching hub (`/hubs/continueWatching/items`, or `/library/onDeck` on an older server). Movies and episodes are kept, newest first, 15 at most, with a picture and a progress bar. Each server is tried on the home network over plain HTTP first, then over Plex's HTTPS names; relays are never used.
3. OK on a card (or a tap) launches the app chosen under **Open videos in** with Plex's launch link, `{"plex": {"server": "<machineIdentifier>", "ratingKey": "<key>", "action": "open"}}`, which [PlxNative documents](https://github.com/plxnative/plx-native/blob/main/docs/launch-links.md) for launchers like this. It lands on the title's page. The launcher closes the app first and then starts it, because on webOS 6 an app that is already running never reads new launch parameters. The official Plex app has no documented link, so it only starts.

- The token is saved in `/media/developer/tvlauncher/plex.json` (mode 600), with the id this TV gave itself. The list and pictures are cached (`cache/plex.json`, `cache/plex-*.img`), so the row shows at once and also when the server is asleep: like Jellyfin, the launcher never wakes it. **Sign out** deletes the login and the cache here; the token stays valid at Plex until you remove the device under *Authorized devices* in your account settings on plex.tv.
- **Open videos in** lists the installed apps whose id or name contains "plex" or "plx". The default is PlxNative (`com.beb.plxnative`) when it is installed, else any Plex app.
- **Continue watching**, a switch under Settings → *Plex*, hides the row from Home (on by default); the library tabs stay.
- HTTPS goes through the TV's `curl`, like Jellyfin and YouTube. The row is refreshed whenever Home starts or comes to the front.
- **Library tabs:** the film and show libraries of the account's own servers get tabs too, shown exactly like [Jellyfin's](#library-tabs) (shared servers are left out: they would add tabs nobody asked for and fetch whole libraries over the internet). The lists come from `/library/sections` and each section's `/all`, over the address the server answered on, and the pictures from the server's `/photo/:/transcode`. They are cached as `cache/plib-*`; `cache/plib-views.json` also holds the server's address and token for the pictures, so it is mode 600. Tabs keep the library's name; when Jellyfin and Plex both have a *Movies*, rename one in Settings → **Tabs**. OK opens the title in PlxNative with the launch link above. When a server doesn't answer, its tabs stay with the cached lists. Sign-out deletes them.

## Playing films

Settings → *Player* → **Play in the launcher** (off by default; it shows once Jellyfin or Plex is signed in) makes OK on a title, in a library tab or in Continue watching, play it with the launcher's own player instead of opening Native Jelly or PlxNative. A series plays its next episode (else its first); a collection, and anything the player cannot start, still opens in the app.

- **A process of its own.** The launcher starts itself as `tvlauncher --film <item>` and waits. That process opens its own window and LG's player, so a crash during a film takes down only the player: Home keeps running, and the player's memory is freed when the film ends. Back ends the film and brings Home back; the Home key does too (the launcher registers with the app manager the first time it plays something, so the key arrives as a message, and it stops the player first so the server hears where you stopped).
- **The file is played as it is.** The player reads the Matroska or MP4 file of the title over HTTP with the TV's `curl` (byte ranges: it needs a server that does ranges, as Jellyfin and Plex do), cuts it into H.264 or H.265 pictures and audio frames with FFmpeg, and feeds LG's decoder the same way it does for a channel. Nothing is transcoded: the server only sends the file. FFmpeg is three libraries in `lib/` (about 1.5 MB, LGPL, see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)), loaded at run time; without them every film is converted by the server, as below.
- **Audio:** AAC, AC-3 and E-AC-3 (Atmos included) go to the TV as they are. TrueHD, FLAC, ALAC, Vorbis and PCM are decoded and sent as AC-3 (5.1 at most; 7.1 is folded into 5.1), which costs the TV about 14 % of one core for TrueHD 7.1 in Native Jelly's measurement. The audio menu (Down) lists every track.
- **Subtitles:** text tracks (SRT, ASS, MOV text, WebVTT) are shown without their styling (a line placed at the top stays at the top), picture tracks (PGS from Blu-rays, DVD VobSub, DVB) are drawn as the bitmaps they are, and subtitle files beside the video (which only the server has) are fetched. ASS styling (fonts, colours, positions, karaoke) is not drawn.
- **Dolby Vision and HDR10:** a Dolby Vision profile 5 or 8 stream is declared to LG's player as Dolby Vision on a TV that reports support; HDR10 needs nothing. A profile 7 disc (a UHD Blu-ray remux) plays its HDR10 picture: the enhancement layer and the Dolby metadata are dropped.
- **What the server converts:** a file the TV cannot decode as it is (10-bit H.264, VP9 and AV1 films, DTS or Opus audio, a server without byte ranges, FFmpeg missing) is played through the server's HLS transcode, the way Live TV reads a channel. The server copies H.264 and H.265 and converts the audio to AC-3, so the cost on the server is usually small, but an AV1 or VP9 picture is a real transcode. Picking a DTS track in the audio menu moves the film to the server from there; picture subtitles are not available in that mode. The same happens if the TV's player refuses or fails on the file's stream.
- **Controls:** OK pauses and resumes; Left and Right seek by 10 seconds (the rewind and fast-forward buttons by 30); Up shows the bar; Down opens the track menu. A seek starts the player again at the new place, on the keyframe at or before it, so it takes about a second and can land a few seconds early.
- **Resume and progress:** a film resumes from its saved place (between 30 seconds in and 95 %). The server is told when it starts, every 10 seconds, on pause and when it stops (Jellyfin's session reports; Plex's timeline, with a watched mark at the end), so it shows in Continue watching.
- **Next episode:** when an episode ends, the next unwatched one of the series starts after five seconds (OK starts it at once, Back stops).
- **Not there:** chapters, skipping intros, thumbnails while seeking, passing DTS through to a sound bar (the TV cannot decode DTS, and the passthrough Native Jelly has is not built), HDR10+ metadata, 3D.
- **Checked without a TV:** `cargo test --release` covers the NAL and ADTS framing, the audio mix, the subtitle parsers, the queue, the title parsing for both servers and the stream addresses. `tvlauncher --direct <url> [seconds [audio [subtitle]]]` reads a file the way the player does and counts what comes out, which is how Matroska and MP4 files with H.264, H.265, AC-3, E-AC-3, AAC, FLAC and TrueHD audio and SRT and ASS subtitles were tried against a local server. `--film-shot out.bmp` and `--film-menu-shot` draw the controls and the track menu, and `sh tools/film-sim.sh "3:enter,5:enter,7:right,16:esc"` runs the whole player on a desktop against a fake Jellyfin and a fake player library, with those keys pressed at those seconds (pause, resume, seek, track menu, subtitles, reports). What only the TV can show, and has not been checked: that LG's decoder accepts these streams, seeking, Dolby Vision, picture subtitles, and the Home key during a film. On the TV, writing `film:<item>` (a Jellyfin id, or `plex:<server>:<rating key>`) to `/tmp/tvlauncher-spawn` starts a film from SSH, and `/tmp/tvlauncher-player.log` is the player's log.

## Hiding and moving apps

**Hold OK** on an app for about 0.7 s, as on LG's own Home (`H` in the simulator; a white bar fills along the card while you hold). A tap still launches it, on release. The menu has *Move*, *Pin to Home* or *Unpin*, and *Hide from Home*:

- **Move**: Left and Right move the card, OK drops it, Back puts it back.
- **Hide from Home**: the app leaves the row. A **Hidden apps (n)** card at the end of the row shows the hidden ones again, dimmed; holding OK on one gives **Show on Home**.

The choices are kept in `/media/developer/tvlauncher/state.json`. Apps that are not in the saved order follow in the TV's own order, so a new app lands at the end. Back is `wcode` 482 (seen on this TV). The yellow button (488) still opens the same menu, without any hint on screen. Keys held down repeat for navigation, but OK acts once; the hold is measured from the first press, and the launch waits for OK's release event, which arrives on this TV.

## YouTube recommendations

A **Recommended on YouTube** row with the home feed of YouTube's own TV app, for the account you sign in with. **Unofficial**, the way [SmartTube](https://github.com/yuliskov/SmartTube) does it (its `MediaServiceCore` is the reference): Google has no official recommendations API.

1. Cog → **Sign in to YouTube**. The launcher reads the OAuth client of YouTube's TV app from `youtube.com/tv`, asks for a device code and shows it with the page to enter it on (`google.com/device`). Enter it on a phone, with the account you want (a spare one is fine).
2. It then asks `youtubei/v1/browse` (`browseId: default`) as the `TVHTML5` client with that account's token, keeps the video tiles, and downloads their thumbnails. The list and pictures are cached in `cache/youtube.json` and `cache/yt-*.img`, so the row shows at once.
3. OK on a card opens the TV's YouTube app (`youtube.leanback.v4`) with `{"contentTarget": "v=<id>", "contentId": "v=<id>"}` (the first is what webosbrew's youtube-webos reads, the second what Home Assistant sends). Nothing is played by the launcher and no stream is fetched.

- HTTPS goes through the TV's **`curl`**, with the config on stdin so tokens never appear in the process list. If `curl` is missing or cannot verify YouTube's certificate, the row stays empty and `/tmp/tvlauncher.log` says `youtube: curl: ...`.
- The feed is asked for at most every 30 minutes, whenever Home starts or comes to the front; **Refresh now** in Settings forces it. The tokens are in `/media/developer/tvlauncher/youtube.json` (mode 600). **Sign out** revokes the refresh token at Google and deletes the file, the list and the pictures.
- The token is YouTube's TV app's own, with the power of that app on the account (not read-only). You can also revoke it at `myaccount.google.com/security`, under connections to third-party apps ("YouTube on TV").
- It can break whenever YouTube changes the client version, the sign-in or the shape of the answer. The client version is read from `youtube.com/tv` on each refresh. If the answer holds no videos, the log says `youtube: no videos in the answer (logged_in ..., error ..., keys ...)`; `logged_in "0"` means YouTube did not accept the token. This is against YouTube's terms in the way SmartTube is; the launcher only reads the feed, a few times a day.
- Tests that need no account: `cargo test --release` parses a real browse answer (`tests/tv-browse-sample.json`, tiles from YouTube's logged-out music page, with a made-up resume bar and a Short), finds the OAuth client in a script, and quotes curl's config.

## Blocking LG's ads and telemetry

Four switches under Settings, *Ads and privacy*, **off by default**: *Block ads*, *Block viewing tracking*, *Stop telemetry logs* and *Stop suggestions*. They are [`debloat.sh`](debloat.sh), ported from CleanYourTV's `debloat-stop.sh` and `debloat-undo.sh` (MIT, credited in the script), with its fixes for HBO Max and Disney+ kept.

| Switch | What it stops |
| --- | --- |
| Block ads | `adoverlay-service`, and `eic.ads.lgtvcommon.com`, `nl.ad.lgsmartad.com`, `nl.info.lgsmartad.com` (plus the TV's own country, for example `dk.ad.lgsmartad.com`) in `/etc/hosts`, marked `# tvlauncher-debloat`. `admanager` is left running: HBO Max and Disney+ wait for it at start. |
| Block viewing tracking | `acr2`, which recognises what is on screen and reports it (LivePlus). |
| Stop telemetry logs | the services `remotelogger` and `service-logger`. |
| Stop suggestions | `contentminer`, `nudge` and `sportsalarm`. |

- A program that luna starts on demand is kept from starting by a do-nothing stub bind-mounted over it; the real file is not changed. Where `/etc/hosts` is on the read-only partition, a copy is mounted over it. Everything is RAM-only, so a reboot undoes it, and nothing is uninstalled.
- It applies only while this launcher is Home: `takeover.sh on` applies the chosen switches, `takeover.sh off` (also the crash watcher) undoes them before LG's Home returns, because LG's Home needs those services. A switch takes effect at once. The chosen groups are the `debloat` list in `state.json`.
- It does not block firmware updates or claim to remove all tracking.
- `sh debloat.sh off` by hand puts everything back, and so does a reboot. `sh tools/debloat-test.sh` runs the script against fake files and commands and checks the stubs, the hosts lines, idempotence and the undo.

## Live TV

Settings → *Live TV* → *Channels* takes your own M3U playlist address, or an Xtream Codes login (server, user name, password), typed on the on-screen keyboard. The login is kept in `iptv.json` (mode 600) and only there; no playlist, channel or logo comes with the launcher. Once it is set, a **Live TV** tab appears at the top (see [Tabs](#tabs)).

- **The Live TV tab** is a programme guide: the channels down the left (Favourites first, marked with a yellow dot, then the playlist's order, with their number and logo) and two hours of programmes across, with a line at the current time. Above the grid, the focused programme: title, times, channel, category, episode title and the start of its description. Left and Right move through a channel's programmes (the grid scrolls through the day), Up and Down change channel near the same time, and Back returns to now before it returns to Home. OK on a programme that is on plays the channel; OK on a later one opens its details (the whole description, OK to watch the channel, Back to close). Holding OK offers *Programme details* and *Add to Favourites* or *Remove from Favourites* (kept in `state.json`). Channels the guide does not know show *No programme information* and still play.
- **Watching:** full screen, with a banner of the channel and its programmes for a few seconds. Up and Down change channel through the whole list, OK shows the banner again, Back returns to the guide. A channel that does not play says why on the banner.
- **The guide:** an XMLTV file, from Settings → *Live TV* → *Guide*, else the playlist's `url-tvg`/`x-tvg-url`, else the Xtream server's `xmltv.php`. It is read as it downloads (a `.gz` one through the TV's `gzip`), and only the next 24 hours of the playlist's channels are kept (at most 48 programmes each, descriptions cut at 600 characters), so a guide of a hundred megabytes never sits in memory. Channels are matched on `tvg-id` (or Xtream's `epg_channel_id`) without a feed suffix, so iptv-org's `TF1.fr@SD` finds `TF1.fr` in a guide such as xmltvfr.fr's.
- **Caching:** the channel list (`cache/iptv-channels.json`, fetched again after 12 hours or with *Refresh now*), the guide (`cache/iptv-guide.json`, after 2 hours) and the logos of the channels on screen (`cache/iptv-logo-*`) are kept, so the tab shows at once. *Remove* deletes `iptv.json` and that cache.

**The player** ([src/player.rs](src/player.rs)) is built in, not handed to another app. The TV's `curl` reads the stream, [src/ts.rs](src/ts.rs) cuts the MPEG-TS into pictures and audio frames, and they are fed to LG's StarfishMediaAPIs (`libplayerAPIs`) in buffer mode, as Native Jelly's player does. The picture is in an SDL exported window under the launcher's transparent one (webOS 5 and later). What it took on the C1:

- LG's library registers bus names that Home's role does not allow, and calls `exit()` when refused. `takeover.sh on` bind-mounts a copy of Home's role (`/usr/share/ls2/roles/{pub,prv}/com.webos.app.home.json`) that adds them (the names in Native Jelly's role) and runs `ls-control scan-services`; `off` unmounts it. RAM-only like the other mounts.
- The media resource manager never gives a decoder to a pipeline whose `appId` is `com.webos.app.home` (`601 Resource Allocation Error` after 20 s), so the pipeline carries `tvlauncher`. The video output then shows it only once `luna://com.webos.media/notifyAppLifeStatus` says that app is in the foreground.
- Each channel gets a new player object and exported window, and the old object is destroyed after its `UNLOADCOMPLETED`: the video output never shows a second pipeline of the same object.
- Play is called once 0.7 s is buffered, and the feed stays at most 1.6 s ahead of the clock; fed greedily, the pipeline stops pulling and never starts.

**What plays:** MPEG-TS over HTTP or HTTPS (an Xtream `.ts` address, most `/live/` playlist entries), and live HLS (`.m3u8`) whose segments are MPEG-TS, with H.264 or H.265 video and AAC, AC-3 or E-AC-3 audio. For HLS ([src/hls.rs](src/hls.rs)) the player takes the variant with the most bandwidth, starts three segments before the live edge and reads each new segment into the same demuxer. Streams and playlists are fetched with a browser's User-Agent (some channels refuse others), and relative addresses are resolved against the address after redirects. **Not yet:** encrypted HLS (Pluto TV's AES-128 segments, which also bring audio in a playlist of its own), HLS with MP4 segments or separate audio, switching variants on a slow network, and MPEG-1 Layer II audio, which many broadcast channels use (such a channel plays without sound, or not at all). Films and series in an Xtream playlist are left out.

**Memory:** about 40 MB on Home with a playlist of 21 channels; once a channel has played, LG's player libraries stay loaded, about 83 MB, and about 95 MB while a channel plays, steady over 30 channel changes.

**Testing:** `LAUNCHER_ROOT=/tmp target/release/tvlauncher --play <url>` reads ten seconds of a stream on a desktop and prints the codecs and what the TV would be fed. On the TV, a channel number or a stream address in `/tmp/tvlauncher-play` makes the next start of the launcher play it, and `luna://com.webos.service.networkinput/sendSpecialKey '{"key":"UP"}'` presses a remote key (also `DOWN`, `LEFT`, `RIGHT`, `ENTER`, `BACK`). `luna://com.webos.service.capture/executeOneShot` captures the screen with the video.

## Why it runs as LG's Home

A sideloaded native app runs jailed as `prisoner`. The jail has no `/usr/palm/applications`, and luna refuses `listApps` there (`Denied method call "listApps"`). So `takeover.sh on` bind-mounts the binary over `/usr/bin/com.webos.app.home`, and the app manager starts it as Home, as root, outside the jail. The mount is RAM-only: a reboot brings back what was Home before (CleanYourTV's stand-in on this TV).

## Install on your TV

You need a rooted LG with the Homebrew Channel (this was built and tried on a C1 with webOS 6.5.3, nothing else) and SSH access to it as root with a key. No toolchain is needed:

1. Download `tvlauncher-<version>-webos-arm.tar.gz` from the [Releases](https://github.com/Mokto/tvlauncher/releases) page and unpack it.
2. Run `./install.sh root@<tv-ip> on`. It copies the files to `/media/developer/tvlauncher` and makes the launcher Home until the next reboot. Without `on` it only installs; run it again, or `ssh root@<tv-ip> sh /media/developer/tvlauncher/takeover.sh on`, to start. Running it again later updates and restarts.
3. To undo before a reboot: `ssh root@<tv-ip> sh /media/developer/tvlauncher/takeover.sh off`. A reboot also brings LG's Home back.
4. Happy with it? Open **Settings → Startup → Start as Home** (the cog at the top right of Home).

Releases are automated. Every push to `main` runs the checks and builds the package ([build.yml](.github/workflows/build.yml)). [release-please](https://github.com/googleapis/release-please) reads the commit messages ([Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`, ...) and keeps a release pull request open that bumps the version in `Cargo.toml` and writes `CHANGELOG.md`. Merging that pull request creates the tag and the GitHub release (a pre-release while the version is 0.x), and the same workflow then builds the package and attaches it. A tag pushed by hand also publishes (its name must match `version` in `Cargo.toml`). The build needs an arm64 runner because webosbrew publishes its toolchain for Linux only for arm64.

From a source checkout instead (needs the toolchain below): `make deploy on TV=root@<tv-ip>`, and `make release` builds the package. Nothing is changed on the TV outside `/media/developer/tvlauncher` and the one bind mount, except the hook that *Start as Home* adds in `/var/lib/webosbrew/init.d`. `TV` defaults to the SSH alias `lg-c1`.

## Build, try and undo

Needs rustup with a nightly toolchain that has `rust-src`, the webOS NDK at `~/webos-ndk` (Native Jelly's `make setup-env`) and, for the simulator, Homebrew `sdl2` (or `sdl2-compat`), `sdl2_image` and `sdl2_ttf`.

```sh
make arm                   # target/arm-unknown-linux-gnueabi/release/tvlauncher
make sim                   # desktop window against fixtures/ (arrows, Enter, Esc quits)
make shot KEYS=RRD         # build/shot.png after those presses, no window interaction
make deploy on             # copy to /media/developer/tvlauncher and make it Home
make deploy off on         # after a rebuild: the mount pins the old file
make off                   # back to the previous Home and restart CleanYourTV's watchdog
```

`make sim` pulls `fixtures/` from the TV the first time (`tools/pull-fixtures.sh`): launch points, inputs, their images and the LG fonts. It is gitignored because the artwork belongs to the app vendors.

If CleanYourTV is installed, the launcher won't offer to uninstall it from the menu: that only removes its apps, not its boot scripts. Remove it with its own `sh /media/developer/cleanyourtv/uninstall.sh` over SSH (it removes everything and reboots the TV; with *Start as Home* on, the launcher is Home again after the reboot, which was checked on the C1). Nothing here needs CleanYourTV.

`on` stops CleanYourTV's watchdog until `off` or the next reboot, because the watchdog puts its own Home back when its stand-in is not running. The launcher logs to `/tmp/tvlauncher.log` on the TV.

**Start at boot** is an option, off until chosen: Settings → Startup → **Start as Home**, or `make boot` / `make unboot` from the Mac (both run `takeover.sh boot on|off`). It links `boot.sh` into webosbrew's `/var/lib/webosbrew/init.d` as `20-tvlauncher-home` (so the TV needs the Homebrew Channel's startup). If CleanYourTV is installed, it also creates CleanYourTV's own `DISABLE` file (the switch in its manager that selects LG's Home; its service and manager app keep running, so nothing of it is removed), and removes that file again only if the option set it (marker `made-cytv-disable`). `boot.sh` waits (up to one minute) until the app manager answers and foreground queries work, then runs `BOOT=1 takeover.sh on`, which doesn't open Home if the TV started on an input or an app. On cold boot the launcher paints cached rows first and loads apps and inputs from the bus on a thread. It doesn't block the other boot hooks. If the launcher binary is missing, nothing happens and the TV has LG's Home (CleanYourTV stays off through `DISABLE`). To skip one boot, `touch /media/developer/tvlauncher/NOBOOT` over SSH; to go back, switch the option off (CleanYourTV's Home returns if it was in use). Verified on the C1: a full reboot started the launcher as Home. The crash watcher still applies: three crashes in a minute put LG's Home back until the next reboot, and the next boot tries again.

### Fallback

`on` also starts a small watcher (`takeover.sh watch`, pid in `/tmp/tvlauncher-watch.pid`). The launcher appends a line to `/tmp/tvlauncher-crashes` when it fails to start or dies on a signal (SEGV, ABRT from a panic, and so on). Three of them within a minute run `takeover.sh off`, which puts LG's Home and CleanYourTV's watchdog back, and the watcher writes why to `/tmp/tvlauncher-watch.log`. Not covered: a launcher that hangs without crashing; `make off` or a reboot fixes that.

### Registering with the app manager (opt-in)

Without it the app manager restarts the launcher on every Home key. With a file `/media/developer/tvlauncher/register` present, the launcher registers as Home through `luna-send -i -a com.webos.app.home .../registerApp`, as CleanYourTV's stand-in does: the Home key then arrives as a `relaunch` event (the launcher raises its window, reloads and focuses the first card), `close` ends it, and when the app manager goes away the launcher exits so the next Home starts a fresh one (the app manager restarts at standby). It is off by default because nobody has seen the window come to the front on a relaunch on this TV. If the Home key shows nothing, remove the file and run `make deploy off on`.

### The ARM build

- It is built with `-Zbuild-std`, `target-cpu=cortex-a9` and NEON off ([.cargo/config.toml](.cargo/config.toml)), as Native Jelly does. The precompiled std for `arm-unknown-linux-gnueabi` emits the ARMv6 CP15 barrier (`mcr p15`), which is undefined on the C1's Cortex-A53 and raises SIGILL. `objdump -d` of the result must show no `p15`. Do not set `RUSTFLAGS`: it replaces the flags in the config file.
- The NDK links against glibc 2.12, which has no `getauxval` (std calls it when it starts a thread), so [src/compat.rs](src/compat.rs) supplies one. [tools/arm-gcc](tools/arm-gcc) is the linker: the NDK gcc has no default sysroot.
- The binary is about 1.8 MB (the C version was 50 KB, with std statically linked).

## Testing

- `cargo test --release` runs the Jellyfin parsing checks. `JF_FIXTURES=$PWD/fixtures cargo test --release -- --ignored quick_connect` signs in against the real server (it authorizes its own code with the fixtures login, then signs out, which replaces that login: sign in again in the simulator). `make qemu` runs the ARM build under QEMU in Docker (`tools/qemu-smoke.sh`) with stock SDL2 and a fake `luna-send` that plays a registration session: a screenshot, a TV-mode start with the Jellyfin row, a relaunch and a close, and the crash counter.
- Pressing remote buttons from a script: any tool that speaks the LG webOS pairing protocol works (for example `aiopylgtv`'s `WebOsClient.button`). Keys from a `/dev/uinput` device do not reach apps; the compositor ignores the device. The colour buttons are not available that way, so check the Options key with a real remote.
- Screen: `luna://com.webos.service.capture/executeOneShot` (webOS 6 name; Native Jelly's `capture-screen.sh` uses the webOS 4.5 `tv.capture` name, which does not exist here).

## Verified

- On the C1 (webOS 6.5.3), with the C version this was ported from: started by the app manager as Home with the GLES2 renderer, launch points and inputs loaded in about 120 ms, RSS about 30 MB, D-pad focus, background crossfade, launching Netflix, switching to HDMI 3, and the Home key coming back with the focus on what was launched.
- **The Rust port on the C1 (2026-11-24):** it starts as Home (OpenGL ES 2), the remote's arrow, OK and Back keys arrive with the right symbols and `wcode`s (Back 482), OK's release arrives (so hold-OK works), the cog and the Settings entry open, RSS is 30 MB and CPU idle, `curl` is at `/usr/bin/curl` and reaches `youtube.com` over HTTPS, and with all four debloat switches on the services are stopped and the ad hosts are blocked (the TV was left that way by the owner; whether Netflix, YouTube and Jellyfin still play is the owner's to confirm). Not yet seen on the TV: Quick Connect, the Continue watching and YouTube rows, the random backdrops, the HTTPS sign-in, the Home key relaunch, launching an app, and the yellow key.
- **Live TV on the C1 (2026-11-24),** with generated test streams and a generated playlist and guide on the TV itself (`file://`), keys pressed through `sendSpecialKey` and the screen captured: picture on screen for H.264 + AAC, H.264 + AC-3, H.265 + AAC and a 720p stream (placed full screen from the size the pipeline reports); the banner with now and next; Up changing channel ten times and more; Back to the TV tab and OK playing another channel; the TV tab, its groups and the guide text. **Not checked:** the sound (nobody listened), a real provider's stream over the network, an Xtream login on the TV (a fake Xtream server was checked in the simulator), a gzipped guide, logos over the network, Favourites and the keyboard on the TV (both checked in the simulator), and the remote's own channel keys (Page Up and Page Down are assumed).
- **Jellyfin library tabs on the C1 (2026-11-24),** against a fake Jellyfin server run on the TV (400 films, 13 series, 3 collections, a music library and generated posters): the tabs in the server's order with music left out, posters loading while scrolling, memory flat over repeated scrolling, OK opening Native Jelly with the title's `itemId`, and Home back after it. Settings → Tabs (hide, show, move) and the sort were checked in the simulator. **Not checked:** a real server's libraries (it was asleep), and whether Native Jelly opens a collection's page.
- **Live TV with real channels (2026-11-24):** a public French playlist of 231 channels (HLS) and xmltvfr.fr's guide (72 of them matched) on the C1: the guide grid with logos, scrolling 20 channels in a row, OK playing a channel that is on and Back returning to it in the grid (64 MB with the guide loaded). Of the 231 channels, 143 gave pictures when read on a desktop; the rest are Pluto TV (encrypted segments) or answer errors at the source.
- Checked without a TV:
  - The desktop simulator renders the same pixels as the C version, apart from the clock, for the Apps and Inputs rows.
  - The ARM binary has no `p15` instruction, its SDL symbols all exist in the TV's SDL libraries (copied from the TV), and its `GLIBC` versions are at most 2.12 (the rest are weak symbols that std looks up at run time).
  - Under QEMU with stock SDL2 and dummy video (`make qemu`): the screenshot matches the simulator except for text smoothing and the clock; the TV-mode start writes `/tmp/tvlauncher.log`, calls `luna-send` with `-n 1 -w 5000 <uri> {}`, fetches the Jellyfin list over HTTP and caches it; the registration session's `relaunch` reloads the data and `close` exits with 0; a failed start and a SEGV each add a line to the crash file. `takeover.sh`'s crash count was tested with a made-up file.
  - Against the real Jellyfin 12.4.0 server on the LAN: resume and next-up parsing, image selection and download, the cache, and an unreachable server (the 6 s timeout keeps the cached row). Quick Connect end to end (code, authorize, sign-in, file mode 600, sign-out revoking the token) passes as a test. In the simulator: the sign-in dialog, the address pad (the broadcast found no server, as expected behind the portproxy) and the code screen, the app picker and its saved choice, and signing out. A person entering a code on a phone was not tested, nor the remote's number keys.
  - `sh tools/debloat-test.sh` passes (fake `systemctl`, `mount`, `umount`, `pkill`, `luna-send`; paths overridden). In the simulator the switches toggle, save to `state.json` and keep the list's selection. Busybox on the TV was not tried: the script uses only what CleanYourTV's scripts use, plus `grep -vx`, `tr` and `head`.
  - YouTube, against the real service from the Mac, without an account: the TV page, the base script and the OAuth client found in it, the device code (shown in the dialog), the `authorization_pending` answer to the token poll, and a logged-out browse answer whose tile format the parser was written against. **Not seen**: a token answer, a refresh, a signed-in home feed, a thumbnail download, `curl` on the TV, and the YouTube app acting on `contentTarget`.
  - Not covered: LG's SDL on a real Wayland session, the key event layout (`sym` at offset 24 and the auto-repeat bit come from the NDK header and Native Jelly's notes), the colour and Back button codes on this TV, a relaunch bringing the window to the front, the fallback watcher on the TV, and Native Jelly acting on the deep link.

## Not done

- Live TV: encrypted or MP4-segment HLS, MPEG-1 Layer II audio, programmes that have ended (the guide starts at now), and the Home key while a channel plays (it restarts the launcher, which ends playback; not seen).
- A screensaver, more Settings. Nothing of the YouTube row has run on the TV, and a signed-in answer has never been seen (see Verified). The debloat has not run on the TV: whether the stubs and the stopped services leave Netflix, YouTube and Jellyfin playback working is unchecked.
