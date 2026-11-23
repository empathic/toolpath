# Held back (not in any PR)

- Producer pins (`feat(goldens): pin the producer…`, `repin after harness change…`, `goldens: producer sha is strict only for nix store binaries…`): complexity for a drift that has not happened. The nix-store strict path is kept as code in 02 but is not demonstrated.
- Qualifier gate: NOT held (Bobby 2026-10-15: qualifier is first-class and drives development). It stays in; the insta crate runs `qualifier review` as a test.
- `docs(goldens): commit the spec as docs/GOLDENS.md…`: the spec reads as meeting minutes; the ARCHITECTURE-style entry replaces it.
- `goldens-baseline: hermetic capture of derive + project-to-claude…`: an earlier implementation nothing references.
- `docs(goldens): demo run sheet` and the later DEMO edits: fold the behaviour parts into 02's docs; drop the run-sheet parts.

Paths (jj fileset of everything in the squashed wip that no PR file claims; `pr/cut.sh`
checks the remainder against this list; `pr/**` is always here):
```
glob:"crates/toolpath-webmock/cassettes/claude/**"
glob:"crates/toolpath-webmock/cassettes/codex/**"
glob:"crates/toolpath-webmock/cassettes/copilot/**"
glob:"crates/toolpath-webmock/cassettes/gemini/**"
glob:"crates/toolpath-webmock/cassettes/opencode/**"
glob:"crates/toolpath-webmock/cassettes/pi/**"
glob:"pr/**"
.gitignore
CHANGELOG.md
crates/path-cli/src/cmd_auth.rs
crates/path-cli/src/cmd_pathbase.rs
crates/path-cli/src/cmd_share.rs
crates/path-cli/src/derive.rs
crates/path-cli/src/projection/.qual
crates/toolpath-cli/Cargo.toml
crates/toolpath-convo/src/.qual
crates/toolpath-gemini/src/.qual
crates/toolpath-pi/src/.qual
crates/toolpath-webmock/cassettes/README.md
crates/toolpath-webmock/tests/secret_headers.rs
docs/findings.md
docs/ideas/2026-07-25-toolpath-capture-gaps-and-checkpoint.md
flake.lock
flake.nix
modules/toolpath.nix
notes/agent-sandbox-landscape.md
```
