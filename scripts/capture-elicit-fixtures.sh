#!/usr/bin/env bash
# shellcheck disable=SC2012
# (uses `ls -1` to enumerate harness session/project directories whose
# names are UUIDs / project slugs — `find` would be more verbose without
# adding safety here.)
#
# Drive each available harness through docs/agents/feature-elicit.prompt.txt
# in a fresh scratch directory and copy the resulting session file into
# test-fixtures/<harness>/ at the workspace root. Then run a second pass
# that forces a context COMPACTION and captures it as
# test-fixtures/<harness>/convo-compacted.<ext>.
#
# Run from a logged-in shell that already has each harness's CLI
# installed and authenticated. Harnesses whose CLIs aren't on PATH are
# skipped with a notice.
#
# Usage:
#   ./scripts/capture-elicit-fixtures.sh                # all harnesses
#   ./scripts/capture-elicit-fixtures.sh claude codex   # specific subset
#
# Env opt-outs:
#   KEEP_SESSIONS=1    keep the scratch sessions in each agent's history
#                      (default: delete them after capture)
#   SKIP_COMPACTION=1  capture only the base convo.* fixtures, no compaction
#
# Compaction is forced per harness (see drivers): Claude honors `/compact`
# in continue+print mode; Codex auto-compacts when resumed with a tiny
# `model_context_window`; Pi auto-compacts when `reserveTokens` is raised;
# opencode is summarized via its headless HTTP server. Gemini compresses
# only in memory and persists nothing, so it has no compaction fixture.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROMPT_FILE="$REPO_ROOT/docs/agents/feature-elicit.prompt.txt"
FIXTURES_ROOT="$REPO_ROOT/test-fixtures"
KEEP_SESSIONS="${KEEP_SESSIONS:-0}"
SKIP_COMPACTION="${SKIP_COMPACTION:-0}"

if [[ ! -f "$PROMPT_FILE" ]]; then
    echo "missing prompt: $PROMPT_FILE" >&2
    exit 1
fi

PROMPT="$(cat "$PROMPT_FILE")"
ALL_HARNESSES=(claude codex copilot gemini pi opencode)
SELECTED=("${@:-${ALL_HARNESSES[@]}}")

# Fresh scratch dir per harness so they can't see each other's files.
SCRATCH_BASE="$(mktemp -d -t toolpath-elicit.XXXXXX)"
echo "scratch base: $SCRATCH_BASE"
[[ "$KEEP_SESSIONS" == "1" ]] && echo "KEEP_SESSIONS=1 — scratch sessions will be left in agent histories"
[[ "$SKIP_COMPACTION" == "1" ]] && echo "SKIP_COMPACTION=1 — capturing base fixtures only"
echo

# Safety net: if the Pi driver patched settings.json and the script dies
# mid-pass, restore it on exit. Format: "<backup-path>::<dest-path>".
PI_SETTINGS_RESTORE=""
# shellcheck disable=SC2329,SC2317  # invoked via the EXIT trap two lines down
on_exit() {
    if [[ -n "${PI_SETTINGS_RESTORE:-}" ]]; then
        local bak="${PI_SETTINGS_RESTORE%%::*}" dst="${PI_SETTINGS_RESTORE##*::}"
        [[ -f "$bak" ]] && cp "$bak" "$dst"
    fi
}
trap on_exit EXIT

# Files-newer-than marker, set just before a step runs so we can diff
# session storage afterwards and pick out the newly written file. Only
# `find -newer` is portable across macOS and Linux without ctime tricks.
mark() {
    local marker="$1"
    : > "$marker"
    sleep 1   # ensure mtime granularity covers files written next
}

newest_under() {
    local dir="$1" pattern="$2" marker="$3"
    [[ -d "$dir" ]] || return 1
    find "$dir" -type f -name "$pattern" -newer "$marker" -print 2>/dev/null | head -1
}

dump_log() {
    local log="$1"
    [[ -f "$log" ]] && sed 's/^/    │ /' < "$log" | tail -10
}

# Remove scratch sessions created during this run, unless KEEP_SESSIONS=1.
# Accepts a harness label followed by paths (files or dirs) to delete.
cleanup_paths() {
    local name="$1"; shift
    [[ "$KEEP_SESSIONS" == "1" ]] && return 0
    local removed=0 p
    for p in "$@"; do
        [[ -n "$p" && -e "$p" ]] && rm -rf "$p" && removed=1
    done
    [[ "$removed" == "1" ]] && echo "$name: cleaned scratch session(s)"
    return 0
}

