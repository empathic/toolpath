#!/usr/bin/env bash
# Walk `path sync` through its lifecycle against a Pathbase that has the
# sync API, using a synthetic Claude Code session in a sandboxed config
# directory. Nothing under ~/.toolpath or ~/.claude is read or written.
#
#   scripts/demo-sync.sh                 # pauses between acts
#   scripts/demo-sync.sh --auto          # no pauses
#   scripts/demo-sync.sh --cleanup       # remove the sandbox and the scheduled agent
#
#   PATHBASE_URL   server (default http://localhost:3000; see pathbase's scripts/demo-pathbase.sh)
#   DEMO_DIR       sandbox root (default /tmp/toolpath-sync-demo)
#   PATHBIN        path binary (default: cargo build -p path-cli)
#   DEMO_LOGIN=browser   log in through the browser instead of auto-registering a demo user
set -euo pipefail

PATHBASE_URL="${PATHBASE_URL:-http://localhost:3000}"
DEMO_DIR="${DEMO_DIR:-/tmp/toolpath-sync-demo}"
AUTO=""; CLEANUP=""
for arg in "$@"; do
  case "$arg" in
    --auto) AUTO=1 ;;
    --cleanup) CLEANUP=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -z "${PATHBIN:-}" ]; then
  (cd "$ROOT" && cargo build -q -p path-cli)
  PATHBIN="$ROOT/target/debug/path"
fi
for cmd in curl jq python3; do command -v "$cmd" >/dev/null || { echo "need $cmd" >&2; exit 1; }; done

export TOOLPATH_CONFIG_DIR="$DEMO_DIR/toolpath"
export CLAUDE_CONFIG_DIR="$DEMO_DIR/claude"
REAL_HOME="$HOME"
export HOME="$DEMO_DIR/home"
PROJECT="$DEMO_DIR/project"
SESSION_ID="7d3e0a1c-0000-4000-8000-00000000d3a0"
SLUG="$(printf '%s' "$PROJECT" | tr '/_.' '---')"
SESSION_FILE="$CLAUDE_CONFIG_DIR/projects/$SLUG/$SESSION_ID.jsonl"

if [ -n "$CLEANUP" ]; then
  "$PATHBIN" sync uninstall 2>/dev/null || true
  rm -rf "$DEMO_DIR"
  echo "removed $DEMO_DIR"
  exit 0
fi

say()   { printf '\n\033[1m%s\033[0m\n' "$*"; }
note()  { printf '   %s\n' "$*"; }
pause() { [ -n "$AUTO" ] || { printf '\n   [enter to continue] '; read -r; }; }
fail()  { printf '\n\033[31mdemo failed: %s\033[0m\n' "$*" >&2; exit 1; }

mkdir -p "$TOOLPATH_CONFIG_DIR" "$(dirname "$SESSION_FILE")" "$HOME" "$PROJECT"

# ── synthetic Claude Code session ─────────────────────────────────────
# Deterministic ids: turn N always gets the same UUIDs, so repeated demos
# produce the same step ids and the same graph shapes.
turn() {  # turn <n> "<user text>" "<assistant text>"
  python3 - "$SESSION_FILE" "$SESSION_ID" "$PROJECT" "$1" "$2" "$3" <<'PY'
import json, sys, uuid, datetime, os
path, sid, cwd, n, user, assistant = sys.argv[1:]
n = int(n)
def uid(tag): return str(uuid.uuid5(uuid.UUID(sid), f"{tag}-{n}"))
ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")
parent = None
if os.path.exists(path):
    for line in open(path):
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if rec.get("type") in ("user", "assistant") and rec.get("uuid"):
            parent = rec["uuid"]
common = {"isSidechain": False, "cwd": cwd, "sessionId": sid, "version": "2.1.212", "gitBranch": "main", "userType": "external"}
u = {"parentUuid": parent, "type": "user", "uuid": uid("user"), "timestamp": ts,
     "message": {"role": "user", "content": user}, **common}
a = {"parentUuid": u["uuid"], "type": "assistant", "uuid": uid("assistant"), "timestamp": ts,
     "message": {"model": "claude-fable-5", "id": "msg_" + uid("msg")[:12], "type": "message", "role": "assistant",
                 "content": [{"type": "text", "text": assistant}], "stop_reason": "end_turn",
                 "usage": {"input_tokens": 40 + 7 * n, "output_tokens": 25 + 3 * n}}, **common}
with open(path, "a") as f:
    f.write(json.dumps(u) + "\n"); f.write(json.dumps(a) + "\n")
PY
}

