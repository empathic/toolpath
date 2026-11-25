# toolpath-antigravity

Derive [toolpath](https://github.com/empathic/toolpath) provenance
documents from [Antigravity CLI](https://antigravity.google/docs/cli/overview)
(`agy`) conversation stores.

Antigravity is the product Google moved individual Gemini CLI users to
(2026-06-18) — a different binary (closed-source Go, the Windsurf/
Codeium "Cascade" engine), a different on-disk format (protobuf
trajectories in SQLite, or a single serialized message on older
builds), flat rather than per-project. It shares only its `~/.gemini`
root with Gemini CLI; nothing in `toolpath-gemini` applies to the
records this crate reads. See
[`docs/agents/formats/antigravity.md`](../../docs/agents/formats/antigravity.md)
for the full format reference this crate is built against, and
[`test-fixtures/antigravity/SOURCE.md`](../../test-fixtures/antigravity/SOURCE.md)
for exactly how the real samples it's tested against were captured.

## What this crate does

- Reads `~/.gemini/antigravity-cli/conversations/<uuid>.db` (current
  layout) read-only via `rusqlite`, decoding the `step_payload` protobuf
  blob per row with a small schema-free wire-format reader
  ([`proto`](src/proto.rs)) rather than a generated `prost` client — the
  on-disk schema is reverse-engineered from a closed binary and only
  partially known, so a hand-maintained client would claim more
  precision than we have.
- Maps each conversation into toolpath's provider-agnostic
  `ConversationView`: a `user_input` step opens a turn; every following
  tool call and model response up to the next `user_input` attaches to
  that one assistant turn (confirmed against a real tool-call sample —
  `agy` can emit more than one generation step per turn, e.g. one that
  decides to call a tool and a later one with the final answer).
- Reads `conversation_summaries.db` (a cache, not authoritative) and
  `cache/last_conversations.json` (what `agy -c`/`--continue` resolves).

## What it leaves out

- **No per-project filtering.** Antigravity has no per-project slot;
  `conversation_summaries.workspace_uris`' encoding is unconfirmed, so
  `list_conversations(project)` returns every conversation regardless
  of `project` rather than guess at a filter.
- **Tool result bodies are decoded only for `read_file`.** The call
  itself (`id`/`name`/`arguments_json`) decodes for every tool; the
  textual result is only decoded where we've confirmed the body's field
  layout (`read_file`, `Step` field 140 in 1.4.8). Other tools surface
  the call with `result: None`.
- **Model names are not resolved.** `ModelUsageStats.model` is a closed
  enum (`exa.codeium_common_pb.Model`); we have no table mapping its
  integer values to model name strings, so `Turn.model` stays `None`
  rather than guess.
- **No writer/projector.** Projecting a toolpath session into a store
  `agy` will resume needs a real model turn to validate against (three
  places have to agree: the `.db`, the summaries row, and
  `last_conversations.json`), and Gemini 3's function-call signatures
  can't be forged for injected tool-call history. See the task report /
  PR description for what was tried and what's still open.
- A handful of `Step` body fields seen in real samples (`Step` types
  `101`/`132`, and most of the ~130 body fields the format doc lists)
  are not decoded — only what toolpath's `ConversationView` needs.

## Status

Not yet merged into `path-cli`'s harness bundle pending review — see
the PR stack. Golden-tested against three real `agy` 1.4.8 conversations
(plain text, one tool call, and one leg of a `--conversation` resume)
captured in a disposable Linux VM with an AI Studio API key, never on
the maintainer's real `~/.gemini` store and never through the
interactive OAuth flow. Field numbers are cited inline in
[`proto.rs`](src/proto.rs) and [`reader.rs`](src/reader.rs) against
exactly those samples; nothing here is asserted from the format doc's
*inferred* claims alone.
