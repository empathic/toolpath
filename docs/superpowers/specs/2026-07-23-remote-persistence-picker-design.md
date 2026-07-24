# Remote resume: session-persistence backend picker

**Status:** design
**Date:** 2026-07-23
**Area:** `crates/path-cli/src/cmd_resume.rs` (`--remote` path)

## Problem

`path resume --remote <host>` launches an interactive
`ssh -t host 'claude -r <id>'`. When the SSH link drops, the harness dies and
the resumed work is lost. The branch already added an opt-in `--tmux` flag that
wraps the launch in `tmux new-session -A -s path-<id>` so it survives drops and
is re-attachable — but it is (a) opt-in, (b) hardcoded to tmux, and (c) blind to
whether the remote even has tmux.

We want the persistence layer to be a **first-class, selectable choice**, modeled
on the existing harness picker (`share`, `resume`, `p import`): probe what the
remote supports, present the options in the fuzzy picker, pre-select a sane
default, and let a flag skip the picker entirely.

## Goals

- Choose a remote session-persistence backend the same way we choose a harness:
  interactive fuzzy picker by default, `--persist <backend>` to skip it.
- Only offer backends actually installed on the remote (probe once, up front).
- Default to a working persistence backend when one is available, falling back
  to a plain launch when none is — never fail *because* a backend is missing.
- Support six backends spanning three launch mechanisms (below).
- Preserve `--tmux` as a deprecated alias so existing usage keeps working.

## Non-goals

- **Implementing** transport-layer persistence (mosh, Eternal Terminal) in v1.
  These wrap the SSH *connection*, not the remote command — a separate axis from
  `--persist`. v1 reserves the flag shape (`--via ssh|mosh`, below) but only
  implements `ssh`; mosh lands later.
- Restore-after-reboot / resurrection semantics beyond what each backend already
  does on its own.
- Local (non-`--remote`) resume is unchanged.

## The four independent layers

Remote resume touches four *separate* concerns that stack rather than compete.
Conflating any two is the classic mistake (expecting tmux to solve roaming, or
mosh to solve NAT). This design keeps them separate:

| Layer | Concern | How we express it |
|---|---|---|
| **Reachability** | Can I even connect? (NAT, mesh, broker) | **`~/.ssh/config`** — not a flag |
| **Connection-survival** | Does the link survive roaming/sleep? | **`--via ssh\|mosh\|et`** |
| **Session-survival** | Does work survive a client death? | **`--persist <backend>`** |
| **Workspace** | Panes/tabs/layout | a `--persist` backend (zellij) |

### Reachability is delegated to ssh config (no flag)

Tailscale SSH, WireGuard, Cloudflare Tunnel (`cloudflared access ssh`),
Teleport, Nebula, NetBird — all of these are expressed as a `Host` block
(`ProxyCommand`, `ProxyJump`, or just a mesh hostname). Because `--remote`
accepts a `~/.ssh/config` alias (see the parsing change on this branch),
resume rides every one of them for free — the CLI launch honors the full config.

**Known limitation:** the **libssh2 probe/ship half does not honor
`ProxyCommand`/`ProxyJump`** — libssh2 dials a raw TCP socket. So a host reachable
*only* through a broker/jump (Cloudflare Tunnel, a bastion) will launch fine but
**fail at the ship step**. Mitigation (candidate, not v1): when the libssh2 dial
fails and the config has a `ProxyCommand`, fall back to shipping over the `ssh`
CLI (`ssh host 'cat > dest'` / scp), which honors the config. Recorded as an open
question; v1 documents the limitation and errors clearly.

## Transport axis (`--via`) — forward-looking

Connection-survival only. Orthogonal to `--persist`; the belt-and-suspenders
combo is `--via mosh` + a `--persist` backend.

- `--via ssh` — **default, the only v1 implementation.** Interactive
  `ssh -t host '<persist-wrapped-cmd>'`.
- `--via mosh` — **deferred.** `mosh host -- '<persist-wrapped-cmd>'` (mosh owns
  the TTY, so no `-t`; needs `mosh-server` on the remote → joins the up-front
  probe).
- `--via et` — **deferred.** Eternal Terminal: `et host -c '<persist-wrapped-cmd>'`
  (TCP auto-reconnect, native scrollback; needs `etserver` + its port → probe).

The persistence wrapping is identical across transports — `--via` only swaps how
the wrapped command is carried. Designing the flag in now keeps the launch code
factored around a `Transport` seam so mosh/et are additive drop-ins, not a
refactor. The libssh2 probe/ship half is unaffected by `--via`.

## Backends and launch mechanisms

Let `INNER` be the existing inner launch string: `[cd <cwd> && ]claude -r <id>`.
Session name is `path-<session_id>` (already used by the tmux path).

Backends fall into **three mechanisms**:

### 1. Direct command-wrap (hand them INNER)
| Backend | Remote command |
|---|---|
| `plain`  | `INNER` |
| `tmux`   | `tmux new-session -A -s path-<id> 'INNER'` |
| `abduco` | `abduco -A path-<id> sh -c 'INNER'` |
| `dtach`  | `dtach -A /tmp/path-dtach-<id> -z sh -c 'INNER'` |

Reattach on a later resume is automatic: tmux `-A`, abduco `-A`, dtach `-A` all
attach-or-create by name.

### 2. Layout-wrap (zellij)
zellij has no "new named session running command X" one-liner; the supported
path is a KDL layout file with the command in a pane.

