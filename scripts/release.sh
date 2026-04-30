#!/usr/bin/env bash
set -euo pipefail

# Verify (default) or publish (--execute) all workspace crates to crates.io
# in dependency order.
#
# Default mode is a dry run — it packages each crate, runs `cargo publish
# --dry-run` against the real crates.io view, and reports what *would*
# happen. Nothing is uploaded. This catches publish-time resolution issues
# (e.g. deps pinning incompatible major versions of a shared crate) that
# `cargo build/test --workspace` cannot see, because workspace builds use
# local path-deps while `cargo publish` resolves against the registry.
#
# Pass --execute to actually publish.
#
# Usage:
#   scripts/release.sh                  # dry-run (default; safe)
#   scripts/release.sh --execute        # publish for real (prompts)
#   scripts/release.sh --execute --yes  # publish for real, skip prompt
#   scripts/release.sh --dry-run        # alias for default (back-compat)
#
# Dependency order:
#   1. toolpath           (no workspace deps)
#      pathbase-client    (no workspace deps; built from schema/pathbase-openapi.json)
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

ALL_CRATES=(toolpath pathbase-client toolpath-convo toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-opencode toolpath-pi path-cli toolpath-cli)

EXECUTE=0
AUTO_YES=""
for arg in "$@"; do
    case "$arg" in
        --execute)  EXECUTE=1 ;;
        --dry-run)  ;;  # back-compat: dry-run is the default
        --yes|-y)   AUTO_YES=1 ;;
        -h|--help)
            sed -n '4,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown argument: $arg"; echo "see --help"; exit 1 ;;
    esac
done

if (( EXECUTE )); then
    DRY_RUN=""
    echo "=== mode: EXECUTE — will publish to crates.io ==="
else
    DRY_RUN="--dry-run"
    echo "=== mode: dry-run (pass --execute to publish for real) ==="
fi
echo

ALLOW_DIRTY=""
if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
    if (( EXECUTE )); then
        echo "error: working directory has uncommitted changes"
        echo "commit or stash before publishing"
        exit 1
    else
        ALLOW_DIRTY="--allow-dirty"
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
    if ! (( EXECUTE )); then
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
# Uses parallel indexed arrays instead of associative arrays (bash 3.2 compat).

echo "=== surveying crates ==="

VERSIONS=()    # version for each crate (parallel to ALL_CRATES)
STATUSES=()    # "publish" or "skip" for each crate (parallel to ALL_CRATES)
TO_PUBLISH=()

for i in "${!ALL_CRATES[@]}"; do
    crate="${ALL_CRATES[$i]}"
    version=$(get_version "$crate")
    VERSIONS+=("$version")
    if already_published "$crate" "$version"; then
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

# --- Confirmation (only when actually publishing) ---

if (( EXECUTE )) && [[ -z "$AUTO_YES" ]]; then
    read -rp "proceed? [y/N] " answer
    if [[ "$answer" != "y" && "$answer" != "Y" ]]; then
        echo "aborted."
        exit 1
    fi
    echo
fi

# --- Pre-flight: registry compatibility check ---
#
# `cargo publish --dry-run` skips already-published crates (cannot
# re-upload them) and defers crates whose deps are themselves being
# published this run, so the pairing "old already-published satellite
# + about-to-publish new foundation" is invisible to dry-run. This
# check fills that gap: for every workspace crate already on the
# registry at its current local version, fetch its on-registry manifest
# and verify each workspace-sibling requirement is satisfied by the
# workspace's about-to-publish version of that sibling. If a foundation
# bump (e.g. toolpath 0.2 -> 0.4) leaves an old satellite still pinning
# the previous major, the cascade gets caught here instead of at real
# publish time (where it manifests as an E0308 dual-version graph).

echo "=== registry compatibility check ==="
WS_ARGS=()
for i in "${!ALL_CRATES[@]}"; do
    WS_ARGS+=("${ALL_CRATES[$i]}=${VERSIONS[$i]}")
