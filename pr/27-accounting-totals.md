# fix(gemini, opencode, copilot): the session total and the steps agree

Status: planned (asked 2026-10-19: fix the three accounting known-defects) · Base: the goldens stack once merged (pr/12 pins the defects) · Siblings: pr/19 (gemini jsonl) touches the gemini goldens too, pr/22 (codex resume cwd) retires another known defect in `defects.rs`, pr/23 (object model) reads the defects and concerns, pr/26 (writer isolation) adds `writer@<agent>` snapshots this piece leaves unchanged; independent code

## Why
The goldens assert one invariant rather than pin it: a session's token usage has three witnesses that must agree
field by field, the `token_usage` of every `conversation.append` step summed, the `total_usage` the adapter's
`to_view` reports, and the total `extract_conversation` rebuilds from the document (agent-coding-session v1.1.0:
summing `token_usage` over a path's steps yields the session totals). Three sets broke it and were pinned as known
defects:
- `gemini-total-usage-drops-breakdowns`: `toolpath_gemini::provider::sum_usage` added the four counters of every
  turn and dropped their breakdowns, so `total_usage` had no `output.reasoning` (steps 489, IR none, extracted 489).
- `opencode-total-usage-drops-breakdowns`: `toolpath_opencode::provider::accumulate_total` the same (285 vs none).
  Both are the bug pr/01 fixed in `toolpath_convo::extract`, still present in two adapters.
- `copilot-session-totals-not-on-steps`: Copilot's `assistant.message` reports only `outputTokens`; the session's
  input and cache totals exist once, on `session.shutdown`. `to_view` put them in `total_usage` and on no turn, so
  the document's steps lacked 54 input, 198884 cache-read and 22998 cache-write tokens the IR claimed. Every
  projection, and any spend derived from a stored document, lost them.

## What changes
- `toolpath-gemini` (`provider.rs`): `sum_usage` adds breakdowns class by class; the main and sub-agent loops share
  one `add_usage`. Test `test_total_usage_sums_breakdowns`.
- `toolpath-opencode` (`provider.rs`): `accumulate_total` adds breakdowns class by class. Test
  `total_usage_sums_breakdowns_across_messages` (two messages, reasoning 5 and 7, total 12).
- `toolpath-copilot` (`provider.rs`): every counter no turn reports is taken from `session.shutdown` and put,
  verbatim, on the session's last assistant turn; `total_usage` is the sum over turns. A counter any turn reports
  stays the per-turn sum (the shutdown's output, verified = Σ `outputTokens`, is never added twice). Without an
  assistant turn the total alone keeps it. Test `shutdown_totals_settle_on_the_last_assistant_turn`. README and
  `docs/agents/formats/copilot-cli/events.md` say where the totals go and that that turn's input/cache are the
  session's, not its own spend.
  **The decision.** Copilot has no per-message input to attribute, so the honest choices were: (a) a session-level
  usage field the invariant reads, which needs a new kind version and changes to derive, extract, every projector
  and the goldens invariant; (b) drop the shutdown totals, the ~222k-token regression the shutdown fallback fixed;
  (c) settle them on the last assistant turn, where the session ends, as Codex settles a round's total on the
  round's final turn. (c) keeps every number verbatim, needs no spec change, and makes every existing consumer
  (projections, `extract_conversation`, spend derived from documents) see the session's real usage. Its cost: that
  one turn's input/cache are not that message's own; recorded on the code as qualifier 34d907f1.
- `toolpath-goldens` (`defects.rs`): the three `Defect` entries go, with the `accounting_gap` measure and
  `Target::Document` only they used (a new accounting defect brings both back). The invariant in
  `tests/goldens.rs` now holds with no exemption for every set.
- Snapshots: `known_defects@{gemini,opencode,copilot}.snap` are `[]`. `ir@gemini` and `ir@opencode` gain
  `total_usage.breakdowns.output.reasoning` (489, 285); nothing else of theirs moves. Copilot: `ir`/`document` (the
  last assistant step carries 54 / 198884 / 22998), every `projection@copilot@*` (the totals on the last assistant
  message), `round_trip@copilot@*` (more key paths; the round trips now show which projectors drop
  `cache_write_tokens`: codex, gemini, cursor, cursor also `cache_read_tokens`, pre-existing projector gaps that had
  no data to lose before), `shed@copilot` (byte counts; the same-format round trip keeps 3 more key paths). insta
  rewrote the stale `input_file: goldens/copilot/input.jsonl` header of the snapshots it touched to the set's real
  input `test-fixtures/copilot/convo.jsonl`. `path goldens capture copilot test-fixtures/copilot/convo.jsonl`
  recaptured `goldens/copilot/to-{claude,codex,pi}.jsonl` (the last assistant line carries the totals),
  `goldens/roundtrip/copilot-{codex,pi}.json` (codex: `cache_write_tokens` lost, the codex token fields gained) and
  the copilot set's manifest pins.
- Qualifier: 18549e27 (the accounting-gap concern on `accounting.rs`) resolved fixed; 122da212 (five defects pinned)
  superseded by d1465c82 (two, none accounting); the copilot decision recorded (34d907f1).

## Verification
Unit tests above (`cargo test -p toolpath-gemini|toolpath-opencode|toolpath-copilot`); `cargo test -p
toolpath-goldens` with no snapshot update pending and the qualifier freshness test green; `path goldens check`
45 ok, 0 problems; the gates
`scripts/quality_gates.sh` runs, with `--features path-cli/goldens`: fmt, clippy `--workspace --all-targets -D
warnings`, `RUSTFLAGS=-D warnings cargo test --workspace`, `RUSTDOCFLAGS=-D warnings cargo doc --workspace
--no-deps`, shellcheck.

## For the next PR
`docs/model/model.json` (pr/23) is generated at 79003bc and still lists the three accounting defects and their
concerns as open; regenerate it (`cargo run -p toolpath-model`, which reads pathbase and the review feedback too).
`toolpath_claude::provider::sum_usage` has the same shape (counters only, breakdowns dropped); no claude set carries
a breakdown today, so the invariant cannot see it. The codex, gemini and cursor projectors drop
`cache_write_tokens` (cursor also `cache_read_tokens`), now visible in the copilot round trips.

## Paths
```
crates/toolpath-gemini/src/provider.rs
crates/toolpath-opencode/src/provider.rs
crates/toolpath-copilot/src/provider.rs
crates/toolpath-copilot/src/.qual
crates/toolpath-copilot/README.md
docs/agents/formats/copilot-cli/events.md
crates/toolpath-goldens/src/defects.rs
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/snapshots/ir@gemini.snap
crates/toolpath-goldens/snapshots/known_defects@gemini.snap
crates/toolpath-goldens/snapshots/ir@opencode.snap
crates/toolpath-goldens/snapshots/known_defects@opencode.snap
crates/toolpath-goldens/snapshots/*@copilot*.snap
goldens/copilot/to-claude.jsonl
goldens/copilot/to-codex.jsonl
goldens/copilot/to-pi.jsonl
goldens/roundtrip/copilot-codex.json
goldens/roundtrip/copilot-pi.json
goldens/manifest.json
```
