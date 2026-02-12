# RFC: Toolpath - A Format for Artifact Transformation Provenance

**Status:** Draft
**Authors:** Alex Kesling <alex@empathic.dev>
**Created:** 2026-01-29

## Abstract

Toolpath is a tool-agnostic format for tracking the complete history of changes
applied to artifacts (initially code but extensible to other artifact types).
Toolpath models state as a collection of discrete artifact transformations,
preserving full provenance across human edits, AI agent changes, automated
formatters, and any other tooling.

Notably, because Toolpath is intended to account for all inputs to changes, it's
a good format for encoding the agentic conversations that led up to some PR.

## Motivation

### The Problem

Current approaches to tracking code changes have significant gaps:

1. **Git knows "what" but not "why"**: Git stores snapshots, but semantic intent
   is often lost and rarely machine readable. A rename refactor looks the same
   as deleting one function and writing another.

2. **AI tooling is conversation-centric**: Existing formats for AI code
   generation focus on chat transcripts and generated spans, not on how code
   evolves through multiple actors.

3. **The tooling problem**: When Claude writes code, `rustfmt` reformats it,
   and a human refines it, git blame attributes everything to the human's
   commit. The actual provenance is lost.

4. **Dead ends disappear**: When you backtrack and try a different approach, the
   abandoned path is typically lost. There's no record of "we tried X, it didn't
   work because Y, so we pivoted to Z."

5. **Tool silos**: Each tool (IDE, AI assistant, linter, formatter) has its own
   view of changes. There's no unified format for "here's everything that
   happened to this code."

### Goals

1. **Change-centric**: The change is primary; metadata annotates it
2. **Multi-perspective**: A change can be represented at multiple levels
   (raw diff, structural AST op, etc.)
3. **Actor-agnostic**: Humans, AI agents, formatters, and scripts are all
   first-class actors
4. **Path-preserving**: Keep abandoned paths for later reflection
5. **Linkable**: Connect changes to design docs, issues, discussions, reasoning
6. **Verifiable**: Support cryptographic signatures for authenticity
7. **Minimally invasive**: Simple changes require minimal ceremony

### Non-Goals

1. Replacing git (Toolpath is complementary)
2. Real-time collaboration (CRDTs, OT) - Toolpath is for provenance, not live
   editing
3. Prescribing storage or transport mechanisms

## Core Concepts

### Vocabulary

| Term             | Meaning                                             |
| ---------------- | --------------------------------------------------- |
| **step**         | A single change to artifact(s)                      |
| **path**         | Collection of steps with a base context             |
| **graph**        | Collection of paths (release, project, etc.)        |
| **base**         | The root context (repo, commit) a path branches from|
| **head**         | The current tip of the active path                  |
| **dead end**     | Abandoned branch in the path tree                   |
| **fork**         | Where paths diverge                                 |

### Three Core Objects

Toolpath defines three object types at decreasing levels of granularity:

| Object    | Contains       | Purpose                                    |
| --------- | -------------- | ------------------------------------------ |
| **Step**  | change, meta   | Individual change to artifacts             |
| **Path**  | steps, meta    | Collection with base context (e.g., a PR)  |
| **Graph** | paths, meta    | Collection of paths (e.g., a release)      |

Each level can exist standalone or be nested. Steps can stream independently,
paths can represent complete PRs, and graphs can bundle related paths.

### Document Envelope

A Toolpath document is **externally tagged**: the top-level JSON object has
exactly one key — `"Step"`, `"Path"`, or `"Graph"` — whose value contains the
document content.  This makes the document type unambiguous without inspecting
the inner fields.

```json
{ "Step":  { "step": {...}, "change": {...} } }
{ "Path":  { "path": {...}, "steps": [...] } }
{ "Graph": { "graph": {...}, "paths": [...] } }
```

PascalCase variant names visually distinguish the type tag from the lowercase
structural fields inside (`step`, `path`, `graph`).

### ID Uniqueness

IDs must be unique within their containing scope:

| Scope   | Constraint                                           |
| ------- | ---------------------------------------------------- |
| Path    | All `step.id` values must be unique within the path  |
| Graph   | All `path.id` values must be unique within the graph |