done
SKIP_ARGS=()
for i in "${!ALL_CRATES[@]}"; do
    if [[ "${STATUSES[$i]}" == "skip" ]]; then
        SKIP_ARGS+=("${ALL_CRATES[$i]}")
    fi
done
if [[ ${#SKIP_ARGS[@]} -eq 0 ]]; then
    echo "    no already-published crates to check"
else
    python3 scripts/check-registry-compat.py "${WS_ARGS[@]}" --skip "${SKIP_ARGS[@]}"
fi
echo

# --- Pre-flight: workspace tests and clippy ---

echo "=== pre-flight checks ==="
cargo test --workspace --quiet
cargo clippy --workspace --quiet -- -D warnings
cargo doc --workspace --no-deps --quiet
echo "workspace checks ok"
echo

# --- Pre-flight: per-crate publish dry-run ---
#
# Workspace `cargo build/test` resolve every dep through local path entries,
# bypassing crates.io entirely. `cargo publish --dry-run` resolves against
# the registry — exactly the view that matters at publish time. This loop
# catches issues like "satellite-crate-A on crates.io still pins old toolpath,
# while we depend directly on a newer toolpath" before any real upload.
#
# Failures classed as chicken-and-egg (the failing dep is itself in this
# release's TO_PUBLISH set, so it'll land on the registry mid-publish) are
# tolerated; everything else aborts.

echo "=== publish dry-runs ==="
PREFLIGHT_FAILED=()
for crate in "${TO_PUBLISH[@]}"; do
    logfile=$(mktemp -t release-dryrun.XXXXXX)
    rc=0
    if [[ "$crate" == "toolpath-cli" ]]; then
        (cd crates/toolpath-cli && cargo publish --dry-run $ALLOW_DIRTY) > "$logfile" 2>&1 || rc=$?
    else
        cargo publish --dry-run $ALLOW_DIRTY -p "$crate" > "$logfile" 2>&1 || rc=$?
    fi
    if (( rc == 0 )); then
        echo "    $crate: ok"
    else
        # Cargo phrases "dep not on registry yet" several ways depending on
        # context. Try each known shape; whichever produces a match wins.
        missing=$(sed -nE \
            -e 's/.*no matching package named `([^`]+)`.*/\1/p' \
            -e 's/.*failed to select a version for the requirement `([^ `]+).*/\1/p' \
            -e 's/.*could not find `([^`]+)` in registry.*/\1/p' \
            "$logfile" | head -1)
        if [[ -n "$missing" ]] && printf '%s\n' "${TO_PUBLISH[@]}" | grep -qFx "$missing"; then
            echo "    $crate: deferred (depends on $missing being published in this run)"
        else
            echo "    $crate: FAILED"
            tail -40 "$logfile" | sed 's/^/        /'
            PREFLIGHT_FAILED+=("$crate")
        fi
    fi
    rm -f "$logfile"
done
if (( ${#PREFLIGHT_FAILED[@]} > 0 )); then
    echo
    echo "publish dry-run failed for: ${PREFLIGHT_FAILED[*]}"
    echo "aborting before any real publishing happens."
    exit 1
fi
echo "publish dry-runs ok"
echo

# In dry-run mode (the default), the dry-run pre-flight above is the whole
# point of the script. Stop here.
if ! (( EXECUTE )); then
    echo "=== dry-run done — pass --execute to publish ==="
    exit 0
fi

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
        (cd crates/toolpath-cli && cargo publish $ALLOW_DIRTY)
    else
        cargo publish -p "$crate" $ALLOW_DIRTY
    fi
    echo
}

# Tier 1: foundation crates (no workspace deps)
publish toolpath
publish pathbase-client
if should_publish toolpath; then
    wait_for_index toolpath "$(crate_version toolpath)"
fi
if should_publish pathbase-client; then
    wait_for_index pathbase-client "$(crate_version pathbase-client)"
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
