# Show available recipes
help:
    @just --list

# Run clippy lints across the workspace
clippy:
    cargo clippy --workspace --features path-cli/develop,toolpath/verify -- -D warnings

# Format all code (Rust + site)
fmt:
    ./scripts/fmt.sh

# Auto-format then run the full CI verification (dev loop)
check *GATES:
    ./scripts/check.sh {{GATES}}

# Run the same quality gates CI runs, without auto-format
ci *GATES:
    ./scripts/quality_gates.sh {{GATES}}

# The snapshot baseline via `path d baseline`: check (default), capture --all, diff <set>, shed <set>, list
[positional-arguments]
baseline *ARGS:
    @if [ "$#" -eq 0 ]; then set -- check; fi; cargo run -q -p path-cli --features develop -- d baseline "$@"

# One agent's cell through the record/replay server: just webmock <agent> [replay|record] --cell <dir> -- <agent args>
[positional-arguments]
webmock AGENT MODE="replay" *ARGS:
    @cargo build -q -p toolpath-webmock --bin webmock && cargo run -q -p path-cli --features develop -- d webmock run "$@"

# Replay every agent's committed cassette through its real CLI, offline (tests/replay_agents.rs).
# Needs the pinned agent CLIs on PATH (claude copilot pi opencode gemini). Builds the cells from the
# codex golden into CELLS, then runs the tests with every agent required: a skip fails.
replay-agents CELLS="target/replay-cells":
    cargo build -q -p path-cli
    scripts/replay-cells.sh "${CARGO_TARGET_DIR:-target}/debug/path" {{CELLS}}
    WEBMOCK_CELLS="$(cd {{CELLS}} && pwd)" WEBMOCK_REQUIRE=claude,copilot,pi,opencode,gemini \
      cargo test -q -p toolpath-webmock --features zstd-bodies --test replay_agents -- --test-threads=1

# Regenerate the goldens object model (docs/model/model.json) via `path d model`
[positional-arguments]
model *ARGS:
    @cargo build -q -p toolpath-model && cargo run -q -p path-cli --features develop -- d model "$@"

# Run the site dev server (eleventy + wasm watcher)
site:
    ./scripts/site.sh

# Re-fetch the Pathbase OpenAPI spec into pathbase-client
refresh-openapi:
    ./scripts/refresh-pathbase-openapi.sh

# Live smoke test against a Pathbase deployment (defaults to https://pathbase.dev)
test-pathbase-live *URL:
    ./scripts/test-pathbase-live.sh {{URL}}

# Verify all workspace crates can be published (dry run; safe)
release-check *FLAGS:
    ./scripts/release.sh {{FLAGS}}

# Publish all workspace crates to crates.io for real
release-publish *FLAGS:
    ./scripts/release.sh --execute {{FLAGS}}

# A workflow locally through act on the pinned runner image (dry run until ci/act.lock is approved): just act replay claude
act WORKFLOW AGENT:
    cargo xtask act {{WORKFLOW}} {{AGENT}}

# One agent's replay test, key-free (what .github/workflows/replay.yml runs)
replay AGENT:
    cargo test -p toolpath-webmock --test replay_agents -- replay_{{AGENT}}

# Render docs/agents/SUPPORT.md from docs/agents/support.toml (the supported-agents list)
agents:
    cargo xtask agents

# Pinned agent CLIs and tools (list, verify; see crates/xtask)
pins *ARGS:
    cargo xtask pins {{ARGS}}
