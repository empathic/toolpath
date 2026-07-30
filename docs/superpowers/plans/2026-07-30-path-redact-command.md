# `path p redact` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `path p redact` plumbing command that removes credentials from an already-generated toolpath document in place, via a reviewable plan-then-apply flow, with detection behind a swappable trait and five transform choices.

**Method:** Test-driven, with an adversarial review gate. Every task writes failing tests first, implements to green, then survives a hostile reviewer before it counts as done. Tasks are grouped into **waves**: one small blocking wave defining shared vocabulary, then wide parallel fan-out. Tracks inside a wave share no files, so separate agents work them concurrently.

**Architecture:** New `toolpath-redact` tier-2 crate holding the engine (traversal, field map, `Detector` trait, transforms, plan types, audit record). `path-cli` gets a thin `cmd_redact.rs` for args, picker, and terminal output. The crate boundary is drawn for **testability** - see the purity rule.

**Tech Stack:** Rust 2024. New deps confined to the new crate: `regex`, `aho-corasick`, `diffy`, `hmac`, `sha2`, `toml`. `path-cli` reuses its existing `fuzzy` picker and `serde_json`.

**Spec:** `docs/superpowers/specs/2026-07-30-path-redact-command-design.md` (commit `a7760d1`).

---

## Execution: how to dispatch this

### Rule zero: implement, do not deliberate

**The design is closed.** Every open question was settled in the spec, which carries a numbered Decisions section. An implementing agent that disagrees **files the objection and proceeds anyway** - it does not stop, redesign, or re-derive. Ten agents each spending five minutes reconsidering the same decision is the largest waste available here.

Do not re-derive any of these; they are answered:

| Question | Answer | Where |
|---|---|---|
| Why not redact at derive time or at egress? | Derive is shared by 7 providers and every round-trip test; egress covers one exit of four | Decision 1 |
| Why a new crate, not a module in `toolpath-convo`? | Keeps a regex engine and vendored ruleset off the 7 provider crates | Decision 2 |
| Why not `extract -> redact -> derive`? | Lossy: `derive.rs:607` writes `extra["edits"]`, `extract.rs` never reads it back | Decision 3 |
| Why in place instead of a copy? | Every downstream verb resolves a cache id, so a copy protects none of them | Decision 6 |
| Why is detection a trait? | Detection is where precision is worst and the field moves fastest; traversal is stable | Decisions 7, 8 |
| Why strings-plus-context, not `Path`, into detectors? | Testable without building a document; keeps a future hook path open | Decision 8 |
| Why does `share` not scan? | Deliberate, with the consequence stated | Non-goals |
| Why is `mask`/`partial` not the default? | They leak length and format; offered, documented, not default | Transforms |

Each task names its file, its tests, and its assertions, and gives the code to type. **Start by creating the test file and typing the test names.** A red bar is the point at which thinking becomes useful.

### Model assignment

| Model | Role | Tasks |
|---|---|---|
| **Opus** | Implement | T0, T1, T2, T7, T10 - contract design and subtle invariants. T1's overlap resolution and T7's byte-identity / idempotence / merge-not-append are where a plausible implementation is silently wrong. T10 modifies load-bearing existing code around a non-obvious hazard. |
| **Opus** | **Review** | Every task. Adversarial review is reasoning work, not proofreading. |
| **Sonnet** | Implement | T3, T4, T5, T8, T9, T11 - well-specified work against named assertions. |
| **Haiku** | Implement | T6, T12 - a clap struct and a file checklist. |
| **Haiku** | Verification loop | Runs the build continuously, fixes nothing. |

Reviewers are spawned **per track**, so review parallelises exactly as implementation does. A reviewer never edits code; it returns a change list.

---

## The adversarial review gate

**No task is done when its tests pass. A task is done when its tests pass and a hostile reviewer has run out of objections.**

### The loop

```
implement -> tests green -> REVIEW -> change list -> revise -> tests still green -> REVIEW
                                          |                                            |
                                          +-------- repeat until clean ----------------+
```

The implementing agent owns the code. The reviewer owns the objections. **The reviewer does not edit files** - it returns a numbered change list, and the implementer applies or rebuts each item. An item may be rebutted once, with a reason; a second rebuttal escalates to the human rather than looping.

### Reviewer instructions (paste this into the reviewer agent)

> You are reviewing a diff for the `toolpath-redact` implementation. You are **adversarial by mandate**: your job is to find what is wrong, not to approve. A review that finds nothing is a review that was not done properly - but do not invent problems to meet a quota, and do not restate the spec back at the author.
>
> Assume the design is closed. Do **not** raise objections to architecture decisions listed in "Rule zero" above; those are settled. Review the implementation, not the plan.
>
> Report findings as a numbered list. For each: the file and line, what is wrong, and the concrete change you want. No prose essays. If you would not block a merge on it, do not list it.
>
> Check, in this order:
>
> 1. **Correctness against the stated invariant.** Every task names its invariants. Does the code actually hold them, or only hold them for the cases the tests happen to cover? Name the input that breaks it.
> 2. **Test adequacy.** Is there a branch with no test? An error path never exercised? A boundary (empty, one element, multibyte, maximum) untested? **Say which test to add**, not "add more tests".
> 3. **Readability.** Would a competent Rust engineer unfamiliar with this code understand this function in one pass? If not, what specifically obstructs them - naming, nesting depth, an unnamed intermediate, a function doing two jobs?
> 4. **Comments.** Apply the comment policy below strictly. Over-commenting is a defect and you should report it as one.
> 5. **Idiom and simplicity.** Unnecessary `clone()`, `unwrap()` on a path that can fail in production, a hand-rolled loop where an iterator reads better, a type that could be borrowed. Do not bikeshed formatting - `cargo fmt` owns that.
>
> You have no authority to change scope. If you believe a requirement is wrong, say so once in a final "out of scope" note and move on.

### The comment policy

**Comments explain WHY, never WHAT.** The code already says what it does. A comment that restates it is noise that rots the moment the code changes.

Write a comment only when one of these is true:

1. **The code cannot be self-explanatory.** A non-obvious algorithm, an ordering constraint, a subtle invariant.
2. **A non-obvious bug or hazard was found here.** Record it so nobody reintroduces it.
3. **An external contract forces the shape.** A vendored format, a schema requirement, an upstream quirk.

**Reject these:**

```rust
// Increment the counter
count += 1;

// Loop over the findings
for f in &findings { ... }

/// Gets the name.
pub fn name(&self) -> &str { &self.name }

// Create a new detector set
let mut set = DetectorSet::default();
```

**Accept these:**

```rust
// Sort descending by start so earlier offsets stay valid as later spans
// are spliced out.
edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));

// Decode `~1` before `~0`, or `~01` round-trips wrong (RFC 6901).
let decoded = token.replace("~1", "/").replace("~0", "~");

// gitleaks' `keywords` gate exists because 221 of 222 rules carry one;
// without it a full sweep per string leaf is unaffordable.
if !self.prefilter.is_match(text) { return Ok(Vec::new()); }

// `is_unchanged` never inspects the document, so a re-derive would
// clobber an in-place redaction. Replay the policy before writing.
```

Doc comments (`///`) on public items are held to the same bar: they earn their place by saying something the signature does not. `/// Returns the id.` on `fn id(&self) -> &str` is a defect.

### Every implementing agent must

- [ ] Re-run its task's tests after **every** review revision. A revision that breaks a test is not a revision.
- [ ] Run `cargo test --workspace` before declaring the task done, not just its own module - cross-track breakage is real.
- [ ] **Add the coverage the reviewer names.** "Tests pass" is not the bar; "the tests exercise the branches" is.
- [ ] Leave `cargo clippy -- -D warnings` and `cargo fmt --check` clean.

### The verification loop (one Haiku agent, running from T0 onward)

```bash
cargo test --workspace 2>&1 | tail -40
cargo clippy --workspace -- -D warnings 2>&1 | tail -40
cargo fmt --check
```

Reports failures to whoever owns the failing file and **fixes nothing itself**. Keeps implementers implementing and catches cross-track breakage within a minute.

