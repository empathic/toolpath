---
layout: base.njk
title: FAQ
nav: faq
permalink: /faq/
---

# FAQ

## General

### What is Toolpath?

Toolpath is a format for recording artifact transformation provenance. It tracks **who** changed **what**, **why**, what they tried that didn't work, and how to verify all of it. Think "git blame, but for everything that happens to code &mdash; including the stuff git doesn't see."

### When should I use Toolpath?

Toolpath is useful when you want to:

- Record the full provenance of a code change across multiple actors (humans, AI agents, formatters, linters)
- Preserve abandoned approaches alongside the successful path
- Attach structured intent, external references, and signatures to changes
- Track changes at finer granularity than VCS commits

### When should I NOT use Toolpath?

Toolpath is not the right tool for:

- **Real-time collaboration** &mdash; Toolpath is for provenance, not live editing (use CRDTs or OT for that)
- **Replacing your VCS** &mdash; Toolpath complements git/jj/hg, it doesn't replace them
- **Large binary artifacts** &mdash; The diff-based change model assumes text-like content

### Can I use Toolpath without a VCS?

Yes. A path's `base` can use a `toolpath:` URI to branch from another path's step, creating a pure Toolpath chain with no VCS backing. You can also use `file:///` URIs for local-only provenance.

### How does Toolpath compare to W3C PROV?

W3C PROV is a general-purpose provenance data model (entities, activities, agents). Toolpath is narrower and more opinionated:

- **PROV** models arbitrary provenance relationships across any domain
- **Toolpath** models artifact transformations specifically, with built-in support for diffs, actor types, DAG structure, dead ends, and signatures

If you need general provenance, use PROV. If you need to track how code evolved through multiple actors, Toolpath gives you a tighter, more useful model out of the box.

### How does Toolpath compare to in-toto or Sigstore?

in-toto and Sigstore focus on **supply chain integrity** &mdash; attesting that specific steps were performed by specific actors in a pipeline. Toolpath focuses on **transformation provenance** &mdash; recording what happened to artifacts and why.

They complement each other: you might use Toolpath to record the full history of a PR, then use Sigstore to attest that the release was built from that provenance chain.

## Format design

### Why is Document externally tagged?

Every Toolpath JSON file has exactly one top-level key &mdash; `"Step"`, `"Path"`, or `"Graph"` &mdash; that identifies the document type. This makes the type unambiguous without inspecting inner fields. PascalCase variant names visually distinguish the type tag from the lowercase structural fields inside.

### Why Unified Diff for the `raw` perspective?

Unified Diff (the format produced by `diff -u` and used by git) is widely understood, human-readable, well-specified, and backward-compatible with existing tooling. Future versions may add alternative perspectives, but `raw` is always Unified Diff.

### How are dead ends detected?

Dead ends are implicit &mdash; no explicit marking required. A step is a dead end if it's not an ancestor of `path.head`:

```
active_steps = ancestors(path.head)
dead_ends = all_steps - active_steps
```

Steps don't know their fate. It's determined by the graph structure relative to the current head.

### Why is `meta` always optional?

A minimal document needs only `step` + `change`. Making `meta` optional means simple changes require minimal ceremony, streaming steps can be lightweight, and you can add provenance incrementally.

### How do multi-parent merges work?

Steps have a `parents` array. An empty or omitted array means root step. A single-element array is linear history. Multiple parents represent a merge from parallel work.

## Open design questions

These questions are not yet resolved. They need more thought before the format stabilizes.

### How should step IDs be generated?

Options under consideration: content-addressed hashing, UUIDs, hierarchical (`session-abc/turn-5`), or sequential (`step-001`). The examples use sequential IDs for readability. No formal requirement yet.

### Who defines structural operation types?

Options: central registry, namespaced extensions (`rust.add_method`), schema-per-language, or emergent convention. Current leaning: namespaced with a `core` namespace for universal ops.

### How should privacy and redaction be handled?

Options: reference-don't-embed (store URIs, not content), redaction markers, access tiers, or encryption. Current leaning: reference by default, with optional redaction markers.

### How does the format evolve?

Options: semver, date-based, or extension-based (frozen core). Current leaning: semver for core schema, with "old readers ignore unknown fields" policy.

## Links

- [Full specification (RFC.md)](https://github.com/empathic/toolpath/blob/main/RFC.md)
- [JSON Schema](https://github.com/empathic/toolpath/blob/main/schema/toolpath.schema.json)
- [Example documents](https://github.com/empathic/toolpath/tree/main/examples)
- [Changelog](https://github.com/empathic/toolpath/blob/main/CHANGELOG.md)
