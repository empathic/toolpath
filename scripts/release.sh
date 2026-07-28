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
#       toolpath-copilot  (depends on toolpath, toolpath-convo)
#       toolpath-amp      (depends on toolpath, toolpath-convo)
#       toolpath-opencode (depends on toolpath, toolpath-convo)
#       toolpath-cursor   (depends on toolpath, toolpath-convo)
#       toolpath-pi       (depends on toolpath, toolpath-convo)
#   3. path-cli           (depends on all of the above)
#   4. toolpath-cli       (deprecated shim that depends on path-cli)

_all_crates=(toolpath pathbase-client toolpath-convo toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-copilot toolpath-amp toolpath-opencode toolpath-cursor toolpath-pi path-cli toolpath-cli)

_execute=0
_auto_yes=""
for _arg in "$@"; do
    case "${_arg}" in
        --execute)  _execute=1 ;;
        --dry-run)  ;;  # back-compat: dry-run is the default
        --yes|-y)   _auto_yes=1 ;;
        -h|--help)
            sed -n '4,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown argument: ${_arg}"; echo "see --help"; exit 1 ;;
    esac
done

if (( _execute )); then
    _dry_run=""
    echo "=== mode: EXECUTE — will publish to crates.io ==="
else
    _dry_run="--dry-run"
    echo "=== mode: dry-run (pass --execute to publish for real) ==="
fi
echo

_allow_dirty=""
if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
    if (( _execute )); then
        echo "error: working directory has uncommitted changes"
        echo "commit or stash before publishing"
        exit 1
    else
        _allow_dirty="--allow-dirty"
    fi
fi

# `toolpath-cli` is the only crate that lives outside the workspace (excluded so
# its `path` bin doesn't collide with `path-cli`'s in the shared workspace
# target dir). Anything that shells out to cargo for a specific package needs
# to know where its manifest lives.
get_version() {
    local _manifest_args=()
    if [[ "$1" == "toolpath-cli" ]]; then
        _manifest_args=(--manifest-path crates/toolpath-cli/Cargo.toml)
    fi
    cargo metadata --format-version 1 --no-deps "${_manifest_args[@]+"${_manifest_args[@]}"}" \
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
    local _crate="$1"
    local _version="$2"
    cargo search "${_crate}" 2>/dev/null | grep -q "^${_crate} = \"${_version}\""
}

wait_for_index() {
    local _crate="$1"
    local _version="$2"
    if ! (( _execute )); then
        return
    fi
    echo "    waiting for ${_crate} ${_version} to appear on crates.io index..."
    for _i in $(seq 1 30); do
        if already_published "${_crate}" "${_version}"; then
            echo "    ${_crate} ${_version} is live"
            return
        fi
        sleep 2
    done
    echo "warning: timed out waiting for ${_crate} ${_version} (continuing anyway)"
}

# --- Survey: check what needs publishing ---
# Uses parallel indexed arrays instead of associative arrays (bash 3.2 compat).

echo "=== surveying crates ==="

_versions=()    # version for each crate (parallel to _all_crates)
_statuses=()    # "publish" or "skip" for each crate (parallel to _all_crates)
_to_publish=()

for _i in "${!_all_crates[@]}"; do
    _crate="${_all_crates[${_i}]}"
    _version=$(get_version "${_crate}")
    _versions+=("${_version}")
    if already_published "${_crate}" "${_version}"; then
        _statuses+=("skip")
    else
        _statuses+=("publish")
        _to_publish+=("${_crate}")
    fi
done

