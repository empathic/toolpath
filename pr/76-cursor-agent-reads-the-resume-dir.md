# fix(goldens): a cursor-agent store is read with the directory it was resumed in

Status: draft · Base: c1e8cd9 (pr/63-75 landed) · Code (pipeline.rs `read_in`, `value_cwd`) + 11 snapshots + contract/cursor-agent.json

## Why (plain words)
claude -> cursor-agent -> back still gave `notes.md` once plus four `/var/folders/.../notes.md` keys. The store keeps no working
directory (only md5(cwd) in the chat folder's name), so the reader had nothing to make file keys relative to. A user knows it:
`path resume -C <project>` chose it. The baseline's read-back never said it.

## What
`read_in` sets the view's working_dir for a cursor-agent read, from the document being round-tripped (same `resume_dir` rule as pr/73),
in round_trip and twice_around. The store format and reader are unchanged. The contract's declared loss of `path.base.uri` is removed
(the writer plus a caller-supplied directory keeps it); the rest of the store's limits stay declared.

## Result
The double key is gone from claude -> cursor-agent (0 `/var/folders` keys in the snapshot). Note this is the caller's directory, not
one read out of the store: a bare `cursor-agent` store read with no directory still has absolute keys, as before. pr/82: a document that names no directory is read with none
(the `/` fallback had gemini -> cursor-agent gain `path.base.uri`).

## Not run
Pinned 1.94, gitleaks, nix-develop recapture.
