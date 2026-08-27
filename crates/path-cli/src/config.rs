//! Environment-derived configuration. [`Config`] holds every value the
//! CLI reads from the process environment. The module also resolves
//! the config directory and the home directory.
//!
//! Kept in its own module so it can be used by `cmd_cache` (needed on every
//! target, including wasm/emscripten) and `cmd_pathbase` (native-only).
//! `cmd_pathbase` is cfg-gated; without this split, anything `cmd_cache`
//! imports from it would break wasm builds.

use anyhow::{Context, Result, anyhow};
use figment::Figment;
use figment::providers::{Env, Serialized};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const CONFIG_DIR_NAME: &str = ".toolpath";
pub(crate) const CONFIG_DIR_ENV: &str = "TOOLPATH_CONFIG_DIR";
/// Pathbase server override (see `cmd_pathbase`).
///
/// Declared here because [`Config::ENV_MAP`] needs it on every build
/// target. `cmd_pathbase` depends on reqwest, so lib.rs compiles it
/// out of the wasm/emscripten build; a constant declared there does
/// not exist on that target.
pub(crate) const PATHBASE_URL_ENV: &str = "PATHBASE_URL";

// Every file and directory name under the config dir is declared here,
// next to the directory resolution — never inline at a use site.

/// The user's config file (see `share_config`).
pub(crate) const CONFIG_FILE_NAME: &str = "config.toml";
/// The artifact manifest under the config dir (see `sync::engine`).
pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
/// Sibling advisory lock serializing manifest writers. A separate
/// file because the manifest itself is replaced by rename on every
/// write, which would drop any lock held on it.
pub(crate) const MANIFEST_LOCK_FILE_NAME: &str = "manifest.json.lock";
/// Pathbase bearer token + user, written by `path auth login`
/// (see `cmd_pathbase`).
pub(crate) const CREDENTIALS_FILE_NAME: &str = "credentials.json";
/// The document cache directory (see `cache`).
pub(crate) const DOCUMENTS_DIR_NAME: &str = "documents";

/// Environment-derived configuration. [`Config::load`] reads the
/// environment once, at the composition root. Code below the root
/// receives values as parameters and does not read the environment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Config {
    /// `$APPDATA`: Windows harness data root.
    pub(crate) appdata: Option<PathBuf>,
    /// `$COPILOT_HOME`: Copilot CLI session root override.
    pub(crate) copilot_home: Option<PathBuf>,
    /// `$HOME`: config-root fallback and the harness resolvers' root.
    pub(crate) home: Option<PathBuf>,
    /// `$PATHBASE_URL`: Pathbase server override (see `cmd_pathbase`).
    pub(crate) pathbase_url: Option<String>,
    /// `$TOOLPATH_CONFIG_DIR`: overrides the `~/.toolpath` root.
    pub(crate) toolpath_config_dir: Option<PathBuf>,
    /// `$TOOLPATH_QUERY_EXPLAIN`: query-planner diagnostics on stderr.
    pub(crate) toolpath_query_explain: Option<String>,
    /// `$USERPROFILE`: Windows home, the fallback when `$HOME` is unset.
    pub(crate) userprofile: Option<PathBuf>,
    /// `$XDG_DATA_HOME`: opencode's data root (Linux).
    pub(crate) xdg_data_home: Option<PathBuf>,
}

/// [`Env`], with values emitted as verbatim strings.
///
/// `Env`'s own `Provider::data` type-infers every value and has no
/// option to disable this: `TOOLPATH_QUERY_EXPLAIN=1` arrives as an
/// integer, not a string. Environment variables are strings; the
/// [`Config`] field types are the single type authority.
struct VerbatimEnv(Env);

impl figment::Provider for VerbatimEnv {
    fn metadata(&self) -> figment::Metadata {
        self.0.metadata()
    }

    fn data(
        &self,
    ) -> Result<figment::value::Map<figment::Profile, figment::value::Dict>, figment::Error> {
        let mut dict = figment::value::Dict::new();
        for (key, value) in self.0.iter() {
            dict.insert(key.as_str().to_string(), value.into());
        }
        Ok(self.0.profile.collect(dict))
    }
}

