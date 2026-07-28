# Adding a harness: share, resume, and `path p export <harness>`

How to take a harness that has forward derivation (native session →
`ConversationView` → `Path`) and wire it all the way through the CLI:
`path share` out, `path resume` back in, and the `path p export <harness>`
plumbing in between.

Originally distilled from the Gemini implementation; since exercised by
Codex, opencode, Pi, Cursor, Copilot, and Amp. The current template to
crib from is **`toolpath-copilot`** (newest both-directions harness with a
local session store) — with **`toolpath-amp`** as the worked example of a
harness whose sessions *don't* live on disk (server-authoritative; the
"on-disk layout" steps become a CLI-mediated writer instead).

The forward path is out of scope here — we assume a working provider
crate already exists. Note `to_view` is a **free function** in each
provider crate (e.g. `toolpath_copilot::provider::to_view`), not a trait
method.

## Mental model

```
              provider::to_view          derive_path
native log ─────────────────▶ View ─────────────────▶ Path
                                ◀───────────────────
native log ◀─── Projector::project ◀─── extract_conversation
```

`ConversationView` is the IR. `derive_path` / `extract_conversation` live
in `toolpath-convo` and you should not reimplement them. Your job is the
rightmost arrow, plus the CLI seams around it.

The IR canonicalizes the *classification* (`ToolCategory`), not the
*name*. `ToolInvocation.name` is preserved verbatim from the source
harness; remapping to a target harness's vocabulary happens in the
projector.

There is **no provider-namespaced escape hatch on `Turn`** — the old
`Turn.extra["<harness>"]` mechanism was removed. Everything the projector
needs must live in typed IR fields; provider-specific UI/decoration fields
are *synthesized* from `args` + results on the way out (see step 2).

## Prerequisites

- `provider::to_view` exists and populates `ToolInvocation.category`
  (this is what the projector routes on for cross-harness translation).
- The on-disk (or on-server) format is documented under
  `docs/agents/formats/` — as a single file for well-understood formats
  (`gemini.md`, `codex.md`, …) or a **directory** for reverse-engineered
  preview harnesses (`copilot-cli/`, `amp/`), where every claim carries a
  confidence tag and a version stamp. If it isn't, write that first — the
  projector is the place where every quirk shows up.

## Steps

### 0. Register the harness in the CLI's enums

Two deliberately parallel enums, both exhaustive — the compiler walks you
to every seam:

- **`ArtifactType`** (`crates/path-cli/src/artifact.rs`) — the general
  artifact-source enum. Its `name()` doubles as the **cache-id prefix**
  (`amp-<inner-id>`, `copilot-<inner-id>`, …) used by `p import`.
- **`Harness`** (`crates/path-cli/src/harness.rs`) — the harness-only
  layer that `share`/`resume` `--harness` accept. Extend `Harness::ALL`,
  both `Harness::artifact_type()` / `ArtifactType::harness()` mappings,
  `HarnessBundle` (the share-time aggregation struct), and the
  `is_not_found_<harness>` probe that lets `share` silently skip machines
  where the harness isn't installed.

Then the import surface: `ImportSource::<Harness> { session, all }` +
`derive_<harness>` dispatch in `cmd_import.rs`, a `derive_<harness>_session{,_with}`
helper in `derive.rs` (the `_with` variant takes an injected provider
manager so tests can point it at fixtures), and a `pick_<harness>` picker
whose `--preview` template calls `path show <harness>`.

### 1. Reverse-map tool names: `native_name`

In `toolpath-<harness>/src/provider.rs`, alongside the existing
`tool_category(name)`. Two shapes exist in the codebase:

- **`Option<&'static str>`** (gemini, claude, codex, opencode, pi) —
  `None` means "no native analog; pass the foreign name through".
- **Total `&'static str`** (copilot, amp) — every category maps to
  *something* renderable.

