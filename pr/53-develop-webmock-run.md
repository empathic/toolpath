# feat(path-cli,webmock): `path d` (baseline, webmock, model), `just` recipes, and `webmock run <agent>` replays a cell from the committed cassettes

Status: planned (asked 2026-10-19 on the goldens demo deck: "what sets this up? it's not manual? we built a tool to help with this right?"; shape from Bobby: `path d …` mirroring `path p`, `just` recipes, the recording/snapshot/baseline vocabulary) · Base: pr/17 and pr/21 once merged

## Why
The record/replay server (`toolpath-webmock`) was committed, but pointing each agent at it was a scratch script:
the base-URL switch, a temp `HOME`/XDG, fake replay credentials, pi's `models.json`/`auth.json`, opencode's
`opencode.json`/`auth.json`, gemini's `.env`/`GEMINI_CLI_HOME`, copilot's two listeners. And the committed cassettes
did not replay at all: they carry `<scratch>` where the recording machine's paths were, so no live request keyed
the same (qualifier `389dcdab`). Bobby also wants the development tools in one place, behind one feature, out of the
release binary.

## What changes
- **`path d`** (alias `path develop`; path-cli feature `develop`, off by default, replacing pr/17's `goldens`
  feature). Porcelain at the top, `p` plumbing, `d` develop. Help: "Develop: goldens, record/replay and other tools for
  working on toolpath itself (built only with the `develop` feature)".
  - `path d baseline …` is the old `path goldens …` (vocabulary: a *recording* is an input session, a *snapshot* a
    stage's pinned output, the *baseline* the versioned set). `goldens` stays a hidden alias so scripts and docs in
    flight keep working; the crate renames (toolpath-goldens, toolpath-golden) are a separate later piece.
  - One entry point for checking (Alex, rehearsal: "Why two commands, `path goldens check` and `cargo test`? Worth
    collapsing"): `path d baseline check` / `just baseline` runs the tests that are the check,
    `cargo test -p toolpath-goldens` then `cargo test -p path-cli --features develop --test goldens`
    (`goldens::CHECK_COMMANDS`). `cargo test` is the implementation; the command is now the thin wrapper. `check <set>`
    and `--in-process` (or `--allow-producer-drift` / `--no-qualifier`) still run the manifest goldens in-process, the
    same `Goldens::check` the path-cli test calls.
  - `path d webmock …` and `path d model …` run the unpublished `toolpath-webmock` (`webmock`) and `toolpath-model`
    binaries (beside `path`, on `PATH`, or `$WEBMOCK_BIN` / `$TOOLPATH_MODEL_BIN`): path-cli is published and cannot
    depend on crates crates.io lacks. The standalone binaries stay; they are the implementations.
  - A default build has no `d`/`develop`, and no `goldens`/`webmock` at the top level in any build (unit test).
  - `justfile`: `just baseline [check|capture --all|diff <set>|shed <set>|list]` (default check), `just webmock
    <agent> [replay|record] …`, `just model …`, each one line over `cargo run -q -p path-cli --features develop -- d
    …` (positional arguments, so quoted prompts survive). `just ci` / `just check` already run
    `scripts/quality_gates.sh`, which builds with `--features path-cli/develop`. `scripts/goldens.sh` stays as the
    wrapper `goldens/DEMO.md` documents and the baseline harness pins, now calling `d baseline`.
  - Gates (`quality_gates.sh`, `justfile clippy`, `pr/cut.sh`, `pr/gate-piece.sh`), the goldens tests'
    `required-features`, the docs (`docs/GOLDENS.md`, `goldens/DEMO.md`) and the messages in `goldens.rs` follow.
- **`webmock run <agent> record|replay --cell <dir> [--mask <live>=<placeholder>]… -- <args, {sid} {project} {home} {base}>`**
  (`src/run.rs`): copies the cell (`home/`, `project/`, `sid`) to a temp work dir (`--in-place` for gemini, pi,
  opencode), starts one server per upstream on free ports (copilot: model API + GitHub API), writes each agent's
  switch and, in replay, a fake credential, runs the agent with an empty environment and a hermetic home, stops the
  servers, prints EXACT/LOOSE/MISS per request; exit 1 on any miss or a nonzero agent. Replay serves a copy of the
  cassettes (misses never land in the repo). Record needs `--upstream` (and `--github-upstream` for copilot) and passes
  only `--pass-env` variables. codex is refused: it needs a TLS listener the Rust server does not have.
- **Masks** (`Config::masks`, `apply_masks`): request bodies and paths are masked before keying and storing (the work
  dir → the placeholder the recording ran under; `/private` spellings both ways); the upstream still gets the request as
  sent.
- **Loose key** ignores `<system-reminder>` blocks and collapses whitespace in the last user text
  (`without_reminders`): Claude Code adds "The date has changed" when resuming on a later day.
- **copilot cassettes**: the `GET /copilot_internal/user` answer from the same recording, identity replaced with
  `webmock-replay` values (provenance `github_user`).

## Verification
- Replay from the committed cassettes (cells from the resume matrix, `--mask <old scratch root>=<scratch>`):
  copilot rc 0 in 1 s, 10/10 EXACT, reply "You're going to walk through END"; claude rc 0, `/v1/messages` LOOSE (date
  reminder + one tool schema differ), same reply. Before this piece both missed every model call.
- Unit tests: per-agent env/files, record refusals, masks, placeholder table, arg expansion, tally, cell copy; a
  masked record → replay in another directory keys EXACT; reminder-insensitive loose key; the loose-key history test
  now counts 4 JSON-RPC + 2 reminder/whitespace changes, 28 unchanged.
- Gates as `scripts/quality_gates.sh` with `--features path-cli/develop`.

## Paths
```
crates/path-cli/Cargo.toml
crates/path-cli/src/lib.rs
crates/path-cli/src/cmd_develop.rs
crates/path-cli/src/goldens.rs
crates/path-cli/tests/goldens.rs
crates/toolpath-goldens/tests/goldens.rs
crates/toolpath-webmock/src/lib.rs
crates/toolpath-webmock/src/run.rs
crates/toolpath-webmock/src/bin/webmock.rs
crates/toolpath-webmock/tests/roundtrip.rs
crates/toolpath-webmock/cassettes/README.md
crates/toolpath-webmock/cassettes/copilot/
docs/GOLDENS.md
goldens/DEMO.md
justfile
scripts/goldens.sh
scripts/quality_gates.sh
```
(`pr/cut.sh` and `pr/gate-piece.sh` change on wip only.)
