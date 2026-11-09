#!/bin/sh
# Builds and installs the OpenRip Flatpak locally.
# Needs: flatpak, flatpak-builder, python3 (with aiohttp, tomlkit, PyYAML) and network access.
#   ./build.sh            build and install for the current user
#   ./build.sh bundle     also write OpenRip.flatpak next to this script
set -e
cd "$(dirname "$0")"
id=io.github.adamwrose.OpenRip

# Cargo dependencies must be listed up front because the Flatpak build has no network.
if [ ! -f flatpak-cargo-generator.py ]; then
    curl -sSLf -o flatpak-cargo-generator.py \
        https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
fi
"${PYTHON:-/usr/bin/python3}" flatpak-cargo-generator.py ../../Cargo.lock -o cargo-sources.json

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --install \
    --repo=repo build-dir "$id.yml"

if [ "$1" = bundle ]; then
    flatpak build-bundle repo OpenRip.flatpak "$id"
    echo "Wrote $(pwd)/OpenRip.flatpak"
fi
echo "Run it with: flatpak run $id"
