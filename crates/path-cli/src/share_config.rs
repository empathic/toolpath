//! Project-directory → share remote mapping for `path share`.
//!
//! `~/.toolpath/config.toml` `[[project]]` rules name the remote a
//! project's sessions upload to: each rule carries a `dir` and a
//! `remote`, matched by subtree against the session's own directory —
//! the project for path-keyed harnesses, the recorded cwd otherwise.
//! The most specific matching `dir` wins. The `--repo` flag beats
//! config (that precedence lives in `cmd_share::resolve_destination`).
//! Rule matching is pure path logic, so a rule still applies when the
//! checkout it names has been deleted.
//!
//! A remote is either a bare `owner/name` (a Pathbase repo on the
//! credentialed/default server) or a canonical Pathbase repo web URL
//! (`https://<host>/u/<owner>/<name>`), which also carries the server.
//! The URL scheme is the extension point for future backends (an
//! `s3://` bucket, say) — unknown schemes are rejected today.
//!
//! Only the user's own config is consulted. A repo-tracked
//! `.toolpath.toml` variant (a team commits the mapping once, every
//! clone follows it) was built and deliberately pulled out: a committed
//! file silently redirecting other users' uploads needs a first-use
//! consent flow first — see issue #179.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::cmd_export::RepoSpec;
use crate::cmd_share::home_relative;

/// Personal config file under the toolpath config dir.
pub(crate) const GLOBAL_CONFIG_FILE: &str = "config.toml";

/// A share remote resolved from config. `display` is the remote exactly
/// as configured; `origin` is the human-readable provenance ("which
/// file, which rule") — both are shown in the "Sharing to" line and in
/// errors.
#[derive(Debug)]
pub(crate) struct ConfiguredRemote {
    pub(crate) repo: RepoSpec,
    /// Server base URL when the remote is a full Pathbase repo URL;
    /// `None` for bare `owner/name` (credentialed/default server).
    pub(crate) base_url: Option<String>,
    pub(crate) display: String,
    pub(crate) origin: String,
}

/// Resolve the share remote configured for `session_dir`, if any.
pub(crate) fn resolve_remote(session_dir: &Path) -> Result<Option<ConfiguredRemote>> {
    let global = crate::config::config_dir()?.join(GLOBAL_CONFIG_FILE);
    resolve_remote_from(
        &global,
        crate::cmd_share::home_dir().as_deref(),
        session_dir,
    )
}

