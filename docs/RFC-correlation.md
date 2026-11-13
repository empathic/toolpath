# RFC: Cross-Path Correlation in Toolpath

**Status:** Draft
**Authors:** with Alex Kesling <alex@empathic.dev>
**Created:** 2026-02-13
**Extends:** [RFC: Toolpath - A Format for Artifact Transformation Provenance](./RFC.md)

## Abstract

This RFC defines conventions for correlating steps and paths across provenance
sources within Toolpath documents. When a graph contains paths derived from
different systems — e.g., one from a Claude conversation log and another from
git history — those paths describe overlapping work but currently float in
isolation. This extension defines how to connect them using existing Toolpath
primitives (`meta.source`, `meta.refs`, artifact keys) plus a small vocabulary
of relationship types.

No schema changes are required. This RFC defines conventions, not new fields.

## Motivation

### The Problem

Toolpath can derive provenance from multiple sources independently:

- **VCS history** produces paths where each step is a commit, with diffs,
  actors, timestamps, and VCS source references.
- **Editor / tool history** each formatter run, each file change, etc. can be
  recorded as steps.
- **Agentic coding conversation logs** produce paths where each step is a
  conversation turn, with file edits, tool invocations, intent, and actor
  information.

These can be combined into a single graph. But the result is two disconnected
subgraphs — paths sitting side by side with no explicit relationship between
them.

In reality, these paths describe **the same work** from different vantage
points:

```
Claude session                  Git history
─────────────                   ───────────
turn-1: "Add validation"        commit abc: "Add validation"
turn-2: "Fix the tests"         commit def: "Fix the tests"
turn-3: "Refactor per review"   commit 123: "Refactor per review"
```

A Claude conversation turn that edits `src/validator.rs` and the git commit
that records that edit are **the same change** viewed from two perspectives.
One preserves the reasoning (why, what was tried, what was rejected). The other
preserves the VCS record (what was committed, who in git's view, the full diff
in repository context).

Without explicit correlation, tools cannot:
- Show "here's the conversation that led to this commit"
- Navigate from a git step to the reasoning that produced it
- Identify which conversation turns produced which commits
- Merge actor information across provenance sources

### Design Principles

This extension follows two principles:

1. **Use what exists.** Toolpath already has `meta.source` for VCS correlation,
   `meta.refs` for typed links, and artifact keys for implicit connections.
   New conventions should compose existing primitives, not introduce new ones.

2. **Convention over invention.** Define relationship vocabulary and URI
   patterns. Let tooling do the wiring. Don't ask humans to hand-link steps.

## Three Correlation Surfaces

Toolpath paths from different sources connect at three levels. Each level uses
an existing Toolpath mechanism.

### Surface 1: Artifact Identity (Implicit)

Paths derived from different sources naturally share **artifact keys** — the
URLs in the `change` object. A Claude-derived step that edits `src/validator.rs`
and a git-derived step that commits changes to `src/validator.rs` both use the
same key.

This is the weakest form of correlation (many steps may touch the same file)
but it's always present and requires no annotation. Tools can use shared
artifact keys to surface "these steps from different paths touched the same
files."

**Mechanism:** Existing `change` object keys.
**Requires:** Nothing — already present by construction.

### Surface 2: VCS Revision (Explicit Join Key)

The strongest correlation between a Claude-derived step and a git-derived step
is a **shared VCS revision**. Both steps describe the same commit, and both
can carry that commit's hash.

Git-derived steps already carry this in `meta.source`:

```json
{
  "step": { "id": "step-abc123", "actor": "human:alex", "..." : "..." },
  "change": { "src/validator.rs": { "raw": "..." } },
  "meta": {
    "source": { "type": "git", "revision": "abc123def456" }
  }
}
```

Claude-derived steps currently do **not** carry this. When Claude runs a tool
that creates a git commit, the commit hash appears in the tool result but is
discarded during derivation. Derivers SHOULD extract commit hashes from tool
results and stamp the corresponding step:

