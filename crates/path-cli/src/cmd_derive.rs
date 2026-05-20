//! `path p derive` — the stdout-JSON sibling of `import`.
//!
//! Same sources as `import`, but always streams the resulting document
//! to stdout (i.e. `--no-cache` is implied). Useful for shell
//! composition where caching to `~/.toolpath/documents/` would be pure
//! overhead.

use anyhow::Result;

pub use crate::cmd_import::ImportSource as DeriveSource;

pub fn run(source: DeriveSource, pretty: bool) -> Result<()> {
    let args = crate::cmd_import::ImportArgs {
        source,
        force: false,
        no_cache: true,
    };
    crate::cmd_import::run(args, pretty)
}
