# Hermetic goldens demo
Inputs are real or committed sessions; `path` runs under an empty env (temp HOME/XDG/store dirs, cwd `/`), so nothing real is read or written.
1. `scripts/goldens.sh check`: every case (codex->claude, claude->codex, claude-compacted->codex, plus claude-session->codex once captured) matches byte-for-byte; prints `test goldens ... ok`.
2. `scripts/goldens.sh defect`: the pinned 09-29 defect, 7 foreign entry types (token_count 13, agent_message 13, ...) in codex->claude output.
3. `scripts/capture-claude-session.sh` (your terminal, not a Claude session; token from `$CLAUDE_CODE_OAUTH_TOKEN`/`$ANTHROPIC_API_KEY` or Keychain `claude-code-oauth-token`): captures one real `claude -p` session (haiku, two tool calls) into `goldens/claude-session/`; then `scripts/goldens.sh capture` and commit.
The model's words vary per run: capture once, commit the input; the TEST is deterministic over the committed input.
Change a golden on purpose: fix the code, `scripts/goldens.sh capture`, review `git diff goldens/`, commit. Never hand-edit.
When the codex->claude projector is fixed, check FAILS with a line diff and `known-defect/*.tsv` empties: that is the signal to capture.
`goldens/manifest.json`: input/output sha256, command line, toolpath rev; the test fails if it disagrees with the files.
Findings: `p export codex -o` stamps the caller's cwd (harness pins `/`); the claude adapter needs the file named `<sessionId>.jsonl`.
Needs `nix`; goldens are `path` 0.30.0 behaviour at the manifest's rev.
