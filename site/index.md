---
layout: base.njk
title: Toolpath
nav: home
---

<div class="hero">
  <div class="hero-content">
    <h1>Toolpath</h1>
    <p class="tagline">
      Know your tools. A tool-agnostic format for tracking artifact transformation provenance.
      Git blame, but for everything that happens to code &mdash; including the stuff git doesn't see.
    </p>
    <div class="hero-install">
      <span class="prompt">$ </span>cargo install toolpath-cli
    </div>
  </div>
  <svg class="topo topo-hero" viewBox="0 0 380 320" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <ellipse cx="190" cy="160" rx="170" ry="140" stroke="#b5652b" stroke-width="1" opacity="0.12"/>
    <ellipse cx="200" cy="155" rx="140" ry="115" stroke="#b5652b" stroke-width="1" opacity="0.16"/>
    <ellipse cx="208" cy="148" rx="112" ry="90" stroke="#b5652b" stroke-width="1" opacity="0.20"/>
    <ellipse cx="214" cy="142" rx="85" ry="68" stroke="#b5652b" stroke-width="1" opacity="0.25"/>
    <ellipse cx="218" cy="138" rx="60" ry="48" stroke="#b5652b" stroke-width="1.2" opacity="0.30"/>
    <ellipse cx="221" cy="135" rx="38" ry="30" stroke="#b5652b" stroke-width="1.2" opacity="0.38"/>
    <ellipse cx="223" cy="133" rx="18" ry="14" stroke="#b5652b" stroke-width="1.5" opacity="0.45"/>
    <circle cx="224" cy="132" r="4" fill="#b5652b" opacity="0.35"/>
    <!-- secondary peak -->
    <ellipse cx="120" cy="210" rx="80" ry="65" stroke="#8a8078" stroke-width="1" opacity="0.10"/>
    <ellipse cx="125" cy="205" rx="55" ry="44" stroke="#8a8078" stroke-width="1" opacity="0.14"/>
    <ellipse cx="128" cy="201" rx="32" ry="26" stroke="#8a8078" stroke-width="1" opacity="0.18"/>
    <ellipse cx="130" cy="199" rx="14" ry="11" stroke="#8a8078" stroke-width="1" opacity="0.22"/>
  </svg>
</div>
<div class="divider"></div>

## The problem

When Claude writes code, `rustfmt` reformats it, and a human refines it, git blame attributes everything to the human's commit. The actual provenance is lost. Dead ends disappear. Tool contributions collapse into whoever typed `git commit`.

Toolpath records **who** changed **what**, **why**, what they tried that didn't work, and how to verify all of it.

## Three core objects

<div class="objects">
  <div class="object-card">
    <h3>Step</h3>
    <p>A single change to artifact(s) by one actor. One commit, one edit, one format pass.</p>
  </div>
  <div class="object-card">
    <h3>Path</h3>
    <p>A DAG of steps with a base context. A PR, a coding session, a branch.</p>
  </div>
  <div class="object-card">
    <h3>Graph</h3>
    <p>A collection of related paths. A release, a sprint, a project.</p>
  </div>
</div>

Steps form a DAG via parent references. Dead ends are implicit: steps not in the ancestry of `path.head`.

