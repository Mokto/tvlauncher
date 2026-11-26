#!/bin/sh
# Runs `takeover.sh boot on|off` against a fake luna-send, ls-control and /var/luna-service2, and checks the key-filter list
# and the early-start files. sh tools/early-test.sh
set -e
H=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/d" "$T/web/init.d"
cp "$H/takeover.sh" "$H/boot.sh" "$T/d/"
echo '[{"handler":"a","file":"/x.js"},{"handler":"b","file":"/y.js"}]' > "$T/list"
cat > "$T/bin/luna-send" <<X
#!/bin/sh
case "\$5" in
*getConfigs) printf '{"subscribed":false,"configs":{"com.webos.surfacemanager.keyFilters":%s},"returnValue":true}\n' "\$(cat $T/list)" ;;
*setConfigs) printf '%s' "\$6" | sed 's/.*keyFilters":\(\[.*\]\)}}\$/\1/' > $T/list; echo '{"returnValue":true}' ;;
esac
X
printf '#!/bin/sh\n' > "$T/bin/ls-control"; chmod +x "$T/bin"/*
# the real script writes /var/lib/webosbrew; point it at the temp dir
sed "s|/var/lib/webosbrew|$T/web|g" "$T/d/takeover.sh" > "$T/d/t.sh"
export PATH="$T/bin:$PATH" EARLY_DIR="$T/e" LS2="$T/ls2"
sh "$T/d/t.sh" boot on
grep -q '"tvlauncherKey"' "$T/list" && grep -q '"handler":"b"' "$T/list" || { echo "filter not added"; exit 1; }
[ -f "$T/ls2/services.d/com.webos.service.tvlauncherearly.service" ] || { echo "service file missing"; exit 1; }
! command -v node >/dev/null || node --check "$T/e/home-filter.js" || { echo "filter script has a syntax error"; exit 1; }
sh "$T/d/t.sh" boot on
[ "$(grep -o tvlauncherKey "$T/list" | wc -l)" -eq 1 ] || { echo "filter added twice"; exit 1; }
sh "$T/d/t.sh" boot off
grep -q tvlauncherKey "$T/list" && { echo "filter not removed"; exit 1; }
[ "$(cat "$T/list")" = '[{"handler":"a","file":"/x.js"},{"handler":"b","file":"/y.js"}]' ] || { echo "list changed"; exit 1; }
[ -e "$T/ls2/services.d/com.webos.service.tvlauncherearly.service" ] && { echo "service file left"; exit 1; }
echo ok
