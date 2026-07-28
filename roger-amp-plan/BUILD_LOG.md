# Build log — Amp harness (`toolpath-amp`)

A chronological record of how the Amp harness was built: what each piece
delivered, what was decided along the way, and why. Written for a reviewer who
wants the reasoning behind the code, and for future build sessions picking up
context.

Conventions:

- **Append-only.** One entry per build piece, newest at the bottom. Entries
  are never edited after the fact; corrections go in a later entry.
- **Commits follow [Conventional Commits v1.0.0](https://www.conventionalcommits.org/en/v1.0.0/).**
  The git log carries the granular *what*; this file carries the *why*.
- **Key decisions use an ADR shape** (Context → Decision → Rationale →
  Alternatives rejected) so the reasoning is legible without having been there.

---

## Piece 00 — format-recon — tag: `amp-m0` — 2026-07-27

### Goal

Close the format-discovery gap the planning session deferred: answer the four
architecture-gating questions with first-hand evidence, land sanitized
fixtures, and write the dossier every later piece cites.

### What was built

- `docs/agents/formats/amp/README.md` — sourcing posture, 5-tag confidence
  table, version anchors, scope exclusions
- `docs/agents/formats/amp/RECON.md` — single-sentence answers to Q1–Q4 plus
  evidence, deviations, cost
- `docs/agents/formats/amp/directory-layout.md`
- `docs/agents/formats/amp/events.md` — both wire shapes, block catalogue,
  native tool vocabulary, and the mapping sketch to the toolpath IR
- `docs/agents/formats/amp/session-state.md`
- `docs/agents/formats/amp/file-fidelity.md`
- `docs/agents/formats/amp/resume-and-sessions.md`
- `docs/agents/formats/amp/known-gaps-and-sourcing.md`
- `test-fixtures/amp/convo.json` — sanitized `amp threads export` of the
  feature-elicit thread (24 messages)
- `test-fixtures/amp/stream.jsonl` — sanitized teed `--stream-json` (26 lines)
- `test-fixtures/amp/README.md` — provenance, checklist coverage, sanitization
  table
- `docs/agents/formats/README.md` — amp folder registered
- `docs/agents/feature-elicit.md` — amp row + the export-recovery recipe
- `roger-amp-plan/BUILD_LOG.md` — this file

### Key decisions (ADR-style)

- **Q1 architecture fork: CLI-mediated server export, not local files**
  - _Context:_ PLAN.md offered three routes — local-log reconstruction, a raw
    server API, or capture-time-only. The per-thread log
    (`~/.cache/amp/logs/threads/T-*.log`) turned out to be structured
    telemetry with a maximum line length of 548 bytes, carrying `blockCount` /
    `blockTypes` / `hasUsage` but no bodies, no tool results and no usage
    numbers. No other local file holds thread content.
  - _Decision:_ the canonical artifact is **`amp threads export <id>`** — a
    fourth route none of the three anticipated. `toolpath-amp` will shell out
    to (or reimplement) that fetch rather than reading a directory.
  - _Rationale:_ it is complete (full text, thinking, tool inputs, tool
    results, per-message usage), it is available for any thread at any time
    rather than only during capture, and it is a supported first-party
    command rather than a reverse-engineered endpoint. Capture-time-only
    would have made `p list`/`p import` useless for anything not recorded
    live.
  - _Alternatives rejected:_ **teed `--stream-json`** — capture-time only, no
    thinking blocks without an extra flag, tool results arrive as stringified
    JSON, and its usage zeros are `||0`-coerced so absent and zero are
    indistinguishable. **The local thread log** — proven content-free.
    **The raw server API** — `amp threads raw` returns 403 even for a thread
    the account owns, and the underlying Rivet-actor endpoints are
    reverse-engineered and unexercised.

- **Q2 token pattern: clean per-message; no `group_id`, no attribution, no breakdowns**
  - _Context:_ kind v1.1.0 admits three legal shapes. Amp attaches a `usage`
    object to each assistant message with `inputTokens`, `outputTokens`,
    `cacheReadInputTokens`, `cacheCreationInputTokens`, plus
    `totalInputTokens` and `maxInputTokens`.
  - _Decision:_ treat it as **clean per-message**. One Amp message → one
    `Turn`; usage goes in `Turn.token_usage`; `group_id` and
    `attributed_token_usage` stay `None`; `breakdowns` is omitted;
    `totalInputTokens` and `maxInputTokens` are **dropped**.
  - _Rationale:_ `outputTokens` is non-monotonic within a thread
    (`35 → 13 → 7`) so it cannot be cumulative; continuing a thread spent 5
    then 5, not 5 then 10; and `totalInputTokens == inputTokens +
    cacheReadInputTokens + cacheCreationInputTokens` held on all 17 usage
    objects across three threads, making it a derived sum rather than a
    counter. Independently, the bundle shows `usage` attaching to exactly two
    zod schemas (assistant message, assistant delta) with a reducer that
    *overwrites* rather than accumulates. Dropping `maxInputTokens` matters:
    it is the 272000-token context-window capacity, and summing it would
    fabricate a spend two orders of magnitude too large. Emitting
    `attributed_token_usage` would be fabrication — Amp reports per message,
    not per content block, so there is no finer-grained truth to attribute.
  - _Alternatives rejected:_ **per-message-repeated + field-wise max**
    (Claude's shape) — Amp emits one usage per message, not one per block.
    **Cumulative deltas** (Codex's shape) — refuted by the non-monotonic
    series. **Escalate as "none"** — not needed; the counters are real and
    per-message.

- **Fixture is `convo.json`, not `convo.jsonl`**
  - _Context:_ PLAN.md and the piece-00 prompt both name
    `test-fixtures/amp/convo.jsonl`, written before Q1 was answered.
  - _Decision:_ the canonical fixture is `test-fixtures/amp/convo.json` (the
    pretty-printed export document), with the teed stream as a sidecar at
    `test-fixtures/amp/stream.jsonl`.
  - _Rationale:_ the export is a single JSON document, not JSON Lines —
    naming it `.jsonl` would be a lie about its shape and would break the
    obvious `jq` idioms. The prompt anticipated this ("or documented
    equivalent if not JSONL"). Both artifacts ship, so nothing is lost.
  - _Alternatives rejected:_ forcing the export into JSONL by emitting one
    message per line — destroys the top-level envelope (`env`, `meta`,
    `title`, `agentMode`) that `to_view` needs.

- **`--stream-json-thinking` tested by continuing the trivial thread**
  - _Context:_ the plan budgeted ≤3 threads and asked for the thinking
    variant on the trivial thread only.
  - _Decision:_ ran it as `amp threads continue <trivial-id> -x …` rather
    than opening a fourth thread.
  - _Rationale:_ kept the budget at exactly 3 threads and produced a bonus
    data point — the continued turn's usage (`output_tokens: 5`,
    `cache_read_input_tokens: 16393`) is direct evidence that usage is
    per-message and that continuation reuses the prompt cache.
  - _Alternatives rejected:_ a fresh fourth thread — more spend, no extra
    signal.

### Deviations from PLAN.md

- **Fixture filename** — `convo.json` + `stream.jsonl` instead of
  `convo.jsonl`. Justified above; DoD equivalent is
  `jq -e '.messages | length' test-fixtures/amp/convo.json`.
- **Recorded Amp version is stale in PLAN.md.** The Global Constraints pin
  `0.0.1785164324-gd1fcef`; the live binary is
  **`0.0.1785170481-ga5b614`** (released 2026-07-27T16:41:21Z). Amp
  self-updated between the planning session and this one. All claims are
  stamped against `ga5b614`. Every later piece must re-run `amp --version`
  rather than trusting the plan.
- **`amp threads raw` attempted and failed** (403). Not in the plan's step
  list; recorded rather than retried.
- **Q3 answered by probe, not by the `AMP_*` route the plan guessed.** There
  is no `AMP_*` data-directory override; `HOME` and the three XDG variables
  do the work, XDG taking precedence.
- **Discovered `amp threads export/raw/markdown/usage` and
  `--stream-json-input`**, none of which appear in PLAN.md's orientation
  list. `export` reshaped the whole piece.

### Tests & verification

Piece 00 adds no Rust, so the workspace gates are unchanged-by-construction;
they were run anyway.

- `ls docs/agents/formats/amp/` — 8 files present (README, RECON,
  directory-layout, events, session-state, file-fidelity,
  resume-and-sessions, known-gaps-and-sourcing).
- `jq -e '.messages | length' test-fixtures/amp/convo.json` → `24`;
  `jq -se 'length' test-fixtures/amp/stream.jsonl` → `26`. Both parse.
- `grep -rn 'observed, 0\.' docs/agents/formats/amp/` — version-stamped
  claims present throughout.
- Sanitization: zero residual matches for the username, home path, capture
  path, `creatorUserID`, `installationID`, or `deviceFingerprint` in either
  fixture.
- Token invariant `totalInputTokens == inputTokens + cacheReadInputTokens +
  cacheCreationInputTokens` — true for all 17 usage objects across the three
  captured threads.
- Stream↔export usage equality — all 12 assistant messages of the
  feature-elicit capture match field-for-field.
- Self-verification against the fixtures caught and fixed four of this
  session's own numbers before commit (thread-log line count, the export's
  `created`/`updatedAt`, the `v`-to-message ratio, export size).
- `just ci` → **7/7 gates pass** (format, shellcheck, clippy, test, doc,
  examples, site). First run was 6/7 with `shellcheck` failing only because
  the tool was absent from this machine; shellcheck 0.11.0 was installed and
  the suite re-run green. `cargo clippy --workspace -- -D warnings` and
  `cargo test --workspace` are covered by the passing clippy/test gates.

All live-`amp` claims carry the stamp `[observed, 0.0.1785170481-ga5b614]`.

### Known limitations / follow-ups

- **Only one agent mode was exercised.** Every capture ran `medium` →
  `gpt-5.6-sol` → `provider: "openai"`. Thinking-block shape
  (`openAIReasoning`) is visibly provider-specific, so an Anthropic-backed
  mode may differ. Tracked in `known-gaps-and-sourcing.md`.
- **4 of 29 tools exercised** (`shell_command`, `apply_patch`, `Task`,
  `skill`). `finder`, `librarian`, `oracle`, `web_search` and the
  thread/schedule/orb families need categories before piece 05's
  `native_name` totality test can pass.
- **Sub-agent turns are unavailable.** `Task` returns a bare string, no child
  thread is created, and `parent_tool_use_id` is `null` throughout —
  `DelegatedWork.turns` will always be empty, like Claude and Copilot.
- **No git state in a thread**, so `Path.base.vcs_*` is always `None` and
  diffs cannot be anchored to a commit.
- **Error signalling is only understood for `shell_command`**
  (`run.result.exitCode`). `run.status` is `"done"` even on failure and the
  stream's `is_error` was `false` for a failing command — both are traps.
- **Two scratch access tokens were minted** during the Q3 isolation probe by
  Amp's unattended browser-login flow, and deleted. Roger may want to revoke
  them at `https://ampcode.com/settings/security`.

- **Q3's "XDG takes precedence" is an over-generalization.** The live probe
  is sound for what it covered (`secrets.json`, `device-id.json`, the log
  tree, the config dir all moved). But bundle mining afterwards showed the
  XDG logic is hand-rolled in **four independent copies with inconsistent
  behavior**: the main data/cache module computes `XDG_DATA_HOME` and then
  **never uses it** (`~/.local/share` is hard-forced), while the
  settings/secrets and shell-history modules do honour it. Net effect:
  `XDG_DATA_HOME` alone **splits** Amp's data dir — `secrets.json`,
  `device-id.json` and `history.jsonl` move; `session.json` and the
  oauth/ide/runner/daemon/portal state do not. The isolation recipe in the
  dossier already sets `HOME` *and* the XDG vars, so it is safe; only the
  one-line precedence sentence in `RECON.md`/`directory-layout.md`
  overstates. **Left unfixed at Roger's instruction** (2026-07-27) to
  unblock piece 01 — fix in the next docs touch.

- **Late bundle-mining findings — three facts the captures could not reveal.**
  A read-only sweep of the Bun bundle finished after the dossier was frozen.
  Recorded here rather than in `docs/agents/formats/amp/` (Roger's call,
  2026-07-27, to unblock piece 01); fold into the docs on the next touch.
  1. **The `usage` zod schema has 9 keys, not the 8 we captured.**
     `_kT = {model?, maxInputTokens, inputTokens, outputTokens,
     cacheCreationInputTokens: nullable, cacheReadInputTokens: nullable,
     totalInputTokens, thinkingBudget?, timestamp?}`. **`thinkingBudget`
     appears in none of the three captured threads**, so piece 01's wire
     struct must carry it as an `Option` regardless or the value-identity
     round-trip test will fail on the first thread that has one. It is a
     *request budget*, not a consumption counter — never sum it, same as
     `maxInputTokens`.
  2. **Both cache counters are `.nullable()`, not merely optional.** No null
     was observed, so the fixture cannot catch this: decode null *and*
     absent to `None`, both distinct from a real `0`.
  3. **Amp's own session total is `Σ(totalInputTokens + outputTokens)` over
     assistant messages** — a client-side function feeding a prop named
     `cumulativeBilledTokens`. That is the figure Amp's UI shows, and it
     counts cache read + creation as billed. Piece 02 should match it so the
     demo reconciles with what Alex sees inside Amp.
  Also confirmed: **no reasoning/thinking token counter exists anywhere** in
  the bundle (0 hits for `reasoningTokens`/`thinkingTokens`), independently
  validating the decision to omit `breakdowns`. And a bundle code path emits
  `usage` on the stream's `result` line, which our `--stream-json` capture
  did not produce — unexplained, low priority.
  **Do not downgrade the `totalInputTokens` sum relation.** The sweep marked
  it `INFERRED` because no arithmetic for it exists client-side (the server
  computes it); our empirical verification across all 17 usage objects makes
  it `[observed]`, which is the stronger evidence.

### Open questions for Roger

1. **Piece 01's crate shape needs a decision.** `toolpath-amp` cannot be a
   filesystem reader. Options: (a) shell out to `amp threads export`
   (simple, needs `amp` on `PATH`, inherits its auth); (b) reimplement the
   HTTP fetch against `AMP_URL` using the token in `secrets.json` (no `amp`
   dependency, but we'd be reading a credentials file and pinning an
   undocumented endpoint); (c) read-only from a pre-exported file, with
   `p import amp --input <file>`. PLAN.md's piece-01 interface list
   (`PathResolver`, `AmpConvo::new()` infallible on missing dirs,
   `AmpDirectoryNotFound`) assumes a directory that does not exist and needs
   reshaping either way. **This blocks piece 01.**
2. **`p list amp` will be N+1.** `amp threads list` gives no `cwd`, no
   `first_user_message`, and only relative timestamps, so the piece-02 TSV
   contract needs one `export` per thread. Acceptable, or should the TSV
   columns be relaxed for amp?
3. **Piece 03's writer route.** Local fabrication is ruled out. The lead is
   the thread actor's `POST /import` (reverse-engineered, unexercised).
   Worth spending a ⚠ probe on, or should L2 be treated as at-risk from now?
4. **Amp self-updates fast** (a version bump mid-recon). Should we pin a
   version for the duration of the build, or accept churn and lean on the
   value-identity tests as tripwires?

## Piece 01 — derive-crate — tag: `amp-m1` — 2026-07-27

### Goal

`crates/toolpath-amp` forward path (export document → `ConversationView` →
agent-coding-session v1.1.0 `Path`) plus the minimal `path p import amp`
seam, per PLAN.md Piece 01 as parameterized by piece 00's Q1/Q2 answers.

### What was built

- `crates/toolpath-amp` 0.1.0 (preview labeling throughout):
  `error/paths/reader/types/io/provider/project/derive`, README with the
  ⚠️ blockquote + version anchors, `#![doc = include_str!]`.
- Wire model (`types.rs`): typed only where the provider needs accessors;
  everything else — including explicit `null`s (`readAt`, `openExpiresAt`)
  — rides `#[serde(flatten)]` extras, so parse → serialize is
  **value-identical** on the whole real capture (asserted whole-document
  and per-message).
- Fetch layer (`io.rs`): `ThreadFetcher` trait — `CliFetcher` shells out to
  `amp threads export <id>` (read-only, inherits the CLI's login);
  `DirFetcher` reads pre-exported `<id>.json` files (tests/offline).
  `cargo test` never touches the network.
- Provider (`provider.rs`): one message → one turn, EXCEPT
  tool-result-only `user` messages (plumbing; results merged onto the
  originating invocation by `toolUseID`, no turn emitted) — the real
  24-message capture derives 13 turns. Linear `push_linked` chaining;
  verbatim tool names + `tool_category` (exact arms then substring
  fallback); `Task` → `DelegatedWork` keyed by the `TU-…` id with the
  result string backfilled; `apply_patch` result `files[]` → per-file
  `FileMutation` with the wire's real unified diff, relativized against
  the tree root; errors from `run.result.exitCode != 0` only (never
  `run.status`).
- Tokens (per Q2 "clean per-message"): `Turn.token_usage` from the four
  real counters; `group_id`/`attributed_token_usage` `None`; `breakdowns`
  omitted; `totalInputTokens`/`maxInputTokens` preserved on the wire
  struct but never summed; `is_usage_zero → None`; session total =
  field-wise Σ (real capture: 0 in / 1537 out / 210 694 cache-read /
  20 758 cache-write).
- `derive.rs`: wraps `toolpath_convo::derive_path`, title
  `"Amp session: <8ch>"`; kind-conformance test validates the maximal
  real-capture path against
  `crates/path-cli/kinds/agent-coding-session/v1.1.0/schema.json`.
- Tests: 72 unit + 3 real-fixture + 4 synthetic-roundtrip + 3 doctests.
  Fixtures: `tests/fixtures/real-session.json` (byte-identical to
  `test-fixtures/amp/convo.json`) + synthetic `sample-session.json`.
- CLI seam: `ArtifactType::Amp` (test `ALL` 8→9, `path_keyed` false),
  `Harness::Amp` (`ALL` 7→8, both mappings, `HarnessBundle.amp`,
  `is_not_found_amp`), `derive_amp_session{,_with}`,
  `ImportSource::Amp {session, all}` + `derive_amp` + `pick_amp`.
  `toolpath-amp` added to both path-cli dep blocks (pure JSON + shell-out
  → copilot-style placement, no `rusqlite`).

### Key decisions (ADR-style)

- **Crate shape: shell out to `amp threads export` behind an injectable
  fetcher** (Roger's in-session pick, 2026-07-27, from the piece-00 open
  question that blocked this piece).
  - _Context:_ Q1 ruled out a filesystem reader; options were (a) CLI
    shell-out, (b) reimplementing the HTTP fetch off `secrets.json`, (c)
    pre-exported files only.
  - _Decision:_ (a), with `ThreadFetcher` as the seam so tests inject
    fixtures and piece 02+ can add sources without changing the public
    surface. `PathResolver` survives but now resolves the *data dir* for
    existence checks (`with_amp_dir()` kept for injection); errors gained
    `AmpCliNotFound`/`AmpCliFailed` alongside the PLAN-named trio.
  - _Rationale:_ first-party command, inherits auth, no credential-file
    reads, no reverse-engineered endpoint pinning.
  - _Alternatives rejected:_ (b) fragile against Amp's build-timestamped
    churn and reads a secrets file; (c) breaks the `p import amp
    --session <id>` DoD and makes piece-02 list/share manual.
- **Token layer: implemented exactly the Q2 "clean per-message" pattern.**
  No new evidence contradicted piece 00: `outputTokens` non-monotonic
  (117 → 142 → 112 …), the `totalInputTokens = input + cacheRead +
  cacheCreation` invariant holds on all 12 usage objects, so per-message
  stamping with field-wise Σ is honest. `maxInputTokens` (272 000
  capacity) and `totalInputTokens` are wire-preserved for round-tripping
  but excluded from every spend figure; a test asserts capacity never
  leaks into totals.
- **Tool-result-only `user` messages are not turns.** The mapping sketch's
  "1 message → 1 turn" would emit 11 empty user turns whose only content
  was already merged onto the assistant's invocations. Precedent: every
  other provider merges results into the originating turn. The projector
  (piece 03) can regenerate the carrier messages from
  `ToolInvocation.result`, and the wire-identity test keeps the source
  artifact lossless regardless.

### Deviations from PLAN.md

- **Interface reshaping** (anticipated by the piece-00 blocker):
  `AmpConvo::with_fetcher(Arc<dyn ThreadFetcher>)` instead of a
  directory-scan surface; `SessionMetadata.dir_path` is `Option<PathBuf>`
  (no backing file exists for a live fetch); `line_count` = message count.
  All PLAN-named types/fields otherwise kept.
- **DoD pipe syntax is stale:** `p render md --input -` errors — `p
  render` reads stdin when `--input` is *omitted* (only `p merge`/`p
  incept` accept a literal `-`). Verified with
  `p import amp … --no-cache --force | p render md --detail full`.
  CLAUDE.md's claude-import example has the same stale `--input -`.
- **Picker preview 404s until piece 02** lands `show amp` (noted in
  `pick_amp`'s doc comment, as the plan anticipated).
- **`most_recent_session` added to `AmpConvo`** (not in the PLAN interface
  list) so the no-picker fallback matches the other providers' shape.
- No format-doc corrections needed: the fixture taught nothing that
  contradicts the piece-00 dossier (live re-check: `amp --version` still
  `0.0.1785170481-ga5b614`).

### Tests & verification

- `cargo test -p toolpath-amp` — 82 tests green (72 unit, 7 integration,
  3 doc).
- Live DoD: `p import amp --session T-019fa4db-…` →
  `~/.toolpath/documents/amp-path-amp-T-019fa4.json` (15 steps);
  `p validate --input <it>` → `Valid: Graph (id: path-amp-T-019fa4, 1
  path)`; rendered markdown shows the elicit beats — prompt, thinking
  quotes, all 11 tool calls with results incl. the failing `cat`, the
  `Task` delegation + quoted sub-agent answer, per-turn token lines.
  (The live thread's tree URI is the piece-00 capture scratchpad, not the
  fixture's sanitized `/tmp/amp-elicit` — expected.)
- Workspace gates: `cargo build/test/clippy --workspace -- -D warnings`
  green; `just ci` **7/7** (format, shellcheck, clippy, test, doc,
  examples, site).

### Known limitations / follow-ups

- `p list amp` (piece 02) inherits the N+1 export-per-thread cost — Q2 of
  the piece-00 open questions is still Roger's call on TSV column
  relaxation.
- `native_name` maps `FileRead → shell_command` (Amp has no read tool),
  which re-classifies to `Shell` — the piece-05 totality/invariant test
  will need to bless or redesign that arm.
- `AmpProjector` is a refusing stub until piece 03's writer recon.

## Piece 02 — share-wiring, L1 — tag: `amp-m2` — 2026-07-27

### Goal

Full forward CLI surface for Amp (`p list amp`, `show amp`, `path share
--harness amp`) and the live L1 check: a captured thread shared to Pathbase
whose page shows the conversation beats at the dossier's token-attribution
level.

### What was built

- `cmd_list.rs`: `ListSource::Amp` + `run_amp` — json (`"source":"amp"`),
  tsv (`id·last_activity·line_count·cwd·first_user_message` through
  `sanitize_tsv`), pretty (`// ── Amp (preview) ──` banner, `msgs` unit
  since the "line count" is a message count).
- `cmd_show.rs`: `ShowSource::Amp { session, hidden --project shim }` +
  `derive_one` arm — the shim is mandatory because share's fzf preview
  template always passes `--project`.
- `cmd_share.rs`: `collect_amp` gather block (cwd from
  `env.initial.trees[0].uri`, `is_not_found_amp` suppression, `warning:
  amp aggregation failed:` otherwise), `harness_status_amp` +
  `format_status_line("amp", …)` in the no-sessions summary. The
  `derive_session` arm already landed with piece 01.
- `cmd_import.rs`: `pick_amp` doc comment un-hedged (the preview pane is
  now backed by `show amp`).
- Tests: 2 gather unit tests + `amp_only_bundle`/`write_amp_session`
  helpers (written first, red at compile/assert, then wired green);
  `harness_status_for_empty_bundle_is_unresolved` extended with the amp
  status; 6 integration tests over `amp_home_fixture`.

### Key decisions (ADR-style)

- **`collect_amp` has no resolver-existence gate.**
  - _Context:_ every other collector reads local state; Amp's listing
    shells out (`amp threads list`) and each row costs one `amp threads
    export` (the N+1 the piece-00 open question flagged; Roger kept the
    TSV contract).
  - _Decision:_ follow the copilot template exactly — call
    `list_sessions()`, suppress `is_not_found_amp` errors (CLI missing,
    data dir missing, no home), warn on anything else. No pre-check of
    the data dir before shelling out.
  - _Rationale:_ `AmpCliNotFound` already covers the "not installed"
    machine quietly; gating on the data dir would need a combined
    resolver+fetcher constructor that `AmpConvo` deliberately doesn't
    have, and would wrongly hide sessions when Amp is installed but its
    data dir moved. The N+1 cost only bites when amp is installed and
    logged in — exactly when the rows are wanted.
  - _Alternatives rejected:_ `resolver().exists()` pre-gate (false
    negatives, extra constructor surface); caching the listing (piece
    06+ concern, not L1).
- **Integration fixture = stub `amp` on `PATH` + fake `HOME`, unix-gated.**
  - _Context:_ the spawned `path` binary builds `AmpConvo::new()` →
    `CliFetcher`, so fixture injection must happen at the process
    boundary; Amp has no session directory to lay out (piece-00 Q1).
  - _Decision:_ `amp_home_fixture()` writes an executable `amp` shell
    stub answering `threads list`/`threads export` from a pre-exported
    JSON file, sets `PATH=<stub>:/usr/bin:/bin` and `HOME=<sandbox>`
    (the piece-00 isolation recipe), `#[cfg(unix)]` on the stub-needing
    tests.
  - _Rationale:_ zero production seams added; exercises the real
    CliFetcher arg surface; no real Amp state touched.
  - _Alternatives rejected:_ an env-var fetcher override in production
    code (a test-only backdoor in shipping code); `#[cfg(windows)]`
    stub variants (no Windows CI today).

### Deviations from PLAN.md

- **`scripts/test-pathbase-live.sh` was not run to completion** — its
  precondition hard-requires `path auth status` to be logged in even
  though its first leg is anonymous. PLAN.md marks the authed check
  "optional (Roger login) — never a DoD blocker". The anon round-trip
  was verified directly instead: `p import pathbase <uploaded-url>
  --no-cache | p render md --detail full` reproduces the beats from the
  server copy. Follow-up: teach the script an anon-only mode.
- The "status-unresolved loop" test is an extension of the existing
  `harness_status_for_empty_bundle_is_unresolved` rather than a new
  test — same coverage, no duplicate scaffold.

### Tests & verification

- TDD order held: gather tests + status-loop extension written first
  (red: `harness_status_amp` unresolved symbol / no amp rows), then
  `collect_amp`/status wiring to green.
- `cargo test -p path-cli` — 329 unit (24 in cmd_share) + 113
  integration green; the 6 new integration tests cover import-help,
  cache write with the `amp-path-amp-` prefix assert, tsv, json
  `"source":"amp"`, show markdown, and the hidden `--project` shim.
- Live DoD (amp still `0.0.1785170481-ga5b614`):
  - `p list amp --format tsv` lists all three captured threads; the
    feature-elicit row carries cwd + full first prompt.
  - ⚠ `share --harness amp --session T-019fa4db-… --anon` (Roger's
    go-ahead in-session) →
    **https://pathbase.dev/u/anon/pathstash/graphs/6e472e59-6abb-43b3-b20e-a13528f850c2**
    (23 609 bytes, cached as `amp-path-amp-T-019fa4`). Re-importing that
    URL and rendering at full detail shows every elicit beat — prompt,
    thinking quotes, all 11 tool calls with results including the
    failing `cat`, the `Task` delegation with the quoted sub-agent
    answer ("11 words") — and a per-message `tokens: X in, Y out, Z
    cached` line on every assistant turn, i.e. the Q2 "clean
    per-message" level. **This URL is the artifact Alex reviews against
    the L1 DoD sentence.**
  - Reconciliation note for that review: Amp's own UI figure is
    `cumulativeBilledTokens = Σ(totalInputTokens + outputTokens)`; on
    this thread our stored counters sum to the same 232 989
    (0 in + 210 694 cache-read + 20 758 cache-write + 1 537 out) — we
    store the four real counters and the derived sum stays computable,
    never stamped (kind v1.1.0).
- Workspace gates: `just ci` **7/7** (format, shellcheck, clippy, test,
  doc, examples, site).

### Known limitations / follow-ups

- `path share` (no flags) on an amp-logged-in machine pays the N+1
  export cost during aggregation; acceptable at current thread counts,
  revisit if listing grows a cheap metadata surface.
- `test-pathbase-live.sh` anon-only mode (deviation above).
- The authed pathstash round-trip remains unexercised (Roger not logged
  in this session) — optional per plan.

## Piece 03 — projector-resume, L2 — tag: `amp-m3` — 2026-07-28

### Goal

`AmpProjector`, `p export amp`, `path resume --harness amp`, the writer
contract doc, and `scripts/verify-amp-live.sh` — gated by the deferred
resume/writer recon, whose outcome picks the route.

### What was built

- `crates/toolpath-amp/src/project.rs` — the real `AmpProjector`
  (replacing the piece-01 refusing stub): `ConversationView` →
  `ThreadExport`. Position-stable ids (turn ids pass through as
  `protocolMessageID`; synthesized ids derive from position, so the same
  view projects byte-identically), preserved delegation ids, tool-result
  carrier messages regenerated as the inverse of the forward merge,
  per-tool `run.result` reconstruction, `native_name` remap for foreign
  tools with an Amp-native passthrough list, and token re-expansion that
  regenerates the derived `totalInputTokens` while refusing to invent
  `maxInputTokens`. Plus `rehydration_prompt`.
- `crates/toolpath-amp/src/io.rs` — `ThreadWriter` trait + `CliWriter`
  (`amp threads new`, `amp threads continue -x`), the writer half of
  `ThreadFetcher`.
- `crates/path-cli/src/cmd_export.rs` — `ExportTarget::Amp` 3-mode,
  `build_amp_session`, `project_amp{,_with}` (injectable writer),
  `amp_rehydration_transcript`, `write_into_amp_project`.
- `crates/path-cli/src/cmd_resume.rs` — `"amp"` source arm, `agent:amp`
  sniff, `argv_for` → `["threads","continue",<id>]`,
  `project_into_harness` arm, stale hint fixed to list all eight harnesses.
- `scripts/verify-amp-live.sh` (shellcheck-clean),
  `docs/agents/formats/amp/writing-compatible.md`.
- Tests: 16 projector unit tests, `project_amp_returns_session_id_and_writes_artifact`
  (the unit test copilot never had) against a fake writer, the
  `file_input_explicit_amp_projects_and_records_exec` RecordingExec case
  driving a scripted `amp` stub, and argv/sniff units.

### Key decisions (ADR-style)

- **Writer route: the first-party CLI two-step, not the reverse-engineered
  server import.**
  - _Context:_ PLAN.md offered (a) local-state write, (b) API thread-create,
    (c) documented infeasibility. (a) died in piece 00 — no local thread
    store exists. So (b) was the plan of record, aiming at the thread
    actor's `POST /import` found in the bundle.
  - _Decision:_ neither. `path resume --harness amp` creates a thread with
    **`amp threads new`** (server-assigned id, no model turn, free) and
    seeds it with **`amp threads continue <id> -x <rendered transcript>`**.
    Both are documented `amp --help` surface.
  - _Rationale:_ (b) was probed live and **does not work as a REST call**.
    `POST /api/thread-actors` answers `201 Created` and creates no thread —
    `amp threads export` immediately after reports "does not exist". The
    bundle shows why: the real `/import` is a Rivet *actor* fetch behind a
    wsToken handshake plus a hardcoded gateway client key, so reaching it
    means reimplementing a credentials exchange and the RivetKit wire
    convention — several undocumented protocols at once, all re-mined on
    every `amp update`. The CLI route pins nothing undocumented, fabricates
    nothing (the thread is server-created and account-owned), and satisfies
    the piece DoD outright.
  - _Alternatives rejected:_ **the Rivet gateway client** — materially
    larger surface, version-volatile, and worth asking Amp for a sanctioned
    import path before reverse-engineering one; **route (c)** — unnecessary
    once (b′) verified.
  - _Cost accepted:_ one execute turn per resume, and a fidelity ceiling
    (below).

- **Fidelity ceiling: context transfer, not transcript import — stated
  everywhere rather than glossed.**
  - _Context:_ the seeded thread holds the prior session as one user
    message containing a Markdown transcript, not native
    `tool_use`/`tool_result`/`thinking` blocks. `amp threads export` on a
    resumed thread will not resemble the source.
  - _Decision:_ ship it, and say so plainly in `writing-compatible.md`
    (its own ⚠ section), in the `project.rs` doc comment, and on stderr at
    resume time ("context transferred (rendered transcript; not native tool
    blocks)").
  - _Rationale:_ it delivers what resume is for — the model reasons about
    the prior work correctly — and the structural projection is not lost:
    it is written beside the thread and is what `--output` emits. Claiming
    parity with the claude/copilot/codex projectors would be false.

- **Never report an import as successful on a status code alone.**
  - _Context:_ the first implementation trusted the REST `2xx` and printed
    "imported to https://ampcode.com" for a thread that did not exist.
  - _Decision:_ verify by read-back before claiming success; that
    discipline survives in the current route (the writer returns the
    server's own id, and the seeding turn's exit status is checked).
  - _Rationale:_ a false success claim is worse than a loud failure — it
    would have shipped a resume that silently resumed nothing.

### Deviations from PLAN.md

- **The route is (b′), not any of the three the plan enumerated** — see the
  ADR. `writing-compatible.md` documents all of (a)/(b)/(b′) with evidence.
- **The verification script does not pass `--no-archive-after-execute`.**
  It does not need to: auto-archiving applies to threads a fresh `-x` run
  creates, and `amp threads new` threads are not auto-archived. (An earlier
  revision of this entry claimed the flag did not exist — it does, and is
  documented `[official]` in `session-state.md` and
  `resume-and-sessions.md`.)
- **`AMP_API_KEY` is not read by path-cli.** The projector shells out to
  `amp`, which reads it from the inherited environment. Isolated runs pass
  it through; a logged-in machine needs nothing.
- **The verification script drives `p export amp --project`, not
  `path resume`** — `resume` ends in an `execvp`, and under output capture
  `amp` auto-enables execute mode and demands a message. Same projection
  code path, no exec.

### Tests & verification

- `cargo test -p toolpath-amp` — 96 tests (87 unit incl. 16 new projector
  tests, 7 integration, 2 doc).
- `cargo test -p path-cli` — lib + 11 resume integration tests green.
- Live L2 DoD (amp `0.0.1785170481-ga5b614`), both piece-00 captures:
  - feature-elicit via full `path resume --harness amp` → thread
    `T-019fa709-…`; probe "In one sentence, what was the most-used tool in
    this session?" → **"The most-used tool was `shell_command`, invoked six
    times."** Correct: the capture has exactly 6 `shell_command` calls.
  - trivial via `bash scripts/verify-amp-live.sh` → thread `T-019fa70b-…`;
    same probe → **"No tools were used in this session."** Correct.
- Two dead-end threads were NOT created during the failed REST probes (the
  route creates nothing), so no cleanup was needed.
- `just ci` — 7/7.

### Known limitations / follow-ups

- **Fidelity ceiling** (above). Cracking route (b) — or getting a
  sanctioned import path from Amp — would lift it; worth raising with Alex
  before anyone reverse-engineers the Rivet gateway.
- **One execute turn per resume** (a few cents, and a visible "ready"
  acknowledgement as the thread's first assistant message).
- **`p export amp --project` and `resume` both need network + login**,
  unlike every other harness's projector. `--output`/stdout stay offline.
- The preview banner still says preview; the five hedge sites flip in
  lockstep in piece 06, which can now cite this verification.
- `scripts/verify-amp-live.sh` judges the probe answer by eye — it asserts
  the thread loads and answers, not that the answer is right.

## Piece 04 — resume-into-cc, L3 — tag: `amp-m4` — 2026-07-28

### Goal

An Amp-shared toolpath resumes in Claude Code: probing-question pass inside
a real `claude -r` on the feature-elicit-derived doc, plus the RecordingExec
test and a fidelity pass on the tool-name mapping.

### What was built

- `crates/path-cli/tests/resume.rs::file_input_amp_source_into_claude_projects_and_records_exec`
  — amp-actor doc + `Harness::Claude`; asserts the exec is
  `claude -r <fresh-id>` where the id is a UUIDv4 **different from the amp
  thread id**, and that `<fresh-id>.jsonl` lands under the scoped
  `$HOME/.claude/projects/`.
- `cmd_export::project_claude` (the `path resume` arm; `p export claude` is
  untouched) now (1) mints a fresh UUIDv4 session id before projection and
  (2) strips `Turn.thinking` from the view. Two unit tests pin the new
  contract (`project_claude_returns_session_id_and_writes_jsonl` updated,
  `project_claude_strips_unsigned_thinking` new).
- `docs/agents/formats/claude-code/writing-compatible-jsonl.md` — the
  thinking-signature section now records the 2.1.220 behavior change with
  the verbatim API error (that doc's own maintenance rule: rules discovered
  by fixing a bug land in the same change).
- Fidelity pass on the real capture: **zero `tool_category`/name-mapping
  fallout**, so `toolpath-amp` needed no changes (evidence below).

### Key decisions (ADR-style)

- **Fresh UUIDv4 mint lives in `project_claude`, not
  `build_claude_conversation`.**
  - _Context:_ the Claude loader requires a UUIDv4 filename stem
    (`writing-compatible-jsonl.md`); amp thread ids (`T-<uuidv7-ish>`) are
    not UUIDs, so the old source-id passthrough made the live DoD
    impossible — and was a latent clobber risk for claude→claude resume
    (same id ⇒ same file under the original project dir).
  - _Decision:_ mint in the resume wrapper only; `p export claude`
    keeps passthrough.
  - _Rationale:_ the Global Constraint names this exact pattern (the
    copilot `view.id = Uuid::new_v4()` precedent); minting one level down
    would change `p export claude` behavior, violating additive-only.
  - _Alternatives rejected:_ passthrough-and-hope (contradicts the writer
    contract); minting in the shared builder (changes export).
- **Unsigned thinking is stripped at resume time, not in `ClaudeProjector`.**
  - _Context:_ live run 1 loaded fine, but the first model call after the
    probing question failed the whole turn:
    `API Error: 400 messages.1.content.0.thinking.signature.str: Input
    should be a valid string` [observed, Claude Code 2.1.220]. The IR
    carries no Anthropic thinking signatures and they cannot be
    synthesized. The old doc claim ("unsigned thinking is silently
    dropped") no longer holds at 2.1.220.
  - _Decision:_ `project_claude` clears `turn.thinking` before projection;
    the projector, `p export claude`, and every round-trip test keep
    emitting thinking.
  - _Rationale:_ resume is context transfer — text + tool history is what
    the model reasons over (proven by the probe answer); a projector-level
    drop would break claude round-trip fidelity and export inspection.
  - _Alternatives rejected:_ emitting `signature: null` (observed 400);
    folding thinking into message text (fabricates content the assistant
    never said).
- **No `toolpath-amp` changes: the piece's budgeted mapping-fallout fixes
  turned out unnecessary.** Projecting the real capture yields:
  `shell_command` → `Bash` with `cmd` → `command` input translation (all 6
  calls; the failing `cat` keeps `is_error: true`), `apply_patch` →
  `Write`, `Task` → `Task` (amp's `description`/`prompt` input keys are
  already Claude-shaped), `skill` → verbatim opaque block (per the
  piece-00 ADR that `skill` stays uncategorized). Tool-result pairing,
  `parentUuid` rewrites through synthesized result entries, and Bash-shaped
  `toolUseResult` blobs all come from the existing cross-harness machinery.

### Deviations from PLAN.md

- **One existing unit test's assertions were updated**
  (`project_claude_returns_session_id_and_writes_jsonl` pinned the
  passthrough id — the exact behavior the gap fix removes). Piece 04's
  "modify only if gaps appear" sanctions the production change; the test
  update is its unavoidable shadow, noted against the global
  "existing tests stay untouched" constraint.
- **The claude-code format doc was touched** (not in the piece's file
  list) — required by that doc's own keep-in-sync rule.
- **The live TUI was driven via `expect` + pty** (`path resume` ends in an
  `execvp` that needs a TTY). Transcripts preserved in the session
  scratchpad: `live-cc-resume-run1-thinking-400.log` (the verbatim
  rejection) and `live-cc-resume-run2-pass.log` (the pass).

### Tests & verification

- `cargo test -p path-cli` — 335 unit (+1 net) + 12 resume integration
  (+1) green; full workspace suite green.
- Live L3 DoD (Claude Code 2.1.220; input = the piece-02 anon Pathbase
  URL, resolved from cache; `-C` a scratch dir):
  - **Run 1** — session `f9cd7980-…` **loaded** in real `claude -r`: the
    loader tolerated non-UUID entry uuids (`M-…`), foreign event types
    (`skill.activated`, `thread.meta`), null `message.id`/`type`, null
    `cwd`, and reported the foreign model ("Session model gpt-5.6-sol
    could not be restored … using claude-fable-5 instead"). The model call
    then failed with the verbatim thinking-signature 400 above → fix.
  - **Run 2** — session `de014653-…`; probe "In one sentence, what was the
    most-used tool in this session?" → **"Bash was the most-used tool this
    session, with six calls covering the directory listing, file read,
    find, rg search, the intentional missing-file failure, and running
    count.sh."** Correct: the capture has exactly 6 `shell_command` calls
    and the enumeration matches them call-for-call.
- `just ci` — **7/7** (format, shellcheck, clippy, test, doc, examples,
  site).

### Known limitations / follow-ups

- **Thinking does not transfer into Claude** (fidelity ceiling for that
  channel; text, tool calls, results, and token history all transfer).
  Signed thinking cannot be synthesized; nothing to lift here.
- The strip + mint also apply to claude→claude resume — fixing a latent
  bug (IR-round-tripped thinking lost its signatures, so replay would have
  400'd identically; same-id projection could clobber the source session).
- Loader tolerance for foreign event types / non-UUID entry uuids is an
  observed-at-2.1.220 fact — re-verify on Claude Code bumps.
- Two live sessions remain under
  `~/.claude/projects/-private-tmp-…-amp-resume-cc/` (`f9cd7980` dead
  end, `de014653` verified); safe for Roger to delete.
- Amp self-updated again (`0.0.1785228716-gedda19`) but no amp code ran in
  this piece; the doc under test remains stamped `ga5b614`.
