# RFC: Lineage Between Paths in Toolpath

**Status:** Draft
**Authors:** Ben Barber <ben@empathic.dev>
**Created:** 2026-11-12
**Extends:** [RFC: Toolpath - A Format for Artifact Transformation Provenance](../RFC.md)

## Abstract

This RFC defines how a path records the earlier work it descends from. A
path names one step of a path in another document as its parent, and every
root step of the path descends from that step. Whether the path continues
the parent path or forks from it follows from which step it names; neither
is declared.

The RFC also defines how the main line of a path and each branch merged
into it are recovered under the core RFC's first-parent rule, and how a
delegated subagent appears as such a branch inside its parent's path.

One additive schema change is required: an optional `parent` field on
`PathIdentity`, which the JSONL format carries on `PathOpen`.

## Motivation

### The Problem

A coding session is often split across several paths. It is resumed after
it ended, handed off to another harness, or compacted into a new segment
that opens with a summary of the old one. Each of these produces a new path
that picks up where an earlier path ended. The core RFC resolves step
references within the containing path and supports no cross-path step
references, so the first step of the new path cannot name the step it
follows. The two paths are recorded with no edge between them.

Work inside one session also branches. An agent that delegates to a
subagent runs the subagent's steps alongside its own and receives the
result back. The core step DAG represents this as a branch and a merge,
but a reader needs a rule to tell which parent of the merge step is the
main conversation and which is the subagent.

### Goals

1. **One explicit link between paths.** A path names the single step it
   descends from, with fields a reader can check.
2. **Nothing declared that can be derived.** Continuation and fork follow
   from the step a path names.
3. **Structure recoverable from `parents` alone.** The main line and each
   branch of a path, a delegated subagent included, are recovered without
   extra metadata.

## Vocabulary

| Term             | Meaning                                                       |
| ---------------- | ------------------------------------------------------------- |
| **parent**       | The step in another path a path descends from                 |
| **continuation** | A path whose parent step is the parent path's head            |
| **fork**         | A path whose parent step is any other step of the parent path |

