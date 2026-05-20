# Adding `path p export <harness>` for a new harness

How to add projection (toolpath document → harness-native session) for
a harness that already has forward derivation. Distilled from the
Gemini implementation; should generalize cleanly to Codex, opencode,
and Pi.

The forward path (native → `ConversationView`) is out of scope here —
we assume a working `Provider` already exists.

## Mental model

```
              Provider::to_view          derive_path
native log ─────────────────▶ View ─────────────────▶ Path
                                ◀───────────────────
native log ◀─── Projector::project ◀─── extract_conversation
```

`ConversationView` is the IR. `derive_path` / `extract_conversation`
live in `toolpath-convo` and you should not reimplement them. Your job
is the rightmost arrow.

The IR canonicalizes the *classification* (`ToolCategory`), not the
*name*. `ToolInvocation.name` is preserved verbatim from the source
harness; remapping to a target harness's vocabulary happens in the
projector.

## Prerequisites

Before starting:

- `Provider::to_view` exists and populates `ToolInvocation.category`
  (this is what the projector routes on for cross-harness translation).
- Provider-specific data that doesn't fit `Turn` lives under
  `Turn.extra["<harness>"]`. The forward path stashed it there;
  the projector pulls it back out.
- The on-disk format is reasonably documented at
  `docs/agents/formats/<harness>.md`. If not, write that first —
  the projector is the place where every quirk shows up.

## Steps

### 1. Reverse-map tool names: `native_name(category, args)`

In `toolpath-<harness>/src/provider.rs`, alongside the existing
`tool_category(name)`:

```rust
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("run_shell_command"),
        ToolCategory::FileWrite => Some(
            // Edit-shape vs Write-shape — disambiguate by args.
            if args.get("old_string").is_some() { "replace" }
            else { "write_file" }
        ),
        ToolCategory::FileRead => Some(if args.get("file_paths").is_some() {
            "read_many_files"
        } else if args.get("path").is_some() && args.get("file_path").is_none() {
            "list_directory"
        } else {
            "read_file"
        }),
        // ...
    }
}
```

`ToolCategory` is too coarse for FileWrite (Edit / Write / MultiEdit
all collapse) and FileRead (read_file / read_many_files /
list_directory), so inspect `args` to pick the native name whose arg
shape best matches the call. The receiving harness's UI keys icons,
labels, and category routing off these names; getting them wrong means
calls render as "unknown tool".

### 2. Implement the projector

`toolpath-<harness>/src/project.rs`:

```rust
pub struct HarnessProjector {
    pub project_path: Option<String>,
    // … other config fields the on-disk format needs …
}

impl ConversationProjector for HarnessProjector {
    type Output = NativeConversation;
    fn project(&self, view: &ConversationView) -> Result<Self::Output> {
        // Walk view.turns → produce native messages.
        // Walk turn.delegations → produce native sub-agent files (if applicable).
        // Pull harness-specific data from Turn.extra["<harness>"].
    }
}
```

Three things the projector MUST do:

1. **Drop foreign-namespace extras.** When projecting Claude's
   conversation into Gemini, `Turn.extra["claude"]` is meaningless and
   pollutes the output JSON. Read only `Turn.extra["<your-harness>"]`;
   discard everything else. See
   `toolpath-gemini::project::split_gemini_extras` for the pattern.
2. **Remap tool names through `category` + `native_name(args)`** when
   the source name isn't already one of yours. Pass through verbatim
   when it is, so same-harness round-trips don't churn names.
3. **Synthesize required UI fields** (description, displayName, render
   hints, etc.) from `args` + `result.content` when not present in the
   IR. For Gemini→Gemini round-trips, `Turn.extra["gemini"]` carries
   the originals; for cross-harness, you have to make them up from
   what's available. See
   `toolpath-gemini::project::synthesize_description` for the
   per-tool-name dispatch and `generic_description_fallback` for the
   foreign-tool last-resort.

### 3. Library/CLI parity for session resolution

If the harness's CLI accepts session identifiers in multiple forms
(file stem AND inner session ID, for instance), the harness's library
reader should too — otherwise `path p export <harness>` followed by
`<harness>cli --resume <uuid>` works but the equivalent library round-
trip doesn't. See `toolpath-gemini::PathResolver::resolve_main_file`
for the stem-then-scan-and-match pattern.

### 4. Add the CLI variant

In `crates/toolpath-cli/src/cmd_export.rs`:

```rust
pub enum ExportTarget {
    // existing variants...
    Harness {
        #[arg(short, long)]
        input: String,

        #[arg(short, long)]
        project: Option<PathBuf>,

        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
}
```

