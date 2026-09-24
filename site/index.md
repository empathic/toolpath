---
layout: base.njk
title: Toolpath
nav: home
---

<div class="hero">
  <div class="hero-content">
    <p class="hero-eyebrow">Open session format · Apache-2.0</p>
    <h1>The <span>Missing IR</span> for Agent Sessions</h1>
    <p class="tagline">
      Toolpath is an open, versioned format for coding-agent sessions.
      <strong>Parse any agent’s session in, project it out to any harness, and
      build your tools once for all of them.</strong>
    </p>
    <div class="hero-actions">
      <a class="hero-cta" href="/format/">Read the spec</a>
      <a class="hero-link" href="#install">Install <span class="cli-name">path</span> →</a>
      <button id="try-it-btn" type="button" class="hero-link">Try it in your browser</button>
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

<div id="playground-section" class="playground" hidden>
<h2>Try it</h2>
<p class="playground-desc">
Real <code>path</code> commands on example documents, running in your browser.
Nothing to install.
</p>
<script>window.__PLAYGROUND_FILES__ = {{ playgroundFiles | dump | safe }};</script>
<div id="playground-terminal" class="playground-terminal"></div>
</div>
<script src="/wasm/path.js"></script>
<script src="/js/playground.js"></script>
<script src="/js/copy-buttons.js"></script>

## Every agent writes its own private log

