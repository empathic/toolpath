---
layout: base.njk
title: "Kind: agent-coding-session"
permalink: /kinds/agent-coding-session/
---

# Kind: `agent-coding-session`

A Toolpath path that records an AI coding conversation. Each conversational-turn step carries a `"conversation.append"` structural change with the turn's role, text, and so on.

Documents reference a specific version URI. They do not depend on this landing page.

## Versions

- [**v1.2.0**](/kinds/agent-coding-session/v1.2.0/): `https://toolpath.net/kinds/agent-coding-session/v1.2.0` _(current)_ — adds an optional `model` to the turn payload (the model that produced the turn; previously only in the step actor and `meta.actors`)
- [**v1.1.0**](/kinds/agent-coding-session/v1.1.0/): `https://toolpath.net/kinds/agent-coding-session/v1.1.0` — adds `group_id` and specifies message-level token accounting (a message's usage appears on exactly one step, so per-step sums equal session totals)
- [**v1.0.0**](/kinds/agent-coding-session/v1.0.0/): `https://toolpath.net/kinds/agent-coding-session/v1.0.0` — superseded; see its erratum on token accounting
