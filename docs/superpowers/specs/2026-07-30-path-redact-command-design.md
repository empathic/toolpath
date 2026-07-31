# `path p redact` - redacting secrets from a generated document

**Status:** Design proposal
**Date:** 2026-07-30

## Goal

Remove credentials from a toolpath document that has already been generated.

```
path p redact --input claude-abc123 --output clean.json
```

Agent sessions carry secrets that arrived as ordinary work content: a `.env`
read into a tool result, a connection string echoed by a command, a token pasted
into a prompt. A census of 95 local Claude Code sessions found credential-shaped
strings in 18 of them, spread across 21 distinct JSON paths. Sharing such a
document publishes them.

## Why this is a post-generation pass

Three other places were considered and rejected.

**At write time, via harness hooks.** Not possible. Claude Code's `PreToolUse`
`updatedInput` changes what executes, and `PostToolUse` `updatedToolOutput`
changes what the model sees next, but neither changes what is persisted. A
controlled experiment (`~/Workspace/scripts/claude-hook-redaction-experiment.sh`)
confirmed both canaries survive to disk: the assistant record keeps the
pre-rewrite command, and the hook's replacement output is *appended* as an extra
record rather than substituted. Hooks can block a call outright; they cannot
scrub one that runs.

**At derive time, inside `toolpath_convo::derive_path`.** Wrong layer. All seven
conversation providers and every round-trip fidelity test run through that
function, and those tests assert exact text equality against the *original*
conversation (`crates/path-cli/tests/roundtrip.rs:89`, `cross_harness_matrix.rs`).
A redactor there breaks them by construction, and it would make the local cache
lossy, which `path resume` depends on it not being.

**At egress, inside `cmd_export::run_pathbase_inner`.** This was the earlier
proposal. It is the only unbypassable point, which is its appeal, but it couples
redaction to one destination and makes it implicit. A document is a document;
whatever is unsafe to upload is also unsafe to paste into an issue, mail to a
colleague, or commit. Redaction belongs to the document, not to one of its exits.

A post-generation `Path -> Path` pass is explicit, destination-agnostic,
composable in a pipeline, and testable in isolation.

## Non-goals

- **`path share` does not scan, warn, or redact.** Redaction is a step the user
  runs. The consequence is accepted deliberately: an un-redacted upload is one
  forgotten command away. Revisit if that turns out to bite.
- **No live credential verification.** Validating a candidate against its issuing
  provider sends secret material off the machine, including false positives that
  may be someone else's real credential. That is the failure this tool exists to
  prevent.
- **Not a history scrubber.** Redacting a shared copy does not un-leak a
  credential that was already exposed. Rotation is still mandatory.

## The model

`redact` is an endomorphism over a document:

```rust
pub fn redact(path: &mut Path, cfg: &RedactConfig) -> RedactReport;
```

It is deliberately **not** implemented as `extract_conversation` -> redact the IR
-> `derive_path`. That round-trip is lossy, and a redactor whose contract is
"change only what I redact" cannot be built on a lossy transform.

The proof is concrete. `toolpath_convo::derive` writes `extra["edits"]`, the raw
MultiEdit old/new array, at `derive.rs:607`. `extract.rs` never reads that key,
and `FileMutation` has no field to hold it. More generally there is **no `extra`
escape hatch on any toolpath-convo IR type**: `Turn` and `FileMutation` are
closed structs, so every provider-specific key outside their typed fields is
dropped on the way through. Round-tripping a document to redact it would silently
delete data that was never a secret.

So the pass walks and mutates the `Path` in place, leaving untouched everything
it does not replace.

## Where the code lives

A new satellite crate, **`toolpath-redact`**, depending on `toolpath` (types) and
`toolpath-convo` (the conversation field map).

The repo's satellite crates each do one thing to a `Path`: `toolpath-dot` renders
it, `toolpath-md` renders it, `toolpath-redact` transforms it. That is the
existing shape.

It does not go in `toolpath` core, which everything depends on and which should
not acquire a regex engine and a vendored ruleset. It does not go in
`toolpath-convo` either, whose dependency list today is `serde`, `chrono`,
`similar`, `thiserror`. Adding detection machinery there would push it onto every
consumer of the conversation IR, including the seven provider crates that have no
use for it.

Cost, per the checklist in `CLAUDE.md`: a new crate means updates to the
workspace `members` and `[workspace.dependencies]`, `site/_data/crates.json`,
`site/pages/crates.md`, `README.md`, the `ALL_CRATES` array and publish tier in
`scripts/release.sh`, and a crate README wired into `lib.rs`. Tier 2, alongside
`toolpath-dot` and `toolpath-md`.

