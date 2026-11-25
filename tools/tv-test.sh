#!/bin/sh
# Drives tests of the film player on the real TV from this computer. Nothing here is needed to run the launcher.
#   sh tools/tv-test.sh status           the TV's power, foreground app, launcher memory, free space
#   sh tools/tv-test.sh deploy           make deploy off on (copies the program, FFmpeg, libass, the HBR files)
#   sh tools/tv-test.sh stage            makes the test films (ffmpeg, cached in build/tv-test) and copies them to the TV
#   sh tools/tv-test.sh probe            opens every staged film on the TV with `tvlauncher --direct`: no screen, no sound
#   sh tools/tv-test.sh fake on|off      a fake Jellyfin on the TV for the staged films (swaps jellyfin.json, off restores it)
#   sh tools/tv-test.sh state '<json>'   writes state.json (the settings); `state restore` puts the TV's own back
#   sh tools/tv-test.sh play <spec> [s]  starts the player (film:<id>, with `fake on` an id of a staged film), waits, takes
#                                        a screenshot into build/tv-test/ and prints the player log
#   sh tools/tv-test.sh keys UP,ENTER,.. remote keys with a second between them (UP DOWN LEFT RIGHT ENTER BACK HOME)
#   sh tools/tv-test.sh shot [name]      screenshot into build/tv-test/
#   sh tools/tv-test.sh logs [n]         the last n lines of the player and launcher logs
#   sh tools/tv-test.sh catalog          sorts the real Jellyfin library by what each title tests (build/tv-test/catalog.json)
#   sh tools/tv-test.sh real <feature>   `--direct` against the real server's smallest title with that feature
#   sh tools/tv-test.sh reset            stops the fake servers and restores jellyfin.json and state.json
# Playing a real title (`play film:<id>` with `fake off`) reports progress and a play to the server: Jellyfin keeps a
# resume point and a play count, Plex a view offset. Undo them afterwards (Jellyfin: DELETE /Users/<id>/PlayedItems/<item>;
# Plex: /:/unscrobble?key=<rating key>&identifier=com.plexapp.plugins.library). `film:<id>@<seconds>` starts part way in.
# `reset` also leaves the launcher running with the settings it was last started with: restart it (takeover.sh off, on).
# The TV is `lg-c1` (an ssh alias); away from home set TVJUMP="-J <jump host>" and TVHOST=<the TV's address>. Keys, the
# player and the screenshots take over a TV somebody may be watching: ask first.
set -eu
cd "$(dirname "$0")/.."
TVHOST=${TVHOST:-lg-c1}
TVJUMP=${TVJUMP:-}
DEST=/media/developer/tvlauncher
T=$DEST/test
OUT=build/tv-test
mkdir -p "$OUT"

# shellcheck disable=SC2086
tv() { ssh -o ConnectTimeout=8 $TVJUMP root@"$TVHOST" "$@" </dev/null; }
# shellcheck disable=SC2086
send() { scp -q $TVJUMP "$@"; }
luna() { tv "luna-send -n 1 -w ${3:-4000} luna://$1 '$2' </dev/null"; }

status() {
  tv 'luna-send -n 1 -w 3000 luna://com.webos.service.tvpower/power/getPowerState "{}" </dev/null; luna-send -n 1 -w 3000 luna://com.webos.applicationManager/getForegroundAppInfo "{}" </dev/null; for p in $(pidof com.webos.app.home); do tr "\0" " " </proc/$p/cmdline | cut -c1-60; grep VmRSS /proc/$p/status; done; df -h /media/developer | tail -1; head -3 /tmp/tvlauncher.log; ls '"$DEST"'/lib'
}

mk() { # name ffmpeg-args...: makes build/tv-test/<name> once
  n=$1; shift
  [ -f "$OUT/$n" ] || { echo "making $n"; ffmpeg -hide_banner -loglevel error -y "$@" "$OUT/$n"; }
}

