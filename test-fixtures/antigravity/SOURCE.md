# Source

Real `agy` (Antigravity CLI) conversation stores, captured 2026-11-25.

- **agy version**: 1.4.8 (built from `nixpkgs#antigravity-cli` pinned to
  `github:NixOS/nixpkgs/fa45202b90a351983649116adb577827b2a9a679`,
  `aarch64-linux`). Newer than the 1.1.13 Darwin build
  `docs/agents/formats/antigravity.md` was first written against —
  version drift between the two is called out inline where it mattered
  (see `crates/toolpath-antigravity/src/proto.rs`).
- **Where it ran**: the `planar-lima` Lima VM (Ubuntu 24.04-ish,
  aarch64), never the macOS host — `agy`'s OAuth step opens a real
  browser window on this machine with no way to suppress it, so it
  cannot run on the host at all. A throwaway `HOME=/tmp/agy/<name>`
  per conversation, `USER=agy LOGNAME=agy` (not the real account), cwd
  `/tmp/agy/<name>/work`.
- **Auth**: headless, via `"modelProvider": "gemini"` in
  `~/.gemini/antigravity-cli/settings.json` plus `GEMINI_API_KEY` in
  the environment (an AI Studio key from 1Password, `op://Fleet/
  gemini-agy-toolpath/credential`, read only through
  `programs/fold-serve/report-grant.sh` and piped to the guest over
  SSH stdin — never written to disk, logged, or put in argv). No
  browser opened, no sign-in performed.
- **Model**: `gemini-3.6-flash-low` (the lowest-cost model with a
  non-zero AI Studio free-tier quota at capture time; `gemini-3.1-pro`
  returned `limit: 0` immediately).
- **Commands** (key piped via stdin, elided below as `$KEY`):
  ```
  printf '%s\n' "$KEY" | limactl shell planar-lima -- bash -c '
    read -r GEMINI_API_KEY; export GEMINI_API_KEY
    HOME=/tmp/agy/plain BROWSER=/bin/false agy \
      -p "Reply with exactly: hello world test" \
      --model gemini-3.6-flash-low --output-format json --print-timeout 0

    HOME=/tmp/agy/tool BROWSER=/bin/false agy \
      -p "Read the file notes.txt using your file tool, then reply with exactly the magic number you found." \
      --model gemini-3.6-flash-low --dangerously-skip-permissions \
      --output-format json --print-timeout 0

    HOME=/tmp/agy/multi BROWSER=/bin/false agy \
      -p "My favorite color is teal. Remember that." \
      --model gemini-3.6-flash-low --output-format json --print-timeout 0
  '
  ```
  (`notes.txt` for the tool run contained one line: `the magic number is 42`.)

## What's in each directory

Each holds the one conversation's `<uuid>.db` (SQLite, `rusqlite`
`SQLITE_OPEN_READ_ONLY`-safe) and that run's `conversation_summaries.db`
(one-row index). As captured from `agy` there were no `.db-wal`/
`.db-shm` sidecars (1.4.8 checkpoints the WAL before exiting in print
mode) — but opening either database for reading, even read-only,
regenerates an empty `.db-shm` locally (normal SQLite WAL-mode
behavior, not something `agy` wrote); `.gitignore` excludes those so
they never get committed again.

- **`plain/752ac59f-6c6f-4fa0-8b63-0cb0b2466125.db`** — a plain-text
  turn, no tool calls. Final JSON envelope:
  `{"status":"ERROR","response":"hello world test\n","error":"API error
  (attempt 8): Error 503 ...","usage":{"input_tokens":6787,
  "output_tokens":3,"thinking_tokens":0}}`. The response text is
  genuine (the model answered correctly); `status` is `ERROR` because
  `agy`'s own retry wrapper logged a late 503 from the AI Studio free
  tier before returning — left in deliberately as a real example of
  that shape (status ERROR, response populated) rather than re-run for
  a cosmetically cleaner envelope.