## The field map

This is the part that makes a schema-aware redactor better than a blind string
walk, and it is why the conversation knowledge matters. The pass dispatches on
`structural.change_type` and treats each field as what it actually is.

| Change type | Field | Treatment |
|---|---|---|
| `conversation.append` | `extra["text"]`, `extra["thinking"]` | prose; span replacement |
| | `extra["tool_uses"][].input` | provider JSON; recurse to string leaves |
| | `extra["tool_uses"][].result.content` | tool output; prose treatment |
| | `extra["delegations"][]` | recursive sub-conversation; re-enter the map |
| | `extra["environment"]` | structured; `working_dir` only |
| `file.write` | `ArtifactChange.raw` | unified diff; line-count-preserving only |
| | `extra["before"]`, `extra["after"]` | whole-file content; span replacement |
| | `extra["edits"][]` | old/new pairs; both sides |
| `conversation.event` | `extra` (whole) | opaque provider payload; blind leaf walk. **The one place a blind walk is correct.** |
| any | artifact key (map key) | file path or URI; redact URI userinfo only |
| `path.meta` | `extra["vcs_remote"]` | URI; redact userinfo only |
| `path.base` | `uri` | URI; redact userinfo only |

Two rules fall out of this table.

**Redact the capture group, not the match.** A connection string becomes
`postgres://svc_user:[REDACTED:db-uri-password:a3c829]@db.internal:5432/prod`,
not an opaque blob. The PEM armor lines survive, the body goes. `Bearer ` stays,
the token goes. This is what keeps a redacted document readable and resumable.

**Diffs are line-count-preserving.** A hunk header `@@ -a,b +c,d @@` declares
line counts, so replacement inside a line is safe and anything that adds or
removes lines is not. A 25-line PEM block cannot collapse to one marker. Parse
with `diffy` rather than regexing raw patch text.

### Why the map is wider than tool input and output

Tool I/O is the biggest surface but not the whole one. Classifying the 120
high-confidence hits from the local census by what kind of field held them:

| Surface | Hits | Share |
|---|---:|---:|
| Tool results and output | 60 | 50% |
| Tool inputs | 19 | 16% |
| Plain prompt and message text | 36 | 30% |
| Diff lines | 3 | 2% |
| Base64 attachment blobs | 2 | 2% |

Scanning only tool inputs and outputs would cover two thirds of the surface and
miss the third that is a human pasting a credential into a prompt, or an
assistant quoting one back in prose. In the raw JSONL that third lives at
`$.content`, `$.message.content`, `$.message.content[].text` and `$.lastPrompt`,
and in a derived document it lands in `extra["text"]`. It has to be in the map.

The diff row is small in count and large in consequence: those hits are in
`old_string`/`oldString`/`originalFile`, which capture pre-edit content. Using
the Edit tool to *remove* a secret from a file writes that secret permanently
into the session log. The remediation creates the durable record, so the field
that records what was removed is exactly the field that must be redacted.

For documents whose `meta.kind` is not `agent-coding-session`, the map degrades
to the generic rows (artifact keys, `base.uri`, `meta.extra`) plus a blind leaf
walk of `structural.extra`. Git and GitHub derived paths get correct, if less
precise, treatment for free.

## The redactor abstraction

The pass owns traversal, the field map, transformation, and the audit record.
**What it does not own is detection.** That is a plug point, so the same command
can drive a built-in regex engine, an external scanner, or a semantic matcher
that does not exist yet.

The split matters because detection is where the field moves fastest and where
the trade-offs are least settled (see the base rates: best open-source precision
is 0.46). Traversal and provenance are stable; detectors are not. Pinning the
former and swapping the latter is the whole point.

### The contract

Detectors take **strings plus context**, never toolpath types. That is a
deliberate constraint, and it buys two things: detectors are testable without
constructing a `Path`, and the same implementations can later be driven from a
pre-tool-use hook path without touching this layer. Keeping harness-time
redaction possible is a non-goal for now, but the seam is placed so it stays
possible.

