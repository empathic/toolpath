#!/usr/bin/env bash
set -euo pipefail

# Publish all workspace crates to crates.io in dependency order.
#
# Usage:
#   scripts/release.sh              # publish for real
#   scripts/release.sh --dry-run    # verify packaging without uploading
#
# Dependency order:
#   1. toolpath           (no workspace deps)
#   2. toolpath-git       (depends on toolpath)
#      toolpath-dot       (depends on toolpath)
#      toolpath-claude    (depends on toolpath)
#   3. toolpath-cli       (depends on all of the above)

DRY_RUN=""
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN="--dry-run"
    echo "=== DRY RUN ==="
    echo
fi

ALLOW_DIRTY=""
if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
    if [[ -n "$DRY_RUN" ]]; then
        ALLOW_DIRTY="--allow-dirty"
    else
        echo "error: working directory has uncommitted changes"
        echo "commit or stash before publishing"
        exit 1
    fi
fi

publish() {
    local crate="$1"
    echo "--- publishing $crate ---"
    cargo publish -p "$crate" $DRY_RUN $ALLOW_DIRTY
    echo
}

wait_for_index() {
    local crate="$1"
    local version="$2"
    if [[ -n "$DRY_RUN" ]]; then
        return
    fi
    echo "    waiting for $crate $version to appear on crates.io index..."
    for i in $(seq 1 30); do
        if cargo search "$crate" 2>/dev/null | grep -q "^${crate} = \"${version}\""; then
            echo "    $crate $version is live"
            return
        fi
        sleep 2
    done
    echo "warning: timed out waiting for $crate $version (continuing anyway)"
}

get_version() {
    cargo metadata --format-version 1 --no-deps \
        | python3 -c "
import json, sys
meta = json.load(sys.stdin)
for pkg in meta['packages']:
    if pkg['name'] == '$1':
        print(pkg['version'])
        break
"
}

# Pre-flight: run tests and clippy
echo "=== pre-flight checks ==="
cargo test --workspace --quiet
cargo clippy --workspace --quiet -- -D warnings
cargo doc --workspace --no-deps --quiet
echo "all checks passed"
echo

# Tier 1: toolpath (foundation, no workspace deps)
TOOLPATH_VERSION=$(get_version toolpath)
publish toolpath
wait_for_index toolpath "$TOOLPATH_VERSION"

# Tier 2: satellite crates (depend only on toolpath, no cross-deps)
for crate in toolpath-git toolpath-dot toolpath-claude; do
    publish "$crate"
done

# Wait for tier 2 to land before publishing the CLI
for crate in toolpath-git toolpath-dot toolpath-claude; do
    version=$(get_version "$crate")
    wait_for_index "$crate" "$version"
done

# Tier 3: CLI binary (depends on everything above)
publish toolpath-cli

echo "=== done ==="
