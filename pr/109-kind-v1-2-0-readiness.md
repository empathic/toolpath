# test(goldens): the baseline against a combined agent-coding-session v1.2.0 (fixture, not our kind)

Status: draft · Base: pr/108 · Source: deck v52 item 8 (Bobby: "assume this kind lands and make sure we test it")

## What this is
Three open PRs each publish their own `agent-coding-session/v1.2.0/schema.json`, all claiming the same version: #108 (Ben: the `conversation.compact` step), #353 (ulmentflam: additive token classes), #132 (`model` on conversation.append).
The combined schema is `crates/toolpath-goldens/fixtures/kind-v1.2.0-combined.schema.json`: a three-way merge (`git merge-file`) of the three schemas against v1.1.0, fetched with `git fetch origin pull/N/head`. The merge was clean except the `description`, which each PR edits;
I wrote one description that carries all three "New in v1.2.0" notes. It is a TEST FIXTURE only, not shipped as our kind, credited to the three PRs in the test header. The owners publish the real one; when any merges, swap the fixture for the real schema and diff.

## Tests (crates/toolpath-goldens/tests/kind_v1_2_0.rs)
- every baseline `document@*.snap` (13 recordings) validates under v1.1.0 (what `path p validate` does today) AND, with its `meta.kind` moved to the v1.2.0 URI, under the combined v1.2.0: the new kind is additive;
- a `conversation.compact` step validates, a bad `trigger` does not;
- token classes: non-negative, `breakdowns` keyed by a known class (`input|output|cache_read|cache_write`); a negative class and an unknown breakdown key are refused;
- `model` on `conversation.append` is optional and a string.

## Verification (sandbox, cargo 1.97.1, offline)
All four tests pass. They passed on the first run: the negative assertions (what the schema refuses) are what show they are not vacuous; no red step applies to a new fixture.
toolpath-goldens full run, clippy -D warnings, fmt, qualifier: see the report. NOT run: other slices, pinned 1.94, gitleaks, nix.

## NOT covered, and why (needs the PRs' CODE, not only their schemas)
- "The compaction recordings round-trip their compact step under v1.2.0": today's derive emits no `conversation.compact` step for the forced compactions (the baseline deliberately waits for #108's code, pr/88); there is nothing to round-trip.
- "Token classes sum per group" on real documents: the sum rule is prose in the kind (JSON Schema cannot express it); #353's `path p validate` checks it, and its code is not in this tree.
- "`model` survives on append" (which would also close the pi/copilot model-setting loss): the derive that writes it is #132's code.
These three become tests when the PRs' code is in the tree (the Ben-integration / everyone-first base).
