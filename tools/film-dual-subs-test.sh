#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
log=$(mktemp)
trap 'rm -f "$log"' EXIT
export FILM_STATE='{"subMode":"on","subLang":"dan","direct":"force"}'
if ! sh tools/film-sim.sh '2:down,3:enter,4:up,5:up,6:enter,7:down,8:enter,14:down,15:enter,16:up,17:up,18:enter,19:down,20:enter,25:esc,26:esc,27:esc' >"$log" 2>&1 ||
   ! grep -q 'second subtitle Some(SubSel { index: 4, server: false })' "$log" ||
   ! grep -q 'second subtitle Some(SubSel { index: 5, server: true })' "$log" ||
   ! grep -Eq 'subtitle on at 9[0-9]{3} ms' "$log" ||
   ! grep -Eq 'subtitle on at 10[0-9]{3} ms' "$log" ||
   ! grep -Eq 'subtitle on at 20[0-9]{3} ms' "$log" ||
   ! grep -q 'REPORT /Sessions/Playing/Stopped' "$log"; then
    tail -80 "$log"
    exit 1
fi
printf '%s\n' 'Dual subtitles check passed'
