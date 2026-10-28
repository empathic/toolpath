# Show available recipes
help:
    @just --list

# Run clippy lints across the workspace
clippy:
    cargo clippy --workspace -- -D warnings

# Format all code (Rust + site)
fmt:
    ./scripts/fmt.sh

# Auto-format then run the full CI verification (dev loop)
check *GATES:
    ./scripts/check.sh {{GATES}}

# Run the same quality gates CI runs, without auto-format
ci *GATES:
    ./scripts/quality_gates.sh {{GATES}}

# Run the site dev server (eleventy + wasm watcher)
site:
    ./scripts/site.sh

# Re-fetch the Pathbase OpenAPI spec into pathbase-client
refresh-openapi:
    ./scripts/refresh-pathbase-openapi.sh

# Live smoke test against a Pathbase deployment (defaults to https://pathbase.dev)
test-pathbase-live *URL:
    ./scripts/test-pathbase-live.sh {{URL}}

# Path sync acceptance tests against a running Pathbase. Set
# TEST_PATHBASE_URL (its API base, e.g. http://127.0.0.1:8788) and
# TEST_PATHBASE_TOKEN (a CLI token for a user there, from
# `POST /api/v1/auth/cli/redeem`)
acceptance:
    cargo test -p path-cli --test sync_acceptance -- --ignored --nocapture

# Verify all workspace crates can be published (dry run; safe)
release-check *FLAGS:
    ./scripts/release.sh {{FLAGS}}

# Publish all workspace crates to crates.io for real
release-publish *FLAGS:
    ./scripts/release.sh --execute {{FLAGS}}
