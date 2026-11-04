#!/usr/bin/env bash
set -euo pipefail

MODE=run
DEMO=0
for argument in "$@"; do
    case "$argument" in
        --build-only|--verify|--logs|--telemetry) MODE="$argument" ;;
        --demo) DEMO=1 ;;
        *) echo "usage: $0 [--build-only|--verify|--logs|--telemetry] [--demo]" >&2; exit 2 ;;
    esac
done

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_BUNDLE="$ROOT_DIR/dist/Codex Devices.app"
CONTENTS="$APP_BUNDLE/Contents"
APP_BINARY="$CONTENTS/MacOS/CodexDevices"
HELPER_CONTENTS="$CONTENTS/Resources/Codex Devices SSH.app/Contents"

staged_app_pids() {
    while IFS= read -r app_pid; do
        running_path="$(/bin/ps -p "$app_pid" -o comm= 2>/dev/null || true)"
        if [[ "$running_path" == "$APP_BINARY" ]]; then echo "$app_pid"; fi
    done < <(/usr/bin/pgrep -x CodexDevices || true)
}

if [[ "$MODE" == --build-only ]]; then
    if [[ -n "$(staged_app_pids)" ]]; then
        echo 'Quit the staged Codex Devices app before using --build-only.' >&2
        exit 3
    fi
else
    # Stop only this project's staged app before replacing its executable.
    while IFS= read -r app_pid; do kill -TERM "$app_pid"; done < <(staged_app_pids)
    for ((build_wait_attempt=0; build_wait_attempt<50; build_wait_attempt++)); do
        if [[ -z "$(staged_app_pids)" ]]; then break; fi
        sleep 0.1
    done
    if [[ -n "$(staged_app_pids)" ]]; then
        echo 'The staged Codex Devices app did not exit; build refused.' >&2
        exit 3
    fi
fi

cd "$ROOT_DIR"
cargo build --locked --release
swift build --package-path "$ROOT_DIR/macos" --configuration release
SWIFT_BIN="$(swift build --package-path "$ROOT_DIR/macos" --configuration release --show-bin-path)"
CARGO_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release/codex-devices"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$HELPER_CONTENTS/MacOS"
cp "$SWIFT_BIN/CodexDevices" "$APP_BINARY"
cp "$SWIFT_BIN/SSHAskpass" "$HELPER_CONTENTS/MacOS/SSHAskpass"
cp "$CARGO_BIN" "$CONTENTS/Resources/codex-devices"
cp "$ROOT_DIR/macos/Resources/Info.plist" "$CONTENTS/Info.plist"
cp "$ROOT_DIR/macos/Resources/SSHAskpass-Info.plist" "$HELPER_CONTENTS/Info.plist"
chmod +x "$APP_BINARY" "$HELPER_CONTENTS/MacOS/SSHAskpass" "$CONTENTS/Resources/codex-devices"
/usr/bin/plutil -lint "$CONTENTS/Info.plist"
/usr/bin/plutil -lint "$HELPER_CONTENTS/Info.plist"
echo "Built $APP_BUNDLE"
if [[ "$MODE" == --build-only ]]; then exit 0; fi

if [[ "$DEMO" == 1 ]]; then
    /usr/bin/open -n "$APP_BUNDLE" --args --demo
else
    /usr/bin/open -n "$APP_BUNDLE"
fi
case "$MODE" in
    --verify)
        sleep 1
        [[ -n "$(staged_app_pids)" ]]
        ;;
    --logs)
        /usr/bin/log stream --info --style compact --predicate 'process == "CodexDevices"'
        ;;
    --telemetry)
        /usr/bin/log stream --info --style compact --predicate 'subsystem == "org.codex-devices.desktop"'
        ;;
esac
