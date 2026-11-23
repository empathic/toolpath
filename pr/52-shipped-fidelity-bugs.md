# docs: which published releases carry this week's fidelity bugs

Status: planned (2026-10-19, rehearsal feedback, product: "Did these bugs ship to users?") · Base: main · Independent,
docs only

## Why
The deck shows resume and accounting bugs fixed this week. The audience's first question is whether users have them.
The answer has to come from what was published, not from the repo. `main` has moved since the release, and the fixes
live on `wip/bdelanghe` and in draft PRs.

## What changes
- `docs/shipped-fidelity-bugs.md` (new): one row per bug. Each row gives the buggy line in the published crate
  (`file:line` inside the crates.io tarball), the first `path-cli` release that has it, and where the fix lives with
  its status. It also has the deck's one-line answer.

## Answer
Every one shipped and is in the current release, `path-cli` 0.26.0 (2026-09-18). Most have been there since 0.13.0
to 0.15.0. They stay until #361, #363 and #365 and the pieces pr/19, pr/20, pr/27 and pr/28 merge and a release is
cut. Three of them (gemini 0.47, Cursor 3.24, the opencode database name) are harness upgrades the release predates,
not regressions.

## Verification
- Downloaded and unpacked from `static.crates.io`: the latest of each crate (`path-cli` 0.26.0, `toolpath-claude`
  0.13.3, `-pi` 0.6.1, `-copilot` 0.1.0, `-gemini` 0.6.1, `-cursor` 0.2.0, `-opencode` 0.5.0, `-codex` 0.6.1, `-convo`
  0.11.1, `toolpath` 0.7.1). For the "since" column, also `path-cli` 0.13.0, 0.14.0, 0.15.0 and 0.16.1 and the crate
  versions they depend on.
- 0.26.0's dependency requirements resolve to those latest versions. GitHub's latest release, `v0.26.0`, is the same
  release.
- Each fix's removed lines (`gh pr diff 361/363/365`; `git show 635f8dc e0415cd 7637753 80e8a16 9eec499 af15dab`)
  were searched for in the tarballs. Each fix's added code was searched for on `origin/main` (77dc16a): none of it is
  there.
- Docs only: no cargo, no `site/` change.

## Paths
```
docs/shipped-fidelity-bugs.md
```
