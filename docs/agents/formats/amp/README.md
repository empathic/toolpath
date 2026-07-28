# Amp CLI session format

> **Reference revision:** 2026-07-27
> **Tracks:** Amp (ampcode.com), the `amp` CLI — a Bun single-file executable
> installed at `~/.amp/bin/amp`.
> **Version anchors:** first-hand captures at `amp --version`
> **`0.0.1785170481-ga5b614`** (released 2026-07-27T16:41:21Z). One older
> thread on the same machine was produced partly under
> `0.0.1785164324-gd1fcef`.
> **First-hand grounding:** three private threads captured on 2026-07-27 — a
> pre-existing install-session thread, a two-turn trivial thread, and a full
> [feature-elicit](../../feature-elicit.md) run (24 messages, 11 tool calls,
> a sub-agent dispatch). Fixtures at
> [`test-fixtures/amp/`](../../../../test-fixtures/amp/README.md).
>
> When you change anything in this directory, bump the revision date here.

This folder documents how an Amp session is represented — on the wire, on
disk, and in the two artifacts a toolpath provider can actually parse. It is
**not** about the Amp VS Code / JetBrains extensions, and not about
`ampcode.com`'s web UI beyond noting where it is the only source of a value.

## ⚠️ Sourcing posture — read this first

Amp publishes **no session-format schema**. Everything here is either
first-hand observation from captures on one machine at one version, output of
the CLI's own `--help`, or reverse-engineering of the shipped Bun bundle's
embedded JavaScript (`strings -n 6 ~/.amp/bin/amp`, ~116k lines).

Two facts dominate the design of any Amp integration and are worth stating up
front:

1. **Threads are server-authoritative.** No local file contains message
   bodies. Reconstruction goes through `amp threads export <id>`, which is a
   network fetch. See [RECON.md](RECON.md) Q1.
2. **Amp self-updates aggressively.** The version moved
   `gd1fcef` → `ga5b614` within ~1.5 hours during this very recon, and
   versions are build-timestamped (`0.0.<epoch>-g<sha>`). Treat every claim
   below as version-pinned.

Every non-trivial claim carries an inline tag:

| Tag | Meaning | Default confidence |
| --- | --- | --- |
| `[observed, <ver>]` | Seen first-hand in a capture at that Amp version. | High |
| `[official]` | Stated by Amp's own `--help` / documented output. | High |
| `[reverse-eng]` | Extracted from the shipped bundle's embedded JS; not exercised. | Medium |
| `[inferred]` | Our structural reasoning; no direct source. | Low |
| `[unverified]` | Believed but unconfirmed; flagged for a future capture. | — |

## How the docs are organized

1. **[RECON.md](RECON.md)** — the four architecture-gating questions
   (reconstruction, tokens, isolation, stream envelope) with one-sentence
   answers and the evidence behind each. **Start here.**
2. **[directory-layout.md](directory-layout.md)** — the full `~/.local/share/amp`
   / `~/.cache/amp` / `~/.config/amp` inventory, what each file holds, and the
   env-var precedence that relocates them.
3. **[events.md](events.md)** — the two wire shapes: the `amp threads export`
   thread document and the `--stream-json` line envelope, the content-block
   catalogue, the native tool vocabulary, and the closing
   **[mapping sketch to the toolpath IR](events.md#mapping-sketch-to-the-toolpath-ir)**.
4. **[session-state.md](session-state.md)** — thread and message identity,
   the revision counter, per-thread environment stamping, and the local
   app-state files that are *not* session content.
5. **[file-fidelity.md](file-fidelity.md)** — how file edits are captured
   (`apply_patch` + inline unified diffs in the result), why fidelity is
   Codex-grade, and how errors are actually signalled.
6. **[resume-and-sessions.md](resume-and-sessions.md)** — the `amp threads`
   command surface, listing/continuation semantics, and the surface a future
   `AmpProjector` / `path resume` integration would have to match.
7. **[writing-compatible.md](writing-compatible.md)** — the writer contract:
   the route fork (local fabrication and server import both ruled out; the
   first-party CLI two-step chosen and verified), what `AmpProjector`
   emits, the writer seam, the fidelity caveat, and the observed-rejection
   table.
8. **[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md)** — methodology,
   the full source list, and the unchecked verification checklist.

## Scope exclusions

- **Amp editor extensions** (VS Code, JetBrains). They talk to the same
  server-side thread store, but their local footprint is not covered here.
- **`~/.local/share/amp/secrets.json`** — the stored API key. Never read,
  never quoted, never committed. Only its existence and size are noted.
- **The `orb` / `apps` / `projects` / `secrets` / `mcp` command families.**
  Real Amp surface, but orthogonal to session derivation; see
  `amp --help`.
- **Billing internals.** `amp usage` and `amp threads usage` report dollars,
  and that is all we document about cost.

## Conventions

- **Field names** appear exactly as on the wire. Note that Amp uses **two
  different casings for the same data**: the export document is camelCase
  (`outputTokens`), the `--stream-json` line is snake_case
  (`output_tokens`). This is not a typo in these docs — see
  [events.md](events.md#the-two-usage-encodings).
- **Versions in parentheses** are what we observed, not what Amp tagged a
  format change at.
- **Keep headings anchor-stable** — cross-links use GitHub auto-anchors.

## Maintenance

This is the single place Amp format knowledge should accumulate. When
`toolpath-amp` learns something the hard way — a new block type, a new tool
name, a rejection from a writer probe — record it here in the same change and
**upgrade the confidence tag**.