impl Config {
    /// Each environment variable [`Config`] reads, paired with the
    /// field it fills. [`Config::load`] admits the left column and
    /// renames each key to the right column; no other variable can
    /// influence a `Config`. Names match case-insensitively.
    const ENV_MAP: &'static [(&'static str, &'static str)] = &[
        ("APPDATA", "appdata"),
        ("COPILOT_HOME", "copilot_home"),
        ("HOME", "home"),
        (PATHBASE_URL_ENV, "pathbase_url"),
        (CONFIG_DIR_ENV, "toolpath_config_dir"),
        ("TOOLPATH_QUERY_EXPLAIN", "toolpath_query_explain"),
        ("USERPROFILE", "userprofile"),
        ("XDG_DATA_HOME", "xdg_data_home"),
    ];

    /// Every environment variable [`Config`] reads.
    pub(crate) fn env_var_names() -> impl Iterator<Item = &'static str> {
        Self::ENV_MAP.iter().map(|(var, _)| *var)
    }

    /// Read the process environment and extract an immutable `Config`.
    pub(crate) fn load() -> Result<Self> {
        let vars: Vec<&str> = Self::env_var_names().collect();
        let env = Env::raw().only(&vars).map(|key| {
            Self::ENV_MAP
                .iter()
                .find(|(var, _)| key.as_str().eq_ignore_ascii_case(var))
                .map(|(_, field)| (*field).into())
                .expect("only() admits exactly the mapped variables")
        });
        Figment::from(Serialized::defaults(Config::default()))
            .merge(VerbatimEnv(env))
            .extract()
            .context("failed to load configuration from the environment")
    }

    /// The configured toolpath config directory (default `~/.toolpath`,
    /// overridable via `$TOOLPATH_CONFIG_DIR`).
    pub(crate) fn config_dir(&self) -> Result<PathBuf> {
        if let Some(override_) = &self.toolpath_config_dir {
            return Ok(override_.clone());
        }
        let home = self
            .home
            .as_ref()
            .ok_or_else(|| anyhow!("$HOME is not set; cannot locate config directory"))?;
        Ok(home.join(CONFIG_DIR_NAME))
    }

    /// The home directory the provider resolvers should use:
    /// `$HOME`, falling back to `$USERPROFILE` (Windows). Matches the
    /// resolvers' own internal fallback.
    pub(crate) fn home_dir(&self) -> Option<&PathBuf> {
        self.home.as_ref().or(self.userprofile.as_ref())
    }

    /// The subset of this configuration that builds a provider manager.
    pub(crate) fn projection(&self) -> ProjectionConfig {
        ProjectionConfig {
            home: self.home_dir().cloned(),
            xdg_data_home: self.xdg_data_home.clone(),
            copilot_home: self.copilot_home.clone(),
            appdata: self.appdata.clone(),
        }
    }
}

/// The subset of [`Config`] that builds a provider manager: the
/// harness store roots the path resolvers read. [`Config::projection`]
/// builds it; `providers` consumes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProjectionConfig {
    /// The resolvers' home: `$HOME`, falling back to `$USERPROFILE`.
    pub(crate) home: Option<PathBuf>,
    /// `$XDG_DATA_HOME`: opencode's data root (Linux).
    pub(crate) xdg_data_home: Option<PathBuf>,
    /// `$COPILOT_HOME`: Copilot CLI session root override.
    pub(crate) copilot_home: Option<PathBuf>,
    /// `$APPDATA`: Windows harness data root.
    pub(crate) appdata: Option<PathBuf>,
}

/// The configured toolpath config directory (default `~/.toolpath`,
/// overridable via `$TOOLPATH_CONFIG_DIR`).
///
/// Transitional: loads a [`Config`] per call. New code takes `&Config`
/// as a parameter and calls [`Config::config_dir`].
pub(crate) fn config_dir() -> Result<PathBuf> {
    Config::load()?.config_dir()
}

