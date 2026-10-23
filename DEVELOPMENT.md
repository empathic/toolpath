# Development

Requires Rust 1.85+ (edition 2024); the exact toolchain is pinned via
`rust-toolchain.toml`. Day-to-day tasks are driven by
[`just`](https://github.com/casey/just); run `just help` to list the
recipes.

Toolpath reads sessions that other programs write, in formats nobody
documents, and writes them back for programs that will reject anything
they don't expect. So development here starts from what the harnesses
actually wrote, not from what we think they write: a change is first
stated as a difference in the baseline, then made, then reviewed as
that difference.

## Words

- **recording**: one real session a harness wrote, committed as test
  input (`test-fixtures/<harness>/convo.*`, `goldens/<name>/input.*`).
  Each records the harness and version that produced it.
- **snapshot**: the saved output of one stage for one recording
  (`crates/toolpath-goldens/snapshots/<stage>@<recording>[@<target>].snap`).
- **baseline**: every recording through every stage, every snapshot,
  the declared losses and the contracts. `goldens` is its older name and
  stays as an alias in the commands below.
- **stage**: parse (`ir`), derive (`document`), project
  (`projection@…@<harness>`), the round trip back (`round_trip@…`),
  `validate` and `validate_transforms` (the schema on every document a
  transform produces), `shed` (what each stage drops), `known_defects`,
  `writer@<harness>` (each writer on its own), `formats` (the file
  formats), `opencode_db` (the opencode database read).
- **contract**: a property the harness or the schema enforces, checked
  on every output and never pinned
  (`crates/toolpath-goldens/src/contracts.rs`), so accepting a snapshot
  cannot accept an output the harness would reject.

## Where this comes from

The baseline builds on the test foundation Ben Barber laid in the spring,
and replaces none of it:

- the elicited real-session recordings in `test-fixtures/` and their
  capture script, `scripts/capture-elicit-fixtures.sh` (7a51195);
- the cross-harness translation matrix and the per-harness real-fixture
  round trips, with the Codex idempotence fixes (#81):
  `crates/path-cli/tests/cross_harness_matrix.rs` and each provider
  crate's `tests/real_fixture_roundtrip.rs`;
- `ConversationView` as the one derive/project interface (#86), which is
  what makes "every recording through every writer" one loop;
- derived actors conforming to the base schema (#87) and `meta.kind`
  (#85), which the baseline's `validate` stage checks per kind.

Those tests check named properties: same turns, same tool-call topology,
same text, a fixed point the second time around. The baseline adds the
rest of the output: every stage of every recording is saved, so any
field that changes shows up by name, and the losses we accept are
written down as data rather than left implicit.

## The loop

1. **State the change as a baseline difference first.** Pick one:
   - a new recording (a harness version, a session shape we don't have);
   - an expected loss or gain going away (a field a writer should now
     keep), or a new one we accept on purpose;
   - a regression case: re-introduce the bug as a fault in a stage's
     output in `crates/toolpath-goldens/tests/regressions.rs` and watch
     it fail with the field and stage named.
2. **Run the baseline and read the named diff.**
   ```bash
   just baseline                    # `path d baseline check`: every recording, the shed
                                    # reports and the qualifier review, through the CLI
   cargo test -p toolpath-goldens   # every stage's snapshot and the contracts
   ```
   `just baseline diff <recording>`, `shed <recording>` and `list` narrow
   it; `just baseline capture --all` rewrites the CLI's pinned outputs
   when you mean the change. `path d` (with `webmock` and `model`) is
   built only with the `develop` feature, never in a release; `goldens`
   is accepted as an alias of `baseline`.
   A failure names the snapshot (stage and recording) and the line; a
   contract failure names the rule, the target, the line and the field.
3. **Fix the code, or accept the difference on purpose.** Accepting
   means three things in the same change: the snapshot (`cargo insta
   review`, or `INSTA_UPDATE=always cargo test -p toolpath-goldens`), the
   loss declared where it is measured (`shed`, `known_defects`), and a
   qualifier record on the owning code that says why. A contract is never
   accepted this way: fix the output.
4. **Write the piece's `pr/NN-*.md` record**: why, what changes, the
   snapshots it changes and what each difference means, the verification.
   `git diff crates/toolpath-goldens/snapshots/` is the review.
5. **Run CI's gates locally**: `just ci` (or `just check` to format
   first). The gates build with `--features path-cli/develop,toolpath/verify`.

## Common changes

**Adding a harness.** In this order, each one landing with its baseline:
a recording from a real session of a named version (with its provenance
in `goldens/manifest.json`); the reader (the provider crate's `to_view`);
the writer (its projector); a `<harness>.md` format note under
`docs/agents/formats/` grounded in that recording; a loader model in
`crates/toolpath-harness-mock` (what the harness would send the model
when it resumes); a recorded model exchange under
`crates/toolpath-webmock/cassettes/<harness>/` (`just webmock <harness>
record --cell <dir> -- <harness args>` once, then `replay` with no
network) so resume can be replayed without a model; a contract for anything the harness is known to reject.

**Changing a reader or writer.** Run the baseline before and after. The
diff is the change's description: which stages, which recordings, which
fields. If a format note says something the change makes false, update
it in the same change (see Format notes below).

**Changing the kind.** Producers write `agent-coding-session` v1.1.0
only. A new kind version is a new immutable URI: add its schema under
`crates/path-cli/kinds/agent-coding-session/v<x.y.z>/`, publish it under
`site/kinds/`, move the producers to it, and the baseline's `validate`
stage (and `validate_transforms`) checks every document and every
transform against it. The old version stays readable; nothing writes it.

**Format notes.** Each harness's fields, as its recordings show them,
are generated into `docs/agents/formats/observed/<harness>.md`. A new
recording that changes them fails `cargo test -p toolpath-goldens --test
observed` until you regenerate (`OBSERVED_UPDATE=1`) and the prose note
names any new entry type or version (see Format notes below).

**Examples.** `examples/` is validated by the `examples` gate (`path p
validate` over each document) and by the site's visualizer, which loads
them. Generating them from real producer runs is in progress; until
then they are hand-written and the gate is what keeps them valid.

**The object model.** `docs/model/model.json` is generated from both
repositories, the `pr/` records, the qualifier records and the deck's
review threads (`just model`, which runs `path d model`; see
`docs/model/README.md`). Its test fails when a piece cites a commit
that isn't on the branch, a claim that reads as pinned has no snapshot
evidence, or a known defect has no owner. Regenerate it after the code
it describes is committed.

**Qualifier.** Findings live on the code they are about, as qualifier
records (`.qual` beside the file). The baseline's check runs `qualifier
review`, and in CI the pinned qualifier binary is installed and a
missing one fails the gate. When your change moves lines a record is
anchored on, re-record it with `--supersedes` (same content, new
anchor); when it fixes what a record describes, `qualifier resolve
--reason fixed`.

## What the baseline does not reach

Some fixed bugs live outside a single recording's pipeline; each is
covered by its own test and recorded as a qualifier concern on
`crates/toolpath-goldens/tests/regressions.rs` until the baseline reaches
it:

- rotated Claude Code session files (`.orphaned-*`): the baseline reads
  one file per recording; `toolpath-claude` `chain.rs` tests the chain;
- the export refusing to overwrite a richer local session: CLI
  behaviour, tested in `path-cli` `cmd_export.rs`;
- Cursor's `state.vscdb`: the cursor recording is a JSON dump;
  `toolpath-cursor`'s reader tests the 3.24 table.

## Other recipes

```bash
just fmt                  # format all code (Rust + site)
just clippy               # clippy across the workspace, warnings denied
just site                 # site dev server (eleventy + wasm watcher)
just refresh-openapi      # re-fetch the Pathbase OpenAPI spec into pathbase-client
just test-pathbase-live   # live smoke test against a Pathbase deployment
just release-check        # dry-run publish of all workspace crates (safe)
just release-publish      # publish to crates.io for real
```

`just ci` and `just check` take gate names to narrow or exclude: the
gates are `format`, `shellcheck`, `clippy`, `test`, `doc`, `examples`,
`plugin`, `quickstart`, `site` and `secrets`, and a `-` prefix excludes one
(`just ci test`, `just check -site`).

## Format notes

[docs/agents/formats/](docs/agents/formats/README.md) records our
understanding of each agent's on-disk session format, written down
while building the derive crates. That includes a twelve-part reference
for Claude Code's JSONL (envelope, entry types, session chains,
compaction, a writing-compatible guide), the writer contract the
Copilot CLI loader appears to enforce, and single-file references for
Codex, Gemini, opencode, Cursor, and Pi.

None of these formats are documented by their vendors. The notes come
from observed behavior of specific versions: they are works in
progress, with gaps, and the agents can change their formats at any
time. Corrections are welcome. If you're building your own session
tooling, they're a useful starting point even if you never run our
code.

The notes are meant to stay in sync with the derive crates and the
recordings. If you change what a crate reads or writes, or add a
recording of a new version, update the matching document under
`docs/agents/formats/` in the same change.