```json
{
  "step": { "id": "claude-turn-7", "actor": "agent:claude-code", "..." : "..." },
  "change": { "src/validator.rs": { "raw": "..." } },
  "meta": {
    "intent": "Add email validation",
    "source": { "type": "git", "revision": "abc123def456" }
  }
}
```

Two steps from different paths that share `meta.source.revision` describe the
same VCS event. This is the primary join key for cross-path correlation.

**Mechanism:** Existing `meta.source` field (`vcsSource` in the schema).
**Requires:** Derivers that extract and stamp commit hashes from tool output.

### Surface 3: Typed Cross-References (Explicit Links)

For relationships that go beyond "same commit" — causal connections between
paths, soft associations between steps, context pointers — Toolpath's existing
`meta.refs` mechanism carries typed links.

Each ref is `{ "rel": "<relationship>", "href": "<URI>" }`. This RFC defines
a vocabulary of relationship types and a URI convention for referencing steps
and paths within the same graph.

**Mechanism:** Existing `meta.refs` field on steps, paths, and graphs.
**Requires:** Relationship vocabulary (below) and tooling support.

## Relationship Vocabulary

This RFC defines the following `rel` values for cross-path correlation. Tools
SHOULD use these values; tools MUST preserve unrecognized `rel` values.

### Step-Level Relationships

| `rel`           | Meaning                                                     |
| --------------- | ----------------------------------------------------------- |
| `same-change`   | This step and the target describe the same artifact mutation. They share a VCS revision or represent the same logical edit from different provenance sources. |
| `produces`      | This step causally produced the target step. E.g., a Claude conversation turn that ran `git commit` produced the corresponding git commit step. |
| `context-for`   | This step provides reasoning or conversational context for the target step. E.g., a Claude turn with thinking/discussion that preceded the actual edit. |

### Path-Level Relationships

| `rel`           | Meaning                                                     |
| --------------- | ----------------------------------------------------------- |
| `produces`      | This path's work produced the target path's VCS record. E.g., a Claude session path that produced a git branch path. |
| `produced-by`   | Inverse of `produces`. This path's VCS record was produced by the target path's session. |
| `complements`   | This path and the target describe overlapping work from different perspectives, without a clear causal direction. |

### Graph-Level Relationships

Graph-level `meta.refs` MAY use any of the above, plus:

| `rel`           | Meaning                                                     |
| --------------- | ----------------------------------------------------------- |
| `correlates`    | This graph contains paths that have been correlated. Informational marker. |

## URI Conventions

Cross-references within a graph use `toolpath:` URIs to identify paths and
steps:

| Pattern                        | Meaning                            |
| ------------------------------ | ---------------------------------- |
| `toolpath:<path-id>`           | Reference to a path within the same graph |
| `toolpath:<path-id>/<step-id>` | Reference to a step within a path  |

These URIs are resolved within the containing graph. They are not global
identifiers.

Examples:

```json
{"rel": "same-change", "href": "toolpath:path-git-main/step-abc123"}
{"rel": "produces", "href": "toolpath:path-git-feature"}
{"rel": "produced-by", "href": "toolpath:path-claude-session-xyz"}
```

## Correlation Semantics

Correlation is the process of enriching a graph containing paths from multiple
provenance sources with cross-references. The operation is idempotent —
applying it twice produces the same result.

### Input

A Toolpath Graph document containing two or more paths.

### Algorithm

