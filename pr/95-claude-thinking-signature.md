# fix(claude): a claude -> claude round trip returns every thinking signature

Status: draft · Base: pr/93 · Approved by Bobby 2026-10-21 · Code: toolpath-convo `ThinkingSignature` + Turn field + derive/extract, toolpath-claude reader and writer, 7 writer contracts, full-view fixture · Renumbered from pr/91 on landing (pr/91 is baseline-live-evidence)

## Why (plain words)
Anthropic signs each thinking block, and the API checks the signature when a session is resumed: unsigned thinking can be rejected or stripped. The Claude reader kept the text and
dropped the signature (the IR held `thinking: Option<String>` only), and the Claude writer always wrote `signature: None`. So a resumed claude -> claude session lost it: the one
dropped field in claude -> claude that changes resume behaviour.

## What
- IR: `Turn.thinking_signature: Option<ThinkingSignature { provider, value, text_sha256 }>` (agent-tagged: provider "anthropic"; the hash is of the thinking text that was signed).
- derive writes it into the step's structural extras (`thinking_signature`), extract reads it back, so it survives the document.
- Claude reader fills it when the message has exactly one thinking block and it is signed (several blocks have no single signature over their joined text); the writer emits it only when
  the provider is anthropic AND the turn's thinking still hashes to `text_sha256` (edited text drops it).
- Every other writer ignores it: a DECLARED loss in the codex, pi, copilot, gemini, opencode, cursor and cursor-agent contracts (3 sub-paths each, reason "a thinking signature is only valid
  for the API that issued it (Anthropic's); only the claude writer returns it"). fixtures/full-view.json gained a signed thinking turn so the contract test exercises it.

## Tests (written first, failing first)
toolpath-claude/tests/real_fixture_roundtrip.rs `claude_to_claude_keeps_every_thinking_signature` (13 signed blocks in the fixture; failed with all None); unit tests: same text written back,
changed text / other provider / unsigned not written.

## Result
shed@claude* no longer lists `message.content[].signature` (lost 62 -> 61 for claude). The IR change touched every `Turn` literal in the workspace (a field with `serde(default)`, so stored IR still reads).

## Not done / notes
- `redacted_thinking` blocks: none in the recordings (0 in claude, claude-session, claude-forced), so not handled; the same approach would apply.
- Multi-block thinking messages carry no signature (see above); Claude Code writes one block per line in practice (13/13 fixture lines are single-block).
- Ben's #144 reshapes the IR into items: the signature belongs on the thinking ITEM there, keyed the same way (provider + text hash); to note in the Ben-first port plan.
- Live: the one claude -> claude own-session check (haiku) that Bobby approved needs the pinned claude CLI and a key; not run from this sandbox (scratch-main-82 / Bobby's shell).
- Cargo.lock: toolpath-convo gains `sha2` (already in the lock for other crates).
- Not run: pinned 1.94, gitleaks, nix-develop recapture.
- The recorded thinking blocks carry an empty `thinking` string with a signature (the text is hidden in Claude Code's transcripts): their `text_sha256` is the hash of "" and the signature is written back with the empty text, which is exactly what the fixture holds.
- `examples/path-02-local-session.path.json(l)` (generated from a claude session) now include `thinking_signature` on assistant steps and were regenerated on purpose (UPDATE_EXAMPLES=1).
