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

# Home started by the app manager is running, or this launches it, unless the TV started on an input or an app.
start_home_if_free() {
  i=0
  while pgrep -f "^$EXE" >/dev/null && [ $i -lt 12 ]; do usleep 250000; i=$((i + 1)); done
  usleep 500000
  pgrep -f "^$EXE" >/dev/null && return 0
  fg=$(luna luna://com.webos.applicationManager/getForegroundAppInfo '{}' | sed -n 's/.*"appId" *: *"\([^"]*\)".*/\1/p')
  if [ -z "$fg" ] || [ "$fg" = com.webos.app.home ]; then
    luna luna://com.webos.applicationManager/launch '{"id":"com.webos.app.home"}' >/dev/null
  fi
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

# Early start. LG's Home is up at about 18 s into a boot, but webosbrew's hooks only run at 35 s or later, so the boot hook alone
# leaves LG's Home on screen for ~20 s. Two things run earlier (the idea is Luma's, see THIRD-PARTY-NOTICES.md): ls-hubd reads
# /var/luna-service2 at core-boot-done (~13 s) and starts a dynamic service as root on the first call, and LG's compositor loads
# a key-filter script (the key filters in configd) within seconds of boot. The script calls the service, whose program is
# $EARLY_SH: it runs `takeover.sh early`. Everything is best effort; boot.sh at ~35 s is still there as the fallback.
EARLY_NAME=com.webos.service.tvlauncherearly
EARLY_DIR=${EARLY_DIR:-/var/lib/tvlauncher}
EARLY_SH=$EARLY_DIR/early.sh
EARLY_JS=$EARLY_DIR/home-filter.js
LS2=${LS2:-/var/luna-service2}
KEYFILTERS=com.webos.surfacemanager.keyFilters
configs() { luna-send -n 1 -w 5000 luna://com.webos.service.config/"$1" "$2" </dev/null; }
# The filter list as one JSON line, or nothing when configd does not answer.
filters_get() {
  configs getConfigs "{\"configNames\":[\"$KEYFILTERS\"]}" | tr -d '\n' |
    sed -n "s/.*\"$KEYFILTERS\": *\(\[[^]]*\]\).*/\1/p"
}
# Our entry out of the list, and added again when $1 is on.
filters_set() {
  list=$(filters_get)
  [ -n "$list" ] || return 1
  new=$(printf '%s' "$list" | sed 's/, *{[^}]*tvlauncherKey[^}]*}//g; s/{[^}]*tvlauncherKey[^}]*} *,\{0,1\}//g')
  [ "$1" = on ] && new=$(printf '%s' "$new" | sed "s|\]\$|,{\"handler\":\"tvlauncherKey\",\"file\":\"$EARLY_JS\"}]|")
  [ "$new" = "$list" ] || configs setConfigs "{\"configs\":{\"$KEYFILTERS\":$new}}" >/dev/null
}
early_on() {
  mkdir -p $EARLY_DIR $LS2/services.d $LS2/roles.d $LS2/api-permissions.d $LS2/manifests.d || return 1
  printf '#!/bin/sh\nexec sh "%s/takeover.sh" early\n' "$D" > $EARLY_SH && chmod 755 $EARLY_SH
  cat > $EARLY_JS <<'EOF2'
/* tvlauncher: asks the early-start service to make the launcher Home while the TV boots. Keys pass through unchanged. */
var tvlauncherBooting = false;
var tvlauncherTries = 0;
function tvlauncherPing() {
  if (!tvlauncherBooting || tvlauncherTries >= 20) return;
  tvlauncherTries++;
  genericTimer.set(2000, tvlauncherPing);
  lunaCall.send('com.webos.service.tvlauncherearly', '/prepare', '{}', function () {});
}
function init() {
  try {
    lunaCall.subscribe('com.webos.bootManager', '/getBootStatus', '{}', function (payload) {
      try {
        var r = typeof payload === 'string' ? JSON.parse(payload) : payload;
        if (!r || !r.returnValue || !r.signals) return;
        if (r.signals['boot-done']) tvlauncherBooting = false;
        else if (!tvlauncherTries && !tvlauncherBooting) { tvlauncherBooting = true; tvlauncherPing(); }
      } catch (e) { console.warn('tvlauncher filter: ' + e); }
    }, true);
  } catch (e) { console.warn('tvlauncher filter: ' + e); }
}
function tvlauncherKey() { return KeyPolicy.NextPolicy; }
EOF2
  printf '[D-BUS Service]\nName=%s\nExec=%s\nType=dynamic\n' $EARLY_NAME $EARLY_SH > $LS2/services.d/$EARLY_NAME.service
  cat > $LS2/roles.d/$EARLY_NAME.json <<EOF2
{"exeName":"$EARLY_SH","type":"regular","allowedNames":["$EARLY_NAME"],"permissions":[{"service":"$EARLY_NAME","inbound":["com.webos.starfish.keyfilters"],"outbound":[]}]}
EOF2
  echo "{\"all\":[\"$EARLY_NAME/prepare\"]}" > $LS2/api-permissions.d/$EARLY_NAME.json
  cat > $LS2/manifests.d/$EARLY_NAME.json <<EOF2
{"id":"$EARLY_NAME","version":"1.0.0","serviceFiles":["$LS2/services.d/$EARLY_NAME.service"],"roleFiles":["$LS2/roles.d/$EARLY_NAME.json"],"apiPermissionFiles":["$LS2/api-permissions.d/$EARLY_NAME.json"]}
EOF2
  ls-control scan-volatile-dirs >/dev/null 2>&1
  filters_set on
}
early_off() {
  filters_set off
  rm -f $LS2/services.d/$EARLY_NAME.service $LS2/roles.d/$EARLY_NAME.json $LS2/api-permissions.d/$EARLY_NAME.json \
    $LS2/manifests.d/$EARLY_NAME.json $EARLY_SH $EARLY_JS
  ls-control scan-volatile-dirs >/dev/null 2>&1
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
    early_on || echo "the early start could not be set up; Home starts at the webosbrew hook instead"
    ;;
  off)
    rm -f $HOOK
    early_off
    if [ -f "$MADE" ]; then rm -f $CYTV/DISABLE "$MADE"; fi
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
early)
  # From the early service, as root, ~15 s into a boot: only the mount and stopping LG's Home (the app manager starts it again,
  # now as the launcher). Opens nothing, so an input the TV starts on stays. Silent exit when the boot option is not on.
  exec 9>/tmp/tvlauncher-early.lock
  flock -w 10 9 || exit 1
  [ -f $MARK ] && exit 0
  [ -e "$D/NOBOOT" ] || [ ! -L $HOOK ] || [ ! -x "$D/tvlauncher" ] && exit 1
  mount --bind "$D/tvlauncher" $EXE || exit 1
  role_on
  touch $MARK
  rm -f $CRASHES
  (sh "$D/takeover.sh" watch </dev/null >/dev/null 2>&1 &)
  pkill -f "^$EXE"
  start_home_if_free
  echo "$(date): early start, uptime $(cut -d' ' -f1 /proc/uptime)" >> /tmp/tvlauncher-early.log
  ;;
on)
  if [ -f $MARK ]; then
    # boot.sh after the early start: only what the early start leaves out
    [ -n "$BOOT" ] && { pkill -f "[c]leanyourtv/watchdog.sh"; start_home_if_free; [ -f "$D/debloat.sh" ] && sh "$D/debloat.sh" apply </dev/null >/dev/null 2>&1; exit 0; }
    echo "already Home; run off first"; exit 1
  fi
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
