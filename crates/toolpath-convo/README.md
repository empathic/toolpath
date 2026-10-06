# toolpath-convo

Provider-agnostic conversation types and traits for AI coding tools.

This crate defines a common vocabulary for representing conversations
from any AI coding assistant (Claude, Codex, OpenCode, etc.) without
coupling consumer code to provider-specific data formats.

Write your conversation analysis once, swap providers without changing a line.

## Overview

**Types** define the common data model:

| Type | What it represents |
|---|---|
| `Turn` | A single conversational turn (text, thinking, tool uses, model, tokens, environment, delegations) |
| `Role` | Who produced the turn: `User`, `Assistant`, `System`, `Other(String)` |
| `ConversationView` | A complete conversation: ordered turns, timestamps, aggregate usage, files changed |
| `ConversationMeta` | Lightweight metadata (no turns loaded) |
| `ToolInvocation` | A tool call within a turn, with optional `ToolCategory` classification |
| `ToolResult` | The result of a tool call |
| `ToolCategory` | Toolpath's classification ontology: `FileRead`, `FileWrite`, `FileSearch`, `Shell`, `Network`, `Delegation` |
| `TokenUsage` | Input/output/cache token counts |
| `EnvironmentSnapshot` | Working directory and VCS branch/revision at time of a turn |
| `DelegatedWork` | A sub-agent delegation: prompt, nested turns, result |
| `WatcherEvent` | A `Turn` (new), `TurnUpdated` (enriched with tool results), or `Progress` event — with `as_turn()`, `as_progress()`, `is_update()`, `turn_id()` helpers for ergonomic dispatch |

**Traits** define how providers expose their data:

| Trait | What it does |
|---|---|
| `ConversationProvider` | List and load conversations from any source |
| `ConversationWatcher` | Poll for new conversational events |

## Usage

```rust
use toolpath_convo::{ConversationView, ConversationProvider, Role, ToolCategory};

// Provider crates implement ConversationProvider.
// Consumer code works against the trait:
fn show_conversation(provider: &dyn ConversationProvider) {
    let view = provider.load_conversation("/path/to/project", "session-id")
        .unwrap();

    if let Some(title) = view.title(80) {
        println!("# {}", title);
    }

    // Session-level summary
    if let Some(usage) = &view.total_usage {
        println!("Tokens: {:?} in / {:?} out", usage.input_tokens, usage.output_tokens);
    }
    println!("Files changed: {:?}", view.files_changed);

    for turn in &view.turns {
        println!("[{}] {}", turn.role, turn.text);

        // Environment context
        if let Some(env) = &turn.environment {
            println!("  cwd: {:?}, branch: {:?}", env.working_dir, env.vcs_branch);
        }

        // Tool classification
        for tool_use in &turn.tool_uses {
            println!("  {} ({:?})", tool_use.name, tool_use.category);
        }

        // Sub-agent delegations
        for d in &turn.delegations {
            println!("  delegated: {}", d.prompt);
            if let Some(result) = &d.result {
                println!("    -> {}", result);
            }
        }
    }
}
```

## Tool classification

`ToolCategory` is toolpath's own ontology for what a tool invocation does,
independent of provider-specific naming. Provider crates map their tool
names into these categories; `None` means the tool isn't recognized.

| Category | Meaning |
|---|---|
| `FileRead` | Read a file — no side effects |
| `FileWrite` | Write, edit, create, or delete a file |
| `FileSearch` | Search or discover files by name or content pattern |
| `Shell` | Shell or terminal command execution |
| `Network` | Web fetch, search, API call |
| `Delegation` | Spawn a sub-agent or delegate work |

Consumers can filter by category without knowing provider tool vocabularies:

```rust,ignore
let writes: Vec<_> = turn.tool_uses.iter()
    .filter(|t| t.category == Some(ToolCategory::FileWrite))
    .collect();
```

## Shell writes

Agents often write files through their shell tool rather than a write
tool. `shell_writes::parse_script` reads a shell script's text and reports,
per simple command, the heredoc writes it makes (`cat <<'EOF' > file`,
`cat >> file <<EOF`, `tee [-a] file <<EOF`) and the patches it feeds to
`apply_patch <<'EOF'`, with literal `cd`s folded into relative paths.
`shell_writes::parse_argv` reads a program's argv the same way: a shell's
`-c` script is parsed, `apply_patch PATCH` is a patch, and any other
program is one `ShellItem::Other`. It is pure and conservative: anything
it cannot follow exactly (subshells, command substitution, compound
commands) is reported as `ShellItem::Other`, and a write whose target
cannot be resolved as `ShellItem::Unresolved` with its reason, never
guessed at. A command that plainly writes files in a form it does not
follow (`cat > f <<A <<B`, `cat <<EOF | tee f`, `echo hi > f`,
`make 2> err.log`) is `ShellItem::Unmodeled`, listing each target (output
redirects to a file on any descriptor and `tee` file arguments, not
`/dev/` paths) as written and, when literal in a known directory,
resolved; never its content. A script it does not split is not searched
for targets. What an item means for a file change, and what outcome the
call had, is up to the provider.

