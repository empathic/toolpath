//! Deprecation shim for `path derive`.
//!
//! `path derive` was renamed to `path import`. This shim preserves the
//! old surface (including stdout JSON output) for one release, printing a
//! deprecation warning on stderr and delegating into `cmd_import`.

use anyhow::Result;

pub use crate::cmd_import::ImportSource as DeriveSource;

pub fn run(source: DeriveSource, pretty: bool) -> Result<()> {
    eprintln!("warning: `path derive` is deprecated; use `path import` instead");
    // Preserve the old stdout-JSON behavior via --no-cache.
    let args = crate::cmd_import::ImportArgs {
        source,
        force: false,
        no_cache: true,
    };
    crate::cmd_import::run(args, pretty)
}
