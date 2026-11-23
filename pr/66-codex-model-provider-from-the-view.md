# fix(codex): the written session's model_provider is the view's, not always openai

Status: draft · Base: pr/65-stop-reason-defaults · Code + one recorded snapshot

## Why
Asked from the deck's "gains" slide. `toolpath-codex/src/project.rs` wrote `model_provider: "openai"` for every session. The
module doc (lines 17-19) says "`openai` unless the view names one", and the view does name one: each turn carries `provider`
(who served its model; pr/40). A pi session served by `ollama` was projected to codex as `openai`. Bug on the write side; the
code contradicted its own doc comment.

## What changes
- `project.rs`: `session_meta.model_provider` is the first turn's non-empty `provider`, else the default `openai`
  (`DEFAULT_MODEL_PROVIDER`, also the value in `SYNTHESIZED`, whose comment now says it is only invented when no turn names one).
- Snapshot re-recorded (INSTA_UPDATE=always, reviewed): `projection@pi@codex` only, `openai` -> `ollama` (the pi recording ran
  gemma on ollama). Its header `input_file` also moved from the stale `goldens/pi/input.jsonl` to `test-fixtures/pi/convo.jsonl`
  because the snapshot is rewritten from what the test passes now. No other snapshot changes: claude, codex, copilot, cursor,
  gemini and opencode turns carry no provider or the same one.

## Open: output golden needs the dev shell
`goldens/pi/to-codex.jsonl` now differs (`openai` -> `ollama`, intended). Same remedy as pr/64 and pr/65, one run for all:
`path d baseline capture --all` in `nix develop`.

## Not changed
- Nothing run unsandboxed: gitleaks, qualifier gate, `just baseline`, the pinned toolchain.
