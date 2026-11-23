# Toolpath follow-up ideas — capture gaps + process checkpoint

Parked backlog (raised 2026-07-25). **Not started.** Sits behind the Lane A
org-root narrative paragraph — recorded here so it isn't lost, not to jump the
queue. Kept on this standalone branch (no PR) rather than a shipped tree.

## 1. Toolpath history-capture gaps

Fact-checked against `RFC.md` + `schema/toolpath.schema.json` (this repo).

Genuine schema-level gaps:
- **Effects that aren't edits.** `change` is a map of artifact-key → `{raw,
  structural}` (diffs/AST ops). No record for an externalized action —
  push-landed, PR-opened, deploy, `terraform apply` — with a commit status.
  `stepMeta.refs` can *link* a PR URL but that's a reference, not a structured
  effect. Biggest gap relative to "attribution infrastructure is resume
  infrastructure."
- **Coverage.** `stepMeta.source` names the origin (e.g. a claude session) but
  nothing says *what wasn't observed*, so absence of a step is ambiguous
  (didn't happen vs wasn't captured). A document-level "derived from X + Y; not
  observed: Z" field would make gaps legible.
- **Environment base.** `base` = `{uri, ref, branch}` — pins the code, not the
  world (toolchain, container digest, lockfile). Cheap to add; matters for
  `path resume` correctness (resuming into a drifted env changes what steps
  mean).
- **Authorization.** `stepMeta` = `{intent, source, refs, actors, signatures}`
  — no grant/capability field. Ties who to *under what authority*. Thesis-
  completing but only meaningful after effects + capture-time signing exist.

Schema + importer:
- **Observations/results.** No first-class call→result / test-output data. CI is
  partly expressible (`ci:` actor prefix + `scope: ci` signature = attestation),
  but the evidence (output) isn't captured.

NOT a format gap (correction to the surface-read):
- **Signatures.** The schema already has `signature`, `actorDefinition.keys`,
  signed steps, scopes (author/reviewer/witness/ci/release), and a
  canonicalization algorithm. The real deficiency is that **nothing on the
  capture path signs at execution time** — importers derive retrospectively;
  `path p track` records intents/visits. Toolpath docs are testimony the format
  can already upgrade to evidence; the missing piece is a gate (keeperd-shaped)
  emitting signed steps when effects happen. That's the prx↔toolpath sentence,
  and it's an implementation gap, not a schema one.

  **Amended 2026-09-05 — a prerequisite this note assumed away.** The line above
  says the schema has "a canonicalization algorithm". It has a *specification* of
  one: `RFC.md:709-710` names JCS (RFC 8785) and `:731-822` gives the per-scope
  digests. Nothing implements it. A workspace grep for `jcs|8785|canonicaliz`
  finds only `std::fs::canonicalize` and a test-only camelCase renamer, and the
  core crate carries no crypto dependency at all — no `sha2`, `ed25519`, `ring`.

  That alone is consistent with the "implementation gap" framing. What is not:
  **the bytes a signer would sign are not deterministic.** The JSONL writer calls
  `serde_json::to_string(line)` (`jsonl.rs:667`) directly on the structs, and
  `Step.change` is a `std::collections::HashMap` (`types.rs:228`) with
  `preserve_order` off, so a step touching two or more artifacts serializes its
  keys in a different order per process. The round-trip test named
  `canonical_json` (`jsonl.rs:733-735`) goes through `to_value`, whose `Map` is
  `BTreeMap`-backed and therefore sorted, so it compares sorted values and
  structurally cannot observe the writer's nondeterminism. Its name asserts a
  property the code does not have.

  So capture-time signing is not one project but two, in order: make the
  serialization byte-stable (or canonicalize before digesting, and stop relying on
  a test that cannot fail), *then* build the gate. Doing the second first produces
  signatures that verify on the machine that made them and nowhere else.

Sequencing: effects + coverage + environment + authorization are small schema
additions; results is importer work (unbounded if you let it be); capture-time
signing is the prx integration (the deep one). Lane check before building any of
it: does it make the resume/attribution claim *true* (Lane C, legitimate), or
just make the format "more complete" (the bifurcation reflex in a schema's
clothes)?

## 2. `podman container checkpoint` / CRIU experiment (Lane B)

Process/container-layer capture is the only "resume = the live process comes
back" option (memory, fds, TCP via kernel repair mode) vs reconstruction. Ties
to gap #1: a checkpoint *is* an effect record with real state.

Sharp hypothesis worth one experiment: **brokered-effects architecture may be
what makes container checkpoint safe.** CRIU's classic pain is restored TCP
resuming against peers that gave up; if every external seam is a reconnectable
broker you own, that failure mode dissolves. Same shape as the resume argument.

Cheap test (one command, contained): `podman container checkpoint` the agent
container mid-run, restore, watch what breaks. Prediction: broker sockets —
which you could make tolerate reconnect since you own both ends. One experiment
before one sentence.

### Capture-layer survey (for reference)
- **Provider-managed:** shellbox (auto memory snapshot on disconnect, not
  on-demand); Sprites (FS-only COW checkpoint API, ~300ms, no memory); exe.dev
  (no snapshot surface — persists by staying alive); big-cloud hibernate/suspend
  (coarse, slow).
- **VM-level (self-hosted, full memory):** Firecracker snapshot/restore +
  fork-clones (the mechanism under shellbox); QEMU/libvirt savevm + live
  migration; Cloud Hypervisor.
- **Process/container (most relevant):** CRIU → `podman container checkpoint
  --export` (portable tarball, restore elsewhere = live migration); DMTCP
  (userspace-only, no root, fragile with async runtimes). TTY topology + restored
  TCP are the gotchas — dtach's socket-owned PTY is the CRIU-friendly shape.
- **Filesystem (disk truth, punt on memory):** btrfs/ZFS/LVM/overlayfs snapshots
  — combined with `claude -r` session-file replay = the journaled-reconstruction
  model, no memory capture needed.
