#!/usr/bin/env bash
# The quick start on toolpath.net (site/index.md, "Start with the sessions
# already on your machine"), run step by step in a throwaway HOME with no
# network. Each step asserts what a user would see. Where a step cannot run as
# the site writes it, the script says so (CANNOT-AS-WRITTEN) and runs the
# nearest form that does, and site/.qual carries a concern for it.
#
#   curl ... https://toolpath.net/install.sh | bash   curl stub: the repo's scripts/install.sh
#                                                     and a release-shaped tarball of this build
#   cargo install path-cli                            `cargo install --path crates/path-cli`
#                                                     (crates.io is not reachable here)
#   path p cache sync                                 seven harness sessions placed in $HOME
#   path query '<the site's filter>'                  over the synced cache
#   path share                                        a stateful Pathbase stub on localhost
#   path resume <pathbase URL> --harness codex        the stub, and a `codex` stub that
#                                                     checks the rollout it is handed
#
# Needs: cargo, python3, sqlite3, tar, shasum/sha256sum (curl is stubbed).

set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
PASS=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { PASS=$((PASS + 1)); echo "ok: $*"; }
cannot() { echo "CANNOT-AS-WRITTEN: $*"; }

for need in cargo python3 sqlite3 tar; do
    command -v "$need" >/dev/null 2>&1 || fail "needs $need on PATH"
done

SANDBOX="$(mktemp -d)"
cleanup() {
    [ -n "${PB_PID:-}" ] && kill "$PB_PID" 2>/dev/null || true
    rm -rf "$SANDBOX"
}
trap cleanup EXIT

export HOME="$SANDBOX/home"
export TOOLPATH_CONFIG_DIR="$HOME/.toolpath"
unset XDG_DATA_HOME XDG_CONFIG_HOME CLAUDE_CONFIG_DIR CODEX_HOME PATHBASE_URL TOOLPATH_INSTALL_DIR TOOLPATH_BIN || true
mkdir -p "$HOME"
SYS_PATH="/usr/bin:/bin:/usr/sbin:/sbin"

# --- cargo install path-cli ------------------------------------------------
# The site's second install route. crates.io is out of reach, so the same
# command against this checkout; --debug reuses the gate's build.
CARGO_ROOT="$SANDBOX/cargo"
cargo install --quiet --locked --debug --path crates/path-cli --root "$CARGO_ROOT" >/dev/null 2>&1 \
    || fail "cargo install --path crates/path-cli failed"
BUILT="$CARGO_ROOT/bin/path"
WANT_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/path-cli/Cargo.toml | head -1)"
[ "$("$BUILT" --version)" = "path $WANT_VERSION" ] || fail "cargo-installed path --version is not $WANT_VERSION"
ok "cargo install (from this checkout, standing in for crates.io) installs path $WANT_VERSION"

# --- curl https://toolpath.net/install.sh | bash ---------------------------
# A curl stub answers the three URLs the flow fetches: the install script
# itself (served from scripts/install.sh, which site/eleventy.config.js
# publishes at /install.sh), the GitHub latest-release redirect, and the
# release assets (a tarball of the binary just built, with its .sha256).
RELEASE="$SANDBOX/release"
mkdir -p "$RELEASE" "$SANDBOX/payload"
cp "$BUILT" "$SANDBOX/payload/path"
for target in aarch64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-gnu; do
    tar -C "$SANDBOX/payload" -czf "$RELEASE/path-$target.tar.gz" path
    (cd "$RELEASE" && { sha256sum "path-$target.tar.gz" 2>/dev/null || shasum -a 256 "path-$target.tar.gz"; } >"path-$target.tar.gz.sha256")
done
STUBS="$SANDBOX/stubs"
mkdir -p "$STUBS"
cat >"$STUBS/curl" <<EOF
#!/usr/bin/env bash
out=""; url=""; prev=""
for a in "\$@"; do
    case "\$prev" in -o) out="\$a" ;; esac
    case "\$a" in -o|-w) prev="\$a" ;; http://*|https://*) url="\$a"; prev="" ;; *) prev="" ;; esac