```
correlate(graph):
  // Phase 1: Build revision index
  revision_index = {}    // map: revision -> [(path_id, step_id)]
  for path in graph.paths:
    for step in path.steps:
      if step.meta.source.revision exists:
        revision_index[revision].append((path.id, step.id))

  // Phase 2: Add step-level refs for shared revisions
  for revision, locations in revision_index:
    if locations has entries from multiple paths:
      for (path_a, step_a), (path_b, step_b) in all_pairs(locations):
        add_ref(step_a, "same-change", "toolpath:{path_b}/{step_b}")
        add_ref(step_b, "same-change", "toolpath:{path_a}/{step_a}")

  // Phase 3: Infer path-level relationships
  correlated_paths = set of path pairs with any step-level correlations
  for (path_a, path_b) in correlated_paths:
    direction = infer_direction(path_a, path_b)
    if direction == "a_produces_b":
      add_ref(path_a.meta, "produces", "toolpath:{path_b.id}")
      add_ref(path_b.meta, "produced-by", "toolpath:{path_a.id}")
    else:
      add_ref(path_a.meta, "complements", "toolpath:{path_b.id}")
      add_ref(path_b.meta, "complements", "toolpath:{path_a.id}")

  // Phase 4: Mark graph as correlated
  add_ref(graph.meta, "correlates", "self")

  return graph
```

### Direction Inference

When two paths are correlated, conforming tools infer causal direction:

- If path A has actor type `agent:*` and path B has `meta.source` with VCS
  type on all steps, path A `produces` path B.
- If path A's `meta.source` field references an `agent://` URI, path A was
  likely `produced-by` the agent session.
- Otherwise, use `complements` (no causal direction).

Tools MAY use heuristics (timestamp ordering, actor types, source URIs) to
refine direction inference. Tools MUST NOT infer `produces` unless the evidence
is strong.

## Examples

### Before Correlation

A graph containing a Claude-derived path and a git-derived path, merged but
unconnected:

```json
{
  "Graph": {
    "graph": { "id": "graph-session-work" },
    "paths": [
      {
        "path": {
          "id": "path-claude-session-abc",
          "base": { "uri": "file:///home/alex/myrepo" },
          "head": "claude-turn-3"
        },
        "steps": [
          {
            "step": {
              "id": "claude-turn-1",
              "actor": "agent:claude-code",
              "timestamp": "2026-02-13T10:00:00Z"
            },
            "change": {
              "src/validator.rs": { "raw": "@@ -1,0 +1,20 @@\n+pub struct Validator..." }
            },
            "meta": {
              "intent": "Add email validation struct",
              "source": { "type": "git", "revision": "abc123" }
            }
          },
          {
            "step": {
              "id": "claude-turn-2",
              "actor": "agent:claude-code",
              "timestamp": "2026-02-13T10:05:00Z",
              "parents": ["claude-turn-1"]
            },
            "change": {
              "src/validator.rs": { "raw": "@@ -15,0 +15,10 @@\n+pub fn validate..." }
            },
            "meta": {
              "intent": "Add validate_email function",
              "source": { "type": "git", "revision": "def456" }
            }
          }
        ],
        "meta": {
          "title": "Claude session: Add email validation",
          "source": "agent://claude-code/session-abc"
        }
      },
      {
        "path": {
          "id": "path-git-feature",
          "base": {
            "uri": "github:myorg/myrepo",
            "ref": "aaa111"
          },
          "head": "step-def456"
        },
        "steps": [
          {
            "step": {
              "id": "step-abc123",
              "actor": "human:alex",
              "timestamp": "2026-02-13T10:00:05Z"
            },
            "change": {
              "src/validator.rs": { "raw": "@@ -1,0 +1,20 @@\n+pub struct Validator..." }
            },
            "meta": {
              "intent": "Add email validation struct",
              "source": { "type": "git", "revision": "abc123" }
            }
          },
          {
            "step": {
              "id": "step-def456",
              "actor": "human:alex",
              "timestamp": "2026-02-13T10:05:05Z",
              "parents": ["step-abc123"]
            },
            "change": {
              "src/validator.rs": { "raw": "@@ -15,0 +15,10 @@\n+pub fn validate..." }
            },
            "meta": {
              "intent": "Add validate_email function",
              "source": { "type": "git", "revision": "def456" }
            }
          }
        ],
        "meta": {
          "title": "Branch: feature/email-validation"
        }
      }
    ]
  }
}
```

### After Correlation

The same graph after correlation:

