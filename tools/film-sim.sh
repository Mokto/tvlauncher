#!/bin/sh
# Runs the film player on this computer, with no TV: a fake Jellyfin (tools/fake-media.js) serves a short test film with
# two audio tracks and subtitles, and a fake player library (tools/fake-starfish.c) takes the place of LG's. Nothing is
# decoded or shown but the controls, so this checks the flow (reading, pause, seeking, tracks, subtitles, reports),
# not what the TV does with the stream. Needs ffmpeg and node on the PATH, and the host FFmpeg libraries
# (`make ffmpeg-host`).
# Usage: sh tools/film-sim.sh [keys]    keys as "seconds:key,..." with enter, left, right, up, down, esc
#   e.g. sh tools/film-sim.sh "3:enter,5:enter,7:right,16:esc"
set -eu
cd "$(dirname "$0")/.."
D=build/film-sim
mkdir -p "$D/root" build/fake-sf
[ -f "$D/film.mkv" ] || {
  printf '1\n00:00:02,000 --> 00:00:05,000\n<i>Bonjour</i> le monde\n\n2\n00:00:10,000 --> 00:00:12,500\n{\\an8}En haut\n' > "$D/s.srt"
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i testsrc2=size=1280x720:rate=24 -f lavfi -i sine=frequency=440:sample_rate=48000 \
    -f lavfi -i sine=frequency=880:sample_rate=48000 -i "$D/s.srt" -t 30 -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 48 \
    -c:a:0 ac3 -c:a:1 flac -ac:a:1 2 -map 0:v -map 1:a -map 2:a -map 3 -c:s srt -metadata:s:a:0 language=eng -metadata:s:a:1 language=fre -metadata:s:s:0 language=fre "$D/film.mkv"
}
[ -f "$D/tile.jpg" ] || ffmpeg -hide_banner -loglevel error -y -f lavfi -i testsrc2=size=320x180:rate=1 -t 30 -vf tile=10x3 -frames:v 1 "$D/tile.jpg"
clang -dynamiclib -o build/fake-sf/libplayerAPIs.so.1 tools/fake-starfish.c
cargo build --release
R=$D/root
rm -rf "$R" && cp -R fixtures "$R" && rm -f "$R/media/developer/tvlauncher/state.json"
printf '{"server":"http://127.0.0.1:8780","serverId":"s1","userId":"u1","user":"demo","token":"t"}' > "$R/media/developer/tvlauncher/jellyfin.json"
# lossless audio on; it only does anything with FILM_RECEIVER=1, which stands in for an ARC receiver
STATE=${FILM_STATE:-'{"lossless":true}'}
printf '%s' "$STATE" > "$R/media/developer/tvlauncher/state.json"
node tools/fake-media.js "$D" 8780 > "$D/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER' EXIT
sleep 1
FILM_KEYS="${1:-6:esc}" DYLD_INSERT_LIBRARIES=build/fake-sf/libplayerAPIs.so.1 DYLD_LIBRARY_PATH=build/fake-sf \
  LAUNCHER_ROOT="$R" target/release/tvlauncher --film-sim m1
echo "--- what the server heard:"
grep -E "REPORT|RANGE|TILE" "$D/server.log" | cut -c1-160