<div class="figure-block">
<span class="figure-label">FIG_001 · Where sessions live today</span>
<dl class="log-list">
  <div><dt>Claude Code</dt><dd><code>~/.claude/projects/…/*.jsonl</code>, rotating chains</dd></div>
  <div><dt>Codex CLI</dt><dd><code>~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl</code></dd></div>
  <div><dt>Gemini CLI</dt><dd><code>~/.gemini/tmp/…</code>, chat directories</dd></div>
  <div><dt>Pi</dt><dd><code>~/.pi/agent/sessions/</code></dd></div>
  <div><dt>Copilot, Cursor, opencode</dt><dd>their own layouts again</dd></div>
</dl>
</div>

None of these are documented, and any of them can change in the next
release. The session that produced a change is locked inside the
harness that ran it.

<p class="stakes"><strong>Whoever owns the format owns the record.</strong>
Switching tools, auditing a change, or building your own tooling all
depend on a file layout you don’t control.</p>

## Parse in, project out

Toolpath is a stable center with every agent at the edge. Harness
formats are undocumented and change without notice, so Toolpath treats
them as boundaries, and everything it does is one of three moves across
them.

<figure class="pp-figure" aria-label="Harness logs are parsed into one Toolpath document, then projected out to any agent or tool">
  <span class="figure-label">FIG_002 · One center, every edge</span>
  <div class="pp">
    <ul class="pp-side">
      <li>Claude Code</li><li>Codex CLI</li><li>Gemini CLI</li><li>opencode</li><li>Pi</li><li>Copilot CLI</li><li>Cursor</li>
    </ul>
    <div class="pp-arrow"><span aria-hidden="true">→</span>parse in</div>
    <div class="pp-doc">
      <span class="pp-doc-title">Toolpath document</span>
      <span><b>steps</b> who changed what</span>
      <span><b>meta.intent</b> why</span>
      <span><b>dead ends</b> what was tried</span>
      <span><b>usage</b> what it cost</span>
    </div>
    <div class="pp-arrow"><span aria-hidden="true">→</span>project out</div>
    <ul class="pp-side">
      <li>Claude Code</li><li>Codex CLI</li><li>…any writable agent</li>
      <li class="pp-tool">path query</li><li class="pp-tool">Pathbase</li><li class="pp-tool">your tool</li>
    </ul>
  </div>
</figure>

- **Parse in.** A session crosses the boundary once and becomes a
  stable document you can keep, query, and share.
- **Project out.** A document becomes the on-disk layout a target
  harness expects. Any writable harness, not just the one the session
  started in.
- **Resume.** A projection followed by a handoff: start in one agent,
  continue in another, with everything the last one knew.

<div class="scenarios">
  <h2>Build it once</h2>
  <p>One schema means tooling stops being per-agent. Build against the
  format and it works with sessions from every supported harness. When
  the next agent ships, one new parser brings it into every tool you
  already have.</p>
  <div class="objects objects-pairs">
    <div class="object-card">
      <h3>Query every session</h3>
      <p><code>path query</code> runs one jq filter across every session
      on the machine, whichever agent wrote it.</p>
    </div>
    <div class="object-card">
      <h3>Resume anywhere</h3>
      <p><code>path resume --harness codex</code> moves a Claude Code
      session into Codex: the intent, the state, and the dead ends
      already ruled out.</p>
    </div>
    <div class="object-card">
      <h3>Archive and search</h3>
      <p><code>path p cache sync</code> keeps every session on the
      machine, incrementally, in one format that won’t rot when an agent
      changes its log.</p>
    </div>
    <div class="object-card adopter-card">
      <h3>Built on Toolpath: Pathbase</h3>
      <p><a href="https://pathbase.dev">Pathbase</a> stores, shares, and
      resumes sessions in Toolpath. Link one from a PR and reviewers see
      what was asked, tried, and rejected.</p>
    </div>
  </div>
</div>

## Built to be depended on

<div class="commitments">
  <div>
    <h3>Apache-2.0</h3>
    <p>The format, the <code>path</code> CLI, and every crate.</p>
  </div>
  <div>
    <h3>Versioned kinds</h3>
    <p>Kinds are immutable and semver-versioned. A revision ships at a
    new URI, and documents written against an old one stay valid.</p>
  </div>
  <div>
    <h3>Open to contributions</h3>
    <p>Parser crates for new agents and proposals for the schema are
    welcome from anyone, not just Empathic.</p>
  </div>
  <div>
    <h3>A published spec</h3>
    <p>An <a href="/rfc/">RFC</a>, a
    <a href="{{ site.repo }}/blob/main/schema/toolpath.schema.json">JSON
    Schema</a>, and <a href="{{ site.repo }}/tree/main/examples">example
    documents</a>. Implement it in any language.</p>
  </div>
</div>

## Supported agents

<ul class="harness-list">
  <li><a href="https://docs.rs/toolpath-claude">Claude Code</a></li>
  <li><a href="https://docs.rs/toolpath-gemini">Gemini CLI</a></li>
  <li><a href="https://docs.rs/toolpath-codex">Codex CLI</a></li>
  <li><a href="https://docs.rs/toolpath-copilot">Copilot CLI <em>(preview)</em></a></li>
  <li><a href="https://docs.rs/toolpath-opencode">opencode</a></li>
  <li><a href="https://docs.rs/toolpath-pi">Pi</a></li>
  <li><a href="https://docs.rs/toolpath-cursor">Cursor IDE</a></li>
</ul>

Parsing captures the full session: prompts, tool calls, reasoning, file
changes, sub-agent work, token usage. Projecting writes a session the
harness accepts as its own, so it resumes natively. Where a harness’s
log doesn’t record something, the [format notes]({{ site.repo }}/tree/main/docs/agents/formats)
say so.

Git history and GitHub pull requests parse into the same schema, so a
session, the PR it became, and the release that shipped it can share
one graph.

<h2 id="install">Start with the sessions already on your machine</h2>

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
</div>

```bash
# Archive every agent session on this machine (all harnesses, incremental)
path p cache sync

# Output tokens by model, across every session, whichever agent produced it
path query 'group_by(.step.actor) | map({actor: .[0].step.actor, output_tokens: ([.[].change[]?.structural.token_usage.output_tokens // 0] | add)})'

# Share a session, then resume it in the original harness or a different one
path share
path resume https://pathbase.dev/alex/pathstash/path-pr-42 --harness codex
```

<svg class="topo topo-wide" viewBox="0 0 900 80" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <style>.topo-accent{stroke:var(--accent);}.topo-pencil{stroke:var(--text-secondary);}</style>
  <path d="M0,55 Q80,20 200,45 Q320,70 450,30 Q580,0 700,50 Q800,75 900,40" class="topo-accent" stroke-width="1" opacity="0.18" fill="none"/>
  <path d="M0,60 Q90,30 210,52 Q340,74 460,38 Q590,8 710,55 Q810,78 900,48" class="topo-accent" stroke-width="1" opacity="0.13" fill="none"/>
  <path d="M0,65 Q100,40 220,58 Q350,76 470,44 Q600,14 720,58 Q815,80 900,54" class="topo-pencil" stroke-width="1" opacity="0.12" fill="none"/>
</svg>

<div class="scenarios">
  <h2>Go deeper</h2>
  <div class="objects">
    <div class="object-card">
      <h3>Read the spec</h3>
      <p>Start with <a href="/format/">the format at a glance</a>: the
      shape of a document, the step DAG, and how it compares to git.
      Then the <a href="/rfc/">RFC</a> for the normative details.</p>
    </div>
    <div class="object-card">
      <h3>Build on the crates</h3>
      <p>Everything the CLI does is a library call: core types, a
      provider crate per harness, renderers for DOT and Markdown. See
      <a href="/crates/">the crates</a> or the
      <a href="https://docs.rs/toolpath">API reference</a>.</p>
    </div>
    <div class="object-card">
      <h3>Stay in Claude Code</h3>
      <p><code>/plugin install path@toolpath</code> adds
      <code>/path:share</code> and <code>/path:query</code> as slash
      commands and installs the CLI on first use. See
      <a href="{{ site.repo }}/tree/main/plugins/claude-code">the
      plugin</a>.</p>
    </div>
  </div>
</div>
