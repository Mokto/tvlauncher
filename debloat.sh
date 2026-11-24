#!/bin/sh
# Stop LG's ad and telemetry services while tvlauncher is Home, and put them back afterwards. RAM-only: a reboot restores everything.
# Adapted from CleanYourTV's debloat-stop.sh / debloat-undo.sh, https://github.com/lasharor/cleanyourtv-WebOS
# (MIT licence, Copyright (c) 2026 cleanyourtv-WebOS contributors; the licence text is at the repository's LICENSE).
#
#   sh debloat.sh apply        match the groups chosen in the launcher's settings (state.json, "debloat")
#   sh debloat.sh on [group..] start blocking these groups (default: the chosen ones)
#   sh debloat.sh off [group..] stop blocking (default: every group applied so far)
#
# Groups:
#   ads          adoverlay-service and the ad servers in /etc/hosts. Not admanager: HBO Max and Disney+ wait for it at start.
#   tracking     acr2 (what is on screen, reported by LivePlus)
#   logs         remotelogger, service-logger
#   suggestions  contentminer, nudge, sportsalarm
#
# A luna dynamic service is kept from starting by a bind-mounted do-nothing stub over its program; the real file is untouched.
# The paths can be overridden so tools/debloat-test.sh can run this against fake files.
export PATH=$PATH:/usr/bin:/usr/sbin:/bin:/sbin
L=${L:-$(cd "$(dirname "$0")" && pwd)}
SBIN=${SBIN:-/usr/sbin}
HOSTS=${HOSTS:-/etc/hosts}
MOUNTS=${MOUNTS:-/proc/mounts}
APPLIED=${APPLIED:-/tmp/tvlauncher-debloat}
HOSTS_COPY=${HOSTS_COPY:-/tmp/tvlauncher-hosts}
STUB=$L/stub-noop
MARK='# tvlauncher-debloat'
ALL="ads tracking logs suggestions"

units_of() {
  case $1 in logs) echo "remotelogger service-logger" ;; suggestions) echo "contentminer nudge" ;; esac
}
bins_of() {
  case $1 in ads) echo adoverlay-service ;; tracking) echo acr2 ;; suggestions) echo sportsalarm ;; esac
}

chosen() {
  sed -n 's/.*"debloat": *\[\([^]]*\)\].*/\1/p' "$L/state.json" 2>/dev/null | tr ',' ' ' | tr -d '"'
}

applied() { cat "$APPLIED" 2>/dev/null; }
has() { case " $1 " in *" $2 "*) return 0 ;; esac; return 1; }
mark() { { applied | tr ' ' '\n' | grep -vx "$1"; [ "$2" = on ] && echo "$1"; } | grep . | tr '\n' ' ' > "$APPLIED.new"; mv "$APPLIED.new" "$APPLIED"; }

ad_hosts() {
  h="eic.ads.lgtvcommon.com nl.ad.lgsmartad.com nl.info.lgsmartad.com"
  # lgsmartad.com has a server per country and admanager asks the one of the TV's service country: refuse that one at once too
  cc=$(luna-send -n 1 -w 5000 luna://com.webos.settingsservice/getSystemSettings '{"category":"option","keys":["smartServiceCountryCode2"]}' </dev/null 2>/dev/null |
    sed -n 's/.*"smartServiceCountryCode2": *"\([A-Za-z][A-Za-z]\)".*/\1/p' | tr 'A-Z' 'a-z')
  [ -n "$cc" ] && [ "$cc" != nl ] && h="$h $cc.ad.lgsmartad.com $cc.info.lgsmartad.com"
  echo "$h"
}

hosts_on() {
  grep -q "$MARK" "$HOSTS" && return
  h=$(ad_hosts)
  lines() { echo "127.0.0.1 $h $MARK"; echo "::1 $h $MARK"; }
  if [ -w "$HOSTS" ] && lines >> "$HOSTS" 2>/dev/null; then
    return
  fi
  # /etc/hosts can be on the read-only system partition: a copy in RAM is mounted over it until the next restart
  cp "$HOSTS" "$HOSTS_COPY" && chmod u+w "$HOSTS_COPY" && lines >> "$HOSTS_COPY" && mount --bind "$HOSTS_COPY" "$HOSTS" && touch "$HOSTS_COPY.mounted" ||
    echo "hosts: $HOSTS cannot be changed on this TV; the ad servers are not blocked"
}

hosts_off() {
  # Only our own mount: a table mounted over it later (the Homebrew Channel's update block) is not ours to unmount.
  if [ -e "$HOSTS_COPY.mounted" ] && [ "$HOSTS" -ef "$HOSTS_COPY" ]; then
    umount "$HOSTS" 2>/dev/null && rm -f "$HOSTS_COPY" "$HOSTS_COPY.mounted"
  fi
  if grep -q "$MARK" "$HOSTS"; then
    grep -v "$MARK" "$HOSTS" > "$HOSTS.new" && cat "$HOSTS.new" > "$HOSTS"
    rm -f "$HOSTS.new"
  fi
}

group_on() {
  printf '#!/bin/sh\nexit 0\n' > "$STUB"; chmod +x "$STUB"
  # --no-block: contentminer can take a minute to stop
  for s in $(units_of "$1"); do systemctl --no-block stop "$s.service" 2>&1 | sed "s/^/$s: /"; done
  for b in $(bins_of "$1"); do
    [ -e "$SBIN/$b" ] || continue
    grep -q " $SBIN/$b " "$MOUNTS" || mount --bind "$STUB" "$SBIN/$b"
    pkill -9 -f "^$SBIN/$b"
  done
  [ "$1" = ads ] && hosts_on
  mark "$1" on
}

group_off() {
  for b in $(bins_of "$1"); do
    # only a stub: the same content check CleanYourTV uses
    while grep -q " $SBIN/$b " "$MOUNTS" && cmp -s "$SBIN/$b" "$STUB"; do umount "$SBIN/$b" 2>/dev/null || break; done
  done
  for s in $(units_of "$1"); do systemctl --no-block start "$s.service"; done
  [ "$1" = ads ] && hosts_off
  mark "$1" off
}

cmd=$1; shift
case $cmd in
apply)
  want=$(chosen); now=$(applied)
  for g in $ALL; do
    if has "$want" "$g"; then has "$now" "$g" || group_on "$g"; else has "$now" "$g" && group_off "$g"; fi
  done
  true
  ;;
on) for g in ${*:-$(chosen)}; do group_on "$g"; done ;;
off) for g in ${*:-$(applied)}; do group_off "$g"; done ;;
*) echo "usage: $0 apply | on [group..] | off [group..]"; exit 2 ;;
esac
