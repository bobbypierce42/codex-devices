#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 0 ]]; then
    echo "usage: $0" >&2
    exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
cargo build --locked --release
swift build --package-path "$ROOT_DIR/macos" --configuration release --product codex-devices-mac
SWIFT_BIN="$(swift build --package-path "$ROOT_DIR/macos" --configuration release --show-bin-path)"
CARGO_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release/codex-devices"
mkdir -p "$ROOT_DIR/dist"
install -m 755 "$CARGO_BIN" "$ROOT_DIR/dist/codex-devices"
install -m 755 "$SWIFT_BIN/codex-devices-mac" "$ROOT_DIR/dist/codex-devices-mac"
echo "Built $ROOT_DIR/dist/codex-devices-mac"
echo 'Run dist/codex-devices-mac --help for commands. Building does not access an account.'
