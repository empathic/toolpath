# test: tests read captured goldens instead of hand-typed fixtures

Status: draft · Base: pr/04 · Stack 4/4

Commits: `test: golden() stub factory (input/events/slice/with_field/first_string); roundtrip test runs on the captured claude session; FABRICATED tag`.

## Why
`tests/roundtrip.rs` round-tripped a hand-written session that no agent ever produced. The captured `claude` golden is a real session; a test that starts from it tests the adapter against the format as it is.

## What changes
- `toolpath_golden::golden("claude")`: input bytes, events, a prefix slice, one field changed on one line, first string by key.
- `tests/roundtrip.rs` starts from the golden; tests that still build their own input carry a `// FABRICATED` tag so the gap is visible.

## Verification
`cargo test -p path-cli --test roundtrip`: 2 pass.
