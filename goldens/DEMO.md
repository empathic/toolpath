# Hermetic goldens demo
Goldens come from the REAL captured sessions in `test-fixtures/`, run through `path` under an empty env (temp HOME/XDG/store dirs, cwd `/`); nothing real is read or written.
1. `scripts/goldens.sh check` runs both cases (codex->claude, claude->codex) and prints `test goldens ... ok`: output matched byte-for-byte.
2. `scripts/goldens.sh defect` prints the pinned 09-29 defect: 7 foreign entry types (token_count 13, agent_message 13, ...) in codex->claude output.
3. `scripts/goldens.sh capture` regenerates everything hermetically; on an unchanged tree `git status --short goldens` prints nothing.
To change a golden on purpose: fix the code, run `capture`, review `git diff goldens/`, commit. Never hand-edit a golden.
When the codex->claude projector is fixed, check FAILS with a line diff and `known-defect/*.tsv` goes empty; that is the signal to capture.
`goldens/manifest.json` holds input/output sha256, command line, toolpath rev; the test fails if it disagrees with the files.
Finding: `p export codex -o` stamps the caller's cwd as the session cwd, so the harness pins cwd to `/`.
Needs `nix` (the script runs `nix develop`); goldens are `path` 0.30.0 behaviour at the manifest's rev.
