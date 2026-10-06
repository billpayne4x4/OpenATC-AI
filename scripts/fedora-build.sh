#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
preview="${1:-plugin}"
packages=(gcc-c++ cmake ninja-build python3 python3-jinja2 openssl-devel libglvnd-opengl)
if [[ "$preview" == "preview" ]]; then
    packages+=(wayland-devel libxkbcommon-devel pkgconf-pkg-config libglvnd-egl)
    desktop=ON
elif [[ "$preview" == "plugin" ]]; then
    desktop=OFF
else
    printf '%s\n' 'Usage: bash scripts/fedora-build.sh [plugin|preview]' >&2
    exit 2
fi
python3 scripts/download_sdk.py
cmake -S . -B build/wayland -G Ninja -DCMAKE_BUILD_TYPE=Debug -DOPENATC_BUILD_DESKTOP="$desktop" -DOPENATC_BUILD_PLUGIN=ON -DXPLANE_SDK="$PWD/vendor/SDK"
cmake --build build/wayland --parallel
ctest --test-dir build/wayland --output-on-failure
python3 scripts/check_linux_dependencies.py build/wayland/lin.xpl build/wayland/open-atc-engine
if [[ "$desktop" == ON ]]; then
    python3 scripts/check_linux_dependencies.py build/wayland/open-atc
fi
cmake --install build/wayland --prefix stage-wayland
