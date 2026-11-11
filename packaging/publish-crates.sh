#!/usr/bin/env bash
# Publish the workspace crates to crates.io in dependency order.
#
# The GUI and CLI crates depend on the library crates through path + version
# dependencies, so the libraries must be on crates.io before them.  crates.io
# rate-limits *new* crates (about five per ten minutes), so publications are
# spaced and a 429 response is retried after a pause.
#
# Usage: packaging/publish-crates.sh [--dry-run]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
[ -n "$VERSION" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }

CRATES=(
    flipper-companion-core
    flipper-companion-dfu
    flipper-companion-protocol
    flipper-companion-update
    flipper-companion-app
    flipper-companion-project
    flipper-companion-cli
    flipper-companion
)

# Time between two publications: crates.io allows about five new crates per
# ten minutes, so 120 s per crate keeps the whole run under the limit.
NEW_CRATE_DELAY=120

already_published() {
    curl -fsSL -A "flipper-companion-release" \
        "https://crates.io/api/v1/crates/$1/$VERSION" >/dev/null 2>&1
}

wait_for_crate() {
    local crate="$1"
    for _ in $(seq 1 60); do
        if already_published "$crate"; then
            return 0
        fi
        sleep 5
    done
    echo "warning: $crate $VERSION is not visible on crates.io yet" >&2
}

publish_one() {
    local crate="$1" output attempt

    for attempt in 1 2 3 4 5; do
        if output="$(cargo publish -p "$crate" --locked 2>&1)"; then
            echo "$output" | tail -2
            return 0
        fi

        echo "$output" | tail -6
        if echo "$output" | grep -qi "too many new crates\|429 Too Many Requests"; then
            echo "crates.io rate limit reached, waiting 10 minutes (attempt $attempt)..."
            sleep 600
        else
            return 1
        fi
    done

    return 1
}

for crate in "${CRATES[@]}"; do
    if [ "$DRY_RUN" = 1 ]; then
        echo "cargo publish -p $crate --dry-run"
        cargo publish -p "$crate" --locked --dry-run
        continue
    fi

    if already_published "$crate"; then
        echo "$crate $VERSION is already on crates.io, skipping."
        continue
    fi

    echo "publishing $crate $VERSION"
    publish_one "$crate"
    wait_for_crate "$crate"
    sleep "$NEW_CRATE_DELAY"
done

echo "crates.io publication complete for $VERSION."
