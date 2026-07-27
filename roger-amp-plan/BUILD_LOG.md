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
