//! Project-directory → Pathbase repo mapping for `path share`.
//!
//! `~/.toolpath/config.toml` `[[project]]` rules name the repo a
//! project's sessions upload to: each rule carries a `dir` and a `repo`,
//! matched by subtree against the session's own directory — the project
//! for path-keyed harnesses, the recorded cwd otherwise. The most
//! specific matching `dir` wins. The `--repo` flag beats config (that
//! precedence lives in `cmd_share::effective_repo`). Rule matching is
//! pure path logic, so a rule still applies when the checkout it names
//! has been deleted.
//!
//! Only the user's own config is consulted. A repo-tracked
//! `.toolpath.toml` variant (a team commits the mapping once, every
//! clone follows it) was built and deliberately pulled out: a committed
//! file silently redirecting other users' uploads needs a first-use
//! consent flow first — see issue #179.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::cmd_export::RepoSpec;
use crate::cmd_share::home_relative;

/// Personal config file under the toolpath config dir.
pub(crate) const GLOBAL_CONFIG_FILE: &str = "config.toml";

/// A repo mapping resolved from config. `origin` is the human-readable
/// provenance ("which file, which rule") shown in the "Sharing to" line
/// and in errors.
#[derive(Debug)]
pub(crate) struct ConfiguredRepo {
    pub(crate) repo: RepoSpec,
    pub(crate) origin: String,
}

/// Resolve the Pathbase repo configured for `session_dir`, if any.
pub(crate) fn resolve_repo(session_dir: &Path) -> Result<Option<ConfiguredRepo>> {
    let global = crate::config::config_dir()?.join(GLOBAL_CONFIG_FILE);
    resolve_repo_from(
        &global,
        crate::cmd_share::home_dir().as_deref(),
        session_dir,
    )
}

fn resolve_repo_from(
    global_config: &Path,
    home: Option<&Path>,
    session_dir: &Path,
) -> Result<Option<ConfiguredRepo>> {
    let dir = canonicalize_prefix(session_dir);
    global_rule(global_config, home, &dir)
}

/// Canonicalize the longest existing ancestor of `p` and re-append the
/// rest. Plain `canonicalize` fails as soon as any component is missing,
/// which would leave a session dir with a deleted tail un-normalized
/// while an existing rule dir normalizes (macOS: `/var` → `/private/var`)
/// — and subtree matching needs both sides in the same form.
fn canonicalize_prefix(p: &Path) -> PathBuf {
    for ancestor in p.ancestors() {
        if let Ok(canon) = std::fs::canonicalize(ancestor) {
            let rest = p.strip_prefix(ancestor).expect("ancestors are prefixes");
            return canon.join(rest);
        }
    }
    p.to_path_buf()
}

/// The personal config. Unknown keys are ignored so an older CLI
/// tolerates config written for a newer one.
#[derive(Debug, Deserialize)]
struct GlobalConfig {
    #[serde(default)]
    project: Vec<ProjectRule>,
}

/// One `[[project]]` rule: a directory subtree (`~/`-expandable) and the
/// settings applying to sessions recorded under it. `repo` is optional
/// so future per-project settings don't force one.
#[derive(Debug, Deserialize)]
struct ProjectRule {
    dir: String,
    #[serde(default)]
    repo: Option<String>,
}

fn global_rule(
    config_path: &Path,
    home: Option<&Path>,
    session_dir: &Path,
) -> Result<Option<ConfiguredRepo>> {
    let Some(text) = read_optional(config_path)? else {
        return Ok(None);
    };
    let config: GlobalConfig = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    // Most specific matching rule wins; among equally deep dirs, the
    // first rule in the file wins.
    let mut best: Option<(usize, &ProjectRule)> = None;
    for rule in &config.project {
        if rule.repo.is_none() {
            continue;
        }
        let rule_dir = canonicalize_prefix(&expand_tilde(&rule.dir, home));
        if !session_dir.starts_with(&rule_dir) {
            continue;
        }
        let depth = rule_dir.components().count();
        if best.is_none_or(|(d, _)| depth > d) {
            best = Some((depth, rule));
        }
    }
    let Some((_, rule)) = best else {
        return Ok(None);
    };
    let origin = format!(
        "{} (dir = {:?})",
        home_relative(config_path, home),
        rule.dir
    );
    let repo = parse_repo(
        rule.repo.as_deref().expect("repo-less rules were skipped"),
        &origin,
    )?;
    Ok(Some(ConfiguredRepo { repo, origin }))
}

/// Read a config file that's allowed to be absent. A missing file is
/// `None`; any other I/O failure is a real error.
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context(format!("failed to read {}", path.display())),
    }
}

fn parse_repo(value: &str, origin: &str) -> Result<RepoSpec> {
    crate::cmd_export::parse_repo_spec(value).map_err(|e| anyhow!("invalid repo in {origin}: {e}"))
}

