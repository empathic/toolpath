# Toolpath FAQ

Frequently asked questions about the Toolpath format, its design decisions, and
how to use it.

---

## General

### What is Toolpath?

Toolpath is a format for recording artifact transformation provenance.  It
tracks **who** changed **what**, **why**, what they tried that didn't work, and
how to verify all of it.  Think "git blame, but for everything that happens to
code — including the stuff git doesn't see."

### When should I use Toolpath?

Toolpath is useful when you want to:

- Record the full provenance of a code change across multiple actors (humans,
  AI agents, formatters, linters)
- Preserve abandoned approaches alongside the successful path
- Attach structured intent, external references, and signatures to changes
- Track changes at finer granularity than VCS commits

### When should I NOT use Toolpath?

Toolpath is not the right tool for:

- **Real-time collaboration** — Toolpath is for provenance, not live editing
  (use CRDTs or OT for that)
- **Replacing your VCS** — Toolpath complements git/jj/hg, it doesn't replace
  them
- **Large binary artifacts** — The diff-based change model assumes text-like
  content; binary blobs don't produce meaningful unified diffs

### Can I use Toolpath without a VCS?

Yes.  A path's `base` can use a `toolpath:` URI to branch from another path's
step, creating a pure Toolpath chain with no VCS backing.  You can also use
`file:///` URIs for local-only provenance.

### How does Toolpath compare to W3C PROV?

W3C PROV is a general-purpose provenance data model (entities, activities,
agents).  Toolpath is narrower and more opinionated:

- **PROV** models arbitrary provenance relationships across any domain
- **Toolpath** models artifact transformations specifically, with built-in
  support for diffs, actor types, DAG structure, dead ends, and signatures

If you need general provenance, use PROV.  If you need to track how code (or
code-like artifacts) evolved through multiple actors, Toolpath gives you a
tighter, more useful model out of the box.

### How does Toolpath compare to in-toto or Sigstore?

in-toto and Sigstore focus on **supply chain integrity** — attesting that
specific steps were performed by specific actors in a pipeline.  Toolpath
focuses on **transformation provenance** — recording what happened to artifacts
and why.

They complement each other: you might use Toolpath to record the full history
of a PR, then use Sigstore to attest that the release was built from that
provenance chain.

---

## Format Design

### Why is Document externally tagged?

The `Document` enum uses external tagging: every Toolpath JSON file has exactly
one top-level key — `"Step"`, `"Path"`, or `"Graph"` — that identifies the
document type.

```json
{ "Step":  { "step": {...}, "change": {...} } }
{ "Path":  { "path": {...}, "steps": [...] } }
{ "Graph": { "graph": {...}, "paths": [...] } }
```

### Why use Unified Diff for the `raw` perspective?

Unified Diff (the format produced by `diff -u` and used by git) is:

- Widely understood across tools and ecosystems
- Human-readable
- Well-specified with clear semantics
- Backward-compatible with existing tooling

Future versions may add alternative perspectives (e.g., `span` for byte-range
edits), but `raw` is always Unified Diff.

### How are dead ends detected?

Dead ends are implicit — no explicit marking is required.  A step is a dead end
if it's not an ancestor of `path.head`:

```
active_steps = ancestors(path.head)  // walk parents backwards
dead_ends = all_steps - active_steps
```

Steps don't know their fate.  It's determined by the graph structure relative
to the current head.  This keeps the format simple: you never need to update a
step's metadata when the path evolves.

### How do multi-parent merges work?

Steps have a `parents` array that supports merges:

| Parents | Meaning |
| ------- | ------- |
| `[]` or omitted | Root step (no parents) |
| `["step-001"]` | Single parent (linear history) |
| `["step-A", "step-B"]` | Merge (derived from parallel work) |

Toolpath models the DAG structure; it doesn't prescribe merge strategies or
conflict resolution.

### What identifies an artifact?

Artifacts are identified by URL.  The keys in the `change` object are URLs,
with bare paths as shorthand for files relative to `path.base`:

| Key Format | Interpretation |
|------------|----------------|
| `src/foo.rs` | Relative file path within `path.base` context |
| `file:///abs/path` | Absolute file path |
| `https://...` | Web resource |
| `s3://...` | S3 object |
| `<scheme>://...` | Any URL scheme |

### Why is `meta` always optional?

A minimal document needs only `step` + `change`.  The `meta` object holds
context: intent, refs, actors, signatures.  Making it optional means:

- Simple changes require minimal ceremony
- Streaming steps can be lightweight
- You can add provenance incrementally

### How does Toolpath relate to VCS tools?

Toolpath is VCS-agnostic.  Any VCS commit can become a step.  The `path.base`
URI scheme indicates which VCS (`github:`, `hg:`, `fossil:`, `file:`, etc.).

What Toolpath adds beyond any VCS:

1. **Finer granularity** — multiple steps between VCS commits
2. **Abandoned paths** — VCS tools lose deleted branches; Toolpath preserves
   dead ends
3. **Multi-perspective changes** — raw diff + structural AST ops + semantic
   intent
4. **Actor provenance** — link actors to external identities
5. **Multi-party signatures** — author, reviewer, CI attestation on same
   artifact
6. **Tool-agnostic history** — changes from AI agents, formatters, linters that
   may not create VCS commits

---

## Open Design Questions

These questions are not yet resolved.  They need more thought before the format
stabilizes.

### How should step IDs be generated?

**Options under consideration:**

1. **Content-addressed** — hash of (parent, actor, change, timestamp).  Good
   for deduplication and verification, but you can't know the ID until you've
   finalized the content.
2. **UUID** — random, simple, but opaque.
3. **Hierarchical** — `session-abc/turn-5/step-2`.  Readable but couples to
   session structure.
4. **Sequential** — `step-001`, `step-002` within some scope.

The current examples use sequential IDs for readability.  No formal requirement
yet.

### Who defines structural operation types?

**Options under consideration:**

1. **Central registry** — Toolpath org maintains canonical op types
2. **Namespaced extensions** — `rust.add_method`, `typescript.add_interface`
3. **Schema-per-language** — each language community maintains their own
4. **Emergent** — let tools emit whatever, see what converges

**Current leaning:** Namespaced with a `core` namespace for universal ops
(e.g., `core.replace`, `core.insert`).

### How should the path tree be stored/transmitted?

**Options under consideration:**

1. **Flat list** — tree structure implicit via `parent` refs (current approach)
2. **Nested tree** — explicit hierarchy
3. **Separate index** — steps stored flat, tree index computed/stored separately

The flat list is simpler for append-only logs and streaming.  Querying "all
descendants of step X" requires scanning, but the step counts in practice are
small enough that this hasn't been a problem.

### How should privacy and redaction be handled?

**Scenarios:** Agent reasoning might contain proprietary information.  Human
identity might need anonymization.  Refs might point to internal docs.

**Options under consideration:**

1. **Reference, don't embed** — `meta.refs` stores URIs, not content
2. **Redaction markers** — `{"redacted": true, "reason": "..."}`
3. **Access tiers** — different views for different audiences
4. **Encryption** — sensitive fields encrypted, keys managed separately

**Current leaning:** Reference by default, with optional redaction markers.

### How does the format evolve?

**Options under consideration:**

1. **Semver** — `1.2.3` with compatibility rules
2. **Date-based** — `2026-01`
3. **Extension-based** — core is frozen, everything else is extensions

**Current leaning:** Semver for core schema, with "old readers ignore unknown
fields" policy.
