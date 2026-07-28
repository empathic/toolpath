# Writing a thread Amp will resume

What a synthesized thread must satisfy for `amp threads continue <id>` to
load it. The copilot analogue of this document records a *local* loader's
contract; Amp's is different in kind: **threads are server-authoritative**,
so the writer contract is a *server import* contract, and every constraint
in it can only be discovered by submitting a fabricated thread and reading
the rejection.

> **Status: ✅ resume verified** (2026-07-27/28, `0.0.1785170481-ga5b614`).
> `path resume --harness amp` produces a real, resumable thread and the
> resumed model answers questions about the prior session correctly. Getting
> there ruled out two routes first — local fabrication (no store exists) and
> the REST-looking import path (returns `201 Created`, creates nothing) —
> and landed on the **first-party CLI two-step** below. The fidelity ceiling
> is context, not native tool blocks; read the caveat before quoting this as
> a round trip.

## The three-route fork (piece 03, PLAN.md)

| Route | Verdict | Evidence |
| --- | --- | --- |
| (a) local-state write (copilot-style) | **Ruled out.** | Piece-00 Q1: no local file holds thread content; the per-thread log is telemetry (548-byte max lines, no bodies, no usage). There is nothing to write into. `[observed, 0.0.1785170481-ga5b614]` |
| (b) server-side import via the thread actor | **Ruled out for now.** | The bundle's `POST /import` is a *Rivet actor* fetch behind a wsToken handshake, not a REST call; the REST-looking path answers `201 Created` and creates nothing (rejection 1). `[observed + reverse-eng]` |
| (b′) **first-party CLI two-step** | **✅ Chosen — verified working.** | `amp threads new` creates a real empty thread server-side (free, no model turn); `amp threads continue <id> -x <message>` seeds it. Both are documented `amp --help` surface. |
| (c) documented infeasibility | Not needed. | Superseded by (b′). |

### Why (b′) instead of holding out for (b)

(b′) uses only documented first-party commands, so it does not pin an
undocumented protocol that re-mines on every `amp update` (builds are
timestamped; one bumped mid-recon). It also never fabricates: the thread it
resumes is one the server created and the account owns. The cost is
fidelity — see the caveat below — and one execute turn per resume.

## What the projector emits

`AmpProjector` (`toolpath-amp/src/project.rs`) inverts the forward
`to_view` mapping into an `amp threads export`-shaped document:

- **Envelope**: fresh `T-<uuidv7>` id (minted in `build_amp_session`, never
  reusing an existing thread id), `title` from the first user prompt,
  `created`/`updatedAt` from the view, `env.initial` with the resume
  directory as `trees[0].uri` and a `toolpath` platform stamp.
- **Messages**: one `user`/`assistant` message per turn, plus regenerated
  tool-result carrier `user` messages (the inverse of the forward merge),
  paired by `toolUseID`. Position-stable: projecting the same view twice is
  byte-identical.
- **Ids**: turn ids pass through as `protocolMessageID`; `messageId` is the
  1-based integer position; delegation (`Task`) tool ids are preserved so
  `DelegatedWork` pairing survives a round trip.
- **Tools**: Amp-native names (the observed 29) pass through verbatim;
  foreign tools remap by category via `native_name` (`FileRead` →
  `shell_command`/`cat` — Amp has no read tool). `run.result` shapes are
  reconstructed per tool (`shell_command` `{output, exitCode}`,
  `apply_patch` `{files[], summary}` with real diffs from `FileMutation`,
  `Task` bare string, `skill` content list).
- **Usage**: the four real counters re-expand verbatim; the derived
  `totalInputTokens` (= input + cacheRead + cacheCreation, the invariant
  verified on all 17 captured usage objects) is regenerated;
  `maxInputTokens` is capacity, not spend — omitted. Usage-free turns emit
  no `usage` stub.

## The writer seam (verified)

`write_into_amp_project` (`path-cli/src/cmd_export.rs`), used by
`path p export amp --project` and `path resume --harness amp`:

1. **Create a thread** — `amp threads new` returns a **server-assigned**
   `T-…` id. This matters: the projector cannot mint an id and hand it to
   Amp, because Amp only resumes ids its own server issued. No model turn
   runs, so this step is free. Only ever creates new threads.
2. **Record the full-fidelity projection** — the `ThreadExport` document
   lands at `~/.toolpath/amp-projected/<id>.json` (`create_new`,
   INSERT-only), keyed on the live thread id so the artifact and the thread
   are the same session. Toolpath-owned directory; Amp-owned state is never
   created or mutated.
3. **Seed the context** — `amp threads continue <id> -x <rehydration
   prompt>`: one execute turn carrying the prior session rendered as a
   Markdown transcript (`toolpath-md` at full detail, wrapped by
   `toolpath_amp::rehydration_prompt`, which tells the model the work has
   already happened and to acknowledge rather than redo it).