/// Expand a leading `~` or `~/` against `home`; everything else passes
/// through untouched.
fn expand_tilde(dir: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if dir == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = dir.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn repo_str(found: &ConfiguredRepo) -> String {
        format!("{}/{}", found.repo.owner, found.repo.name)
    }

    #[test]
    fn no_config_resolves_to_none() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let got = resolve_repo_from(&temp.path().join("config.toml"), None, &project).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn global_rule_matches_subtree() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("work/proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nrepo = \"team/sessions\"\n",
                temp.path().join("work").display().to_string()
            ),
        );
        // Both the configured dir itself and a nested session dir match.
        let found = resolve_repo_from(&config, None, &project).unwrap().unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
        assert!(
            found.origin.contains("config.toml"),
            "origin: {}",
            found.origin
        );
        let found = resolve_repo_from(&config, None, &project.join("deep/nested"))
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
    }

    #[test]
    fn global_rule_ignores_non_matching_dir() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        write(
            &config,
            "[[project]]\ndir = \"/somewhere/else\"\nrepo = \"a/b\"\n",
        );
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        assert!(
            resolve_repo_from(&config, None, &project)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn global_rule_does_not_match_sibling_with_shared_prefix() {
        // Subtree matching is component-wise: a rule for `.../proj` must
        // not catch `.../proj-two`.
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nrepo = \"a/b\"\n",
                temp.path().join("proj").display().to_string()
            ),
        );
        let sibling = temp.path().join("proj-two");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(
            resolve_repo_from(&config, None, &sibling)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn most_specific_global_rule_wins() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let clients = temp.path().join("clients");
        let acme = clients.join("acme");
        std::fs::create_dir_all(&acme).unwrap();
        // Broad rule first, narrow rule second — order in the file must
        // not matter, only specificity.
        write(
            &config,
            &format!(
                "[[project]]\ndir = {broad:?}\nrepo = \"me/misc\"\n\n\
                 [[project]]\ndir = {narrow:?}\nrepo = \"acme/sessions\"\n",
                broad = clients.display().to_string(),
                narrow = acme.display().to_string(),
            ),
        );
        let found = resolve_repo_from(&config, None, &acme).unwrap().unwrap();
        assert_eq!(repo_str(&found), "acme/sessions");
        let found = resolve_repo_from(&config, None, &clients.join("other"))
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "me/misc");
    }

    #[test]
    fn global_rule_expands_tilde_against_home() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let project = home.join("work/proj");
        std::fs::create_dir_all(&project).unwrap();
        let config = home.join("cfg/config.toml");
        write(
            &config,
            "[[project]]\ndir = \"~/work\"\nrepo = \"team/sessions\"\n",
        );
        let found = resolve_repo_from(&config, Some(home), &project)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
        // Without a home dir the tilde can't expand and the rule is inert.
        assert!(
            resolve_repo_from(&config, None, &project)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn global_rule_without_repo_is_skipped() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!("[[project]]\ndir = {:?}\n", project.display().to_string()),
        );
        assert!(
            resolve_repo_from(&config, None, &project)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn global_rule_matches_deleted_checkout_by_path_string() {
        // The rule dir and session dir don't exist on disk; matching is
        // pure path logic so the mapping still applies (e.g. sharing a
        // cached session whose checkout was removed).
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        write(
            &config,
            "[[project]]\ndir = \"/gone/checkout\"\nrepo = \"team/sessions\"\n",
        );
        let found = resolve_repo_from(&config, None, Path::new("/gone/checkout/sub"))
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
    }

    #[test]
    fn tracked_toolpath_toml_in_repo_is_not_consulted() {
        // A committed `.toolpath.toml` must not redirect uploads without
        // a consent flow (issue #179) — only the user's own config
        // applies.
        let temp = TempDir::new().unwrap();
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(
            &repo_root.join(".toolpath.toml"),
            "[share]\nrepo = \"team/sessions\"\n",
        );
        let missing_global = temp.path().join("config.toml");
        assert!(
            resolve_repo_from(&missing_global, None, &repo_root)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_global_config_errors_with_file_path() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        write(&config, "[[project]\ndir = broken");
        let err = resolve_repo_from(&config, None, temp.path()).unwrap_err();
        assert!(
            err.to_string().contains("config.toml"),
            "error should name the file: {err:#}"
        );
    }

    #[test]
    fn invalid_repo_string_errors_with_origin() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nrepo = \"not-owner-slash-name\"\n",
                project.display().to_string()
            ),
        );
        let err = resolve_repo_from(&config, None, &project).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner/name"), "got: {msg}");
        assert!(msg.contains("config.toml"), "got: {msg}");
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // Forward compat: config written for a newer CLI must not break
        // this one.
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!(
                "[defaults]\nfuture = true\n\n\
                 [[project]]\ndir = {:?}\nrepo = \"team/sessions\"\nfuture_knob = 3\n",
                project.display().to_string()
            ),
        );
        let found = resolve_repo_from(&config, None, &project).unwrap().unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
    }

    #[test]
    #[cfg(unix)]
    fn session_dir_reached_via_symlink_matches_canonical_rule() {
        let temp = TempDir::new().unwrap();
        let real = temp.path().join("real-proj");
        std::fs::create_dir_all(&real).unwrap();
        let link = temp.path().join("link-proj");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = temp.path().join("config.toml");
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nrepo = \"team/sessions\"\n",
                real.display().to_string()
            ),
        );
        let found = resolve_repo_from(&config, None, &link).unwrap().unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
    }

    #[test]
    fn expand_tilde_handles_bare_and_prefixed_forms() {
        let home = Path::new("/home/alex");
        assert_eq!(expand_tilde("~", Some(home)), PathBuf::from("/home/alex"));
        assert_eq!(
            expand_tilde("~/work", Some(home)),
            PathBuf::from("/home/alex/work")
        );
        assert_eq!(
            expand_tilde("/abs/path", Some(home)),
            PathBuf::from("/abs/path")
        );
        assert_eq!(expand_tilde("~/work", None), PathBuf::from("~/work"));
    }
}