This means:
- Two paths in the same graph cannot share a `path.id`
- Two steps in the same path cannot share a `step.id`
- Steps in *different* paths may have the same ID (they're in different scopes)
- Paths in *different* graphs may have the same ID (they're in different scopes)

When referencing steps (e.g., in `parents` arrays or `path.head`), the reference
is resolved within the containing path. Cross-path step references are not
supported.

### Steps

A **step** is the core primitive: a recorded change to one or more artifacts.

Every step has three top-level keys:

| Key      | What it is                                           |
| -------- | ---------------------------------------------------- |
| `step`   | Identity and lineage (id, parents, actor, timestamp) |
| `change` | The actual modifications (URLs → perspectives)       |
| `meta`   | Everything else (intent, refs, actors, signatures)   |

The `step.parents` array references parent step IDs:

| Parents | Meaning |
| ------- | ------- |
| `[]` or omitted | Root step (no parents) |
| `["step-001"]` | Single parent (linear history) |
| `["step-A", "step-B"]` | Merge (derived from parallel work) |

### Paths

A **path** collects steps and provides root context:

| Key      | What it is                                          |
| -------- | --------------------------------------------------- |
| `path`   | Identity, base context, and head reference          |
| `steps`  | Array of step objects                               |
| `meta`   | Path-level metadata (title, actors, signatures)     |

The `path.base` anchors the entire tree to a specific state (repo + ref +
commit). Steps within inherit this context.

### Graphs

A **graph** collects related paths:

| Key      | What it is                                          |
| -------- | --------------------------------------------------- |
| `graph`  | Identity                                            |
| `paths`  | Array of path objects or references                 |
| `meta`   | Graph-level metadata (title, actors, signatures)    |

Paths can be inline or referenced:

```json
{
  "paths": [
    { "path": {...}, "steps": [...], "meta": {...} },
    { "$ref": "toolpath://archive/path-pr-43" },
    { "$ref": "https://example.com/toolpath/path-44.json" }
  ]
}
```

The `$ref` syntax (borrowed from JSON Schema) allows paths to be stored
externally and referenced by URL. This enables:
- Archives of paths that can be composed into graphs
- Distributed storage across systems
- Deduplication when paths appear in multiple graphs

### Artifacts

Artifacts are identified by URL. The keys in the `change` object are URLs:

```json
{
  "change": {
    "src/auth/validator.rs": {...},
    "https://example.com/api/schema.json": {...},
    "s3://bucket/config/settings.yaml": {...}
  }
}
```

| Key Format          | Interpretation                              |
| ------------------- | ------------------------------------------- |
| `src/foo.rs`        | Relative path within `path.base` context    |
| `file:///abs/path`  | Absolute file path                          |
| `https://...`       | Web resource                                |
| `s3://...`          | S3 object                                   |
| `<scheme>://...`    | Any URL scheme                              |

Relative paths (no scheme) are interpreted as files relative to the `path.base`
repository root. This keeps the common case compact while allowing references to
web APIs, cloud storage, or any URL-addressable resource.

### Change Perspectives

Each artifact in `change` maps to one or more **perspectives** on the
modification:

| Perspective  | Description                    | Example                    |
| ------------ | ------------------------------ | -------------------------- |
| `raw`        | Unified Diff format            | `@@ -1,5 +1,10 @@\n...`    |
| `structural` | Language-aware AST operations  | "add method to impl block" |

The `raw` perspective uses **Unified Diff** (the format produced by `diff -u`
and used by git). This format is widely understood, human-readable, and
backward-compatible with existing tooling.

```
@@ -start,count +start,count @@
 context line
-removed line
+added line
 context line
```

Consumers use the perspective they understand. A dumb text tool uses `raw`. An
IDE might use `structural`.

### Actors

An **actor** is anything that performs steps. The short form is a string:

```
human:alex
agent:claude-code
agent:claude-code/session-xyz
tool:rustfmt
tool:rustfmt/1.5.0
ci:github-actions/workflow-123
```

Actor strings are referenced in `step.actor`. Full actor definitions with
identity and key information are provided in `meta.actors`.

### Meta Object

The `meta` object holds all optional metadata. It can appear on both steps and
paths.

| Field        | Description                                        |
| ------------ | -------------------------------------------------- |
| `intent`     | Human-readable description of purpose              |
| `refs`       | Links to issues, docs, reasoning                   |
| `actors`     | Actor definitions with identities and keys         |
| `signatures` | Cryptographic signatures for verification          |

#### Actor Definitions

`meta.actors` maps actor strings to full definitions:

```json
{
  "meta": {
    "actors": {
      "human:alex": {
        "name": "Alex Kesling",
        "identities": [
          {"system": "github", "id": "akesling"},
          {"system": "twitter", "id": "AlexKesling"},
          {"system": "email", "id": "alex@empathic.dev"},
          {"system": "orcid", "id": "0000-0001-2345-6789"}
        ],
        "keys": [
          {
            "type": "gpg",
            "fingerprint": "ABCD 1234 5678 90EF GHIJ",
            "href": "https://keys.openpgp.org/vks/v1/by-fingerprint/ABCD1234567890EFGHIJ"
          },
          {
            "type": "ssh",
            "fingerprint": "SHA256:abcdef123456789...",
            "href": "https://github.com/akesling.keys"
          }
        ]
      },
      "agent:claude-code": {
        "name": "Claude Code",
        "provider": "anthropic",
        "model": "claude-sonnet-4-20250514",
        "identities": [
          {"system": "anthropic", "id": "claude-code/1.0.0"}
        ]
      },
      "tool:rustfmt": {
        "name": "rustfmt",
        "identities": [
          {"system": "crates.io", "id": "rustfmt-nightly/1.7.0"},
          {"system": "github", "id": "rust-lang/rustfmt"}
        ]
      }
    }
  }
}
```

Actor definitions can be provided at path level (shared by all steps) or step
level (for one-offs or overrides).

#### Signatures

`meta.signatures` holds cryptographic signatures for verification:

```json
{
  "meta": {
    "signatures": [
      {
        "signer": "human:alex",
        "key": "gpg:ABCD1234567890EFGHIJ",
        "scope": "author",
        "timestamp": "2026-01-29T15:30:00Z",
        "sig": "-----BEGIN PGP SIGNATURE-----\n..."
      },
      {
        "signer": "human:bob",
        "key": "gpg:WXYZ9876543210",
        "scope": "reviewer",
        "timestamp": "2026-01-29T16:00:00Z",
        "sig": "-----BEGIN PGP SIGNATURE-----\n..."
      }
    ]
  }
}
```

| Field       | Description                                      |
| ----------- | ------------------------------------------------ |
| `signer`    | Actor reference (must be defined in actors)      |
| `key`       | Key identifier (type:fingerprint)                |
| `scope`     | Role: `author`, `reviewer`, `witness`, `ci`, etc.|
| `timestamp` | When the signature was created                   |
| `sig`       | The signature data                               |

Signatures can appear on both steps and paths, enabling:
- Author signatures on individual steps
- Reviewer signatures on paths (PR approval)
- CI attestation signatures
- Witness signatures for audit trails

### Path Tree Structure

Steps form a DAG via their `parents` references:

```
              ┌─ step-3a ─ step-4a  (dead end)
step-1 ─ step-2 ─┤
              └─ step-3b ─ step-4b ─ step-5b  (head)
```

Merges create steps with multiple parents:

```
step-1 ─ step-2a ─ step-3a ─┐
                            ├─ step-4 (merge) ─ step-5 (head)
step-1 ─ step-2b ─ step-3b ─┘
```

The path tree preserves abandoned branches for later reflection. The `path.head`
indicates which step is the current tip.

## Format Specification

### Step Object

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
        "raw": "@@ -1,5 +1,25 @@\n use std::error::Error;\n+...",
        "structural": {
          "type": "rust.add_items",
          "items": [
            {"kind": "struct", "name": "ValidationError"},
            {"kind": "fn", "name": "validate_email"}
          ]
        }
      },
      "src/auth/mod.rs": {
        "raw": "@@ -1,1 +1,2 @@\n+pub mod validator;"
      }
    },

    "meta": {
      "intent": "Add email validation to prevent malformed input",
      "refs": [
        {"rel": "fixes", "href": "issue://github/repo/issues/42"},
        {"rel": "implements", "href": "doc://design/input-validation.md"}
      ]
    }
  }
}
```

### Minimal Step

A step can be minimal:

```json
{
  "Step": {
    "step": {
      "id": "step-001",
      "actor": "human:alex",
      "timestamp": "2026-01-29T10:00:00Z"
    },

    "change": {
      "src/main.rs": {
        "raw": "@@ -12,1 +12,1 @@\n-    println!(\"Hello world\");\n+    println!(\"Hello, world!\");"
      }
    }
  }
}
```

No parents (it's the first step). No meta. One file, one perspective. Still valid.

### Signed Step

A step with full actor and signature metadata:

```json
{
  "Step": {
    "step": {
      "id": "step-001",
      "actor": "human:alex",
      "timestamp": "2026-01-29T10:00:00Z"
    },

    "change": {
      "src/main.rs": {
        "raw": "@@ -12,1 +12,1 @@\n-    println!(\"Hello world\");\n+    println!(\"Hello, world!\");"
      }
    },

    "meta": {
      "intent": "Fix greeting punctuation",
      "actors": {
        "human:alex": {
          "name": "Alex Kesling",
          "identities": [{"system": "github", "id": "akesling"}],
          "keys": [{"type": "gpg", "fingerprint": "ABCD1234..."}]
        }
      },
      "signatures": [
        {
          "signer": "human:alex",
          "key": "gpg:ABCD1234",
          "scope": "author",
          "sig": "-----BEGIN PGP SIGNATURE-----\n..."
        }
      ]
    }
  }
}
```

### Path Object

A path collects steps and provides context:

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
        "step": { "id": "step-001", "actor": "agent:claude-code", "timestamp": "..." },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Add validation struct" }
      },
      {
        "step": { "id": "step-002", "parents": ["step-001"], "actor": "tool:rustfmt", "timestamp": "..." },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Auto-format" }
      },
      {
        "step": { "id": "step-001a", "parents": ["step-001"], "actor": "agent:claude-code", "timestamp": "..." },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Regex approach (abandoned)" }
      },
      {
        "step": { "id": "step-003", "parents": ["step-002"], "actor": "human:alex", "timestamp": "..." },
        "change": { "src/validator.rs": { "raw": "..." } },
        "meta": { "intent": "Refine error messages" }
      }
    ],

    "meta": {
      "title": "Add email validation",
      "source": "github:myorg/myrepo/pull/42",
      "actors": {
        "human:alex": {
          "name": "Alex Kesling",
          "identities": [{"system": "github", "id": "akesling"}],
          "keys": [{"type": "gpg", "fingerprint": "ABCD1234..."}]
        },
        "agent:claude-code": {
          "name": "Claude Code",
          "provider": "anthropic"
        },
        "tool:rustfmt": {
          "name": "rustfmt",
          "identities": [{"system": "crates.io", "id": "rustfmt-nightly/1.7.0"}]
        }
      },
      "signatures": [
        {"signer": "human:alex", "key": "gpg:ABCD1234", "scope": "author", "sig": "..."},
        {"signer": "human:bob", "key": "gpg:EFGH5678", "scope": "reviewer", "sig": "..."}
      ]
    }
  }
}
```