```rust
/// What the host hands a detector: one field's full value, plus enough
/// context to judge it.
pub struct Candidate<'a> {
    /// The string to examine. Never a fragment - always the whole field.
    pub text: &'a str,
    /// What this field holds. A diff is scanned line-wise, a URI is parsed,
    /// opaque JSON is walked. The shape tells a detector which to do.
    pub shape: FieldShape,
    /// RFC 6901 pointer relative to the step. Passed through verbatim to the
    /// audit record; the detector never has to construct one.
    pub at: &'a str,
    pub ctx: Context<'a>,
}

pub enum FieldShape {
    Prose,        // extra["text"], extra["thinking"]
    ToolInput,    // tool_uses[].input
    ToolOutput,   // tool_uses[].result.content
    UnifiedDiff,  // ArtifactChange.raw
    FileContent,  // extra["before"], extra["after"]
    Uri,          // vcs_remote, base.uri, artifact keys
    OpaqueJson,   // conversation.event.extra
}

pub struct Context<'a> {
    pub change_type: &'a str,        // "conversation.append" | "file.write" | ...
    pub tool_name: Option<&'a str>,  // "Bash", "Read", "Edit", ...
    pub actor: &'a str,              // "agent:claude-opus-5" | "human:alex"
    pub kind: Option<&'a str>,       // path.meta.kind
}

/// What a detector hands back.
pub struct Finding {
    /// Byte range into `Candidate.text`. Must land on char boundaries; the
    /// host validates and rejects a detector that returns otherwise.
    pub span: Range<usize>,
    /// Stable id. Goes verbatim into the audit record, so it is part of the
    /// document's contract, not an implementation detail.
    pub rule: String,
    /// 0.0..=1.0. The host compares against one threshold.
    pub score: f32,
    /// Which detector produced this. Recorded for provenance when more than
    /// one is configured.
    pub detector: &'static str,
}

pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;

    fn detect(&self, c: &Candidate<'_>) -> Result<Vec<Finding>>;

    /// Cheap gate so the host can skip the call entirely. The built-in
    /// detector implements this as an aho-corasick keyword scan.
    fn prefilter(&self, _text: &str) -> bool { true }

    /// Whether this detector performs network I/O. The host **refuses to run
    /// a networked detector** unless explicitly allowed, because validating a
    /// candidate against its issuing provider sends secret material off the
    /// machine - the exact failure this tool exists to prevent.
    fn egress(&self) -> Egress { Egress::LocalOnly }
}
```

Composition is a set, so an internal engine and an external scanner can run
together and their findings merge through the same overlap resolution:

```rust
pub struct DetectorSet(Vec<Box<dyn Detector>>);
```

This mirrors patterns the repo already uses: `ConversationProjector` with
`AnyProjector` for type erasure, the `ArtifactSource` trait with one impl per
provider, and `cmd_resume::ExecStrategy` for injectable execution. Tests get a
`FixedDetector` that returns canned spans, the same way `RecordingExec` stands
in for a real harness.

### Transformation is also pluggable, with a safe default

An external tool that wants to supply the replacement text as well as find the
span implements the second trait. Most will not.

```rust
pub trait Transformer: Send + Sync {
    fn id(&self) -> &'static str;
    fn replace(&self, f: &Finding, value: &str, fp: &Fingerprint) -> String;
}
```

Default is `MarkerTransformer`, which emits `[REDACTED:<rule>:<fp>]`. The host
always computes the fingerprint and always writes the audit record, whatever the
transformer returns - those are policy, not plugin territory.

### Bundled implementations

| Detector | `id()` | How it runs |
|---|---|---|
| Built-in regex engine | `internal` | In-process. Vendored `gitleaks.toml`, aho-corasick prefilter, checksum validation. The default. |
| External binary | `exec:<name>` | Subprocess, JSON in and JSON out (below). Covers gitleaks, kingfisher, and anything else with a CLI. |
| Rust crate | `keyhog` | In-process behind a feature flag. `keyhog-scanner` is the one crates.io-consumable engine. |
| Semantic | `semantic` | Not in v1. Slots in as another impl with no change to this layer. |
| Manual map | `manual` | Reads an explicit list of values or pointers. No inference. |

The subprocess protocol keeps the boundary honest: one JSON object per
candidate on stdin, one array of findings on stdout.

```json
{"text": "...", "shape": "tool_output", "at": "/change/...", "ctx": {"tool_name": "Bash"}}
```
```json
[{"span": [42, 62], "rule": "aws-access-key-id", "score": 0.99}]
```

A detector that returns overlapping, out-of-order, or non-char-boundary spans is
normalised by the host rather than trusted, because a third-party scanner is not
part of this codebase's test suite.

## Dry run, and the plan file

The command is plan-then-apply, like `terraform plan` / `apply`. A dry run
produces a **redaction plan**: a reviewable, editable JSON artifact that is also
the exact input to the apply step. Nothing is redacted without one, even when it
is generated implicitly.

### The dry run surfaces everything it looked at, not just what it found

This is the part that matters most and the part most tools get wrong. A report
listing only findings answers "what did you find" but not "**what did you even
look at**" - and the second question is the one a user needs in order to trust
the first. A surface with zero findings is information: it says the pass reached
that field and the detectors were silent, which is different from the pass never
having visited it.

