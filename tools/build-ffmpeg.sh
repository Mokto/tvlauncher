#!/bin/sh
# Builds the three FFmpeg libraries the film player reads files with: libavutil, libavcodec and libavformat.
# Usage: tools/build-ffmpeg.sh arm     for the TV (needs the webOS NDK, see README), into build/ffmpeg-arm
#        tools/build-ffmpeg.sh host    for this computer's simulator, into build/ffmpeg-host
#
# The libraries are LGPL-2.1: no --enable-gpl, no --enable-nonfree, and they stay separate shared libraries so that
# anyone can replace them. The source is the unmodified release tarball below, the same one a release attaches next to
# the package. The list of parts is short on purpose (it sets the size): the Matroska, MP4 and MPEG-TS readers, the
# parsers they need, the subtitle decoders, the lossless audio decoders and the AC-3 encoder that turns what the TV
# cannot play (TrueHD, FLAC, ...) into AC-3. Video is never decoded here: the TV does that. No network code: the
# program reads the file with the TV's curl. The component list is that of Native Jelly's ci/build-ffmpeg.sh (GPL-3.0),
# whose layouts rust-modules/src/ff.rs and src/ff/mod.rs rely on.
set -eu

VERSION=9.0.2
SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
TARGET=${1:?usage: build-ffmpeg.sh arm|host}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
WORK=$ROOT/build/ffmpeg-work-$TARGET
OUT=$ROOT/build/ffmpeg-$TARGET
TAR=$ROOT/build/ffmpeg-$VERSION.tar.xz

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

mkdir -p "$ROOT/build"
if [ ! -f "$TAR" ] || [ "$(sha "$TAR")" != "$SHA256" ]; then
  curl -fsSL --retry 3 "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$TAR"
  [ "$(sha "$TAR")" = "$SHA256" ] || { echo "ffmpeg: the tarball does not match its checksum" >&2; exit 1; }
fi

rm -rf "$WORK" && mkdir -p "$WORK" "$OUT"
tar xf "$TAR" -C "$WORK"
cd "$WORK/ffmpeg-$VERSION"

case "$TARGET" in
  arm)
    SYSROOT=$NDK/arm-webos-linux-gnueabi/sysroot
    CROSS=$NDK/bin/arm-webos-linux-gnueabi-
    [ -x "${CROSS}gcc" ] || { echo "ffmpeg: no webOS NDK at $NDK" >&2; exit 1; }
    # -tno-glibc-polyfill keeps the toolchain's glibc polyfill archive out of the libraries.
    set -- --enable-cross-compile --cross-prefix="$CROSS" --host-cc=cc --arch=arm --cpu=cortex-a9 --target-os=linux \
      --sysroot="$SYSROOT" --extra-ldflags=-tno-glibc-polyfill
    ;;
  host)
    set --
    if [ "$(uname -m)" = x86_64 ] && ! command -v nasm >/dev/null 2>&1; then set -- --disable-x86asm; fi
    ;;
  *) echo "usage: build-ffmpeg.sh arm|host" >&2; exit 1 ;;
esac

# --prefix=/plx is a stand-in, so that no path of this machine ends up inside the libraries; DESTDIR below puts the
# files where they are wanted.
./configure --prefix=/plx "$@" \
  --build-suffix=-plx --enable-shared --disable-static --enable-pic \
  --extra-cflags='-Dstatic_assert=_Static_assert' \
  --disable-programs --disable-doc --disable-avdevice --disable-avfilter --disable-swscale --disable-swresample \
  --disable-network --disable-debug --enable-small --disable-everything --disable-autodetect \
  --enable-zlib \
  --enable-demuxer=matroska,mov,mpegts,h264,hevc \
  --enable-parser=h264,hevc,aac,ac3,dvdsub,dvbsub,mlp,flac \
  --enable-decoder=pgssub,dvdsub,dvbsub,ass,srt,subrip,text,mov_text,webvtt \
  --enable-decoder=truehd,mlp,flac,alac,vorbis,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_bluray,pcm_dvd \
  --enable-encoder=ac3 \
  --enable-protocol=file > "$WORK/configure.log" 2>&1 || { tail -20 "$WORK/configure.log" >&2; exit 1; }
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" > "$WORK/build.log" 2>&1 || { tail -20 "$WORK/build.log" >&2; exit 1; }
make install DESTDIR="$WORK/dest" > "$WORK/install.log" 2>&1

for lib in avutil:61 avcodec:63 avformat:63; do
  name=${lib%:*}; major=${lib#*:}
  if [ "$TARGET" = arm ]; then
    cp -L "$WORK/dest/plx/lib/lib$name-plx.so.$major" "$OUT/lib$name-plx.so.$major"
    "${CROSS}strip" --strip-unneeded "$OUT/lib$name-plx.so.$major"
  else
    cp -L "$WORK/dest/plx/lib/lib$name-plx.$major.dylib" "$OUT/lib$name-plx.$major.dylib" 2>/dev/null ||
      cp -L "$WORK/dest/plx/lib/lib$name-plx.so.$major" "$OUT/lib$name-plx.so.$major"
  fi
done
rm -rf "$WORK"
ls -l "$OUT"
