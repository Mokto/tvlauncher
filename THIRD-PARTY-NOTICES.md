# Third-party notices

## FFmpeg (the film player)

The package carries three FFmpeg 9.0.2 libraries in `lib/`: `libavutil-plx`, `libavcodec-plx` and `libavformat-plx`.
They are licensed under the GNU Lesser General Public License, version 2.1 or later
(<https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html>). FFmpeg is a trademark of Fabrice Bellard.

- They are built from the **unmodified** FFmpeg source release `ffmpeg-9.0.2.tar.xz` (SHA-256
  `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`, <https://ffmpeg.org/releases/>), which every
  release of this project attaches next to the package, with [`tools/build-ffmpeg.sh`](tools/build-ffmpeg.sh), the
  exact configuration. Without `--enable-gpl` and `--enable-nonfree`; zlib is the only external library.
- They are separate shared libraries, loaded at run time, so you can replace them: build your own with the script
  (`sh tools/build-ffmpeg.sh arm`, or `host` for the simulator) and copy the files over `lib/` on the TV
  (`/media/developer/tvlauncher/lib`).
- The `-plx` suffix in the file names keeps them apart from any FFmpeg the TV has.

## Native Jelly and PLX Native

Parts of the film player follow, and some code is adapted from, [Native Jelly](https://github.com/sostk/native-jelly)
and [PLX Native](https://github.com/GLinnik21/plx-native) by GLinnik21 (both GPL-3.0-or-later, like this project):

- the calls into LG's media pipeline and the shape of its Load payload (already noted in `src/player.rs`);
- `src/ff/`: the field offsets of FFmpeg's structures, the framing of H.264, H.265 and AAC for the pipeline
  (Annex-B, ADTS) and the conversion of TrueHD, FLAC and similar audio to AC-3 (`convert.rs`, with its tests);
- the Dolby Vision declaration in the Load payload (`DolbyHdrInfo`);
- `src/ff/lossless.rs` and `src/ff/silent-eac3.bin`: the lossless audio carrier, from Native Jelly's `es_carrier.rs` and
  `es_swap.c` (pull request 46: DTS-HD and TrueHD bursts on LG's Dolby Digital Plus route, the TrueHD MAT packer ported
  from FFmpeg's `spdif` muxer) and the DTS core burst (`ff.rs`).

## earc_route (kernel module for HBR audio)

`hbr/earc_route-4.4.ko` is the **GPL-2.0-only** Linux kernel module `earc_route` of Native Jelly's pull request 46, from
[dts_restore_plus](https://github.com/josippapez/dts_restore_plus) `e42c98e` plus Native Jelly's hooks, which in turn
derives from Glasshouse's native-route and lg-g5-dts (see `vendor/earc_route/NOTICE`). It is a separate program: the
launcher never links it, and it only loads when the owner installs it from Settings (`hbr/hbr.sh`). The complete
corresponding source, the build scripts, the patch, `LICENSE` and `NOTICE` are in `vendor/earc_route/` of the repository
and in `earc_route-source/` of the package. It is built for webOS 6 kernel 4.4.84; the loader and the module refuse
anything else. It contains no LG code.