```json
{
  "Graph": {
    "graph": { "id": "graph-session-work" },
    "paths": [
      {
        "path": {
          "id": "path-claude-session-abc",
          "base": { "uri": "file:///home/alex/myrepo" },
          "head": "claude-turn-3"
        },
        "steps": [
          {
            "step": {
              "id": "claude-turn-1",
              "actor": "agent:claude-code",
              "timestamp": "2026-02-13T10:00:00Z"
            },
            "change": {
              "src/validator.rs": { "raw": "@@ ..." }
            },
            "meta": {
              "intent": "Add email validation struct",
              "source": { "type": "git", "revision": "abc123" },
              "refs": [
                {"rel": "same-change", "href": "toolpath:path-git-feature/step-abc123"}
              ]
            }
          },
          {
            "step": {
              "id": "claude-turn-2",
              "actor": "agent:claude-code",
              "timestamp": "2026-02-13T10:05:00Z",
              "parents": ["claude-turn-1"]
            },
            "change": {
              "src/validator.rs": { "raw": "@@ ..." }
            },
            "meta": {
              "intent": "Add validate_email function",
              "source": { "type": "git", "revision": "def456" },
              "refs": [
                {"rel": "same-change", "href": "toolpath:path-git-feature/step-def456"}
              ]
            }
          }
        ],
        "meta": {
          "title": "Claude session: Add email validation",
          "source": "agent://claude-code/session-abc",
          "refs": [
            {"rel": "produces", "href": "toolpath:path-git-feature"}
          ]
        }
      },
      {
        "path": {
          "id": "path-git-feature",
          "base": {
            "uri": "github:myorg/myrepo",
            "ref": "aaa111"
          },
          "head": "step-def456"
        },
        "steps": [
          {
            "step": {
              "id": "step-abc123",
              "actor": "human:alex",
              "timestamp": "2026-02-13T10:00:05Z"
            },
            "change": {
              "src/validator.rs": { "raw": "@@ ..." }
            },
            "meta": {
              "intent": "Add email validation struct",
              "source": { "type": "git", "revision": "abc123" },
              "refs": [
                {"rel": "same-change", "href": "toolpath:path-claude-session-abc/claude-turn-1"}
              ]
            }
          },
          {
            "step": {
              "id": "step-def456",
              "actor": "human:alex",
              "timestamp": "2026-02-13T10:05:05Z",
              "parents": ["step-abc123"]
            },
            "change": {
              "src/validator.rs": { "raw": "@@ ..." }
            },
            "meta": {
              "intent": "Add validate_email function",
              "source": { "type": "git", "revision": "def456" },
              "refs": [
                {"rel": "same-change", "href": "toolpath:path-claude-session-abc/claude-turn-2"}
              ]
            }
          }
        ],
        "meta": {
          "title": "Branch: feature/email-validation",
          "refs": [
            {"rel": "produced-by", "href": "toolpath:path-claude-session-abc"}
          ]
        }
      }
    ],
    "meta": {
      "refs": [
        {"rel": "correlates", "href": "self"}
      ]
    }
  }
}
```

## Guidance for Derivers

### Stamping VCS Revisions

For correlation to work via Surface 2, derivers that produce steps from
non-VCS sources (conversation logs, CI pipelines, etc.) SHOULD detect VCS
revision hashes in their source material and stamp the corresponding step's
`meta.source` field.

For conversation-based derivers, this means inspecting tool use results for
evidence of git commits (patterns like `Created commit abc123` or git output
containing a commit hash). Heuristics are acceptable; false negatives are
preferable to false positives.

### Writing Correlation Refs

Tools that correlate a graph SHOULD:

1. Join steps across paths on `meta.source.revision`.
2. Add symmetric `same-change` refs between matched step pairs.
3. Infer path-level relationships (`produces`, `produced-by`, or
   `complements`) from the aggregate step-level correlations.
4. Mark the graph with `{"rel": "correlates", "href": "self"}` to indicate
   that correlation has been applied.

The operation MUST be idempotent — running it on an already-correlated graph
produces no additional changes.

### Rendering Correlation