```rust
pub fn native_name(category: ToolCategory, args: &Value) -> &'static str {
    match category {
        ToolCategory::Shell => "shell_command",
        // ToolCategory is too coarse for FileWrite (Edit / Write /
        // MultiEdit all collapse) — disambiguate by arg shape.
        ToolCategory::FileWrite => "apply_patch",
        // …
    }
}
```

The receiving harness's UI keys icons, labels, and category routing off
these names; getting them wrong means calls render as "unknown tool".
**Names, then args**: when the target's UI reads specific arg keys, remap
the args only if you can fill them honestly (amp synthesizes
`apply_patch {patchText}` envelopes from foreign FileWrites, `finder
{query}` from FileSearch patterns); when you can't, pass the foreign name
and input through untouched rather than emitting a native name wrapping an
empty shell.

### 2. Implement the projector

`toolpath-<harness>/src/project.rs`:

```rust
impl ConversationProjector for HarnessProjector {
    type Output = NativeSession;
    fn project(&self, view: &ConversationView) -> Result<Self::Output> {
        // Walk view.turns → native messages.
        // Walk turn.delegations → native sub-agent records.
    }
}
```

Things the projector MUST do:

1. **Remap tool names through `category` + `native_name(args)`** when the
   source name isn't already one of yours. Pass through verbatim when it
   is, so same-harness round-trips don't churn names. (Enforced by the
   cross-harness matrix and each crate's `real_fixture_roundtrip.rs`.)
2. **Synthesize required UI fields** (description, displayName, render
   hints, result mirrors) from `args` + `result.content` when the IR has
   nothing native. See `toolpath-gemini::project::synthesize_description`
   for per-tool dispatch with a generic fallback.
3. **Be position-stable**: ids you must invent (tool-result carrier
   messages, missing tool ids) derive deterministically from position, so
   projecting the same view twice is byte-identical. Ids the source gave
   you (turn ids, delegation ids) pass through so pairing survives a
   round trip.
4. **Re-expand tokens honestly.** Invert exactly what the forward path
   folded (gemini un-folds the reasoning breakdown; amp regenerates its
   derived `totalInputTokens`), and refuse to invent what can't be
   reconstructed (capacity fields, attribution the source never made).

### 3. Library/CLI parity for session resolution

If the harness's CLI accepts session identifiers in multiple forms (file
stem AND inner session id, for instance), the harness's library reader
should too — otherwise `path p export <harness>` followed by the CLI's
resume command works but the equivalent library round-trip doesn't. See
`toolpath-gemini::PathResolver::resolve_main_file`; the copilot analogue
honors `COPILOT_HOME` the same way in both.

### 4. Add the CLI export variant

In `crates/path-cli/src/cmd_export.rs` (**not** `toolpath-cli` — that is
a two-line shim), mirror **Copilot's** variant:

```rust
pub enum ExportTarget {
    // …
    Harness {
        #[arg(short, long)] input: String,
        #[arg(short, long)] project: Option<PathBuf>,
        #[arg(short, long, conflicts_with = "project")] output: Option<PathBuf>,
    },
}
```

Three modes:

- **`--project DIR`** — write the resume-ready layout (session files +
  any index/store row the harness's picker reads) so the harness's CLI
  invoked from `DIR` can resume it.
- **`--output FILE`** — write the primary artifact to `FILE`; multi-file
  formats land secondaries next to it with a clear convention.
- **Neither** — print the primary artifact to stdout.

Factor the projection into **`build_<harness>_session`** (pure: doc →
projected session) and **`project_<harness>`** (build + write + return
the session id). The split is load-bearing: `path resume` calls
`project_<harness>` directly in its `project_into_harness` arm, and the
`--output`/stdout modes stay offline even when `--project` needs network
(amp) or a database (copilot, opencode, cursor). Give `project_<harness>`
a unit test that asserts it returns the session id **and** writes the
artifact — with an injectable writer if the real one touches network.

### 5. Wire `path share`

`crates/path-cli/src/cmd_share.rs`:

- **`collect_<harness>`** — gather the harness's sessions into the
  aggregated picker. Follow the copilot/amp template: call
  `list_sessions()`, suppress `is_not_found_<harness>` errors quietly
  (harness not installed), print a `warning: <harness> aggregation
  failed:` for anything else.
- **`harness_status_<harness>` + `format_status_line`** — the
  no-sessions summary line.
- A `derive_session` arm routing `--harness <h> --session <id>` to the
  per-session derive helper.

The picker rides `path p list <harness> --format tsv` (single-keyed
columns: `id · last_activity · count · cwd · first_user_message`, all
through `sanitize_tsv`) and previews via `path show <harness>`. If share's
preview template always passes `--project`, give `show <harness>` a
hidden `--project` shim arg even when it's meaningless for the harness.

### 6. Wire `path resume`

`crates/path-cli/src/cmd_resume.rs`:

- **Source inference**: add the provider id to `infer_source_harness`'s
  `meta.source` match and an `agent:<harness>` prefix to the actor sniff
  (the picker pre-selects the source harness).
- **`argv_for`**: the harness's resume argv (`["--resume", id]`,
  `["threads", "continue", id]`, …) — a per-harness CLI convention,
  recorded with its evidence tag in the format doc.
- **`project_into_harness`**: call `cmd_export::project_<harness>`.
- **Exec seam**: nothing to add — `ExecStrategy` already abstracts the
  final `execvp`. Production uses `RealExec`; tests use `RecordingExec`
  to capture `(binary, args, cwd)` without launching anything.

Test in `crates/path-cli/tests/resume.rs`: a `RecordingExec` case that
feeds a file-input doc, points `$HOME`/`$PATH` at a scoped sandbox
(`ScopedHome` + `ScopedPath::with_binary("<harness>")`, plus a scripted
stub binary if projection itself shells out), and asserts the exec recipe
and the on-disk artifacts. Add a **cross-harness** case too (foreign-source
doc + `--harness <yours>`) — that is where resume earns its keep.