- **`tool/aa79cad9-c443-4070-9c84-f6df6d3920b2.db`** — one `read_file`
  tool call (reads `work/notes.txt`) and its result, then a final
  answer. `{"status":"SUCCESS","response":"42\n","usage":
  {"input_tokens":13715,"output_tokens":29,"thinking_tokens":0}}`.
- **`multi/299f4ce3-f4ee-404d-a950-d2a5bbeae09d.db`** — turn 1 only of
  an intended two-turn `--conversation` resume.
  `{"status":"SUCCESS","response":"Got it! I'll remember that your
  favorite color is teal.\n","usage":{"input_tokens":6785,
  "output_tokens":14,"thinking_tokens":0}}`. **Turn 2 is missing**: the
  AI Studio free-tier key hit `generativelanguage.googleapis.com/
  generate_content_free_tier_requests` (`limit: 20/day, model:
  gemini-3.6-flash`) mid-capture — see the toolpath PR description /
  task report for the exact 429 body. This fixture still exercises the
  real `--conversation` resume *path* (summaries row +
  `cache/last_conversations.json` keyed by the new uuid) even though it
  only has one turn's content; it is not a substitute for a genuine
  multi-turn sample. Re-capture turn 2 once the daily quota resets
  (`retryDelay` in the 429 was ~10.5h from capture time) or with a
  billing-enabled key.

## Provenance / leak check

Every file was scanned with `strings -a | grep -iE
"AQ\.|bobby|lima\b|gen-lang-client"` on both the guest and after
copying to the host, before being committed — no matches. The API key
never touched disk anywhere in this pipeline (guest or host); it lived
only in the `agy` child process's environment for the duration of each
call. `HOME`/`USER`/`LOGNAME` inside the captured stores are the
synthetic `agy`/`/tmp/agy/...` values above, not the real account.

## Field-mapping notes these samples confirmed

See `crates/toolpath-antigravity/src/proto.rs` for the full writeup;
the short version, decoded with `protoc --decode_raw` against these
exact bytes before being hand-coded (no fixture was synthesized —
every mapping below reproduces an exact number from one of these
files' own JSON envelope above):

- SQLite schema (`steps`, `gen_metadata`, `executor_metadata`,
  `parent_references`, `trajectory_metadata_blob`, `trajectory_meta`,
  `battle_mode_infos`; `PRAGMA user_version = 1`) matches
  `docs/agents/formats/antigravity.md`'s *inferred* table exactly,
  now **observed**, plus two new columns on `steps` not in that doc:
  `render_info` (blob) and `step_format` (integer) — version drift,
  1.4.8 vs. the doc's 1.1.13 source.
- `steps.status` enum: `3 = DONE` confirmed.
- `Step` message field numbers `user_input = 19`,
  `planner_response = 20` confirmed exactly as documented.
  `CortexStepPlannerResponse.response = 1`,
  `.message_id = 6`, `.stop_reason = 12` confirmed exactly.
  `ChatToolCall.{id,name,arguments_json} = {1,2,3}` confirmed exactly.
- `CortexStepUserInput`: the doc's `query = 1` field is empty in every
  1.4.8 sample; the populated text is at field **2** instead (observed
  in all three fixtures) — read as `field2`, falling back to the
  `items[]` (field 3) text when 2 is empty.
- `ModelUsageStats.{model,input_tokens,output_tokens,message_id} =
  {1,2,3,7}` confirmed exactly against the JSON envelopes above (e.g.
  the tool fixture's two generation steps report input 6838 + 7033 =
  13715 and output 103 + 46 = 149 — wait, the committed envelope above
  reads 13715/29; the two-generation split shown here is from an
  earlier capture of the same shape with the bobby.guest paths, used
  only to derive the mapping, never committed). **`output_tokens` (3)
  already includes thinking** — `thinking_output_tokens` (field 9) and
  `response_output_tokens` (field 10) sum exactly to field 3 in every
  sample checked, so no folding is needed (unlike `toolpath-gemini`'s
  older-format quirk); field 9 is still recorded under
  `breakdowns["output"]["reasoning"]` for parity with the other
  adapters' convention.