So the plan carries two lists. `surfaces` is every field the map named, whether
or not anything fired. `findings` is what fired.

```json
{
  "v": 1,
  "document": "claude-abc123",
  "generated": "2026-07-30T18:04:11Z",
  "detectors": ["internal"],
  "defaults": { "transform": "marker", "threshold": 0.8 },
  "surfaces": [
    { "step": "turn-0f3a", "at": "/change/claude:~1~1sess-abc/structural/extra/text",
      "shape": "prose", "bytes": 412, "findings": 0 },
    { "step": "turn-0f3a", "at": "/change/claude:~1~1sess-abc/structural/extra/tool_uses/0/result/content",
      "shape": "tool_output", "bytes": 4211, "findings": 2 },
    { "step": "turn-9c21", "at": "/change/src~1config.rs/raw",
      "shape": "unified_diff", "bytes": 880, "findings": 1 }
  ],
  "findings": [
    { "id": "f01", "step": "turn-0f3a",
      "at": "/change/claude:~1~1sess-abc/structural/extra/tool_uses/0/result/content",
      "rule": "aws-access-key-id", "span": [1042, 1062], "score": 0.99,
      "detector": "internal", "shape": "tool_output",
      "context": "export AWS_ACCESS_KEY_ID=<aws-access-key-id> && aws s3 ls",
      "action": "redact", "transform": "marker" }
  ]
}
```

`context` elides the match by default - it shows the surrounding line with the
value replaced by its rule name, and **not** the value's length. A plan file is
a local artifact but it is exactly the kind of thing that gets pasted into a
ticket, so it does not carry material by default. `--reveal` includes the real
values for cases where you genuinely cannot decide without seeing them; it
writes `0600` and prints a warning.

### Deciding: bulk, then individual

Every finding carries an `action` (`redact` or `skip`) and an optional
`transform` override. Three ways to set them, and they compose in this order:

**1. Bulk, by predicate.** Repeatable, evaluated in order, last match wins:

```
--accept 'rule=aws-access-key-id'      # every AWS key
--accept 'score>=0.95'                 # everything the detector is sure about
--accept 'shape=unified_diff'          # everything in a diff
--reject 'rule=generic-assignment'     # the noisy heuristic, wholesale
--accept 'step=turn-0f3a'              # one turn entirely
--mode-for 'rule=us-social-security-number:mask'
```

Predicate fields are `rule`, `shape`, `step`, `detector`, `at` (prefix match) and
`score` (with `>=`, `>`, `<=`, `<`, `=`). No expression language beyond that - a
DSL here would be a liability.

**2. Individual, interactively.** `--interactive` opens the picker the repo
already ships (`fuzzy::pick` with `multi: true`, external `fzf` or the embedded
skim fallback), one row per finding, TAB to toggle, preview pane showing the
elided context. This is the same UX as `p import`'s session picker, so it needs
no new interaction vocabulary.

```
  rule                  score  step        context
> aws-access-key-id     0.99   turn-0f3a   export AWS_ACCESS_KEY_ID=<…> && aws s3 ls
  db-uri-password       0.97   turn-9c21   DATABASE_URL=postgres://svc:<…>@db.internal
  generic-assignment    0.42   turn-0f3a   deploy_token = "<…>"   # ops pasted this
```

**3. Individual, by hand.** The plan is JSON. Edit `action` and `transform`,
save, apply. This is the escape hatch that makes the other two optional, and it
is why the plan is a file rather than an interactive-only flow.

### Apply

```
path p redact --input claude-abc123 --plan plan.json --output clean.json
```

Apply re-derives the surfaces from the input document and **verifies the plan
still matches** before touching anything: same document id, same step ids, same
spans at the recorded offsets. A plan generated against a different document, or
against one that has since changed, is refused rather than applied approximately.

Without `--plan`, apply generates one internally from the flags and runs it, so
the one-liner still works:

```
path p redact --input claude-abc123 --accept 'score>=0.9' --output clean.json
```

## Selection modes

What gets replaced is a separate axis from what gets found.

```rust
pub enum Selection {
    /// Run the detector set; replace at or above the threshold, report below it.
    Auto { threshold: f32 },
    /// Detect nothing. Replace only what a previous pass already recorded in
    /// meta.redaction as flagged.
    Flagged,
    /// Replace exactly what an explicit mapping names. No inference.
    Manual(ManualMap),
}
```

`Auto` is the default. `Flagged` exists because the honest answer to a noisy
heuristic is to surface it and let a human decide: run once to get the report,
review, then run again to apply what you accepted. `Manual` covers the case the
detectors will always miss - a bare unstructured password that only the user
knows is a password - and is the escape hatch that keeps the tool useful when
detection fails.

