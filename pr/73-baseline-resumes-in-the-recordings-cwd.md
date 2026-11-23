# fix(goldens): the baseline resumes each projection in the recording's own directory

Status: draft · Base: pr/71/72 · Code (pipeline.rs `resume_dir`) + 49 snapshot files + goldens/shed + manifest

## Why (plain words)
After a round trip through codex, pi, copilot, gemini or cursor-agent, the Claude recording came back with `notes.md` once plus
`/var/folders/.../claude/notes.md` four times: one file under two keys. The baseline projected every document with
`CALLER_CWD = "/"` (the directory the hermetic tool ran from), so the written session said it ran in `/`. Reading it back there is no
working directory to make paths relative to, so the absolute spellings stayed. pr/71 only merged `/private/var` with `/var`.

## What
`resume_dir(path)`: the projection is resumed in the DOCUMENT's own working directory (its base `file://` uri), exactly what a user
does with `path resume -C <project>` when resuming where the session ran; `/` only when the document names none. Used for codex, pi,
copilot and gemini (`CALLER_CWD` stays as the fallback; its doc and qualifier record say so). Test what users do, not a hermetic stand-in.

## Result
The double key is gone from the round trips into codex, pi, copilot, gemini. One case remains and is not a bug in the paths code:
claude -> cursor-agent. The cursor-agent store records no working directory (the directory is only the md5 of it in the store's path,
agent_store.rs:8), so its reader cannot make keys relative; the snapshot pins 5 absolute keys gained and the relative ones lost. A
format limit, to be declared in the cursor-agent contract (not done here).

## Open
- Re-run `capture --all` in nix develop (the manifest was captured outside it). `baseline check`'s test run passes once the qualifier
  records are fresh (re-anchored: pipeline.rs :41, :144, :344, :611).
- Not run: pinned 1.94, gitleaks.
