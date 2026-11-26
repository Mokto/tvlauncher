#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! sh tools/film-sim.sh '2:down,3:play,4:enter,5:play,6:play,7:esc,8:play,9:play,10:esc,11:up,12:right,12:esc,13:play,14:play,15:esc,16:esc,18:play' >"$log" 2>&1 ||
   [ "$(grep -c 'film: paused at ' "$log")" -ne 4 ] ||
   [ "$(grep -c 'film: resumed at ' "$log")" -ne 3 ] ||
   grep -q 'film: seek from ' "$log" ||
   ! grep -q 'sim: key 27 at 16 s' "$log" ||
   grep -q 'sim: key .* at 18 s' "$log" ||
   ! grep -q 'REPORT /Sessions/Playing/Stopped' "$log"; then
    tail -80 "$log"
    exit 1
fi
printf '%s\n' 'Film controls check passed'
