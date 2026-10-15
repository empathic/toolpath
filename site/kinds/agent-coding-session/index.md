---
layout: base.njk
title: "Kind: agent-coding-session"
permalink: /kinds/agent-coding-session/
---

# Kind: `agent-coding-session`

A Toolpath path that records an AI coding conversation. Each conversational-turn step carries a `"conversation.append"` structural change with the turn's role, text, and so on.

Documents reference a specific version URI. They do not depend on this landing page.

## Versions

- [**v1.1.0**](/kinds/agent-coding-session/v1.1.0/): `https://toolpath.net/kinds/agent-coding-session/v1.1.0` _(current)_ — adds `group_id` and specifies message-level token accounting (a message's usage appears on exactly one step, so per-step sums equal session totals)
- [**v1.0.0**](/kinds/agent-coding-session/v1.0.0/): `https://toolpath.net/kinds/agent-coding-session/v1.0.0` — **read-only legacy**, superseded 2026-06-17. Not supported for writing: no producer emits it (every `toolpath-*` crate derives through `toolpath-convo`, which stamps v1.1.0, and a workspace test asserts it). Readers accept it; `path p validate` prints a warning naming its erratum on token accounting, and consumers summing `token_usage` over a v1.0.0 path must deduplicate as that erratum describes.
