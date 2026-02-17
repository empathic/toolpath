---
layout: base.njk
title: Format
nav: format
permalink: /format/
---

# The Toolpath format

<p class="subtitle">
Three objects. One DAG. Full provenance.
</p>

Toolpath defines three object types at decreasing levels of granularity. Every Toolpath JSON document is externally tagged with exactly one top-level key:

```json
{ "Step":  { "step": {...}, "change": {...} } }
{ "Path":  { "path": {...}, "steps": [...] } }
{ "Graph": { "graph": {...}, "paths": [...] } }
```

## Step

The core primitive. A step records a single change to one or more artifacts by one actor.

Every step has three top-level keys:

| Key      | What it holds                                                  |
| -------- | -------------------------------------------------------------- |
| `step`   | Identity and lineage: id, parents, actor, timestamp            |
| `change` | The actual modifications: artifact URLs mapped to perspectives |
| `meta`   | Everything else: intent, refs, actors, signatures (optional)   |

### Parents

The `step.parents` array establishes the DAG:

| Parents                | Meaning                            |
| ---------------------- | ---------------------------------- |
| `[]` or omitted        | Root step (no parents)             |
| `["step-001"]`         | Single parent (linear history)     |
| `["step-A", "step-B"]` | Merge (derived from parallel work) |

### Example

```json
{
  "Step": {
    "step": {
      "id": "step-003",
      "parents": ["step-002"],
      "actor": "agent:claude-code",
      "timestamp": "2026-01-29T15:30:00Z"
    },
    "change": {
      "src/auth/validator.rs": {
        "raw": "@@ -1,5 +1,25 @@\n use std::error::Error;\n+..."
      }
    },
    "meta": {
      "intent": "Add email validation to prevent malformed input",
      "refs": [{ "rel": "fixes", "href": "issue://github/repo/issues/42" }]
    }
  }
}
```

## Path

A path collects steps and provides a root context. Think of it as a PR, a coding session, or a branch.

| Key     | What it holds                                             |
| ------- | --------------------------------------------------------- |
| `path`  | Identity, base context, and head reference                |
| `steps` | Array of step objects (the full DAG, including dead ends) |
| `meta`  | Path-level metadata: title, actors, signatures (optional) |

### Base context

The `path.base` anchors the path to a specific state:

```json
{
  "base": {
    "uri": "github:myorg/myrepo",
    "ref": "abc123def456"
  }
}
```

| URI scheme                 | Meaning                         |
| -------------------------- | ------------------------------- |
| `github:org/repo`          | GitHub repository               |
| `file:///path`             | Local filesystem                |
| `toolpath:path-id/step-id` | Branch from another path's step |

### Dead ends

Dead ends are implicit. A step is a dead end if it has no descendants leading to `path.head`. No explicit marking required &mdash; the DAG structure determines it.

```
active_steps = ancestors(path.head)
dead_ends = all_steps - active_steps
```

### Example

```json
{
  "Path": {
    "path": {
      "id": "path-pr-42",
      "base": {
        "uri": "github:myorg/myrepo",
        "ref": "abc123def456"
      },
      "head": "step-004"
    },
    "steps": [
      {
        "step": { "id": "step-001", "actor": "human:alex", "timestamp": "..." },
        "change": { "src/main.rs": { "raw": "..." } },
        "meta": { "intent": "Initial change" }
      },
      {
        "step": {
          "id": "step-002a",
          "parents": ["step-001"],
          "actor": "agent:claude-code",
          "timestamp": "..."
        },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Regex approach (abandoned)" }
      },
      {
        "step": {
          "id": "step-002",
          "parents": ["step-001"],
          "actor": "agent:claude-code",
          "timestamp": "..."
        },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Custom error type approach" }
      },
      {
        "step": {
          "id": "step-003",
          "parents": ["step-002"],
          "actor": "tool:rustfmt",
          "timestamp": "..."
        },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Auto-format" }
      },
      {
        "step": {
          "id": "step-004",
          "parents": ["step-003"],
          "actor": "human:alex",
          "timestamp": "..."
        },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Refine error messages" }
      }
    ],
    "meta": {
      "title": "Add email validation"
    }
  }
}
```

