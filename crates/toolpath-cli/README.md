# toolpath-cli

**Deprecated.** This crate is now a thin shim that re-exports [`path-cli`](https://crates.io/crates/path-cli). It exists so that `cargo install toolpath-cli` keeps working — it installs the same `path` binary as before.

For new installs, use `path-cli` directly:

```bash
cargo install path-cli
```

The shim only ships the `path` binary. The dev-only `gen_synthetic_path` helper lives only in `path-cli`.

A future release will retire this crate entirely. Pin to `path-cli` to avoid the eventual removal.
