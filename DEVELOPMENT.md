# Development

Requires Rust 1.85+ (edition 2024); the exact toolchain is pinned via
`rust-toolchain.toml`. Day-to-day tasks are driven by
[`just`](https://github.com/casey/just); run `just help` to list the
recipes.

## The dev loop

```bash
just check            # auto-format, then run the full CI verification
just ci               # the same quality gates CI runs, without auto-format
```

Both take gate names to narrow or exclude: the gates are `format`,
`shellcheck`, `clippy`, `test`, `doc`, `examples`, `plugin`, and
`site`, and a `-` prefix excludes one.

```bash
just ci test          # just the workspace tests
just check -site      # everything except the site build
```

Individual pieces are also available directly:

```bash
just fmt              # format all code (Rust + site)
just clippy           # clippy across the workspace, warnings denied
cargo test --workspace
```

The `examples` gate validates every document under `examples/` with
`path p validate`.

## Other recipes

```bash
just site                 # site dev server (eleventy + wasm watcher)
just refresh-openapi      # re-fetch the Pathbase OpenAPI spec into pathbase-client
just test-pathbase-live   # live smoke test against a Pathbase deployment
just release-check        # dry-run publish of all workspace crates (safe)
just release-publish      # publish to crates.io for real
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
