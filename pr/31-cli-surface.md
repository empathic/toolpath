# test(path-cli): the CLI surface pinned — help tree, the CLI page against the binary, every documented workflow

Status: planned (asked 2026-10-19: "is there any cli surface we should also be testing for breakage against the goldens") · Base: the goldens stack once merged (needs the `goldens` feature from pr/17) · Siblings: the quick-start flow test (`p cache sync`, `query`, `share`, `resume`, the plugin), the site `path query` example, core types / both formats / VCS mapping, and `path develop webmock` are separate pieces; this one does not repeat them

## Why
The goldens pin what each transformation does in-process. Nothing pinned the `path` binary's surface: a renamed
flag, a removed subcommand or a changed default broke users and nothing failed, and `site/pages/cli.md` (toolpath.net/cli)
could drift from the binary without anyone noticing. Running the page against the binary shows it already has.

## What changes
`crates/path-cli/tests/cli_surface.rs` (`required-features = ["goldens"]`), hermetic: empty environment, temp
`HOME`/`TMPDIR`/XDG dirs, `TZ=UTC`, `COLUMNS=100`, no network; JSON output canonicalised (sorted keys), temp
paths and wall-clock stamps normalised.
- **Help tree** — `help@<group>.snap` (23 files): every command and subcommand the binary exposes, found by
  walking `--help` recursively (99 commands, the dev-only `goldens` group included), with its full help text. A rename/removal/default change is a named
  snapshot diff.
- **CLI page vs binary** — `cli_page_vs_binary.snap`: the "## Commands" tree of `site/pages/cli.md` against the
  binary: documented-but-missing (none), present-but-undocumented (45 commands), and per-command flag differences
  (10 commands, 31 flags) plus the global `--picker`.
- **Behaviour** — `behaviour@*.snap`, stdout + exit code:
  - `kind` (list; the unversioned selector prints the v1.1.0 schema — asserted), `auth status` without credentials,
    `haiku` (exit 0).
  - `p validate`: a committed example (valid), a step with a dangling parent (reported **valid**), a step without
    `actor` (rejected, exit 1).
  - `p query ancestors` and the page's dead-end query on `examples/path-04-exploration.path.json` (returns `step-002a`).
  - `p merge` (two examples → one Graph) and `p render dot`.
  - The page's "Track changes in real time" workflow end to end: `init` (stdin), two `step`s, `annotate`, `note`,
    `visit`, `export`, `list`, `close`; the closed document validates.
  - `p import git` single- and multi-branch over a deterministic two-branch repo (multi-branch → one Path per branch,
    asserted).
  - Per harness over the golden sessions (`test-fixtures/{claude,codex,copilot,pi}/convo.jsonl`, placed the way
    `path goldens` places them): `p list` finds the session, `p derive` (kind v1.1.0, steps, actors, head is a step),
    `p import --no-cache` yields the same document as `derive`, `show` (first lines).
  - One derived document through every CLI writer: `p export {claude,gemini,pi,codex,opencode,copilot,cursor}`,
    `p project claude`, `p incept claude`; then `p import` → `p cache ls` → `p cache rm`.
- `site/pages/.qual`: 5 concerns and 3 passes on the CLI page (below).

## Findings (qualifier records on `site/pages/cli.md`)
- concern 24–66: the page's command tree omits 45 commands and 31 flags the binary has — including `p cache sync`,
  which the home page's quick start uses, `p render md`, and every harness but claude/git/pathbase.
- concern 44: `[--force] [--no-cache]` are marked global; they're accepted on `p import <source>` (and
  `--no-cache` on share/resume), not everywhere.
- concern 141–151: the track workflow passes `--session /tmp/session.json`, but `track init` writes
  `$TMPDIR/track-<stamp>-<pid>.path.jsonl` and prints it; nothing creates `/tmp/session.json`.
- concern 171: `p validate` reports a step whose parent does not exist as valid; RFC.md resolves parents within the
  path. It checks the JSON Schema, not reference integrity.
- concern 70: JSON output key order is not deterministic (`p merge`, `p query ancestors` print hash-ordered maps:
  the same input printed two different byte streams across runs). Pinned on canonicalised JSON.
- pass 114–120 (import git, multi-branch), 157–163 (merge), 130–136 (dead-end query).

## Not covered here (and why)
- `share`, `resume`, `p export pathbase`, `auth login`: need Pathbase or an interactive terminal; their flag rules and
  anon/authed uploads are in `tests/integration.rs`, and the quick-start piece runs them against recordings.
- `p cache sync`, `query` over every session: the quick-start and query pieces.
- `p render md`: `tests/render_md_snapshots.rs`.
- `show/list/derive/import` for gemini, opencode, cursor: those adapters read a database or a `.json` store the
  goldens don't place yet (`path goldens` places claude, codex, copilot, pi); `config edit` opens `$EDITOR`.
- The writers' bytes: pinned by `toolpath-goldens` (`projection@…`); here only that each CLI writer runs and writes.
- `help@goldens.snap` changes when `path goldens` moves under `path develop` (the develop-webmock piece); that piece
  updates it.

## Verification
- `cargo test -p path-cli --features goldens --test cli_surface`: 12 passed, then 3 more consecutive runs with
  `INSTA_UPDATE=no`, all identical (the first two-run check is what exposed the key-order concern).
- Gates on the tree rebased onto 26ad6b4: fmt, clippy `--workspace --all-targets --features path-cli/goldens -D warnings`,
  `RUSTFLAGS=-D warnings cargo test --workspace --features path-cli/goldens` (94 suites ok), doc, shellcheck.

## Paths
```
crates/path-cli/Cargo.toml
crates/path-cli/tests/cli_surface.rs
crates/path-cli/tests/snapshots/help@*.snap
crates/path-cli/tests/snapshots/behaviour@*.snap
crates/path-cli/tests/snapshots/cli_page_vs_binary.snap
site/pages/.qual
```
