#!/bin/sh
# hbr.sh: the kernel module that lets the film player send TrueHD (Atmos) and full-rate DTS-HD to a receiver
# (vendor/earc_route, GPL-2.0-only, a separate program that is never linked).
#
#   install   copy the module and its loader to /var/lib/webosbrew/tvlauncher-hbr, hook this script into Homebrew
#             Channel's boot scripts, then run `boot`
#   boot      load the module (its loader checks the running kernel first), set its HBR parameters, and make the
#             lease node /dev/earc_route that the film player opens
#   remove    undo `install`; the module stays loaded, switched off, until the next reboot
#   status    one line: installed / active
#
# Runs as root. Prints one result line. The module and loader are looked for beside this script's package
# (DIR/lib/earc_route-4.4.ko, DIR/hbr/earc-route-load.sh).
APP=/media/developer/tvlauncher
D=/var/lib/webosbrew/tvlauncher-hbr
HOOK=/var/lib/webosbrew/init.d/30-tvlauncher-hbr
P=/sys/module/earc_route/parameters

node() {
    minor=$(cut -d: -f2 /sys/class/misc/earc_route/dev 2>/dev/null) || return 1
    rm -f /dev/earc_route
    mknod /dev/earc_route c 10 "$minor" && chmod 666 /dev/earc_route
}

case "$1" in
install)
    [ "$(id -u)" = 0 ] || { echo "hbr: REFUSED: not root"; exit 1; }
    [ -f "$APP/hbr/earc_route-4.4.ko" ] || { echo "hbr: REFUSED: no module in $APP/hbr"; exit 1; }
    mkdir -p "$D" /var/lib/webosbrew/init.d || exit 1
    cp "$APP/hbr/earc_route-4.4.ko" "$APP/hbr/earc-route-load.sh" "$APP/hbr/hbr.sh" "$D/" || exit 1
    printf '#!/bin/sh\nexec sh %s/hbr.sh boot\n' "$D" > "$HOOK" && chmod 755 "$HOOK" || exit 1
    exec sh "$D/hbr.sh" boot
    ;;
boot)
    if [ ! -e /sys/module/earc_route ]; then
        out=$(sh "$D/earc-route-load.sh" --dts-hd </dev/null 2>&1) || { echo "hbr: REFUSED: $(echo "$out" | tail -n 1)"; exit 1; }
    fi
    [ -w "$P/es_ch" ] || { echo "hbr: REFUSED: another earc_route build is loaded; reboot without it"; exit 1; }
    echo 8 > "$P/es_ch"; echo 192000 > "$P/es_rate"; echo 1 > "$P/es_out"; echo 6 > "$P/arc_type"
    echo 1 > "$P/enabled"
    node || { echo "hbr: REFUSED: no /dev/earc_route"; exit 1; }
    echo "hbr: active"
    ;;
remove)
    rm -f "$HOOK"; rm -rf "$D"
    rm -f /dev/earc_route
    [ -w "$P/enabled" ] && echo 0 > "$P/enabled"
    echo "hbr: removed"
    ;;
status)
    if [ -x "$HOOK" ] && [ "$(cat "$P/enabled" 2>/dev/null)" = 1 ]; then echo "hbr: active"
    elif [ -x "$HOOK" ]; then echo "hbr: installed, not active (restart the TV)"
    else echo "hbr: not installed"; fi
    ;;
*)
    echo "usage: $0 install | boot | remove | status"; exit 2
    ;;
esac
