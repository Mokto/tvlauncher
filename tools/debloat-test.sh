#!/bin/sh
# Runs debloat.sh against fake files and fake systemctl/mount/umount/pkill/luna-send, and checks what it did.
# sh tools/debloat-test.sh
set -e
H=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/sbin" "$T/l"
cp "$H/debloat.sh" "$T/l/"
: > "$T/mounts"; : > "$T/calls"
printf '127.0.0.1 localhost\n' > "$T/hosts"
for b in acr2 adoverlay-service sportsalarm admanager; do echo "real $b" > "$T/sbin/$b"; done
cat > "$T/bin/systemctl" <<X
#!/bin/sh
echo "systemctl \$*" >> $T/calls
X
cat > "$T/bin/pkill" <<X
#!/bin/sh
echo "pkill \$*" >> $T/calls
X
cat > "$T/bin/luna-send" <<X
#!/bin/sh
echo '{"settings":{"smartServiceCountryCode2": "DK"}}'
X
# a bind mount shows the stub's content at the target
cat > "$T/bin/mount" <<X
#!/bin/sh
echo "mount \$*" >> $T/calls
echo " \$3 " >> $T/mounts
[ "\$3" = "$T/hosts" ] || cp "\$2" "\$3"
X
cat > "$T/bin/umount" <<X
#!/bin/sh
echo "umount \$*" >> $T/calls
grep -v " \$1 " $T/mounts > $T/mounts.new; mv $T/mounts.new $T/mounts
echo "restored \$(basename \$1)" > "\$1"
X
chmod +x "$T/bin/"*
export PATH="$T/bin:$PATH" L="$T/l" SBIN="$T/sbin" HOSTS="$T/hosts" MOUNTS="$T/mounts" APPLIED="$T/applied" HOSTS_COPY="$T/hosts-copy"
run() { sh "$T/l/debloat.sh" "$@" >/dev/null; }
ok() { grep -q "$1" "$2" || { echo "FAIL: $3"; cat "$2"; exit 1; }; }
no() { ! grep -q "$1" "$2" || { echo "FAIL: $3"; cat "$2"; exit 1; }; }

# nothing chosen: nothing happens
echo '{"hidden":[]}' > "$T/l/state.json"
run apply
[ ! -s "$T/calls" ] || { echo "FAIL: apply with nothing chosen did something"; exit 1; }

# all four groups
echo '{"debloat":["ads","tracking","logs","suggestions"],"hidden":[]}' > "$T/l/state.json"
run apply
for u in remotelogger service-logger contentminer nudge; do ok "systemctl --no-block stop $u.service" "$T/calls" "$u not stopped"; done
for b in acr2 adoverlay-service sportsalarm; do cmp -s "$T/l/stub-noop" "$T/sbin/$b" || { echo "FAIL: $b not stubbed"; exit 1; }; done
cmp -s "$T/sbin/admanager" "$T/l/stub-noop" && { echo "FAIL: admanager must not be stubbed"; exit 1; }
ok "127.0.0.1 eic.ads.lgtvcommon.com .* # tvlauncher-debloat" "$T/hosts" "ad hosts missing"
ok "dk.ad.lgsmartad.com" "$T/hosts" "country host missing"
ok "localhost" "$T/hosts" "existing hosts line lost"
[ "$(grep -c tvlauncher-debloat "$T/hosts")" = 2 ] || { echo "FAIL: expected an IPv4 and an IPv6 line"; exit 1; }

# a second apply changes nothing
: > "$T/calls"; run apply
[ ! -s "$T/calls" ] || { echo "FAIL: second apply was not a no-op"; cat "$T/calls"; exit 1; }
[ "$(grep -c tvlauncher-debloat "$T/hosts")" = 2 ] || { echo "FAIL: hosts lines duplicated"; exit 1; }

# dropping logs and ads restores only those
echo '{"debloat":["tracking","suggestions"]}' > "$T/l/state.json"
: > "$T/calls"; run apply
ok "systemctl --no-block start remotelogger.service" "$T/calls" "logs not restarted"
no "contentminer" "$T/calls" "suggestions touched"
no tvlauncher-debloat "$T/hosts" "ad hosts not removed"
ok "restored adoverlay-service" "$T/sbin/adoverlay-service" "adoverlay stub not removed"
cmp -s "$T/sbin/acr2" "$T/l/stub-noop" || { echo "FAIL: acr2 stub dropped"; exit 1; }

# off puts everything back and never touches a real file
run off
ok "restored acr2" "$T/sbin/acr2" "acr2 stub not removed"
ok "restored sportsalarm" "$T/sbin/sportsalarm" "sportsalarm stub not removed"
[ "$(cat "$T/sbin/admanager")" = "real admanager" ] || { echo "FAIL: admanager changed"; exit 1; }
[ ! -s "$T/applied" ] || { echo "FAIL: applied list not empty: $(cat "$T/applied")"; exit 1; }
: > "$T/calls"; run off
[ ! -s "$T/calls" ] || { echo "FAIL: off twice did something"; exit 1; }

# read-only /etc/hosts: a RAM copy is mounted over it, and removed again
chmod a-w "$T/hosts"
if [ -w "$T/hosts" ]; then echo "(running as root: the read-only hosts case is skipped)"; else
  echo '{"debloat":["ads"]}' > "$T/l/state.json"; run apply
  ok "mount --bind $T/hosts-copy $T/hosts" "$T/calls" "hosts not mounted over"
  ok tvlauncher-debloat "$T/hosts-copy" "copy lacks the lines"
fi
echo "debloat tests passed"
