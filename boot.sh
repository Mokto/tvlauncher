#!/bin/sh
# Linked into /var/lib/webosbrew/init.d by `make boot`: makes tvlauncher Home at every boot.
# Create $D/NOBOOT to skip it (SSH in and `touch`), or run `make unboot` from the Mac.
export PATH=/usr/bin:/usr/sbin:/bin:/sbin:$PATH
D=/media/developer/tvlauncher
[ -e $D/NOBOOT ] && exit 0
[ -x $D/tvlauncher ] || exit 0
(
  # run-parts waits for this script, and Home can only be switched once the app manager answers
  for i in $(seq 1 60); do
    luna-send -n 1 -w 3000 luna://com.webos.applicationManager/listLaunchPoints '{}' </dev/null 2>/dev/null | grep -q launchPoints && break
    sleep 2
  done
  sleep 5
  BOOT=1 sh $D/takeover.sh on
) </dev/null >/tmp/tvlauncher-boot.log 2>&1 &
exit 0
