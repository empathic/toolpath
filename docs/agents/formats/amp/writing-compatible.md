# Writing a thread Amp will resume

What a synthesized thread must satisfy for `amp threads continue <id>` to
load it. The copilot analogue of this document records a *local* loader's
contract; Amp's is different in kind: **threads are server-authoritative**,
so the writer contract is a *server import* contract, and every constraint
in it can only be discovered by submitting a fabricated thread and reading
the rejection.

> **Status: live loop STARTED, L2 BLOCKED.** Two fabricated threads were
> submitted to the real server under fresh ids (2026-07-27,
> `0.0.1785170481-ga5b614`). The plain-REST route **returns `201 Created`
> and creates no thread** — see the rejection table. The real import seam
> is the Rivet actor gateway, which needs a websocket-token handshake this
> piece did not budget for. `path resume --harness amp` therefore does not
> yet produce a resumable thread; the export-to-file path is green.

## The three-route fork (piece 03, PLAN.md)

| Route | Verdict | Evidence |
| --- | --- | --- |
| (a) local-state write (copilot-style) | **Ruled out.** | Piece-00 Q1: no local file holds thread content; the per-thread log is telemetry (548-byte max lines, no bodies, no usage). There is nothing to write into. `[observed, 0.0.1785170481-ga5b614]` |
| (b) server-side import | **Chosen — the only viable writer.** | The bundle contains a thread-actor `POST /import` call whose body is a whole serialized thread (`{ thread: … }`), with 409 tolerated, plus `POST /api/thread-actors/<id>` ("Failed to mark thread \<id\> as imported"). `[reverse-eng]` |
| (c) documented infeasibility | Fallback if (b)'s probe fails. | The evidence for that outcome would live here, verbatim. |

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

## The import seam `[reverse-eng, unexercised]`

`write_into_amp_project` (`path-cli/src/cmd_export.rs`), used by
`path p export amp --project` and `path resume --harness amp`:

1. **Local artifact first** — the full export document lands at
   `~/.toolpath/amp-projected/<id>.json` (`create_new`, INSERT-only). This
   is the record of exactly what was submitted and the substrate the
   rejection loop iterates on. It is a toolpath-owned directory; Amp-owned
   state is never created or mutated.
2. **Server import, warn-don't-fail** — `POST $AMP_URL/api/thread-actors`
   with body `{"thread": <export>}` (409 tolerated), then a best-effort
   `POST /api/thread-actors/<id>` (mark-imported). Auth is **only**
   `$AMP_API_KEY` — `secrets.json` is never read, and without the variable
   the import is skipped with a warning so no login flow can trigger.
   **Per rejection 1, a 2xx is not trusted**: the thread is read back
   through the first-party CLI, and anything short of a successful
   read-back reports `warning: … the import did not take effect` rather
   than claiming success. As of today this route always lands in that
   unverified state — it is retained as the honest, evidence-producing
   probe, not as a working writer.
3. A preview banner prints until a projected thread has been verified to
   resume in the real CLI.

`path resume`'s exec recipe is `amp threads continue <id>`
(`[observed]` for the `-x` form; see
[resume-and-sessions.md](resume-and-sessions.md#resuming)).

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

## Verification recipe (the ⚠ loop)

```sh
cargo build -p path-cli
AMP_API_KEY=… bash scripts/verify-amp-live.sh <doc.json|pathbase-url|cache-id>
```

The script (isolated `HOME` + all three XDG roots per the piece-00 Q3
recipe, `AMP_API_KEY` mandatory to keep the unattended browser-login flow
unreachable) projects under a fresh id, runs the real
`amp threads continue <id> -x "…" --no-archive-after-execute`, greps for
loader rejections, and prints the context-probe answer ("In one sentence,
what was the most-used tool in this session?") for human judgment.

⚠ Costs and state: the import creates a real private thread on the
authenticated account and the continuation spends real credits. Fresh ids
only; existing threads are never touched.
