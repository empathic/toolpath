#!/usr/bin/env bash
# Checks for the Claude Code plugin (plugins/claude-code): manifest
# consistency, and ensure-path.sh resolution/install behavior against a
# stubbed GitHub release. Fully offline; safe to run anywhere.

set -euo pipefail
cd "$(dirname "$0")/.."

PLUGIN="plugins/claude-code"
ENSURE="$PWD/$PLUGIN/scripts/ensure-path.sh"
PASS=0

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    PASS=$((PASS + 1))
    echo "ok: $*"
}

# --- manifest checks -------------------------------------------------------

python3 - "$PLUGIN" <<'PY' || fail "manifest checks"
import json, sys

plugin_dir = sys.argv[1]
market = json.load(open(".claude-plugin/marketplace.json"))
plugin = json.load(open(f"{plugin_dir}/.claude-plugin/plugin.json"))

assert market["name"] == "toolpath", "marketplace must be named 'toolpath'"
entries = [p for p in market["plugins"] if p["name"] == plugin["name"]]
assert len(entries) == 1, f"marketplace must list exactly one '{plugin['name']}' plugin"
entry = entries[0]
assert entry["source"] == f"./{plugin_dir}", f"marketplace source {entry['source']!r} != ./{plugin_dir}"
assert entry["version"] == plugin["version"], (
    f"version mismatch: marketplace {entry['version']} vs plugin.json {plugin['version']}"
)
assert plugin["name"] == "path", "plugin must be named 'path' (command namespace /path:*)"
PY
ok "manifests parse and agree (plugin 'path', versions match)"

bash -n "$ENSURE" || fail "ensure-path.sh does not parse"
for cmd in share query resume link-pr; do
    [ -f "$PLUGIN/commands/$cmd.md" ] || fail "missing command $cmd.md"
    grep -q "ensure-path.sh" "$PLUGIN/commands/$cmd.md" \
        || fail "$cmd.md does not invoke the ensure-path.sh wrapper"
done
ok "scripts parse; all four commands exist and use the wrapper"

# --- ensure-path.sh behavior ----------------------------------------------

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
export HOME="$SANDBOX/home"
export TOOLPATH_INSTALL_DIR="$SANDBOX/global-bin"
export TOOLPATH_CONFIG_DIR="$SANDBOX/toolpath"
mkdir -p "$HOME"

make_fake_path() {
    # A stand-in binary that answers --help/--version like the real CLI.
    local file="$1" version="$2" identity="${3:-Toolpath}"
    mkdir -p "$(dirname "$file")"
    cat >"$file" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
    --help) echo "Derive, query, and visualize $identity provenance documents" ;;
    --version) echo "path $version" ;;
    *) echo "fake-path ran: \$*" ;;
esac
EOF
    chmod +x "$file"
}

# 1. An existing Toolpath binary on PATH wins.
STUB1="$SANDBOX/stub1"
make_fake_path "$STUB1/path" "9.9.9"
out="$(PATH="$STUB1:$PATH" "$ENSURE")"
[ "$out" = "$STUB1/path" ] || fail "expected $STUB1/path, got $out"
ok "prefers an existing Toolpath binary on PATH"

# 2. exec mode runs the resolved binary.
out="$(PATH="$STUB1:$PATH" "$ENSURE" exec --version)"
[ "$out" = "path 9.9.9" ] || fail "exec mode: expected 'path 9.9.9', got '$out'"
ok "exec mode runs the resolved binary"

# 3. sessions mode lists Claude sessions for the cwd without $PWD on the
#    command line (inline slash-command contexts can't pass variables).
out="$(PATH="$STUB1:$PATH" "$ENSURE" sessions)"
[ "$out" = "fake-path ran: p list claude --project $PWD --format tsv" ] \
    || fail "sessions mode: unexpected invocation: $out"
ok "sessions mode lists sessions for the current directory"

# 4. current-session echoes the harness env var (or "unknown") and never
#    resolves a binary — it must work with no `path` and no network.
out="$(PATH="/usr/bin:/bin" CLAUDE_CODE_SESSION_ID="sess-123" "$ENSURE" current-session)"
[ "$out" = "sess-123" ] || fail "current-session: expected sess-123, got $out"
out="$(env -u CLAUDE_CODE_SESSION_ID PATH="/usr/bin:/bin" "$ENSURE" current-session)"
[ "$out" = "unknown" ] || fail "current-session without env: expected unknown, got $out"
ok "current-session reports the env var without resolving a binary"

# 5. A binary older than MIN_VERSION warns on stderr but still resolves.
STUB_OLD="$SANDBOX/stub-old"
make_fake_path "$STUB_OLD/path" "0.1.0"
err="$(PATH="$STUB_OLD:$PATH" "$ENSURE" 2>&1 >/dev/null)"
echo "$err" | grep -q "older than" || fail "expected an old-version warning, got: $err"
ok "warns when the resolved binary predates MIN_VERSION"

