#!/usr/bin/env bash
set -euo pipefail

# Publish all workspace crates to crates.io in dependency order.
#
# Usage:
#   scripts/release.sh              # publish for real (prompts for confirmation)
#   scripts/release.sh --dry-run    # verify packaging without uploading
#   scripts/release.sh --yes        # skip confirmation prompt
#
# Dependency order:
#   1. toolpath           (no workspace deps)
#   2. toolpath-git       (depends on toolpath)
#      toolpath-dot       (depends on toolpath)
#      toolpath-claude    (depends on toolpath)
#   3. toolpath-cli       (depends on all of the above)

ALL_CRATES=(toolpath toolpath-git toolpath-dot toolpath-claude toolpath-cli)

DRY_RUN=""
AUTO_YES=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN="--dry-run" ;;
        --yes|-y) AUTO_YES=1 ;;
        *) echo "unknown argument: $arg"; exit 1 ;;
    esac
done

if [[ -n "$DRY_RUN" ]]; then
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

already_published() {
    local crate="$1"
    local version="$2"
    cargo search "$crate" 2>/dev/null | grep -q "^${crate} = \"${version}\""
}

wait_for_index() {
    local crate="$1"
    local version="$2"
    if [[ -n "$DRY_RUN" ]]; then
        return
    fi
    echo "    waiting for $crate $version to appear on crates.io index..."
    for i in $(seq 1 30); do
        if already_published "$crate" "$version"; then
            echo "    $crate $version is live"
            return
        fi
        sleep 2
    done
    echo "warning: timed out waiting for $crate $version (continuing anyway)"
}

# --- Survey: check what needs publishing ---
# Uses parallel indexed arrays instead of associative arrays (bash 3.2 compat)

echo "=== surveying crates ==="

VERSIONS=()    # version for each crate (parallel to ALL_CRATES)
STATUSES=()    # "publish" or "skip" for each crate (parallel to ALL_CRATES)
TO_PUBLISH=()

for i in "${!ALL_CRATES[@]}"; do
    crate="${ALL_CRATES[$i]}"
    version=$(get_version "$crate")
    VERSIONS+=("$version")
    if [[ -n "$DRY_RUN" ]]; then
        STATUSES+=("publish")
        TO_PUBLISH+=("$crate")
    elif already_published "$crate" "$version"; then
        STATUSES+=("skip")
    else
        STATUSES+=("publish")
        TO_PUBLISH+=("$crate")
    fi
done

echo
if [[ ${#TO_PUBLISH[@]} -eq 0 ]]; then
    echo "all crates are already published at their current versions:"
    for i in "${!ALL_CRATES[@]}"; do
        echo "  ${ALL_CRATES[$i]} ${VERSIONS[$i]}  (up to date)"
    done
    echo
    echo "nothing to do."
    exit 0
fi

echo "publish plan:"
for i in "${!ALL_CRATES[@]}"; do
    if [[ "${STATUSES[$i]}" == "publish" ]]; then
        echo "  ${ALL_CRATES[$i]} ${VERSIONS[$i]}  -> publish"
    else
        echo "  ${ALL_CRATES[$i]} ${VERSIONS[$i]}  (already published, skip)"
    fi
done
echo

# --- Confirmation ---

if [[ -z "$DRY_RUN" && -z "$AUTO_YES" ]]; then
    read -rp "proceed? [y/N] " answer
    if [[ "$answer" != "y" && "$answer" != "Y" ]]; then
        echo "aborted."
        exit 1
    fi
    echo
fi

# --- Pre-flight: run tests and clippy ---

echo "=== pre-flight checks ==="
cargo test --workspace --quiet
cargo clippy --workspace --quiet -- -D warnings
cargo doc --workspace --no-deps --quiet
echo "all checks passed"
echo

# --- Helpers to look up survey results ---

crate_index() {
    local name="$1"
    for i in "${!ALL_CRATES[@]}"; do
        if [[ "${ALL_CRATES[$i]}" == "$name" ]]; then
            echo "$i"
            return
        fi
    done
    echo "error: unknown crate $name" >&2
    exit 1
}

should_publish() {
    local idx
    idx=$(crate_index "$1")
    [[ "${STATUSES[$idx]}" == "publish" ]]
}

crate_version() {
    local idx
    idx=$(crate_index "$1")
    echo "${VERSIONS[$idx]}"
}

# --- Publish in dependency order ---

publish() {
    local crate="$1"
    local version
    version=$(crate_version "$crate")
    if ! should_publish "$crate"; then
        echo "--- $crate $version already published, skipping ---"
        echo
        return
    fi
    echo "--- publishing $crate $version ---"
    cargo publish -p "$crate" $DRY_RUN $ALLOW_DIRTY
    echo
}

# Tier 1: toolpath (foundation, no workspace deps)
publish toolpath
if should_publish toolpath; then
    wait_for_index toolpath "$(crate_version toolpath)"
fi

# Tier 2: satellite crates (depend only on toolpath, no cross-deps)
for crate in toolpath-git toolpath-dot toolpath-claude; do
    publish "$crate"
done

# Wait for tier 2 publishes to land before publishing the CLI
for crate in toolpath-git toolpath-dot toolpath-claude; do
    if should_publish "$crate"; then
        wait_for_index "$crate" "$(crate_version "$crate")"
    fi
done

# Tier 3: CLI binary (depends on everything above)
publish toolpath-cli

echo "=== done ==="
