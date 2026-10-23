# fix(goldens): file-path keys under `changes` collapse to `<path>` in key paths

Status: draft · Base: pr/106 · Source: pr/99 finding "a machine path in a key"; controller-core's order

## Why
Codex keys an `apply_patch` call's `changes` map by the ABSOLUTE path of each patched file, which names the recording machine (`/var/folders/.../T/toolpath-elicit.XXXXXX/codex/...`).
Those keys became key paths, so the tables and shed snapshots carried the machine path and every re-recording on another machine would churn them.

## What changes
`is_file_path_key` (toolpath-goldens/src/keypaths.rs): a key starting with `/` under a `changes` object is `<path>`, in `key_paths`, `leaf_values` and `raw_paths`. Tables, round-trip and shed snapshots, and docs/baseline/{classification,unexplained}.md regenerated.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `file_path_keys_under_changes_are_collapsed` failed (the path was in the key), passes. The existing key-path tests pass.
- unexplained.md no longer contains `/var/folders` (42 lines before); 28 lines now read `changes.<path>...`.
- toolpath-goldens tests, `path-cli --features develop --test goldens`, fmt, clippy -D warnings (nix clippy 1.97.1), qualifier 238/0.
- NOT run: other crates' slices (unchanged by this piece), pinned 1.94, gitleaks, nix.