# --- stubbed download ------------------------------------------------------
# A curl stub serves a fixture release so the download/verify/install code
# path runs offline. It emulates the two calls ensure-path.sh makes:
# the /releases/latest redirect probe and the asset downloads.

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)            TARGET="aarch64-apple-darwin" ;;
    Linux-x86_64)            TARGET="x86_64-unknown-linux-musl" ;;
    Linux-aarch64|Linux-arm64) TARGET="aarch64-unknown-linux-gnu" ;;
    *)
        echo "skip: no release target for $(uname -s)-$(uname -m); download tests skipped"
        echo "test-plugin: $PASS checks passed"
        exit 0
        ;;
esac

export FIXTURE_DIR="$SANDBOX/release"
mkdir -p "$FIXTURE_DIR"
make_fake_path "$SANDBOX/payload/path" "9.9.9"
tar -C "$SANDBOX/payload" -czf "$FIXTURE_DIR/path-$TARGET.tar.gz" path
(
    cd "$FIXTURE_DIR"
    if command -v sha256sum &>/dev/null; then
        sha256sum "path-$TARGET.tar.gz" >"path-$TARGET.tar.gz.sha256"
    else
        shasum -a 256 "path-$TARGET.tar.gz" >"path-$TARGET.tar.gz.sha256"
    fi
)

CURL_STUB="$SANDBOX/curl-stub"
mkdir -p "$CURL_STUB"
cat >"$CURL_STUB/curl" <<'EOF'
#!/usr/bin/env bash
out=""; url=""; prev=""
for a in "$@"; do
    case "$prev" in
        -o) out="$a" ;;
    esac
    case "$a" in
        -o|-w) prev="$a" ;;
        http://*|https://*) url="$a"; prev="" ;;
        *) prev="" ;;
    esac
done
case "$url" in
    */releases/latest)
        printf '%s' "https://github.com/empathic/toolpath/releases/tag/v9.9.9" ;;
    */releases/download/v9.9.9/*)
        cp "$FIXTURE_DIR/$(basename "$url")" "$out" ;;
    *)
        echo "curl stub: unexpected URL $url" >&2
        exit 22 ;;
esac
EOF
chmod +x "$CURL_STUB/curl"

# The download tests run under a sanitized PATH (curl stub + system dirs
# only) so a Toolpath CLI installed on the host machine can't satisfy the
# resolution and mask the install logic.
SAFE_PATH="$CURL_STUB:/usr/bin:/bin:/usr/sbin:/sbin"
if PATH="/usr/bin:/bin:/usr/sbin:/sbin" command -v path >/dev/null 2>&1; then
    echo "skip: a 'path' binary exists in the system dirs; download tests skipped"
    echo "test-plugin: $PASS checks passed"
    exit 0
fi

# 4. No binary anywhere: downloads, verifies, installs to TOOLPATH_INSTALL_DIR.
out="$(PATH="$SAFE_PATH" "$ENSURE")"
[ "$out" = "$TOOLPATH_INSTALL_DIR/path" ] || fail "expected install to $TOOLPATH_INSTALL_DIR/path, got $out"
[ -x "$out" ] || fail "installed binary is not executable"
[ "$("$out" --version)" = "path 9.9.9" ] || fail "installed binary --version mismatch"
ok "clean environment: downloads, verifies, and installs globally"

# 5. Second run resolves the installed binary without curl (stub removed).
out="$(PATH="/usr/bin:/bin:/usr/sbin:/sbin" "$ENSURE")"
[ "$out" = "$TOOLPATH_INSTALL_DIR/path" ] || fail "re-resolve after install failed, got $out"
ok "subsequent runs resolve the installed binary without downloading"
rm -rf "$TOOLPATH_INSTALL_DIR"

# 6. A foreign `path` binary on PATH diverts the install to the fallback dir.
STUB_FOREIGN="$SANDBOX/stub-foreign"
make_fake_path "$STUB_FOREIGN/path" "1.0" "SomethingElse"
out="$(PATH="$STUB_FOREIGN:$SAFE_PATH" "$ENSURE")"
[ "$out" = "$TOOLPATH_CONFIG_DIR/bin/path" ] || fail "expected fallback install to $TOOLPATH_CONFIG_DIR/bin/path, got $out"
[ "$("$out" --version)" = "path 9.9.9" ] || fail "fallback-installed binary --version mismatch"
ok "foreign 'path' binary diverts the install to ~/.toolpath/bin"

# 7. A corrupted download fails checksum verification and installs nothing.
rm -rf "$TOOLPATH_CONFIG_DIR"
echo "tampered" >>"$FIXTURE_DIR/path-$TARGET.tar.gz"
if PATH="$STUB_FOREIGN:$SAFE_PATH" "$ENSURE" >/dev/null 2>&1; then
    fail "tampered tarball was accepted"
fi
[ ! -e "$TOOLPATH_CONFIG_DIR/bin/path" ] || fail "tampered tarball left an installed binary behind"
ok "tampered download fails checksum verification"

echo "test-plugin: $PASS checks passed"