**Fresh identifiers only.** Resume must mint a fresh session id before
projection when the target's loader constrains id shape (Claude requires
a UUIDv4 filename stem; amp only resumes server-issued ids) — and even
when it doesn't, reusing the source id risks clobbering the original
session on a same-harness resume. Index registration is INSERT-only;
never create a store the harness owns (`if !db_path.exists()` → warn and
skip, don't fabricate).

### 7. Document the format — including the writer contract

Single file or directory per the prerequisite above. Beyond the reader's
reference, projection adds **`writing-compatible.md`**: what a synthesized
session must satisfy for the harness to load it. Capture especially:

- Filename/id conventions enforced by the loader (UUID stems, `session-`
  prefixes, id shapes the server issues).
- How the resume command resolves an identifier (filename? inner field?
  index row?).
- Required fields for the file to load at all, and every observed
  rejection **verbatim** with a version stamp (see step 9).
- Round-trip fidelity gotchas (absent vs empty, nullable vs missing,
  polymorphic result shapes) — and, where fidelity has a ceiling (amp's
  rendered-transcript resume), say so plainly rather than glossing.

### 8. Tests — five layers

In order of cost:

1. **Projector unit tests** in `project.rs` — content shape, role
   mapping, tool-call construction (with/without results, errors),
   native-name remap, position-stable ids, token re-expansion.
2. **Round-trip integration test** (`tests/roundtrip.rs` or
   `projection_roundtrip.rs`) — `native fixture → to_view → derive_path →
   serialize+reparse → extract_conversation → project → native`,
   asserted field-by-field. Include a **foreign-source** case (a
   claude-shaped view through your projector).
3. **CLI integration test** — `path p export <harness>` writes something
   the harness's own library reader opens by the same identifier the
   resume command would use. Isolate `$HOME`.
4. **A cross-harness matrix row** —
   `crates/path-cli/tests/cross_harness_matrix.rs`. Implement the
   harness struct (`name`, `roundtrip`, `load_fixture`,
   `schema_validates`) over the real fixture at
   `test-fixtures/<harness>/convo.*` and register it in **both** vectors
   (sources and targets). This is what catches other-harness fallout:
   amp's row surfaced a cursor projector bug, not an amp one.
5. **A real-capture fixture test** (`tests/real_fixture_roundtrip.rs`) —
   forward invariants against a first-hand capture, projection
   round-trip fidelity, and **wire-level serde value-identity** (parse →
   serialize == input, byte-for-byte modulo formatting). The
   value-identity test is the tripwire that catches schema drift when
   the harness updates.

The fixture comes from the **feature-elicit pipeline**: run
`docs/agents/feature-elicit.prompt.txt` through the harness
(`scripts/capture-elicit-fixtures.sh` automates every driveable harness),
sanitize per the checklist in `test-fixtures/<harness>/README.md`, and
commit it as both the crate-local test fixture and the matrix fixture.
The elicit session exercises shell, file create/edit, search, an
intentional error, a sub-agent, and reasoning — which is exactly what the
later probing question keys on.

### 9. The writer-contract loop (how `writing-compatible.md` gets written)

The loader's real constraints are discovered, not designed. The
methodology, proven on copilot and amp:

1. **Isolate the harness's state** (`COPILOT_HOME`, or `HOME` + XDG vars
   per the format doc's isolation recipe) so probing never touches real
   sessions, and pin auth via env (`AMP_API_KEY`) so no interactive
   login flow can trigger.
2. Project a session, invoke the real resume command, and read the
   rejection.
3. Fix **exactly one** rejection, record its message **verbatim** with an
   `[observed, <version>]` stamp in `writing-compatible.md`, repeat.
4. When it loads: ask the resumed model a **probing question** ("In one
   sentence, what was the most-used tool in this session?"). A specific,
   correct answer proves the context reached the model; "I don't have
   prior context" means the file loaded but the content didn't.
5. Freeze the loop as `scripts/verify-<harness>-live.sh` (shellcheck-
   clean, isolated home, loud failure, probe printed for human
   judgment) so the contract can be re-verified after every harness
   update.

Never report success on a status code alone — verify by read-back (amp's
REST import answered `201 Created` and created nothing).

### 10. Live end-to-end verification

Tests passing is necessary but **not sufficient**. Before declaring the
harness done:

**A. Full pipeline on a real conversation** — import a non-trivial
session from another harness, `path resume <it> --harness <yours> -C
<dir>` (or `p export --project`), and confirm the summary reports the
full message count.

**B. The probing question in the real CLI** — step 9's loop, run against
both a small capture and the feature-elicit capture. This is the DoD
standard: the answer must be specific and correct.

**C. Compare against a real session of the harness.** Where the harness
has an on-disk format, diff shapes against a session it wrote itself
(field coverage, tool-name distribution, no foreign top-level keys, no
`null`s where real sessions omit). In practice this is now covered by the
wire value-identity tests plus the live loader loop — but a manual look
at one real-vs-projected pair still catches "loads, but renders wrong"
(hunkless diffs rendering flat, decoration fields the UI dispatches on).

## Preview labeling and version stamping

A reverse-engineered harness ships as a **preview** in lockstep across:
the clap doc comments (`(preview)`), the crate `description`
(`(preview; schema reverse-engineered)`), the crate README blockquote,
the CLAUDE.md dependency-graph suffix + prose block, and
`site/_data/crates.json`. Every format claim carries an evidence tag
(`[observed]`/`[official]`/`[reverse-eng]`/`[inferred]`/`[unverified]`)
and a version stamp. When live verification lands, flip the hedges to
"✅ Verified in <harness> <version>" **everywhere at once** — including
any runtime stderr banner; a banner still claiming "unverified" after
the verification commit is a bug (copilot shipped exactly that stale
banner for a while).

## Release bookkeeping

A new harness crate is a new workspace crate: walk the **Versioning and
release checklist** in `CLAUDE.md` (items 1–11 — workspace `members` +
`[workspace.dependencies]`, CLAUDE.md, README, `site/_data/crates.json`,
`site/pages/crates.md`, `scripts/release.sh` tier lists, crate README
wired via `#![doc = include_str!]`), and bump `path-cli`'s minor version
for the new subcommands.

## Pitfalls (real ones we hit)

1. **Filename conventions are load-bearing.** Gemini's CLI filters
   `chats/*.json` by `session-` stem prefix *before* opening any file.
   The copilot analogue is the `session-store.db` `sessions` row: without
   it the session exists on disk and `--resume` can't see it. Always
   check: does the harness's own listing show the projected session?
2. **Identifier resolution often differs from filesystem layout.**
   Gemini's `--resume <uuid>` matches the inner `sessionId` field, not
   the filename stem. See `copilot-cli/resume-and-sessions.md` for the
   copilot variant.
3. **Multi-file formats need a thoughtful `--output` design.** Copilot
   writes three artifacts across two stores; `--output` emits the
   primary stream and documents what's elided.
4. **Never mutate state the harness owns.** Fresh ids, INSERT-only index
   rows, `create_new` (fail on collision) for filed artifacts, and
   warn-don't-create when the harness's store is missing. The projector
   must be incapable of touching an existing session.
5. **Tool args don't match across harnesses.** Claude's `Edit
   {file_path, old_string, new_string}` and Gemini's `replace {…}` line
   up; many pairs don't. Map names always; reshape args only when the
   target keys can be filled honestly (see step 1).
6. **UI decoration fields feel cosmetic but aren't.** Copilot's timeline
   dispatches on the `toolRequests` mirror; hunkless diffs render flat
   (`copilot-cli/file-fidelity.md`). Synthesize from args and result
   text.
7. **The reader is the next surprise.** The harness's library reader may
   not resolve identifiers the way its CLI does. Either add the missing
   path or document the asymmetry.
8. **Don't trust commit comments about backward-compat fallbacks.** If a
   comment says "fallback for older files," verify those files actually
   exist before preserving the branch.

## Concrete references

**Copilot** (local session store, the default template):

- `crates/toolpath-copilot/src/provider.rs` — `tool_category` + total
  `native_name`
- `crates/toolpath-copilot/src/project.rs` — `CopilotProjector`
- `crates/path-cli/src/cmd_export.rs` — `build_copilot_session` /
  `project_copilot` / `write_into_copilot_project` (events.jsonl +
  workspace.yaml + session-store.db row)
- `docs/agents/formats/copilot-cli/` — directory-form format reference
  incl. `writing-compatible.md` (9 verbatim rejections)
- `scripts/verify-copilot-live.sh` — the frozen live loop

**Amp** (server-authoritative, the no-local-store variant):

- `crates/toolpath-amp/src/io.rs` — `ThreadFetcher`/`ThreadWriter`
  seams around the first-party CLI
- `crates/toolpath-amp/src/project.rs` — `AmpProjector` +
  `rehydration_prompt` (content-derived fence for untrusted transcripts)
- `crates/path-cli/src/cmd_export.rs` — `build_amp_session` /
  `project_amp{,_with}` / `write_into_amp_project` (create → file
  INSERT-only artifact → seed)
- `docs/agents/formats/amp/writing-compatible.md` — a *server import*
  contract: routes ruled out with evidence, fidelity ceiling stated
- `scripts/verify-amp-live.sh`

**Gemini** (the original local multi-file worked example):

- `crates/toolpath-gemini/src/project.rs` — `synthesize_description` and
  the decoration-field synthesis pattern
- `crates/toolpath-gemini/src/paths.rs` — `resolve_main_file` CLI-parity
  lookup

Cross-cutting: `crates/path-cli/src/{artifact.rs,harness.rs}` (enum
registration), `cmd_share.rs` (`collect_*`), `cmd_resume.rs` (`argv_for`,
`ExecStrategy`), `tests/cross_harness_matrix.rs` (both vectors),
`scripts/capture-elicit-fixtures.sh` + `docs/agents/feature-elicit.md`
(the fixture pipeline).
