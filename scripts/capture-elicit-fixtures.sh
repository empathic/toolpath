#!/usr/bin/env bash
# shellcheck disable=SC2012
# (uses `ls -1` to enumerate harness session/project directories whose
# names are UUIDs / project slugs — `find` would be more verbose without
# adding safety here.)
#
# Drive each available harness through docs/agents/feature-elicit.prompt.txt
# in a fresh scratch directory and copy the resulting session file into
# test-fixtures/<harness>/ at the workspace root.
#
# Run from a logged-in shell that already has each harness's CLI
# installed and authenticated. Harnesses whose CLIs aren't on PATH are
# skipped with a notice.
#
# Usage:
#   ./scripts/capture-elicit-fixtures.sh                # all harnesses
#   ./scripts/capture-elicit-fixtures.sh claude codex   # specific subset

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROMPT_FILE="$REPO_ROOT/docs/agents/feature-elicit.prompt.txt"
FIXTURES_ROOT="$REPO_ROOT/test-fixtures"

if [[ ! -f "$PROMPT_FILE" ]]; then
    echo "missing prompt: $PROMPT_FILE" >&2
    exit 1
fi

PROMPT="$(cat "$PROMPT_FILE")"
ALL_HARNESSES=(amp claude codex copilot gemini pi opencode)
SELECTED=("${@:-${ALL_HARNESSES[@]}}")

# Fresh scratch dir per harness so they can't see each other's files.
SCRATCH_BASE="$(mktemp -d -t toolpath-elicit.XXXXXX)"
echo "scratch base: $SCRATCH_BASE"
echo

# Files-newer-than marker, set just before each harness runs so we can
# diff its session storage afterwards and pick out the newly written
# session file. Only `find -newer` is portable across macOS and Linux
# without ctime tricks.
mark() {
    local marker="$1"
    : > "$marker"
    sleep 1   # ensure mtime granularity covers files written next
}

newest_under() {
    local dir="$1"
    local pattern="$2"
    local marker="$3"
    if [[ ! -d "$dir" ]]; then
        return 1
    fi
    find "$dir" -type f -name "$pattern" -newer "$marker" -print 2>/dev/null \
        | head -1
}

# Tail the last 10 lines of a captured stderr file with a leading indent
# so it's visually nested under the harness's FAIL line.
dump_log() {
    local log="$1"
    if [[ -f "$log" ]]; then
        sed 's/^/    │ /' < "$log" | tail -10
    fi
}

# ── Harness drivers ──────────────────────────────────────────────────
#
# Each driver:
#   1. cd's into a fresh scratch dir
#   2. snapshots its session storage location with a marker file
#   3. invokes its CLI in non-interactive prompt mode
#   4. finds the new session file and copies it into fixtures/<name>/
#
# Edit the invocation lines if your harness version uses different
# flags; the driver shape stays the same.

