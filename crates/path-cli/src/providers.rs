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
//! opencode, copilot, and cursor (Windows) get their directory
//! injected, not just the home: their resolvers read `$XDG_DATA_HOME`
//! / `$COPILOT_HOME` / `$APPDATA` internally, and those reads win
//! against `with_home`. The injected directory wins against both.

use crate::config::Config;
#[cfg(not(target_os = "emscripten"))]
use crate::harness::HarnessBundle;
use std::path::Path;

use anyhow::{Result, anyhow};

fn missing_home(harness: &str) -> anyhow::Error {
    anyhow!(
        "cannot determine the home directory; set $HOME ($USERPROFILE on Windows) to reach {harness} sessions"
    )
}

pub(crate) fn claude_convo(config: &Config) -> toolpath_claude::ClaudeConvo {
    let mut resolver = toolpath_claude::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    toolpath_claude::ClaudeConvo::with_resolver(resolver)
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

pub(crate) fn copilot_convo(config: &Config) -> toolpath_copilot::CopilotConvo {
    let mut resolver = toolpath_copilot::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    if let Some(dir) = &config.copilot_home {
        resolver = resolver.with_copilot_dir(dir);
    }
    toolpath_copilot::CopilotConvo::with_resolver(resolver)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn opencode_convo(config: &Config) -> toolpath_opencode::OpencodeConvo {
    let mut resolver = toolpath_opencode::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    if let Some(xdg) = &config.xdg_data_home {
        resolver = resolver.with_data_dir(xdg.join("opencode"));
    }
    toolpath_opencode::OpencodeConvo::with_resolver(resolver)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn cursor_convo(config: &Config) -> toolpath_cursor::CursorConvo {
    let mut resolver = toolpath_cursor::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    // The resolver consults $APPDATA only on Windows; injecting it on
    // other platforms would change resolution there.
    #[cfg(windows)]
    if let Some(appdata) = &config.appdata {
        resolver = resolver.with_user_data_dir(appdata.join("Cursor"));
    }
    toolpath_cursor::CursorConvo::with_resolver(resolver)
}

/// `base` replaces the sessions directory: `--base` wins over the
/// config home.
pub(crate) fn pi_convo(config: &Config, base: Option<&Path>) -> toolpath_pi::PiConvo {
    let mut resolver = toolpath_pi::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    if let Some(dir) = base {
        resolver = resolver.with_sessions_dir(dir);
    }
    toolpath_pi::PiConvo::with_resolver(resolver)
}

/// The production [`HarnessBundle`], every provider built from
/// `config`. A provider whose resolver needs a home directory is
/// present only when `config` carries one; consumers skip the ones
/// whose listing returns empty/NotFound.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn harness_bundle(config: &Config) -> HarnessBundle {
    HarnessBundle {
        claude: Some(claude_convo(config)),
        gemini: gemini_resolver(config).map(toolpath_gemini::GeminiConvo::with_resolver),
        codex: codex_resolver(config).map(|r| {
            toolpath_codex::CodexConvo::with_resolver(r).with_strict(codex_strict(config))
        }),
        copilot: Some(copilot_convo(config)),
        opencode: Some(opencode_convo(config)),
        cursor: Some(cursor_convo(config)),
        pi: Some(pi_convo(config, None)),
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Assertions stay on paths fully determined by injected values;
    // resolver defaults that read the ambient environment (home
    // fallbacks, `$XDG_DATA_HOME` when no directory is injected) are
    // not asserted here.

    fn config_with_home() -> Config {
        Config {
            home: Some(PathBuf::from("/home/jailed")),
            ..Config::default()
        }
    }

    #[test]
    fn claude_convo_roots_at_config_home() {
        let manager = claude_convo(&config_with_home());
        assert_eq!(
            manager.resolver().projects_dir().unwrap(),
            PathBuf::from("/home/jailed/.claude/projects")
        );
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
    fn copilot_convo_injects_copilot_dir() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            copilot_home: Some(PathBuf::from("/copilot/root")),
            ..Config::default()
        };
        let manager = copilot_convo(&config);
        assert_eq!(
            manager.resolver().copilot_dir().unwrap(),
            PathBuf::from("/copilot/root")
        );
    }

    #[test]
    fn opencode_convo_injects_data_dir() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            xdg_data_home: Some(PathBuf::from("/xdg/data")),
            ..Config::default()
        };
        let manager = opencode_convo(&config);
        assert_eq!(
            manager.resolver().db_path().unwrap(),
            PathBuf::from("/xdg/data/opencode/opencode.db")
        );
    }

    #[test]
    fn cursor_convo_roots_at_config_home() {
        let manager = cursor_convo(&config_with_home());
        assert_eq!(
            manager.resolver().anysphere_dir().unwrap(),
            PathBuf::from("/home/jailed/.cursor")
        );
    }

    #[test]
    fn convos_fall_back_to_config_userprofile() {
        let config = Config {
            userprofile: Some(PathBuf::from("/users/jailed")),
            ..Config::default()
        };
        let manager = claude_convo(&config);
        assert_eq!(
            manager.resolver().projects_dir().unwrap(),
            PathBuf::from("/users/jailed/.claude/projects")
        );
    }

    #[cfg(windows)]
    #[test]
    fn cursor_convo_injects_user_data_dir_from_appdata() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            appdata: Some(PathBuf::from("/appdata/roaming")),
            ..Config::default()
        };
        let manager = cursor_convo(&config);
        assert_eq!(
            manager.resolver().db_path().unwrap(),
            PathBuf::from("/appdata/roaming/Cursor/User/globalStorage/state.vscdb")
        );
    }

    #[test]
    fn pi_convo_roots_at_config_home() {
        let manager = pi_convo(&config_with_home(), None);
        assert_eq!(
            manager.resolver().sessions_dir(),
            PathBuf::from("/home/jailed/.pi/agent/sessions")
        );
    }

    #[test]
    fn pi_convo_base_replaces_the_sessions_dir() {
        let manager = pi_convo(&config_with_home(), Some(Path::new("/pi/base")));
        assert_eq!(manager.resolver().sessions_dir(), PathBuf::from("/pi/base"));
    }

    #[test]
    fn harness_bundle_roots_providers_at_config_home() {
        let bundle = harness_bundle(&config_with_home());
        assert_eq!(
            bundle.claude.unwrap().resolver().projects_dir().unwrap(),
            PathBuf::from("/home/jailed/.claude/projects")
        );
        assert_eq!(
            bundle.pi.unwrap().resolver().sessions_dir(),
            PathBuf::from("/home/jailed/.pi/agent/sessions")
        );
    }
}
