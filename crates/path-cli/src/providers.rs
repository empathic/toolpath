//! Provider construction from [`Config`].
//!
//! A `*_convo` factory returns the harness's conversation manager
//! with every environment value it consumes taken from [`Config`].
//! Command modules construct managers through these factories.
//!
//! A `*_resolver` factory builds the manager's `PathResolver`; the
//! `*_convo` factories consume them. A call site takes a resolver
//! directly only to chain a per-command override (e.g.
//! `with_sessions_dir`) before it constructs the manager.
//!
//! opencode, copilot, and cursor (Windows) get their directory
//! injected, not just the home: their resolvers read `$XDG_DATA_HOME`
//! / `$COPILOT_HOME` / `$APPDATA` internally, and those reads win
//! against `with_home`. The injected directory wins against both.
#![cfg(not(target_os = "emscripten"))]

use crate::config::Config;

pub(crate) fn claude_convo(config: &Config) -> toolpath_claude::ClaudeConvo {
    toolpath_claude::ClaudeConvo::with_resolver(claude_resolver(config))
}

pub(crate) fn claude_resolver(config: &Config) -> toolpath_claude::PathResolver {
    let mut resolver = toolpath_claude::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    resolver
}

pub(crate) fn gemini_convo(config: &Config) -> toolpath_gemini::GeminiConvo {
    toolpath_gemini::GeminiConvo::with_resolver(gemini_resolver(config))
}

pub(crate) fn gemini_resolver(config: &Config) -> toolpath_gemini::PathResolver {
    let mut resolver = toolpath_gemini::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    resolver
}

pub(crate) fn codex_convo(config: &Config) -> toolpath_codex::CodexConvo {
    toolpath_codex::CodexConvo::with_resolver(codex_resolver(config))
}

pub(crate) fn codex_resolver(config: &Config) -> toolpath_codex::PathResolver {
    let mut resolver = toolpath_codex::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    resolver
}

pub(crate) fn copilot_convo(config: &Config) -> toolpath_copilot::CopilotConvo {
    toolpath_copilot::CopilotConvo::with_resolver(copilot_resolver(config))
}

pub(crate) fn copilot_resolver(config: &Config) -> toolpath_copilot::PathResolver {
    let mut resolver = toolpath_copilot::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    if let Some(dir) = &config.copilot_home {
        resolver = resolver.with_copilot_dir(dir);
    }
    resolver
}

pub(crate) fn opencode_convo(config: &Config) -> toolpath_opencode::OpencodeConvo {
    toolpath_opencode::OpencodeConvo::with_resolver(opencode_resolver(config))
}

pub(crate) fn opencode_resolver(config: &Config) -> toolpath_opencode::PathResolver {
    let mut resolver = toolpath_opencode::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    if let Some(xdg) = &config.xdg_data_home {
        resolver = resolver.with_data_dir(xdg.join("opencode"));
    }
    resolver
}

pub(crate) fn cursor_resolver(config: &Config) -> toolpath_cursor::PathResolver {
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
    resolver
}

pub(crate) fn pi_resolver(config: &Config) -> toolpath_pi::PathResolver {
    let mut resolver = toolpath_pi::PathResolver::new();
    if let Some(home) = config.home_dir() {
        resolver = resolver.with_home(home);
    }
    resolver
}

#[cfg(test)]
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
    fn claude_resolver_roots_at_config_home() {
        let resolver = claude_resolver(&config_with_home());
        assert_eq!(
            resolver.projects_dir().unwrap(),
            PathBuf::from("/home/jailed/.claude/projects")
        );
    }

    #[test]
    fn gemini_convo_roots_at_config_home() {
        let manager = gemini_convo(&config_with_home());
        assert_eq!(
            manager.resolver().gemini_dir().unwrap(),
            PathBuf::from("/home/jailed/.gemini")
        );
    }

    #[test]
    fn gemini_resolver_roots_at_config_home() {
        let resolver = gemini_resolver(&config_with_home());
        assert_eq!(
            resolver.gemini_dir().unwrap(),
            PathBuf::from("/home/jailed/.gemini")
        );
    }

    #[test]
    fn codex_convo_roots_at_config_home() {
        let manager = codex_convo(&config_with_home());
        assert_eq!(
            manager.resolver().sessions_root().unwrap(),
            PathBuf::from("/home/jailed/.codex/sessions")
        );
    }

    #[test]
    fn codex_resolver_roots_at_config_home() {
        let resolver = codex_resolver(&config_with_home());
        assert_eq!(
            resolver.sessions_root().unwrap(),
            PathBuf::from("/home/jailed/.codex/sessions")
        );
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
    fn copilot_resolver_injects_copilot_dir() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            copilot_home: Some(PathBuf::from("/copilot/root")),
            ..Config::default()
        };
        let resolver = copilot_resolver(&config);
        assert_eq!(
            resolver.copilot_dir().unwrap(),
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
    fn opencode_resolver_injects_data_dir() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            xdg_data_home: Some(PathBuf::from("/xdg/data")),
            ..Config::default()
        };
        let resolver = opencode_resolver(&config);
        assert_eq!(
            resolver.db_path().unwrap(),
            PathBuf::from("/xdg/data/opencode/opencode.db")
        );
    }

    #[test]
    fn cursor_resolver_roots_at_config_home() {
        let resolver = cursor_resolver(&config_with_home());
        assert_eq!(
            resolver.anysphere_dir().unwrap(),
            PathBuf::from("/home/jailed/.cursor")
        );
    }

    #[test]
    fn resolvers_fall_back_to_config_userprofile() {
        let config = Config {
            userprofile: Some(PathBuf::from("/users/jailed")),
            ..Config::default()
        };
        let resolver = claude_resolver(&config);
        assert_eq!(
            resolver.projects_dir().unwrap(),
            PathBuf::from("/users/jailed/.claude/projects")
        );
    }

    #[cfg(windows)]
    #[test]
    fn cursor_resolver_injects_user_data_dir_from_appdata() {
        let config = Config {
            home: Some(PathBuf::from("/home/jailed")),
            appdata: Some(PathBuf::from("/appdata/roaming")),
            ..Config::default()
        };
        let resolver = cursor_resolver(&config);
        assert_eq!(
            resolver.db_path().unwrap(),
            PathBuf::from("/appdata/roaming/Cursor/User/globalStorage/state.vscdb")
        );
    }

    #[test]
    fn pi_resolver_roots_at_config_home() {
        let resolver = pi_resolver(&config_with_home());
        assert_eq!(
            resolver.sessions_dir(),
            PathBuf::from("/home/jailed/.pi/agent/sessions")
        );
    }
}
