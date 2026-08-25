#!/usr/bin/env bash
# Resume a Claude Code session on a remote host. Builds `path`
# from this checkout, projects one local Claude session to JSONL, ships
# the JSONL to an ssh host, launches claude under tmux there, and
# attaches.
#
# `path` does the local projection (`p import claude`, `p export
# claude`). This script does every other remote-resume step in shell,
# behind strict preconditions. Each step marked [shell] is a candidate
# to move into `path`.
#
# Usage:
#   scripts/resume-remote.sh <ssh-destination> [options]
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
#   --force            Re-ship over an existing remote session file. The
#                      remote state decides what a run does:
#                        fresh     no session file, no tmux session:
#                                  ship, launch, attach. --force has no
#                                  effect.
#                        existing  session file present, no tmux
#                                  session: launch on the remote file,
#                                  attach. --force ships the local
#                                  projection first and overwrites the
#                                  remote turns.
#                        live      tmux session running: attach.
#                                  --force is an error; stop the
#                                  session first (the error prints the
#                                  command).
#   --dry-run          Print the create, setup, and sync commands instead
#                      of running them, and stop after the plan. Every
#                      remote call is read-only. With --create, the run
#                      stops after printing the create command unless the
#                      VM already exists.
#
# Preconditions. Each one is checked before the first remote write. A
# failed check exits 1 with a message.
#   Local:
#   - cargo, git, ssh, jq, sha256sum are on PATH. rsync is on PATH
#     unless --no-sync. scp is on PATH with --setup.
#   - stdin is a terminal unless --dry-run (tmux attach needs one).
#   - <ssh-destination> matches [A-Za-z0-9][A-Za-z0-9@._-]*.
#   - --project and -C are absolute, physical (no symlink components,
#     no `..`, `.`, or empty component; a trailing `/` is stripped),
#     and match [A-Za-z0-9/._-]+. Every value this script
#     sends to the remote shell is restricted to that character set, so
#     the script quotes nothing and escapes nothing.
#   - --session is a UUID. At least one Claude session exists for
#     --project.
#   Remote (two read-only ssh calls):
#   - Each reply is exactly the TP_* lines the probe prints. A login
#     banner or a registration notice fails the run verbatim.
#   - Call 1, before any remote write: $HOME is absolute, claude is on
#     PATH or in a probed location, tmux is on PATH.
#   - Call 2, after setup and sync: <remote-dir> exists and is physical
#     (pwd -P returns it); the tmux session state and whether the
#     target session file exists are known. A live tmux session with
#     --force is an error.
#
# Steps (always in this order):
#   1. cargo build -p path-cli; the script runs target/debug/path and does
#      not touch any installed `path`.
#   2. Resolve the session. `path p import claude --no-cache` writes
#      the document to $TMPDIR/path-resume-remote/.
#      [shell] Mint the remote session id from the key-sorted document
#      (jq -S | sha256sum, formatted as a v4 UUID).
#   3. Optional VM creation (--create).
#   4. [shell] Call 1: remote home, claude path, tmux presence. Derive
#      <remote-dir> from the remote home unless -C is given.
#   5. `path p export claude --cwd <remote-dir> --session-id <ID>`
#      projects the document to JSONL rooted at the remote project
#      directory under the minted ID. [shell] Check the JSONL carries
#      the remote cwd and ID.
#      [shell] Compute the remote Claude project slug (/, _, and .
#      become -).
#   6. Optional remote seeding (--setup). rsync the working tree
#      (tracked, untracked, and uncommitted files, plus .git; minus
#      target/ and anything .gitignore lists) into the remote project
#      dir. --delete makes the remote mirror the local tree. The remote
#      has no Rust toolchain.
#   7. [shell] Call 2: the physical project dir, whether the tmux
#      session is live, and whether the target session file exists.
#      These decide the state: fresh, existing, or live (see --force).
#   8. Print the plan, including the state. --dry-run stops here.
#   9. [shell] fresh: ship the JSONL over ssh stdin (0600 via umask
#      077), launch `claude -r <id>` in a detached tmux session, attach.
#      existing: launch on the remote file, attach. live: attach.
#      Detach with ctrl-b d.
#  10. Print the reattach command.

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
FORCE=0

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
        --force) FORCE=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage ;;
    esac
done