# ── Harness drivers ──────────────────────────────────────────────────
#
# Each driver:
#   1. cd's into a fresh scratch dir
#   2. snapshots its session storage location
#   3. invokes its CLI in non-interactive prompt mode (base fixture)
#   4. forces a compaction and captures convo-compacted.* (unless SKIP_COMPACTION)
#   5. deletes the scratch session(s) from the agent's history (unless KEEP_SESSIONS)
#
# Edit the invocation lines if your harness version uses different flags;
# the driver shape stays the same. Compaction passes are best-effort: a
# failure warns and continues without aborting the base capture.

drive_claude() {
    if ! command -v claude >/dev/null; then
        echo "claude: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/claude"; mkdir -p "$scratch"; cd "$scratch"
    # Snapshot existing project dirs before the run; whichever appears
    # afterward is the scratch's project dir. Claude sanitizes the cwd
    # path into the dir name; snapshot-diff avoids hard-coding the rule.
    local projs_before
    projs_before="$(ls -1 "$HOME/.claude/projects" 2>/dev/null | sort)"
    local log="$scratch/.stderr.log"
    echo "claude: running…"
    if ! claude -p "$PROMPT" >/dev/null 2> "$log"; then
        echo "claude: FAIL (CLI returned non-zero)"; dump_log "$log"; return 1
    fi
    local projs_after new_proj
    projs_after="$(ls -1 "$HOME/.claude/projects" 2>/dev/null | sort)"
    new_proj="$(comm -13 <(echo "$projs_before") <(echo "$projs_after") | head -1)"
    if [[ -z "$new_proj" ]]; then
        echo "claude: FAIL (no new project dir under ~/.claude/projects)"; return 1
    fi
    local proj_dir="$HOME/.claude/projects/$new_proj" session
    session="$(ls -1t "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "claude: FAIL (no session file under $proj_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/claude"
    cp "$session" "$FIXTURES_ROOT/claude/convo.jsonl"
    echo "claude: OK → test-fixtures/claude/convo.jsonl"

    # ── compaction: `/compact` is honored in continue (-c) + print (-p)
    # mode and appends a compact_boundary + synthetic summary to the same
    # session file. ──
    if [[ "$SKIP_COMPACTION" != "1" ]]; then
        if claude -c -p "/compact" </dev/null >/dev/null 2>&1; then
            local cfile
            cfile="$(grep -l compact_boundary "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
            if [[ -n "$cfile" ]]; then
                cp "$cfile" "$FIXTURES_ROOT/claude/convo-compacted.jsonl"
                echo "claude: OK → test-fixtures/claude/convo-compacted.jsonl"
            else
                echo "claude: WARN (no compact_boundary after /compact; skipped compacted fixture)"
            fi
        else
            echo "claude: WARN (/compact failed; skipped compacted fixture)"
        fi
    fi
    cleanup_paths claude "$proj_dir"
}

