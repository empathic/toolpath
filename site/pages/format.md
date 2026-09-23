---
layout: base.njk
title: Format
nav: format
permalink: /format/
---

# Toolpath at a glance

<p class="subtitle">
A format for what happens between commits.
</p>

Yesterday's tooling was a footnote. You manually ran `rustfmt` and it was incidental to the actual change. Coding agents now make a majority of the decisions on a PR — what to write, when to abandon an approach, how to phrase the test. That's a new class of information about how artifacts come to be: dense, branching, multi-actor. Nothing was built to capture it.

Git stores the final snapshot. Chat logs live in proprietary formats. Tool decisions vanish into telemetry. There's no neutral format for **saving** what actually happened, **transforming** it between systems, or **analyzing** it after the fact.

Toolpath is that format.

## The shape

> _A story is made up of many paths, one step at a time._

Every document is a **Graph**. A Graph holds **Paths**. A Path holds **Steps**. A Step is one **transformation**.

| Layer   | Holds      | Represents                                               |
| ------- | ---------- | -------------------------------------------------------- |
| `graph` | `paths[]`  | A collection of stories — a release, a project, a bundle |
| `path`  | `steps[]`  | One story — a PR, a coding session, a branch             |
| `step`  | `change{}` | One transformation — touching one or more artifacts      |

`Step` and `Path` are inner types — they appear inside `graph.paths` and `path.steps`, never as the JSON root on their own. Even a document recording a single change is a Graph that holds one Path that holds one Step. One root type, one parser path.

A step's `change` maps artifact URLs to perspectives — a unified diff under `raw`, a structural AST operation under `structural`, or both. The `meta` object is optional at every level: minimal documents need only `step` and `change`.

## What a document looks like

A complete Toolpath document is small enough to read in one breath:

```json
{
  "graph": { "id": "graph-step-001" },
  "paths": [
    {
      "path": { "id": "path-step-001", "head": "step-001" },
      "steps": [
        {
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
      ]
    }
  ]
}
```

That's the canonical fixture [`step-01-minimal.json`](https://github.com/empathic/toolpath/blob/main/examples/step-01-minimal.json) — one author, one timestamp, one file changed, one diff. Every Toolpath document looks like this. The objects nest the same way. Bigger documents just hold more of them.

## Steps form a DAG

Steps form a DAG via parent references. Dead ends are implicit: steps not in the
ancestry of `path.head`.

<div class="dag-figure">
<span class="fig-label">FIG_001 &nbsp; STEP DAG</span>
<svg class="dag-svg" viewBox="0 0 780 200" fill="none" xmlns="http://www.w3.org/2000/svg" aria-label="DAG diagram showing steps with a dead-end branch and a head branch">
<style>
  .e-active { stroke: var(--text); }
  .e-inactive { stroke: var(--text-secondary); }
  .e-base { stroke: var(--accent); }
  .n-fill-human { fill: var(--accent); fill-opacity: 0.18; }
  .n-fill-agent { fill: var(--accent); fill-opacity: 0.30; }
  .n-fill-dead { fill: var(--alert); fill-opacity: 0.18; }
  .n-stroke-accent { stroke: var(--accent); }
  .n-stroke-dead { stroke: var(--alert); }
  .t-text { fill: var(--text); }
  .t-secondary { fill: var(--text-secondary); }
  .t-accent { fill: var(--accent); }
  .t-alert { fill: var(--alert); }
</style>
<line x1="108" y1="100" x2="172" y2="100" class="e-active" stroke-width="2"/>
<path d="M280,100 L310,100 Q320,100 320,90 L320,45 Q320,35 330,35 L352,35" class="e-inactive" stroke-width="1.5" stroke-dasharray="6 3" fill="none"/>
<path d="M280,100 L310,100 Q320,100 320,110 L320,165 Q320,175 330,175 L352,175" class="e-active" stroke-width="2" fill="none"/>
<line x1="460" y1="35" x2="532" y2="35" class="e-inactive" stroke-width="1.5" stroke-dasharray="6 3"/>
<line x1="460" y1="175" x2="532" y2="175" class="e-active" stroke-width="2"/>
<line x1="640" y1="175" x2="672" y2="175" class="e-active" stroke-width="2"/>
<rect x="0" y="78" width="108" height="44" class="n-fill-human n-stroke-accent" stroke-width="1.5"/>
<text x="54" y="97" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-1</text>
<text x="54" y="112" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">human:alex</text>
<rect x="172" y="78" width="108" height="44" class="n-fill-agent n-stroke-accent" stroke-width="1.5"/>
<text x="226" y="97" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-2</text>
<text x="226" y="112" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">agent:claude</text>
<rect x="352" y="13" width="108" height="44" class="n-fill-dead n-stroke-dead" stroke-width="1.5" stroke-dasharray="5 3"/>
<text x="406" y="32" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-3a</text>
<text x="406" y="47" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">agent:claude</text>
<rect x="532" y="13" width="108" height="44" class="n-fill-dead n-stroke-dead" stroke-width="1.5" stroke-dasharray="5 3"/>
<text x="586" y="32" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-4a</text>
<text x="586" y="47" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">agent:claude</text>
<rect x="352" y="153" width="108" height="44" class="n-fill-agent n-stroke-accent" stroke-width="1.5"/>
<text x="406" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-3b</text>
<text x="406" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">tool:rustfmt</text>
<rect x="532" y="153" width="108" height="44" class="n-fill-human n-stroke-accent" stroke-width="1.5"/>
<text x="586" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" class="t-text">step-4b</text>
<text x="586" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">human:alex</text>
<rect x="672" y="153" width="108" height="44" class="n-fill-human n-stroke-accent" stroke-width="3"/>
<text x="726" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="700" class="t-text">step-5b</text>
<text x="726" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" class="t-secondary">human:alex</text>
<text x="586" y="72" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="10" font-weight="600" class="t-alert" letter-spacing="0.08em">DEAD END</text>
<text x="726" y="146" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="10" font-weight="600" class="t-accent" letter-spacing="0.08em">HEAD</text>
</svg>
</div>

