//! Provider construction from [`Config`].
//!
//! A `*_convo` factory returns the harness's conversation manager
//! with every environment value it consumes taken from [`Config`].
//! Command modules construct managers through these factories.
//!
//! A factory returns `Option` when its resolver takes the home
//! directory as a required argument: `None` means [`Config`] carries no
//! home, so the harness is out of reach. `require_*` turns that into an
//! error for a command that targets one harness.
//!
//! opencode takes the XDG data root as well as the home:
//! `$XDG_DATA_HOME` comes from [`Config`], and the resolver appends
//! `opencode` to it.
//!
//! copilot gets its directory injected, not just the home:
//! `$COPILOT_HOME` replaces the whole Copilot root, so the injected
//! directory wins against the home-derived default.
//!
//! cursor takes `$APPDATA` as an argument next to the home directory.
//! Only its Windows default user-data directory consults the value, so
//! the injection is gated to Windows.
//!
//! GitHub is a remote source: it takes an API token instead of a
//! resolver, and [`github_token`] builds it here.

use crate::config::Config;
#[cfg(not(target_os = "emscripten"))]
use crate::harness::HarnessBundle;
#[cfg(not(target_os = "emscripten"))]
use anyhow::{Context, bail};
use anyhow::{Result, anyhow};

fn missing_home(harness: &str) -> anyhow::Error {
    anyhow!(
        "cannot determine the home directory; set $HOME ($USERPROFILE on Windows) to reach {harness} sessions"
    )
}

pub(crate) fn claude_resolver(config: &Config) -> Option<toolpath_claude::PathResolver> {
    config.home_dir().map(toolpath_claude::PathResolver::new)
}

/// [`claude_resolver`] for a command that targets Claude.
pub(crate) fn require_claude_resolver(config: &Config) -> Result<toolpath_claude::PathResolver> {
    claude_resolver(config).ok_or_else(|| missing_home("Claude"))
}

/// The Claude reader's verbose-warning flag. `$CLAUDE_CLI_DEBUG` is
/// verbose when set, whatever its value.
pub(crate) fn claude_verbose_warnings(config: &Config) -> bool {
    config.claude_cli_debug.is_some()
}

pub(crate) fn gemini_resolver(config: &Config) -> Option<toolpath_gemini::PathResolver> {
    config.home_dir().map(toolpath_gemini::PathResolver::new)
}

/// [`gemini_resolver`] for a command that targets Gemini.
pub(crate) fn require_gemini_resolver(config: &Config) -> Result<toolpath_gemini::PathResolver> {
    gemini_resolver(config).ok_or_else(|| missing_home("Gemini"))
}

pub(crate) fn codex_resolver(config: &Config) -> Option<toolpath_codex::PathResolver> {
    config.home_dir().map(toolpath_codex::PathResolver::new)
}

/// [`codex_resolver`] for a command that targets Codex.
pub(crate) fn require_codex_resolver(config: &Config) -> Result<toolpath_codex::PathResolver> {
    codex_resolver(config).ok_or_else(|| missing_home("Codex"))
}

/// The Codex reader's strict flag. `$CODEX_ROLLOUT_STRICT` is strict
/// when set, whatever its value.
pub(crate) fn codex_strict(config: &Config) -> bool {
    config.codex_rollout_strict.is_some()
}

pub(crate) fn copilot_resolver(config: &Config) -> Option<toolpath_copilot::PathResolver> {
    config.home_dir().map(|home| {
        let resolver = toolpath_copilot::PathResolver::new(home);
        match &config.copilot_home {
            Some(dir) => resolver.with_copilot_dir(dir),
            None => resolver,
        }
    })
}

/// [`copilot_resolver`] for a command that targets Copilot.
pub(crate) fn require_copilot_resolver(config: &Config) -> Result<toolpath_copilot::PathResolver> {
    copilot_resolver(config).ok_or_else(|| missing_home("Copilot"))
}

/// The Copilot reader's strict flag. `$COPILOT_EVENTS_STRICT` is strict
/// when set, whatever its value.
pub(crate) fn copilot_strict(config: &Config) -> bool {
    config.copilot_events_strict.is_some()
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn opencode_resolver(config: &Config) -> Option<toolpath_opencode::PathResolver> {
    config.home_dir().map(|home| {
        let resolver = toolpath_opencode::PathResolver::new(home);
        match &config.xdg_data_home {
            Some(xdg) => resolver.with_xdg_data_home(xdg),
            None => resolver,
        }
    })
}

/// [`opencode_resolver`] for a command that targets opencode.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn require_opencode_resolver(
    config: &Config,
) -> Result<toolpath_opencode::PathResolver> {
    opencode_resolver(config).ok_or_else(|| missing_home("opencode"))
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn cursor_resolver(config: &Config) -> Option<toolpath_cursor::PathResolver> {
    let resolver = toolpath_cursor::PathResolver::new(config.home_dir()?);
    // The resolver applies $APPDATA only on Windows; injecting it on
    // other platforms would change resolution there.
    #[cfg(windows)]
    let resolver = match &config.appdata {
        Some(appdata) => resolver.with_appdata(appdata),
        None => resolver,
    };
    Some(resolver)
}

/// [`cursor_resolver`] for a command that targets Cursor.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn require_cursor_resolver(config: &Config) -> Result<toolpath_cursor::PathResolver> {
    cursor_resolver(config).ok_or_else(|| missing_home("Cursor"))
}

