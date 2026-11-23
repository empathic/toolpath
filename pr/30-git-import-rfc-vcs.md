# test(git): RFC.md's "Relationship to Version Control Systems" table, row by row, through the git import

Status: planned (asked 2026-10-19: prove the VCS table through `path p import git`) · Base: main

## Why
RFC.md maps Commit → Step, Branch → Path, Commit message → meta.intent, GPG signature → meta.signatures, Author identity →
meta.actors, and says Toolpath preserves the dead ends VCS tools lose. Nothing tested that the git import does any of it.

## What changes
- `crates/toolpath-git/tests/vcs_mapping.rs`: a repository built in a temp dir with fixed authors and times (two authors,
  main + feature, a merge, a subject+body message, a branch deleted before import, a commit signed with a throwaway
  SSH key made by the test; never a real key) imported with `toolpath_git::derive(main, feature)`. `vcs@mapping` pins one
  row per claim:
  - Commit → Step: **holds** (one step per commit; the merge step's parents are git's).
  - Branch → Path, base context: **holds** (one path per branch; base = repo, branch point, branch name).
  - Commit message → meta.intent: **does not hold fully**, only the subject line is kept, the body is dropped
    (`commit_to_step` takes `message.lines().next()`).
  - GPG signature → meta.signatures: **does not hold**, the import never reads `gpgsig`. Not implemented here on
    purpose: a git signature covers the commit object, an RFC step signature covers `JCS({step, change})`, so copying it
    into `meta.signatures` would make RFC verification fail; it needs an RFC decision (a VCS attestation field or a
    commit-object scope).
  - Author identity → meta.actors: **holds** (name + email identity per author; one system only, committer not
    imported).
  - Dead ends preserved: **does not hold for import** (a deleted branch is gone before import; the walk follows branch
    tips only).
- Qualifier records on each RFC.md row (928–932, 937): pass ×3, concern ×3.
- `crates/toolpath-git/Cargo.toml`: dev-deps `insta` (json) and `serde_json`.

## Verification
`cargo test -p toolpath-git --test vcs_mapping`; the signature row needs `ssh-keygen` on PATH and says so when it is
absent. Workspace gates with `--features path-cli/goldens`.

## Paths
```
crates/toolpath-git/Cargo.toml
crates/toolpath-git/tests/vcs_mapping.rs
crates/toolpath-git/tests/snapshots/
Cargo.lock
.qual
```