<div class="dag-figure">
<svg class="dag-svg" viewBox="0 0 780 200" fill="none" xmlns="http://www.w3.org/2000/svg" aria-label="DAG diagram showing steps with a dead-end branch and a head branch">
<line x1="108" y1="100" x2="172" y2="100" stroke="#2d2a26" stroke-width="2"/>
<path d="M280,100 L310,100 Q320,100 320,90 L320,45 Q320,35 330,35 L352,35" stroke="#8a8078" stroke-width="1.5" stroke-dasharray="6 3" fill="none"/>
<path d="M280,100 L310,100 Q320,100 320,110 L320,165 Q320,175 330,175 L352,175" stroke="#2d2a26" stroke-width="2" fill="none"/>
<line x1="460" y1="35" x2="532" y2="35" stroke="#8a8078" stroke-width="1.5" stroke-dasharray="6 3"/>
<line x1="460" y1="175" x2="532" y2="175" stroke="#2d2a26" stroke-width="2"/>
<line x1="640" y1="175" x2="672" y2="175" stroke="#2d2a26" stroke-width="2"/>
<rect x="0" y="78" width="108" height="44" fill="#b5652b18" stroke="#b5652b" stroke-width="1.5"/>
<text x="54" y="97" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-1</text>
<text x="54" y="112" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">human:alex</text>
<rect x="172" y="78" width="108" height="44" fill="#b5652b30" stroke="#b5652b" stroke-width="1.5"/>
<text x="226" y="97" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-2</text>
<text x="226" y="112" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">agent:claude</text>
<rect x="352" y="13" width="108" height="44" fill="#c4403018" stroke="#c44030" stroke-width="1.5" stroke-dasharray="5 3"/>
<text x="406" y="32" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-3a</text>
<text x="406" y="47" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">agent:claude</text>
<rect x="532" y="13" width="108" height="44" fill="#c4403018" stroke="#c44030" stroke-width="1.5" stroke-dasharray="5 3"/>
<text x="586" y="32" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-4a</text>
<text x="586" y="47" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">agent:claude</text>
<rect x="352" y="153" width="108" height="44" fill="#b5652b30" stroke="#b5652b" stroke-width="1.5"/>
<text x="406" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-3b</text>
<text x="406" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">tool:rustfmt</text>
<rect x="532" y="153" width="108" height="44" fill="#b5652b18" stroke="#b5652b" stroke-width="1.5"/>
<text x="586" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="600" fill="#2d2a26">step-4b</text>
<text x="586" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">human:alex</text>
<rect x="672" y="153" width="108" height="44" fill="#b5652b18" stroke="#b5652b" stroke-width="3"/>
<text x="726" y="172" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="11" font-weight="700" fill="#2d2a26">step-5b</text>
<text x="726" y="187" text-anchor="middle" font-family="IBM Plex Mono, monospace" font-size="9" fill="#8a8078">human:alex</text>
<text x="586" y="72" text-anchor="middle" font-family="IBM Plex Sans Condensed, sans-serif" font-size="10" font-weight="600" fill="#c44030" letter-spacing="0.05em">DEAD END</text>
<text x="726" y="146" text-anchor="middle" font-family="IBM Plex Sans Condensed, sans-serif" font-size="10" font-weight="600" fill="#b5652b" letter-spacing="0.05em">HEAD</text>
</svg>
</div>

## What Toolpath adds

| What                   | Git                         | Toolpath                                         |
| ---------------------- | --------------------------- | ------------------------------------------------ |
| Who made the change    | Single author per commit    | Typed actors: `human:`, `agent:`, `tool:`, `ci:` |
| Why they changed it    | Unstructured commit message | `meta.intent` + linked refs                      |
| Abandoned approaches   | Lost when branch is deleted | Dead ends preserved in the DAG                   |
| Multi-actor provenance | Collapsed into one commit   | Each actor gets their own step                   |
| Verification           | GPG on whole commit         | Scoped signatures: author, reviewer, CI          |
| Granularity            | Commit-level                | Sub-commit: multiple steps between commits       |

## Minimal example

A valid Toolpath document can be tiny:

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

## Quick start

```bash
# Install
cargo install toolpath-cli

# Derive provenance from this repo's git history
path derive git --repo . --branch main --pretty

# Visualize it
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png

# Derive from Claude conversation logs
path derive claude --project /path/to/project --pretty

# Query for dead ends
path query dead-ends --input doc.json

# Filter by actor
path query filter --input doc.json --actor "agent:"
```

<svg class="topo topo-wide" viewBox="0 0 900 80" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <path d="M0,55 Q80,20 200,45 Q320,70 450,30 Q580,0 700,50 Q800,75 900,40" stroke="#b5652b" stroke-width="1" opacity="0.12" fill="none"/>
  <path d="M0,60 Q90,30 210,52 Q340,74 460,38 Q590,8 710,55 Q810,78 900,48" stroke="#b5652b" stroke-width="1" opacity="0.09" fill="none"/>
  <path d="M0,65 Q100,40 220,58 Q350,76 470,44 Q600,14 720,58 Q815,80 900,54" stroke="#8a8078" stroke-width="1" opacity="0.08" fill="none"/>
</svg>

## Workspace

Toolpath is a Rust workspace of focused crates:

| Crate                                                | What it does                               |
| ---------------------------------------------------- | ------------------------------------------ |
| [`toolpath`](https://docs.rs/toolpath)               | Core types, builders, query API            |
| [`toolpath-git`](https://docs.rs/toolpath-git)       | Derive from git history                    |
| [`toolpath-claude`](https://docs.rs/toolpath-claude) | Derive from Claude conversations           |
| [`toolpath-dot`](https://docs.rs/toolpath-dot)       | Graphviz DOT visualization                 |
| [`toolpath-cli`](https://docs.rs/toolpath-cli)       | Unified CLI (`cargo install toolpath-cli`) |

See [Crates](/crates/) for details, or [docs.rs](https://docs.rs/toolpath) for API reference.
