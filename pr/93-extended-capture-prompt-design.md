# docs(agents): an extended capture prompt and its design record (draft for Bobby's review)

Status: draft · Base: pr/92 · Design for review; nothing is recorded or run until Bobby approves the design

## Why (plain words)
Ben's 10-step `feature-elicit.prompt.txt` is good for what it was built for (one deterministic script, every tool category, a failing call, a sub-agent), but it is one user message, too short to compact, with no model switch, no images or MCP tools, one tiny file and an optional sub-agent. The map's lossy and unknown cells are mostly in exactly those gaps. The core prompt stays unchanged as the core recording; this adds a second, extended session.

## What changes
- `docs/agents/feature-elicit-extended.prompt.txt` (new): the draft extended prompt, as separate user turns on one session.
- `docs/agents/feature-elicit-extended.md` (new): the design record — each gap in the core recording, the turn that exercises it, which map cells and unexplained clusters it should move, and per agent which turns can run headless and how they are driven.
- No recordings, no capture-script change.

## Verification
Docs only: qualifier review (0 drifted) and the secrets gate.

## For the next PR
After Bobby approves the design: drive it per agent through the capture script with pinned CLIs (pr/92), record, snapshot every stage, and classify.
