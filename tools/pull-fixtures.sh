#!/bin/sh
# Copy the TV's launch points, inputs, their images and the fonts into fixtures/ for the desktop simulator.
# fixtures/ is gitignored: the artwork belongs to the app vendors.
set -eu
TV=${1:-lg-c1}
cd "$(dirname "$0")/.."
mkdir -p fixtures/luna
for m in com.webos.applicationManager/listLaunchPoints com.webos.service.eim/getAllInputStatus; do
  ssh "$TV" "luna-send -n 1 -w 5000 luna://$m '{}' </dev/null" > "fixtures/luna/${m##*/}.json"
done
python3 - > fixtures/paths.txt <<'EOF'
import json
lp = json.load(open("fixtures/luna/listLaunchPoints.json"))["launchPoints"]
ins = json.load(open("fixtures/luna/getAllInputStatus.json"))["devices"]
paths = {p for l in lp for k in ("extraLargeIcon", "mediumLargeIcon", "largeIcon", "icon", "bgImage") if (p := l.get(k))}
paths |= {"/usr/palm/plugins/inputapps/assets/2k/medium/" + d["icon"] for d in ins}
paths |= {"/usr/share/fonts/LG_Smart_UI-Regular.ttf", "/usr/share/fonts/LG_Smart_UI-SemiBold.ttf"}
print("\n".join(sorted(paths)))
EOF
# busybox tar stops at the first missing file, and launch points can name icons that are not installed
ssh "$TV" 'cd / && set -- && while read -r f; do [ -f "$f" ] && set -- "$@" "${f#/}"; done && tar cf - "$@"' < fixtures/paths.txt | tar xf - -C fixtures
echo "fixtures: $(wc -l < fixtures/paths.txt) files"
