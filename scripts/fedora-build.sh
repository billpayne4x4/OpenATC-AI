#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-plugin}"
if [[ "$mode" != plugin && "$mode" != preview ]]; then
    printf '%s\n' 'Usage: bash scripts/fedora-build.sh [plugin|preview]' >&2
    exit 2
fi
cargo build --locked --release -p openatc-plugin -p openatc-engine
cargo test --locked -p openatc-core -p openatc-engine -p openatc-ui -p openatc-plugin
python3 scripts/test-radio-engine.py target/release/open-atc-engine speech
python3 scripts/check_linux_dependencies.py target/release/libopenatc_plugin.so target/release/open-atc-engine
if [[ "$mode" == preview ]]; then
    cargo build --locked --release -p openatc-desktop
fi
printf '%s\n' 'Built Rust binaries in target/release. Install with: python3 scripts/install-rust-plugin.py "/path/to/X-Plane 12"'