Step `step-002a` is a dead end: it forks from `step-001` but has no descendants leading to the head (`step-004`).

## Graph

A graph collects related paths. Think of it as a release, a sprint, or a project.

| Key     | What it holds                                              |
| ------- | ---------------------------------------------------------- |
| `graph` | Identity (id)                                              |
| `paths` | Array of inline Path objects or `$ref` references          |
| `meta`  | Graph-level metadata: title, actors, signatures (optional) |

Paths can be inline or referenced externally:

```json
{
  "Graph": {
    "graph": { "id": "graph-release-v2" },
    "paths": [
      { "path": {...}, "steps": [...] },
      { "$ref": "https://archive.example.com/toolpath/path-44.json" }
    ],
    "meta": { "title": "Release v2.0" }
  }
}
```

<svg class="topo topo-wide" viewBox="0 0 900 70" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <path d="M0,35 Q150,10 300,40 Q450,65 600,25 Q750,0 900,35" stroke="#b5652b" stroke-width="1" opacity="0.10" fill="none"/>
  <path d="M0,40 Q160,18 310,45 Q460,68 610,32 Q760,5 900,42" stroke="#8a8078" stroke-width="1" opacity="0.08" fill="none"/>
  <path d="M0,45 Q170,25 320,48 Q470,70 620,38 Q770,10 900,48" stroke="#b5652b" stroke-width="1" opacity="0.06" fill="none"/>
</svg>

## Artifacts

Artifact keys in `change` are URLs. Bare paths are relative to `path.base`:

| Key format         | Interpretation               |
| ------------------ | ---------------------------- |
| `src/foo.rs`       | Relative to `path.base` root |
| `file:///abs/path` | Absolute file path           |
| `https://...`      | Web resource                 |
| `s3://...`         | S3 object                    |

## Change perspectives

Each artifact maps to one or more perspectives on the modification:

| Perspective  | Description                            | Example                           |
| ------------ | -------------------------------------- | --------------------------------- |
| `raw`        | Unified Diff (same as `diff -u` / git) | `@@ -1,5 +1,10 @@\n...`           |
| `structural` | Language-aware AST operations          | `{"type": "rust.add_items", ...}` |

Consumers use the perspective they understand. A dumb text tool uses `raw`. An IDE might use `structural`.

## Actors

Actors follow the pattern `type:name`:

```
human:alex
agent:claude-code
tool:rustfmt/1.7.0
ci:github-actions/workflow-123
```

Full actor definitions (identity, keys) live in `meta.actors`.

## Signatures

<svg class="topo topo-float-right topo-sm" viewBox="0 0 150 150" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <ellipse cx="75" cy="75" rx="65" ry="60" stroke="#8a8078" stroke-width="1" opacity="0.10"/>
  <ellipse cx="80" cy="72" rx="48" ry="44" stroke="#b5652b" stroke-width="1" opacity="0.14"/>
  <ellipse cx="84" cy="70" rx="32" ry="29" stroke="#b5652b" stroke-width="1" opacity="0.20"/>
  <ellipse cx="86" cy="68" rx="18" ry="16" stroke="#b5652b" stroke-width="1.2" opacity="0.28"/>
  <circle cx="87" cy="67" r="4" fill="#b5652b" opacity="0.25"/>
</svg>

Toolpath supports multi-party, scoped cryptographic signatures using JCS (RFC 8785) canonicalization:

| Scope      | What it attests                |
| ---------- | ------------------------------ |
| `author`   | "I authored this change"       |
| `reviewer` | "I reviewed and approved this" |
| `witness`  | "I observed this happened"     |
| `ci`       | "CI verified this"             |
| `release`  | "This is an official release"  |

## Full specification

The complete format specification is in [RFC.md](https://github.com/empathic/toolpath/blob/main/RFC.md). A JSON Schema is available at [schema/toolpath.schema.json](https://github.com/empathic/toolpath/blob/main/schema/toolpath.schema.json).
