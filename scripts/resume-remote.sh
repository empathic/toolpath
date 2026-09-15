#!/usr/bin/env bash
# Resume a Claude Code session on a remote host. Builds `path`
# from this checkout, seeds and syncs the host, and hands off to
# `path resume --remote`, which plans, ships, launches claude under
# tmux, and attaches.
#
# `path` does the resume (`p import claude`, `resume --remote`). This
# script does the host bootstrap in shell: VM creation, credential
# seeding, and the working-tree sync. Each step marked [shell] is a
# candidate to move into `path`.
#
# Usage:
#   scripts/resume-remote.sh <user@host> [options]
#   scripts/resume-remote.sh --create <vm-name> [options]
#
# Options:
#   --create <name>    Create an exe.dev VM named <name> first
#                      (`ssh exe.dev new`), install tmux on it via the
#                      first-boot setup script, wait until ssh and tmux
#                      answer, then continue with --setup against
#                      exedev@<name>.exe.xyz. The exeuntu image ships
#                      claude at /usr/local/bin/claude.
#   --session <id>     Claude session id to push. Default: the newest
#                      session recorded for --project.
#   --project <dir>    Local project directory the session belongs to.
#                      Default: the current directory.
#   -C <remote-dir>    Remote project directory (absolute, physical).
#                      Default: the local project directory with $HOME
#                      swapped for the remote home.
#   --setup            Seed the remote before the run: create ~/.claude
#                      and the project directory, copy
#                      ~/.claude/.credentials.json, and write a minimal
#                      ~/.claude.json that skips onboarding and trusts the
#                      project directory. Idempotent.
#   --no-sync          Do not push the working tree to the remote.
#   --no-pause         Do not wait for Enter between steps.
#   --dry-run          Print the setup and sync commands instead of
#                      running them, and stop after the `path resume`
#                      plan. Every remote call is read-only. With
#                      --create, the run stops after printing the
#                      create command unless the VM already exists.
#
# Preconditions. Each one is checked before the first remote write. A
# failed check exits 1 with a message.
#   Local:
#   - cargo, git, ssh, jq are on PATH. rsync is on PATH
#     unless --no-sync. scp is on PATH with --setup.
#   - stdin is a terminal unless --dry-run (tmux attach needs one).
#   - <user@host> has both parts, each matching
#     [A-Za-z0-9][A-Za-z0-9._-]*. `path resume --remote` requires the
#     user part.
#   - `ssh -G <user@host>` resolves to the value itself: the hostname
#     is its host part, the port is 22, the user is its user part, and
#     no ProxyCommand, ProxyJump, or CertificateFile is set. `path
#     resume --remote` reads no ~/.ssh/config, so a destination that
#     needs one seeds and syncs over OpenSSH and then fails at the
#     handoff.
#   - --project and -C are absolute, physical (no symlink components,
#     no `..`, `.`, or empty component; a trailing `/` is stripped),
#     and match [A-Za-z0-9/._-]+. Every value this script
#     sends to the remote shell is restricted to that character set, so
#     the script quotes nothing and escapes nothing.
#   - --session is a UUID. At least one Claude session exists for
#     --project.
#   Remote (one read-only ssh call before any remote write; `path
#   resume --remote` runs its own read-only probes after it):
#   - The reply is exactly the TP_* lines the probe prints. A login
#     banner or a registration notice fails the run verbatim.
#   - The probe reports $HOME and whether the target session file
#     exists. The file's absence means the run ships, which gates the
#     sync.
#
# Steps (always in this order):
#   1. cargo build -p path-cli --features resume-remote; the script runs
#      target/debug/path and does not touch any installed `path`.
#   2. Resolve the session. `path p import claude --no-cache` writes
#      the document to $TMPDIR/path-resume-remote/. The remote session
#      ID comes from `p export claude --content-addressed-session-id`:
#      the same document yields the same ID on every run.
#   3. Optional VM creation (--create).
#   4. [shell] The probe: remote home and whether the target session
#      file exists. Derive <remote-dir> from the remote home unless
#      -C is given. An absent file means the run ships.
#   5. Optional remote seeding (--setup). When the run ships, rsync
#      the working tree (tracked, untracked, and uncommitted files,
#      plus .git; minus target/ and anything .gitignore lists) into the
#      remote project dir. --delete makes the remote mirror the local
#      tree. The remote has no Rust toolchain. --dry-run prints these
#      commands instead of running them.
#   6. Hand off: `path resume <doc> --remote <dest> -C <remote-dir>`.
#      It re-probes read-only, prints the plan, and does what the
#      remote state asks: a live tmux session is attached to as is, a
#      present session file is launched as is, an absent file is
#      shipped first. To reset a remote session, delete its file on
#      the remote and re-run. --dry-run stops after its plan. Detach
#      with ctrl-b d; re-run the script to reattach.