The path provides:
- **base**: Where this tree branches from (repo + ref + commit)
- **head**: Current tip of the active path
- **steps**: All steps including dead ends (step-001a has no descendants)
- **meta**: Path-level metadata including actors and signatures

### Base Context

The `path.base` object anchors the path to a specific state. The `uri` field
determines the type of base:

| Field  | Description                                           |
| ------ | ----------------------------------------------------- |
| `uri`  | Repository or toolpath reference                      |
| `ref`  | VCS state identifier (commit, revision, tag, etc.)    |

#### VCS Base

For paths branching from a VCS state:

```json
{
  "base": {
    "uri": "github:myorg/myrepo",
    "ref": "abc123def456"
  }
}
```

The `ref` field holds whatever identifier the VCS uses for a specific state:
- Git: commit hash, tag, or branch name
- SVN: revision number
- Mercurial: changeset ID
- Fossil: checkin hash

#### Toolpath Base

For paths branching from a step in another path (within the same graph):

```json
{
  "base": {
    "uri": "toolpath:path-main/step-005"
  }
}
```

The `toolpath:` URI format is:
- `toolpath:<path-id>/<step-id>` - branch from a specific step in a path

When using a toolpath URI, the `ref` field is omitted. This enables pure
Toolpath documents without VCS backing, and allows branching from steps that
occur between VCS commits.