### Concurrency

**Per wave:** W0 = 1 · **W1 = 6** · W2 = 2 · W3 = 1 · W4 = 3, each with a paired reviewer, plus the verification agent, plus T12 (no dependencies, hand to anyone idle).

**Peak: 8 implementers + up to 6 concurrent reviewers** in Wave 1. Reviewers are short-lived; spawn one when a track goes green rather than holding one idle.

Run T0 **solo and first**, then commit. Each Wave 1 track owns exactly one file:

| Track | Owns | Implement | Review |
|---|---|---|---|
| T1 | `src/detect.rs` | Opus | Opus |
| T2 | `src/surface.rs` | Opus | Opus |
| T3 | `src/internal/` | Sonnet | Opus |
| T4 | `src/transform.rs` | Sonnet | Opus |
| T5 | `src/plan.rs` | Sonnet | Opus |
| T6 | `cmd_redact.rs` (args) | Haiku | Opus |
| T12 | docs + release files | Haiku | Opus |

T1 exports `FixedDetector`, which T7 and T8 need. If T7 would block on it, stub it locally rather than wait.

**If short on time, cut in this order:** T3's checksum validators, then `exec.rs` in T8, then T11's corpus smoke test. Do **not** cut T7's non-destruction and idempotence tests, T10, or the review gate.

---

## The purity rule (this is what makes the tests parallel)

**`toolpath-redact` touches no environment variable, no filesystem, no clock, and no process global.** The engine is a pure function:

```rust
redact(document, plan, config) -> (document, report)
```

The key arrives as **bytes**, not a path. The timestamp arrives as a **parameter**, not `Utc::now()`. Everything needing `$TOOLPATH_CONFIG_DIR`, the manifest, or a key file lives in `path-cli` and passes data in.

1. Engine tests are pure `fn(input) -> output`. No temp dirs, no locks. `cargo test -p toolpath-redact` saturates every core.
2. Determinism is free - "regenerating a plan twice yields identical bytes" is testable because no hidden clock or RNG exists.
3. The env-dependent surface shrinks to a handful of `path-cli` tests.

### Banned in new code

| Banned | Why | Use instead |
|---|---|---|
| `std::env::set_var` in a unit test | Forces the test through `config::TEST_ENV_LOCK` (`config.rs:37`), a process-wide `Mutex`. Serialized. | Subprocess tests with `.env()` on the `Command`, per `tests/integration.rs`. |
| `fuzzy::set_picker_override` in a test | A `OnceLock` (`fuzzy.rs:58`). Write-once, process-global: one test poisons the binary. | Inject the picker, mirroring `cmd_resume::ExecStrategy`. |
| `Utc::now()` in the engine | Defeats byte-identical plan assertions. | `now: DateTime<Utc>` parameter. |
| A fixed temp path | Cross-test collisions. | `tempfile::TempDir` per test. |
| Reading a key file in the engine | Filesystem dependence. | `key: &[u8]` parameter. |

---

## Dependency graph

```
WAVE 0  --  T0 vocabulary (small, blocking)
              |
              +----------+----------+----------+----------+----------+
WAVE 1      T1 normalise  T2 surfaces  T3 detector  T4 transforms  T5 plan  T6 args
              |            |            |            |              |         |
              +----------+-+------------+-+----------+              |         |
                         |                |                         |         |
WAVE 2              T7 apply         T8 plan generation ------------+         |
                    (T2, T4)         (T2, T3, T5)                             |
                         +----------------+---------------------------------- +
                                          |
WAVE 3                              T9 CLI dispatch
                                          |
                    +---------------------+---------------------+
WAVE 4         T10 sync            T11 integration          T12 docs
               (T9)                 (T9)                    (independent)
```

**Critical path: T0 -> T2 -> T7 -> T9 -> T10.** Everything else is slack.

---

## File map

- **Create** `crates/toolpath-redact/Cargo.toml`, `README.md`
- **Create** `crates/toolpath-redact/src/{lib,detect,surface,plan,transform,apply,exec}.rs`
- **Create** `crates/toolpath-redact/src/internal/{mod,rules,entropy}.rs`, `src/internal/gitleaks.toml`
- **Create** `crates/path-cli/src/cmd_redact.rs`
- **Modify** `crates/path-cli/src/{cmd_p.rs,sync/engine.rs,cache.rs}`, `crates/path-cli/Cargo.toml`, workspace `Cargo.toml`
- **Modify** `crates/path-cli/tests/integration.rs`
- **Modify** `CLAUDE.md`, `README.md`, `site/_data/crates.json`, `site/pages/crates.md`, `scripts/release.sh`, `CHANGELOG.md`

---

# WAVE 0 - blocking

## Task 0: Shared vocabulary **[Opus]**

Types and trait signatures only, bodies `todo!()`. Deliberately tiny: nothing else can start without it.

**Files:** Create `crates/toolpath-redact/Cargo.toml`, `src/{lib,detect,surface,plan,transform}.rs`; modify workspace `Cargo.toml`.

- [ ] **Step 0.1: Crate manifest**

```toml
[package]
name = "toolpath-redact"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository = "https://github.com/empathic/toolpath"
description = "Detect and redact credentials in Toolpath documents"
keywords = ["redaction", "secrets", "toolpath", "privacy"]
categories = ["development-tools"]

[dependencies]
toolpath = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
regex = "1.12"
aho-corasick = "1.1"
diffy = "0.4"
hmac = "0.12"
sha2 = "0.10"
toml = "0.8"
```

Add to workspace `members` and `[workspace.dependencies]`:

```toml
toolpath-redact = { version = "0.1.0", path = "crates/toolpath-redact" }
```

- [ ] **Step 0.2: `src/detect.rs` - the detection contract**

```rust
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldShape {
    Prose,
    ToolInput,
    ToolOutput,
    UnifiedDiff,
    FileContent,
    Uri,
    OpaqueJson,
}

#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub change_type: &'a str,
    pub tool_name: Option<&'a str>,
    pub actor: &'a str,
    pub kind: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub text: &'a str,
    pub shape: FieldShape,
    /// RFC 6901 pointer relative to the step. Passed through to the audit
    /// record verbatim, so a detector never constructs one.
    pub at: &'a str,
    pub ctx: Context<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub span: Range<usize>,
    /// Lands in the audit record, so it is part of the document contract.
    pub rule: String,
    pub score: f32,
    pub detector: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egress {
    LocalOnly,
    Network,
}

pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;

    fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>>;

    fn prefilter(&self, _text: &str) -> bool {
        true
    }

    /// The host refuses a `Network` detector unless explicitly allowed:
    /// validating a candidate against its issuing provider sends secret
    /// material off the machine.
    fn egress(&self) -> Egress {
        Egress::LocalOnly
    }
}

#[derive(Default)]
pub struct DetectorSet(Vec<Box<dyn Detector>>);

impl DetectorSet {
    pub fn push(&mut self, d: Box<dyn Detector>) {
        self.0.push(d);
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.0.iter().map(|d| d.id()).collect()
    }

    pub fn detect_all(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        todo!("T1")
    }
}
```

- [ ] **Step 0.3: `src/surface.rs`**

```rust
use crate::detect::FieldShape;

/// One field the map named, whether or not anything was found in it. A
/// surface with zero findings is information: the pass reached that field
/// and the detectors were silent.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Surface {
    pub step: String,
    pub at: String,
    pub shape: FieldShape,
    pub bytes: usize,
}

pub fn surfaces(path: &toolpath::v1::Path) -> Vec<Surface> {
    todo!("T2")
}

pub struct SurfaceCursor<'a> {
    pub(crate) path: &'a mut toolpath::v1::Path,
}

impl SurfaceCursor<'_> {
    pub fn read(&self, step: &str, at: &str) -> Option<String> {
        todo!("T2")
    }
    pub fn write(&mut self, step: &str, at: &str, value: &str) -> crate::Result<()> {
        todo!("T2")
    }
}

pub fn ptr_escape(token: &str) -> String {
    todo!("T2")
}
```