drive_amp() {
    if ! command -v amp >/dev/null; then
        echo "amp: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/amp"; mkdir -p "$scratch"; cd "$scratch"
    echo "amp: running…"
    # Amp threads are server-authoritative — no local transcript exists, so
    # the snapshot-diff trick doesn't apply. Tee the --stream-json output to
    # learn the thread id, then fetch the canonical document with
    # `amp threads export`. `--no-archive-after-execute` keeps the fresh
    # thread out of auto-archive so it stays visible in `amp threads list`.
    # See docs/agents/feature-elicit.md and docs/agents/formats/amp/.
    local log="$scratch/.stderr.log"
    local stream="$scratch/.stream.jsonl"
    if ! amp -x "$PROMPT" --stream-json --no-archive-after-execute \
        </dev/null > "$stream" 2> "$log"; then
        echo "amp: FAIL (CLI returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local thread_id
    thread_id="$(grep -oE '"session_id":"T-[^"]+"' "$stream" \
        | head -1 | cut -d'"' -f4)"
    if [[ -z "$thread_id" ]]; then
        echo "amp: FAIL (no T-… session_id in the --stream-json output)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/amp"
    # The export embeds the scratch path (env.initial.trees); stabilize it.
    # Review the result by hand before committing — the piece-00 sanitization
    # checklist (usernames, ids, tokens) lives in test-fixtures/amp/README.md.
    if ! amp threads export "$thread_id" 2>> "$log" \
        | sed "s|$scratch|/tmp/elicit-scratch|g" \
        > "$FIXTURES_ROOT/amp/convo.json"; then
        echo "amp: FAIL (amp threads export $thread_id failed)"
        dump_log "$log"
        return 1
    fi
    echo "amp: OK → test-fixtures/amp/convo.json (thread $thread_id)"
}

drive_claude() {
    if ! command -v claude >/dev/null; then
        echo "claude: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/claude"; mkdir -p "$scratch"; cd "$scratch"
    # Snapshot existing project dirs before the run; whichever appears
    # afterward is the scratch's project dir. Claude sanitizes the cwd
    # path into the dir name (replacing `/`, `.`, and other punctuation
    # with `-`), and the exact rule isn't documented; snapshot-diff
    # avoids hard-coding it.
    local projs_before
    projs_before="$(ls -1 "$HOME/.claude/projects" 2>/dev/null | sort)"
    local log="$scratch/.stderr.log"
    echo "claude: running…"
    if ! claude -p "$PROMPT" >/dev/null 2> "$log"; then
        echo "claude: FAIL (CLI returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local projs_after
    projs_after="$(ls -1 "$HOME/.claude/projects" 2>/dev/null | sort)"
    local new_proj
    new_proj="$(comm -13 <(echo "$projs_before") <(echo "$projs_after") | head -1)"
    if [[ -z "$new_proj" ]]; then
        echo "claude: FAIL (no new project dir under ~/.claude/projects)"; return 1
    fi
    local proj_dir="$HOME/.claude/projects/$new_proj"
    local session
    session="$(ls -1t "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "claude: FAIL (no session file under $proj_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/claude"
    cp "$session" "$FIXTURES_ROOT/claude/convo.jsonl"
    echo "claude: OK → test-fixtures/claude/convo.jsonl"
}

drive_codex() {
    if ! command -v codex >/dev/null; then
        echo "codex: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/codex"; mkdir -p "$scratch"; cd "$scratch"
    local marker="$scratch/.marker"; mark "$marker"
    echo "codex: running…"
    # `--cd` pins workdir (without it codex resets to the nearest git ancestor).
    # `--skip-git-repo-check` avoids the "trusted directory" gate.
    # `-s workspace-write` lets the model write files in cwd.
    # `</dev/null` closes stdin so codex doesn't wait for piped input.
    local log="$scratch/.stderr.log"
    if ! codex exec --skip-git-repo-check --cd "$scratch" -s workspace-write "$PROMPT" \
        </dev/null >/dev/null 2> "$log"; then
        echo "codex: FAIL (CLI returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local session
    session="$(newest_under "$HOME/.codex/sessions" "rollout-*.jsonl" "$marker")"
    if [[ -z "$session" ]]; then
        echo "codex: FAIL (no new rollout under ~/.codex/sessions)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/codex"
    cp "$session" "$FIXTURES_ROOT/codex/convo.jsonl"
    echo "codex: OK → test-fixtures/codex/convo.jsonl"
}

drive_copilot() {
    if ! command -v copilot >/dev/null; then
        echo "copilot: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/copilot"; mkdir -p "$scratch"; cd "$scratch"
    echo "copilot: running…"
    # Isolated COPILOT_HOME so the real ~/.copilot session list stays clean;
    # config.json is copied over for auth. `--allow-all` skips tool prompts.
    local home="$scratch/.copilot-home"; mkdir -p "$home"
    cp "$HOME/.copilot/config.json" "$home/" 2>/dev/null || {
        echo "copilot: SKIP (~/.copilot/config.json missing — authenticate first)"; return 0
    }
    local log="$scratch/.stderr.log"
    if ! COPILOT_HOME="$home" copilot --allow-all -p "$PROMPT"         </dev/null >/dev/null 2> "$log"; then
        echo "copilot: FAIL (CLI returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local session
    session="$(ls -t "$home"/session-state/*/events.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "copilot: FAIL (no events.jsonl under isolated session-state)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/copilot"
    # Stabilize scratch paths embedded in args/results.
    sed "s|$scratch|/tmp/elicit-scratch|g; s|$home|/tmp/elicit-home|g"         "$session" > "$FIXTURES_ROOT/copilot/convo.jsonl"
    echo "copilot: OK → test-fixtures/copilot/convo.jsonl"
}

drive_gemini() {
    if ! command -v gemini >/dev/null; then
        echo "gemini: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/gemini"; mkdir -p "$scratch"; cd "$scratch"
    # Snapshot existing slot dirs before the run; whichever appears
    # afterward is the slot for this scratch (gemini disambiguates by
    # appending `-N` when a basename collides, so we can't predict it).
    local slots_before
    slots_before="$(ls -1 "$HOME/.gemini/tmp" 2>/dev/null | sort)"
    echo "gemini: running…"
    local log="$scratch/.stderr.log"
    if ! gemini --skip-trust --yolo -p "$PROMPT" </dev/null >/dev/null 2> "$log"; then
        echo "gemini: FAIL (CLI returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local slots_after
    slots_after="$(ls -1 "$HOME/.gemini/tmp" 2>/dev/null | sort)"
    local new_slot
    new_slot="$(comm -13 <(echo "$slots_before") <(echo "$slots_after") | head -1)"
    if [[ -z "$new_slot" ]]; then
        echo "gemini: FAIL (no new slot under ~/.gemini/tmp)"; return 1
    fi
    local chats_dir="$HOME/.gemini/tmp/$new_slot/chats"
    local session
    session="$(ls -1 "$chats_dir"/session-*.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        session="$(ls -1 "$chats_dir"/session-*.json 2>/dev/null | head -1)"
    fi
    if [[ -z "$session" ]]; then
        echo "gemini: FAIL (no session file under $chats_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/gemini"
    local out_ext="${session##*.}"
    cp "$session" "$FIXTURES_ROOT/gemini/convo.${out_ext}"
    # Sub-agent sibling dir lands next to the main file; copy it too if present.
    local stem
    stem="$(basename "$session" ".${out_ext}")"
    local sub_dir="$chats_dir/${stem#session-}"
    if [[ -d "$sub_dir" ]]; then
        cp -r "$sub_dir" "$FIXTURES_ROOT/gemini/"
    fi
    echo "gemini: OK → test-fixtures/gemini/convo.${out_ext}"
}

drive_pi() {
    if ! command -v pi >/dev/null; then
        echo "pi: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/pi"; mkdir -p "$scratch"; cd "$scratch"
    local projs_before
    projs_before="$(ls -1 "$HOME/.pi/agent/sessions" 2>/dev/null | sort)"
    echo "pi: running…"
    # Pi's non-interactive flag varies by version; `-p` is the
    # best-guess default. If your version uses something different,
    # edit this line.
    local log="$scratch/.stderr.log"
    # pi prints provider/auth errors to stdout, not stderr; capture both.
    if ! pi -p "$PROMPT" </dev/null > "$log" 2>&1; then
        echo "pi: FAIL (CLI returned non-zero — see docs/agents/feature-elicit.md for the manual workflow)"
        dump_log "$log"
        return 1
    fi
    local projs_after
    projs_after="$(ls -1 "$HOME/.pi/agent/sessions" 2>/dev/null | sort)"
    local new_proj
    new_proj="$(comm -13 <(echo "$projs_before") <(echo "$projs_after") | head -1)"
    if [[ -z "$new_proj" ]]; then
        echo "pi: FAIL (no new project dir under ~/.pi/agent/sessions)"; return 1
    fi
    local proj_dir="$HOME/.pi/agent/sessions/$new_proj"
    local session
    session="$(ls -1t "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "pi: FAIL (no session file under $proj_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/pi"
    cp "$session" "$FIXTURES_ROOT/pi/convo.jsonl"
    echo "pi: OK → test-fixtures/pi/convo.jsonl"
}

drive_opencode() {
    if ! command -v opencode >/dev/null; then
        echo "opencode: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/opencode"; mkdir -p "$scratch"; cd "$scratch"
    echo "opencode: running…"
    # `--format json` makes opencode emit structured events with explicit
    # `sessionID` fields, easy to extract with grep. Sessions live in
    # SQLite, so snapshot-diff doesn't apply; we get the id, then dump
    # via `opencode export <id>` which writes JSON to stdout (with a
    # chatty header on stderr we discard).
    local log="$scratch/.stderr.log"
    local run_out
    if ! run_out="$(opencode run --format json "$PROMPT" </dev/null 2> "$log")"; then
        echo "opencode: FAIL (opencode run returned non-zero)"
        dump_log "$log"
        return 1
    fi
    local session_id
    session_id="$(printf '%s\n' "$run_out" | grep -oE 'ses_[A-Za-z0-9]+' | head -1)"
    if [[ -z "$session_id" ]]; then
        echo "opencode: FAIL (no ses_… id found in run output)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/opencode"
    if ! opencode export "$session_id" 2>/dev/null > "$FIXTURES_ROOT/opencode/convo.json"; then
        echo "opencode: FAIL (opencode export $session_id failed)"; return 1
    fi
    echo "opencode: OK → test-fixtures/opencode/convo.json"
}

# ── Driver dispatch ──────────────────────────────────────────────────

ok=0
fail=0
for h in "${SELECTED[@]}"; do
    case "$h" in
        amp)      if drive_amp;      then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        claude)   if drive_claude;   then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        codex)    if drive_codex;    then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        copilot)  if drive_copilot;  then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        gemini)   if drive_gemini;   then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        pi)       if drive_pi;       then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        opencode) if drive_opencode; then ok=$((ok+1)); else fail=$((fail+1)); fi ;;
        *) echo "unknown harness: $h" >&2; fail=$((fail+1)) ;;
    esac
done

echo
echo "summary: ok=$ok fail=$fail (scratch left at $SCRATCH_BASE)"
echo
echo "Next: walk the completeness checklist in docs/agents/feature-elicit.md"
echo "for each captured fixture, then commit test-fixtures/."

exit $(( fail > 0 ? 1 : 0 ))
