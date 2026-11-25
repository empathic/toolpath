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
- Provider-specific data that doesn't fit the generic IR lives on your
  own native message type's `extra: HashMap<String, Value>` field (see
  `toolpath-gemini::types::GeminiMessage.extra`), *not* on
  `toolpath_convo::Turn` — as of #375, `Turn` has no generic extras map
  at all (checked; Gemini's own `project.rs` doesn't read one either,
  it synthesizes from `args`/`result`). A same-harness round trip that
  needs to preserve something `Turn`'s typed fields don't carry has to
  either add a typed `Turn` field (a cross-cutting change, see #375)
  or accept the loss and say so — there is currently no generic
  passthrough slot. (Earlier drafts of this doc said
  `Turn.extra["<harness>"]`; that was aspirational, not a field that
  exists.)
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

1. **Drop foreign-namespace extras.** If your projector's `project()`
   signature only takes a `&ConversationView` (the normal case — see
   the Prerequisites note above), there usually isn't a foreign extras
   map reachable to drop: `Turn` doesn't carry one. The MUST is really
   "never reconstruct another harness's native fields from a foreign
   `Turn`" — e.g. don't invent a plausible-looking `claude` block when
   projecting into Gemini just because some IR field happens to be
   named similarly. See `toolpath-gemini::project::split_gemini_extras`
   for the one case where a real extras map *is* in play: Gemini's own
   `GeminiMessage.extra` (native-type-to-native-type, same harness).
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

In `crates/path-cli/src/cmd_export.rs` (this file lived at
`crates/toolpath-cli/src/cmd_export.rs` when this guide was first
written; `toolpath-cli` is now a separate, deprecated release shim —
don't follow the old path):

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
results, errors), no foreign-harness fields reconstructed from a
foreign `Turn`, harness-native extras preserved when your own native
type carries them, serde round-trip of the projected output through
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

Not every harness has a `--list-sessions` flag. When it doesn't, its
own listing *store* is the equivalent check: read it with your own
`list_conversations`/`list_summaries` reader and confirm the projected
id shows up there, the same way a human using that harness's own
UI/picker would find it. Antigravity has no `--list-sessions`; its
`conversation_summaries.db` is the listing (see
`docs/agents/formats/antigravity.md`), so step B there is "read the
summaries row back" plus the probing-question check:

```bash
agy --conversation <uuid> -p "in one sentence, what was the most-used tool in this session?"
```

(no separate list-check command exists to run first).

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

This assumes a JSON-lines/JSON-object format. For a harness whose
store is SQLite with protobuf blobs (no field names on the wire, no
`json.load`), the equivalent diff reads structure rather than keys —
the signals are the same, the extraction is different:

```python
# Equivalent for a SQLite + protobuf store (no protoc/.proto needed —
# this only needs to tell shapes apart, not decode field *meanings*).
import sqlite3, collections

def wire_fields(buf):
    """Every (field_number, wire_type) tag at the TOP LEVEL of buf,
    skipping over each value's bytes without interpreting them."""
    out, i = [], 0
    while i < len(buf):
        tag = 0; shift = 0
        while True:
            b = buf[i]; i += 1
            tag |= (b & 0x7f) << shift; shift += 7
            if not (b & 0x80): break
        field, wire = tag >> 3, tag & 7
        out.append((field, wire))
        if wire == 0:  # varint
            while buf[i] & 0x80: i += 1
            i += 1
        elif wire == 2:  # length-delimited
            n = 0; shift = 0
            while True:
                b = buf[i]; i += 1
                n |= (b & 0x7f) << shift; shift += 7
                if not (b & 0x80): break
            i += n
        elif wire == 1: i += 8
        elif wire == 5: i += 4
    return out

def stats(db_path, conversation_id, label):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    rows = conn.execute(
        "SELECT idx, step_type, status, step_payload FROM steps ORDER BY idx"
    ).fetchall()
    print(f"\n=== {label} ({conversation_id}) ===")
    print(f"  steps: {len(rows)}")
    print(f"  step_type histogram: {dict(collections.Counter(r[1] for r in rows))}")
    print(f"  status histogram: {dict(collections.Counter(r[2] for r in rows))}")
    # top-level field-tag histogram per step_type -- this is the SQLite/
    # protobuf analog of "foreign top-level msg fields": a field number
    # the real file never has at a given step_type, that yours does (or
    # vice versa), is the same class of asymmetry a JSON diff would flag
    # as pollution or a missing synthesizer.
    by_type = collections.defaultdict(collections.Counter)
    for _, step_type, _, payload in rows:
        for field, _wire in wire_fields(payload):
            by_type[step_type][field] += 1
    for step_type, hist in sorted(by_type.items()):
        print(f"  step_type {step_type} top-level fields: {dict(hist)}")

stats('<path-to-real-conversation.db>', '<real-uuid>', 'REAL')
stats('<path-to-projected-conversation.db>', '<projected-uuid>', 'PROJECTED')
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
9. **Provider-issued signatures can't be forged.** Some harnesses sign
   part of what they send back to the model — Claude's
   `ThinkingSignature` (`toolpath_convo::Turn.thinking_signature`,
   added by #375), Antigravity's `thought_signature` on a tool call and
   `thinking_signature` on a generation. These are opaque bytes only
   the issuing provider's API can produce or validate; a projector
   cannot synthesize a passing one. The safe default is to **omit**
   the field entirely rather than invent a value — a missing signature
   fails predictably (or is tolerated, which you find out at step 7),
   where a fabricated one fails unpredictably or, worse, might get
   treated as valid by a lenient checker and sent back to a provider
   that then rejects the *next* turn. Only re-emit a signature when
   it's your own harness's and the text it covers hasn't changed (a
   same-harness round trip): see `ThinkingSignature::value_for`'s
   hash-check for the general shape of that test, and
   `docs/agents/formats/antigravity.md`'s Pitfalls section (gotcha 4)
   for a harness that can't do even that today, because nothing
   downstream of the reader currently carries the original bytes far
   enough to re-emit them.
10. **Closed model enums change per release.** A harness that encodes
    its model as a small integer against a vocabulary it doesn't
    publish (Antigravity's `exa.codeium_common_pb.Model`) forces every
    projected session to pick *some* value the current binary
    recognizes, with no way to know if an old or made-up one is
    silently accepted, migrated, or rejected. Decide, and say so in the
    constant's name: carry the value through on a same-harness round
    trip if your reader captured it; otherwise synthesize one observed
    on a real run of a current version, name it `SYNTHESIZED_*`, and
    treat "does the binary accept it" as an explicit step-7 question,
    not an assumption either way.
11. **Multi-store consistency.** A harness whose session isn't one
    file — a summaries/index cache, a last-resumed pointer, a
    multi-file layout — needs every store that names or indexes the
    conversation written together, from one id, or its own CLI will
    disagree with itself about what's resumable (`--resume <uuid>`
    finds it, `-c`/`--continue` doesn't, or vice versa). Treat "which
    stores name this conversation" as part of the format doc (step 5),
    and write all of them in one call on the projector's output type —
    see `AntigravityStore::write_to` for a worked example (the `.db`,
    the summaries row, and a cwd-keyed last-used cache file, from one
    conversation id).

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
- `crates/path-cli/src/cmd_export.rs` — the `Gemini` variant,
  three-mode dispatch, integration tests
- `docs/agents/formats/gemini.md` — on-disk format including the
  Session resolution and Round-trip fidelity gotchas sections

## Concrete: Antigravity, a second worked example

Gemini is JSON, single/multi-file, one slot per project. Antigravity
(`toolpath-antigravity`) is the opposite shape on every one of those
axes — SQLite with protobuf blobs, no per-project slot at all, three
stores that must agree, a closed model enum, and provider-issued
signatures a projector genuinely cannot forge. Useful as the "how much
of this guide survives a harness that looks nothing like Gemini"
check:

- `crates/toolpath-antigravity/src/project.rs` — `native_name`,
  `resolve_tool_name` (step 1 and the MUSTs of step 2), applied to a
  harness with no `Turn.extra` equivalent available at all (the
  Prerequisites section's correction above is written from this
  crate's experience)
- `crates/toolpath-antigravity/src/proto.rs` — the protobuf encoder
  the projector needs, deliberately **not reinvented**: the wire
  format and function shapes (`put_varint`/`put_bytes_field`) mirror
  `toolpath_cursor::proto`'s `put_varint`/`put_bytes` exactly, because
  it's the same problem (write raw protobuf without a generated
  client) toolpath had already solved once
- `crates/toolpath-antigravity/tests/projection_roundtrip.rs` — the
  step-6 contract test for a non-JSON format, plus a cross-harness
  case (a real Claude fixture projected into Antigravity, read back
  through Antigravity's own reader) with two declared tool-name gaps
  rather than a clean 100%
- `docs/agents/formats/antigravity.md` — on-disk format, including a
  Round-trip gotchas section written specifically around pitfalls 9–11
  above (signatures, the closed enum, multi-store consistency) with
  what the projector actually does about each, not just what's hard
  about them
- **Not done**: the `path p export antigravity` CLI variant (step 4)
  and step 7's live end-to-end check — library-level work landed first
  on purpose (see the task report this stack came from) rather than
  wiring a CLI surface under time pressure; the gap is recorded, not
  silently skipped.
