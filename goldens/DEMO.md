# Hermetic goldens demo
Inputs are real or committed sessions; `path` runs under an empty env (temp HOME/XDG/store dirs, cwd `/`), so nothing real is read or written.
1. `scripts/goldens.sh check`: all cases (codex/claude/copilot/pi origins, claude/codex/pi/copilot targets) match, and every pin matches; prints `test goldens ... ok`.
2. `scripts/goldens.sh defect`: the two pinned known defects: codex->claude foreign entry types, and `export codex -o` writing the caller's cwd (`written_cwd /` vs `source_cwd ...`).
3. Pin drift: `jq '.pins.rust_toolchain.channel="1.93.0"' goldens/manifest.json > /tmp/m && cp /tmp/m goldens/manifest.json`, then `scripts/goldens.sh check` prints `PIN MOVED ... pins.rust_toolchain.channel`; restore with `git checkout goldens/manifest.json`.
4. Real Claude session (your terminal, not a Claude session; token from `$CLAUDE_CODE_OAUTH_TOKEN`/`$ANTHROPIC_API_KEY` or Keychain `claude-code-oauth-token`): `scripts/capture-claude-session.sh`, then `scripts/goldens.sh capture`, commit. Model output varies per run: capture once; the TEST is deterministic over the committed input.
`goldens/manifest.json` pins per case input/output sha256 and command, plus the harness files, Cargo.lock, flake.lock, rust-toolchain, `path` version and the nix store `path` binary (narHash). Any move fails check by name.
Change a golden on purpose: fix the code, `scripts/goldens.sh capture`, review `git diff goldens/`, commit. Never hand-edit. A fix to either defect fails check and empties its `known-defect/*.tsv`: that is the signal.
Findings: `export codex -o` stamps the caller's cwd (harness pins `/`); the claude adapter needs `<sessionId>.jsonl`; `export copilot` is nondeterministic (random session id, key order), so that golden is canonical form.
Needs `nix`; the nix pin is verified only where its store path exists. Repo rev is recorded, not pinned (HEAD moves with every commit).