drive_codex() {
    if ! command -v codex >/dev/null; then
        echo "codex: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/codex"; mkdir -p "$scratch"; cd "$scratch"
    local marker="$scratch/.marker"; mark "$marker"
    echo "codex: running…"
    local log="$scratch/.stderr.log"
    if ! codex exec --skip-git-repo-check --cd "$scratch" -s workspace-write "$PROMPT" \
        </dev/null >/dev/null 2> "$log"; then
        echo "codex: FAIL (CLI returned non-zero)"; dump_log "$log"; return 1
    fi
    local session
    session="$(newest_under "$HOME/.codex/sessions" "rollout-*.jsonl" "$marker")"
    if [[ -z "$session" ]]; then
        echo "codex: FAIL (no new rollout under ~/.codex/sessions)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/codex"
    cp "$session" "$FIXTURES_ROOT/codex/convo.jsonl"
    echo "codex: OK → test-fixtures/codex/convo.jsonl"

    # ── compaction: resuming with a tiny `model_context_window` makes the
    # next turn exceed it and auto-compact. The resume forks a new rollout
    # that carries the full history plus a `compacted` item. ──
    local compacted=""
    if [[ "$SKIP_COMPACTION" != "1" ]]; then
        local mk2="$scratch/.marker2"; mark "$mk2"
        if codex exec --skip-git-repo-check --cd "$scratch" -s workspace-write \
            -c model_context_window=4000 resume --last "Reply with the single word: ok." \
            </dev/null >/dev/null 2>&1; then
            compacted="$(newest_under "$HOME/.codex/sessions" "rollout-*.jsonl" "$mk2")"
            if [[ -n "$compacted" ]] && grep -q '"type":"compacted"' "$compacted"; then
                cp "$compacted" "$FIXTURES_ROOT/codex/convo-compacted.jsonl"
                echo "codex: OK → test-fixtures/codex/convo-compacted.jsonl"
            else
                echo "codex: WARN (no compacted item after resume; skipped compacted fixture)"
                compacted=""
            fi
        else
            echo "codex: WARN (resume failed; skipped compacted fixture)"
        fi
    fi
    cleanup_paths codex "$session" "$compacted"
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
    local slots_before
    slots_before="$(ls -1 "$HOME/.gemini/tmp" 2>/dev/null | sort)"
    echo "gemini: running…"
    local log="$scratch/.stderr.log"
    if ! gemini --skip-trust --yolo -p "$PROMPT" </dev/null >/dev/null 2> "$log"; then
        echo "gemini: FAIL (CLI returned non-zero)"; dump_log "$log"; return 1
    fi
    local slots_after new_slot
    slots_after="$(ls -1 "$HOME/.gemini/tmp" 2>/dev/null | sort)"
    new_slot="$(comm -13 <(echo "$slots_before") <(echo "$slots_after") | head -1)"
    if [[ -z "$new_slot" ]]; then
        echo "gemini: FAIL (no new slot under ~/.gemini/tmp)"; return 1
    fi
    local chats_dir="$HOME/.gemini/tmp/$new_slot/chats" session
    session="$(ls -1 "$chats_dir"/session-*.jsonl 2>/dev/null | head -1)"
    [[ -z "$session" ]] && session="$(ls -1 "$chats_dir"/session-*.json 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "gemini: FAIL (no session file under $chats_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/gemini"
    local out_ext="${session##*.}"
    cp "$session" "$FIXTURES_ROOT/gemini/convo.${out_ext}"
    local stem; stem="$(basename "$session" ".${out_ext}")"
    local sub_dir="$chats_dir/${stem#session-}"
    [[ -d "$sub_dir" ]] && cp -r "$sub_dir" "$FIXTURES_ROOT/gemini/"
    echo "gemini: OK → test-fixtures/gemini/convo.${out_ext}"

    # Gemini compresses context only in memory and never writes a marker
    # to disk, so there is no compaction fixture to capture.
    [[ "$SKIP_COMPACTION" != "1" ]] && echo "gemini: (no compaction persisted on disk — none to capture)"
    cleanup_paths gemini "$HOME/.gemini/tmp/$new_slot"
}

drive_pi() {
    if ! command -v pi >/dev/null; then
        echo "pi: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/pi"; mkdir -p "$scratch"; cd "$scratch"
    local projs_before
    projs_before="$(ls -1 "$HOME/.pi/agent/sessions" 2>/dev/null | sort)"
    echo "pi: running…"
    local log="$scratch/.stderr.log"
    # pi prints provider/auth errors to stdout, not stderr; capture both.
    if ! pi -p "$PROMPT" </dev/null > "$log" 2>&1; then
        echo "pi: FAIL (CLI returned non-zero — see docs/agents/feature-elicit.md for the manual workflow)"
        dump_log "$log"; return 1
    fi
    local projs_after new_proj
    projs_after="$(ls -1 "$HOME/.pi/agent/sessions" 2>/dev/null | sort)"
    new_proj="$(comm -13 <(echo "$projs_before") <(echo "$projs_after") | head -1)"
    if [[ -z "$new_proj" ]]; then
        echo "pi: FAIL (no new project dir under ~/.pi/agent/sessions)"; return 1
    fi
    local proj_dir="$HOME/.pi/agent/sessions/$new_proj" session
    session="$(ls -1t "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
    if [[ -z "$session" ]]; then
        echo "pi: FAIL (no session file under $proj_dir)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/pi"
    cp "$session" "$FIXTURES_ROOT/pi/convo.jsonl"
    echo "pi: OK → test-fixtures/pi/convo.jsonl"

    # ── compaction: Pi auto-compacts when context exceeds
    # `contextWindow - compaction.reserveTokens`. Temporarily raise
    # reserveTokens so the next turn compacts; restore settings after
    # (the EXIT trap is a safety net if we die mid-pass). ──
    if [[ "$SKIP_COMPACTION" != "1" ]]; then
        local settings="$HOME/.pi/agent/settings.json" bak
        if [[ -f "$settings" ]]; then
            bak="$(mktemp)"; cp "$settings" "$bak"
            PI_SETTINGS_RESTORE="$bak::$settings"
            python3 - "$settings" <<'PY' || true
import json,sys
p=sys.argv[1]
try: d=json.load(open(p))
except Exception: d={}
d["compaction"]={"enabled":True,"reserveTokens":10000000,"keepRecentTokens":400}
json.dump(d,open(p,"w"),indent=2)
PY
            pi -c -p "Now print the single word: done." </dev/null >/dev/null 2>&1 || true
            cp "$bak" "$settings"; rm -f "$bak"; PI_SETTINGS_RESTORE=""   # restore now
            local cfile
            cfile="$(grep -l '"type":"compaction"' "$proj_dir"/*.jsonl 2>/dev/null | head -1)"
            if [[ -n "$cfile" ]]; then
                cp "$cfile" "$FIXTURES_ROOT/pi/convo-compacted.jsonl"
                echo "pi: OK → test-fixtures/pi/convo-compacted.jsonl"
            else
                echo "pi: WARN (no compaction entry after forced auto-compact; skipped compacted fixture)"
            fi
        else
            echo "pi: WARN (no settings.json to tune; skipped compacted fixture)"
        fi
    fi
    cleanup_paths pi "$proj_dir"
}

drive_opencode() {
    if ! command -v opencode >/dev/null; then
        echo "opencode: SKIP (not on PATH)"; return 0
    fi
    local scratch="$SCRATCH_BASE/opencode"; mkdir -p "$scratch"; cd "$scratch"
    local db="$HOME/.local/share/opencode/opencode.db"
    echo "opencode: running…"
    local log="$scratch/.stderr.log" run_out
    if ! run_out="$(opencode run --format json "$PROMPT" </dev/null 2> "$log")"; then
        echo "opencode: FAIL (opencode run returned non-zero)"; dump_log "$log"; return 1
    fi
    local session_id
    session_id="$(printf '%s\n' "$run_out" | grep -oE 'ses_[A-Za-z0-9]+' | head -1)"
    # Fallback: the newest session row in the DB (run output format varies by version).
    [[ -z "$session_id" ]] && session_id="$(sqlite3 "$db" "select id from session order by time_created desc limit 1" 2>/dev/null)"
    if [[ -z "$session_id" ]]; then
        echo "opencode: FAIL (could not determine ses_… id)"; return 1
    fi
    mkdir -p "$FIXTURES_ROOT/opencode"
    if ! opencode export "$session_id" 2>/dev/null > "$FIXTURES_ROOT/opencode/convo.json"; then
        echo "opencode: FAIL (opencode export $session_id failed)"; return 1
    fi
    echo "opencode: OK → test-fixtures/opencode/convo.json"

    # ── compaction: opencode's headless server exposes the same summarize
    # the TUI `/compact` calls. Start a server, POST summarize with the
    # provider/model the session used, then re-export. ──
    if [[ "$SKIP_COMPACTION" != "1" ]]; then
        local pm prov modl
        pm="$(sqlite3 "$db" "select data from message where session_id='$session_id' order by time_created desc limit 8" 2>/dev/null | python3 -c "
import sys,json
prov=modl=''
for line in sys.stdin:
    line=line.strip()
    if not line: continue
    try: d=json.loads(line)
    except Exception: continue
    def w(o):
        global prov,modl
        if isinstance(o,dict):
            for k,v in o.items():
                if k=='providerID' and isinstance(v,str) and not prov: prov=v
                if k=='modelID' and isinstance(v,str) and not modl: modl=v
                w(v)
        elif isinstance(o,list):
            [w(x) for x in o]
    w(d)
print(prov+'|'+modl)
" 2>/dev/null)"
        prov="${pm%%|*}"; modl="${pm##*|}"
        if [[ -n "$prov" && -n "$modl" ]]; then
            local port=7771
            opencode serve --port "$port" >/dev/null 2>&1 &
            local svpid=$!
            local up=0
            for _ in $(seq 1 40); do curl -s "http://127.0.0.1:$port/app" >/dev/null 2>&1 && { up=1; break; }; sleep 0.5; done
            if [[ "$up" == "1" ]]; then
                curl -s -X POST "http://127.0.0.1:$port/session/$session_id/summarize" \
                    -H 'content-type: application/json' \
                    -d "{\"providerID\":\"$prov\",\"modelID\":\"$modl\"}" >/dev/null 2>&1 || true
                sleep 8
            fi
            kill "$svpid" 2>/dev/null || true
            if opencode export "$session_id" 2>/dev/null | grep -qE '"type": *"compaction"'; then
                opencode export "$session_id" 2>/dev/null > "$FIXTURES_ROOT/opencode/convo-compacted.json"
                echo "opencode: OK → test-fixtures/opencode/convo-compacted.json"
            else
                echo "opencode: WARN (summarize produced no compaction part; skipped compacted fixture)"
            fi
        else
            echo "opencode: WARN (could not resolve provider/model; skipped compacted fixture)"
        fi
    fi
    if [[ "$KEEP_SESSIONS" != "1" ]]; then
        opencode session delete "$session_id" >/dev/null 2>&1 && echo "opencode: cleaned scratch session(s)"
    fi
}

# ── Driver dispatch ──────────────────────────────────────────────────

ok=0
fail=0
for h in "${SELECTED[@]}"; do
    case "$h" in
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
