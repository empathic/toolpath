# Channels and actors

This is the axis that sets OpenClaw apart from every other provider we
derive from. The other agents are single-user coding CLIs: the human is
whoever is at the terminal. OpenClaw is a multi-channel assistant — the same
agent persona talks to many people across WhatsApp, Telegram, Slack,
Discord, Matrix, Signal, and more — so "who is the human" is a real
question, and the answer is **not a field on the message**. It is encoded
structurally in the session key.

## The session key grammar

Built by `buildAgentPeerSessionKey` (`src/routing/session-key.ts:222-273`):

```
agent:<agentId>:<channel>:<peerKind>:<peerId>
agent:<agentId>:<channel>:<accountId>:direct:<peerId>   # per-account-channel-peer DM scope
agent:<agentId>:direct:<peerId>                          # per-peer DM scope
agent:<agentId>:main                                     # CLI / main session
```

| Segment | Values / meaning |
|---|---|
| `agentId` | The OpenClaw agent persona (default `main`). Also the directory bucket ([directory-layout.md](directory-layout.md)). |
| `channel` | `whatsapp` \| `telegram` \| `slack` \| `discord` \| `matrix` \| `signal` \| … |
| `peerKind` | `direct` \| `dm` \| `group` \| `channel` (`ParsedSessionDeliveryRoute`, `src/sessions/session-key-utils.ts`). |
| `peerId` | The channel-native id. For a **DM** this is the human's channel user id; for a **group/channel** it is the room/group id, *not* an individual person. |

Threads append `:thread:<threadId>` (`resolveThreadSessionKeys`). Opaque,
case-sensitive peer ids (Signal groups, Matrix rooms) are preserved verbatim
via `CASE_PRESERVING_PEERS`; everything else is lowercased. The key is a
stable, parseable identity string, and it is what `sessions.json` is keyed
on.

## Who is the human?

There is **no `sender` field** on a transcript `UserMessage`
(`llm-core/src/types.ts:280-284` — just `role`/`content`/`timestamp`).
Identity comes from the key:

- **Direct messages:** `peerId` *is* the human's channel user id. You can
  form a reasonable actor string like `whatsapp:15555550123`.
- **Groups / channels:** the key only gives you `group:<groupId>`. The
  **individual speaker is text-only** — OpenClaw injects a
  `[from: Sender Name (+E164)]` marker into the prompt text at the end of
  each group batch (`docs/channels/group-messages.md`), and lists members in
  the system prompt. The inbound layer *does* have structured `senderId` /
  `senderName` / `pushName` (e.g. `extensions/whatsapp/src/inbound/types.ts`),
  but those are consumed at routing/policy time and **flattened into message
  text** — they do **not** persist as transcript fields.

So per-message individual identity in a group is only recoverable by
**parsing the `[from: …]` marker** out of the user text. Treat that as
best-effort.

### `InputProvenance`

The closest structured "who / where from" kept on a user message is
`InputProvenance` (`src/sessions/input-provenance.ts:14-21`):

```ts
type InputProvenance = {
  kind: "external_user" | "inter_session" | "internal_system";
  originSessionId?: string;
  sourceSessionKey?: string;
  sourceChannel?: string;
  sourceTool?: string;
};
```

Use `kind` to distinguish a real channel user from an inter-session message
(one agent/session driving another) or an internal system prompt.

## Who is the agent?

Two different axes, and a toolpath derivation must decide which is its
`agent:<name>`:

- **The persona:** `agentId` (plus a public identity blob
  `AgentIdentityResultSchema` — `agentId`, `name`, `avatar`, `emoji` —
  `gateway-protocol/src/schema/agent.ts:955-966`). Stable across turns and
  models.
- **The model:** every `AssistantMessage` carries `provider` / `model` /
  `api` ([messages.md](messages.md#assistant-message-metadata)), and
  `model_change` entries mark switches. This can change mid-session.

## Closest `type:name` actor strings

toolpath actors are `type:name`. The honest mapping:

| Side | Suggested actor string | Caveat |
|---|---|---|
| Human (DM) | `<channel>:<peerId>` e.g. `whatsapp:15555550123` | Clean. |
| Human (group) | `<channel>:group:<groupId>` + `[from:]` parse for the person | The group id is structural; the person is text-only. |
| Agent (persona) | `agent:<agentId>` | Stable; loses the model. |
| Agent (model) | `<provider>:<model>` e.g. `anthropic:claude-…` | Per-message; changes on `model_change`. |

This is a **decision for the eventual `toolpath-openclaw` crate**, not
something the format dictates — the format gives you both axes and a
multi-party group reality that none of the single-user providers have. Pick
deliberately and document it.