# ── login ─────────────────────────────────────────────────────────────
CREDS="$TOOLPATH_CONFIG_DIR/credentials.json"
if [ ! -s "$CREDS" ]; then
  say "Logging the sandboxed CLI in to $PATHBASE_URL"
  if [ "${DEMO_LOGIN:-}" = "browser" ]; then
    "$PATHBIN" auth login --url "$PATHBASE_URL"
  else
    # The local dev login (available when GitHub OAuth is not configured)
    # signs the browser in as `dev`; the CLI pairs with that same account.
    COOKIES="$DEMO_DIR/cookies"
    if curl -sS -o /dev/null -b "$COOKIES" -c "$COOKIES" -w '%{http_code}' "$PATHBASE_URL/api/v1/internal/auth/dev" | grep -q '^30'; then
      note "browser session: open $PATHBASE_URL/api/v1/internal/auth/dev (dev login, same user as the CLI)"
    else
      USER="demo$(date +%s)"
      curl -sS --fail-with-body -b "$COOKIES" -c "$COOKIES" -X POST "$PATHBASE_URL/api/v1/internal/auth/register" \
        -H 'content-type: application/json' \
        -d "{\"username\":\"$USER\",\"email\":\"$USER@example.com\",\"password\":\"correct-horse-battery-staple\"}" >/dev/null \
        || fail "could not log in at $PATHBASE_URL; is it a local Pathbase?"
      note "browser session: open $PATHBASE_URL/login and sign in as $USER / correct-horse-battery-staple"
    fi
    CODE=$(curl -sS --fail-with-body -b "$COOKIES" -c "$COOKIES" -X POST "$PATHBASE_URL/api/v1/auth/cli/request-grant" | jq -r .code)
    echo "$CODE" | "$PATHBIN" auth login --url "$PATHBASE_URL" | tail -1
  fi
fi
TOKEN=$(jq -r .token "$CREDS")
USERNAME=$(jq -r .user.username "$CREDS")
REPO_URL="$PATHBASE_URL/u/$USERNAME/pathstash"
meta() { curl -sS -H "authorization: Bearer $TOKEN" "$PATHBASE_URL/api/v1/u/$USERNAME/repos/pathstash/graphs/$1/meta"; }
state() { jq -r "$1 // empty" "$TOOLPATH_CONFIG_DIR/sync-state/claude-$SESSION_ID.json"; }

if [ ! -s "$SESSION_FILE" ]; then
  turn 1 "Add a --dry-run flag to the deploy script." "Added --dry-run: it prints the plan and exits before touching anything."
  turn 2 "Make it list the files it would change." "It now walks the manifest and prints each path with the action."
  turn 3 "Good. Also refuse to run on main without --force." "Done; main is refused unless --force is passed."
fi

say "1. Install: preflight the server and repo, write [sync], schedule the agent"
note "path sync install --include $PROJECT --harness claude --remote $REPO_URL"
"$PATHBIN" sync install --include "$PROJECT" --harness claude --remote "$REPO_URL" 2>&1 | sed 's/^/   /'
note "config: $TOOLPATH_CONFIG_DIR/config.toml"
pause