pub(crate) fn pi_resolver(config: &Config) -> Option<toolpath_pi::PathResolver> {
    config.home_dir().map(toolpath_pi::PathResolver::new)
}

/// [`pi_resolver`] for a command that targets Pi.
pub(crate) fn require_pi_resolver(config: &Config) -> Result<toolpath_pi::PathResolver> {
    pi_resolver(config).ok_or_else(|| missing_home("Pi"))
}

/// The GitHub API token: `$GITHUB_TOKEN` when it is set and not empty,
/// otherwise the token the GitHub CLI holds.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn github_token(config: &Config) -> Result<String> {
    github_token_or_else(config, gh_auth_token)
}

#[cfg(not(target_os = "emscripten"))]
fn github_token_or_else(
    config: &Config,
    fallback: impl FnOnce() -> Result<String>,
) -> Result<String> {
    match config.github_token.as_deref() {
        Some(token) if !token.is_empty() => Ok(token.to_string()),
        _ => fallback(),
    }
}

/// Read the token out of the GitHub CLI.
#[cfg(not(target_os = "emscripten"))]
fn gh_auth_token() -> Result<String> {
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context(
            "Failed to run 'gh auth token'. Set GITHUB_TOKEN or install the GitHub CLI (gh).",
        )?;

    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    bail!(
        "No GitHub token found. Set GITHUB_TOKEN environment variable \
         or authenticate with 'gh auth login'."
    )
}