done
echo "\$url" >>"$SANDBOX/curl.log"
emit() { if [ -n "\$out" ]; then cp "\$1" "\$out"; else cat "\$1"; fi; }
case "\$url" in
    https://toolpath.net/install.sh) emit "$ROOT/scripts/install.sh" ;;
    https://github.com/empathic/toolpath/releases/latest)
        printf '%s' "https://github.com/empathic/toolpath/releases/tag/v$WANT_VERSION" ;;
    https://github.com/empathic/toolpath/releases/download/v$WANT_VERSION/*)
        emit "$RELEASE/\$(basename "\$url")" ;;
    *) echo "curl stub: unexpected URL \$url" >&2; exit 22 ;;
esac
EOF
chmod +x "$STUBS/curl"

host_target() {
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) echo aarch64-apple-darwin ;;
        Linux-x86_64) echo x86_64-unknown-linux-musl ;;
        Linux-aarch64|Linux-arm64) echo aarch64-unknown-linux-gnu ;;
        *) echo "" ;;
    esac
}
TARGET="$(host_target)"
if [ -z "$TARGET" ]; then
    cannot "no release target for $(uname -s)-$(uname -m): install.sh refuses it (by design); skipping its run"
else
    # Exactly the site's line, with only curl stubbed.
    out="$(PATH="$STUBS:$SYS_PATH" bash -c "curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash" 2>&1)" \
        || fail "install.sh pipeline failed: $out"
    [ -x "$HOME/.local/bin/path" ] || fail "install.sh did not install to ~/.local/bin/path"
    [ "$("$HOME/.local/bin/path" --version)" = "path $WANT_VERSION" ] || fail "installed binary --version mismatch"
    grep -q "releases/download/v$WANT_VERSION/path-$TARGET.tar.gz" "$SANDBOX/curl.log" \
        || fail "install.sh did not fetch path-$TARGET.tar.gz"
    case "$out" in *"not in your PATH"*) ;; *) fail "install.sh did not say ~/.local/bin is off PATH" ;; esac
    ok "curl … toolpath.net/install.sh | bash installs path $WANT_VERSION ($TARGET) to ~/.local/bin and says it is off PATH"
fi

# The platform table, through a uname stub (no real download needed).
uname_as() {
    local s="$1" m="$2" d="$SANDBOX/uname-$1-$2"
    mkdir -p "$d"
    # The stub's own $1 must stay literal in the file it writes.
    # shellcheck disable=SC2016
    printf '#!/bin/sh\ncase "$1" in -s) echo %s ;; -m) echo %s ;; *) echo %s ;; esac\n' "$s" "$m" "$s" >"$d/uname"
    chmod +x "$d/uname"
    echo "$d"
}
for row in "Linux x86_64 x86_64-unknown-linux-musl" "Linux aarch64 aarch64-unknown-linux-gnu" "Darwin arm64 aarch64-apple-darwin"; do
    read -r os_name machine want_target <<<"$row"
    set -- "$os_name" "$machine" "$want_target"
    : >"$SANDBOX/curl.log"
    d="$(uname_as "$1" "$2")"
    PATH="$d:$STUBS:$SYS_PATH" TOOLPATH_INSTALL_DIR="$SANDBOX/plat-$3" bash scripts/install.sh >/dev/null 2>&1 \
        || fail "install.sh failed for $1/$2"
    grep -q "path-$3.tar.gz" "$SANDBOX/curl.log" || fail "$1/$2 did not resolve to $3"
done
ok "install.sh maps Linux x86_64 → musl, Linux aarch64 → gnu, macOS arm64 → apple-darwin"
d="$(uname_as Darwin x86_64)"
if out="$(PATH="$d:$STUBS:$SYS_PATH" TOOLPATH_INSTALL_DIR="$SANDBOX/intel" bash scripts/install.sh 2>&1)"; then
    fail "install.sh accepted an Intel Mac"