set -euo pipefail

# ── Args ──────────────────────────────────────────────────────────────────

# usage [exit-code]: prints the header; exits 2 unless a code is given.
usage() {
    sed -n '2,/^set /p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
    exit "${1:-2}"
}

[[ $# -ge 1 ]] || usage
REMOTE=""
VM_NAME=""
case "$1" in
    -h|--help) usage 0 ;;
    --create) ;;
    *) REMOTE="$1"; shift ;;
esac

SESSION=""
PROJECT="$PWD"
REMOTE_DIR=""
SETUP=0
SYNC=1
PAUSE=1
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --create) VM_NAME="$2"; SETUP=1; shift 2 ;;
        --session) SESSION="$2"; shift 2 ;;
        --project) PROJECT="$2"; shift 2 ;;
        -C) REMOTE_DIR="$2"; shift 2 ;;
        --setup) SETUP=1; shift ;;
        --no-sync) SYNC=0; shift ;;
        --no-pause) PAUSE=0; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage ;;
    esac
done

if [[ -n "$VM_NAME" && -n "$REMOTE" ]]; then
    echo "pass either <user@host> or --create <vm-name>, not both" >&2; exit 2
fi
if [[ -n "$VM_NAME" ]]; then
    case "$VM_NAME" in
        *[!a-z0-9-]*|"") echo "--create name must match [a-z0-9-]+ (got '$VM_NAME')" >&2; exit 2 ;;
    esac
    REMOTE="exedev@$VM_NAME.exe.xyz"
fi

