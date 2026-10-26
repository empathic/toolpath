# test(goldens): the compactions Ben Barber forced in four agents become baseline recordings (pr/88)

Status: draft · Base: pr/87 · Approved by Bobby 2026-10-21 · Recordings: Ben Barber, empathic/toolpath#108 (copied unchanged, no re-record)

See docs/baseline/compaction.md (the recordings, the row for the map's resume and project slides, what it shows, what is pending).

- Sets `claude-forced`, `codex-forced`, `pi-forced`, `opencode-forced` (`Set::from_input_file` names `test-fixtures/<agent>/convo-compacted.*` `<agent>-forced`); every stage snapshotted: 91 round trips (63 + 28).
- Not fixed ahead of #108: our readers have no compaction item, so the snapshots show what compaction loses (text survives everywhere; the compaction markers do not). When #108 lands the diffs prove it.
- Kept both Claude compactions: the hand-made `claude-compacted` carries the old tool's pinned `to-*` outputs.
- Test changes: counts (13 documents, 91 round trips), `UNTIL_108` (an allow-list that requires the break to still happen), harness-mock `KNOWN` entry, the query snapshot.
- Pending: real compacted sessions (privacy pass), copilot (none), live resume of compacted sessions (Bobby's go per run), the map slide row (data is in compaction.md; scratch-main-82 owns the deck).
- Not run: pinned 1.94, gitleaks, nix-develop recapture.

Credit: the recordings and the capture script that made them are Ben Barber's (#108).
