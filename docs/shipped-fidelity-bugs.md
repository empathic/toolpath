# Did this week's fidelity bugs ship?

Checked 2026-10-19 against the published crate sources: the `.crate` tarballs from `static.crates.io`, not the repo.
The current release is `path-cli` 0.26.0 (crates.io and GitHub release `v0.26.0`, 2026-09-18), which depends on
`toolpath-claude` ^0.13.3, `toolpath-codex` ^0.6.1, `toolpath-pi` ^0.6.1, `toolpath-copilot` ^0.1.0,
`toolpath-gemini` ^0.6.1, `toolpath-cursor` ^0.2.0, `toolpath-opencode` ^0.5.0 and `toolpath-convo` ^0.11.1. Each of
those is the newest published version of its crate, so `cargo install path-cli` today builds exactly these sources.

**Answer:** yes. All of them shipped, and every one is still in the current release (path-cli 0.26.0). They are also
on `main` (77dc16a, unreleased 0.29.0). The fixes exist only on `wip/bdelanghe`, in draft PRs #361, #363 and #365 and
in the unopened pieces pr/19, pr/20, pr/27 and pr/28.

For the deck: *Shipped: every one is in path-cli 0.26.0, the current release, most of them since 0.13.0 to 0.15.0.
They stay there until #361, #363 and #365 and pieces pr/19, 20, 27 and 28 merge and a release is cut.*

| Bug | Buggy code in the published crate | Shipped since (path-cli) | Fixed in | Fix status |
|---|---|---|---|---|
| pi parent chain: a resumed pi session shows only the last message | `toolpath-pi` 0.6.1 `provider.rs:256,276,296,321,461` `parent_id: base.parent_id.clone()` (no resolve past folded tool-result/model-change entries); `project.rs:168` emits tool-result entries without re-chaining the next turn | 0.13.0 (`toolpath-pi` 0.5.0 has the same lines) | #361 (pr/06) | draft PR |
| Copilot: a failed tool call is read as an errored result with empty content | `toolpath-copilot` 0.1.0 `provider.rs:231` `content: te.output.clone().unwrap_or_default()`; `types.rs` has no `error` field, so `error.message` is never read | 0.15.0 (first `toolpath-copilot`) | #361 (pr/06) | draft PR |
| Claude writer: an empty errored `tool_result`, which the Messages API rejects (400) on the first resumed turn | `toolpath-claude` 0.13.3 `project.rs:607` `ToolResultContent::Text(result.content.clone())` with no stand-in text | 0.13.0 (`toolpath-claude` 0.11.0) | #361 (pr/06) | draft PR |
| Copilot `workspace.yaml` timestamps that Copilot rejects (found with the above; also in #361) | `path-cli` 0.26.0 `cmd_export.rs:573,608` `chrono::Utc::now().to_rfc3339()` (`+00:00`, microseconds) | 0.15.0 | #361 (pr/06) | draft PR |
| codex→claude: the Claude writer emits non-Claude entry types | `toolpath-claude` 0.13.3 `project.rs:209` skips only consumed events, so `toolpath-codex` 0.6.1's `response_item.*` events (`provider.rs:436`) are written as Claude entries | 0.13.0 (`toolpath-claude` 0.11.0 and `toolpath-codex` 0.5.0) | pr/28 (`af15dab`) | on wip, no PR |
| Claude writer renames `message.stop_reason` to `stopReason` | `toolpath-claude` 0.13.3 `types.rs:75` `#[serde(skip_serializing_if…, alias = "stop_reason")]` on a `rename_all = "camelCase"` struct (same for `stop_sequence`, `:78`) | 0.13.0 (`toolpath-claude` 0.11.0) | pr/28 (`af15dab`) | on wip, no PR |
| Gemini CLI 0.47's `session-*.jsonl` stream not read | `toolpath-gemini` 0.6.1 `paths.rs:244,294,346,370` accept only `.json` files | all releases; a failure for users on gemini-cli ≥ 0.47 | pr/19 (`635f8dc`) | on wip, no PR |
| Cursor 3.24's `composerHeaders` table not read (sessions not listed) | `toolpath-cursor` 0.2.0 `reader.rs:20` reads only the `ItemTable` key `composer.composerHeaders`, which Cursor 3.24 migrates into a table and deletes (0.26.0 has no Cursor writer, so only reading is affected) | all releases; a failure for users on Cursor ≥ 3.24 | pr/20 (`e0415cd`) | on wip, no PR |
| Gemini session total drops token breakdowns (reasoning) | `toolpath-gemini` 0.6.1 `provider.rs:478` `sum_usage` adds the four counters, not `breakdowns` | 0.14.0 (`toolpath-gemini` 0.6.0, when breakdowns arrived) | pr/27 (`7637753`) | on wip, no PR |
| opencode session total drops token breakdowns | `toolpath-opencode` 0.5.0 `provider.rs:678` `accumulate_total`, the same | 0.14.0 (`toolpath-opencode` 0.5.0) | pr/27 (`80e8a16`) | on wip, no PR |
| Copilot input/cache totals on no step, so documents and projections lose them | `toolpath-copilot` 0.1.0 `provider.rs:312` puts `session.shutdown`'s totals in `total_usage` only | 0.15.0 | pr/27 (`9eec499`) | on wip, no PR |
| opencode resume writes a database opencode does not read ("Session not found") | `toolpath-opencode` 0.5.0 `paths.rs:21` `const DB_FILE: &str = "opencode.db"`; opencode 1.18 reads `opencode-stable.db` | all releases; a failure for users on opencode ≥ 1.18 | #363 (pr/08) | draft PR |
| opencode projected ids are refused by Zen's free tier (403) | `toolpath-opencode` 0.5.0 `project.rs:675` `format!("ses_{}", stable_hex24(seed))`, not opencode's `ses_` + 12 hex + 14 base62 | 0.13.0 (`toolpath-opencode` 0.4.0) | #365 (pr/10) | draft PR |

## Notes
- Three of these were not regressions: the gemini, Cursor and opencode-database rows are harness upgrades the
  published reader predates. 0.26.0 still reads the older formats; it fails only for users on the newer harness.
- `toolpath-convo` 0.11.1 already has `TokenUsage::breakdowns`. The two breakdown rows are therefore real losses in
  the released total, not a field that did not exist yet.
- Unrelated open PRs touch the same code: #339 (gemini additive token classes) and #353 (kind v1.2.0). None of them
  contains these fixes.
- How it was checked: each fix commit's removed lines are the signature of its bug, and those lines were searched
  for in the unpacked tarballs. path-cli 0.13.0, 0.14.0, 0.15.0 and 0.16.1, and the crate versions they depend on,
  were checked the same way for the "since" column.