# ── Helpers ───────────────────────────────────────────────────────────────

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
show() { printf '\033[2m$ %s\033[0m\n' "$*" >&2; }
run()  { show "$@"; "$@"; }
skip() { printf '\033[2m$ %s\033[0m  (skipped: dry run)\n' "$*" >&2; }
die()  { echo "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "$1 is required on PATH"; }
pause() {
    [[ $PAUSE -eq 1 ]] || return 0
    printf '\n[Enter to continue] '
    read -r _ </dev/tty
}

PLAIN_PATH_RE='^/[A-Za-z0-9/._-]*$'
UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
DEST_RE='^[A-Za-z0-9][A-Za-z0-9._-]*@[A-Za-z0-9][A-Za-z0-9._-]*$'

# check_plain_path <value> <what>
check_plain_path() {
    [[ $1 =~ $PLAIN_PATH_RE ]] || die "$2 must be absolute and match [A-Za-z0-9/._-]+ (got '$1')"
    case "$1/" in
        */../*) die "$2 must not contain a .. component (got '$1')" ;;
        */./*)  die "$2 must not contain a . component (got '$1')" ;;
        *//*)   die "$2 must not contain an empty component (got '$1')" ;;
    esac
}

# remote_facts <script> <tag>...: runs <script> on the remote in one
# read-only ssh call and parses the reply into PF_VALS, one value per
# tag, in order. Each reply line is `<tag>=<value>`. Any other reply
# shape fails verbatim.
PF_VALS=()
remote_facts() {
    local script="$1" reply line i=0
    shift
    reply="$(ssh -n -o BatchMode=yes "$REMOTE" "$script")"
    PF_VALS=()
    while IFS= read -r line; do
        [[ $i -lt $# && $line == "${*:$((i + 1)):1}="* ]] \
            || die "unexpected reply from $REMOTE (a login banner or notice?):"$'\n'"$reply"
        PF_VALS[i]="${line#*=}"
        i=$((i + 1))
    done <<<"$reply"
    [[ $i -eq $# ]] || die "reply from $REMOTE has $i of $# lines:"$'\n'"$reply"
}

# ── Preconditions (local) ─────────────────────────────────────────────────

step "Preconditions"
need cargo; need git; need ssh; need jq
[[ $SYNC -eq 0 ]] || need rsync
[[ $SETUP -eq 0 ]] || need scp
[[ $DRY_RUN -eq 1 || -t 0 ]] || die "stdin is not a terminal; path resume --remote attaches and needs one (pass --dry-run to stop at the plan)"
[[ $REMOTE =~ $DEST_RE ]] || die "<user@host> must match $DEST_RE (got '$REMOTE')"
# `path resume --remote` reads no ~/.ssh/config, so the destination must
# mean the same thing to OpenSSH (which seeds and syncs) as to the
# command: its own host, port 22, its own user, and no proxy or
# certificate. `ssh -G` prints proxycommand, proxyjump, and
# certificatefile lines only when they are set.
SSH_CONFIG="$(ssh -G "$REMOTE" 2>/dev/null)" || die "ssh -G $REMOTE failed"
ssh_option() { awk -v key="$1" '$1 == key { print $2; exit }' <<<"$SSH_CONFIG"; }
[[ $(ssh_option hostname) == "${REMOTE#*@}" ]] || die "ssh resolves $REMOTE to host $(ssh_option hostname); path resume --remote reads no ~/.ssh/config"
[[ $(ssh_option port) == 22 ]] || die "ssh resolves $REMOTE to port $(ssh_option port); path resume --remote reads no ~/.ssh/config"
[[ $(ssh_option user) == "${REMOTE%@*}" ]] || die "ssh resolves $REMOTE to user $(ssh_option user); path resume --remote reads no ~/.ssh/config"
for key in proxycommand proxyjump certificatefile; do
    [[ -z $(ssh_option "$key") ]] || die "ssh sets $key for $REMOTE; path resume --remote reads no ~/.ssh/config"
done
[[ $HOME == /* ]] || die "local \$HOME is not absolute (got '$HOME')"
[[ -d $PROJECT ]] || die "--project is not a directory: $PROJECT"
PROJECT="$(cd "$PROJECT" && pwd -P)"
check_plain_path "$PROJECT" "--project"
[[ -z $SESSION || $SESSION =~ $UUID_RE ]] || die "--session must be a UUID (got '$SESSION')"
if [[ -n $REMOTE_DIR ]]; then
    REMOTE_DIR="${REMOTE_DIR%/}"
    check_plain_path "$REMOTE_DIR" "-C"
fi
if [[ $SETUP -eq 1 ]]; then
    CREDS="$HOME/.claude/.credentials.json"
    [[ -f "$CREDS" ]] || die "missing $CREDS; log into claude locally first"
    [[ -f "$HOME/.claude.json" ]] || die "missing ~/.claude.json"
fi
echo "ok: local tools, $REMOTE, $PROJECT"

# ── 1. Build ──────────────────────────────────────────────────────────────

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
step "Build path from $(git -C "$ROOT" rev-parse --short HEAD) ($(git -C "$ROOT" branch --show-current))"
run cargo build -q -p path-cli --features resume-remote --manifest-path "$ROOT/Cargo.toml"
PATH_BIN="$ROOT/target/debug/path"
"$PATH_BIN" --version

# ── 2. Resolve the session ────────────────────────────────────────────────

step "Resolve session"
if [[ -z "$SESSION" ]]; then
    # TSV columns: project, session id, last activity, messages, first message.
    ROW="$("$PATH_BIN" p list claude --project "$PROJECT" --format tsv | sort -t $'\t' -k3,3r | sed -n 1p)"
    [[ -n "$ROW" ]] || die "no Claude sessions found for $PROJECT"
    SESSION="$(cut -f2 <<<"$ROW")"
    [[ $SESSION =~ $UUID_RE ]] || die "session id from p list is not a UUID (got '$SESSION')"
    echo "newest session: $SESSION"
    echo "first message:  $(cut -f5 <<<"$ROW" | cut -c1-100)"
fi

WORK_DIR="${TMPDIR:-/tmp}/path-resume-remote"
(umask 077; mkdir -p "$WORK_DIR")
DOC="$WORK_DIR/$SESSION.json"
run "$PATH_BIN" p import claude --project "$PROJECT" --session "$SESSION" --no-cache >"$DOC"
echo "doc: $DOC ($(wc -c <"$DOC") bytes)"

# ── 3. Create the VM (optional) ───────────────────────────────────────────

if [[ -n "$VM_NAME" ]]; then
    step "Create exe.dev VM $VM_NAME"
    if ssh -n -o BatchMode=yes exe.dev ls --json 2>/dev/null | grep -q "\"$VM_NAME\""; then
        echo "VM $VM_NAME already exists; skipping creation" >&2
    elif [[ $DRY_RUN -eq 1 ]]; then
        skip "ssh exe.dev new --name=$VM_NAME --setup-script /dev/stdin"
        echo "Dry run: the VM does not exist yet, so nothing further can be probed."
        exit 0
    else
        # exeuntu ships claude but not tmux; exedev has passwordless sudo.
        SETUP_SCRIPT='#!/bin/sh
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq tmux
'
        show "ssh exe.dev new --name=$VM_NAME --setup-script /dev/stdin"
        printf '%s' "$SETUP_SCRIPT" | ssh exe.dev new "--name=$VM_NAME" --setup-script /dev/stdin

        step "Wait for $REMOTE"
        DEADLINE=$((SECONDS + 300))
        until ssh -n -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new \
                "$REMOTE" 'command -v tmux >/dev/null' 2>/dev/null; do
            if (( SECONDS >= DEADLINE )); then
                echo "timed out after 300s waiting for ssh and tmux on $REMOTE" >&2
                echo "check: ssh exe.dev ls -l; ssh $REMOTE cat /exe.dev/setup" >&2
                exit 1
            fi
            printf '.'
            sleep 5
        done
        echo
        echo "ssh and tmux are up on $REMOTE"
    fi
fi

# ── 4. [shell] Probe: remote home, target session file ───────────────────

step "Derive the remote session id"
JSONL="$WORK_DIR/$SESSION.jsonl"
run "$PATH_BIN" p export claude --input "$DOC" --content-addressed-session-id >"$JSONL"
# The remote session ID is the sessionId every line that carries one
# agrees on. `sort -u` yields one line only when they agree. The ID
# hashes the document, so it is independent of --cwd.
REMOTE_ID="$(jq -r '.sessionId // empty' "$JSONL" | sort -u)"
[[ $REMOTE_ID =~ $UUID_RE ]] || die "the projected JSONL does not carry one sessionId (got '$REMOTE_ID')"
echo "remote session id: $REMOTE_ID"

# probe_script: read-only. `path resume --remote` re-checks everything
# it plans on; this probe only feeds the pre-steps: the remote home
# (for -C derivation and the --setup trust entry) and whether the
# target session file exists (an absent file means the run ships,
# which gates the sync). __SUFFIX__ is the project path relative to
# the local home ('.' for the home itself); __DIR__ is the -C value or
# empty. Both match $PLAIN_PATH_RE or are empty, so plain substitution
# is safe. The slug must match `sanitize_project_path` in
# crates/toolpath-claude/src/paths.rs (/, _, and . become -).
probe_script() {
    sed "s|__DIR__|$REMOTE_DIR|; s|__SUFFIX__|$1|; s|__ID__|$REMOTE_ID|" <<'EOF'
set -u
printf 'TP_HOME=%s\n' "$HOME"
d='__DIR__'
if [ -z "$d" ]; then d="$HOME/__SUFFIX__"; d="${d%/.}"; fi
slug=$(printf '%s' "$d" | tr '/_.' '---')
if [ -e "$HOME/.claude/projects/$slug/__ID__.jsonl" ]; then e=yes; else e=no; fi
printf 'TP_TARGET=%s\n' "$e"
EOF
}

SUFFIX=""
if [[ -z $REMOTE_DIR ]]; then
    case "$PROJECT" in
        "$HOME"/*) SUFFIX="${PROJECT#"$HOME"/}" ;;
        "$HOME")   SUFFIX="." ;;
        *) die "local project $PROJECT is not under \$HOME; pass -C <remote-dir>" ;;
    esac
fi

step "Probe $REMOTE (read-only)"
show "ssh -n $REMOTE <probe script>"
remote_facts "$(probe_script "$SUFFIX")" TP_HOME TP_TARGET
REMOTE_HOME="${PF_VALS[0]}"
check_plain_path "$REMOTE_HOME" "remote \$HOME"
if [[ -z $REMOTE_DIR ]]; then
    if [[ $SUFFIX == . ]]; then REMOTE_DIR="$REMOTE_HOME"; else REMOTE_DIR="$REMOTE_HOME/$SUFFIX"; fi
    check_plain_path "$REMOTE_DIR" "the derived remote project dir"
fi
TARGET_EXISTS="${PF_VALS[1]}"
[[ $TARGET_EXISTS == yes || $TARGET_EXISTS == no ]] || die "bad target state '$TARGET_EXISTS'"
SHIP=1
[[ $TARGET_EXISTS == no ]] || SHIP=0
echo "ok: home=$REMOTE_HOME dir=$REMOTE_DIR target=$TARGET_EXISTS"

# ── 5. Seed (optional) and sync ───────────────────────────────────────────

if [[ $SETUP -eq 1 ]]; then
    step "Seed $REMOTE"
    if [[ $DRY_RUN -eq 1 ]]; then
        skip "ssh -n $REMOTE mkdir -p ~/.claude $REMOTE_DIR"
        skip "scp -pq $CREDS $REMOTE:.claude/"
        skip "jq '...' ~/.claude.json | ssh $REMOTE 'umask 077; cat > ~/.claude.json'"
    else
        run ssh -n "$REMOTE" "mkdir -p ~/.claude $REMOTE_DIR"
        run scp -pq "$CREDS" "$REMOTE:.claude/"
        show "jq '...' ~/.claude.json | ssh $REMOTE 'umask 077; cat > ~/.claude.json'"
        jq --arg dir "$REMOTE_DIR" '{
            hasCompletedOnboarding: true,
            theme: (.theme // "dark"),
            oauthAccount,
            projects: { ($dir): { hasTrustDialogAccepted: true } }
        }' "$HOME/.claude.json" | ssh "$REMOTE" 'umask 077; cat > ~/.claude.json'
        echo "seeded ~/.claude/.credentials.json and ~/.claude.json"
    fi
fi

if [[ $SYNC -eq 1 && $SHIP -eq 1 ]]; then
    step "Sync $PROJECT to $REMOTE:$REMOTE_DIR"
    if [[ $DRY_RUN -eq 1 ]]; then
        skip "ssh -n $REMOTE mkdir -p $REMOTE_DIR"
        skip "rsync -az --delete --exclude=target/ --filter=':- .gitignore' $PROJECT/ $REMOTE:$REMOTE_DIR/"
    else
        run ssh -n "$REMOTE" "mkdir -p $REMOTE_DIR"
        run rsync -az --delete --stats \
            --exclude=target/ --filter=':- .gitignore' \
            "$PROJECT/" "$REMOTE:$REMOTE_DIR/" | grep -E '^(Number of (regular )?files|Total transferred)'
    fi
elif [[ $SYNC -eq 1 ]]; then
    echo "sync skipped: the remote session file exists, so the run does not ship"
fi

# ── 6. Hand off to path resume ────────────────────────────────────────────

RESUME_ARGS=(resume "$DOC" --remote "$REMOTE" -C "$REMOTE_DIR")
[[ $DRY_RUN -eq 0 ]] || RESUME_ARGS+=(--dry-run)
step "path ${RESUME_ARGS[*]}"
echo "After a detach (ctrl-b d), re-run this script to reattach; the live tmux session is reused and nothing is re-shipped."
if [[ -n "$VM_NAME" ]]; then
    cat <<EOF
Tear down the VM when finished:
  ssh exe.dev rm $VM_NAME
EOF
fi
pause
exec "$PATH_BIN" "${RESUME_ARGS[@]}"
