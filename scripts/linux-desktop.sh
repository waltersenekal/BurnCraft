#!/bin/sh
# Installs BurnCraft's menu entry and icon for the current user, so the desktop shows the BurnCraft
# icon instead of a generic one, and BurnCraft appears in the application menu.
#
#   sh scripts/linux-desktop.sh                  # install
#   sh scripts/linux-desktop.sh --uninstall      # remove again
#   sh scripts/linux-desktop.sh path/to/burncraft
#
# The work is done by the program itself (`burncraft --install-desktop`), which carries its own icon
# and writes the same files; this wrapper only spares you the path to the binary when working from a
# source checkout. Wayland has no way for a window to carry its own icon: the compositor looks up
# the .desktop file whose name matches the window's app id (org.burncraft.BurnCraft, set in
# main.rs). Without that file the window gets the compositor's placeholder icon.
set -e
cd "$(dirname "$0")/.."

BIN=target/release/burncraft
FLAG=--install-desktop
case "$1" in
    --uninstall) FLAG=--uninstall-desktop; shift ;;
    "") ;;
    -*) echo "Unknown option: $1" >&2; echo "Use --uninstall, or give the path to the BurnCraft binary." >&2; exit 2 ;;
    *) BIN=$1; shift ;;
esac

if [ ! -x "$BIN" ]; then
    echo "$BIN does not exist yet. Build it first: cargo build --release -p burncraft" >&2
    exit 1
fi

exec "$BIN" "$FLAG"
