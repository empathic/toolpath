# Token usage accounting

Assistant entries carry a `usage` object inside `message.usage` that
records the token counts Anthropic billed for that turn, including
prompt-cache statistics. The shape has grown over time and now mixes
flat fields with nested breakdowns that duplicate the flat totals.

## One message, many lines: don't sum per entry

Claude Code writes one JSONL line **per content block** of an assistant
API message (see [entry-types](entry-types.md)), each stamped with a
`usage` object. A message with thinking + text + two `tool_use` blocks
lands as four entries. Summing `message.usage` across entries
over-counts (~3× on typical sessions) — the values are **cumulative
snapshots of one message, not per-line bills.**

The grouping key is **`message.id`** (`msg_…`), identical on every line
of the split. When a line has no `message.id`, `toolpath-claude` falls
back to the entry-level `requestId` for assistant entries
([jsonl-envelope.md](jsonl-envelope.md)): one API request yields one
assistant message, so the lines of an id-less message still form one
group. User entries never group, even if one carries a `requestId`.

A group's lines are **not** guaranteed to be contiguous: interleaved
writes (see [known-issues.md](known-issues.md), "Multi-terminal writes
to the same project") can put another message's lines between them.
Grouping is by key across the whole session, never by consecutive run;
grouping by run would count an interleaved message once per fragment.

Empirically, across every session sampled:

- `input_tokens` and the cache counters are **constant** across a
  message's lines (prompt-side cost, paid once for the message).
- `output_tokens` is **cumulative and non-decreasing**: it streams
  upward as the model generates, and the **last line carries the
  message total**. ~73% of split messages repeat one value on every
  line (stamped after generation); ~27% genuinely stream (distinct
  values). Either way the max — which is the last line — is the total.

Which of the two you see depends on the Claude Code version. Since
v2.1.132 every line of a message repeats the identical final `usage`;
older versions wrote the growing streaming snapshots
([anthropics/claude-code#27361](https://github.com/anthropics/claude-code/issues/27361)).
The field-wise max is correct for both.

Correct accounting: take the field-wise **maximum** `usage` per
distinct group key (don't trust line order; the format is undocumented).
This is what `toolpath-claude` does. A derived path puts the message
total on the group's last step in path document order, and no other step
of the group carries `token_usage`, per the
[`agent-coding-session` v1.2.0 kind](https://toolpath.net/kinds/agent-coding-session/v1.2.0/).
Derived steps follow JSONL line order, so that step is the group's last
line.

**All-zero usage is not recorded.** `<synthetic>` assistant messages
(Claude Code's locally generated placeholders, such as "No response
requested.") carry a `usage` with every counter at 0. That is a
placeholder, not a spend, so `toolpath-claude` derives no `token_usage`
for it.

**Why this is a snapshot, not a per-block bill.** The Anthropic
[streaming API](https://platform.claude.com/docs/en/build-with-claude/streaming.md)
reports usage incrementally: the `message_start` event seeds
`output_tokens` near zero, and each `message_delta` carries the running
**cumulative** total, the final delta being the message total. Claude
Code stamps each content-block line with whatever snapshot was current
when it flushed the line — so the early lines hold near-`message_start`
values and the full total lands on the last line. A real prose `text`
block routinely shows `output_tokens: 1`. The per-line values therefore
track *flush timing*, not the tokens a given block cost.

**No per-block attribution.** Because of the above, differencing
consecutive lines does **not** yield per-block token costs — the
intermediate values are streaming snapshots, not block bills. We take
the max as the message total and do not derive `attributed_token_usage`
for Claude. (Codex, by contrast, reports a genuine per-call delta — see
[`codex.md`](../codex.md).)

One caution: the `iterations` array (below) is a breakdown *inside* one
message's `usage` — subordinate detail, not an accounting unit; never sum
it alongside the enclosing totals.

## Full observed shape

```jsonc
{
  "input_tokens": 2,
  "output_tokens": 21,

  "cache_creation_input_tokens": 12291,
  "cache_read_input_tokens":     12361,

  "cache_creation": {
    "ephemeral_5m_input_tokens": 0,
    "ephemeral_1h_input_tokens": 12291
  },

  "server_tool_use": {
    "web_search_requests": 0,
    "web_fetch_requests":  0
  },

  "service_tier":   "standard",
  "inference_geo":  "",
  "speed":          "standard",

  "iterations": [
    {
      "input_tokens": 2,
      "output_tokens": 21,
      "cache_read_input_tokens":     12361,
      "cache_creation_input_tokens": 12291,
      "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 12291},
      "type": "message"
    }
  ]
}
```

## Field reference

### Core token counts

- **`input_tokens`** — tokens sent in the request (input side, after
  prompt-cache reads).
- **`output_tokens`** — tokens generated by the model in this response.

### Prompt caching

Prompt-cache counts are **separate from** `input_tokens`. The model
sees all three, but they're billed at different rates (cache writes
are more expensive than regular inputs; cache reads are cheaper).

- **`cache_creation_input_tokens`** — tokens written to the prompt
  cache during this call.
- **`cache_read_input_tokens`** — tokens retrieved from the prompt
  cache during this call.
- **`cache_creation`** — a nested breakdown of
  `cache_creation_input_tokens` by TTL bucket:
  - `ephemeral_5m_input_tokens` — cached for ~5 minutes.
  - `ephemeral_1h_input_tokens` — cached for ~1 hour.

  The two TTL buckets sum to the flat `cache_creation_input_tokens`.
  Older versions of Claude Code only emitted the flat field; newer
  versions emit both.

### Server-side tool use

- **`server_tool_use`** — counts of tool calls executed
  server-side by Anthropic (as opposed to client-side by Claude Code).
  Observed subfields: `web_search_requests`, `web_fetch_requests`. Other
  server-tool subfields may appear as Anthropic ships new built-ins.

### Billing / routing

- **`service_tier`** — `"standard"` or `"batch"`. Determines billing
  rate. Sessions run through batch API land in `"batch"`; everything
  else is `"standard"`.
- **`inference_geo`** — geographic region the request was routed to.
  Often empty string; otherwise a region code.
- **`speed`** — inference speed tier; `"standard"` is the only value
  we've seen.

### Agentic loop iterations

- **`iterations`** — array of per-iteration usage objects for turns
  that ran through an internal agentic loop (multi-step tool-use
  cycles inside a single message API call). Each iteration has the
  same shape as the enclosing `usage`.

  The enclosing totals (`input_tokens`, `output_tokens`, etc.) are
  typically the sum of the iterations' corresponding fields. Not all
  turns have `iterations`.

## Older vs. newer shapes

Versioning is gradual and largely additive. Rules of thumb:

| Shape feature                                             | Since        |
|-----------------------------------------------------------|--------------|
| Flat `input_tokens` / `output_tokens`                     | always       |
| Flat `cache_creation_input_tokens` / `cache_read_input_tokens` | always (for cached prompts) |
| Nested `cache_creation: {ephemeral_5m, ephemeral_1h}`     | 2.0.x+       |
| `service_tier`                                            | 2.0.x+       |
| `server_tool_use`                                         | 2.1.x+       |
| `iterations`                                              | 2.1.x+       |
| `inference_geo`, `speed`                                  | 2.1.x+ (often empty) |

The practical consequence: any field may be absent on an older entry.
A tool doing token accounting should treat everything but
`input_tokens` / `output_tokens` as `Optional<number>` and default to
zero when summing.

## Not recorded in `usage`

- **Thinking tokens** are not reported separately. They are included
  in the `output_tokens` total.
- **Sidechain usage** is recorded on the sidechain's own assistant
  entries, not rolled up into the parent conversation's totals.
  Cache-read tokens on sidechain entries often "mirror" the parent
  because the prompt cache is shared.
- **User-entry token counts** are not recorded. Only assistant entries
  carry `usage`.
