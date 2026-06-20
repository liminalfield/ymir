#!/usr/bin/env sh
# Installs Ymir's desktop entry and icon for the current user. Wayland needs this:
# winit cannot set a window icon at runtime there, so the taskbar/launcher icon comes
# from a desktop entry (matched by app_id "ymir") plus icons in the hicolor theme.
#
# Run from the repo root. Requires ImageMagick (`magick`) to generate icon sizes; the
# KDE taskbar wants a small size, not just the 512 source.
set -eu

src=ymir-icon-512.png
apps="$HOME/.local/share/applications"
theme="$HOME/.local/share/icons/hicolor"

install -Dm644 packaging/ymir.desktop "$apps/ymir.desktop"

for size in 16 24 32 48 64 128 256 512; do
    dir="$theme/${size}x${size}/apps"
    mkdir -p "$dir"
    magick "$src" -resize "${size}x${size}" "$dir/ymir.png"
done

# Refresh the icon-theme and KDE service caches (each is best-effort).
gtk-update-icon-cache "$theme" 2>/dev/null || true
kbuildsycoca6 --noincremental 2>/dev/null || true
update-desktop-database "$apps" 2>/dev/null || true

echo "Installed Ymir's desktop entry and icons."
echo "If the taskbar icon still does not appear, restart the panel:"
echo "    kquitapp6 plasmashell && kstart plasmashell   # (or just log out and back in)"