Renderers that visualize graphs SHOULD distinguish cross-path correlation
refs from structural edges. For example, `same-change` links might be drawn
as dashed edges between steps in different path subgraphs, while path-level
`produces`/`produced-by` relationships might be drawn as labeled edges
between path boundaries.

## Design Rationale

### Why no schema changes?

The existing Toolpath schema already contains every primitive needed for
cross-path correlation:

- `meta.source` with `vcsSource` for VCS revision correlation
- `meta.refs` with typed `{rel, href}` links for explicit cross-references
- `toolpath:` URI scheme for intra-graph references
- Artifact keys in `change` for implicit overlap

Adding new fields would create parallel mechanisms for things that existing
fields already express. Convention is cheaper than invention, and existing
parsers already handle these fields.

### Why `same-change` instead of merging steps?

Two steps from different provenance sources that describe the same commit
could be merged into a single step. We explicitly avoid this because:

1. **Each path should remain independently valid.** Removing a step from a
   path breaks its DAG. Links between steps preserve both paths intact.

2. **The perspectives are different.** A Claude-derived step carries
   conversation context (intent from the discussion, tool invocations,
   thinking). A git-derived step carries VCS context (full repository diff,
   committer identity, signature). Merging loses one perspective.

3. **Provenance is append-only.** Correlation should enrich, not restructure.
   The original derivations remain untouched; refs are additive.

### Why not cross-path parent edges?

The base RFC states: "Cross-path step references are not supported" for
`parents`. This is deliberate — each path's DAG must be self-contained for
independent validation, signature verification, and query operations.

`meta.refs` links are explicitly **not** structural. They don't affect the
DAG, ancestor queries, or dead-end detection. They're hyperlinks — typed
pointers that add meaning without altering structure.

### Why infer direction?

Requiring users to manually specify "this Claude session produced that git
branch" adds ceremony for something the tool can usually figure out. An
`agent:*` actor path correlating with a VCS-sourced path is almost certainly
the producer. Automatic inference with a `complements` fallback handles the
common case while staying safe for ambiguous ones.

## Compatibility

This extension is fully backward-compatible:

- **Old readers** ignore unknown `rel` values in `meta.refs` (per the base
  RFC's "ignore unknown fields" policy)
- **Old writers** simply don't emit correlation refs — uncorrelated graphs
  remain valid
- **No schema changes** — all `rel` values and URI patterns are valid under
  the existing schema
- **Idempotent** — correlating an already-correlated graph is a no-op

## Open Questions

### How should partial correlation be handled?

When a Claude session produces 10 conversation turns but only 3 result in
git commits, should the non-committing turns be linked to anything? Current
answer: no. Only steps with matching `meta.source.revision` values get
`same-change` links. Context turns adjacent to a committing turn could get
`context-for` links in a future refinement.

### Should correlation work across graphs?

This RFC scopes correlation to within a single graph. Cross-graph correlation
(e.g., linking a step in graph A to a step in graph B) would require global
URIs rather than graph-local `toolpath:` URIs. This is deferred.

### How should timestamp-only correlation work?

When VCS revision matching isn't available (e.g., a deriver couldn't extract
commit hashes), timestamp proximity is a weaker correlation signal. Should
tools fall back to timestamp matching? Current answer: not in v1.
Timestamp correlation is fuzzy and error-prone. Better to improve hash
extraction than to rely on clock agreement.

## Prior Art

- **Linked Data / RDF**: Resources connected by typed links with URIs.
  Toolpath's `refs` mechanism is a simplified version of this pattern.
- **Atom Link Relations (RFC 4287)**: Typed `rel`/`href` pairs for relating
  web resources. Direct inspiration for Toolpath's `ref` object.
- **HTTP Link Header (RFC 8288)**: Web linking with relation types.
  The `rel` vocabulary pattern comes from this tradition.
- **Git Notes**: Attaching metadata to commits after the fact. Correlation
  refs serve a similar purpose — enriching existing objects without modifying
  their core content.
