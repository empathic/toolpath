# fix(convo): file-change keys are relative to the working directory, one key per file

Status: draft · Base: pr/66-codex-model-provider-from-the-view · Code + recorded snapshots + examples

## Why
Asked from the deck's "gains" slide: the Claude source listed one file under three spellings. In `toolpath_convo::derive_path`
the `change` key of a file write was whatever the tool or the snapshot diff said: the absolute `/private/var/.../claude/notes.md`,
the macOS alias `/var/.../claude/notes.md` (same directory, other spelling), or a relative `notes.md`. One file, three keys, so a
reader could not tell it was one file and the round-trip counts counted it three times. Bug in the shared derive, undocumented.

## What changes
- `toolpath-convo/src/derive.rs`: `normalize_artifact_path(path, working_dir)` (public) files a change under the path relative to
  the session's working directory when it lies inside it (the turn's `environment.working_dir`, else `view.base.working_dir`,
  else the first turn's), drops a leading `./`, treats `/private/var` and `/var` (and `/private/tmp`, `/private/etc`) as one,
  maps the working directory itself to `.`, and leaves a path outside it, or a sibling that only shares a prefix, as given.
  Used for both `file_mutations` and the tool-input fallback. Unit test for the spellings.
- Four tests that asserted absolute keys now assert the relative key (codex derive and fidelity, cursor, opencode).
- Snapshots re-recorded (INSTA_UPDATE=always), reviewed: `document`, `shed`, 27 `round_trip` files and one projection, and
  `examples/path-02-local-session.path.json(l)` (UPDATE_EXAMPLES=1). The diffs are file keys that lose their directory prefix
  and the lost/gained key counts that follow from one file no longer being three.

## Open (need the dev shell / qualifier)
- `path d baseline check` problems: round-trip pins (claude -> codex/pi/copilot gains, copilot -> codex/pi loses and gains), and
  the output goldens from pr/65 and pr/66; all intended. One `path d baseline capture --all` in `nix develop` covers pr/64-67.
- qualifier review: 2 annotations in `crates/toolpath-convo/src/derive.rs` drift because this piece adds lines above them
  (`:400` "derive_path chains a parentless event..." and `:428` "path.head is the latest step..."). The code they describe is
  unchanged; re-anchor them with the qualifier (as pr/62 did for the RFC), do not hand-edit `.qual`.

## Not changed
- Absolute paths are still written when the file is outside the working directory.
- Nothing run unsandboxed: gitleaks, `just baseline`, the pinned toolchain.

## Amendment (gate found it incomplete)
The written codex/pi/copilot session records the CALLER's cwd (`/`, pipeline.rs CALLER_CWD), so reading it back there is no working
directory to relativise against and `/private/var/...` and `/var/...` stayed two keys. `normalize_artifact_path` now drops the macOS
alias from every absolute path (`/private/var` -> `/var`, also /tmp, /etc), whether or not the path is inside a known working
directory. Why it only showed on the way back: the first derive knows the real cwd (relative key); the written session does not.
Round-trip snapshots re-recorded; the qualifier anchors it moved were re-recorded with `qualifier record --supersedes`.
