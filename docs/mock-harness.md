# Mock harness: what an agent would send when it resumes

`toolpath-webmock` stands in for the model API. A **mock harness** stands in for the agent: given
a session file toolpath projected for a harness, it computes the history that harness's own
loader would send to the model on resume. With both, "resume works" becomes a `cargo test`
assertion instead of a live run of seven agent binaries.

## Why

Resume is proven today only by the resume matrix: the real binary reopens the projected session
and a live model turn answers. That cannot run in CI (binaries, credentials, quota). And every
resume bug found so far lived in the *harness-local* half of a session file, the half a
projection has to invent (`review/payload-to-session.md`: 56% of appended fields):

| bug (2026-10-15) | what the loader did | modelled by |
|---|---|---|
| pi → claude, pi → pi | the `parentUuid` / `parentId` walk stopped at the first assistant with no parent; the model saw only the newest message | the leaf-to-root walk, `unreachable` |
| copilot → claude | the Messages API refused an errored `tool_result` with empty content (400) | `check_api` |
| (claude projector guard) | same rule, defensively for every source | `check_api` |

## Buy / borrow / build

| option | verdict | why |
|---|---|---|
| Run the real binary (resume matrix, understudy screens) | keep, as the oracle | the only ground truth, but needs binaries, auth and a model; not CI |
| understudy (our TUI driver) | borrow for the contract run | drives the real TUI; says what is on screen, not what reaches the model |
| `real_fixture_roundtrip`, `cross_harness_matrix` tests | keep | prove our reader accepts our writer's output; they never apply the *harness's* loading rules, so the pi chain bug passed them |
| Call the harness's own loader as a library | per harness, later | codex (Rust, open source), gemini-cli and opencode (TypeScript, open source) and pi have loaders we could call or port; Claude Code and Copilot CLI ship minified bundles, no library. Linking four toolchains into `cargo test` costs more than modelling the rules that matter |
| webmock cassettes as the oracle for the model | borrow | a recorded resume already holds the exact `messages[]` the binary sent; the mock is checked against it |
| **Build: a small per-harness model** | build | the rules that bugs depended on are few and documented in `docs/agents/formats/*`; checked against cassettes, so drift shows up |

## Design

Crate `toolpath-harness-mock` (`publish = false`, test/dev only, like `toolpath-goldens`).

- `Message { role, blocks }`, `Block::{Text, Thinking, ToolUse, ToolResult}`: the history as the
  model API sees it, reduced to what comparisons need.
- `<harness>::resume(session) -> Resumed { messages, unreachable }`: the loader's rules for that
  harness. `unreachable` lists conversation entries present in the file that the loader never
  reaches: lost history, named by id.
- `check_api(messages)`: the model API's own rules (errored `tool_result` not empty; a
  `tool_result` follows its `tool_use`).

**Kept honest by a contract.** For each harness, a committed webmock cassette recorded from the
real binary resuming a projected session holds the `messages[]` it really sent. The mock's history
for that same projection must be a prefix of it (the binary appends the new prompt and its own
reminders). When a harness version changes how it loads, the cassette is re-recorded and the
contract says whether the mock still agrees.

**Claude Code rules** (`src/claude.rs`, checked against Claude Code 2.1.285's cassette): leaf =
last conversation entry; walk `parentUuid` to the root, through non-conversation entries; a
`user` entry is user text whatever its `message.role`; consecutive user entries merge; assistant
entries sharing `message.id` merge across intervening tool results, `tool_use` last, results
after. Results of one assistant entry's several tool calls are siblings; the loader brings back the
ones the walk passed by (rule 5, inferred).

## What it catches (tests/claude.rs)

- **Historical bugs, re-introduced in a copy of the fixture** (never in code): the pi parent-chain
  bug (entries unreached, first user message lost) and the copilot empty errored result
  (`check_api` refuses it with the API's own message). Both fail as they should.
- **Contract:** the codex golden's projection, block for block, is a prefix of what Claude Code
  2.1.285 sent (cassette `9d1356…`). Two allowances, both Claude Code's own doing: trailing
  whitespace of a text block, and the `<system-reminder>` text it appends inside the last
  `tool_result` it sends.
- **New findings on today's goldens, pinned in `KNOWN`** (a change either way fails the test):
  - **opencode → claude loses its history.** opencode records each assistant message's parent as
    the user message it answers; the projection writes that as `parentUuid`, so every assistant
    hangs off the first user turn. Claude Code's walk reaches 2 of 22 conversation entries. The
    pi → claude bug class, in a source the resume matrix never ran.
  - **cursor → claude has an unanswered tool call.** The cursor golden's Glob call has no result
    anywhere; the projection writes the `tool_use` and nothing answers it, which the Messages API
    refuses unless Claude Code repairs it on load (not verified).
- The other four projections into Claude Code (codex, copilot, gemini, pi) resume whole.

Rule 5 (parallel tool results) is inferred, not recorded: copilot → claude has a two-call turn and
resumed in the 2026-10-15 matrix. A cassette of that resume would make it a checked rule.

## Order for the other harnesses

Each harness's loading rules (what it skips, what fails the load, how it orders history) are in
[`agents/formats/validation.md`](agents/formats/validation.md), sourced to file and line.

1. **pi**: the same chain bug class (main-thread `parentId` walk); open-source loader to read the
   rules from; a pi cassette exists.
2. **copilot**: event validation (the errored result, `workspace.yaml` timestamps it rejected);
   cassette exists (replays since pr/21).
3. **codex**: `threads.rollout_path` (the absolute-path known defect) and the rollout's
   `response_item` history; open source, so rules can be lifted exactly.
4. **opencode**: database rows (`opencode-stable.db`) and the pi → opencode no-reply cell, which
   this would help diagnose without a live call.
5. **gemini**: 0.47 record replay (now in the reader); blocked on a recordable cassette (key).
6. **cursor**: composer heads in `composerHeaders`; no model cassette (no headless mode), so the
   contract would rest on Allie reading the transcript back.
