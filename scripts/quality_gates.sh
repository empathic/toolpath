#!/usr/bin/env bash
#
# Run quality gates for the repository.
#
# Usage:
#   scripts/quality_gates.sh [--verbose] [[-]gate ...]
#
# Gates: format, clippy, test, doc, examples, site
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

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ── Colors (when stdout is a terminal) ────────────────────────────────────────

if [[ -t 1 ]]; then
    _grn=$'\033[32m' _red=$'\033[31m' _bld=$'\033[1m' _dim=$'\033[2m' _rst=$'\033[0m'
else
    _grn='' _red='' _bld='' _dim='' _rst=''
fi

# ── Temp dir for captured output ──────────────────────────────────────────────

_tmpdir=$(mktemp -d)
trap 'rm -rf "$_tmpdir"' EXIT

# ── Gate definitions ──────────────────────────────────────────────────────────

_all_gates=(format clippy test doc examples site)

gate_format() {
    echo "--- cargo fmt ---"
    cargo fmt --all --manifest-path "$ROOT/Cargo.toml" --check 2>&1
    echo "--- prettier ---"
    cd "$ROOT/site"
    npx --yes prettier --check --no-color "**/*.{md,css,json,js}" 2>&1
}

gate_clippy() {
    cargo clippy --workspace -- -D warnings 2>&1
}

gate_test() {
    cargo test --workspace 2>&1
}

gate_doc() {
    cargo doc --workspace --no-deps 2>&1
}

gate_examples() {
    local failed=0
    for f in "$ROOT"/examples/*.json; do
        if ! cargo run --quiet -p toolpath-cli -- validate --input "$f" 2>&1; then
            failed=1
        fi
    done
    return $failed
}

gate_site() {
    cd "$ROOT/site" && pnpm run build 2>&1
}

# ── Runner ────────────────────────────────────────────────────────────────────

run_gate() {
    local name=$1

    if [[ $_verbose -eq 1 ]]; then
        echo "${_bld}── $name ──${_rst}"
        local start=$SECONDS
        if "gate_$name"; then
            local elapsed=$(( SECONDS - start ))
            echo "${_grn}PASS${_rst}: $name (${elapsed}s)"
            echo ""
            return 0
        else
            local elapsed=$(( SECONDS - start ))
            echo "${_red}FAIL${_rst}: $name (${elapsed}s)"
            echo ""
            return 1
        fi
    else
        local logfile="$_tmpdir/$name.log"
        printf "%s: " "$name"
        local start=$SECONDS
        if "gate_$name" > "$logfile" 2>&1; then
            local elapsed=$(( SECONDS - start ))
            echo "${_grn}PASS${_rst} (${elapsed}s)"
            return 0
        else
            local elapsed=$(( SECONDS - start ))
            echo "${_red}FAIL${_rst} (${elapsed}s)"
            tail -50 "$logfile" | sed 's/^/    /'
            return 1
        fi
    fi
}

# ── Parse args ────────────────────────────────────────────────────────────────

_valid_gate() {
    local name=$1
    for g in "${_all_gates[@]}"; do [[ "$name" == "$g" ]] && return 0; done
    return 1
}

_verbose=0
gates=()
excludes=()
for arg in "$@"; do
    if [[ "$arg" == "--verbose" ]]; then
        _verbose=1
        continue
    elif [[ "$arg" == -* ]]; then
        name="${arg#-}"
        if ! _valid_gate "$name"; then
            echo "Unknown gate: $name"
            echo "Valid gates: ${_all_gates[*]}"
            exit 2
        fi
        excludes+=("$name")
    else
        if ! _valid_gate "$arg"; then
            echo "Unknown gate: $arg"
            echo "Valid gates: ${_all_gates[*]}"
            exit 2
        fi
        gates+=("$arg")
    fi
done

# Default: all gates
if [[ ${#gates[@]} -eq 0 ]]; then
    gates=("${_all_gates[@]}")
fi

# Apply exclusions
if [[ ${#excludes[@]} -gt 0 ]]; then
    filtered=()
    for g in "${gates[@]}"; do
        excluded=0
        for e in "${excludes[@]}"; do
            [[ "$g" == "$e" ]] && excluded=1 && break
        done
        [[ $excluded -eq 0 ]] && filtered+=("$g")
    done
    gates=("${filtered[@]}")
fi

# ── Run gates ─────────────────────────────────────────────────────────────────

passed=0
total=${#gates[@]}

echo "${_bld}Running: ${gates[*]}${_rst} ${_dim}(skip with -gate, e.g. -site)${_rst}"
echo ""

for gate in "${gates[@]}"; do
    if run_gate "$gate"; then
        ((passed++))
    fi
done

echo ""

if [[ $passed -eq $total ]]; then
    echo "${_grn}${passed}/${total} passed${_rst}"
    exit 0
else
    echo "${_red}${passed}/${total} passed${_rst}"
    exit 1
fi