- [ ] **Step 0.4: `src/plan.rs`**

```rust
use crate::{detect::FieldShape, transform::Transform};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Redact,
    Skip,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanFinding {
    pub id: String,
    pub step: String,
    pub at: String,
    pub rule: String,
    pub span: (usize, usize),
    pub score: f32,
    pub detector: String,
    pub shape: FieldShape,
    /// Surrounding line with the match replaced by its rule name. Never
    /// the value, never its length, unless `reveal` was set.
    pub context: String,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanDefaults {
    pub transform: Transform,
    pub threshold: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub v: u32,
    pub document: String,
    pub generated: DateTime<Utc>,
    pub detectors: Vec<String>,
    pub defaults: PlanDefaults,
    pub surfaces: Vec<crate::surface::Surface>,
    pub findings: Vec<PlanFinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub predicate: Predicate,
    pub action: Action,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Rule(String),
    Shape(FieldShape),
    Step(String),
    Detector(String),
    AtPrefix(String),
    Score(Cmp, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

/// Persisted in the sync manifest so a re-derive can replay redaction.
/// Rule-based only: individual finding ids cannot be replayed against
/// content that has moved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RedactionPolicy {
    pub detectors: Vec<String>,
    pub threshold: f32,
    pub mode: Transform,
    #[serde(default)]
    pub mode_for: Vec<(String, Transform)>,
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub reject: Vec<String>,
    pub key_id: String,
}
```

- [ ] **Step 0.5: `src/transform.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    Marker,
    Remove,
    Hash,
    /// Length-preserving, and therefore publishes the exact length.
    Mask,
    /// Keeps 4 leading and 4 trailing chars: leaks provider and format.
    Partial,
}

impl Default for Transform {
    fn default() -> Self {
        Transform::Marker
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub String);

impl Fingerprint {
    /// Keyed, never a bare hash: a hash of a low-entropy secret is a
    /// dictionary attack away from the secret (EDPB 01/2025 para 88).
    pub fn new(key: &[u8], value: &str) -> Self {
        todo!("T4")
    }
}

pub trait Transformer: Send + Sync {
    fn id(&self) -> &'static str;
    fn replace(&self, rule: &str, value: &str, fp: &Fingerprint) -> String;
}
```

- [ ] **Step 0.6: `src/lib.rs`**

```rust
#![doc = include_str!("../README.md")]

pub mod apply;
pub mod detect;
pub mod exec;
pub mod internal;
pub mod plan;
pub mod surface;
pub mod transform;

pub use detect::{Candidate, Context, Detector, DetectorSet, Egress, FieldShape, Finding};
pub use plan::{Action, Plan, PlanFinding, RedactionPolicy};
pub use surface::{Surface, surfaces};
pub use transform::{Fingerprint, Transform};

use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error)]
pub enum RedactError {
    #[error("detector {0} performs network I/O; pass --allow-network-detectors to permit it")]
    NetworkDetectorRefused(String),
    #[error("plan does not match document: {0}")]
    PlanMismatch(String),
    #[error("document carries signatures over redacted content; pass --drop-signatures")]
    SignedDocument,
    #[error("pointer {0} does not resolve")]
    BadPointer(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RedactError>;

/// Everything the engine needs, supplied by the caller. No env, no
/// filesystem, no clock, no globals - see the purity rule in the plan.
#[derive(Debug, Clone)]
pub struct RedactConfig {
    pub threshold: f32,
    pub mode: Transform,
    pub mode_for: Vec<(String, Transform)>,
    pub key: Vec<u8>,
    pub now: DateTime<Utc>,
    pub drop_signatures: bool,
    pub reveal: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RedactReport {
    pub steps_touched: usize,
    pub replaced: std::collections::BTreeMap<String, usize>,
    pub flagged: std::collections::BTreeMap<String, usize>,
    pub signatures_dropped: usize,
    pub surfaces_scanned: usize,
}
```

- [ ] **Step 0.7: Verify and review**

```bash
cargo build -p toolpath-redact && cargo clippy -p toolpath-redact -- -D warnings
```

**Gate:** compiles clean **and** an Opus reviewer has signed off on the type contract - this is the one artifact every other track builds against, so a naming or shape mistake here is expensive. **Commit before fanning out.**

---

# WAVE 1 - six parallel tracks

## Task 1: Span normalisation in `DetectorSet` **[Opus impl / Opus review]**

A third-party detector is not part of this test suite, so its output is normalised rather than trusted.

**Files:** modify `crates/toolpath-redact/src/detect.rs`

- [ ] **Step 1.1: Write the hostile-input tests first**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct HostileDetector(Vec<Finding>);
    impl Detector for HostileDetector {
        fn id(&self) -> &'static str { "hostile" }
        fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            Ok(self.0.clone())
        }
    }

    fn cand(text: &str) -> Candidate<'_> {
        Candidate {
            text,
            shape: FieldShape::Prose,
            at: "/change/x/structural/extra/text",
            ctx: Context {
                change_type: "conversation.append",
                tool_name: None,
                actor: "human:t",
                kind: None,
            },
        }
    }

    fn f(span: Range<usize>, rule: &str, score: f32) -> Finding {
        Finding { span, rule: rule.into(), score, detector: "hostile" }
    }

    #[test]
    fn drops_out_of_range_spans() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..999, "x", 0.9)])));
        assert!(s.detect_all(&cand("short")).unwrap().is_empty());
    }

    #[test]
    fn drops_reversed_spans() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![Finding {
            span: 5..2, rule: "x".into(), score: 0.9, detector: "hostile",
        }])));
        assert!(s.detect_all(&cand("abcdefgh")).unwrap().is_empty());
    }

    #[test]
    fn drops_mid_codepoint_spans() {
        // "é" is two bytes; 0..1 splits it.
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..1, "x", 0.9)])));
        assert!(s.detect_all(&cand("é-tail")).unwrap().is_empty());
    }

    #[test]
    fn identical_spans_higher_score_wins() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..4, "low", 0.4), f(0..4, "high", 0.9)])));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, "high");
    }

    #[test]
    fn nested_span_container_wins_regardless_of_score() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(2..4, "inner", 0.99), f(0..8, "outer", 0.20)])));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, "outer");
    }

    #[test]
    fn output_is_sorted_and_deterministic() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![
            f(6..8, "b", 0.9), f(0..2, "a", 0.9), f(3..5, "c", 0.9),
        ])));
        let a = s.detect_all(&cand("abcdefgh")).unwrap();
        let b = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(a, b);
        assert!(a.windows(2).all(|w| w[0].span.start <= w[1].span.start));
    }

    #[test]
    fn prefilter_short_circuits_detect() {
        struct NeverCalled;
        impl Detector for NeverCalled {
            fn id(&self) -> &'static str { "never" }
            fn prefilter(&self, _t: &str) -> bool { false }
            fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
                panic!("detect() must not run when prefilter() is false")
            }
        }
        let mut s = DetectorSet::default();
        s.push(Box::new(NeverCalled));
        assert!(s.detect_all(&cand("anything")).unwrap().is_empty());
    }
}
```

- [ ] **Step 1.2: Implement `detect_all` and `normalise`**

```rust
impl DetectorSet {
    pub fn detect_all(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        let mut raw = Vec::new();
        for d in &self.0 {
            if !d.prefilter(c.text) {
                continue;
            }
            raw.extend(d.detect(c)?);
        }
        Ok(normalise(c.text, raw))
    }
}