Future modes slot in here without touching the traversal or the record.

## Detection in the built-in redactor

Everything below describes the `internal` detector specifically. Another
implementation is free to work differently; the contract above is what the host
relies on.

One scored pass, not two tiers. High-confidence formats clear the threshold
alone; a bare high-entropy value clears it only with a nearby hotword.

1. **Keyword prefilter.** `aho-corasick` over the rules' literal keywords, run
   before any regex. 221 of gitleaks' 222 rules carry keywords, which is what
   makes a large ruleset affordable per string leaf.
2. **Structured rules.** Vendor `gitleaks.toml` (MIT, 222 rules). Verified: zero
   lookarounds, zero backreferences, one named group, so the entire ruleset
   compiles under the pure-Rust `regex` crate with no `fancy-regex` fallback.
   Order alternations longest-first, because Rust `regex` is leftmost-first with
   no leftmost-longest option.
3. **Entropy as a filter, never a detector.** Gate an already-structurally-matched
   candidate. A 47-character random credential and the phrase
   `ThisIsAReallyLongString` score within 0.03 bits of each other, so entropy
   alone cannot separate them.
4. **Hotword proximity adjustment.** A secret-ish name within ~50 characters
   (Macie's default `maximumMatchDistance`) raises the score. This is the
   principled form of the `NAME=value` heuristic, which measured 55% suppressible
   noise on the local corpus and is not safe to auto-replace on its own.
5. **Checksum validation** where the format defines one. GitHub PATs are CRC32 ->
   base62 -> last 6 characters. Offline, free, and it kills the dummy-key false
   positive that entropy cannot.

Findings at or above the replace threshold are replaced. Findings below it are
**reported but not touched**, and `--include-heuristic` opts into replacing them.

Overlap resolution, adopted from Presidio: identical spans, higher score wins;
nested spans, the container wins regardless of score; adjacent same-type spans
separated only by whitespace, merge. Tie-break on rule id so output is
reproducible.

## Transforms

Default is a typed marker:

```
[REDACTED:aws-access-key-id:a3c829]
```

The rule id says what kind of thing was there, which is the part with analytical
value. The fingerprint is a keyed hash, so the same secret maps to the same token
throughout the document and a reader can still see that one key recurs across
twelve steps.

**Key derivation.** Per-document random salt by default: within-document
coreference works, cross-document correlation does not. An optional `--key-file`
gives stable fingerprints across documents for anyone who wants rotation triage
across sessions. Never a bare unkeyed hash; a hash of a low-entropy secret is a
dictionary attack away from the secret, and the EDPB pseudonymisation guidance is
explicit that the transformation must involve a secret with sufficient entropy.

### The full set

Five transforms ship. `marker` is the default and the recommendation; the rest
are available because the right answer is sometimes situational, and refusing to
offer them just pushes people to hand-edit documents, which is worse.

| id | output | length | what it leaks |
|---|---|---|---|
| `marker` | `[REDACTED:aws-access-key-id:a3c829]` | no | the *type*, plus a correlation handle. Both by design. |
| `remove` | *(empty)* | no | nothing. Also destroys the surrounding structure's readability. |
| `hash` | `a3c829` | no | a correlation handle only. No type, so the reader loses the "what was here". |
| `mask` | `████████████████████` | **yes** | the exact length. |
| `partial` | `AKIA…MPLE` | yes | the provider, the format, and 8 characters of material. |

The bottom two rows are real disclosures, not stylistic preferences. A preserved
prefix identifies the provider, which is targeting information, and pins the
format and therefore the exact brute-force search space. A length-preserving mask
publishes the length, which for a fixed-format credential narrows the space
further. They are offered, they are documented, and they are not the default.

Transform is settable globally and **per rule**, which is what makes the choice
useful rather than a blunt instrument:

```
--mode marker --mode-for us-social-security-number=mask
```

That reflects the actual situation: a credential wants a typed marker so the
reader knows a key was rotated; a PII field often wants a mask so the shape of
the record survives.

## Idempotence

The marker grammar is allowlisted before any rule runs, so a second pass finds
nothing and re-running produces byte-identical output. This is what makes
re-sharing an already-redacted document safe, and it is cheap to get right up
front and painful to retrofit.

Invariant to test: `redact(redact(d)) == redact(d)`.

## Signatures

`meta.signatures[]` on a step, path, or graph covers content the pass is about to
change, so redaction invalidates any signature over the redacted scope. v1
refuses to redact a signed document unless `--drop-signatures` is passed, which
strips them and records the fact in the report. Silently leaving a broken
signature is not an option.

## The audit record

The pass records what it did, at the turn where it did it, without recording the
values. Toolpath's entire job is recording what happened to an artifact, so a
redaction that leaves no trace is off-format.

**No new types.** The record rides the existing structs. Verified against the
schema:

- `stepMeta` is `additionalProperties: true`, and `StepMeta.extra` is a
  `#[serde(flatten)] HashMap<String, Value>`, so `Step.meta.extra["redaction"]`
  is legal and needs no schema change. `Step.meta` is `Option<StepMeta>`, so the
  pass creates it where absent.
- `pathMeta` is likewise `additionalProperties: true`, so the document-level
  rollup goes at `path.meta.extra["redaction"]`.
- `step` itself is `additionalProperties: false`, so the record cannot sit as a
  sibling of `step`/`change`/`meta`. Inside `meta` is the only legal home.

Per step, on the turn where the secret appeared:

```json
{
  "step": { "id": "turn-0f3a", "actor": "agent:claude-opus-5", "...": "..." },
  "change": { "...": "..." },
  "meta": {
    "redaction": {
      "v": 1,
      "findings": [
        {
          "rule": "aws-access-key-id",
          "at": "/change/claude:~1~1sess-abc/structural/tool_uses/0/result/content",
          "n": 2,
          "fp": "a3c829",
          "op": "marker"
        },
        {
          "rule": "db-uri-password",
          "at": "/change/src~1config.rs/raw",
          "n": 1,
          "fp": "7b1e04",
          "op": "marker"
        }
      ]
    }
  }
}
```

`at` is an RFC 6901 JSON Pointer relative to the step object. Note the escaping:
artifact keys are URLs and file paths containing `/`, which becomes `~1` (and
`~` becomes `~0`), so `claude://sess-abc` addresses as `claude:~1~1sess-abc`.
That is fiddly enough to deserve a helper and a test of its own.

`fp` is the same keyed fingerprint that appears in the marker, so a reader can
correlate: this step's finding and that step's finding are the same credential,
without either revealing it.

The document-level rollup answers "what happened overall" without walking every
step:

```json
{
  "path": { "id": "path-claude-code-0f3a2b71", "head": "turn-9c21" },
  "meta": {
    "title": "Claude session: 0f3a2b71",
    "kind": "https://toolpath.net/kinds/agent-coding-session/v1.1.0",
    "redaction": {
      "v": 1,
      "at": "2026-07-30T18:04:11Z",
      "tool": "toolpath-redact/0.1.0",
      "ruleset": "gitleaks@8.28.0",
      "mode": "marker",
      "steps_touched": 4,
      "replaced": { "aws-access-key-id": 2, "db-uri-password": 1 },
      "flagged": { "generic-assignment": 4 },
      "signatures_dropped": 0
    }
  }
}
```

### What the record must not contain

- **The value**, in any form.
- **Any substring of it.** A preserved prefix leaks the provider and the format,
  hence the search space.
- **The original length.** Sentry's `_meta` records `len`, and that is a mistake
  to copy here: for a fixed-format credential, length narrows the format space,
  and combined with the rule id it is close to redundant with a prefix. The
  marker's own length reveals nothing about the original.

The record holds a *type*, a *location*, a *count*, and an opaque *handle*. That
is the honest account of what a reader is not seeing.

### Consequences to handle

- **The record must be allowlisted from scanning**, alongside the marker grammar.
  A hex fingerprint in `meta.extra` would otherwise trip an entropy rule on the
  next pass and the document would never converge.
- **Re-running merges, it does not append.** A second pass over an already
  redacted document finds nothing new and leaves the existing record untouched,
  which is what makes `redact(redact(d)) == redact(d)` hold at the byte level.
- **`extract_conversation` drops it**, because `Turn` has no field for step meta.
  So a `path resume` of a redacted document shows the markers in the text but not
  the structured record. That is acceptable: the marker carries the signal where
  a reader will actually be looking. Worth revisiting if the IR ever grows an
  escape hatch.
- **It changes the signed scope**, which is already covered by the
  `--drop-signatures` rule above.

## CLI

```
path p redact --input <ref> [--output <path>]

  detection      --detector internal|keyhog|exec:<path>   (repeatable)
                 --threshold <0.0-1.0>                    (default 0.8)
                 --allow-network-detectors                (off by default)

  plan           --dry-run                                emit a plan, change nothing
                 --plan <file>                            apply an existing plan
                 --reveal                                 include real values in the plan (0600)

  decide         --accept <predicate>                     repeatable, last match wins
                 --reject <predicate>
                 --interactive                            picker, TAB to toggle
                 --mode-for <predicate>:<transform>

  transform      --mode marker|remove|hash|mask|partial   (default marker)
                 --key-file <path>

  output         --output <path>  --json
                 --drop-signatures
```

`--detector` is repeatable, so an internal sweep and an external scanner can run
together:

```bash
path p redact --input claude-abc123 \
  --detector internal --detector exec:/usr/local/bin/gitleaks
```

The two-pass review flow that `--select flagged` exists for:

```bash
# 1. see what a noisy heuristic thinks, change nothing
path p redact --input claude-abc123 --report-only --json > findings.json

# 2. after review, apply only what was accepted
path p redact --input claude-abc123 --select flagged --output clean.json
```

`<ref>` is a cache id or a file path, matching `p render` and `p validate`.

**In place is the point.** A cache id redacts the cached document where it sits.
There is no second file.

The earlier design wrote a redacted copy elsewhere and left the cache untouched,
on the reasoning that the cache is a faithful archive. That was wrong, and the
reason it was wrong is the same reason `share` has no safety net: two documents
means remembering which one to send. Every downstream verb - `path resume`,
`path share`, `p export`, `p render` - resolves a cache id, so a redacted copy
sitting beside the original protects none of them. Redacting in place means once
you have redacted, everything downstream is redacted, without anyone having to
remember anything.

What this gives up is the cache as an archive of original content. That is an
acceptable trade because **the cache was never the source of truth** - the
harness session log is, and `p cache sync` re-derives from it. The real cost is
narrower and worth stating plainly: once the source session ages out (30 days by
default, `cleanupPeriodDays`), the redaction is permanent and the original text
is gone.

`--output` still exists and still writes elsewhere; it is how you redact a
standalone `.json` file, or keep a copy. With a file input and no `--output`,
output goes to stdout so it composes:

```bash
path p redact --input doc.json | path p export pathbase --input -
```

In-place writes are temp-plus-rename at `0600`, matching `cache::write_cached`,
so an interrupted redaction never leaves a half-written document.

## The sync collision, and how it is handled

In-place redaction has one serious interaction, and it is not obvious.

`sync::engine::is_unchanged` (`engine.rs:128`) gates re-derivation purely on the
source artifact's mtime and size plus the cache file existing. **It never
inspects the document.** So:

- Redact in place, source session untouched → the next sync sees matching stamps
  and skips. The redaction survives.
- Resume that session and add a single turn → the source mtime and size change →
  sync re-derives **with force** → the redaction is silently destroyed, and the
  newly-appended turns arrive un-redacted too.

`path query` auto-syncs implicitly, so this can happen without the user ever
typing `sync`. A design that ignored it would quietly un-redact documents.

**The fix: sync knows about redaction and re-applies it.**

`SyncRecord` gains an optional field. It is additive, and every existing field
already carries `#[serde(default, skip_serializing_if)]`, so old manifests load
unchanged:

```rust
pub(crate) struct SyncRecord {
    // … path, cache_id, modified, size, synced_at …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) redaction: Option<RedactionPolicy>,
}

pub(crate) struct RedactionPolicy {
    pub detectors: Vec<String>,
    pub threshold: f32,
    pub mode: Transform,
    pub mode_for: Vec<(String, Transform)>,
    pub accept: Vec<String>,   // the predicates, verbatim
    pub reject: Vec<String>,
    pub key_id: String,        // which stored key produced the fingerprints
}
```

When sync re-derives an artifact whose record carries a policy, it re-runs
redaction with that policy before writing the cache. The document stays current
*and* stays redacted.

Two honest limitations of replay:

- **Only rule-based decisions replay.** A predicate re-evaluates against new
  content correctly. An individually hand-picked finding cannot - its id and span
  refer to content that may have moved. So anything the user individually
  *skipped* comes back on re-derive. That is fail-closed, which is the right
  direction, but it is surprising, so sync reports it: `re-redacted 3 documents;
  2 previously-skipped findings reappeared`.
- **The fingerprint key must persist.** A per-run random salt would give every
  re-derive different markers and churn the document on every sync. The key is
  stored once per document under `$TOOLPATH_CONFIG_DIR/redact-keys/` at `0600`
  and referenced by `key_id`. This supersedes the "per-document random salt"
  wording above: the salt is per-document and *persisted*, not per-run.

`p cache rm` drops the key alongside the document. A record whose key is missing
fails loudly on re-redaction rather than silently producing new fingerprints.

`--report-only` prints findings and writes no document:

```
$ path p redact --input claude-abc123 --report-only
7 findings in 4 steps

  replaced
    aws-access-key-id      2   conversation.append.tool_uses[].result.content
    db-uri-password        1   file.write.raw
  flagged (not replaced, --include-heuristic to replace)
    generic-assignment     4   conversation.append.text
```

Exit 0 when clean, 0 with findings after a successful redaction, non-zero only on
error. `--report-only` exits 1 when findings exist, so it works in a pre-share
check a user can wire up themselves.

## Testing

Mirrors the repo's existing style: unit tests alongside the code, integration
tests in `crates/path-cli/tests/`.

- **Field map coverage.** One fixture document per change type, asserting the
  right fields are visited and the wrong ones are not.
- **Detector contract.** A `FixedDetector` returning canned spans drives every
  traversal and transform test, so those never depend on regex behaviour. Its
  counterpart is a `HostileDetector` returning overlapping, reversed,
  out-of-range and mid-codepoint spans, asserting the host normalises rather
  than panics or corrupts.
- **Candidate shape dispatch.** Each `FieldShape` reaches the detector with the
  right value and pointer, verified by a recording detector that captures every
  `Candidate` it is handed.
- **Egress refusal.** A detector declaring `Egress::Network` errors without
  `--allow-network-detectors`.
- **Selection modes.** `Auto` replaces above threshold and reports below it;
  `Flagged` replaces exactly the previously recorded flags and detects nothing;
  `Manual` replaces exactly what the map names and nothing else.
- **Non-destruction.** For a document with no secrets, `redact(d) == d` byte for
  byte. This is the single most important test: the pass must not perturb
  anything it is not redacting.
- **Lossiness guard.** A document carrying `extra["edits"]` and provider-specific
  keys survives redaction with those keys intact. Directly guards against anyone
  later "simplifying" the implementation into an extract/derive round-trip.
- **Idempotence.** `redact(redact(d)) == redact(d)` across all modes.
- **Diff integrity.** A redacted `file.write` raw diff still parses, and its hunk
  headers still match its line counts.
- **Determinism.** Same input plus same key gives the same output, including
  fingerprints, across runs and across HashMap iteration orders.
- **Signature refusal.** A signed document errors without `--drop-signatures`.
- **Round-trip untouched.** The existing fidelity suites still pass, confirming
  the pass sits outside `derive_path` and the projectors.

Corpus check: run against the local cache and confirm the known base rates from
the census reappear.

## Decisions

1. **Post-generation, not at egress.** Redaction belongs to the document, not to
   one of its exits. Accepted cost: `path share` has no safety net.
2. **A new crate, not a module in `toolpath-convo`.** Keeps a regex engine and a
   vendored ruleset off the seven provider crates.
3. **Mutate the `Path` in place; never round-trip through the IR.** The IR is
   lossy by construction and a redactor cannot be built on a lossy transform.
4. **Typed markers with keyed fingerprints; no masking or partial redaction.**
   Preserves what has analytical value and leaks nothing.
5. **Heuristic findings are reported, not replaced, by default.** 55% measured
   noise makes auto-replacement worse than useless.
6. **Redact the cached document in place.** Every downstream verb resolves a
   cache id, so a redacted copy beside the original protects none of them. The
   cache stops being an archive of original content; the harness session log
   already was one. Reversed from the first draft, which wrote a copy elsewhere.
7. **Detection is a plug point; traversal, transform and provenance are not.**
   Detection is where the field moves fastest and where precision is worst.
   Pinning the stable parts and swapping the volatile one is the point of the
   abstraction.
8. **Detectors take strings plus context, never toolpath types.** Keeps them
   testable in isolation, and keeps a future pre-tool-use path open without
   rework at this layer.
9. **Networked detectors are refused unless explicitly allowed.** Validating a
   candidate against its issuing provider is the failure this tool exists to
   prevent, so it cannot be reachable by accident.
10. **Finding what to redact and deciding what to replace are separate axes.**
    `--select flagged` makes "report, review, then apply" a first-class flow
    rather than a workaround for a noisy detector.
11. **Sync re-applies redaction rather than clobbering it.** In-place redaction
    without this is a silent un-redaction the moment the user resumes a session,
    triggered implicitly by `path query`. The policy is persisted in the manifest
    record; the fingerprint key is persisted per document so replay does not
    churn markers.

## Future, not in v1

- A `--policy` file for per-repo rules and allowlists.
- Redacting `Graph` documents element-wise (v1 handles the single-`Path` case
  that `share` and `resume` produce).
- Reversible pseudonymization with a stored key, for teams that need to
  re-identify inside a trusted boundary.
- Extending the field map as new `meta.kind` values are defined.
