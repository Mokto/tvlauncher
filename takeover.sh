#!/bin/sh
# Make tvlauncher LG's Home until the next reboot (on), or put back what was Home before (off).
# RAM-only: a bind mount over LG's Home program, the same way CleanYourTV's stand-in works on webOS 6.
# The app manager then starts tvlauncher as com.webos.app.home, as root and outside the app jail.
export PATH=/usr/bin:/usr/sbin:/bin:/sbin:$PATH
D=$(cd "$(dirname "$0")" && pwd)
EXE=/usr/bin/com.webos.app.home
MARK=/tmp/tvlauncher-home
CRASHES=/tmp/tvlauncher-crashes
WATCH_PID=/tmp/tvlauncher-watch.pid
WATCH_LOG=/tmp/tvlauncher-watch.log
CYTV=/media/developer/cleanyourtv
luna() { luna-send -n 1 -w 5000 "$@" </dev/null; }
# The app manager starts Home again as soon as it exits, from whatever file is mounted at that moment.
stop_home() {
  pkill -f "^$EXE"
  sleep 2
}

# Crashes of the launcher in the last minute: it appends the time (seconds) of each one to $CRASHES.
crashes() {
  n=0
  [ -f $CRASHES ] || { echo 0; return; }
  now=$(date +%s)
  while read -r t; do
    [ -n "$t" ] && [ $((now - t)) -le 60 ] && n=$((n + 1))
  done < $CRASHES
  echo $n
}

# LG's role for Home's program, plus the bus names LG's media player library registers (those of Native Jelly's
# role): without them the live TV player cannot start.
ROLES=/usr/share/ls2/roles
ROLE=/tmp/tvlauncher-home-role.json
role_on() {
  [ -f $ROLES/pub/com.webos.app.home.json ] || return 0
  cat > $ROLE <<'EOF'
{
    "role": {
        "exeName": "/usr/bin/com.webos.app.home",
        "type": "privileged",
        "allowedNames": ["", "com.webos.app.home", "com.webos.media.client.*", "com.webos.rm.client.*", "com.webos.pipeline.*",
            "com.webos.pipe.avmuteclient.*", "com.webos.vt.*", "com.webos.chromium.media.*", "com.webos.avconnector.*"]
    },
    "permissions": [
        {"service": "com.webos.app.home", "inbound": ["*"], "outbound": ["*"]},
        {"service": "", "inbound": ["com.webos.service.videosinkmanager", "com.webos.service.tv.display"],
            "outbound": ["com.webos.service.videosinkmanager", "com.webos.service.tv.display"]},
        {"service": "com.webos.media.client.*", "inbound": ["com.webos.media"], "outbound": ["com.webos.media"]},
        {"service": "com.webos.rm.client.*", "inbound": ["com.webos.media"], "outbound": ["com.webos.media"]},
        {"service": "com.webos.pipeline.*", "inbound": ["com.webos.pipelinectrl.*"], "outbound": ["com.webos.pipelinectrl.*"]},
        {"service": "com.webos.pipe.avmuteclient.*", "inbound": [], "outbound": ["com.webos.service.avmutemanager"]},
        {"service": "com.webos.vt.*", "inbound": ["*"], "outbound": ["*"]},
        {"service": "com.webos.chromium.media.*", "inbound": [], "outbound": ["*"]},
        {"service": "com.webos.avconnector.*", "inbound": ["com.webos.service.videooutput", "com.webos.service.audio"],
            "outbound": ["com.webos.service.videooutput", "com.webos.service.audio", "com.webos.service.utp.extinputs"]}
    ]
}
EOF
  for d in pub prv; do
    [ -f $ROLES/$d/com.webos.app.home.json ] && mount --bind $ROLE $ROLES/$d/com.webos.app.home.json
  done
  ls-control scan-services >/dev/null 2>&1
}
role_off() {
  for d in pub prv; do
    grep -q "$ROLES/$d/com.webos.app.home.json" /proc/mounts && umount $ROLES/$d/com.webos.app.home.json
  done
  ls-control scan-services >/dev/null 2>&1
}