Simple commands are split at `&&`, `||`, `;`, `|`, `&` and newlines, and
each write or patch carries a `StatusLink`: `Sole` (the script's exit status
is its own), `ImpliedBySuccess` (a zero exit means it ran and exited zero)
or `Independent`. A literal `cd DIR` moves later relative paths only while
an unbroken `&&` chain follows it, since a `cd` that failed before `;`
leaves the script where it was; after any other directory change (`cd $X`,
`pushd`, `source`, `eval`, an env-prefixed `cd`, …) relative targets are
unresolvable, and a patch's `dir` is `ScriptDir::Unknown`.
`ParsedScript::dir_changes` lists the commands that may change the
working directory of the shell the script runs in, for callers whose shell
persists across calls: `ScriptDir::At` (absolute) for a literal `cd /DIR`,
`ScriptDir::Unknown` for any other (`cd sub`, `pushd`, `source`, `eval`,
`trap`, `alias`, a non-literal command word such as `$CD`, or a script it
cannot match); `may_change_dir` is `true` when it is not empty.
`ParsedScript::dir_on_success` is where a zero exit status leaves that
shell: `Start` with no change, `At` (relative to the start, or absolute)
when every change is a literal `cd` in an `&&` chain from the script's
start, `Unknown` otherwise (`cd -N` and zsh's `cd +N` included).
`ParsedScript::command_words` holds each simple command's words from its
command word on. Subshells
(`( … )`, `$( … )`, backquotes, process substitution), quoted text and
heredoc bodies are skipped, so Claude Code's
`git commit -m "$(cat <<'EOF' … EOF)"` names no change. It over-reports
(a function body counts even if never called) and never under-reports,
except that functions and aliases the shell already had are invisible.

Parsing follows bash: `\r` is a word character, a line ending in an odd
number of `\` in an unquoted heredoc continues before the terminator test,
and `<<-` strips leading tabs from the terminator line as well as the body.
A tag counts as quoted when any part of it is quoted or escaped (`'T'`,
`"T"`, `\T`); a target is non-literal when it has `$`, a leading `~`, a
glob or a brace outside quotes.

`shell_writes::parse_patch` reads the files of a V4A patch (the
`*** Begin Patch` … `*** End Patch` text Codex's `apply_patch` takes):
one `PatchFile` per `*** Add File:`, `*** Update File:` (with its
`*** Move to:`) or `*** Delete File:`, an added file carrying its content.
Update hunks are not unified diffs, so an update carries none.
`HeredocPatch::files` reads a patch found in a script.

```rust,ignore
use toolpath_convo::shell_writes::{parse_script, ShellItem};

for item in parse_script("cat <<'EOF' > notes.md\nhello\nEOF").items {
    if let ShellItem::Write(w) = item {
        assert_eq!((w.path.as_str(), w.body.as_str()), ("notes.md", "hello\n"));
    }
}
```

## Watching

Dispatch on `WatcherEvent` with `match` — three variants, exhaustive:

```rust,ignore
use toolpath_convo::{ConversationWatcher, WatcherEvent};

for event in watcher.poll()? {
    match &event {
        WatcherEvent::Turn(turn) => ui.add_turn(turn),
        WatcherEvent::TurnUpdated(turn) => ui.replace_turn(turn),
        WatcherEvent::Progress { kind, data } => ui.show_progress(kind, data),
    }
}
```

Convenience methods (`as_turn()`, `as_progress()`, `is_update()`, `turn_id()`)
are available for cases where the distinction between `Turn` and `TurnUpdated`
collapses — e.g. a formatting pipeline that takes a turn + flag:

```rust,ignore
// When Turn/TurnUpdated go through the same path:
if let Some(turn) = event.as_turn() {
    format_turn(turn, event.is_update());
}

// Keying/dedup without matching two variants:
if let Some(id) = event.turn_id() {
    seen.insert(id);
}
```

Provider-specific metadata lives in `Turn.extra`, namespaced by provider (e.g. `turn.extra["claude"]`). This keeps the common schema clean while giving consumers opt-in access to provider internals.

## Provider implementations

| Provider | Crate |
|---|---|
| Claude Code | [`toolpath-claude`](https://crates.io/crates/toolpath-claude) |

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core provenance types and query API
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- Claude conversation provider
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`path-cli`](https://crates.io/crates/path-cli) -- unified CLI (`cargo install path-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
