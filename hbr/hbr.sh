#!/bin/sh
# hbr.sh: the launcher's one-line view of earc-route-hbr.sh, the HBR installer of the earc_route release
# (https://github.com/Mokto/earc_route, GPL-2.0-only, a separate program that is never linked) in this directory.
#
#   install | remove | status      each prints one line: `hbr: active`, `hbr: removed`, `hbr: REFUSED: <why>`,
#                                  `hbr: installed, not active (restart the TV)` or `hbr: not installed`
#
# The launcher is not sandboxed, so the lease node goes in the host's /dev only. Runs as root.
H=$(cd "$(dirname "$0")" && pwd)/earc-route-hbr.sh
# the module and hook of the versions that shipped their own copy of this installer
old() { rm -f /var/lib/webosbrew/init.d/30-tvlauncher-hbr; rm -rf /var/lib/webosbrew/tvlauncher-hbr; }

case "$1" in
install) old; exec sh "$H" install ;;
remove) old; exec sh "$H" remove ;;
status)
    s=$(sh "$H" status </dev/null)
    case "$s" in
    *installed=0*) echo "hbr: not installed" ;;
    *enabled=1*) echo "hbr: active" ;;
    *) echo "hbr: installed, not active (restart the TV)" ;;
    esac
    ;;
*) echo "usage: $0 install | remove | status"; exit 2 ;;
esac
