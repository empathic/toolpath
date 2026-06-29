---
layout: base.njk
title: CLI
nav: cli
permalink: /cli/
---

# CLI

The `path-cli` crate installs a binary called `path`.

```bash
cargo install path-cli
```

> The older `toolpath-cli` crate name is preserved as a deprecated shim — `cargo install toolpath-cli` still installs the same `path` binary, but new users should reach for `path-cli` directly.

## Commands

The CLI splits into porcelain (high-level day-to-day flows) and
plumbing (`path p …`, the building blocks porcelain composes from).

```
path
  haiku
  show        # markdown summary for one session (used by fzf preview)
  share       # one-shot interactive picker + Pathbase upload
  resume      # project a doc into a coding agent and exec --resume
  query
    ancestors --input FILE --step-id ID
    dead-ends --input FILE
    filter    --input FILE [--actor PREFIX] [--artifact PATH] [--after TIME] [--before TIME]
  auth        login | status | whoami | logout [--url URL]
  p           # plumbing
    list
      git       [--repo PATH] [--remote NAME] [--json]
      claude    [--project PATH] [--json]
    import
      git       --repo PATH --branch NAME[:START] [--base COMMIT] [--remote NAME] [--title TEXT]
      claude    --project PATH [--session ID] [--all]
                                                    # global: [--force] [--no-cache]
    export
      claude    --input REF [--project DIR | --output FILE]
      pathbase  --input REF [--url URL]
    cache       ls | rm CACHE-ID
    render
      dot       [--input FILE] [--output FILE] [--show-files] [--show-timestamps]
                [--highlight-dead-ends BOOL]
    merge       FILE... [--title TEXT]
    validate    --input FILE
    derive      # stdout-JSON sibling of import (same sources, --no-cache implied)
    project     # narrower file-shaped sibling of export
    incept      # file/stdin-shaped sibling of `export claude --project`
    track
      init      --file PATH --actor ACTOR [--title TEXT] [--base-uri URI] [--base-ref REF]
                [--actor-def JSON] [--source TEXT] [--session-dir PATH]
      step      --session FILE --seq N --parent-seq N [--actor ACTOR] [--source JSON]
                [--time ISO8601]
      visit     --session FILE --seq N [--inherit-from N]
      note      --session FILE --intent TEXT
      annotate  --session FILE [--step ID] [--intent TEXT] [--source JSON] [--ref JSON]...
      export    --session FILE
      close     --session FILE [--output FILE]
      list      [--session-dir PATH] [--json]
```

All commands that produce JSON output accept `--pretty` for formatted output.

## When to reach for each command

Porcelain:

- **haiku** — Print a small bit of provenance poetry
- **show** — Render a single session as a markdown summary (used as the fzf preview)
- **share** — One-shot: pick an agent session and upload it to Pathbase
- **resume** — Open a Toolpath document back into your coding agent
- **query** — Ask questions of an existing document (who did what, what was abandoned, what came before)
- **auth** — Manage Pathbase credentials

Plumbing (`path p …`):

- **p list** — See what's available before importing (branches, Claude projects, active sessions)
- **p import** — Generate a Toolpath document from an existing source (git history, Claude conversations, …)
- **p export** — Project a Toolpath document back into a foreign format (Claude JSONL, Pathbase upload)
- **p cache** — Manage the on-disk document cache
- **p render** — Produce a visual from a document (pipe through Graphviz for PNG/SVG)
- **p merge** — Combine multiple documents into a single Graph (e.g. collecting PRs into a release)
- **p validate** — Check that a document is structurally valid
- **p derive** — Stream a Toolpath document to stdout (the stdout-only sibling of `import`)
- **p project** — Write a Toolpath document into a harness's session format as a file
- **p incept** — Project a Toolpath document into a Claude session layout under `--project` (file/stdin-shaped sibling of `export claude`)
- **p track** — Build a Path incrementally as you work (editor integrations, live sessions)

<svg class="topo topo-wide" viewBox="0 0 900 70" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <style>.topo-accent{stroke:var(--accent);}.topo-pencil{stroke:var(--text-secondary);}</style>
  <path d="M0,50 Q150,15 350,45 Q550,70 700,30 Q800,10 900,40" class="topo-accent" stroke-width="1" opacity="0.16" fill="none"/>
  <path d="M0,55 Q160,22 360,50 Q560,72 710,36 Q810,16 900,46" class="topo-pencil" stroke-width="1" opacity="0.11" fill="none"/>
  <path d="M0,60 Q170,30 370,55 Q565,74 715,42 Q815,22 900,52" class="topo-accent" stroke-width="1" opacity="0.09" fill="none"/>
</svg>

## Typical workflows

### See what happened in this repo

```bash
path p import git --repo . --branch main --no-cache --pretty
```

Walks the git history on `main`, converts each commit to a Step, and outputs a Path document.

### Visualize the provenance

```bash
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png
```

Pipe the imported document through the DOT renderer, then through Graphviz. Steps are color-coded by actor type, dead ends get red dashed borders.

### Import from a Claude session

```bash
path p import claude --project /path/to/project --no-cache --pretty
```

Reads the most recent Claude conversation for that project and maps it to a Toolpath Path.

### Find abandoned approaches

```bash
path query --input doc.json 'map(select(.dead_end))'
```

`path query` loads every step in the local cache into one JSON array and transforms it with a jaq (jq) filter; `--input` scopes it to a single document instead. Each element wraps a step with `cache_id`, `path`, and `dead_end` — so `map(select(.dead_end))` returns the steps that have no descendants leading to the path head: the things that were tried and discarded.

### Track changes in real time

```bash
# Start a session (pipe initial content via stdin)
cat src/main.rs | path p track init --file src/main.rs --actor human:alex --title "Refactoring auth"

# After each save, record a step (pipe current content via stdin)
cat src/main.rs | path p track step --session /tmp/session.json --seq 1 --parent-seq 0
cat src/main.rs | path p track step --session /tmp/session.json --seq 2 --parent-seq 1

# Annotate a step with intent or VCS source
path p track annotate --session /tmp/session.json --intent "Extract helper"

# Export the finished document
path p track export --session /tmp/session.json --pretty
```

The `p track` command group records changes to a file over time, building a Path document incrementally. Each `step` captures a diff from the previous state.

### Combine multiple documents

```bash
path p merge pr-42.json pr-43.json pr-44.json --title "Release v2" --pretty
```

Merges Path documents into a Graph. Useful for collecting related PRs into a release provenance bundle.

### Validate a document

```bash
path p validate --input doc.json
```

Checks that a Toolpath document is structurally valid against the format specification.

### Multi-branch import

```bash
path p import git --repo . --branch main --branch feature/auth --no-cache --pretty
```

When given multiple branches, produces a Graph document with one Path per branch.
