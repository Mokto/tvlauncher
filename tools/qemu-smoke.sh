#!/bin/sh
# Runs the ARM build under QEMU (Docker, arm32v5 Debian with stock SDL2, dummy video), without a TV.
# Checks: a screenshot, the TV-mode start (log file, luna calls, Jellyfin row), registration with a relaunch and a close,
# and that a crash is counted for the watchdog. Needs `make arm`, fixtures/ and a Jellyfin login in fixtures.
set -e
cd "$(dirname "$0")/.."
BIN=$PWD/target/arm-unknown-linux-gnueabi/release/tvlauncher
FX=$PWD/fixtures
docker image inspect tvlauncher-armtest >/dev/null 2>&1 || {
  printf 'FROM arm32v5/debian:bookworm-slim\nRUN apt-get update -qq && apt-get install -y -qq --no-install-recommends libsdl2-2.0-0 libsdl2-image-2.0-0 libsdl2-ttf-2.0-0 procps >/dev/null\n' |
    docker build --platform linux/arm/v5 --load -t tvlauncher-armtest -
}
run() { # $1: shell script run in the container
  docker run --rm --platform linux/arm/v5 -v "$BIN:/tvl:ro" -v "$FX:/fx:ro" -v "$PWD/tools/fake-luna-send:/usr/bin/luna-send:ro" \
    -v "$FX/usr/share/fonts:/usr/share/fonts:ro" -v "$FX/usr/palm:/usr/palm:ro" -v "$PWD/build:/out" \
    -e SDL_VIDEODRIVER=dummy -e SDL_RENDER_DRIVER=software tvlauncher-armtest sh -c "$1"
}
mkdir -p build
echo "== screenshot (simulator mode)"
run 'LAUNCHER_ROOT=/fx /tvl --shot /out/arm.bmp DM; echo rc=$?'
echo "== TV mode, registered, with Jellyfin"
run 'mkdir -p /media/developer/tvlauncher && cp /fx/media/developer/tvlauncher/jellyfin.json /media/developer/tvlauncher/ &&
  touch /media/developer/tvlauncher/register && timeout 12 /tvl "{}"; echo rc=$?; cat /tmp/tvlauncher.log; sort -u /tmp/luna-calls;
  ls /media/developer/tvlauncher/cache | head -3'
echo "== crash counter"
run 'SDL_VIDEODRIVER=nonexistent /tvl; echo rc=$?; cat /tmp/tvlauncher-crashes;
  /tvl & p=$!; sleep 4; kill -SEGV $p; sleep 1; echo; cat /tmp/tvlauncher-crashes'