stage() {
  command -v ffmpeg >/dev/null || { echo "ffmpeg is needed" >&2; exit 1; }
  V720="-f lavfi -i testsrc2=size=1280x720:rate=24"
  V1080="-f lavfi -i testsrc2=size=1920x1080:rate=24"
  V4K="-f lavfi -i testsrc2=size=3840x2160:rate=24"
  A="-f lavfi -i sine=frequency=440:sample_rate=48000"
  A2="-f lavfi -i sine=frequency=880:sample_rate=48000"
  A6="-f lavfi -i anoisesrc=amplitude=0.1:sample_rate=48000"
  HDR="-pix_fmt yuv420p10le -c:v libx265 -preset ultrafast -x265-params hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400:log-level=error"
  printf '1\n00:00:02,000 --> 00:00:05,000\n<i>Bonjour</i> le monde\n\n2\n00:00:10,000 --> 00:00:12,500\n{\\an8}En haut\n' > "$OUT/fr.srt"
  printf '1\n00:00:03,000 --> 00:00:06,000\nHello world\n\n2\n00:00:12,000 --> 00:00:15,000\nSecond line\n' > "$OUT/en.srt"
  printf '1\n00:00:04,000 --> 00:00:07,000\nForced: speaking Klingon\n' > "$OUT/forced.srt"
  # film.mkv is item m1: AC-3 English and FLAC French, French subtitles (what film-sim uses)
  mk film.mkv $V720 $A $A2 -i "$OUT/fr.srt" -t 30 -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 48 -c:a:0 ac3 -c:a:1 flac -ac:a:1 2 \
    -map 0:v -map 1:a -map 2:a -map 3 -c:s srt -metadata:s:a:0 language=eng -metadata:s:a:1 language=fre -metadata:s:s:0 language=fre
  mk hevc-hdr10.mkv $V1080 $A6 -t 20 $HDR -c:a eac3 -ac 6 -b:a 384k -metadata:s:a:0 language=eng
  mk uhd-hdr10.mkv $V4K $A6 -t 10 $HDR -c:a eac3 -ac 6 -b:a 384k -metadata:s:a:0 language=eng
  mk aac.mp4 $V720 $A -t 20 -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac
  mk dts.mkv $V720 $A6 -t 20 -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a dca -strict -2 -ac 6 -b:a 768k
  mk truehd.mkv $V1080 $A6 -t 20 -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a truehd -strict -2 -ac 6
  mk h264-10bit.mkv $V720 $A -t 10 -c:v libx264 -preset veryfast -pix_fmt yuv420p10le -c:a ac3
  mk subs.mkv $V720 $A $A2 -i "$OUT/en.srt" -i "$OUT/fr.srt" -i "$OUT/forced.srt" -t 30 -c:v libx264 -preset veryfast -pix_fmt yuv420p \
    -c:a ac3 -map 0:v -map 1:a -map 2:a -map 3 -map 4 -map 5 -c:s srt -metadata:s:a:0 language=eng -metadata:s:a:1 language=fre \
    -metadata:s:s:0 language=eng -metadata:s:s:1 language=fre -metadata:s:s:2 language=fre -disposition:s:2 forced
  # 10 minutes for seeking: a keyframe every 2 s, small
  mk long.mkv $V720 $A -t 600 -c:v libx264 -preset veryfast -pix_fmt yuv420p -b:v 400k -g 48 -c:a aac -b:a 96k
  if [ -f build/film-sim/film-ass.mkv ]; then cp -n build/film-sim/film-ass.mkv "$OUT/ass.mkv" || true; fi
  for f in "$OUT"/*.mkv "$OUT"/*.mp4; do
    id=$(basename "$f"); id=${id%.*}
    [ "$id" = film ] && continue
    python3 tools/tv-catalog.py item "$f" "$OUT/$id.item.json"
  done
  tv "mkdir -p $T"
  send tools/fake-media.js tools/range-server.js root@"$TVHOST":$T/
  for f in "$OUT"/*.mkv "$OUT"/*.mp4 "$OUT"/*.item.json "$OUT"/tile.jpg; do
    [ -f "$f" ] || continue
    send "$f" root@"$TVHOST":$T/
  done
  [ -f "$OUT/tile.jpg" ] || { ffmpeg -hide_banner -loglevel error -y -f lavfi -i testsrc2=size=320x180:rate=1 -t 30 -vf tile=10x3 -frames:v 1 "$OUT/tile.jpg"; send "$OUT/tile.jpg" root@"$TVHOST":$T/; }
  tv "ls -l $T | cut -c25-; df -h /media/developer | tail -1"
}

# starts a node script on the TV that is told apart by its first argument (webOS runs node processes of its own)
node_on() { tv "cd $T && (nohup node $1 $2 $3 >/tmp/tvtest-$4.log 2>&1 &)"; }
node_off() { tv 'for p in $(pidof node); do tr "\0" " " </proc/$p/cmdline | grep -q "'"$1"'" && kill $p; done; true'; }

probe() {
  node_off range-server; node_on range-server.js "$T" 8771 range; sleep 2
  for f in $(tv "ls $T" | grep -E '\.(mkv|mp4)$'); do
    echo "== $f"
    tv "$DEST/tvlauncher --direct http://127.0.0.1:8771/$f 0 2>&1 | cut -c1-230" | tee "$OUT/probe-$f.txt" | grep -v "^$" | head -14
  done
  node_off range-server
}

fake() {
  case "$1" in
  on)
    tv "[ -f /media/developer/jellyfin.json.bak ] || cp $DEST/jellyfin.json /media/developer/jellyfin.json.bak 2>/dev/null; true"
    node_off fake-media; node_on fake-media.js "$T" 8767 fake; sleep 2
    tv "printf '%s' '{\"server\":\"http://127.0.0.1:8767\",\"serverId\":\"s1\",\"userId\":\"u1\",\"user\":\"demo\",\"token\":\"t\"}' > $DEST/jellyfin.json; chmod 600 $DEST/jellyfin.json"
    echo "fake Jellyfin on: play film:m1 (film.mkv), film:hevc-hdr10, film:uhd-hdr10, film:aac, film:dts, film:truehd, film:h264-10bit, film:subs, film:long, film:ass"
    ;;
  off)
    node_off fake-media
    tv "[ -f /media/developer/jellyfin.json.bak ] && mv /media/developer/jellyfin.json.bak $DEST/jellyfin.json && chmod 600 $DEST/jellyfin.json; rm -f $DEST/cache/lib-* $DEST/cache/home-* $DEST/cache/watching.json; true"
    ;;
  *) echo "fake on|off" >&2; exit 1 ;;
  esac
}

state() {
  # merges the JSON into the TV's own state.json (kept once as state.json.bak) and restarts the launcher to read it
  if [ "$1" = restore ]; then
    tv "[ -f /media/developer/state.json.bak ] && mv /media/developer/state.json.bak $DEST/state.json; true"
  else
    printf '%s' "$1" > "$OUT/patch.json"
    send "$OUT/patch.json" root@"$TVHOST":/tmp/tvtest-patch.json
    tv "[ -f /media/developer/state.json.bak ] || cp $DEST/state.json /media/developer/state.json.bak 2>/dev/null; python -c 'import json; d = json.load(open(\"/media/developer/state.json.bak\")); d.update(json.load(open(\"/tmp/tvtest-patch.json\"))); json.dump(d, open(\"$DEST/state.json\", \"w\"))'"
  fi
  tv "rm -f /tmp/tvlauncher-crashes; sh $DEST/takeover.sh off; sh $DEST/takeover.sh on" >/dev/null
  sleep 6
}

shot() {
  n=${1:-shot}
  luna com.webos.service.capture/executeOneShot '{"path":"/tmp/tvtest.jpg","method":"DISPLAY","width":1280,"height":720,"format":"JPEG"}' 8000 >/dev/null
  send root@"$TVHOST":/tmp/tvtest.jpg "$OUT/$n.jpg"
  echo "$OUT/$n.jpg"
}

play() {
  spec=$1; secs=${2:-20}
  tv ': > /tmp/tvlauncher-player.log; echo "'"$spec"'" > /tmp/tvlauncher-spawn'
  sleep "$secs"
  shot "play-$(echo "$spec" | tr ':/' '__')"
  tv 'luna-send -n 1 -w 3000 luna://com.webos.applicationManager/getForegroundAppInfo "{}" </dev/null; cut -c1-250 /tmp/tvlauncher-player.log | tail -60'
}

keys() {
  for k in $(echo "$1" | tr ',' ' '); do
    luna com.webos.service.networkinput/sendSpecialKey "{\"key\":\"$k\"}" 3000 >/dev/null
    sleep "${KEY_WAIT:-1}"
  done
}

logs() {
  tv "echo '--- player'; tail -n ${1:-60} /tmp/tvlauncher-player.log | cut -c1-250; echo '--- launcher'; tail -n 20 /tmp/tvlauncher.log | cut -c1-250"
}

catalog() {
  cfg=$(tv "cat $DEST/jellyfin.json")
  field() { echo "$cfg" | python3 -c "import json,sys; print(json.load(sys.stdin)['$1'])"; }
  python3 tools/tv-catalog.py catalog "$(field server)" "$(field userId)" "$(field token)" | tee "$OUT/catalog.json"
}

real() {
  cfg=$(tv "cat $DEST/jellyfin.json")
  f() { echo "$cfg" | python3 -c "import json,sys; print(json.load(sys.stdin)['$1'])"; }
  [ -f "$OUT/catalog.json" ] || catalog >/dev/null
  id=$(python3 -c "import json; d=json.load(open('$OUT/catalog.json'))['$1']; print(d['id'], d['source'], d['name'])")
  set -- $id
  echo "== $1 ($3 ...)"
  tv "timeout -t ${SECS:-25} $DEST/tvlauncher --direct '$(f server)/Videos/$1/stream?static=true&MediaSourceId=$2&api_key=$(f token)' 0 2>&1 | cut -c1-230"
}

reset() { node_off fake-media; node_off range-server; fake off; state restore; }

cmd=${1:-}; [ $# -gt 0 ] && shift
case "$cmd" in
  status|deploy|stage|probe|fake|state|play|keys|shot|logs|catalog|real|reset)
    if [ "$cmd" = deploy ]; then make deploy off on; else "$cmd" "$@"; fi ;;
  *) sed -n 2,22p "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
