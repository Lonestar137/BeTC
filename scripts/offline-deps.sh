#!/usr/bin/env bash
# Assert the offline launcher resolves without the network/SRP feature graph.
set -euo pipefail

tree="$(cargo tree -p benilla-offline -e features)"
if printf '%s\n' "$tree" | grep -Eq 'benilla-srp|benilla-protocol feature "network"'; then
    printf '%s\n' "offline-deps: network or SRP dependency found in benilla-offline graph" >&2
    exit 1
fi
printf '%s\n' "offline-deps: no network or SRP feature in benilla-offline graph"
