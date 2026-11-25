#!/bin/sh
# Installs tvlauncher on a rooted LG TV (webOS with the Homebrew Channel's SSH) from this folder.
# Usage: ./install.sh root@<tv-ip> [on]      "on" also makes it Home now (until the next reboot).
# An update while it is Home restarts it. Start at every boot is chosen later, in the launcher's Settings.
set -e
TV=${1:?usage: ./install.sh root@<tv-ip> [on]}
HERE=$(cd "$(dirname "$0")" && pwd)
DEST=/media/developer/tvlauncher
for f in tvlauncher takeover.sh debloat.sh boot.sh; do
  [ -f "$HERE/$f" ] || { echo "$f is missing next to install.sh. A source checkout has no binary: use 'make deploy'."; exit 1; }
done
ssh "$TV" "mkdir -p $DEST && rm -f $DEST/tvlauncher.new"
scp -q "$HERE/tvlauncher" "$TV:$DEST/tvlauncher.new"
scp -q "$HERE/takeover.sh" "$HERE/debloat.sh" "$HERE/boot.sh" "$TV:$DEST/"
# the film player's FFmpeg libraries
if [ -d "$HERE/lib" ]; then
  ssh "$TV" "mkdir -p $DEST/lib"
  scp -q "$HERE"/lib/*.so.* "$TV:$DEST/lib/"
fi
# the receiver support of the film player (Settings, Player)
if [ -d "$HERE/hbr" ]; then
  ssh "$TV" "mkdir -p $DEST/hbr"
  scp -q "$HERE"/hbr/* "$TV:$DEST/hbr/"
fi
ssh "$TV" "chmod 755 $DEST/tvlauncher.new $DEST/takeover.sh $DEST/boot.sh && mv $DEST/tvlauncher.new $DEST/tvlauncher"
# the mount pins the old file, so a running launcher is switched off and on again
if ssh "$TV" "[ -f /tmp/tvlauncher-home ]"; then
  ssh "$TV" "sh $DEST/takeover.sh off; sh $DEST/takeover.sh on"
  echo "Updated and restarted."
elif [ "$2" = on ]; then
  ssh "$TV" "sh $DEST/takeover.sh on"
  echo "Done: the launcher is Home until the next reboot."
else
  echo "Installed. Try it with: ssh $TV sh $DEST/takeover.sh on    (undo: ... off, or reboot)"
fi
