# chore(pr): era-restore.sh derives the machine identity instead of carrying it

Status: planned (2026-10-19: the secrets gate fails on wip) · Base: wip/bdelanghe · Scripts and docs under `pr/` only;
independent of every other piece

## Why
The secrets gate (`scripts/quality_gates.sh secrets`, gitleaks with `.gitleaks.toml`) reported 3 findings, all in
`pr/era-restore.sh` from b5aa80e:
- line 21, `machine-identity-claude-tmp` and `machine-identity-encoded-home`: the default for `S` was the absolute
  scratchpad path of the Claude Code session that wrote the script (Claude Code's per-user temp root, the encoded
  project directory naming the home, then the session id).
  That directory belonged to one dead session, so the default was wrong anywhere else.
- line 88, `machine-identity-user-at`: the scrub check greps each piece for the recording machine's email address
  and home directory so it can fail if they come back. Because the strings were written out literally, the script
  that checks for the scrubbed strings contained them itself.

None of these is a false positive (each names the machine and its user), so they are removed rather than allowlisted.

## What changes
- `pr/era-restore.sh`: `S=${S:?…}`, so the caller must set it (no default path); `SCRUB_USER=${SCRUB_USER:-$(id -un)}`,
  and the scrub check greps for `"$SCRUB_USER@empathic.dev"` and `"/Users/$SCRUB_USER"` with `git grep -F` (fixed
  strings; before, the `.` in the address matched any character). On the recording machine the needles are the same
  as before. A comment that named the home directory now describes it without naming it.
- `pr/README.md` (Repair tools): says `era-restore.sh` requires `S` and derives the scrubbed strings, and that
  `gate-piece.sh` takes its paths as arguments.
- `.gitleaks.toml`: unchanged (no allowlist entry).

## Verification
- `gitleaks dir . --config .gitleaks.toml --no-banner --redact` (8.30.1) from the repo root: 3 findings before,
  `no leaks found` after.
- `shellcheck pr/era-restore.sh` (0.11.0): clean.

## Paths
```
pr/era-restore.sh
pr/README.md
pr/42-era-restore-no-machine-identity.md
```