Auth is whatever the `amp` CLI itself uses — the child process inherits the
environment, so `AMP_API_KEY` works for isolated runs, and a normal
logged-in machine needs nothing. `secrets.json` is never read by toolpath.

`path resume`'s exec recipe is `amp threads continue <id>`
(`[observed]` for the `-x` form; see
[resume-and-sessions.md](resume-and-sessions.md#resuming)).

### ⚠ Fidelity caveat — this is context transfer, not a transcript import

The resumed thread contains the prior session as **one user message holding
a rendered Markdown transcript**, plus the model's acknowledgement. It does
**not** contain native Amp `tool_use`/`tool_result`/`thinking` blocks, and
`amp threads export` on it will not resemble the source thread.

This is a real difference from the claude / copilot / codex projectors,
which write native session files and round-trip structurally. Amp exposes
no first-party way to inject assistant or tool structure into a thread, so
prose is the honest ceiling until route (b) is cracked. What it buys is the
thing resume is *for*: the model can reason about the prior work, and
answers questions about it correctly (verified below). The structural
projection is not lost — it is written to disk beside the thread and is
what `p export amp --output` emits.

## Import-contract rejections (observed)

| # | Requirement | Verbatim rejection | Status |
|---|---|---|---|
| 1 | **A 2xx from `POST /api/thread-actors` does not mean the thread exists.** The plain-REST route accepts the whole serialized thread and answers `201 Created`, but no thread is created: reading it back immediately fails. Any writer MUST verify by read-back rather than trusting the status. | `Thread T-019fa541-56d6-7ea2-9f8f-8c0ac4a27470 does not exist.` (from `amp threads export` right after a `201 Created`) | `[observed, 0.0.1785170481-ga5b614]` |
| 2 | **The real import is not a REST call at all.** It is a Rivet *actor* fetch: the client resolves per-thread actor credentials, opens the actor by id against the gateway, and `fetch`es `/import` on it. A bare HTTPS POST to the documented-looking path cannot reach that handler. | *(no server error — the bundle's own call shape is the evidence)* `.threadActor.get([threadId],{params:{wsToken,transport:"json-rpc"}}).fetch("/import",{method:"POST",body:JSON.stringify({thread:…})})` | `[reverse-eng, 0.0.1785170481-ga5b614]` |

### What row 2 implies for a working writer

The gateway is `<ampURL>/actors` with a **hardcoded public client key as
HTTP basic-auth password** (baked into the shipped binary; deliberately not
reproduced here), and each actor call carries a **`wsToken`** obtained from
a prior credentials exchange (`/api/user-actor-credentials`, returning
`{poolName, threadId, wsToken, usesThreadActors}`). So a Rust writer must
reimplement: credentials exchange → gateway actor addressing → the
RivetKit actor-fetch wire convention → `POST /import`. That is a
materially larger surface than "post a document", it pins several
undocumented protocols at once, and it re-mines on every `amp update`
(builds are timestamped; one bumped mid-recon).

## ✅ Verified (amp 0.0.1785170481-ga5b614)

Both piece-00 captures were projected into fresh threads and probed. The
answers are specific and correct, which is what distinguishes a transferred
context from an amnesiac one:

| Source capture | Probe | Answer | Correct? |
| --- | --- | --- | --- |
| feature-elicit (24 messages, 11 tool calls) | *In one sentence, what was the most-used tool in this session?* | "The most-used tool was `shell_command`, invoked six times." | ✅ the capture has exactly 6 `shell_command` calls |
| trivial ("Reply with exactly: ok") | same probe | "No tools were used in this session." | ✅ that thread used no tools |

The feature-elicit run went through the full `path resume --harness amp`
path end to end (thread `T-019fa709-…`); the trivial run went through
`scripts/verify-amp-live.sh` (thread `T-019fa70b-…`), which also exercises
`p export amp --project`.

### Reproduce

```sh
cargo build -p path-cli
AMP_API_KEY=… bash scripts/verify-amp-live.sh <doc.json|pathbase-url|cache-id>
```

The script isolates `HOME` + all three XDG roots (piece-00 Q3 recipe) and
requires `AMP_API_KEY` so the unattended browser-login flow is never
reachable. It projects into a fresh thread, runs the real
`amp threads continue <id> -x "…"`, fails loudly on rejection, and prints
the probe answer for human judgment.

⚠ Costs and state: creates one real private thread on the authenticated
account and spends credits on two turns (seed + probe). Fresh threads only;
existing threads are never touched.

### One trap worth knowing

`amp` auto-enables execute mode whenever stdout is not a TTY, so a piped
`amp threads continue <id>` with no message errors with *"User message must
be provided through stdin or as argument when using execute mode"*. That is
the capture harness talking, not a rejected thread — interactively the same
command opens the TUI normally. It is why the verification script probes
with an explicit `-x` message rather than parsing a bare resume.