echo
if [[ ${#_to_publish[@]} -eq 0 ]]; then
    echo "all crates are already published at their current versions:"
    for _i in "${!_all_crates[@]}"; do
        echo "  ${_all_crates[${_i}]} ${_versions[${_i}]}  (up to date)"
    done
    echo
    echo "nothing to do."
    exit 0
fi

echo "publish plan:"
for _i in "${!_all_crates[@]}"; do
    if [[ "${_statuses[${_i}]}" == "publish" ]]; then
        echo "  ${_all_crates[${_i}]} ${_versions[${_i}]}  -> publish"
    else
        echo "  ${_all_crates[${_i}]} ${_versions[${_i}]}  (already published, skip)"
    fi
done
echo

# --- Confirmation (only when actually publishing) ---

if (( _execute )) && [[ -z "${_auto_yes}" ]]; then
    read -rp "proceed? [y/N] " _answer
    if [[ "${_answer}" != "y" && "${_answer}" != "Y" ]]; then
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
_ws_args=()
for _i in "${!_all_crates[@]}"; do
    _ws_args+=("${_all_crates[${_i}]}=${_versions[${_i}]}")
done
_skip_args=()
for _i in "${!_all_crates[@]}"; do
    if [[ "${_statuses[${_i}]}" == "skip" ]]; then
        _skip_args+=("${_all_crates[${_i}]}")
    fi
done
if [[ ${#_skip_args[@]} -eq 0 ]]; then
    echo "    no already-published crates to check"
else
    python3 scripts/check-registry-compat.py "${_ws_args[@]}" --skip "${_skip_args[@]}"
fi
echo

# --- Pre-flight: workspace tests and clippy ---

echo "=== pre-flight checks ==="
RUSTFLAGS="-D warnings" cargo test --workspace --quiet
cargo clippy --workspace --quiet -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --quiet
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
# release's _to_publish set, so it'll land on the registry mid-publish) are
# tolerated; everything else aborts.

echo "=== publish dry-runs ==="
_preflight_failed=()
for _crate in "${_to_publish[@]}"; do
    _logfile=$(mktemp -t release-dryrun.XXXXXX)
    _rc=0
    if [[ "${_crate}" == "toolpath-cli" ]]; then
        (cd crates/toolpath-cli && cargo publish --dry-run ${_allow_dirty}) > "${_logfile}" 2>&1 || _rc=$?
    else
        cargo publish --dry-run ${_allow_dirty} -p "${_crate}" > "${_logfile}" 2>&1 || _rc=$?
    fi
    if (( _rc == 0 )); then
        echo "    ${_crate}: ok"
    else
        # Cargo phrases "dep not on registry yet" several ways depending on
        # context. Try each known shape; whichever produces a match wins.
        # shellcheck disable=SC2016  # backticks in single quotes are literal sed input, not command substitution
        _missing=$(sed -nE \
            -e 's/.*no matching package named `([^`]+)`.*/\1/p' \
            -e 's/.*failed to select a version for the requirement `([^ `]+).*/\1/p' \
            -e 's/.*could not find `([^`]+)` in registry.*/\1/p' \
            "${_logfile}" | head -1)
        if [[ -n "${_missing}" ]] && printf '%s\n' "${_to_publish[@]}" | grep -qFx "${_missing}"; then
            echo "    ${_crate}: deferred (depends on ${_missing} being published in this run)"
        else
            echo "    ${_crate}: FAILED"
            tail -40 "${_logfile}" | sed 's/^/        /'
            _preflight_failed+=("${_crate}")
        fi
    fi
    rm -f "${_logfile}"
done
if (( ${#_preflight_failed[@]} > 0 )); then
    echo
    echo "publish dry-run failed for: ${_preflight_failed[*]}"
    echo "aborting before any real publishing happens."
    exit 1
fi
echo "publish dry-runs ok"
echo

# In dry-run mode (the default), the dry-run pre-flight above is the whole
# point of the script. Stop here.
if ! (( _execute )); then
    echo "=== dry-run done — pass --execute to publish ==="
    exit 0
fi

# --- Helpers to look up survey results ---

crate_index() {
    local _name="$1"
    for _i in "${!_all_crates[@]}"; do
        if [[ "${_all_crates[${_i}]}" == "${_name}" ]]; then
            echo "${_i}"
            return
        fi
    done
    echo "error: unknown crate ${_name}" >&2
    exit 1
}

should_publish() {
    local _idx
    _idx=$(crate_index "$1")
    [[ "${_statuses[${_idx}]}" == "publish" ]]
}

crate_version() {
    local _idx
    _idx=$(crate_index "$1")
    echo "${_versions[${_idx}]}"
}

# --- Publish in dependency order ---

publish() {
    local _crate="$1"
    local _version
    _version=$(crate_version "${_crate}")
    if ! should_publish "${_crate}"; then
        echo "--- ${_crate} ${_version} already published, skipping ---"
        echo
        return
    fi
    echo "--- publishing ${_crate} ${_version} ---"
    if [[ "${_crate}" == "toolpath-cli" ]]; then
        # Excluded from the workspace; publish from its own manifest.
        (cd crates/toolpath-cli && cargo publish ${_allow_dirty})
    else
        cargo publish -p "${_crate}" ${_allow_dirty}
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
for _crate in toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-copilot toolpath-amp toolpath-opencode toolpath-cursor toolpath-pi; do
    publish "${_crate}"
done

for _crate in toolpath-git toolpath-github toolpath-dot toolpath-md toolpath-claude toolpath-gemini toolpath-codex toolpath-copilot toolpath-amp toolpath-opencode toolpath-cursor toolpath-pi; do
    if should_publish "${_crate}"; then
        wait_for_index "${_crate}" "$(crate_version "${_crate}")"
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
