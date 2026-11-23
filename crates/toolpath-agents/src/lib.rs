//! The agent CLIs toolpath's tests run, resolved ONLY through `agents.lock`.
//!
//! - [`Lock`]: per agent and platform, a version, the https url to fetch, the sha256 of what that
//!   url serves (taken from the vendor's published checksum), the archive format and the member
//!   that is the executable.
//! - [`cache`]: `fetch` downloads the url, refuses unless the bytes hash to the lock's sha256, and
//!   keeps them under that hash (`$XDG_CACHE_HOME/toolpath/agents/<sha256>/blob`); `install`
//!   unpacks a verified blob next to it. A cache entry is re-hashed on every lookup, never trusted.
//! - [`hermetic`]: runs an installed agent with an empty environment, a fresh temporary HOME and
//!   TMPDIR, and `/` as its working directory, so a test sees only what it passes in.
//!
//! Nothing here reads `PATH`, Homebrew or the developer's own install of an agent.

pub mod cache;
pub mod hermetic;
pub mod lock;

pub use cache::{Cache, sha256_hex};
pub use hermetic::Hermetic;
pub use lock::{Agent, Archive, Lock, Platform};

/// The Rust target triple of this machine, as the lock spells platforms.
pub fn host_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
