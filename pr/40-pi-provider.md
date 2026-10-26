# feat(convo,pi): who served the model rides the RFC's actor slot; pi keeps its provider across a round trip

Status: planned (ruling 2026-10-19, design consultant: "Pi provider via the RFC's existing slot; do it now") · Base: the goldens stack once merged

## Why
A pi session run on `ollama` came back as `anthropic` after any round trip: the conversation view had no field for
who served the model, the pi reader dropped `provider`, and the pi writer filled in `"anthropic"`. The format already
has the slot — RFC.md and the agent-coding-session kind put `provider` and `model` on `meta.actors["agent:…"]` — but
derive filled it with the harness name (`pi`, `claude-code`), which duplicates `meta.source` and inverts the RFC's own
example (`"agent:claude-code": {provider: "anthropic", …}`).

## What changes
- `toolpath-convo`: `Turn.provider: Option<String>` (serde default, skipped when absent). `derive` writes it to
  `meta.actors["agent:<model>"].provider` (a later turn can fill it if the first had none) and no longer writes the
  harness there; `extract` reads it back, ignoring legacy values equal to the harness id. Non-agent actors unchanged.
- `toolpath-pi`: the reader keeps `AgentMessage::Assistant.provider`; the writer prefers `Turn.provider` over its
  `"anthropic"` default.
- Stale header docs in `toolpath-pi`/`toolpath-codex` `project.rs` that described a `Turn.extra` field (it does not
  exist) are replaced with what the code does; codex's encrypted reasoning and per-call tool extras are not round-tripped.
- Every other `Turn { … }` literal in the workspace gains `provider: None` (mechanical).
- Qualifier: f898c8ff (pi provider defaults to anthropic) resolved as fixed; 39565202 (path-02 jsonl twin) resolved
  as fixed by 8813140.

## Snapshot review
- `document@*`: the agent actor's `provider` is no longer the harness (pi: `"ollama"`, others: absent until their
  readers fill it — follow-ups: codex `session_meta.model_provider`, opencode `providerID`, claude `"anthropic"`).
- `ir@pi`: each assistant turn carries `provider: "ollama"`.
- 32 cross-harness `round_trip@*` list `paths[].meta.actors.<actor>.provider` as lost: the key path now exists in the
  source only on non-agent actors (tool:/ci:), which the targets don't recreate. Not a content loss.
- `shed@*` / `goldens/shed/*.json`: IR `string_chars` drops by the harness name's length (11 for `claude-code`,
  5 `codex`, 7 `copilot`) — the removed duplicate, not conversation text. Recaptured with `path goldens shed --json`.

## Verification
- `cargo test -p toolpath-pi --test real_fixture_roundtrip provider_survives_round_trip`: the real pi fixture rewritten
  to `ollama`/`gemma4` reads, derives (`meta.actors["agent:gemma4"].provider == "ollama"`), round-trips and projects
  back with `provider: "ollama"`, `model: "gemma4"` on every assistant entry.
- `toolpath-convo` derive tests: `test_actors_in_meta` (provider = the turn's, not the harness),
  `test_agent_actor_without_provider_names_none`.
- `path goldens check`: 43 ok, 0 problems. Full gates with `--features path-cli/goldens,toolpath/verify`.

## Paths
```
crates/toolpath-convo/src/lib.rs
crates/toolpath-convo/src/derive.rs
crates/toolpath-convo/src/extract.rs
crates/toolpath-pi/src/provider.rs
crates/toolpath-pi/src/project.rs
crates/toolpath-pi/tests/real_fixture_roundtrip.rs
crates/toolpath-codex/src/project.rs
crates/*/src/**/*.rs (provider: None)
crates/toolpath-goldens/snapshots/
goldens/
```
