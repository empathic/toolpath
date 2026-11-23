# fix(convo): drop the macOS alias from cwd-less paths; capture the goldens; re-anchor qualifier records

Status: draft · Base: pr/70 · Code: toolpath-convo `derive.rs` (cwd-less path normalization), goldens recapture, qualifier re-anchors

## Why
A path with no working directory still carried macOS's `/private/var/...` alias, so the same file appeared under two spellings across agents
(`/private/var/x` vs `/var/x`) and round trips into codex, copilot, cursor-agent, gemini and pi reported it as a changed file key. The goldens and the
round-trip snapshots recorded the alias as if it were content.

## What changes
- toolpath-convo `derive.rs`: cwd-less paths drop the `/private` alias the same way cwd-relative ones already did (`strip_private_alias`).
- The goldens outputs and the affected `round_trip@*` / `shed@copilot` snapshots recaptured; qualifier records in `toolpath-convo`, `toolpath-copilot`
  and `toolpath-goldens` re-anchored to the moved lines.
- Adds the WHY sections to the pr/65, pr/67–pr/70 records.

## Verification
`cargo test --workspace` and `just baseline` green at the landing; `qualifier review` 0 drifted. Snapshot diffs are the alias only.

## For the next PR
None depends on it beyond the recaptured goldens; it rides with the pr/65–pr/70 fixes.