#### Local/Filesystem Base

```json
{
  "base": {
    "uri": "file:///home/alex/projects/myrepo",
    "ref": "abc123def456"
  }
}
```

### Graph Object

A graph collects related paths:

```json
{
  "Graph": {
    "graph": {
      "id": "graph-release-v2"
    },

    "paths": [
      {
        "path": { "id": "path-pr-42", "base": {...}, "head": "step-004" },
        "steps": [...],
        "meta": { "title": "Add email validation" }
      },
      {
        "path": { "id": "path-pr-43", "base": {...}, "head": "step-003" },
        "steps": [...],
        "meta": { "title": "Fix authentication bug" }
      },
      { "$ref": "https://archive.example.com/toolpath/path-pr-44.json" },
      { "$ref": "toolpath://internal/path-pr-45" }
    ],

    "meta": {
      "title": "Release v2.0",
      "refs": [
        {"rel": "milestone", "href": "issue://github/myorg/myrepo/milestone/5"}
      ],
      "actors": {...},
      "signatures": [
        {"signer": "human:release-manager", "key": "gpg:...", "scope": "release", "sig": "..."}
      ]
    }
  }
}
```

The graph provides:
- **id**: Graph identifier
- **paths**: Inline path objects or `$ref` references to external paths
- **meta**: Graph-level metadata, actors, and signatures

