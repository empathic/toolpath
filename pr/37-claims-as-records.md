# docs(claims): every public statement carries a qualifier record at its own text

Status: planned (asked 2026-10-19: "could we make all of these statements qualifier statements in the toolpath repo?") · Base: pr/23 (object model), pr/27 (accounting) once merged · Siblings: pr/14 / #369 (claims audit) owns the wording; this piece only records status, it changes no site text

## Why
The site (built from `site/` by `.github/workflows/deploy-site.yml`), the format page, the v1.1.0 kind pages and
the RFC make claims that the tests either prove or don't. Until now the status lived in a slide and in
`toolpath-model`'s own computation. Putting a record on each claim's lines makes the status visible where the text
is, reviewable like code, and checkable: change the text and the record drifts (`qualifier review`); add a claim
without a record and a test fails.

## What changes
- 172 claims in `docs/model/claims.json` (`file` + exact `quote`): home page 32, format page 27, FAQ page 20
  (`site/pages/faq.md`) plus 3 in `FAQ.md`, kind pages 16 (`site/kinds/index.md`,
  `site/kinds/agent-coding-session/v1.1.0/index.md`; v1.1.0 is the only kind in scope, v1.0.0's page is not
  recorded), RFC 15 (including one per "What Toolpath adds beyond any VCS" bullet), the repo README 31, and the
  14 crate READMEs docs.rs renders 28 (one doctest record per README: run, no_run = compiled only, ignore =
  unchecked; plus their prose claims). docs.rs shows the last published versions (toolpath 0.7.1), not wip.
- One qualifier record per claim on its own lines, tagged `claim:<id>`: `pass` cites the test or snapshot,
  `concern` says the gap, `blocker` when false today. Files: `site/.qual`, `site/pages/.qual`, `site/kinds/.qual`,
  `site/kinds/agent-coding-session/v1.1.0/.qual`, `.qual` (RFC.md, FAQ.md).
- One extra blocker on `RFC.md:303` for origin/main's text, which still names v1.0.0 as the only kind; the wip text
  resolves it. Resolve the record when #369 reaches main.
- `toolpath-model`: a claim's status is its record (pass → pinned, concern → partly, blocker → false, none →
  no_record); the snapshot reading stays beside it as `snapshot_status`; `check()` flags a claim with no record.
  Schema and README updated.
- `crates/toolpath-model/tests/claims.rs`: every claim's quote is in its file and a live record tagged with it
  covers those lines, read from the working tree (no regeneration needed).

## Status at a350fb8 (after pr/34 head fix and pr/27, pr/28, the core-type, query and VCS tests, signatures, the CLI surface and the quickstart gate)
172 claims: 102 pinned (pass), 64 partly (concern), 6 false (blocker), plus the blocker for main's RFC line.
- Blockers (false on wip today): v1.1.0 `meta.source` lists six harnesses but copilot documents carry `"copilot"`;
  README's harness table says gemini, opencode and cursor have no golden (all have), and that the gemini reader
  does not see 0.47 `.jsonl` (it does since pr/19); README says goldens cover four harnesses (all seven); README
  says there is no test workflow on GitHub Actions (deploy-site.yml runs quality_gates.sh on every PR).
- Concerns, by theme: projection gains and invented values; resume shown live for 19 of 28 cells, not committed;
  the page's resume URL example cannot resolve and `cargo install` from crates.io is untested; agent sessions carry no `meta.intent` and no producer emits dead ends or AST ops;
  signatures verify for ssh author/reviewer only (no CI scope, no gpg/sigstore) and no producer signs; IDs and `$ref` targets not enforced; the v1.1.0 group rule (last step of a
  run carries the total) untested; the `.path.jsonl` examples not schema-checked; unsourced positioning lines;
  FAQ open questions practice already answered (step ids, format evolution) or not (privacy); `toolpath:` bases
  constructible, never resolved.
- Re-pointed after they landed: path query over every harness (query_every_session.rs), the core types and the
  RFC's validate gaps (core_types.rs), both file formats (formats.rs), git's VCS table (vcs_mapping.rs).

## Verification
`cargo test -p toolpath-model` (claims.rs + the committed model's rules), regenerated model, workspace gates.

## Paths
```
docs/model/claims.json
docs/model/model.json
docs/model/model.schema.json
docs/model/README.md
crates/toolpath-model/src/lib.rs
crates/toolpath-model/tests/claims.rs
.qual
site/.qual
site/pages/.qual
pr/27-accounting-totals.md (one squashed-away sha replaced by "pr/01")
site/kinds/.qual
site/kinds/agent-coding-session/v1.1.0/.qual
```
