# toolpath-openclaw

Read [OpenClaw](https://github.com/openclaw/openclaw) agent-session logs and
derive [Toolpath](https://toolpath.net) provenance documents.

OpenClaw is a local-first, multi-channel personal AI assistant. It stores
each session as an append-only JSONL transcript at
`~/.openclaw/agents/<agentId>/sessions/<sessionId>.jsonl` (format version 3:
a session header line followed by an `id`/`parentId` tree of entries).

This crate implements the [`toolpath_convo::ConversationProvider`] trait and
a `derive_path` wrapper that produces a [`toolpath::v1::Path`], plus an
[`crate::project::OpenClawProjector`] that projects a `Path` back into an
OpenClaw session on disk (inception).

Because an OpenClaw session is scoped to one channel peer, the human actor
is recovered from the `sessions.json` routing key (`agent:<id>:<channel>:…`)
and set as a channel-aware `human:<channel>/<peerId>` actor; the channel,
peer, and session-kind metadata ride on `path.meta.extra["openclaw"]`.

The on-disk format is documented in detail at
[`docs/agents/formats/openclaw/`](https://github.com/empathic/toolpath/tree/main/docs/agents/formats/openclaw).