References use `$ref` (borrowed from JSON Schema) and can point to:
- URLs: `https://...`, `s3://...`
- Local files: `file:///...`
- Named archives: `toolpath://archive-name/path-id`

## Signature Algorithm

Signatures require a canonical byte representation. Toolpath uses **JCS (RFC
8785)** - JSON Canonicalization Scheme.

### Canonicalization Rules

To produce a signable byte sequence:

1. **Key Ordering**: Object keys sorted lexicographically by UTF-8 bytes
2. **No Whitespace**: No spaces, newlines, or indentation
3. **Unicode**: Literal characters when valid; escape only `"`, `\`, and control
   characters (U+0000-U+001F); lowercase hex in escapes (`\u001f` not `\u001F`)
4. **Numbers**: No leading zeros, no trailing zeros, no positive sign, no
   unnecessary exponential notation
5. **Strings**: UTF-8 encoded, minimal escaping

### What Gets Signed

#### Step Signature (Author)

Signs the inner `step` + `change` fields (excluding both `meta` and the
`"Step"` document wrapper):

```
canonical_input = JCS({
  "change": <change object>,
  "step": <step object>
})

digest = SHA-256(canonical_input)
signature = sign(digest, private_key)
```

This attests: "I authored this change."

#### Path Signature (Author)

Signs `path` + ordered step IDs:

```
canonical_input = JCS({
  "path": <path object>,
  "step_ids": [<step.id for each step in steps array, in order>]
})

digest = SHA-256(canonical_input)
signature = sign(digest, private_key)
```

This attests: "I authored this path containing these steps in this order."

#### Path Signature (Reviewer)

Signs path identity + head at review time:

```
canonical_input = JCS({
  "head": <path.head>,
  "path_id": <path.id>,
  "reviewed_at": <signature timestamp>
})

digest = SHA-256(canonical_input)
signature = sign(digest, private_key)
```

This attests: "I reviewed this path at this head and approve it."

### Canonicalization Example

Input document:

```json
{
  "Step": {
    "step": {
      "timestamp": "2026-01-29T10:00:00Z",
      "id": "step-001",
      "actor": "human:alex"
    },
    "change": {
      "src/main.rs": {
        "raw": "@@ -1,1 +1,1 @@\n-hello\n+world"
      }
    },
    "meta": { "intent": "Fix greeting" }
  }
}
```

Signing operates on the **inner** step content (without the `"Step"` wrapper).

Canonical form (for signing):

```
{"change":{"src/main.rs":{"raw":"@@ -1,1 +1,1 @@\n-hello\n+world"}},"step":{"actor":"human:alex","id":"step-001","timestamp":"2026-01-29T10:00:00Z"}}
```

Note:
- `meta` excluded (signatures live there)
- Keys sorted: `change` < `step`; `actor` < `id` < `timestamp`
- No whitespace between tokens

### Verification Algorithm

```
verify_step_signature(step_document, signature, public_key):
  1. Reconstruct canonical form: JCS({"change": ..., "step": ...})
  2. Compute digest: SHA-256(canonical_form)
  3. Verify: signature_verify(digest, signature.sig, public_key)

verify_path_signatures(path_document, required_scopes):
  1. For each required scope (e.g., ["author", "reviewer"]):
     a. Find signature in meta.signatures with matching scope
     b. Look up signer's public key in meta.actors[signer].keys
     c. Reconstruct canonical form for that scope type
     d. Verify signature against canonical form
  2. Optionally verify each step's author signature
  3. Return success only if all required signatures verify
