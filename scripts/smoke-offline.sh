#!/usr/bin/env bash
# Boot the offline launcher through the local roster and exit; no install or server is required.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
log="$(mktemp "${TMPDIR:-/tmp}/benilla-offline-smoke.XXXXXX")"
trap 'rm -f "$log"' EXIT

runner=()
if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
    if command -v xvfb-run >/dev/null 2>&1; then
        runner=(xvfb-run -a)
    else
        printf '%s\n' "offline smoke needs a display or xvfb-run" >&2
        exit 1
    fi
fi

if ! "${runner[@]}" env WOW_DATA= WOW_HOST=127.0.0.1:1 WOW_USER=offline-probe \
    WOW_PASS=unused WOW_CHAR=OfflineProbe BENILLA_OFFLINE_BOOT_CHECK=1 \
    cargo run -q -p benilla-offline >"$log" 2>&1; then
    printf '%s\n' "offline smoke failed"
    tail -40 "$log"
    exit 1
fi
if ! grep -q 'offline: local roster reached without transport' "$log"; then
    printf '%s\n' "offline smoke did not reach the local roster"
    tail -40 "$log"
    exit 1
fi
printf '%s\n' "offline smoke: local roster reached; process exited successfully"