## A real example

The minimal example above shows the shape. To see what a Toolpath document looks like when actual work has happened, open the **exploration** fixture in the visualizer. It's a single Path with seven steps and the four DAG features that recur everywhere:

- **`path.base`** anchors the document to a starting commit. Bare paths in `change` (`src/main.rs`) are relative to that base.
- **`path.head`** points at the current tip — `step-004`. Walking back from the head gives the active history; everything else is exploration.
- **`step-002a`** and **`step-002b`** both branch from `step-001` — that's a **fork**. The Path keeps both, even though only one becomes part of the active history.
- **`step-002a`** is also a **dead end** — nothing on its descendant chain reaches `path.head`. Dead ends aren't marked anywhere in the document; they fall out structurally as `all_steps − ancestors(head)`.
- **`step-004`** lists two parents (`step-003b`, `step-003c`) — that's a **merge** of two parallel branches.

→ Open it in the [visualizer](/visualizer/) (it's the default example) and the structure clicks immediately.

## What Toolpath adds to git

| What                   | Git                         | Toolpath                                         |
| ---------------------- | --------------------------- | ------------------------------------------------ |
| Who made the change    | Single author per commit    | Typed actors: `human:`, `agent:`, `tool:`, `ci:` |
| Why they changed it    | Unstructured commit message | `meta.intent` + linked refs                      |
| Abandoned approaches   | Lost when branch is deleted | Dead ends preserved in the DAG                   |
| Multi-actor provenance | Collapsed into one commit   | Each actor gets their own step                   |
| Verification           | GPG on whole commit         | Scoped signatures: author, reviewer, CI          |
| Granularity            | Commit-level                | Sub-commit: multiple steps between commits       |

## File extensions

| Extension     | Shape             | Use it when                                                   |
| ------------- | ----------------- | ------------------------------------------------------------- |
| `.path.json`  | Graph (canonical) | Sealed documents — PRs, releases, archived sessions           |
| `.path.jsonl` | Graph (streaming) | Live capture — one Path appended line-by-line as work happens |

A `.path.jsonl` stream encodes exactly one inline Path and seals to a single-path Graph at the file boundary. Multi-path graphs and `$ref`-only entries can't be represented in JSONL — those require canonical `.path.json`.

<svg class="topo topo-wide" viewBox="0 0 900 70" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <style>.topo-accent{stroke:var(--accent);}.topo-pencil{stroke:var(--text-secondary);}</style>
  <path d="M0,35 Q150,10 300,40 Q450,65 600,25 Q750,0 900,35" class="topo-accent" stroke-width="1" opacity="0.16" fill="none"/>
  <path d="M0,40 Q160,18 310,45 Q460,68 610,32 Q760,5 900,42" class="topo-pencil" stroke-width="1" opacity="0.12" fill="none"/>
  <path d="M0,45 Q170,25 320,48 Q470,70 620,38 Q770,10 900,48" class="topo-accent" stroke-width="1" opacity="0.10" fill="none"/>
</svg>

## Where to next

- **[Full specification](/rfc/)** — the normative details: signatures, perspectives, the `meta` object, ID uniqueness, JSONL streaming.
- **[JSON Schema](https://github.com/empathic/toolpath/blob/main/schema/toolpath.schema.json)** — authoritative shape; what `path p validate` checks against.
- **[Examples](https://github.com/empathic/toolpath/tree/main/examples)** — the fixtures used throughout these docs and tested in CI.
- **[CLI](/cli/)** — `path share`, `path resume`, `path query`, and the `path p …` plumbing (`p import`, `p render`, `p validate`, …).
- **[Visualizer](/visualizer/)** — paste a document, see the DAG.
- **[Design notes](/faq/)** — why the format is shaped the way it is.