HOOK=/var/lib/webosbrew/init.d/20-tvlauncher-home
MADE=$D/made-cytv-disable
case "$1" in
boot)
  # Start as Home at every boot (webosbrew's init.d runs boot.sh). CleanYourTV, when installed, would put its own Home back at
  # boot, so its own switch for LG's Home (a DISABLE file) is set, and removed again only if we set it.
  case "$2" in
  on)
    [ -d /var/lib/webosbrew ] || { echo "webosbrew (the Homebrew Channel's startup) is missing"; exit 1; }
    mkdir -p /var/lib/webosbrew/init.d
    chmod 755 "$D/boot.sh"
    if [ -d $CYTV ] && [ ! -e $CYTV/DISABLE ]; then touch $CYTV/DISABLE && touch "$MADE"; fi
    ln -sf "$D/boot.sh" $HOOK
    ;;
  off)
    rm -f $HOOK
    [ -f "$MADE" ] && { rm -f $CYTV/DISABLE "$MADE"; }
    ;;
  status) [ -L $HOOK ] ;;
  *) echo "usage: $0 boot on|off|status"; exit 2 ;;
  esac
  ;;
watch)
  # Started by `on`. Three crashes within a minute put LG's Home back, or the Home key would stay dead.
  echo $$ > $WATCH_PID
  while sleep 2; do
    [ -f $MARK ] || exit 0
    if [ "$(crashes)" -ge 3 ]; then
      echo "$(date): the launcher crashed three times in a minute; LG's Home is back" >> $WATCH_LOG
      WATCHER=1 sh "$D/takeover.sh" off
      exit 0
    fi
  done
  ;;
on)
  [ -f $MARK ] && { echo "already Home; run off first"; exit 1; }
  [ -x "$D/tvlauncher" ] || { echo "$D/tvlauncher missing"; exit 1; }
  # CleanYourTV's watchdog would see its stand-in gone and put its own Home back
  pkill -f "[c]leanyourtv/watchdog.sh"
  mount --bind "$D/tvlauncher" $EXE || exit 1
  role_on
  touch $MARK
  rm -f $CRASHES
  (sh "$D/takeover.sh" watch </dev/null >/dev/null 2>&1 &)
  # whatever runs as Home now: CleanYourTV's stand-in and the web launcher it opened, or LG's Home
  pkill -f "^/bin/sh $EXE"
  pkill -f "[l]una-send -i -a com.webos.app.home"
  luna luna://com.palm.webappmanager/killApp '{"appId":"com.webos.app.home"}' >/dev/null
  stop_home
  # At boot the TV may start on an input or an app: Home must not cover that
  fg=$(luna luna://com.webos.applicationManager/getForegroundAppInfo '{}' | sed -n 's/.*"appId" *: *"\([^"]*\)".*/\1/p')
  if [ -z "$BOOT" ] || [ -z "$fg" ] || [ "$fg" = com.webos.app.home ]; then
    luna luna://com.webos.applicationManager/launch '{"id":"com.webos.app.home"}'
  fi
  # the ads and telemetry groups chosen in the launcher's settings (all off until one is switched on)
  [ -f "$D/debloat.sh" ] && sh "$D/debloat.sh" apply </dev/null >/dev/null 2>&1
  ;;
off)
  [ -f $MARK ] || { echo "not Home"; exit 1; }
  [ -n "$WATCHER" ] || { [ -f $WATCH_PID ] && kill "$(cat $WATCH_PID)" 2>/dev/null; }
  rm -f $WATCH_PID
  # lazy: the running launcher keeps its file busy until it is stopped below
  umount -l $EXE || { echo "umount $EXE failed"; exit 1; }
  role_off
  rm -f $MARK
  # LG's own Home needs the services debloat.sh stops
  [ -f "$D/debloat.sh" ] && sh "$D/debloat.sh" off </dev/null >/dev/null 2>&1
  stop_home
  [ -f $CYTV/watchdog.sh ] && ! pgrep -f "[c]leanyourtv/watchdog.sh" >/dev/null &&
    (sh $CYTV/watchdog.sh </dev/null >/dev/null 2>&1 &)
  sleep 1
  luna luna://com.webos.applicationManager/launch '{"id":"com.webos.app.home"}'
  ;;
*) echo "usage: $0 on|off"; exit 2 ;;
esac
