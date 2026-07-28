# BUILD_LOG.md — template and conventions

This file defines the format every Amp build session uses when appending to
`roger-amp-plan/BUILD_LOG.md`. It is a template and rulebook only — **do not
append entries here.** Piece 00 creates `BUILD_LOG.md` in this directory,
seeded with the preamble below verbatim; every subsequent session appends one
entry and never edits prior ones.

---

## 1. Preamble (copy this verbatim as the top of `BUILD_LOG.md`)

```markdown
# Build log — Amp harness (`toolpath-amp`)

A chronological record of how the Amp harness was built: what each piece
delivered, what was decided along the way, and why. Written for a reviewer who
wants the reasoning behind the code, and for future build sessions picking up
context.

Conventions:

- **Append-only.** One entry per build piece, newest at the bottom. Entries
  are never edited after the fact; corrections go in a later entry.
- **Commits follow [Conventional Commits v1.0.0](https://www.conventionalcommits.org/en/v1.0.0/).**
  The git log carries the granular *what*; this file carries the *why*.
- **Key decisions use an ADR shape** (Context → Decision → Rationale →
  Alternatives rejected) so the reasoning is legible without having been there.
```

---

## 2. Entry format

Each entry is headed exactly:

```markdown
## Piece NN — <name> — tag: <amp-mN or "none"> — <YYYY-MM-DD>
```

`NN`/`<name>` come from `PLAN.md`'s piece table (`00`/`format-recon` …
`06`/`docs-release`); `tag:` is the git tag applied iff the piece's DoD went
green, else the literal word `none`.

Each entry contains these sections, in this order, all present (write "none"
rather than omitting a section):

### Goal

One line: what this piece delivers.

### What was built

Concrete path-named inventory bullets (`crates/toolpath-amp/src/provider.rs`,
`path share --harness amp`), one line each. Inventory, not narrative.

### Key decisions (ADR-style)

```markdown
- **<Short decision title>**
  - *Context:* the situation or constraint that forced a choice.
  - *Decision:* what was chosen, stated as a fact.
  - *Rationale:* why — the property gained, the risk avoided.
  - *Alternatives rejected:* what else was considered and why each lost.
```

Record what a senior reviewer would ask "why?" about (format interpretations,
token-pattern choice, the piece-03 route taken, dependency choices). Do not
record decisions that merely follow `PLAN.md` — reference it instead
("per PLAN.md Piece 01").

### Deviations from PLAN.md

What changed versus the plan and why, or "none". A deviation is not a
failure — an unexplained one is.

### Tests & verification

Exactly what was run and the result, so "it works" is trustworthy. Minimum:
the workspace gates (`cargo build/test/clippy --workspace -- -D warnings`),
`just ci`, and this piece's DoD commands from `PLAN.md` with the observed
output summarized in one line each. Any claim about live `amp` behavior
carries the `amp --version` stamp.

### Known limitations / follow-ups

Honest scope edges; if deliberate, say so and point at where it's documented.

### Open questions for Roger

Anything needing a decision before or during a later piece, or "none".

---

## 3. Style rules

- Tight prose. No filler; no restating the diff.
- Explain intent and trade-offs, not mechanics the code already shows.
- Repo-relative paths so a reviewer can jump straight there.
- A busy senior engineer reads any entry in under two minutes.
