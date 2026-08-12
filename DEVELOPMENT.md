# Development

## Building and testing

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Requires Rust 1.85+ (edition 2024); the exact toolchain is pinned via
`rust-toolchain.toml`.

Validate the example documents:

```bash
for f in examples/*.json; do cargo run -p path-cli -- p validate --input "$f"; done
```

## Agent session format notes

[docs/agents/formats/](docs/agents/formats/README.md) records our
understanding of each agent's on-disk session format, written down
while building the derive crates. That includes a twelve-part reference
for Claude Code's JSONL (envelope, entry types, session chains,
compaction, a writing-compatible guide), the writer contract the
Copilot CLI loader appears to enforce, and single-file references for
Codex, Gemini, opencode, Cursor, and Pi.

None of these formats are documented by their vendors. The notes come
from observed behavior of specific versions: they are works in
progress, with gaps, and the agents can change their formats at any
time. Corrections are welcome. If you're building your own session
tooling, they're a useful starting point even if you never run our
code.

The notes are meant to stay in sync with the derive crates. If you
change what a crate reads or writes, update the matching document under
`docs/agents/formats/`.