say "2. First pass: the session becomes a mutable graph"
# The agent that install just loaded runs its first pass right away, but it
# looks at the real ~/.claude, not this sandbox, so it finds nothing here.
for _ in $(seq 1 40); do [ -s "$TOOLPATH_CONFIG_DIR/sync-status.json" ] && break; sleep 0.5; done
if [ -s "$TOOLPATH_CONFIG_DIR/sync-status.json" ]; then
  note "the scheduled agent already ran once: $(jq -r '"\(.sessions) session(s) in scope"' "$TOOLPATH_CONFIG_DIR/sync-status.json") (the sandbox session lives outside its Claude root)"
fi
"$PATHBIN" sync --dry-run 2>&1 | sed 's/^/   /'
"$PATHBIN" sync 2>&1 | sed 's/^/   /'
G1=$(state .current.graph_id); [ -n "$G1" ] || fail "no current graph recorded"
meta "$G1" | jq -e '.state == "mutable" and .generation == 0' >/dev/null || fail "graph is not mutable at generation 0"
note "open $REPO_URL/graphs/$G1   (mutable pill, 6 steps)"
pause

say "3. The session continues; the next pass updates the graph in place"
turn 4 "One more thing: log the dry-run plan to a file too." "Logging to deploy-plan.log alongside the console output."
"$PATHBIN" sync 2>&1 | sed 's/^/   /'
meta "$G1" | jq -e '.generation == 1 and .paths[0].step_count == 8' >/dev/null || fail "expected generation 1 with 8 owned steps"
note "same URL, generation 1, 8 steps"
pause

say "4. Two hours pass with no activity (faked by rewinding the recorded activity); the next pass freezes it"
python3 - "$TOOLPATH_CONFIG_DIR/manifest.json" <<'PY'
import json, sys, datetime
p = sys.argv[1]; m = json.load(open(p))
for recs in m.values():
    for rec in recs.values():
        if rec.get("activity"):
            rec["activity"]["last_activity_at"] = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=3)).strftime("%Y-%m-%dT%H:%M:%SZ")
json.dump(m, open(p, "w"))
PY
"$PATHBIN" sync 2>&1 | sed 's/^/   /'
meta "$G1" | jq -e '.state == "frozen"' >/dev/null || fail "graph did not freeze"
note "open $REPO_URL/graphs/$G1   (frozen pill; a further pass changes nothing)"
"$PATHBIN" sync 2>&1 | sed 's/^/   /'
pause

say "5. Work resumes: new turns go into a continuation that references the frozen head"
turn 5 "Back to this. Can the plan be JSON?" "Yes: --format json emits the plan as a JSON array."
"$PATHBIN" sync 2>&1 | sed 's/^/   /'
G2=$(state .current.graph_id); [ -n "$G2" ] && [ "$G2" != "$G1" ] || fail "no continuation graph recorded"
meta "$G2" | jq -e --arg g1 "$G1" '.lineage.source_graph_id == $g1 and .paths[0].step_count == 2' >/dev/null || fail "continuation does not reference the frozen graph"
note "open $REPO_URL/graphs/$G2   (chain strip, 'continued from' divider, reads as one conversation)"
note "base.from = $(meta "$G2" | jq -r .base.from)"
pause

say "6. A manual share of the same session updates the continuation instead of duplicating it"
turn 6 "Ship it." "Shipped; the deploy script now has --dry-run, --force, and --format json."
"$PATHBIN" share --harness claude --session "$SESSION_ID" --project "$PROJECT" 2>&1 | sed 's/^/   /'
[ "$(state .current.graph_id)" = "$G2" ] || fail "share created a new graph instead of updating"
meta "$G2" | jq -e '.paths[0].step_count == 4' >/dev/null || fail "share did not update the continuation"
pause

say "7. Status"
"$PATHBIN" sync status 2>&1 | sed 's/^/   /'
"$PATHBIN" sync uninstall 2>&1 | sed 's/^/   /'
say "Done. Rerun with --cleanup to remove $DEMO_DIR."
export HOME="$REAL_HOME"
