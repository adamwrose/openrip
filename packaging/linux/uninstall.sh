#!/bin/sh
# Removes what install.sh put in place. Use the same PREFIX (or user) you installed with.
set -e
id=io.github.adamwrose.OpenRip
if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
fi
rm -f "$PREFIX/bin/openrip" \
      "$PREFIX/share/applications/$id.desktop" \
      "$PREFIX/share/icons/hicolor/scalable/apps/$id.svg" \
      "$PREFIX/share/metainfo/$id.metainfo.xml"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
echo "OpenRip removed from $PREFIX"
