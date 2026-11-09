#!/bin/sh
# Installs OpenRip. Run as a normal user to install into ~/.local, or as root
# (sudo ./install.sh) to install into /usr/local. Override with PREFIX=/some/dir.
set -e
here=$(cd "$(dirname "$0")" && pwd)
id=io.github.adamwrose.OpenRip

if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
fi

install -Dm755 "$here/openrip" "$PREFIX/bin/openrip"
install -Dm644 "$here/$id.desktop" "$PREFIX/share/applications/$id.desktop"
install -Dm644 "$here/$id.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 "$here/$id.metainfo.xml" "$PREFIX/share/metainfo/$id.metainfo.xml"

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qf "$PREFIX/share/icons/hicolor" 2>/dev/null || true

echo "OpenRip installed to $PREFIX"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: $PREFIX/bin isn't on your PATH." ;; esac
echo
for tool in ffmpeg ffprobe; do
    command -v "$tool" >/dev/null 2>&1 || echo "Missing: $tool. Install FFmpeg with your package manager (apt/dnf/pacman install ffmpeg)."
done
echo "Optional: libbluray and libaacs, plus your own ~/.config/aacs/keydb.cfg, for copy-protected Blu-rays."