- **Ship** an extra file `~/.cache/path/zellij-<id>.kdl` alongside the session
  JSONL, containing a single pane that runs `INNER`.
- **Launch**: `zellij --session path-<id> --layout ~/.cache/path/zellij-<id>.kdl`
  — creates with the layout when new, attaches when the session already exists.
- Reattach later: same command (zellij attaches an existing session).

### 3. Attach-only (shpool)
`shpool attach <name>` only ever starts a **shell** — there is no supported way
to run a one-shot command in it. So shpool does **not** auto-run the harness.

- **Ship** the session JSONL as normal.
- **Launch**: `ssh -t host 'shpool attach path-<id>'` — drops the user into a
  persistent shell.
- **Guidance**: before handing off, print the exact command to run:
  `run in the shpool session:  cd <cwd> && claude -r <id>`.

This is honest about shpool's model rather than faking a launch it can't do.

## User-facing surface

### Flags
- `--persist <backend>` — `plain|tmux|abduco|dtach|zellij|shpool`. Skips the
  picker. Requires `--remote`.
- `--tmux` — **deprecated alias** for `--persist tmux`. Kept working; emits a
  one-line deprecation note pointing at `--persist tmux`. Errors if combined
  with an explicit `--persist`.
- `--via <transport>` — `ssh` (default) | `mosh` | `et` (both deferred; error
  with a "not yet supported" message in v1). Requires `--remote`. See the
  Transport axis section.

### Picker behavior (mirrors the harness picker)
1. After the remote is reachable and home is resolved, **probe** the remote once:
   `command -v tmux zellij abduco dtach shpool` in a single exec channel →
   the set of available backends.
2. Candidate list = `plain` + every available backend (in a fixed display order:
   `tmux, zellij, abduco, dtach, shpool, plain`), each with a short description
   of what it buys (`tmux — detachable, survives drops`, `shpool — persistent
   shell (attach-only)`, `plain — no persistence`, …).
3. **Pre-select** the highest-priority available backend by the order
   `tmux > zellij > abduco > dtach > plain` (shpool is not auto-preferred because
   it can't auto-launch). If none installed, pre-select `plain`.
4. **Skip the picker** when: `--persist` given; or `--tmux` given; or not
   interactive (no TTY on stdin+stderr) — in which case use the pre-selected
   default. Emit a note when falling back to `plain` because nothing was
   installed.

## Internal design

- New `enum PersistBackend { Plain, Tmux, Abduco, Dtach, Zellij, Shpool }` with:
  - `bin() -> Option<&str>` (probe target; `Plain` → `None`).
  - `describe() -> &str` (picker row text).
  - display/parse for clap (`ValueEnum`) and the picker.
- New `struct PersistPlan { remote_command: String, extra_file: Option<(String, Vec<u8>)>, post_note: Option<String> }`
  built from `(backend, session_id, launch_cwd)`. `extra_file` is the zellij
  layout; `post_note` is the shpool guidance.
- `ExecStrategy` gains `fn remote_which(&self, target, bins: &[&str]) -> Result<BTreeSet<String>>`
  (single exec channel running `command -v …`), so probing is mockable in tests.
- `run_remote` sequence becomes: resolve/project → connect/home → **probe** →
  **resolve backend** (flag or picker) → **build PersistPlan** → ship JSONL
  (+ `extra_file`) → print `post_note` → interactive launch of
  `plan.remote_command` **via the selected transport**.
- New `enum Transport { Ssh, Mosh, Et }`. A `fn launch_invocation(transport,
  remote, remote_cmd) -> (binary, argv)` seam replaces the direct
  `ssh_invocation_tty` call: `Ssh` → today's `ssh -t …`; `Mosh`/`Et` → v1 returns
  a "not yet supported" error (shape reserved). The probe/ship half never sees
  `Transport`.
- `remote_launch_command(harness, id, cwd, tmux: bool)` is replaced by
  `persist_plan(harness, id, cwd, backend) -> PersistPlan`. Existing
  `tmux: bool` call sites and tests migrate to `PersistBackend`.

### Quoting
The nested quoting (`ssh 'sh -c '\''cd … && claude …'\'''`) is the sharp edge.
Reuse `shell_single_quote` and add a focused unit test per backend asserting the
exact remote command string for a representative `INNER` (with and without
`--cwd`), so the escaping is pinned.

## Testing

- Unit: `persist_plan` output string per backend (× cwd / no-cwd); `PersistBackend`
  clap parse + display; picker candidate assembly + pre-selection given a probed
  availability set; `--tmux` → tmux mapping + conflict-with-`--persist` error.
- `remote_which` exercised via `RecordingExec` (records the probe, returns a
  canned availability set).
- Integration (`tests/resume.rs`, `RecordingExec`): `--persist tmux/abduco/dtach`
  record the expected `remote_command`; zellij records the shipped layout file;
  shpool records the attach command + post-note; non-TTY defaults to the
  pre-selected backend.
- Keep the existing `remote_resume_*` tests green (migrated to the new enum).

## Rollout / compat

- Pre-1.0; `--tmux` stays as a deprecated alias (no hard break).
- Bump `path-cli` per the release checklist (minor: additive CLI surface).

## Open questions

- zellij `--session X --layout Y` when the session already exists: confirm it
  attaches (ignoring the layout) rather than erroring. If it errors, launch logic
  branches on "session exists" (probe with `zellij list-sessions`).
- dtach socket dir: `/tmp/path-dtach-<id>` is world-readable-parent; acceptable
  for a single-user box, but consider `${XDG_RUNTIME_DIR:-/tmp}`.
