#!/bin/sh
# Builds libass-plx, the library the film player draws styled ASS/SSA subtitles with: libass with its own FreeType,
# FriBidi and HarfBuzz linked in statically, as one shared library that exports only `ass_*`, so that nothing of the
# TV's own font libraries can meet it. No font provider: the player hands it the fonts attached to the film and one
# default font.
# Usage: tools/build-libass.sh arm     for the TV (needs the webOS NDK and cmake), into build/libass-arm
#        tools/build-libass.sh host    for this computer's simulator, into build/libass-host
#
# libass is ISC, FreeType is the FreeType License (or GPL-2.0), HarfBuzz is MIT and FriBidi is LGPL-2.1-or-later: the
# unmodified source archives below are what a release attaches next to the package, and THIRD-PARTY-NOTICES.md says
# how to rebuild the library with changed ones. The pins are those of Native Jelly's ci/libass-dependencies.json
# (GPL-3.0), whose recipe this follows.
set -eu

TARGET=${1:?usage: build-libass.sh arm|host}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
WORK=$ROOT/build/libass-work-$TARGET
OUT=$ROOT/build/libass-$TARGET
SRC=$ROOT/build/libass-src
PREFIX=$WORK/prefix
JOBS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# name version sha256 url
PINS="freetype 2.15.0 93cd47eb719c325107f2d3cf087d06299e944c3b48a40b55537302304dea1dd5 https://download.savannah.gnu.org/releases/freetype/freetype-2.15.0.tar.xz
fribidi 1.0.18 1de5e8073d035388b71d0b2d452b1da1f8caeea8a7702997b401fe1b6250727f https://github.com/fribidi/fribidi/releases/download/v1.0.18/fribidi-1.0.18.tar.xz
harfbuzz 15.0.0 968536e9abd782f07f7954044c0c4020199fcdd39b41d221ec42d2a3be4a44df https://github.com/harfbuzz/harfbuzz/releases/download/15.0.0/harfbuzz-15.0.0.tar.xz
libass 0.17.5 2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz"

command -v cmake >/dev/null 2>&1 || { echo "libass: cmake is needed" >&2; exit 1; }
command -v pkg-config >/dev/null 2>&1 || { echo "libass: pkg-config is needed" >&2; exit 1; }
mkdir -p "$SRC" "$OUT"
echo "$PINS" | while read -r name version sum url; do
  tar=$SRC/$name-$version.tar.xz
  if [ ! -f "$tar" ] || [ "$(sha "$tar")" != "$sum" ]; then
    curl -fsSL --retry 3 "$url" -o "$tar"
    [ "$(sha "$tar")" = "$sum" ] || { echo "libass: $name does not match its checksum" >&2; exit 1; }
  fi
done

rm -rf "$WORK" && mkdir -p "$WORK/src" "$PREFIX"
echo "$PINS" | while read -r name version sum url; do tar xf "$SRC/$name-$version.tar.xz" -C "$WORK/src"; done

FLAGS="-O2 -fPIC"
CMAKE_ARGS="-DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_PREFIX_PATH=$PREFIX -DCMAKE_INSTALL_PREFIX=$PREFIX"
CONF_ARGS="--prefix=$PREFIX --libdir=$PREFIX/lib --disable-shared --enable-static --with-pic"
case "$TARGET" in
  arm)
    SYSROOT=$NDK/arm-webos-linux-gnueabi/sysroot
    CROSS=$NDK/bin/arm-webos-linux-gnueabi-
    [ -x "${CROSS}gcc" ] || { echo "libass: no webOS NDK at $NDK" >&2; exit 1; }
    CC=${CROSS}gcc CXX=${CROSS}c++ AR=${CROSS}ar RANLIB=${CROSS}ranlib
    FLAGS="$FLAGS --sysroot=$SYSROOT"
    CMAKE_ARGS="$CMAKE_ARGS -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=arm -DCMAKE_SYSROOT=$SYSROOT \
      -DCMAKE_FIND_ROOT_PATH=$PREFIX;$SYSROOT -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER \
      -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY \
      -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY"
    CONF_ARGS="$CONF_ARGS --host=arm-webos-linux-gnueabi"
    ;;
  host)
    CC=$(command -v cc) CXX=$(command -v c++) AR=$(command -v ar) RANLIB=$(command -v ranlib)
    if [ "$(uname)" = Darwin ]; then
      FLAGS="$FLAGS -mmacosx-version-min=11.0"
      CMAKE_ARGS="$CMAKE_ARGS -DCMAKE_OSX_DEPLOYMENT_TARGET=11.0"
    fi
    ;;
  *) echo "usage: build-libass.sh arm|host" >&2; exit 1 ;;
esac
export CC CXX AR RANLIB CFLAGS="$FLAGS" CXXFLAGS="$FLAGS" PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
unset CPPFLAGS LDFLAGS LIBS PKG_CONFIG_PATH PKG_CONFIG_SYSROOT_DIR

