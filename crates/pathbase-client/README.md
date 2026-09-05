# pathbase-client

Auto-generated typed Rust client for the [Pathbase](https://pathbase.dev) HTTP API.

The client is derived at build time from `openapi.json` (committed alongside the crate) via [progenitor](https://github.com/oxidecomputer/progenitor). Spec drift surfaces as a `cargo build` failure rather than a runtime "HTML where JSON was expected" error.

## What's in here

```rust
use pathbase_client::Client;

let client = Client::new("https://pathbase.dev");
// `client.health()`, `client.create_anon_path(...)`, `client.create_path(...)`, etc.
```

The full surface mirrors the OpenAPI document. Only the operations actually documented there are available — including the CLI auth-redeem endpoint (`POST /api/v1/auth/cli/redeem`), which is generated as `cli_redeem` and is what `path-cli` calls.

## Build pipeline

1. The committed spec is OAS 3.1; progenitor's `openapiv3` dependency only understands 3.0.
2. `build.rs` reads the spec, downgrades 3.1 idioms in-memory (`"type": ["string", "null"]` → `"type": "string", "nullable": true`; injects permissive schemas for empty media-type bodies), then hands the result to `progenitor::Generator`.
3. The generated code lands in `$OUT_DIR/pathbase_client.rs` and is included from `src/lib.rs`.

## Refreshing the spec

From the workspace root:

```bash
scripts/refresh-pathbase-openapi.sh                              # defaults to https://pathbase.dev
PATHBASE_URL=https://staging.example.com scripts/refresh-pathbase-openapi.sh
```

The script overwrites `crates/pathbase-client/openapi.json` (the same file `build.rs` reads). After refresh, `cargo build -p pathbase-client` regenerates the client against the new spec.

## License

Apache-2.0.
