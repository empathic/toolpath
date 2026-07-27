# Amp fixtures

Real-world Amp session captured with
[`docs/agents/feature-elicit.prompt.txt`](../../docs/agents/feature-elicit.prompt.txt),
run verbatim as a single `amp -x` execute-mode turn.

| File | What it is |
| --- | --- |
| `convo.json` | **Canonical.** `amp threads export <thread-id>` output — the full thread document (24 messages). This is what a forward `toolpath-amp` provider parses. |
| `stream.jsonl` | The teed `--stream-json` capture of the same run (26 lines). Claude-Code-compatible envelope; available only at capture time. |

## Provenance

- **Amp version:** `0.0.1785170481-ga5b614` (released 2026-07-27T16:41:21Z)
- **Captured:** 2026-07-27
- **Thread:** `T-019fa4db-29cf-70c9-8d9b-81524df70e52` (private), title
  auto-generated as "Filesystem tool exercise"
- **Agent mode:** `medium` → model `gpt-5.6-sol`
- **Command:**
  `amp -x "$(cat docs/agents/feature-elicit.prompt.txt)" --stream-json --no-archive-after-execute`
- **Cost:** $0.32

## Coverage against the feature-elicit checklist

| Category | Present | How |
| --- | --- | --- |
| User turns | ✅ 1 | Single execute-mode prompt (`-x` sends one user message) |
| Assistant turns | ✅ 12 | — |
| Shell exec | ✅ 6 | `shell_command` |
| File write | ✅ | `apply_patch` (`*** Add File:`) |
| File edit | ✅ | `apply_patch` (`*** Update File:`) |
| File read | ✅ | `shell_command` (`cat notes.md`) — Amp has no dedicated read tool |
| Search by name | ✅ | `shell_command` (`find . -maxdepth 1 -name 'note*'`) |
| Search by content | ✅ | `shell_command` (`rg -n 'fixture' .`) |
| Errored tool result | ✅ | `cat does-not-exist.txt` → `run.result.exitCode: 1` |
| Delegation | ✅ | `Task` tool; sub-agent's own turns are **not** in the thread |
| Thinking / reasoning | ⚠️ partial | 5 of 12 assistant messages carry non-empty `thinking`; the rest are `""` with only the encrypted blob |
| Final summary | ✅ | Message 24 |

Only **one** user turn: execute mode sends a single message, so the
"2+ user turns" line of the generic checklist can't be met by `-x`. The 11
`role: "user"` messages that carry `tool_result` blocks are tool plumbing,
not human turns.

## Sanitization

Applied by literal substitution before committing; the replacements are
internally consistent, so embedded diffs and `ls -la` output still parse:

| Real | Fixture |
| --- | --- |
| capture working directory | `/tmp/amp-elicit` |
| `/Users/<user>` | `/Users/example` |
| `<username>` (in `ls -la` owner columns) | `example` |
| `creatorUserID` | `user_00000000000000000000000000` |
| `installationID` | `00000000-0000-0000-0000-000000000000` |
| `deviceFingerprint` | `v1:fp_0000…` (64 zeros) |

Thread and message ids are **kept as captured** — the thread is private and
requires authentication, and piece 01's wire-level round-trip test needs
realistic id shapes. The opaque `thinking.signature` /
`openAIReasoning.encryptedContent` blobs are also kept verbatim: they are the
model provider's encrypted reasoning, not credentials, and dropping them would
break value-identity round-tripping.

## Verifying

```bash
jq -e '.messages | length' test-fixtures/amp/convo.json     # 24
jq -se 'length' test-fixtures/amp/stream.jsonl              # 26
```

Format reference: [`docs/agents/formats/amp/`](../../docs/agents/formats/amp/README.md).
