---
layout: base.njk
title: Toolpath
nav: home
---

<div class="hero">
  <div class="hero-content">
    <h1>Toolpath</h1>
    <p class="tagline">
      <strong>What happens between commits?</strong> Toolpath records the
      decisions that get lost at merge time.  Record, transform, and analyze
      sessions in a tool agnostic way.
    </p>
    <div class="hero-install">
      <div class="install-option">
        <span class="install-label">Quick install the <span class="cli-name">path</span> CLI</span>
        <div class="install-cmd-line">
          <code class="install-cmd"><span class="prompt">$ </span>curl --proto '=https' --tlsv1.2 -fsS \
https://toolpath.net/install.sh | bash</code>
          <button class="copy-btn" type="button" data-copy="curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash" aria-label="Copy command to clipboard">
            <svg class="copy-icon" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
            <svg class="check-icon" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>
          </button>
        </div>
      </div>
      <div class="install-option">
        <span class="install-label">From crates.io</span>
        <div class="install-cmd-line">
          <code class="install-cmd"><span class="prompt">$ </span>cargo install path-cli</code>
          <button class="copy-btn" type="button" data-copy="cargo install path-cli" aria-label="Copy command to clipboard">
            <svg class="copy-icon" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
            <svg class="check-icon" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>
          </button>
        </div>
      </div>
      <button id="try-it-btn" class="try-it-btn">Try in browser</button>
    </div>
  </div>
  <svg class="topo topo-hero" viewBox="0 0 380 320" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <style>
      .topo-accent { stroke: var(--accent); }
      .topo-pencil { stroke: var(--text-secondary); }
      .topo-accent-fill { fill: var(--accent); }
    </style>
    <ellipse cx="190" cy="160" rx="170" ry="140" class="topo-accent" stroke-width="1" opacity="0.12"/>
    <ellipse cx="200" cy="155" rx="140" ry="115" class="topo-accent" stroke-width="1" opacity="0.16"/>
    <ellipse cx="208" cy="148" rx="112" ry="90" class="topo-accent" stroke-width="1" opacity="0.20"/>
    <ellipse cx="214" cy="142" rx="85" ry="68" class="topo-accent" stroke-width="1" opacity="0.25"/>
    <ellipse cx="218" cy="138" rx="60" ry="48" class="topo-accent" stroke-width="1.2" opacity="0.30"/>
    <ellipse cx="221" cy="135" rx="38" ry="30" class="topo-accent" stroke-width="1.2" opacity="0.38"/>
    <ellipse cx="223" cy="133" rx="18" ry="14" class="topo-accent" stroke-width="1.5" opacity="0.45"/>
    <circle cx="224" cy="132" r="4" class="topo-accent-fill" opacity="0.35"/>
    <!-- secondary peak -->
    <ellipse cx="120" cy="210" rx="80" ry="65" class="topo-pencil" stroke-width="1" opacity="0.10"/>
    <ellipse cx="125" cy="205" rx="55" ry="44" class="topo-pencil" stroke-width="1" opacity="0.14"/>
    <ellipse cx="128" cy="201" rx="32" ry="26" class="topo-pencil" stroke-width="1" opacity="0.18"/>
    <ellipse cx="130" cy="199" rx="14" ry="11" class="topo-pencil" stroke-width="1" opacity="0.22"/>
  </svg>
</div>
<div class="divider"></div>

<div id="playground-section" class="playground" hidden>
<h2>Try it</h2>
<p class="playground-desc">
Explore Toolpath documents in your browser. Real <code>path</code> commands, real output.
</p>
<script>window.__PLAYGROUND_FILES__ = {{ playgroundFiles | dump | safe }};</script>
<div id="playground-terminal" class="playground-terminal"></div>
</div>
<script src="/wasm/path.js"></script>
<script src="/js/playground.js"></script>
<script src="/js/copy-buttons.js"></script>

## The problem

When Claude writes code, `rustfmt` reformats it, and a human refines it, git
blame attributes everything to the human's commit. The actual provenance is
lost. Dead ends disappear. Tool contributions collapse into whoever typed `git
commit`.

Toolpath records **who** changed **what**, **why**, what they tried that didn't
work, and how to verify all of it.

<div class="scenarios">
  <h2>When you need it</h2>
  <div class="objects">
    <div class="object-card">
      <h3>Multi-actor PR</h3>
      <p>Claude wrote the implementation, rustfmt reformatted, you refined the
      error messages. Toolpath gives each actor their own step so reviewers see
      who did what.</p>
    </div>
    <div class="object-card">
      <h3>Rotated AI session</h3>
      <p>Claude Code hit context limits mid-task and rotated to a new session.
      Toolpath chains the segments together so no work is lost.</p>
    </div>
    <div class="object-card">
      <h3>Release lineage</h3>
      <p>Three teams contributed PRs to the release. Toolpath merges the
      provenance into a single Graph so you can trace any line back to the
      intent behind it.</p>
    </div>
  </div>
</div>

## Three core objects

<div class="objects">
  <div class="object-card">
    <h3>Step</h3>
    <p>A single change to artifact(s) by one actor. One commit, one edit, one
    format pass.</p>
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

No parents (it's the first step). No meta. One file, one perspective. Every document is a Graph at the root — single-step documents like this one are a Graph holding one Path holding one Step.

## Quick start

```bash
# Install
cargo install path-cli

# Import provenance from this repo's git history (top-level surface is
# the porcelain; plumbing lives under `path p …`)
path p import git --repo . --branch main --no-cache --pretty

# Visualize it
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png

# Import from Claude conversation logs
path p import claude --project /path/to/project --no-cache --pretty

# Query for dead ends
path query dead-ends --input doc.json

# Filter by actor
path query filter --input doc.json --actor "agent:"
```

<svg class="topo topo-wide" viewBox="0 0 900 80" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <style>.topo-accent{stroke:var(--accent);}.topo-pencil{stroke:var(--text-secondary);}</style>
  <path d="M0,55 Q80,20 200,45 Q320,70 450,30 Q580,0 700,50 Q800,75 900,40" class="topo-accent" stroke-width="1" opacity="0.18" fill="none"/>
  <path d="M0,60 Q90,30 210,52 Q340,74 460,38 Q590,8 710,55 Q810,78 900,48" class="topo-accent" stroke-width="1" opacity="0.13" fill="none"/>
  <path d="M0,65 Q100,40 220,58 Q350,76 470,44 Q600,14 720,58 Q815,80 900,54" class="topo-pencil" stroke-width="1" opacity="0.12" fill="none"/>
</svg>

## Workspace

Toolpath is a Rust workspace of focused crates:

| Crate                                                | What it does                           |
| ---------------------------------------------------- | -------------------------------------- |
| [`toolpath`](https://docs.rs/toolpath)               | Core types, builders, query API        |
| [`toolpath-convo`](https://docs.rs/toolpath-convo)   | Provider-agnostic conversation traits  |
| [`toolpath-git`](https://docs.rs/toolpath-git)       | Derive from git history                |
| [`toolpath-github`](https://docs.rs/toolpath-github) | Derive from GitHub pull requests       |
| [`toolpath-claude`](https://docs.rs/toolpath-claude) | Derive from Claude conversations       |
| [`toolpath-dot`](https://docs.rs/toolpath-dot)       | Graphviz DOT visualization             |
| [`path-cli`](https://docs.rs/path-cli)               | Unified CLI (`cargo install path-cli`) |

See [Crates](/crates/) for details, or [docs.rs](https://docs.rs/toolpath) for API reference.
