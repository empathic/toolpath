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
#   2a. toolpath-convo    (depends on toolpath)
#   2b. toolpath-git      (depends on toolpath)
#       toolpath-github   (depends on toolpath)
#       toolpath-dot      (depends on toolpath)
#       toolpath-md       (depends on toolpath)
#       toolpath-claude   (depends on toolpath, toolpath-convo)
#       toolpath-gemini   (depends on toolpath, toolpath-convo)
#       toolpath-codex    (depends on toolpath, toolpath-convo)
#       toolpath-opencode (depends on toolpath, toolpath-convo)
#       toolpath-pi       (depends on toolpath, toolpath-convo)
#   3. path-cli           (depends on all of the above)
#   4. toolpath-cli       (deprecated shim that depends on path-cli)

ALL_CRATES=(toolpath toolpath-convo toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-opencode toolpath-pi path-cli toolpath-cli)

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

# `toolpath-cli` is the only crate that lives outside the workspace (excluded so
# its `path` bin doesn't collide with `path-cli`'s in the shared workspace
# target dir). Anything that shells out to cargo for a specific package needs
# to know where its manifest lives.
manifest_arg_for() {
    if [[ "$1" == "toolpath-cli" ]]; then
        echo "--manifest-path crates/toolpath-cli/Cargo.toml"
    fi
}

get_version() {
    cargo metadata --format-version 1 --no-deps $(manifest_arg_for "$1") \
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
    if [[ "$crate" == "toolpath-cli" ]]; then
        # Excluded from the workspace; publish from its own manifest.
        (cd crates/toolpath-cli && cargo publish $DRY_RUN $ALLOW_DIRTY)
    else
        cargo publish -p "$crate" $DRY_RUN $ALLOW_DIRTY
    fi
    echo
}

# Tier 1: foundation crate (no workspace deps)
publish toolpath
if should_publish toolpath; then
    wait_for_index toolpath "$(crate_version toolpath)"
fi

# Tier 2a: toolpath-convo (depends on toolpath). Published before the other
# satellite crates so that toolpath-claude, toolpath-gemini, and toolpath-pi
# (which depend on it) see it live on the index.
publish toolpath-convo
if should_publish toolpath-convo; then
    wait_for_index toolpath-convo "$(crate_version toolpath-convo)"
fi

# Tier 2b: satellite crates (depend on tier 1 and/or toolpath-convo)
for crate in toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-opencode toolpath-pi; do
    publish "$crate"
done

for crate in toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-opencode toolpath-pi; do
    if should_publish "$crate"; then
        wait_for_index "$crate" "$(crate_version "$crate")"
    fi
done

# Tier 3: CLI binary (depends on everything above)
publish path-cli
if should_publish path-cli; then
    wait_for_index path-cli "$(crate_version path-cli)"
fi

# Tier 4: deprecated shim that re-exports path-cli (so `cargo install toolpath-cli` keeps working)
publish toolpath-cli

echo "=== done ==="