/// Drop what cannot be applied, then resolve overlaps.
///
/// Policy from Presidio: identical spans, higher score wins; nested, the
/// container wins regardless of score. Ties break on rule id so output
/// does not depend on HashMap iteration order.
fn normalise(text: &str, mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.retain(|f| {
        f.span.start < f.span.end
            && f.span.end <= text.len()
            && text.is_char_boundary(f.span.start)
            && text.is_char_boundary(f.span.end)
    });

    findings.sort_by(|a, b| {
        a.span.start
            .cmp(&b.span.start)
            .then((b.span.end - b.span.start).cmp(&(a.span.end - a.span.start)))
            .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.rule.cmp(&b.rule))
    });

    let mut out: Vec<Finding> = Vec::new();
    for f in findings {
        match out.iter_mut().find(|o| f.span.start < o.span.end && f.span.end > o.span.start) {
            None => out.push(f),
            Some(clash) => {
                let f_len = f.span.end - f.span.start;
                let c_len = clash.span.end - clash.span.start;
                let wins = f_len > c_len
                    || (f_len == c_len && f.score > clash.score)
                    || (f_len == c_len && f.score == clash.score && f.rule < clash.rule);
                if wins {
                    *clash = f;
                }
            }
        }
    }
    out.sort_by(|a, b| a.span.start.cmp(&b.span.start).then(a.rule.cmp(&b.rule)));
    out
}
```

- [ ] **Step 1.3: Export `FixedDetector` for downstream tracks**

T7 and T8 both need it, so it lands here rather than being duplicated.

```rust
/// Canned findings, for tests exercising traversal or transform without
/// depending on regex behaviour.
pub struct FixedDetector(pub Vec<Finding>);

impl Detector for FixedDetector {
    fn id(&self) -> &'static str { "fixed" }
    fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        Ok(self.0.clone())
    }
}
```

**Gate:** `cargo test -p toolpath-redact detect::` green **and** review clean. Reviewer: press hardest on whether `normalise` holds for three-way overlaps and for spans that are adjacent but not overlapping.

---

## Task 2: The field map **[Opus impl / Opus review]**

Produces every candidate the detectors will see, which is also exactly what the dry run reports.

**Files:** modify `crates/toolpath-redact/src/surface.rs`

- [ ] **Step 2.1: Write the pointer tests first**

```rust
#[test]
fn ptr_escape_handles_urls_and_tildes() {
    assert_eq!(ptr_escape("claude://sess-abc"), "claude:~1~1sess-abc");
    assert_eq!(ptr_escape("src/config.rs"), "src~1config.rs");
    assert_eq!(ptr_escape("a~b"), "a~0b");
    assert_eq!(ptr_escape("~/x"), "~0~1x");
}

#[test]
fn ptr_escape_round_trips() {
    for raw in ["claude://sess-abc", "src/config.rs", "a~b/c", "~01"] {
        // Decode `~1` before `~0`, or `~01` round-trips wrong (RFC 6901).
        let dec = ptr_escape(raw).replace("~1", "/").replace("~0", "~");
        assert_eq!(dec, raw);
    }
}
```

- [ ] **Step 2.2: Write the traversal tests first**

```rust
#[test]
fn conversation_append_surfaces_all_text_fields() {
    let p = fixture_conversation_append();
    let ats: Vec<&str> = surfaces(&p).iter().map(|s| s.at.as_str()).collect();
    assert!(ats.iter().any(|a| a.ends_with("/structural/extra/text")));
    assert!(ats.iter().any(|a| a.ends_with("/structural/extra/thinking")));
    assert!(ats.iter().any(|a| a.contains("/tool_uses/0/input")));
    assert!(ats.iter().any(|a| a.contains("/tool_uses/0/result/content")));
}

#[test]
fn file_write_surfaces_diff_and_both_file_states() {
    let p = fixture_file_write();
    let shapes: Vec<FieldShape> = surfaces(&p).iter().map(|s| s.shape).collect();
    assert!(shapes.contains(&FieldShape::UnifiedDiff));
    assert_eq!(shapes.iter().filter(|s| **s == FieldShape::FileContent).count(), 2);
}

#[test]
fn identity_fields_are_never_surfaced() {
    for s in surfaces(&fixture_conversation_append()) {
        for banned in ["/step/id", "/step/actor", "/step/timestamp", "/step/parents"] {
            assert!(!s.at.starts_with(banned), "surfaced identity field: {}", s.at);
        }
    }
}

#[test]
fn clean_field_still_appears_as_a_surface() {
    // The dry-run guarantee: a surface with nothing in it is information.
    let p = fixture_clean_conversation();
    assert!(surfaces(&p).iter().any(|s| s.at.ends_with("/structural/extra/text")));
}

#[test]
fn delegations_recurse() {
    assert!(surfaces(&fixture_with_delegation())
        .iter()
        .any(|s| s.at.contains("/delegations/0/turns/0")));
}

#[test]
fn unknown_change_type_degrades_to_blind_walk() {
    assert!(!surfaces(&fixture_unknown_change_type()).is_empty());
}

#[test]
fn cursor_write_is_readable_at_the_same_pointer() {
    let mut p = fixture_conversation_append();
    let at = surfaces(&p)[0].at.clone();
    let step = surfaces(&p)[0].step.clone();
    let mut c = SurfaceCursor { path: &mut p };
    c.write(&step, &at, "replaced").unwrap();
    assert_eq!(c.read(&step, &at).as_deref(), Some("replaced"));
}
```

- [ ] **Step 2.3: Implement `ptr_escape` and `surfaces`**

```rust
pub fn ptr_escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

pub fn surfaces(path: &toolpath::v1::Path) -> Vec<Surface> {
    let mut out = Vec::new();
    for step in &path.steps {
        let sid = &step.step.id;
        for (artifact_key, change) in &step.change {
            let akey = ptr_escape(artifact_key);
            push(&mut out, sid, format!("/change/{akey}"), FieldShape::Uri, artifact_key);

            if let Some(raw) = &change.raw {
                push(&mut out, sid, format!("/change/{akey}/raw"), FieldShape::UnifiedDiff, raw);
            }
            let Some(s) = &change.structural else { continue };
            let base = format!("/change/{akey}/structural");
            match s.change_type.as_str() {
                "conversation.append" => append_surfaces(&mut out, sid, &base, &s.extra),
                "file.write" => file_write_surfaces(&mut out, sid, &base, &s.extra),
                // The one change type where a blind leaf walk is correct:
                // the payload is unmodelled provider JSON.
                _ => walk_json(&mut out, sid, &base, &s.extra),
            }
        }
    }
    if let Some(b) = &path.path.base {
        push(&mut out, "", "/path/base/uri".into(), FieldShape::Uri, &b.uri);
    }
    if let Some(v) = path
        .meta
        .as_ref()
        .and_then(|m| m.extra.get("vcs_remote"))
        .and_then(|v| v.as_str())
    {
        push(&mut out, "", "/meta/vcs_remote".into(), FieldShape::Uri, v);
    }
    out
}