/// The production [`HarnessBundle`], every provider built from
/// `config`. A provider whose resolver needs a home directory is
/// present only when `config` carries one; consumers skip the ones
/// whose listing returns empty/NotFound.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn harness_bundle(config: &Config) -> HarnessBundle {
    HarnessBundle {
        claude: claude_resolver(config).map(|r| {
            toolpath_claude::ClaudeConvo::with_resolver(r)
                .with_verbose_warnings(claude_verbose_warnings(config))
        }),
        gemini: gemini_resolver(config).map(toolpath_gemini::GeminiConvo::with_resolver),
        codex: codex_resolver(config).map(|r| {
            toolpath_codex::CodexConvo::with_resolver(r).with_strict(codex_strict(config))
        }),
        copilot: copilot_resolver(config).map(|r| {
            toolpath_copilot::CopilotConvo::with_resolver(r).with_strict(copilot_strict(config))
        }),
        opencode: opencode_resolver(config).map(toolpath_opencode::OpencodeConvo::with_resolver),
        cursor: cursor_resolver(config).map(toolpath_cursor::CursorConvo::with_resolver),
        pi: pi_resolver(config).map(toolpath_pi::PiConvo::with_resolver),
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Assertions stay on paths fully determined by injected values;
    // resolver defaults that read the ambient environment (home
    // fallbacks) are not asserted here.

    fn config_with_home() -> Config {
        Config {
            home: Some(PathBuf::from("/home/jailed")),
            ..Config::default()
        }
    }

    #[test]
    fn claude_resolver_roots_at_config_home() {
        let resolver = claude_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.projects_dir(),
            PathBuf::from("/home/jailed/.claude/projects")
        );
    }

    #[test]
    fn claude_resolver_is_none_without_a_home() {
        assert!(claude_resolver(&Config::default()).is_none());
        let err = require_claude_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn claude_verbose_warnings_follows_presence_of_the_variable() {
        assert!(!claude_verbose_warnings(&Config::default()));
        let config = Config {
            claude_cli_debug: Some(String::new()),
            ..Config::default()
        };
        assert!(claude_verbose_warnings(&config));
    }

    #[test]
    fn gemini_resolver_roots_at_config_home() {
        let resolver = gemini_resolver(&config_with_home()).unwrap();
        assert_eq!(resolver.gemini_dir(), PathBuf::from("/home/jailed/.gemini"));
    }

    #[test]
    fn gemini_resolver_is_none_without_a_home() {
        assert!(gemini_resolver(&Config::default()).is_none());
        let err = require_gemini_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn codex_resolver_roots_at_config_home() {
        let resolver = codex_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.sessions_root(),
            PathBuf::from("/home/jailed/.codex/sessions")
        );
    }

    #[test]
    fn codex_resolver_is_none_without_a_home() {
        assert!(codex_resolver(&Config::default()).is_none());
        let err = require_codex_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn codex_strict_follows_presence_of_the_variable() {
        assert!(!codex_strict(&Config::default()));
        let config = Config {
            codex_rollout_strict: Some(String::new()),
            ..Config::default()
        };
        assert!(codex_strict(&config));
    }

    #[test]
    fn copilot_resolver_injects_copilot_dir() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            copilot_home: Some(PathBuf::from("/copilot/root")),
            ..Config::default()
        };
        let resolver = copilot_resolver(&config).unwrap();
        assert_eq!(resolver.copilot_dir(), PathBuf::from("/copilot/root"));
    }

    #[test]
    fn copilot_resolver_roots_at_config_home() {
        let resolver = copilot_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.session_state_dir(),
            PathBuf::from("/home/jailed/.copilot/session-state")
        );
    }

    #[test]
    fn copilot_resolver_is_none_without_a_home() {
        assert!(copilot_resolver(&Config::default()).is_none());
        let err = require_copilot_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn copilot_strict_follows_presence_of_the_variable() {
        assert!(!copilot_strict(&Config::default()));
        let config = Config {
            copilot_events_strict: Some(String::new()),
            ..Config::default()
        };
        assert!(copilot_strict(&config));
    }

    #[test]
    fn opencode_resolver_injects_the_xdg_data_root() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            xdg_data_home: Some(PathBuf::from("/xdg/data")),
            ..Config::default()
        };
        let resolver = opencode_resolver(&config).unwrap();
        assert_eq!(
            resolver.db_path(),
            PathBuf::from("/xdg/data/opencode/opencode.db")
        );
    }

    #[test]
    fn opencode_resolver_roots_at_config_home() {
        let resolver = opencode_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.db_path(),
            PathBuf::from("/home/jailed/.local/share/opencode/opencode.db")
        );
    }

    #[test]
    fn opencode_resolver_is_none_without_a_home() {
        assert!(opencode_resolver(&Config::default()).is_none());
        let err = require_opencode_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn cursor_resolver_roots_at_config_home() {
        let resolver = cursor_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.anysphere_dir(),
            PathBuf::from("/home/jailed/.cursor")
        );
    }

    #[test]
    fn cursor_resolver_is_none_without_a_home() {
        assert!(cursor_resolver(&Config::default()).is_none());
        let err = require_cursor_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn claude_resolver_falls_back_to_config_userprofile() {
        let config = Config {
            userprofile: Some(PathBuf::from("/users/jailed")),
            ..Config::default()
        };
        let resolver = claude_resolver(&config).unwrap();
        assert_eq!(
            resolver.projects_dir(),
            PathBuf::from("/users/jailed/.claude/projects")
        );
    }

    #[cfg(windows)]
    #[test]
    fn cursor_resolver_injects_appdata() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            appdata: Some(PathBuf::from("/appdata/roaming")),
            ..Config::default()
        };
        let resolver = cursor_resolver(&config).unwrap();
        assert_eq!(
            resolver.db_path(),
            PathBuf::from("/appdata/roaming/Cursor/User/globalStorage/state.vscdb")
        );
    }

    #[test]
    fn harness_bundle_roots_providers_at_config_home() {
        let bundle = harness_bundle(&config_with_home());
        assert_eq!(
            bundle.claude.unwrap().resolver().projects_dir(),
            PathBuf::from("/home/jailed/.claude/projects")
        );
        assert_eq!(
            bundle.pi.unwrap().resolver().sessions_dir(),
            PathBuf::from("/home/jailed/.pi/agent/sessions")
        );
    }

    #[test]
    fn github_token_prefers_the_configured_value() {
        let config = Config {
            github_token: Some("configured".to_string()),
            ..Config::default()
        };
        let token = github_token_or_else(&config, || Ok("fallback".to_string())).unwrap();
        assert_eq!(token, "configured");
    }

    #[test]
    fn github_token_falls_back_when_unset_or_empty() {
        let fallback = || Ok("fallback".to_string());
        assert_eq!(
            github_token_or_else(&Config::default(), fallback).unwrap(),
            "fallback"
        );
        let config = Config {
            github_token: Some(String::new()),
            ..Config::default()
        };
        assert_eq!(github_token_or_else(&config, fallback).unwrap(), "fallback");
    }

    #[test]
    fn pi_resolver_roots_at_config_home() {
        let resolver = pi_resolver(&config_with_home()).unwrap();
        assert_eq!(
            resolver.sessions_dir(),
            PathBuf::from("/home/jailed/.pi/agent/sessions")
        );
    }

    #[test]
    fn pi_resolver_is_none_without_a_home() {
        assert!(pi_resolver(&Config::default()).is_none());
        let err = require_pi_resolver(&Config::default()).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }
}