if [[ -n "$VM_NAME" && -n "$REMOTE" ]]; then
    echo "pass either <ssh-destination> or --create <vm-name>, not both" >&2; exit 2
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
DEST_RE='^[A-Za-z0-9][A-Za-z0-9@._-]*$'

# check_plain_path <value> <what>
check_plain_path() {
    [[ $1 =~ $PLAIN_PATH_RE ]] || die "$2 must be absolute and match [A-Za-z0-9/._-]+ (got '$1')"
    case "$1/" in
        */../*) die "$2 must not contain a .. component (got '$1')" ;;
        */./*)  die "$2 must not contain a . component (got '$1')" ;;
        *//*)   die "$2 must not contain an empty component (got '$1')" ;;
    esac
}

# mint_uuid: stdin is the document bytes; stdout is a v4-shaped UUID
# built from the first 128 bits of their SHA-256.
mint_uuid() {
    local h
    h="$(sha256sum | cut -c1-32)"
    printf '%s-%s-4%s-%x%s-%s\n' "${h:0:8}" "${h:8:4}" "${h:13:3}" \
        $(( (16#${h:16:1} & 3) | 8 )) "${h:17:3}" "${h:20:12}"
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
need cargo; need git; need ssh; need jq; need sha256sum
[[ $SYNC -eq 0 ]] || need rsync
[[ $SETUP -eq 0 ]] || need scp
[[ $DRY_RUN -eq 1 || -t 0 ]] || die "stdin is not a terminal; tmux attach needs one (pass --dry-run to stop before attach)"
[[ $REMOTE =~ $DEST_RE ]] || die "<ssh-destination> must match $DEST_RE (got '$REMOTE')"
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
run cargo build -q -p path-cli --manifest-path "$ROOT/Cargo.toml"
PATH_BIN="$ROOT/target/debug/path"
"$PATH_BIN" --version

# ── 2. Resolve the session ────────────────────────────────────────────────

step "Resolve session"
if [[ -z "$SESSION" ]]; then
    # TSV columns: project, session id, last activity, messages, first message.
    ROW="$("$PATH_BIN" p list claude --project "$PROJECT" --format tsv | sort -t $'\t' -k3,3r | head -n 1)"
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

# [shell] Mint the remote session id from the key-sorted document.
REMOTE_ID="$(jq -cS . "$DOC" | mint_uuid)"
[[ $REMOTE_ID =~ $UUID_RE ]] || die "minted id is not a UUID: $REMOTE_ID"
[[ $REMOTE_ID != "$SESSION" ]] || die "minted id equals the source session id"
TMUX_NAME="path-${SESSION:0:8}"
echo "remote session id: $REMOTE_ID"

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

# ── 4. [shell] Call 1: remote home, claude, tmux ──────────────────────────

# probe_script: read-only. Prints one `TP_<NAME>=<value>` line per fact.
probe_script() {
    cat <<'EOF'
set -u
printf 'TP_HOME=%s\n' "$HOME"
c=''
if command -v claude >/dev/null 2>&1; then c=$(command -v claude); fi
for p in .local/bin/claude .claude/local/claude .npm-global/bin/claude; do
    if [ -z "$c" ] && [ -x "$HOME/$p" ]; then c="$HOME/$p"; fi
done
printf 'TP_CLAUDE=%s\n' "$c"
if command -v tmux >/dev/null 2>&1; then t=ok; else t=missing; fi
printf 'TP_TMUX=%s\n' "$t"
EOF
}

step "Probe $REMOTE (read-only)"
show "ssh -n $REMOTE <probe script>"
remote_facts "$(probe_script)" TP_HOME TP_CLAUDE TP_TMUX
REMOTE_HOME="${PF_VALS[0]}"
check_plain_path "$REMOTE_HOME" "remote \$HOME"
REMOTE_CLAUDE="${PF_VALS[1]}"
[[ -n $REMOTE_CLAUDE ]] || die "claude not found on $REMOTE; probed PATH, ~/.local/bin, ~/.claude/local, ~/.npm-global/bin"
check_plain_path "$REMOTE_CLAUDE" "remote claude path"
[[ ${PF_VALS[2]} == ok ]] || die "tmux not found on $REMOTE"

if [[ -z "$REMOTE_DIR" ]]; then
    case "$PROJECT" in
        "$HOME"/*) REMOTE_DIR="$REMOTE_HOME/${PROJECT#"$HOME"/}" ;;
        "$HOME")   REMOTE_DIR="$REMOTE_HOME" ;;
        *) die "local project $PROJECT is not under \$HOME; pass -C <remote-dir>" ;;
    esac
    check_plain_path "$REMOTE_DIR" "the derived remote project dir"
fi
echo "ok: home=$REMOTE_HOME claude=$REMOTE_CLAUDE tmux=ok"
echo "remote project dir: $REMOTE_DIR"

# ── 5. Project the session ────────────────────────────────────────────────

step "Project session $SESSION to JSONL"
JSONL="$WORK_DIR/$REMOTE_ID.jsonl"
run "$PATH_BIN" p export claude --input "$DOC" --cwd "$REMOTE_DIR" --session-id "$REMOTE_ID" >"$JSONL"
N_CWD="$(grep -cF "\"cwd\":\"$REMOTE_DIR\"" "$JSONL" || true)"
[[ $N_CWD -gt 0 ]] || die "the projected JSONL has no cwd equal to $REMOTE_DIR"
N_SID="$(grep -cF "\"sessionId\":\"$REMOTE_ID\"" "$JSONL" || true)"
[[ $N_SID -gt 0 ]] || die "the projected JSONL has no sessionId equal to $REMOTE_ID"

# [shell] Claude project slug: /, _, and . become -.
SLUG="$(printf '%s' "$REMOTE_DIR" | tr '/_.' '---')"
SLUG_DIR="$REMOTE_HOME/.claude/projects/$SLUG"
TARGET="$SLUG_DIR/$REMOTE_ID.jsonl"

SHIP_CMD="umask 077; mkdir -p $SLUG_DIR && cat > $TARGET"
LAUNCH_CMD="tmux new-session -d -s $TMUX_NAME -c $REMOTE_DIR 'env LANG=C.UTF-8 $REMOTE_CLAUDE -r $REMOTE_ID'"
ATTACH_CMD="tmux attach-session -d -t =$TMUX_NAME"

# ── 6. Seed (optional) and sync ───────────────────────────────────────────

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

if [[ $SYNC -eq 1 ]]; then
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
fi

# ── 7. [shell] Call 2: project dir, tmux state ────────────────────────────

# preflight_script: read-only. Prints one `TP_<NAME>=<value>` line per fact.
preflight_script() {
    sed "s|__DIR__|$REMOTE_DIR|; s|__TMUX__|$TMUX_NAME|; s|__TARGET__|$TARGET|" <<'EOF'
set -u
if cd __DIR__ 2>/dev/null; then p=$(pwd -P); else p=''; fi
printf 'TP_PWD=%s\n' "$p"
if tmux has-session -t =__TMUX__ 2>/dev/null; then s=live; else s=none; fi
printf 'TP_SESSION=%s\n' "$s"
if [ -e __TARGET__ ]; then e=yes; else e=no; fi
printf 'TP_TARGET=%s\n' "$e"
EOF
}

step "Preflight $REMOTE (read-only)"
show "ssh -n $REMOTE <preflight script>"
remote_facts "$(preflight_script)" TP_PWD TP_SESSION TP_TARGET
DIR_NOTE=""
case "${PF_VALS[0]}" in
    "$REMOTE_DIR") ;;
    "")
        # Without --dry-run, setup and sync created the dir before this
        # call, so a missing dir here means both were off.
        if [[ $DRY_RUN -eq 1 && $SETUP -eq 1 ]]; then
            DIR_NOTE="missing; --setup creates it"
        elif [[ $DRY_RUN -eq 1 && $SYNC -eq 1 ]]; then
            DIR_NOTE="missing; sync creates it"
        else
            die "project directory $REMOTE_DIR does not exist on $REMOTE; pass --setup, drop --no-sync, or pass -C"
        fi
        ;;
    *) die "project directory $REMOTE_DIR is not physical on $REMOTE (it resolves to ${PF_VALS[0]}); pass -C ${PF_VALS[0]}" ;;
esac
TMUX_STATE="${PF_VALS[1]}"
[[ $TMUX_STATE == live || $TMUX_STATE == none ]] || die "bad tmux state '$TMUX_STATE'"
TARGET_EXISTS="${PF_VALS[2]}"
[[ $TARGET_EXISTS == yes || $TARGET_EXISTS == no ]] || die "bad target state '$TARGET_EXISTS'"
echo "ok: dir=$REMOTE_DIR${DIR_NOTE:+ ($DIR_NOTE)} session=$TMUX_STATE target=$TARGET_EXISTS"

KILL_CMD="ssh $REMOTE tmux kill-session -t =$TMUX_NAME"
if [[ $TMUX_STATE == live ]]; then
    [[ $FORCE -eq 0 ]] || die "tmux session $TMUX_NAME is live on $REMOTE; --force cannot re-ship under a running claude. Stop it first: $KILL_CMD"
    STATE=live
    SHIP=0
    STATE_NOTE="attach. The live session keeps the content of its original push. To push the current content: $KILL_CMD, then re-run."
elif [[ $TARGET_EXISTS == yes && $FORCE -eq 0 ]]; then
    STATE=existing
    SHIP=0
    STATE_NOTE="launch on the remote file, attach. The remote turns are kept. --force re-ships and overwrites them."
elif [[ $TARGET_EXISTS == yes ]]; then
    STATE=existing
    SHIP=1
    STATE_NOTE="--force: ship, launch, attach. The remote turns under $REMOTE_ID are overwritten."
else
    STATE=fresh
    SHIP=1
    STATE_NOTE="ship, launch, attach."
fi

# ── 8. Plan ───────────────────────────────────────────────────────────────

step "Plan"
cat <<EOF
  remote:        $REMOTE
  remote home:   $REMOTE_HOME
  claude:        $REMOTE_CLAUDE
  project dir:   $REMOTE_DIR${DIR_NOTE:+ ($DIR_NOTE)}
  session id:    $REMOTE_ID (source $SESSION)
  session file:  $TARGET (exists: $TARGET_EXISTS)
  tmux session:  $TMUX_NAME ($TMUX_STATE)
  jsonl:         $JSONL ($(wc -c <"$JSONL") bytes; $N_CWD cwd and $N_SID sessionId keys)
  state:         $STATE: $STATE_NOTE
EOF
[[ $SHIP -eq 0 ]] || echo "  ship:    ssh $REMOTE \"$SHIP_CMD\" < $JSONL"
[[ $STATE == live ]] || echo "  launch:  ssh $REMOTE \"$LAUNCH_CMD\""
echo "  attach:  ssh -t $REMOTE \"$ATTACH_CMD\""
if [[ $DRY_RUN -eq 1 ]]; then
    echo "Dry run: nothing was written or launched."
    exit 0
fi
pause

# ── 9. [shell] Ship, launch, attach ───────────────────────────────────────

if [[ $SHIP -eq 1 ]]; then
    step "Ship $JSONL to $REMOTE:$TARGET"
    show "ssh $REMOTE \"$SHIP_CMD\" < $JSONL"
    ssh -o BatchMode=yes "$REMOTE" "$SHIP_CMD" <"$JSONL"
fi
if [[ $STATE != live ]]; then
    step "Launch $TMUX_NAME in $REMOTE_DIR"
    show "ssh $REMOTE \"$LAUNCH_CMD\""
    ssh -n -o BatchMode=yes "$REMOTE" "$LAUNCH_CMD"
else
    echo "tmux session $TMUX_NAME is live on $REMOTE; attaching"
fi

step "Attach (detach with ctrl-b d)"
show "ssh -t $REMOTE \"$ATTACH_CMD\""
set +e
ssh -t "$REMOTE" "$ATTACH_CMD"
STATUS=$?
set -e

# ── 10. Reattach ──────────────────────────────────────────────────────────

step "Detached (exit $STATUS)"
cat <<EOF
Reattach (the live tmux session is reused, nothing is re-shipped):
  ssh -t $REMOTE "$ATTACH_CMD"

Inspect on the remote:
  ssh $REMOTE tmux ls
  ssh $REMOTE ls -la $SLUG_DIR

The remote has the working tree but no Rust toolchain. To build there, ask
the remote session to install rustup (rust-toolchain.toml pins the version).
EOF
if [[ -n "$VM_NAME" ]]; then
    cat <<EOF

Tear down the VM when finished:
  ssh exe.dev rm $VM_NAME
EOF
fi