Three modes (mirror Claude's variant exactly):

- **`--project DIR`** — write the resume-ready on-disk layout under
  `~/.<harness>/…` so the harness's CLI invoked from `DIR` can resume
  it. Bake any cwd-derived metadata (project hashes, working
  directories) into the projector here.
- **`--output FILE`** — write the harness's primary file to `FILE`.
  Multi-file formats land secondary files (sub-agents, attachments)
  in a sibling location next to it.
- **Neither** — pretty-print the primary file to stdout. Warn on
  stderr if a multi-file format would lose data.

Refactor projection into a helper (`build_<harness>_conversation`) so
all three modes share it. The Gemini implementation in `cmd_export.rs`
is a complete worked example.

### 5. Document the on-disk format

`docs/agents/formats/<harness>.md`. Capture especially the load-
bearing details the projector tripped over:

- Filename conventions enforced by the CLI (Gemini requires `session-`
  prefix; the CLI filters before opening files).
- How the CLI's resume command resolves an identifier. Filename stem?
  Inner session ID? Hash? Multiple paths in priority order?
- Single-file vs multi-file format, and the relationships between
  files (Gemini's main file + sibling UUID dir for sub-agents).
- Required fields for the file to load at all (Gemini's `kind: "main"`
  is implicitly required by some readers).
- Round-trip fidelity gotchas (absent vs empty arrays, polymorphic
  fields, nullable vs absent semantics).

### 6. Tests

Three layers of automated tests, in order of cost:

**Projector unit tests** in `project.rs` — cheap, focused. Cover:
content shape, role mapping, tool-call construction (with and without
results, errors), foreign-namespace extras dropped, harness-native
extras preserved, serde round-trip of the projected output through
the harness's own types.

**Round-trip integration test** in `tests/projection_roundtrip.rs` —
the contract test. Walk the full chain: `native fixture → to_view →
derive_path → serialize+reparse Path → extract_conversation → project
→ native`. Assert field-by-field that the round-trip preserves
messages, roles, content text, tool calls (name, args, result text,
error status), thoughts, tokens, sub-agents. The Gemini test file is
the template.

**CLI integration test** in `cmd_export.rs::tests` — proves the
`path p export <harness>` command writes a resume-ready file that the
harness's library reader can open by the same identifier the CLI's
resume command would use. Isolate `$HOME` to a temp dir.

### 7. Live end-to-end verification

Tests passing is necessary but **not sufficient**. The unit and
round-trip tests verify the projector against the *intended* contract;
they don't tell you whether the resulting file actually loads in the
harness's CLI, whether the receiving model can read its context, or
whether the output looks like a normal session of that harness rather
than a foreign-shape one. Three checks worth running before declaring
the harness done:

**A. Run the full pipeline against a real conversation.** Pick a
non-trivial conversation from another harness (Claude is convenient
since the source is rich) and pipe it through:

```bash
# Import a real session into a Path doc
path p import claude --session <session-uuid> --project /path/to/project

# Or, if not using cache:
cargo run -q -p path-cli -- p import claude \
  --project /path/to/project --session <session-uuid> --no-cache --pretty \
  > /tmp/source.path.json

# Project into the new harness
path p export <harness> --input /tmp/source.path.json --project $(pwd)
```

The summary line should report the full message count, not zero or
some truncated subset.

**B. Verify the harness's CLI accepts it.** Three things to check:

```bash
<harness>cli --list-sessions             # session is discoverable
<harness>cli --resume <session-uuid>     # session loads without error
# In the resumed session, ask a probing question:
<harness>cli --resume <uuid> -p "in one sentence, what was the most-used tool in this session?"
```

The third check is the strongest: if the model gives a specific,
correct answer, it loaded the context AND parsed the tool-call
records. If it says "I don't have any prior context" or invents a
generic answer, the file loaded structurally but the content didn't
reach the model.

**C. Diff the projected output against a real session of the same
harness on disk.** This is where slop hides. Find a real session the
harness wrote itself (`~/.<harness>/.../*.json`) and compare key
shapes. A small Python analyzer:

```python
import json
from collections import Counter

real = json.load(open('<path-to-real-session>'))
inc  = json.load(open('<path-to-projected-session>'))

def stats(c, label):
    print(f'\n=== {label} ===')
    print(f'  messages: {len(c["messages"])}')
    types = Counter(m.get('type') for m in c['messages'])
    print(f'  types: {dict(types)}')
    names = Counter(tc['name']
                    for m in c['messages']
                    for tc in (m.get('toolCalls') or []))
    total = sum(names.values())
    print(f'  tool calls: {total}')
    for n, k in names.most_common(10):
        print(f'    {k:4d}  {n}')
    # decoration coverage
    for field in ('description', 'displayName', 'resultDisplay',
                  'renderOutputAsMarkdown'):
        have = sum(1 for m in c['messages']
                   for tc in (m.get('toolCalls') or [])
                   if tc.get(field) is not None)
        print(f'  {field}: {have}/{total}')
    # message-level pollution
    keys = Counter(k for m in c['messages'] for k in m.keys())
    foreign = [k for k in keys
               if k not in ('id', 'timestamp', 'type', 'content',
                            'tokens', 'model', 'toolCalls', 'thoughts')]
    print(f'  foreign top-level msg fields: {foreign}')

stats(real, 'REAL')
stats(inc, 'INCEPTED')
```

Quality signals to compare:

- **Tool name distribution**: are foreign names (e.g. Claude's `Bash`,
  `Edit`, `Read`) still present in the incepted output, or have they
  been remapped to the target harness's vocabulary
  (`run_shell_command`, `replace`, `read_file`)? Anything that didn't
  remap is either a missing entry in `native_name`, a missing
  `tool_category` mapping in the source harness, or a tool with no
  target-harness analog (legitimate, but flag it).
- **Decoration coverage**: how many tool calls have `description`,
  `displayName`, `resultDisplay`, `renderOutputAsMarkdown` populated
  in the real output vs ours? A real session usually has 100% on each.
  Less than 100% in ours means the synthesizers aren't covering all
  the tools.
- **Message-level pollution**: are any foreign-namespace fields
  (`claude`, `codex`, etc.) appearing as top-level keys on messages?
  They shouldn't be — that's the foreign-extras leak from pitfall #4.
- **Tokens shape**: are any fields written as `null` instead of
  absent? Real sessions tend to omit unknown fields, not null them.

When all three of these pass — pipeline produces full output, CLI
loads and the model engages with the context, and a field-coverage
diff against real sessions shows no glaring asymmetries — the harness
is ready to ship. Anything short of all three and you're guessing.

## Pitfalls (real ones we hit)

1. **Filename conventions are load-bearing.** Gemini's CLI filters
   `chats/*.json` by `session-` stem prefix *before* opening any file.
   Mis-named files are silently invisible to `--resume`. Always check:
   does the harness's listing tool see the file?
2. **Identifier resolution often differs from filesystem layout.**
   Gemini's `--resume <uuid>` matches the inner `sessionId` field, not
   the filename stem. So a session resumable by UUID may live in a
   file whose name is unrelated.
3. **Multi-file formats need a thoughtful `--output` design.** A
   harness whose session is one main file plus a sibling sub-agent dir
   can't fit in a single output file. Either accept a directory there,
   or write the main to the file and put secondaries in a sibling
   location with a clear convention.
4. **Foreign-namespace extras silently leak.** If your message type
   uses `#[serde(flatten)]` on its extras map, anything in
   `Turn.extra` that isn't your namespace flatlands as a top-level
   message field. The projector should drop foreign namespaces
   explicitly.
5. **Tool args don't match across harnesses.** Claude's `Edit
   {file_path, old_string, new_string}` and Gemini's `replace
   {file_path, old_string, new_string, instruction}` line up; Claude's
   `Write {file_path, content}` and Gemini's `write_file {file_path,
   content}` line up. Many other pairs don't. Preserve `args`
   verbatim, map names not args.
6. **UI decoration fields feel cosmetic but aren't.** `description`,
   `displayName`, `renderOutputAsMarkdown`, `resultDisplay` —
   Gemini-style harnesses populate these on every call. Without them,
   the receiving CLI shows generic blank-ish entries. Synthesize from
   args and result text.
7. **The reader is the next surprise.** The harness's library reader
   may not resolve identifiers the way its CLI does. Either add the
   missing path or document the asymmetry. (See `resolve_main_file`
   in `toolpath-gemini`.)
8. **Don't trust commit comments about backward-compat fallbacks.**
   If a comment says "fallback for older `.path` files," verify those
   files actually exist. If no caller has ever produced the shape the
   fallback handles, it's dead code with a misleading explanation.

## Concrete: Gemini reference

For every step above there's a working, committed reference:

- `crates/toolpath-gemini/src/provider.rs` — `tool_category` +
  `native_name`
- `crates/toolpath-gemini/src/project.rs` — `GeminiProjector`,
  `split_gemini_extras`, `synthesize_*`
- `crates/toolpath-gemini/src/paths.rs` — `resolve_main_file` for
  CLI-parity identifier lookup
- `crates/toolpath-gemini/tests/projection_roundtrip.rs` — round-trip
  contract tests
- `crates/toolpath-cli/src/cmd_export.rs` — the `Gemini` variant,
  three-mode dispatch, integration tests
- `docs/agents/formats/gemini.md` — on-disk format including the
  Session resolution and Round-trip fidelity gotchas sections
