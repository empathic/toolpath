# pathbase-client

Auto-generated typed Rust client for the [Pathbase](https://pathbase.dev) HTTP API.

The client is derived at build time from `schema/pathbase-openapi.json` (in the workspace root) via [progenitor](https://github.com/oxidecomputer/progenitor). Spec drift surfaces as a `cargo build` failure rather than a runtime "HTML where JSON was expected" error.

## What's in here

```rust
use pathbase_client::Client;

let client = Client::new("https://pathbase.dev");
// `client.health()`, `client.create_anon_path(...)`, `client.create_path(...)`, etc.
```

The full surface mirrors the OpenAPI document. Only the operations actually documented there are available. The CLI auth-redeem endpoint (`POST /api/v1/auth/cli/redeem`) is real in production but absent from the spec, so it is **not** present in this client — `path-cli` keeps a hand-rolled redeem implementation.

## Build pipeline

1. The committed spec is OAS 3.1; progenitor's `openapiv3` dependency only understands 3.0.
2. `build.rs` reads the spec, downgrades 3.1 idioms in-memory (`"type": ["string", "null"]` → `"type": "string", "nullable": true`; injects permissive schemas for empty media-type bodies), then hands the result to `progenitor::Generator`.
3. The generated code lands in `$OUT_DIR/pathbase_client.rs` and is included from `src/lib.rs`.

## Refreshing the spec

```bash
scripts/refresh-pathbase-openapi.sh        # uses pathbase-dev.fly.dev
PATHBASE_URL=https://pathbase.dev scripts/refresh-pathbase-openapi.sh
```

After refresh, `cargo build -p pathbase-client` regenerates against the new spec.

## License

Apache-2.0.
