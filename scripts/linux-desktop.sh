#!/bin/sh
# Installs BurnCraft's menu entry and icon for the current user, so the desktop shows the BurnCraft
# icon instead of a generic one, and BurnCraft appears in the application menu.
#
#   sh scripts/linux-desktop.sh                  # install
#   sh scripts/linux-desktop.sh --uninstall      # remove again
#
# Wayland has no way for a window to carry its own icon: the compositor looks up the .desktop file
# whose name matches the window's app id (org.burncraft.BurnCraft, set in main.rs). Without that
# file the window gets the compositor's placeholder icon.
set -e
cd "$(dirname "$0")/.."

APP_ID=org.burncraft.BurnCraft
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
APPS="$DATA/applications"
ICONS="$DATA/icons/hicolor"
ENTRY="$APPS/$APP_ID.desktop"

refresh_caches() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" >/dev/null 2>&1 || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$ICONS" >/dev/null 2>&1 || true
    command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
    command -v kbuildsycoca5 >/dev/null 2>&1 && kbuildsycoca5 --noincremental >/dev/null 2>&1 || true
}

remove_icons() {
    for dir in "$ICONS"/*/apps; do
        [ -d "$dir" ] || continue
        rm -f "$dir/$APP_ID.png"
    done
}

if [ "$1" = "--uninstall" ]; then
    rm -f "$ENTRY"
    remove_icons
    refresh_caches
    echo "Removed the BurnCraft menu entry and icon."
    exit 0
fi

BIN=${1:-target/release/burncraft}
if [ ! -x "$BIN" ]; then
    echo "$BIN does not exist yet. Build it first: cargo build --release -p burncraft" >&2
    exit 1
fi
BIN=$(readlink -f "$BIN")

# The icon, at the sizes panels and menus ask for.
mkdir -p "$APPS"
remove_icons
if command -v magick >/dev/null 2>&1; then
    RESIZE=magick
elif command -v convert >/dev/null 2>&1; then
    RESIZE=convert
else
    RESIZE=
fi
if [ -n "$RESIZE" ]; then
    for size in 16 24 32 48 64 128 256; do
        mkdir -p "$ICONS/${size}x${size}/apps"
        "$RESIZE" assets/icon.png -resize "${size}x${size}" -strip "$ICONS/${size}x${size}/apps/$APP_ID.png"
    done
else
    # No ImageMagick: the 256 px icon on its own is enough to be found and scaled down.
    mkdir -p "$ICONS/256x256/apps"
    cp assets/icon.png "$ICONS/256x256/apps/$APP_ID.png"
fi

cat > "$ENTRY" <<DESKTOP
[Desktop Entry]
Type=Application
Name=BurnCraft
GenericName=Laser cutter software
Comment=Design, prepare and run jobs on laser cutters and engravers
Exec=$BIN
Icon=$APP_ID
Terminal=false
Categories=Graphics;Engineering;
Keywords=laser;lasercut;engrave;cut;svg;dxf;
StartupWMClass=$APP_ID
DESKTOP

refresh_caches
echo "Installed $ENTRY"
echo "Launch BurnCraft again (or start it from the menu) to see it with its icon."
