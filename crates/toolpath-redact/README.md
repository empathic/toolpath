# toolpath-redact

Detect and redact credentials in Toolpath documents.

The engine behind `path p redact`: it walks a `toolpath::v1::Path`, names
every string field a secret could hide in, runs a swappable set of
detectors over them, and rewrites the ones you approve.

## The shape of a redaction

Redaction is plan-then-apply, not a single opaque pass:

1. `surfaces()` names every field the map reaches, whether or not anything
   was found there. A surface with zero findings is information: the pass
   looked and the detectors were silent.
2. `plan::generate()` runs the detectors over those surfaces and emits a
   reviewable `Plan` - stable finding ids, elided context, one action per
   finding.
3. `apply()` consumes the plan and rewrites the document.

Because the plan is data, it can be decided by predicate, by picker, or by
hand-editing JSON before it is applied.

## Purity

This crate touches no environment variable, no filesystem, no clock, and
no process global. The fingerprint key arrives as bytes and the timestamp
arrives as a parameter, so a plan generated twice from the same document
is byte-identical, and the test suite needs neither a temp directory nor a
lock.

## The `Detector` contract

Detection is the part of this problem where precision is worst and the
field moves fastest, so it sits behind a trait. A `Detector` receives a
`Candidate` - a string, its `FieldShape`, its RFC 6901 pointer, and a
little context - and returns spans. It never sees the document, which is
what keeps detectors testable in isolation and leaves a harness-time hook
path open.

`DetectorSet::detect_all` normalises whatever comes back: spans that are
reversed, out of range, or split a UTF-8 codepoint are dropped, and
overlaps resolve to one finding.

A detector that would send candidate material off the machine reports
`Egress::Network` and the host refuses it unless explicitly allowed.

## Vendored ruleset

The built-in detector compiles its rules from a vendored copy of the
gitleaks configuration at `src/internal/gitleaks.toml`.

- Upstream: <https://github.com/gitleaks/gitleaks>, `config/gitleaks.toml`
- Commit: `b58d3f102cf3a2c84cb7f923d05c25c9b1aed84b` (2026-07-22)
- License: MIT, <https://github.com/gitleaks/gitleaks/blob/master/LICENSE>

The copy is kept byte-verbatim so it can be diffed against upstream. Do
not hand-edit it; rules this crate adds on top live in `internal/rules.rs`
as `supplemental_rules`, and rules that will not compile under Rust's
`regex` are listed there too.