fn resolve_remote_from(
    global_config: &Path,
    home: Option<&Path>,
    session_dir: &Path,
) -> Result<Option<ConfiguredRemote>> {
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
/// settings applying to sessions recorded under it. `remote` is optional
/// so future per-project settings don't force one.
#[derive(Debug, Deserialize)]
struct ProjectRule {
    dir: String,
    #[serde(default)]
    remote: Option<String>,
}

fn global_rule(
    config_path: &Path,
    home: Option<&Path>,
    session_dir: &Path,
) -> Result<Option<ConfiguredRemote>> {
    let Some(text) = read_optional(config_path)? else {
        return Ok(None);
    };
    let config: GlobalConfig = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    // Most specific matching rule wins; among equally deep dirs, the
    // first rule in the file wins.
    let mut best: Option<(usize, &ProjectRule)> = None;
    for rule in &config.project {
        if rule.remote.is_none() {
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
    let value = rule
        .remote
        .as_deref()
        .expect("remote-less rules were skipped");
    let (repo, base_url) = parse_remote(value, &origin)?;
    Ok(Some(ConfiguredRemote {
        repo,
        base_url,
        display: value.to_string(),
        origin,
    }))
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

/// Parse a remote value: bare `owner/name`, or a canonical Pathbase
/// repo web URL (`https://<host>/u/<owner>/<name>`) whose authority
/// becomes the server base URL. The scheme is the backend selector, so
/// anything other than http(s) is rejected with the supported forms.
fn parse_remote(value: &str, origin: &str) -> Result<(RepoSpec, Option<String>)> {
    if let Some((scheme, _)) = value.split_once("://") {
        if scheme != "http" && scheme != "https" {
            bail!(
                "unsupported remote scheme `{scheme}` in {origin}: expected `owner/name` \
                 or a Pathbase repo URL like https://pathbase.dev/u/owner/name"
            );
        }
        let (base_url, repo) = parse_pathbase_repo_url(value, origin)?;
        return Ok((repo, Some(base_url)));
    }
    let repo = crate::cmd_export::parse_repo_spec(value)
        .map_err(|e| anyhow!("invalid remote in {origin}: {e}"))?;
    Ok((repo, None))
}

/// Split a canonical Pathbase repo web URL into its server base URL and
/// `owner/name`. The path must be exactly `/u/<owner>/<name>` (trailing
/// slash tolerated) — graph URLs and other server pages are not remotes.
fn parse_pathbase_repo_url(value: &str, origin: &str) -> Result<(String, RepoSpec)> {
    let complaint = || {
        anyhow!(
            "invalid Pathbase repo URL in {origin}: expected \
             https://<host>/u/<owner>/<name>, got `{value}`"
        )
    };
    if value.contains('?') || value.contains('#') {
        return Err(complaint());
    }
    let (scheme, rest) = value.split_once("://").expect("caller checked the scheme");
    let (authority, path) = rest.split_once('/').ok_or_else(complaint)?;
    if authority.is_empty() {
        return Err(complaint());
    }
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    match segments.as_slice() {
        ["u", owner, name] if !owner.is_empty() && !name.is_empty() => Ok((
            format!("{scheme}://{authority}"),
            RepoSpec {
                owner: (*owner).to_string(),
                name: (*name).to_string(),
            },
        )),
        _ => Err(complaint()),
    }
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

    fn repo_str(found: &ConfiguredRemote) -> String {
        format!("{}/{}", found.repo.owner, found.repo.name)
    }

    #[test]
    fn no_config_resolves_to_none() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let got = resolve_remote_from(&temp.path().join("config.toml"), None, &project).unwrap();
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
                "[[project]]\ndir = {:?}\nremote = \"team/sessions\"\n",
                temp.path().join("work").display().to_string()
            ),
        );
        // Both the configured dir itself and a nested session dir match.
        let found = resolve_remote_from(&config, None, &project)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
        assert_eq!(found.base_url, None);
        assert_eq!(found.display, "team/sessions");
        assert!(
            found.origin.contains("config.toml"),
            "origin: {}",
            found.origin
        );
        let found = resolve_remote_from(&config, None, &project.join("deep/nested"))
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
            "[[project]]\ndir = \"/somewhere/else\"\nremote = \"a/b\"\n",
        );
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        assert!(
            resolve_remote_from(&config, None, &project)
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
                "[[project]]\ndir = {:?}\nremote = \"a/b\"\n",
                temp.path().join("proj").display().to_string()
            ),
        );
        let sibling = temp.path().join("proj-two");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(
            resolve_remote_from(&config, None, &sibling)
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
                "[[project]]\ndir = {broad:?}\nremote = \"me/misc\"\n\n\
                 [[project]]\ndir = {narrow:?}\nremote = \"acme/sessions\"\n",
                broad = clients.display().to_string(),
                narrow = acme.display().to_string(),
            ),
        );
        let found = resolve_remote_from(&config, None, &acme).unwrap().unwrap();
        assert_eq!(repo_str(&found), "acme/sessions");
        let found = resolve_remote_from(&config, None, &clients.join("other"))
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
            "[[project]]\ndir = \"~/work\"\nremote = \"team/sessions\"\n",
        );
        let found = resolve_remote_from(&config, Some(home), &project)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
        // Without a home dir the tilde can't expand and the rule is inert.
        assert!(
            resolve_remote_from(&config, None, &project)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn global_rule_without_remote_is_skipped() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!("[[project]]\ndir = {:?}\n", project.display().to_string()),
        );
        assert!(
            resolve_remote_from(&config, None, &project)
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
            "[[project]]\ndir = \"/gone/checkout\"\nremote = \"team/sessions\"\n",
        );
        let found = resolve_remote_from(&config, None, Path::new("/gone/checkout/sub"))
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
            "[share]\nremote = \"team/sessions\"\n",
        );
        let missing_global = temp.path().join("config.toml");
        assert!(
            resolve_remote_from(&missing_global, None, &repo_root)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn url_remote_carries_server_and_repo() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nremote = \"https://pathbase.dev/u/team/sessions\"\n",
                project.display().to_string()
            ),
        );
        let found = resolve_remote_from(&config, None, &project)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "team/sessions");
        assert_eq!(found.base_url.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(found.display, "https://pathbase.dev/u/team/sessions");
    }

    #[test]
    fn url_remote_keeps_port_and_scheme() {
        let (base, repo) = parse_pathbase_repo_url("http://127.0.0.1:8080/u/a/b", "test").unwrap();
        assert_eq!(base, "http://127.0.0.1:8080");
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("a", "b"));
    }

    #[test]
    fn url_remote_tolerates_trailing_slash() {
        let (base, repo) =
            parse_pathbase_repo_url("https://pathbase.dev/u/team/sessions/", "test").unwrap();
        assert_eq!(base, "https://pathbase.dev");
        assert_eq!(repo.name, "sessions");
    }

    #[test]
    fn url_remote_rejects_non_repo_pages() {
        // Graph URLs, bare hosts, missing /u/ prefixes, and query
        // strings are not remotes.
        for bad in [
            "https://pathbase.dev/u/team/sessions/graphs/abc",
            "https://pathbase.dev",
            "https://pathbase.dev/",
            "https://pathbase.dev/team/sessions",
            "https://pathbase.dev/u/team",
            "https:///u/team/sessions",
            "https://pathbase.dev/u/team/sessions?x=1",
        ] {
            let err = parse_remote(bad, "test").unwrap_err();
            assert!(
                err.to_string().contains("expected"),
                "`{bad}` should be rejected with the expected form: {err}"
            );
        }
    }

    #[test]
    fn unsupported_scheme_is_rejected() {
        let err = parse_remote("s3://bucket/sessions", "test").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported remote scheme `s3`"), "got: {msg}");
        assert!(msg.contains("owner/name"), "got: {msg}");
    }

    #[test]
    fn malformed_global_config_errors_with_file_path() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        write(&config, "[[project]\ndir = broken");
        let err = resolve_remote_from(&config, None, temp.path()).unwrap_err();
        assert!(
            err.to_string().contains("config.toml"),
            "error should name the file: {err:#}"
        );
    }

    #[test]
    fn invalid_remote_string_errors_with_origin() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &config,
            &format!(
                "[[project]]\ndir = {:?}\nremote = \"not-owner-slash-name\"\n",
                project.display().to_string()
            ),
        );
        let err = resolve_remote_from(&config, None, &project).unwrap_err();
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
                 [[project]]\ndir = {:?}\nremote = \"team/sessions\"\nfuture_knob = 3\n",
                project.display().to_string()
            ),
        );
        let found = resolve_remote_from(&config, None, &project)
            .unwrap()
            .unwrap();
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
                "[[project]]\ndir = {:?}\nremote = \"team/sessions\"\n",
                real.display().to_string()
            ),
        );
        let found = resolve_remote_from(&config, None, &link).unwrap().unwrap();
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