fi
case "$out" in *"cargo install path-cli"*) ;; *) fail "Intel Mac refusal does not point at cargo install: $out" ;; esac
ok "install.sh refuses an Intel Mac and points at cargo install path-cli"
if [ -n "$TARGET" ]; then
    echo tampered >>"$RELEASE/path-$TARGET.tar.gz"
    if PATH="$STUBS:$SYS_PATH" TOOLPATH_INSTALL_DIR="$SANDBOX/tampered" bash scripts/install.sh >/dev/null 2>&1; then
        fail "install.sh accepted a tampered tarball"
    fi
    [ ! -e "$SANDBOX/tampered/path" ] || fail "a tampered tarball left a binary behind"
    ok "install.sh refuses a tarball whose sha256 does not match"
fi

PATH_BIN="$BUILT"
p() { "$PATH_BIN" "$@"; }

# --- seven harnesses' sessions where they live -----------------------------
# Real fixture bytes where the store is a file layout (Claude Code, Codex, Pi,
# Copilot); `path p export` from the committed golden documents where the
# store is a database or keyed index (Gemini CLI, opencode, Cursor).
PROJ="$SANDBOX/proj"
mkdir -p "$PROJ"
claude_cwd="$(grep -o '"cwd":"[^"]*"' test-fixtures/claude/convo.jsonl | head -1 | sed 's/"cwd":"\(.*\)"/\1/')"
claude_sid="$(grep -o '"sessionId":"[^"]*"' test-fixtures/claude/convo.jsonl | head -1 | sed 's/.*:"\(.*\)"/\1/')"
claude_slug="$(printf '%s' "$claude_cwd" | tr '/_.' '---')"
mkdir -p "$HOME/.claude/projects/$claude_slug"
cp test-fixtures/claude/convo.jsonl "$HOME/.claude/projects/$claude_slug/$claude_sid.jsonl"

codex_id="$(head -1 test-fixtures/codex/convo.jsonl | sed 's/.*"payload":{"id":"\([^"]*\)".*/\1/')"
mkdir -p "$HOME/.codex/sessions/2026/05/07"
cp test-fixtures/codex/convo.jsonl "$HOME/.codex/sessions/2026/05/07/rollout-2026-05-07T18-27-42-$codex_id.jsonl"

pi_cwd="$(head -1 test-fixtures/pi/convo.jsonl | sed 's/.*"cwd":"\([^"]*\)".*/\1/')"
pi_id="$(head -1 test-fixtures/pi/convo.jsonl | sed 's/.*"id":"\([^"]*\)".*/\1/')"
pi_dir="--$(printf '%s' "${pi_cwd#/}" | tr '/' '-')--"
mkdir -p "$HOME/.pi/agent/sessions/$pi_dir"
cp test-fixtures/pi/convo.jsonl "$HOME/.pi/agent/sessions/$pi_dir/2026-05-07T18-36-14-561Z_$pi_id.jsonl"

copilot_sid="$(grep -o '"sessionId":"[^"]*"' test-fixtures/copilot/convo.jsonl | head -1 | sed 's/.*:"\(.*\)"/\1/')"
mkdir -p "$HOME/.copilot/session-state/$copilot_sid"
cp test-fixtures/copilot/convo.jsonl "$HOME/.copilot/session-state/$copilot_sid/events.jsonl"

golden_doc() { awk 'n>=2{print} /^---$/{n++}' "crates/toolpath-goldens/snapshots/document@$1.snap" >"$SANDBOX/doc-$1.json"; echo "$SANDBOX/doc-$1.json"; }
# Cursor's global store exists once Cursor.app has run; the export writes into
# it and refuses to create it. Make the empty store a first launch leaves.
case "$(uname -s)" in
    Darwin) CURSOR_GLOBAL="$HOME/Library/Application Support/Cursor/User/globalStorage" ;;
    *)      CURSOR_GLOBAL="$HOME/.config/Cursor/User/globalStorage" ;;
esac
mkdir -p "$CURSOR_GLOBAL"
sqlite3 "$CURSOR_GLOBAL/state.vscdb" \
    'CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
     CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);' \
    || fail "could not create an empty Cursor state.vscdb"
# Same for opencode: its database exists once opencode has run. The schema is
# the one opencode 1.18.18 creates (test-fixtures/opencode/schema-1.18.18.sql).
mkdir -p "$HOME/.local/share/opencode"
sqlite3 "$HOME/.local/share/opencode/opencode-stable.db" <test-fixtures/opencode/schema-1.18.18.sql \
    || fail "could not create an empty opencode database"
for h in gemini opencode cursor; do
    doc="$(golden_doc "$h")"
    err="$(p p export "$h" --input "$doc" --project "$PROJ" 2>&1 >/dev/null)" \
        || fail "p export $h --project could not place a session: $err"
done
ok "placed one session per harness: claude, codex, pi, copilot (fixture bytes); gemini, opencode, cursor (p export)"

# --- path p cache sync ------------------------------------------------------
out="$(p p cache sync 2>&1)" || fail "path p cache sync failed: $out"
ls_out="$(p p cache ls 2>&1)" || fail "path p cache ls failed: $ls_out"
for h in claude codex pi copilot gemini opencode cursor; do
    echo "$ls_out" | grep -q "^$h-\|[[:space:]]$h-\|\"$h-" || fail "cache sync ingested no $h session (cache ls: $ls_out)"
done
ok "path p cache sync ingests a session from each of the seven harnesses"
again="$(p p cache sync 2>&1)" || fail "second sync failed: $again"
[ "$(p p cache ls 2>&1)" = "$ls_out" ] || fail "a second sync changed the cache (not incremental)"
ok "a second path p cache sync changes nothing (incremental)"

# --- path query (the site's filter, verbatim) -------------------------------
FILTER='group_by(.step.actor) | map({actor: .[0].step.actor, output_tokens: ([.[].change[]?.structural.token_usage.output_tokens // 0] | add)})'
q="$(p query "$FILTER" 2>/dev/null)" || fail "path query with the site's filter failed"
python3 - "$q" <<'PY' || fail "query result is not the per-actor token table"
import json, sys
rows = json.loads(sys.argv[1])
assert isinstance(rows, list) and rows, "empty result"
for r in rows:
    assert set(r) == {"actor", "output_tokens"}, r
    assert isinstance(r["output_tokens"], (int, float)) and r["output_tokens"] >= 0, r
agents = [r for r in rows if str(r["actor"]).startswith("agent:")]
assert len(agents) >= 3, f"expected actors from several harnesses, got {rows}"
assert sum(r["output_tokens"] for r in agents) > 0, "no output tokens counted for any agent"
PY
ok "path query '<site filter>' returns output tokens per actor across the synced harnesses"

# README.md's quick-start queries, verbatim, over the same cache.
[[ "$(p query 'map(select(.dead_end)) | length' 2>/dev/null)" =~ ^[0-9]+$ ]] || fail "README's dead-end query did not print an integer"
ok "path query 'map(select(.dead_end)) | length' (README) prints an integer"
p query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))' 2>/dev/null | python3 -c 'import json,sys; rows=json.load(sys.stdin); assert isinstance(rows, list) and all(r["cache_id"].startswith("claude") for r in rows), rows' || fail "README's --source claude query failed or returned a non-claude row"
# The fixtures have no >50000-token turn, so that runs empty; the scope itself, over seven harnesses:
p query --source claude 'map(.cache_id) | unique' 2>/dev/null | python3 -c 'import json,sys; ids=json.load(sys.stdin); assert ids and all(i.startswith("claude-") for i in ids), ids' || fail "--source claude did not scope the synced cache to claude sessions"
ok "path query --source claude '<README filter>' (README) runs, and --source claude scopes the seven-harness cache to claude sessions only"

# --- path share -------------------------------------------------------------
# A stateful Pathbase stub: anonymous upload (the route `path share` uses
# when not logged in) and download, the shapes the openapi and the recorded
# cassette (crates/toolpath-webmock/cassettes/pathbase) show.
PB_PORT_FILE="$SANDBOX/pb.port"
python3 - "$PB_PORT_FILE" "$SANDBOX/pb-store" <<'PY' &
import http.server, json, os, sys, uuid, socketserver
port_file, store = sys.argv[1], sys.argv[2]
os.makedirs(store, exist_ok=True)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        if self.path.rstrip("/") != "/api/v1/u/anon/repos/pathstash/graphs":
            return self.send(404, {"error": "not found"})
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        doc = json.loads(body).get("document") or {}
        gid = str(uuid.uuid4())
        open(os.path.join(store, gid + ".json"), "wb").write(json.dumps(doc).encode())
        host = self.headers.get("host")
        self.send(201, {"id": gid, "repo_id": "00000000-0000-0000-0000-000000000a02", "name": None,
                        "toolpath_id": doc.get("graph", {}).get("id") or "", "title": None,
                        "header": {"graph": doc.get("graph", {})}, "visibility": "unlisted",
                        "created_at": "2026-10-19T00:00:00Z", "updated_at": "2026-10-19T00:00:00Z",
                        "path_count": len(doc.get("paths", [])), "document": {},
                        "url": f"http://{host}/u/anon/pathstash/graphs/{gid}"})
    def do_GET(self):
        parts = self.path.split("?")[0].strip("/").split("/")
        if len(parts) == 9 and parts[-1] == "download":
            f = os.path.join(store, parts[-2] + ".json")
            if os.path.exists(f):
                return self.send(200, json.load(open(f)))
        self.send(404, {"error": "not found"})
s = socketserver.TCPServer(("127.0.0.1", 0), H)
open(port_file, "w").write(str(s.server_address[1]))
s.serve_forever()
PY
PB_PID=$!
for _ in $(seq 1 50); do [ -s "$PB_PORT_FILE" ] && break; sleep 0.1; done
[ -s "$PB_PORT_FILE" ] || fail "Pathbase stub did not start"
PB="http://127.0.0.1:$(cat "$PB_PORT_FILE")"

if out="$(PATHBASE_URL="$PB" p share </dev/null 2>&1)"; then
    fail "bare 'path share' succeeded with no terminal; expected the interactive picker to refuse"
fi
case "$out" in *"uploading"*|*"/graphs/"*) fail "bare 'path share' got as far as an upload: $out" ;; esac
# Not a defect in the site's text: a person at a terminal gets the picker.
# A test has no terminal, so it drives the picker-free form of the same upload.
echo "note: bare 'path share' is interactive ($(echo "$out" | grep -i -m1 -E 'picker|terminal|tty|fzf' || echo "$out" | tail -1)); the test runs 'path share --harness claude --session <id> --project <dir>'"
share_out="$(p share --harness claude --session "$claude_sid" --project "$claude_cwd" --url "$PB" 2>&1)" \
    || fail "path share --harness claude … failed: $share_out"
SHARED_URL="$(echo "$share_out" | grep -o "$PB/u/anon/pathstash/graphs/[0-9a-f-]*" | head -1)"
[ -n "$SHARED_URL" ] || fail "path share printed no Pathbase URL: $share_out"
case "$share_out" in *"uploading anonymously"*) ;; *) fail "share did not say it uploaded anonymously when logged out: $share_out" ;; esac
ok "path share (not logged in) uploads anonymously and prints the graph URL"

# --- path resume <pathbase URL> --harness codex ------------------------------
# A codex stub stands in for the harness: it records argv and the rollout
# path the projection wrote, so the test can check codex would find it.
mkdir -p "$STUBS/codex-bin"
cat >"$STUBS/codex-bin/codex" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >"$SANDBOX/codex.argv"
pwd >"$SANDBOX/codex.cwd"
EOF
chmod +x "$STUBS/codex-bin/codex"
RESUME_DIR="$SANDBOX/resume-here"
mkdir -p "$RESUME_DIR"
before="$(find "$HOME/.codex/sessions" -name 'rollout-*.jsonl' | sort)"
out="$(cd "$RESUME_DIR" && PATH="$STUBS/codex-bin:$SYS_PATH" "$PATH_BIN" resume "$SHARED_URL" --harness codex --no-cache 2>&1)" \
    || fail "path resume <shared URL> --harness codex failed: $out"
[ -f "$SANDBOX/codex.argv" ] || fail "resume did not exec codex"
grep -qx resume "$SANDBOX/codex.argv" || fail "codex was not invoked with 'resume': $(tr '\n' ' ' <"$SANDBOX/codex.argv")"
after="$(find "$HOME/.codex/sessions" -name 'rollout-*.jsonl' | sort)"
new_rollout="$(comm -13 <(echo "$before") <(echo "$after") | head -1)"
[ -n "$new_rollout" ] || fail "resume wrote no new rollout under ~/.codex/sessions"
sid="$(head -1 "$new_rollout" | python3 -c 'import json,sys; print(json.loads(sys.stdin.readline())["payload"]["id"])')"
grep -qx "$sid" "$SANDBOX/codex.argv" || fail "codex was not handed the projected session id $sid"
head -1 "$new_rollout" | python3 -c 'import json,sys,os; m=json.loads(sys.stdin.readline())["payload"]; assert os.path.realpath(m["cwd"]) == os.path.realpath(sys.argv[1]), m["cwd"]' "$RESUME_DIR" \
    || fail "the projected rollout's cwd is not where codex is resumed"
ok "path resume <shared Pathbase URL> --harness codex writes a rollout under ~/.codex/sessions and execs 'codex resume <its id>' from the resume directory"

# The site's own resume line, with only the host rewritten to the stub. The
# stub serves the shared graph under the page's example id.
SITE_LINE="$(grep -o 'path resume https://pathbase\.dev/[^ ]* --harness codex' site/index.md | head -1)"
[ -n "$SITE_LINE" ] || fail "site/index.md has no 'path resume https://pathbase.dev/… --harness codex' line"
SITE_URL="${SITE_LINE#path resume }"
SITE_URL="${SITE_URL% --harness codex}"
cp "$SANDBOX/pb-store/${SHARED_URL##*/}.json" "$SANDBOX/pb-store/${SITE_URL##*/}.json"
rm -f "$SANDBOX/codex.argv"
out="$(cd "$RESUME_DIR" && PATH="$STUBS/codex-bin:$SYS_PATH" "$PATH_BIN" resume "$PB${SITE_URL#https://pathbase.dev}" --harness codex --no-cache 2>&1)" \
    || fail "the site's '$SITE_LINE' (host rewritten to the stub) failed: $out"
grep -qx resume "$SANDBOX/codex.argv" 2>/dev/null || fail "the site's resume line did not exec 'codex resume'"
ok "the site's '$SITE_LINE' resolves (host rewritten to the stub) and execs 'codex resume'"

# --- Stay in Claude Code: /plugin install path@toolpath ----------------------
# The marketplace, the manifest and install-on-first-use (ensure-path.sh's
# download/verify/install) are covered offline by scripts/test-plugin.sh.
# Here: the two slash commands the site names exist, drive the CLI through the
# wrapper, and the subcommands they call answer on the cache synced above.
PLUGIN=plugins/claude-code
python3 - <<'PYEOF' || fail "marketplace does not offer path@toolpath"
import json
m = json.load(open(".claude-plugin/marketplace.json"))
assert m["name"] == "toolpath" and any(p["name"] == "path" for p in m["plugins"])
PYEOF
for c in share query; do
    f="$PLUGIN/commands/$c.md"
    [ -f "$f" ] || fail "/path:$c is missing ($f)"
    grep -q "ensure-path.sh\" exec $c " "$f" || fail "/path:$c does not call 'path $c' through ensure-path.sh"
done
q_plugin="$(TOOLPATH_BIN="$BUILT" "$PLUGIN/scripts/ensure-path.sh" exec query "$FILTER" 2>/dev/null)" \
    || fail "/path:query's wrapper could not run the site's filter"
# `path query` syncs first, so the resume above (a new codex rollout) is in it
# now: compare with the CLI run at the same moment.
q_now="$(p query "$FILTER" 2>/dev/null)" || fail "path query failed after resume"
[ "$q_plugin" = "$q_now" ] || fail "/path:query via the wrapper differs from 'path query'"
TOOLPATH_BIN="$BUILT" "$PLUGIN/scripts/ensure-path.sh" exec share --help >/dev/null 2>&1 \
    || fail "/path:share's wrapper cannot reach 'path share'"
ok "path@toolpath offers /path:share and /path:query; through ensure-path.sh they reach 'path share' and 'path query' (same result as the CLI)"

echo "test-quickstart: $PASS checks passed"
