#!/usr/bin/env bash
# The gate_* functions are invoked indirectly via "gate_${_name}", which
# older shellcheck flags as SC2317 (unreachable) and newer as SC2329 (unused).
# shellcheck disable=SC2317
#
# Run quality gates for the repository.
#
# Usage:
#   scripts/quality_gates.sh [--verbose] [[-]gate ...]
#
# Gates: format, shellcheck, clippy, test, doc, examples, site
# No args runs all gates. Prefix with - to exclude a gate.
#
# Options:
#   --verbose    Stream all output (useful for CI)
#
# Examples:
#   scripts/quality_gates.sh              # all gates
#   scripts/quality_gates.sh test         # just tests
#   scripts/quality_gates.sh -site        # everything except site build
#   scripts/quality_gates.sh --verbose    # all gates, full output
#

set -uo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"

# ── Colors (when stdout is a terminal) ────────────────────────────────────────

if [[ -t 1 ]]; then
    _grn=$'\033[32m' _red=$'\033[31m' _bld=$'\033[1m' _dim=$'\033[2m' _rst=$'\033[0m'
else
    _grn='' _red='' _bld='' _dim='' _rst=''
fi

# ── Temp dir for captured output ──────────────────────────────────────────────

_tmpdir=$(mktemp -d)
trap 'rm -rf "${_tmpdir}"' EXIT

# ── Gate definitions ──────────────────────────────────────────────────────────
#
# All `gate_*` functions are dispatched indirectly via "gate_${_name}" inside
# run_gate; shellcheck can't see the call sites and flags them as unused.

_all_gates=(format shellcheck clippy test doc examples site)

# shellcheck disable=SC2329
gate_format() {
    echo "--- cargo fmt ---"
    cargo fmt --all --manifest-path "${_root}/Cargo.toml" --check 2>&1
    echo "--- prettier ---"
    cd "${_root}/site" || return 1
    npx --yes prettier --check --no-color "**/*.{md,css,json,js}" 2>&1
}

# shellcheck disable=SC2329
gate_shellcheck() {
    if ! command -v shellcheck >/dev/null 2>&1; then
        echo "shellcheck not found on PATH; install it (e.g. \`brew install shellcheck\`)" >&2
        return 1
    fi
    shellcheck "${_root}"/scripts/*.sh 2>&1
}

# shellcheck disable=SC2329
gate_clippy() {
    cargo clippy --workspace -- -D warnings 2>&1
}

# shellcheck disable=SC2329
gate_test() {
    RUSTFLAGS="-D warnings" cargo test --workspace 2>&1
}

# shellcheck disable=SC2329
gate_doc() {
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps 2>&1
}

# shellcheck disable=SC2329
gate_examples() {
    local _failed=0
    for _f in "${_root}"/examples/*.json; do
        if ! RUSTFLAGS="-D warnings" cargo run --quiet -p path-cli -- p validate --input "${_f}" 2>&1; then
            _failed=1
        fi
    done
    return "${_failed}"
}

# shellcheck disable=SC2329
gate_site() {
    # `pnpm install --frozen-lockfile` is a no-op when node_modules is in sync
    # with pnpm-lock.yaml, so the cost is only paid once per fresh checkout
    # (notably, every new worktree). Without it the build fails with
    # `sh: eleventy: command not found` and a stale "did you mean to install?"
    # warning that doesn't say which directory.
    cd "${_root}/site" && pnpm install --frozen-lockfile 2>&1 && pnpm run build 2>&1
}

# ── Runner ────────────────────────────────────────────────────────────────────

run_gate() {
    local _name=$1

    if [[ ${_verbose} -eq 1 ]]; then
        echo "${_bld}── ${_name} ──${_rst}"
        local _start=${SECONDS}
        if "gate_${_name}"; then
            local _elapsed=$(( SECONDS - _start ))
            echo "${_grn}PASS${_rst}: ${_name} (${_elapsed}s)"
            echo ""
            return 0
        else
            local _elapsed=$(( SECONDS - _start ))
            echo "${_red}FAIL${_rst}: ${_name} (${_elapsed}s)"
            echo ""
            return 1
        fi
    else
        local _logfile="${_tmpdir}/${_name}.log"
        printf "%s: " "${_name}"
        local _start=${SECONDS}
        if "gate_${_name}" > "${_logfile}" 2>&1; then
            local _elapsed=$(( SECONDS - _start ))
            echo "${_grn}PASS${_rst} (${_elapsed}s)"
            return 0
        else
            local _elapsed=$(( SECONDS - _start ))
            echo "${_red}FAIL${_rst} (${_elapsed}s)"
            tail -50 "${_logfile}" | sed 's/^/    /'
            return 1
        fi
    fi
}

# ── Parse args ────────────────────────────────────────────────────────────────

_valid_gate() {
    local _name=$1
    for _g in "${_all_gates[@]}"; do [[ "${_name}" == "${_g}" ]] && return 0; done
    return 1
}

_verbose=0
_gates=()
_excludes=()
for _arg in "$@"; do
    if [[ "${_arg}" == "--verbose" ]]; then
        _verbose=1
        continue
    elif [[ "${_arg}" == -* ]]; then
        _name="${_arg#-}"
        if ! _valid_gate "${_name}"; then
            echo "Unknown gate: ${_name}"
            echo "Valid gates: ${_all_gates[*]}"
            exit 2
        fi
        _excludes+=("${_name}")
    else
        if ! _valid_gate "${_arg}"; then
            echo "Unknown gate: ${_arg}"
            echo "Valid gates: ${_all_gates[*]}"
            exit 2
        fi
        _gates+=("${_arg}")
    fi
done

# Default: all gates
if [[ ${#_gates[@]} -eq 0 ]]; then
    _gates=("${_all_gates[@]}")
fi

# Apply exclusions
if [[ ${#_excludes[@]} -gt 0 ]]; then
    _filtered=()
    for _g in "${_gates[@]}"; do
        _excluded=0
        for _e in "${_excludes[@]}"; do
            [[ "${_g}" == "${_e}" ]] && _excluded=1 && break
        done
        [[ ${_excluded} -eq 0 ]] && _filtered+=("${_g}")
    done
    _gates=("${_filtered[@]}")
fi

# ── Run gates ─────────────────────────────────────────────────────────────────

_passed=0
_total=${#_gates[@]}

echo "${_bld}Running: ${_gates[*]}${_rst} ${_dim}(skip with -gate, e.g. -site)${_rst}"
echo ""

for _gate in "${_gates[@]}"; do
    if run_gate "${_gate}"; then
        ((_passed++))
    fi
done

echo ""

if [[ ${_passed} -eq ${_total} ]]; then
    echo "${_grn}${_passed}/${_total} passed${_rst}"
    exit 0
else
    echo "${_red}${_passed}/${_total} passed${_rst}"
    exit 1
fi