/// Cross-platform `$HOME` lookup matching the providers' internal helpers.
/// Returns `None` only when neither `$HOME` nor `$USERPROFILE` is set.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Display `path` as `~/relative/part` when it's under `home`, otherwise
/// return its absolute lossy form. Pure helper — does no filesystem I/O.
pub(crate) fn home_relative(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        // strip_prefix returns the empty path when path == home; treat that
        // as plain "~".
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Shared lock for tests that manipulate `$TOOLPATH_CONFIG_DIR`. Every
/// test module that calls `set_var` / `remove_var` on this env var should
/// grab this lock first, otherwise parallel tests race and clobber each
/// other's directories.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// `figment::Jail` restores the variables it sets, but it serializes
    /// only against other Jail tests. Hold `TEST_ENV_LOCK` too: other
    /// test modules mutate `$HOME` / `$TOOLPATH_CONFIG_DIR` under that
    /// lock.
    // result_large_err: the Jail closure returns figment's own
    // 208-byte error type.
    #[test]
    #[allow(clippy::result_large_err)]
    fn load_maps_every_owned_env_var() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        figment::Jail::expect_with(|jail| {
            jail.set_env(CONFIG_DIR_ENV, "/tmp/cfg-root");
            jail.set_env("HOME", "/home/jailed");
            jail.set_env("XDG_DATA_HOME", "/home/jailed/.local/share");
            jail.set_env("COPILOT_HOME", "/home/jailed/.copilot");
            jail.set_env("APPDATA", "/home/jailed/appdata");
            jail.set_env(PATHBASE_URL_ENV, "https://pathbase.test");
            jail.set_env("TOOLPATH_QUERY_EXPLAIN", "1");
            jail.set_env("USERPROFILE", "/home/jailed-profile");
            let config = Config::load().unwrap();
            assert_eq!(
                config,
                Config {
                    appdata: Some(PathBuf::from("/home/jailed/appdata")),
                    copilot_home: Some(PathBuf::from("/home/jailed/.copilot")),
                    home: Some(PathBuf::from("/home/jailed")),
                    pathbase_url: Some("https://pathbase.test".to_string()),
                    toolpath_config_dir: Some(PathBuf::from("/tmp/cfg-root")),
                    toolpath_query_explain: Some("1".to_string()),
                    userprofile: Some(PathBuf::from("/home/jailed-profile")),
                    xdg_data_home: Some(PathBuf::from("/home/jailed/.local/share")),
                }
            );
            Ok(())
        });
    }

    /// Variables outside [`Config::ENV_MAP`] never reach the `Config`.
    /// The `only` filter and serde's unknown-field drop both enforce
    /// this; the test observes their combined effect and cannot tell
    /// them apart. Jail does not clear the ambient environment. Assert
    /// only on fields whose variables are set here or never exported
    /// ambiently (`TOOLPATH_QUERY_EXPLAIN`, unlike `PATHBASE_URL` or
    /// `HOME`).
    #[test]
    #[allow(clippy::result_large_err)]
    fn load_ignores_unowned_env_vars() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        figment::Jail::expect_with(|jail| {
            jail.set_env(PATHBASE_URL_ENV, "https://real.example");
            jail.set_env("PATHBASE_URL_BACKUP", "https://wrong.example");
            jail.set_env("TOOLPATH_QUERY_EXPLAINED", "yes");
            let config = Config::load().unwrap();
            assert_eq!(
                config.pathbase_url,
                Some("https://real.example".to_string())
            );
            assert_eq!(config.toolpath_query_explain, None);
            Ok(())
        });
    }

    /// The named env-var constants stay in [`Config::ENV_MAP`]. A
    /// rename in one place but not the other fails here.
    #[test]
    fn named_env_constants_are_mapped() {
        let names: Vec<&str> = Config::env_var_names().collect();
        assert!(names.contains(&CONFIG_DIR_ENV));
        assert!(names.contains(&PATHBASE_URL_ENV));
    }

    /// Values reach the `Config` verbatim. Type inference would turn
    /// `01` into the integer `1`.
    #[test]
    #[allow(clippy::result_large_err)]
    fn load_keeps_scalar_looking_values_verbatim() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        figment::Jail::expect_with(|jail| {
            jail.set_env("TOOLPATH_QUERY_EXPLAIN", "01");
            let config = Config::load().unwrap();
            assert_eq!(config.toolpath_query_explain, Some("01".to_string()));
            Ok(())
        });
    }

    #[test]
    fn config_dir_prefers_override() {
        let config = Config {
            toolpath_config_dir: Some(PathBuf::from("/tmp/test-toolpath")),
            home: Some(PathBuf::from("/home/alex")),
            ..Config::default()
        };
        assert_eq!(
            config.config_dir().unwrap(),
            PathBuf::from("/tmp/test-toolpath")
        );
    }

    #[test]
    fn config_dir_falls_back_to_home() {
        let config = Config {
            home: Some(PathBuf::from("/home/alex")),
            ..Config::default()
        };
        assert_eq!(
            config.config_dir().unwrap(),
            PathBuf::from("/home/alex/.toolpath")
        );
    }

    #[test]
    fn home_dir_prefers_home_over_userprofile() {
        let config = Config {
            home: Some(PathBuf::from("/home/alex")),
            userprofile: Some(PathBuf::from("C:/Users/alex")),
            ..Config::default()
        };
        assert_eq!(config.home_dir(), Some(&PathBuf::from("/home/alex")));
    }

    #[test]
    fn home_dir_falls_back_to_userprofile() {
        let config = Config {
            userprofile: Some(PathBuf::from("C:/Users/alex")),
            ..Config::default()
        };
        assert_eq!(config.home_dir(), Some(&PathBuf::from("C:/Users/alex")));
        assert_eq!(Config::default().home_dir(), None);
    }

    #[test]
    fn config_dir_errors_without_override_or_home() {
        let err = Config::default().config_dir().unwrap_err();
        assert!(err.to_string().contains("$HOME"));
    }

    /// The transitional free function honors the env override
    /// end-to-end.
    #[test]
    fn config_dir_honors_override() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, "/tmp/test-toolpath");
        }
        let dir = config_dir().unwrap();
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        assert_eq!(dir, PathBuf::from("/tmp/test-toolpath"));
    }

    #[test]
    fn home_relative_strips_home_prefix() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(
            home_relative(
                std::path::Path::new("/Users/alex/.claude/projects"),
                Some(home)
            ),
            "~/.claude/projects"
        );
    }

    #[test]
    fn home_relative_returns_tilde_for_home_itself() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(home_relative(home, Some(home)), "~");
    }

    #[test]
    fn home_relative_passes_through_paths_outside_home() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(
            home_relative(std::path::Path::new("/tmp/elsewhere"), Some(home)),
            "/tmp/elsewhere"
        );
    }

    #[test]
    fn home_relative_passes_through_when_no_home() {
        assert_eq!(
            home_relative(std::path::Path::new("/foo/bar"), None),
            "/foo/bar"
        );
    }
}
