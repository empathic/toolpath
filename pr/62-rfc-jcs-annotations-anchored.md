# docs(rfc): keep pr/60's rule 1 on one line so RFC.md's annotations stay anchored; resolve the :719 blocker

Status: draft · Base: pr/60-rfc-jcs-key-order · RFC.md layout and one qualifier record

## Why
pr/60 rewrote RFC.md rule 1 (JCS key ordering) as three lines where there had been one. Every span-bound qualifier
annotation below it moved by two lines, so `qualifier review` reported 31 drifted (all in RFC.md), and two tests failed:
- `path-cli --test goldens`: its `qualifier review` step fails on any drift.
- `toolpath-model --test claims`: nine `claim:rfc-*` records no longer covered their RFC.md lines.
pr/60's "cargo test was not needed for a text-only change" did not hold: these tests read RFC.md.

## What changes
- RFC.md rule 1 is one line again, with pr/60's wording unchanged (Markdown renders it the same). Lines 720 onward are back
  where the annotations expect them.
- The RFC.md:719 blocker ("False as written: … UTF-16 code units, not UTF-8 bytes") is resolved with `qualifier resolve
  --reason fixed`, pointing at pr/60. pr/60 left that for after it landed; with its line rewritten the blocker could only
  drift, and the goldens test fails on drift.

## Checked
- `qualifier review`: 240 annotations checked, 240 fresh, 0 drifted, 0 missing.
- `cargo test -p path-cli --test goldens` and `cargo test -p toolpath-model --test claims` pass.
