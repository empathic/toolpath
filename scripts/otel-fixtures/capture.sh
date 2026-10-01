#!/usr/bin/env bash
# Offline OTel GenAI fixture capture. Maintainers only; never run by CI.
#   ./capture.sh            capture every scenario into test-fixtures/otel/ (the semconv
#                           scenarios twice: span mode, then event mode)
#   ./capture.sh verify     re-capture into a temp dir and require byte-identical output
#   ./capture.sh selftest   import smoke check of both venvs, then unit tests of the harness
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
py="${OTEL_FIXTURES_PYTHON:-python3.12}"
cd "$here"
# Two venvs, one per lock: openai-v2 2.4b0 cannot share util-genai with the rest (README).
setup_venv() {  # <venv dir> <requirements file>
    if [[ ! -x "$1/bin/python" ]]; then
        "$py" -m venv "$1"
    fi
    "$1/bin/python" -m pip install --quiet --require-hashes -r "$2"
}
setup_venv .venv requirements.txt
setup_venv .venv-openai-v2 requirements-openai-v2.txt

# The venv a scenario captures in: its instrumentation's lock.
venv_for() {
    case "$1" in
        openai_chat) echo .venv-openai-v2 ;;
        *) echo .venv ;;
    esac
}

scenarios=(openai_chat openai_responses anthropic gemini openinference_openai_chat)
# Scenarios that also capture in event mode (content on log records).
event_scenarios=(openai_chat openai_responses anthropic gemini)
capture_all() {  # [capture.py options...]
    for s in "${scenarios[@]}"; do "$(venv_for "$s")/bin/python" capture.py "$s" "$@"; done
    for s in "${event_scenarios[@]}"; do
        "$(venv_for "$s")/bin/python" capture.py "$s" --mode event "$@"
    done
}
mode="${1:-capture}"
case "$mode" in
    capture)
        capture_all
        ;;
    verify)
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        capture_all --out "$tmp"
        .venv/bin/python verify.py "$tmp"
        ;;
    selftest)
        .venv/bin/python smoke.py main
        .venv-openai-v2/bin/python smoke.py openai-v2
        for venv in .venv .venv-openai-v2; do
            "$venv/bin/python" -m unittest discover -s tests -t . -v
        done
        ;;
    *)
        echo "usage: $0 [capture|verify|selftest]" >&2
        exit 2
        ;;
esac
