# toolpath-pi

Read [Pi](https://pi.dev/) coding-agent session logs and derive Toolpath
provenance documents.

Pi stores sessions as JSONL files at
`~/.pi/agent/sessions/--<encoded-cwd>--/<timestamp>_<uuid>.jsonl`. Each
entry forms a tree via `id`/`parentId` fields, enabling in-place branching.
Sessions can link to parent sessions via a `parentSession` file path in
the session header.

This crate implements the [`toolpath_convo::ConversationProvider`] trait
and a `derive_path` wrapper that produces a [`toolpath::v1::Path`] via
the shared derivation in [`toolpath_convo::derive_path`].

## Quick example

```rust,no_run
use toolpath_pi::PiConvo;

let manager = PiConvo::new("/Users/alex");
let session = manager
    .most_recent_session("/Users/alex/project")
    .unwrap()
    .expect("no Pi sessions");
let view = manager.to_view(&session);
```

## Listing sessions

`list_sessions` returns lightweight `SessionMeta` summaries — id, timestamp,
file path, entry count, and `first_user_message` (the first non-empty
user-prompt text). The last field is what makes the listing useful for
"pick a session by topic" surfaces like an `fzf` picker.

```rust,no_run
use toolpath_pi::PiConvo;

let manager = PiConvo::new("/Users/alex");
for meta in manager.list_sessions("/Users/alex/project").unwrap() {
    println!(
        "{}: {}",
        meta.id,
        meta.first_user_message.as_deref().unwrap_or("(no prompt)"),
    );
}
```
