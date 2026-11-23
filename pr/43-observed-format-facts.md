# docs(formats): the format notes' facts generated from the recordings, and checked

Status: planned (asked 2026-10-19: make the format notes' facts generated or checked, not hand-maintained) · Base:
the goldens stack once merged · Independent of pr/19–42

## Why
`docs/agents/formats/` states facts that come from samples ("optional = seen without it, required = never seen
missing", "what we've seen in sample data") and versions, all kept by hand, and stale against the recordings the
baseline now runs: the Claude Code note's samples stop at 2.1.112 (the recordings are 2.1.132 and 2.1.285), the Codex
note's at 0.118.0 (recording: 0.128.0), the opencode note names 1.3.10 and 1.18.18 (recording: 1.14.41), and the index
still called the Copilot notes "not yet sample-grounded" although they cite 1.0.67 captures and the baseline records
1.0.68. Nothing failed when a recording and a note disagreed.

## What changes
- `crates/toolpath-goldens/src/observed.rs` (new): over a harness's recordings, per record kind (`type`, Codex's
  `type/payload.type`, or `document`), every field (key path, arrays collapsed; a key that is data — a path, an id, a
  dated model id — shown as `<key>`), the JSON shapes seen, and `required` (never missing) or `optional (n/m)`; plus the
  versions the recordings name. Unit tests.
- `docs/agents/formats/observed/<harness>.md` (new, generated, seven files): that table per harness, with the
  recordings it was computed from.
- `crates/toolpath-goldens/tests/observed.rs` (new): fails when a table is stale (regenerate:
  `OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed`), when a note doesn't link its table, when a
  recording contains a record kind the harness's notes never name, or a dotted version the note doesn't name.
- Notes: each links its table and names the recordings' versions (Claude Code 2.1.132 and 2.1.285, Codex 0.128.0,
  Copilot 1.0.68, opencode 1.14.41; pi, Gemini and Cursor recordings carry no version, said so). Claude Code's
  `entry-types.md` describes `cost-state` (seen in 2.1.285, the one entry type no note named). The index explains
  `observed/` and corrects the Copilot line: grounded in captures; the `[reverse-eng]` rows are exactly the event
  types no recording contains (checked: none of them occurs in the 1.0.68 recording).
- Qualifier: a concern on the index's Maintenance rule. The rule is now enforced from the recordings; a reader or
  writer change with no new recording still is not, and the record says why whole-file spans were not used and what
  would close it.

## Verification
- `cargo test -p toolpath-goldens` (lib `observed` tests, `--test observed`): ok.
- At this base the workspace test is red independently of this piece: 7a93457 (Cursor 3.24 model history) changed
  the Cursor projection without recapturing `projection@*@cursor`, `round_trip@*@cursor`, `shed@*` and
  `writer@cursor`; nothing here touches them.
- Gates as `scripts/quality_gates.sh` with `--features path-cli/goldens,toolpath/verify`.

## Paths
```
crates/toolpath-goldens/src/lib.rs
crates/toolpath-goldens/src/observed.rs
crates/toolpath-goldens/tests/observed.rs
docs/agents/formats/.qual
docs/agents/formats/README.md
docs/agents/formats/claude-code/README.md
docs/agents/formats/claude-code/entry-types.md
docs/agents/formats/codex.md
docs/agents/formats/copilot-cli/README.md
docs/agents/formats/cursor.md
docs/agents/formats/gemini.md
docs/agents/formats/opencode.md
docs/agents/formats/pi.md
docs/agents/formats/observed/claude.md
docs/agents/formats/observed/codex.md
docs/agents/formats/observed/copilot.md
docs/agents/formats/observed/cursor.md
docs/agents/formats/observed/gemini.md
docs/agents/formats/observed/opencode.md
docs/agents/formats/observed/pi.md
pr/43-observed-format-facts.md
```
