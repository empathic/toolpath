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

- Transport-layer persistence (mosh, Eternal Terminal). Those wrap the SSH
  *connection*, not the remote command; different composition, out of scope.
- Restore-after-reboot / resurrection semantics beyond what each backend already
  does on its own.
- Local (non-`--remote`) resume is unchanged.

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
  (+ `extra_file`) → print `post_note` → interactive `ssh -t` of
  `plan.remote_command`.
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
