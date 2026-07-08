# Token usage

OpenClaw records token usage in **two different shapes**, and they don't
carry the same information. Getting this right matters for toolpath's
token-accounting invariants (kind v1.1.0): the persisted transcript is the
source of truth, but it lacks the reasoning breakdown.

## Shape A: persisted per-message `Usage` (source of truth)

Stored on every `AssistantMessage.usage` (`llm-core/src/types.ts:261-275`):

```json
"usage": {
  "input": 1200, "output": 340, "cacheRead": 0, "cacheWrite": 0,
  "totalTokens": 1540,
  "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
}
```

| Field | Shape | Notes |
|---|---|---|
| `input` / `output` | int | Prompt / completion tokens for **this call**. |
| `cacheRead` / `cacheWrite` | int | Prompt-cache read / write tokens. |
| `totalTokens` | int | Headline total (= `Usage.total`). |
| `cost` | object | Per-class cost breakdown; provider-specific, often all-zero when cost can't be computed. |

Key properties:

- **Per-step delta, not cumulative.** One `Usage` per assistant message =
  one provider call. The runtime accumulator sums them with `+=`, confirming
  each is a per-call delta. So — unlike Codex's cumulative counters — you do
  **not** difference these; a session total is `Σ` of per-message totals.
- **No reasoning/thinking field.** This shape has input/output/cache/total
  only (see below).

### `totalTokens` / `prompt_tokens` conventions

`Usage.total` is `totalTokens`. **Observed** (11/11 per-call rows in real
v2026.6.11 sessions): `totalTokens = input + output + cacheRead +
cacheWrite`. OpenClaw's notion of `prompt_tokens` is `input + cacheRead`
(cacheWrite excluded). For toolpath, prefer summing the raw
`input`/`output`/`cacheRead`/`cacheWrite` independently rather than
trusting a single headline number, the way `toolpath-pi` does — it stays
correct regardless of convention drift.

### The run-cumulative aggregate row (observed)

One row per multi-call run violates "per-step delta": after a
`sessions_yield` re-context, the run's **final assembled reply** is written
as an assistant message **without a `responseId`** whose `usage` equals the
**field-wise sum of every prior per-call usage** in the run (the runtime
accumulator's totals). Its `totalTokens`, confusingly, repeats the previous
row's context total rather than the sum. Summing transcript usage naively
therefore **double-counts** such sessions. Every real API response carries
a `responseId`; the aggregate row is the one that doesn't. A reader must
detect this row (`toolpath-openclaw` drops its usage on an exact
field-wise-sum match in sessions that stamp `responseId`s) — never stamp it
onto a step.

> **Do not confuse with `SessionEntry.totalTokens`.** A separate
> `deriveSessionTotalTokens` (`src/agents/usage.ts`) produces a
> prompt/context-size snapshot that **excludes output tokens**. That is a
> context-window gauge, not a turn cost. Only `AssistantMessage.usage` is a
> per-turn spend.

## Compaction zeroes stale usage

`stripStaleAssistantUsageBeforeLatestCompaction`
(`src/agents/compaction-usage.ts`) **zeroes** the `usage` on assistant
messages at or under the latest compaction boundary (via
`makeZeroUsageSnapshot`). So **summing transcript usage naively across a
compaction undercounts** — the pre-compaction turns may carry zeroed usage.
A reader reconstructing a session total must account for compaction
boundaries (see [entry-types.md §compaction](entry-types.md#compaction) and
[lineage.md](lineage.md)).

## Reasoning tokens are runtime-only

A richer `NormalizedUsage` exists at runtime (`src/agents/usage.ts:52-60`):

```ts
type NormalizedUsage = {
  input?; output?; cacheRead?; cacheWrite?;
  reasoningTokens?: number;   // the reasoning / thinking breakdown
  total?;
};
```

`normalizeUsage` maps ~20 provider aliases into these buckets and pulls
reasoning from `reasoning_tokens` /
`completion_tokens_details.reasoning_tokens` /
`output_tokens_details.reasoning_tokens`; cache-read is de-double-counted
out of OpenAI-style prompt totals. `UsageAccumulator`
(`src/agents/embedded-agent-runner/usage-accumulator.ts`) keeps both a
running total and the last call.

But this richer usage is surfaced only on **chat stream events** and in the
**trajectory artifacts** (`src/trajectory/metadata.ts`) — it is **not** in
the persisted transcript `Usage`. A reasoning **content** block exists per
message ([messages.md §thinking](messages.md#thinking-llm-coresrctypests233-241)),
but not a reasoning **token count**.

### Consequence for toolpath derivation

- Per-step `attributed_token_usage` and per-session `token_usage` map
  cleanly from `AssistantMessage.usage` (already a per-step delta — no
  differencing).
- A reasoning `breakdowns["output"]["reasoning"]` sub-class **cannot** be
  populated from the transcript — that data only exists in the runtime
  accumulator / stream events / trajectory. Unlike Gemini/OpenCode/Codex
  (which all expose a reasoning sub-count the projector can record),
  OpenClaw records no per-step reasoning token count on disk. Omit the
  breakdown rather than fabricate it.