In this RFC, *fork* names a kind of path. The core RFC's
[vocabulary](../RFC.md#vocabulary) uses the word for where paths diverge.

## Path Parent

`path.parent` names the step in another path that this path descends
from. Every root step of this path (a step with no `parents`) has
`parent.step` as its implicit parent; a reader composing the two paths
restores that edge.

```json
{
  "path": {
    "id": "p2",
    "base": { "uri": "github:org/repo", "ref": "abc123" },
    "head": "s9",
    "parent": {
      "uri": "https://pathbase.dev/u/org/repo/graphs/5b4e…",
      "path": "p1",
      "step": "s17"
    }
  }
}
```

| Field  | Description                                                              |
| ------ | ------------------------------------------------------------------------ |
| `uri`  | The document holding the parent path. Same URI forms as `$ref`, no fragment. |
| `path` | ID of the parent path in that document. Required even for a single-path document, so a reader can check it. |
| `step` | ID of the step this path's root steps descend from.                      |

All three fields are required.

### Continuations and Forks

Whether the path is a continuation or a fork is not declared; it follows
from `parent.step`:

| Term             | Definition                                                            |
| ---------------- | --------------------------------------------------------------------- |
| **continuation** | `parent.step` is the parent path's `head`: this path picks up where the parent ended. |
| **fork**         | `parent.step` is any other step of the parent path: new work rooted in its history. |

Several paths may be rooted at the same head, and each is a continuation;
which of them a viewer follows is a presentation choice outside this
format. Handoff to another harness and compaction (a new segment opening
with a summary of the old) are ordinary continuations. A client resuming
a session sets `parent.step` to the parent's head, even when a deriver
placed trailing steps after the session's last turn.

A reader classifies against the parent path's `head` as it stands. The
parent document is expected not to change once another path descends
from it.

### Relation to `base`, `step.parents` and `meta.refs`

`parent` is lineage; [`base`](../RFC.md#base-context) is the project
context the path ran against. The two answer different questions and a
path may carry both; a continuation usually shares its parent's base,
since both ran against the same project. Three things are easy to
confuse: branching inside a path is `step.parents`; the project context a
path ran against is `base`, the [`toolpath:`](../RFC.md#toolpath-base) form
included; descent from work in another document is `parent`. Soft
correlation between paths — the same change seen from two sources, say —
stays in `meta.refs` (see [Correlation RFC](RFC-correlation.md)); it
creates no edges.

## Main Line and Branches

Under the core RFC's [first-parent rule](../RFC.md#first-parent), the main
line and each branch of a path are recoverable from structure alone: the
main line is the `parents[0]` chain from `head` to a root; the branch
merged at step *m* starts at `m.parents[1..]` and follows `parents[0]`
until it reaches a step on *m*'s own first-parent chain. That step is part
of the branch: it is the step the branch left the main line at, and for a
delegation it is the tool-use step that spawned the subagent and carries
what the subagent was told to do.

### Delegated Subagents

A delegated subagent is a branch. It leaves the main line at the tool-use
step that spawned it, its steps carry their own actor, and the tool-result
step is the merge:

```
turn-7 ─ agent-call ─────────────────────── agent-result ─ turn-8 (head)
            └─ sub-1 ─ sub-2 ─ sub-3 ─────────┘
```

```json
{ "step": { "id": "sub-1", "parents": ["agent-call"], "actor": "agent:claude-code/explore", … } }
{ "step": { "id": "agent-result", "parents": ["agent-call", "sub-3"], "actor": "agent:claude-code", … } }
```

When the main conversation moved on while the subagent ran, the merge's
first parent is the later main-line step instead of `agent-call`. A branch
that never merges — a subagent that was stopped or never reported back — is
a dead end. Nested subagents are branches off a branch.

## JSONL

This RFC adds one field to the `PathOpen` line of the
[JSONL Streaming RFC](RFC-jsonl.md):

| Field | Required | Description |
| ----- | -------- | ----------- |
| `parent` | no | `PathIdentity.parent` — the step in another path this path descends from, same shape as canonical JSON. |

```json
{"PathOpen": {
  "version": "1",
  "id": "pr-42",
  "base": {"uri": "github:org/repo", "ref": "abc123"},
  "graph_ref": "toolpath://archive/release-v2",
  "parent": {"uri": "https://pathbase.dev/u/org/repo/graphs/5b4e", "path": "pr-41", "step": "s17"},
  "meta": {
    "title": "Add email validation"
  }
}}
```

`PathOpen` is the only line kind that carries `path.parent`. A reader
takes `parent` from `PathOpen` together with `id`, `base` and
`graph_ref`, and emits it as `path.parent` of the sealed `Path`. A writer
emits `path.parent` as `PathOpen.parent`.

## Schema Change

One additive change to the canonical schema, in
`crates/toolpath/src/types.rs`:

```rust
pub struct PathIdentity {
    pub id: String,
    pub base: Option<Base>,
    pub head: String,
    pub graph_ref: Option<String>,
    pub parent: Option<Parent>,   // NEW
}

pub struct Parent {
    pub uri: String,
    pub path: String,
    pub step: String,
}
```

`parent` is optional and omitted when empty. In the JSON Schema,
`pathIdentity` gains a `parent` property whose `uri`, `path` and `step`
are all required, and which admits no other fields.

## Example

[`examples/path-05-continuation.path.json`](../examples/path-05-continuation.path.json)
(JSONL form:
[`examples/path-05-continuation.path.jsonl`](../examples/path-05-continuation.path.jsonl))
continues the session in
[`examples/path-02-local-session.path.json`](../examples/path-02-local-session.path.json):

```json
"path": {
  "id": "path-session-xyz-2",
  "base": { "uri": "file:///home/alex/projects/myrepo", "ref": "def789abc123456" },
  "head": "step-007",
  "parent": {
    "uri": "file:///home/alex/.toolpath/path-02-local-session.path.json",
    "path": "path-session-xyz",
    "step": "step-003"
  }
}
```

`step-003` is the head of `path-session-xyz`, so the path is a
continuation. It shares its parent's base. Its root step, `step-004`, has
no `parents` and descends from `step-003`.

At `step-005` the agent delegates to a subagent. The subagent's steps,
`step-005a` and `step-005b`, carry the actor `agent:claude-code/explore`,
and `step-006` merges the branch back with
`parents: ["step-005", "step-005b"]`. The main line from the head is
`step-007`, `step-006`, `step-005`, `step-004`. The branch merged at
`step-006` is `step-005b`, `step-005a` and `step-005`, the tool-use step
the branch left the main line at.

## Design Rationale

### Why is a subagent a branch and not its own path?

The core RFC does not support cross-path step references. A subagent in a
path of its own could not merge back: the step at which its result arrives
in the main conversation could not name the subagent's last step as a
parent. As a branch inside the parent path, the result step is a merge
with an edge to the subagent's last step, and the first-parent rule keeps
the main line recoverable.

## Compatibility

- **Documents without `parent`** are unaffected. The field is optional,
  and documents that do not carry it validate unchanged.
- **Validators using a schema from before this RFC** reject a document
  that carries `parent`, because `pathIdentity` admits no unknown fields.
- **Readers that ignore unknown fields** read a path that carries `parent`
  as a path without one: its root steps have no parent and it is not
  linked to the earlier work.
- **Documents written before the first-parent rule** may order `parents`
  differently. A reader recovering the main line or a branch from such a
  document may follow a branch as the main line.
- **Existing signatures** remain valid. Canonicalization is unchanged. A
  path author signature over a path that carries `parent` covers it,
  because that signature covers the whole `path` object.

## Prior Art

- **Git's first-parent convention**: a merge commit's first parent is the
  branch that was merged into; `git log --first-parent` follows the main
  line. The core RFC's ordering of `parents` follows the same rule.