cmake_build() { # name options...
  n=$1; shift
  # shellcheck disable=SC2086
  cmake -S "$WORK/src/$n"-* -B "$WORK/obj/$n" $CMAKE_ARGS -DCMAKE_C_COMPILER="$CC" -DCMAKE_CXX_COMPILER="$CXX" \
    -DCMAKE_AR="$AR" -DCMAKE_RANLIB="$RANLIB" -DCMAKE_C_FLAGS="$FLAGS" -DCMAKE_CXX_FLAGS="$FLAGS" "$@" > "$WORK/$n.log" 2>&1 &&
    cmake --build "$WORK/obj/$n" --parallel "$JOBS" >> "$WORK/$n.log" 2>&1 &&
    cmake --install "$WORK/obj/$n" >> "$WORK/$n.log" 2>&1 || { tail -20 "$WORK/$n.log" >&2; exit 1; }
}
conf_build() { # name options...
  n=$1; shift
  mkdir -p "$WORK/obj/$n"
  conf=$(echo "$WORK"/src/"$n"-*/configure)
  # shellcheck disable=SC2086
  (cd "$WORK/obj/$n" && "$conf" $CONF_ARGS "$@" > "$WORK/$n.log" 2>&1) || { tail -20 "$WORK/$n.log" >&2; exit 1; }
  if [ "$n" = fribidi ]; then
    # its command line tools need help2man and cannot run in a cross build: the library alone
    (cd "$WORK/obj/$n" && make -C lib -j"$JOBS" && make -C lib install && make install-pkgconfigDATA) >> "$WORK/$n.log" 2>&1
  else
    (cd "$WORK/obj/$n" && make -j"$JOBS" && make install) >> "$WORK/$n.log" 2>&1
  fi || { tail -20 "$WORK/$n.log" >&2; exit 1; }
}

cmake_build freetype -DFT_DISABLE_ZLIB=ON -DFT_DISABLE_BZIP2=ON -DFT_DISABLE_PNG=ON -DFT_DISABLE_HARFBUZZ=ON \
  -DFT_DISABLE_BROTLI=ON -DFT_DISABLE_HVF=ON
conf_build fribidi
cmake_build harfbuzz -DHB_HAVE_FREETYPE=ON -DFREETYPE_LIBRARY="$PREFIX/lib/libfreetype.a" \
  -DFREETYPE_INCLUDE_DIRS="$PREFIX/include/freetype2" -DHB_HAVE_CORETEXT=OFF -DHB_BUILD_UTILS=OFF \
  -DHB_BUILD_SUBSET=OFF -DHB_BUILD_RASTER=OFF -DHB_BUILD_VECTOR=OFF -DHB_BUILD_GPU=OFF
conf_build libass --disable-fontconfig --disable-coretext --disable-directwrite --disable-require-system-font-provider \
  --disable-libunibreak --disable-test --disable-compare --disable-profile

if [ "$TARGET" = arm ]; then
  NAME=libass-plx.so.0
  printf '{ global: ass_*; local: *; };\n' > "$WORK/exports.map"
  # -tno-glibc-polyfill keeps the toolchain's glibc polyfill archive out of the library.
  # shellcheck disable=SC2086
  $CC $FLAGS -shared -Wl,-soname,$NAME -Wl,--no-undefined -Wl,--version-script="$WORK/exports.map" \
    -tno-glibc-polyfill -Wl,--whole-archive "$PREFIX/lib/libass.a" -Wl,--no-whole-archive \
    "$PREFIX/lib/libharfbuzz.a" "$PREFIX/lib/libfribidi.a" "$PREFIX/lib/libfreetype.a" -lm -o "$OUT/$NAME"
  "${CROSS}strip" --strip-unneeded "$OUT/$NAME"
  # nothing but libc may be needed, and only ass_* may leave
  needed=$("${CROSS}readelf" -d "$OUT/$NAME" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | grep -v -x -e libc.so.6 -e libm.so.6 -e libgcc_s.so.1 -e libpthread.so.0 -e ld-linux.so.3 || true)
  [ -z "$needed" ] || { echo "libass: unexpected dependencies: $needed" >&2; exit 1; }
else
  if [ "$(uname)" = Darwin ]; then
    NAME=libass-plx.0.dylib
    printf '_ass_*\n' > "$WORK/exports.txt"
    # shellcheck disable=SC2086
    $CC $FLAGS -dynamiclib -Wl,-install_name,@loader_path/$NAME -Wl,-exported_symbols_list,"$WORK/exports.txt" \
      -Wl,-force_load,"$PREFIX/lib/libass.a" "$PREFIX/lib/libharfbuzz.a" "$PREFIX/lib/libfribidi.a" \
      "$PREFIX/lib/libfreetype.a" -liconv -lc++ -o "$OUT/$NAME"
  else
    NAME=libass-plx.so.0
    printf '{ global: ass_*; local: *; };\n' > "$WORK/exports.map"
    # shellcheck disable=SC2086
    $CC $FLAGS -shared -Wl,-soname,$NAME -Wl,--version-script="$WORK/exports.map" \
      -Wl,--whole-archive "$PREFIX/lib/libass.a" -Wl,--no-whole-archive \
      "$PREFIX/lib/libharfbuzz.a" "$PREFIX/lib/libfribidi.a" "$PREFIX/lib/libfreetype.a" -lm -lstdc++ -o "$OUT/$NAME"
  fi
fi
rm -rf "$WORK"
ls -l "$OUT"