fn push(out: &mut Vec<Surface>, step: &str, at: String, shape: FieldShape, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push(Surface { step: step.to_string(), at, shape, bytes: text.len() });
}
```

Implement `append_surfaces` (`text`/`thinking` as `Prose`; `tool_uses[].input` as `ToolInput` recursing to string leaves; `tool_uses[].result.content` as `ToolOutput`; `delegations[]` recursively; `environment.working_dir` as `Uri`), `file_write_surfaces` (`before`/`after`/`edits[]` as `FileContent`), and `walk_json`.

- [ ] **Step 2.4: Implement `SurfaceCursor`**

Split the pointer on `/`, decode each token (`~1` then `~0`), walk `serde_json::Value` by key or array index. Read and write must resolve identically - the test in 2.2 pins that.

**Gate:** `cargo test -p toolpath-redact surface::` green **and** review clean. Reviewer: check the pointer decode order, and that `bytes` is byte length rather than char length everywhere it is compared.

---

## Task 3: The internal detector **[Sonnet impl / Opus review]**

**Files:** create `crates/toolpath-redact/src/internal/{mod,rules,entropy}.rs`, `src/internal/gitleaks.toml`

- [ ] **Step 3.1: Vendor the ruleset**

Copy `config/gitleaks.toml` from `github.com/gitleaks/gitleaks` verbatim. Record the upstream commit and MIT license in the crate README.

- [ ] **Step 3.2: Write the compile-guard test first**

If this fails, stop: the vendoring assumption is wrong and the task changes shape.

```rust
#[test]
fn every_vendored_rule_compiles_under_rust_regex() {
    let rules = load_rules();
    assert!(rules.len() >= 200, "expected the full ruleset, got {}", rules.len());
    for r in &rules {
        regex::Regex::new(&r.regex)
            .unwrap_or_else(|e| panic!("rule {} failed to compile: {e}", r.id));
    }
}
```

- [ ] **Step 3.3: Write positive and negative fixtures first**

```rust
#[test]
fn detects_shipped_formats() {
    for (label, sample) in [
        ("aws", "AKIAIOSFODNN7REALKEY"),
        ("google", "AIzaSyD-0123456789abcdefghijklmnopqrstu"),
        ("jwt", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.QQQQQQQQQQ"),
        ("pem", "-----BEGIN RSA PRIVATE KEY-----"),
        ("dburi", "postgres://u:s3cr3tpass@db.internal:5432/prod"),
    ] {
        assert!(!detect_one(sample).is_empty(), "missed {label}");
    }
}

#[test]
fn documented_false_positives_stay_below_threshold() {
    for sample in [
        "AKIAIOSFODNN7EXAMPLE",                    // AWS's own documentation key
        "redis://localhost:6379",                  // no password
        "0e2b3d4e3dec5f38ae95f62519eb2736f73c0b",  // git SHA
        "550e8400-e29b-41d4-a716-446655440000",    // UUID
        "ThisIsAReallyLongString",                 // high entropy, not a secret
    ] {
        assert!(detect_one(sample).iter().all(|f| f.score < 0.8), "false positive on {sample}");
    }
}

#[test]
fn diff_spans_never_cross_a_newline() {
    let c = diff_candidate();
    for f in InternalDetector::new().detect(&c).unwrap() {
        assert!(!c.text[f.span.clone()].contains('\n'));
    }
}

#[test]
fn uri_shape_redacts_only_the_password() {
    let c = uri_candidate("postgres://svc_user:h0rr1bl3@db.internal:5432/prod");
    let f = &InternalDetector::new().detect(&c).unwrap()[0];
    assert_eq!(&c.text[f.span.clone()], "h0rr1bl3");
}

#[test]
fn existing_markers_are_never_re_detected() {
    assert!(detect_one("[REDACTED:aws-access-key-id:a3c829]").is_empty());
    assert!(detect_one("████████████████████").is_empty());
}
```

- [ ] **Step 3.4: Implement rule loading and the prefilter**

```rust
pub struct Rule {
    pub id: String,
    pub regex: String,
    pub entropy: Option<f64>,
    pub keywords: Vec<String>,
    pub stopwords: Vec<String>,
}

pub struct InternalDetector {
    rules: Vec<(Rule, regex::Regex)>,
    prefilter: aho_corasick::AhoCorasick,
}
```

Build the automaton with `MatchKind::LeftmostLongest` over every rule keyword.

- [ ] **Step 3.5: Implement scoring**

Base score from the rule; subtract when Shannon entropy is below the rule's threshold; add when a hotword appears within 50 characters; clamp to `0.0..=1.0`.

```rust
pub fn shannon(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = std::collections::HashMap::new();
    for ch in s.chars() {
        *counts.entry(ch).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    -counts.values().map(|&c| { let p = c as f64 / n; p * p.log2() }).sum::<f64>()
}
```

- [ ] **Step 3.6: Implement checksum validators** *(first cut candidate)*

GitHub PAT: last 6 chars are base62 CRC32 of the body. AWS key id: base32 body. Luhn for cards.

**Gate:** `cargo test -p toolpath-redact internal::` green **and** review clean. Reviewer: the scoring function is the highest-risk code here - demand a test per scoring branch.

---

## Task 4: Transforms and fingerprints **[Sonnet impl / Opus review]**

**Files:** modify `crates/toolpath-redact/src/transform.rs`

- [ ] **Step 4.1: Write the tests first**

```rust
#[test]
fn fingerprint_is_deterministic() {
    let k = b"test-key";
    assert_eq!(Fingerprint::new(k, "abc"), Fingerprint::new(k, "abc"));
    assert_ne!(Fingerprint::new(k, "abc"), Fingerprint::new(b"other", "abc"));
}

#[test]
fn mask_preserves_character_count_not_byte_count() {
    let out = apply_transform(Transform::Mask, "rule", "héllo", &fp());
    assert_eq!(out.chars().count(), 5);
}

#[test]
fn partial_falls_back_to_mask_below_the_floor() {
    let out = apply_transform(Transform::Partial, "rule", "short", &fp());
    assert!(!out.contains("short"));
    assert_eq!(out.chars().count(), 5);
}

#[test]
fn only_partial_ever_emits_a_substring_of_its_input() {
    let value = "AKIAIOSFODNN7REALKEY";
    for t in [Transform::Marker, Transform::Remove, Transform::Hash, Transform::Mask] {
        let out = apply_transform(t, "aws-access-key-id", value, &fp());
        for w in 4..=value.len() {
            for s in value.as_bytes().windows(w) {
                let sub = std::str::from_utf8(s).unwrap();
                assert!(!out.contains(sub), "{t:?} leaked {sub:?}");
            }
        }
    }
}

#[test]
fn right_to_left_application_matches_one_at_a_time() {
    let text = "aaa BBB ccc DDD eee";
    let spans = vec![4..7, 12..15];
    assert_eq!(apply_spans(text, &spans), apply_one_by_one_from_right(text, &spans));
}

#[test]
fn per_rule_override_beats_global_and_per_finding_beats_both() {
    let cfg = cfg_with(Transform::Marker, vec![("us-ssn".into(), Transform::Mask)]);
    assert_eq!(resolve(&cfg, "us-ssn", None), Transform::Mask);
    assert_eq!(resolve(&cfg, "aws-access-key-id", None), Transform::Marker);
    assert_eq!(resolve(&cfg, "us-ssn", Some(Transform::Remove)), Transform::Remove);
}
```

- [ ] **Step 4.2: Implement**

```rust
impl Fingerprint {
    pub fn new(key: &[u8], value: &str) -> Self {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(value.as_bytes());
        Fingerprint(hex(&mac.finalize().into_bytes())[..6].to_string())
    }
}

pub fn apply_transform(t: Transform, rule: &str, value: &str, fp: &Fingerprint) -> String {
    match t {
        Transform::Marker => format!("[REDACTED:{rule}:{}]", fp.0),
        Transform::Remove => String::new(),
        Transform::Hash => fp.0.clone(),
        Transform::Mask => "\u{2588}".repeat(value.chars().count()),
        Transform::Partial => {
            let n = value.chars().count();
            if n > 10 {
                let head: String = value.chars().take(4).collect();
                let tail: String = value.chars().skip(n - 4).collect();
                format!("{head}\u{2026}{tail}")
            } else {
                "\u{2588}".repeat(n)
            }
        }
    }
}

/// Sort descending by start so earlier offsets stay valid as later spans
/// are spliced out.
pub fn apply_spans_desc(text: &str, edits: &mut [(std::ops::Range<usize>, String)]) -> String {
    edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    let mut out = text.to_string();
    for (span, repl) in edits.iter() {
        out.replace_range(span.clone(), repl);
    }
    out
}
```

**Gate:** `cargo test -p toolpath-redact transform::` green **and** review clean.

---

## Task 5: Plan predicates and verification **[Sonnet impl / Opus review]**

**Files:** modify `crates/toolpath-redact/src/plan.rs`

- [ ] **Step 5.1: Write the tests first**

```rust
#[test]
fn parses_every_predicate_field() {
    assert!(matches!(parse_predicate("rule=aws-access-key-id").unwrap(), Predicate::Rule(_)));
    assert!(matches!(parse_predicate("shape=unified_diff").unwrap(), Predicate::Shape(_)));
    assert!(matches!(parse_predicate("step=turn-0f3a").unwrap(), Predicate::Step(_)));
    assert!(matches!(parse_predicate("detector=internal").unwrap(), Predicate::Detector(_)));
    assert!(matches!(parse_predicate("at=/change/x").unwrap(), Predicate::AtPrefix(_)));
    assert!(matches!(parse_predicate("score>=0.95").unwrap(), Predicate::Score(Cmp::Ge, _)));
}

#[test]
fn rejects_anything_else_clearly() {
    let e = parse_predicate("colour=red").unwrap_err().to_string();
    assert!(e.contains("colour"), "error should name the bad field: {e}");
}

#[test]
fn last_matching_decision_wins() {
    let mut plan = plan_with_findings(&[("f01", "aws-access-key-id", 0.99)]);
    apply_decisions(&mut plan, &[
        decision("rule=aws-access-key-id", Action::Redact),
        decision("score>=0.9", Action::Skip),
    ]);
    assert_eq!(plan.findings[0].action, Action::Skip);
}

#[test]
fn ids_are_stable_across_regeneration() {
    let p = fixture_path();
    let a = generate(&p, &detectors(), &cfg());
    let b = generate(&p, &detectors(), &cfg());
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
}

#[test]
fn verify_refuses_a_changed_document() {
    let p = fixture_path();
    let plan = generate(&p, &detectors(), &cfg());
    let mutated = mutate_one_byte_inside_a_recorded_span(&p);
    let e = verify(&plan, &mutated).unwrap_err().to_string();
    assert!(e.contains("f01"), "error should name the first divergence: {e}");
}

#[test]
fn context_never_carries_the_value_or_its_length() {
    let plan = generate(&fixture_with_secret("AKIAIOSFODNN7REALKEY"), &detectors(), &cfg());
    let ctx = &plan.findings[0].context;
    assert!(!ctx.contains("AKIAIOSFODNN7REALKEY"));
    assert!(!ctx.contains("20"));
    assert!(ctx.contains("<aws-access-key-id>"));
}

#[test]
fn reveal_includes_the_value() {
    let cfg = RedactConfig { reveal: true, ..cfg() };
    let plan = generate(&fixture_with_secret("AKIAIOSFODNN7REALKEY"), &detectors(), &cfg);
    assert!(plan.findings[0].context.contains("AKIAIOSFODNN7REALKEY"));
}
```

- [ ] **Step 5.2: Implement predicate parsing**

Only these forms. No expression language.

```rust
pub fn parse_predicate(s: &str) -> crate::Result<Predicate> {
    // Longest operators first, or `>=` parses as `>`.
    for (op, cmp) in [(">=", Cmp::Ge), ("<=", Cmp::Le), (">", Cmp::Gt), ("<", Cmp::Lt)] {
        if let Some((k, v)) = s.split_once(op) {
            if k.trim() == "score" {
                return Ok(Predicate::Score(cmp, v.trim().parse().map_err(|_| bad(s))?));
            }
        }
    }
    let (k, v) = s.split_once('=').ok_or_else(|| bad(s))?;
    Ok(match k.trim() {
        "rule" => Predicate::Rule(v.trim().into()),
        "shape" => Predicate::Shape(parse_shape(v.trim())?),
        "step" => Predicate::Step(v.trim().into()),
        "detector" => Predicate::Detector(v.trim().into()),
        "at" => Predicate::AtPrefix(v.trim().into()),
        "score" => Predicate::Score(Cmp::Eq, v.trim().parse().map_err(|_| bad(s))?),
        other => return Err(unknown_field(other)),
    })
}
```

- [ ] **Step 5.3: Implement id generation, verification, context builder**

Ids are `f01`, `f02`, ... ordered by `(step index, pointer, span start)`, which is what makes a regenerated plan byte-identical. `verify()` checks document id, step existence, and that each span still lands at the recorded offsets, naming the first divergence.

**Gate:** `cargo test -p toolpath-redact plan::` green **and** review clean. Reviewer: `>=` versus `>` ordering in the parser is a classic silent bug - confirm there is a test.

---

## Task 6: CLI argument surface **[Haiku impl / Opus review]**

Parsing only, no dispatch, so it does not wait on the engine.

**Files:** create `crates/path-cli/src/cmd_redact.rs`; modify `crates/path-cli/src/cmd_p.rs`

- [ ] **Step 6.1: Define `RedactArgs`**

```rust
use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct RedactArgs {
    /// Cache id or file path.
    #[arg(short, long)]
    pub input: String,

    /// Write elsewhere instead of in place.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[arg(long, conflicts_with = "plan")]
    pub dry_run: bool,
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// Include real values in the plan (written 0600).
    #[arg(long)]
    pub reveal: bool,

    #[arg(long, value_name = "PREDICATE")]
    pub accept: Vec<String>,
    #[arg(long, value_name = "PREDICATE")]
    pub reject: Vec<String>,
    #[arg(long)]
    pub interactive: bool,
    #[arg(long, value_name = "PREDICATE:TRANSFORM")]
    pub mode_for: Vec<String>,

    #[arg(long, default_value = "internal")]
    pub detector: Vec<String>,
    #[arg(long, default_value_t = 0.8)]
    pub threshold: f32,
    #[arg(long)]
    pub allow_network_detectors: bool,

    #[arg(long, value_enum, default_value_t = TransformArg::Marker)]
    pub mode: TransformArg,
    #[arg(long)]
    pub key_file: Option<PathBuf>,

    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub drop_signatures: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum TransformArg { Marker, Remove, Hash, Mask, Partial }
```

- [ ] **Step 6.2: Wire `PCommand::Redact`** in `cmd_p.rs` with a `todo!()` dispatch. T9 fills it in.

- [ ] **Step 6.3: Define the injectable picker**

**Do not call `fuzzy::set_picker_override` anywhere.** Mirror `cmd_resume::ExecStrategy`:

```rust
pub(crate) trait PickerStrategy {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>>;
}

pub(crate) struct RealPicker;

pub(crate) struct RecordingPicker {
    pub selection: Vec<String>,
    pub seen: std::cell::RefCell<Vec<String>>,
}
```

- [ ] **Step 6.4: Write the arg tests**

```rust
#[test]
fn dry_run_conflicts_with_plan() {
    assert!(try_parse(&["redact", "-i", "x", "--dry-run", "--plan", "p.json"]).is_err());
}

#[test]
fn mode_for_rejects_unknown_transform() {
    assert!(parse_mode_for("rule=x:invented").is_err());
}

#[test]
fn detector_flag_is_repeatable() {
    let a = try_parse(&["redact", "-i", "x", "--detector", "internal", "--detector", "exec:/bin/s"]).unwrap();
    assert_eq!(a.detector.len(), 2);
}
```

**Gate:** `cargo test -p path-cli cmd_redact::args` green **and** review clean.

---

# WAVE 2 - two parallel tracks

## Task 7: Apply **[Opus impl / Opus review]** *(needs T2, T4)*

Consumes a `Plan`; needs no detector, so it does not wait on T3.

**Files:** modify `crates/toolpath-redact/src/apply.rs`

- [ ] **Step 7.1: Write the invariant tests first. These are the suite.**

```rust
#[test]
fn no_findings_means_byte_identical_output() {
    // The most important test here: the pass must not perturb anything it
    // is not redacting.
    let before = fixture_clean_document();
    let mut after = before.clone();
    apply(&mut after, &empty_plan(&before), &cfg()).unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&before).unwrap(),
        serde_json::to_string_pretty(&after).unwrap()
    );
}

#[test]
fn unknown_provider_keys_survive() {
    // Guards against reimplementing this as extract -> derive:
    // `extra["edits"]` is written by toolpath-convo's derive and never
    // read back by extract, so a round-trip silently drops it.
    let mut doc = fixture_with_extra_keys(&["edits", "vendor_specific", "entry_extra"]);
    apply(&mut doc, &plan_touching_only_text(&doc), &cfg()).unwrap();
    for k in ["edits", "vendor_specific", "entry_extra"] {
        assert!(has_key_somewhere(&doc, k), "lost {k}");
    }
}

#[test]
fn idempotent_across_all_transforms() {
    for mode in [Transform::Marker, Transform::Remove, Transform::Hash,
                 Transform::Mask, Transform::Partial] {
        let cfg = RedactConfig { mode, ..cfg() };
        let mut once = fixture_with_secrets();
        apply(&mut once, &plan_for(&once), &cfg).unwrap();
        let mut twice = once.clone();
        apply(&mut twice, &plan_for(&twice), &cfg).unwrap();
        assert_eq!(
            serde_json::to_string(&once).unwrap(),
            serde_json::to_string(&twice).unwrap(),
            "{mode:?} is not idempotent"
        );
    }
}

#[test]
fn redacted_diff_still_parses_and_line_counts_hold() {
    let mut doc = fixture_file_write_with_secret_in_diff();
    apply(&mut doc, &plan_for(&doc), &cfg()).unwrap();
    let raw = raw_diff_of(&doc);
    let patch = diffy::Patch::from_str(&raw).expect("redacted diff must still parse");
    for h in patch.hunks() {
        assert_eq!(h.old_range().len(), count_lines(h, '-'));
        assert_eq!(h.new_range().len(), count_lines(h, '+'));
    }
}

#[test]
fn audit_record_lands_on_the_step_and_merges_on_rerun() {
    let mut doc = fixture_with_secrets();
    apply(&mut doc, &plan_for(&doc), &cfg()).unwrap();
    let first = record_of(&doc, "turn-0f3a").clone();
    apply(&mut doc, &plan_for(&doc), &cfg()).unwrap();
    assert_eq!(record_of(&doc, "turn-0f3a"), &first, "record must merge, not append");
}

#[test]
fn audit_record_carries_no_value_substring_or_length() {
    let secret = "AKIAIOSFODNN7REALKEY";
    let mut doc = fixture_with_secret(secret);
    apply(&mut doc, &plan_for(&doc), &cfg()).unwrap();
    let rec = serde_json::to_string(record_of(&doc, "turn-0f3a")).unwrap();
    assert!(!rec.contains(secret));
    for w in 6..secret.len() {
        for s in secret.as_bytes().windows(w) {
            assert!(!rec.contains(std::str::from_utf8(s).unwrap()));
        }
    }
    assert!(!rec.contains(&secret.len().to_string()));
}

#[test]
fn signed_document_refuses_without_the_flag() {
    let mut doc = fixture_signed();
    assert!(matches!(
        apply(&mut doc, &plan_for(&doc), &cfg()),
        Err(RedactError::SignedDocument)
    ));
    let cfg = RedactConfig { drop_signatures: true, ..cfg() };
    assert_eq!(apply(&mut doc, &plan_for(&doc), &cfg).unwrap().signatures_dropped, 1);
}

#[test]
fn output_validates_against_both_schemas() {
    let mut doc = fixture_with_secrets();
    apply(&mut doc, &plan_for(&doc), &cfg()).unwrap();
    let v = serde_json::to_value(&doc).unwrap();
    assert!(base_schema().is_valid(&v));
    assert!(kind_schema_v1_1_0().is_valid(&v));
}
```

- [ ] **Step 7.2: Implement `apply`**

```rust
pub fn apply(
    path: &mut toolpath::v1::Path,
    plan: &crate::plan::Plan,
    cfg: &crate::RedactConfig,
) -> crate::Result<crate::RedactReport> {
    crate::plan::verify(plan, path)?;
    guard_signatures(path, cfg)?;

    let mut report = crate::RedactReport {
        surfaces_scanned: plan.surfaces.len(),
        ..Default::default()
    };

    // Group by field so every edit to one string applies in a single
    // right-to-left pass; applying them one at a time would invalidate
    // the offsets of the ones still pending.
    for ((step, at), group) in group_by_field(plan) {
        let mut cursor = crate::surface::SurfaceCursor { path };
        let Some(text) = cursor.read(&step, &at) else { continue };

        let mut edits = Vec::new();
        for f in group.iter().filter(|f| f.action == crate::plan::Action::Redact) {
            let value = &text[f.span.0..f.span.1];
            let fp = crate::transform::Fingerprint::new(&cfg.key, value);
            let t = resolve_transform(cfg, &f.rule, f.transform);
            edits.push((
                f.span.0..f.span.1,
                crate::transform::apply_transform(t, &f.rule, value, &fp),
            ));
            *report.replaced.entry(f.rule.clone()).or_default() += 1;
        }
        if edits.is_empty() {
            continue;
        }
        let new_text = crate::transform::apply_spans_desc(&text, &mut edits);
        cursor.write(&step, &at, &new_text)?;
        report.steps_touched += 1;
    }

    write_step_records(path, plan, cfg)?;
    write_rollup(path, plan, cfg, &report)?;
    Ok(report)
}
```

- [ ] **Step 7.3: Implement the audit record**

`Step.meta.extra["redaction"]`, creating `Step.meta` where absent. Aggregate by `(rule, fp)`. **Merge, never append.** Rollup at `path.meta.extra["redaction"]`.

**Gate:** `cargo test -p toolpath-redact apply::` green **and** review clean. Reviewer: this task carries the most invariants in the codebase - for each one, name an input the tests do not cover and demand a test for it.

---

## Task 8: Plan generation **[Sonnet impl / Opus review]** *(needs T2, T3, T5)*

**Files:** modify `crates/toolpath-redact/src/plan.rs`; create `src/exec.rs`

- [ ] **Step 8.1: Write the tests first**

```rust
#[test]
fn surfaces_and_findings_are_both_populated() {
    let plan = generate(&fixture_mixed(), &detectors(), &cfg());
    assert!(plan.surfaces.iter().any(|s| findings_at(&plan, &s.at) == 0));
    assert!(!plan.findings.is_empty());
}

#[test]
fn two_detectors_merge_through_one_resolution() {
    let mut set = DetectorSet::default();
    set.push(Box::new(FixedDetector(vec![f(0..20, "a", 0.9)])));
    set.push(Box::new(FixedDetector(vec![f(0..20, "b", 0.5)])));
    assert_eq!(generate(&fixture_one_field(), &set, &cfg()).findings.len(), 1);
}

#[test]
fn network_detector_is_refused_without_the_flag() {
    let mut set = DetectorSet::default();
    set.push(Box::new(NetworkDetector));
    assert!(matches!(
        generate_checked(&fixture_one_field(), &set, &cfg()),
        Err(RedactError::NetworkDetectorRefused(_))
    ));
}
```

- [ ] **Step 8.2: Implement `generate`**

Walk `surfaces()`, build a `Candidate` per surface, call `DetectorSet::detect_all`, convert to `PlanFinding` with a stable id and elided context, set `action` from the threshold.

- [ ] **Step 8.3: Implement `exec.rs`** *(second cut candidate)*

One JSON object per candidate on stdin, one array of findings on stdout. Test against a fixture shell script, not a real scanner.

**Gate:** `cargo test -p toolpath-redact plan_gen:: exec::` green **and** review clean.

---

# WAVE 3 - synchronisation point

## Task 9: CLI dispatch **[Sonnet impl / Opus review]** *(needs T6, T7, T8)*

**Files:** modify `crates/path-cli/src/cmd_redact.rs`, `crates/path-cli/src/cache.rs`

- [ ] **Step 9.1: Write the tests first**

```rust
#[test]
fn cache_input_rewrites_in_place() {
    let cfg = sandbox();
    seed_cached_document(&cfg, "claude-abc123");
    run_redact(&cfg, &["-i", "claude-abc123"]).unwrap();
    assert!(read_cached(&cfg, "claude-abc123").contains("[REDACTED:"));
    assert_eq!(mode_of(cache_path(&cfg, "claude-abc123")), 0o600);
}

#[test]
fn key_is_generated_once_and_reused() {
    let cfg = sandbox();
    seed_cached_document(&cfg, "claude-abc123");
    run_redact(&cfg, &["-i", "claude-abc123"]).unwrap();
    let first = fingerprints_in(read_cached(&cfg, "claude-abc123"));
    reseed_same_document(&cfg, "claude-abc123");
    run_redact(&cfg, &["-i", "claude-abc123"]).unwrap();
    assert_eq!(first, fingerprints_in(read_cached(&cfg, "claude-abc123")));
}

#[test]
fn interactive_uses_the_injected_picker() {
    let picker = RecordingPicker { selection: vec!["f01".into()], seen: Default::default() };
    let out = run_redact_with_picker(&sandbox(), &["-i", "doc.json", "--interactive"], &picker);
    assert_eq!(picker.seen.borrow().len(), 3);
    assert_eq!(redacted_ids(&out), vec!["f01"]);
}

#[test]
fn dry_run_exits_one_when_findings_exist() {
    assert_eq!(run_redact(&sandbox(), &["-i", "doc.json", "--dry-run"]).code(), 1);
}
```

- [ ] **Step 9.2: Implement dispatch**

Resolve `--input` as cache id or file. Build the `DetectorSet`, refusing `Egress::Network` without the flag. Load or create the key. Generate or load the plan. Apply decisions. Write in place for a cache id, to `--output`, or to stdout.

- [ ] **Step 9.3: Implement key storage in `cache.rs`**

`$TOOLPATH_CONFIG_DIR/redact-keys/<cache-id>`, file `0600`, parent `0700`. A missing key on re-redaction is a hard error, never a silent new key.

**Gate:** `cargo test -p path-cli cmd_redact::` green **and** review clean. **Unblocks Wave 4.**

---

# WAVE 4 - three parallel tracks

## Task 10: Sync integration **[Opus impl / Opus review]** *(needs T9)*

**Load-bearing.** `sync::engine::is_unchanged` (`engine.rs:128`) decides re-derivation from source mtime + size + cache-file existence, and **never inspects the document**. An in-place redaction survives while the session is untouched and is **silently destroyed** the moment the user resumes it and sync re-derives with force - which `path query` triggers implicitly.

**Files:** modify `crates/path-cli/src/sync/engine.rs`, `crates/path-cli/src/cmd_cache.rs`

- [ ] **Step 10.1: Write the regression test for the hazard first**

```rust
#[test]
fn sync_reapplies_redaction_after_source_grows() {
    let cfg = sandbox();
    let session = seed_claude_session(&cfg, "sess-1", &["AKIAIOSFODNN7REALKEY"]);
    run_sync(&cfg);
    run_redact(&cfg, &["-i", "claude-sess-1"]).unwrap();

    append_turn(&session, "another turn with AKIAIOSFODNN7SECONDKEY");
    run_sync(&cfg);

    let doc = read_cached(&cfg, "claude-sess-1");
    assert!(doc.contains("another turn"), "new content must land");
    assert!(!doc.contains("AKIAIOSFODNN7REALKEY"), "redaction must survive re-derive");
    assert!(!doc.contains("AKIAIOSFODNN7SECONDKEY"), "new content must be redacted too");
}

#[test]
fn sync_skips_redacted_doc_when_source_unchanged() { /* ... */ }

#[test]
fn sync_reports_reappeared_skips() { /* ... */ }

#[test]
fn sync_fails_loudly_on_missing_key() {
    // Must never write an un-redacted document over a redacted one.
}

#[test]
fn manifest_without_redaction_field_still_loads() {
    let json = r#"{"claude":{"sess-1":{"cache_id":"claude-sess-1","synced_at":"2026-01-01T00:00:00Z"}}}"#;
    assert!(serde_json::from_str::<Manifest>(json).is_ok());
}
```

- [ ] **Step 10.2: Extend `SyncRecord`**

```rust
pub(crate) struct SyncRecord {
    // ... path, cache_id, modified, size, synced_at ...
    /// Policy to replay after a re-derive. Rule-based only: individual
    /// finding ids cannot be replayed against content that has moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) redaction: Option<toolpath_redact::RedactionPolicy>,
}
```

- [ ] **Step 10.3: Replay in the ingestion loop.** After a successful re-derive, if the record carries a policy: load the key, re-run detection, apply the policy, then write. On a missing key, fail that artifact and tally it.

- [ ] **Step 10.4: Report replay** in the sync summary: `re-redacted 3 documents; 2 previously-skipped findings reappeared`.

- [ ] **Step 10.5:** `p cache rm` drops the stored key alongside the document.

**Gate:** `cargo test -p path-cli sync` green **and** review clean; manually confirm `path query` on a redacted-then-resumed session leaves it redacted.

---

## Task 11: End-to-end integration **[Sonnet impl / Opus review]** *(needs T9)*

All subprocess-based with `.env()` per `Command`, so they parallelise.

**Files:** modify `crates/path-cli/tests/integration.rs`

- [ ] `redact_dry_run_lists_surfaces_with_zero_findings`
- [ ] `redact_plan_apply_round_trip` - dry run to a file, apply it, matches a single-shot run
- [ ] `redact_plan_refuses_mismatched_document`
- [ ] `redact_accept_reject_precedence`
- [ ] `redact_in_place_rewrites_cache_entry`
- [ ] `redact_refuses_network_detector_without_flag`
- [ ] `redact_output_still_validates` - piped through `p validate`
- [ ] `redact_corpus_smoke` (`#[ignore]`) - redact every document in the local cache; no panics, no schema violations

**Gate:** `cargo test -p path-cli --test integration redact_` green **and** review clean.

---

## Task 12: Docs and release wiring **[Haiku impl / Opus review]** *(no dependencies)*

**Files:** create `crates/toolpath-redact/README.md`; modify `CLAUDE.md`, `README.md`, `site/_data/crates.json`, `site/pages/crates.md`, `scripts/release.sh`, `CHANGELOG.md`

- [ ] **Step 12.1:** Crate README - purpose, the `Detector` contract, vendored-ruleset attribution with upstream commit and MIT license; `#![doc = include_str!("../README.md")]` in `lib.rs`.
- [ ] **Step 12.2:** `CLAUDE.md` - repository layout, dependency graph, CLI usage block, per-crate test counts, and a "Things to know" entry covering in-place semantics, plan-then-apply, the `Detector` plug point, and the sync replay. Also add a line for `docs/superpowers/`, which is currently undocumented.
- [ ] **Step 12.3:** `README.md` workspace listing.
- [ ] **Step 12.4:** `site/_data/crates.json` entry and `site/pages/crates.md` dependency diagram.
- [ ] **Step 12.5:** `scripts/release.sh` - `ALL_CRATES` and tier 2.
- [ ] **Step 12.6:** `CHANGELOG.md` new section; bump `path-cli` minor.
- [ ] **Step 12.7:** `cd site && pnpm run build` produces its expected page count.

**Gate:** workspace build, test, and clippy all clean **and** review clean.

---

## Done criteria

- [ ] Every task's tests were written before its implementation, are green, and survived adversarial review.
- [ ] Every reviewer change list was applied or rebutted once with a reason; nothing was silently dropped.
- [ ] `cargo test -p toolpath-redact` needs no temp dir, no env var, and no lock.
- [ ] No new code calls `std::env::set_var` in a unit test or `fuzzy::set_picker_override` anywhere.
- [ ] No comment restates what the code does; every comment explains why, records a hazard, or cites an external contract.
- [ ] `path p redact --input <ref> --dry-run` lists **every** surface the map names, including zero-finding ones, and exits `1` when findings exist.
- [ ] A plan can be decided by predicate, by picker, or by hand-editing JSON, and applied to produce exactly the edits it describes.
- [ ] All five transforms are selectable globally and per rule; `marker` is the default.
- [ ] `--input <cache-id>` rewrites in place; no second file.
- [ ] Resuming a redacted session and re-syncing leaves it redacted, with new turns redacted too.
- [ ] Redacting a document with no findings is byte-identical to its input; redacting twice is byte-identical to redacting once.
- [ ] Redacted output validates against the base schema and the `agent-coding-session` kind schema.

---

## Deliberately out of scope

- `path share` gains no scan, warning, or automatic redaction.
- No live credential verification, in any detector, by default.
- Pre-tool-use / harness-time redaction. The `Detector` contract takes strings plus context specifically so this stays possible; nothing here implements it.
- `Graph` documents are handled one `Path` at a time.