```

### Supported Key Types

| Type      | Description                        | Signature Format           |
| --------- | ---------------------------------- | -------------------------- |
| `gpg`     | OpenPGP/GPG key                    | ASCII-armored PGP signature|
| `ssh`     | SSH key (Ed25519, RSA, etc.)       | SSH signature format       |
| `sigstore`| Sigstore keyless signing           | Sigstore bundle            |

## Use Cases

### 1. Representing a PR

A PR is a path rooted at the target branch:

```json
{
  "Path": {
    "path": {
      "id": "pr-123",
      "base": { "uri": "github:myorg/myrepo", "ref": "abc123" },
      "head": "step-final"
    },
    "steps": [...],
    "meta": {
      "title": "Add email validation",
      "source": "github:myorg/myrepo/pull/123"
    }
  }
}
```

### 2. Blame with Noise Filtering

Given a path, blame can skip over formatting-only steps by checking:

- Actor type (`tool:rustfmt` vs `human:*` vs `agent:*`)
- Whether `structural` perspective exists (no structural change = formatting)

### 3. Actor-Filtered History

"Show me everything Claude did to this file":

```
filter(path, actor: "agent:claude-code", artifact: "src/auth/validator.rs")
```

### 4. Exploring Dead Ends

"What approaches did we try that didn't work?":

```
dead_ends(path, artifact: "src/auth/*")
```

Returns steps that have no descendants leading to head.

### 5. Provenance Chain

"How did we get here?":

```
ancestors(path, from: path.head)
```

Returns the linear sequence of steps from first step to head.

### 6. Signature Verification

"Verify this path was authored and reviewed by trusted parties":

```
verify(path,
  require: [
    {scope: "author", trusted_keys: [...]},
    {scope: "reviewer", trusted_keys: [...]}
  ]
)
```

### 7. Identity Resolution

"Who is human:alex?":

```
resolve_actor(path, "human:alex")
→ {name: "Alex Kesling", github: "akesling", ...}
```

## Relationship to Version Control Systems

Toolpath is VCS-agnostic. It complements git, jj, Mercurial, Darcs, Fossil, and
other version control systems by capturing information they don't:

| VCS Concept        | Toolpath Equivalent | What Toolpath Adds                    |
| ------------------ | ------------------- | ------------------------------------- |
| Commit             | Step                | Structured intent, refs, multi-perspective |
| Branch             | Path                | Base context, dead end preservation   |
| Commit message     | `meta.intent`       | Machine-readable, linkable            |
| GPG signature      | `meta.signatures`   | Multi-party, scoped (author/reviewer) |
| Author identity    | `meta.actors`       | Rich identity, multiple systems       |

**What Toolpath adds beyond any VCS:**

1. **Finer granularity**: Multiple steps can occur between VCS commits
2. **Abandoned paths**: VCS tools lose deleted branches; Toolpath preserves dead
   ends for reflection
3. **Multi-perspective changes**: Raw diff + structural AST ops + semantic intent
4. **Actor provenance**: Link actors to external identities (GitHub, email, etc.)
5. **Multi-party signatures**: Author, reviewer, CI attestation on same artifact
6. **Tool-agnostic history**: Capture changes from AI agents, formatters, linters
   that may not create VCS commits

**Deriving Toolpath from VCS history:**

A VCS commit can become a Toolpath step. The `path.base` references the VCS
state:

```json
{
  "base": {
    "uri": "github:myorg/myrepo",   // or "hg:", "fossil:", "file:", etc.
    "ref": "abc123def456"           // commit hash, revision number, etc.
  }
}
```

Toolpath doesn't replace your VCS—it layers richer provenance on top.

## Schema

A JSON Schema for validating Toolpath documents is available at
[schema/toolpath.schema.json](./schema/toolpath.schema.json).

The schema validates all three externally tagged document types (`Step`, `Path`,
`Graph`) and enforces:
- Required fields and structure
- Actor reference format (`type:name`)
- Timestamp format (ISO 8601)
- Signature scopes (`author`, `reviewer`, `witness`, `ci`, `release`)
- Key types (`gpg`, `ssh`, `sigstore`)

## FAQ and Open Questions

See [FAQ.md](./FAQ.md) for design rationale, frequently asked questions, and
open design decisions.

## Prior Art

- **Git**: Content-addressable storage, DAG of commits, GPG signing
- **Mercurial Evolve**: Changeset evolution, obsolescence markers
- **W3C PROV**: General provenance data model
- **OpenLineage**: Data pipeline lineage
- **Sigstore**: Keyless signing for software artifacts
- **in-toto**: Software supply chain integrity
- **JCS (RFC 8785)**: JSON Canonicalization Scheme for deterministic serialization
