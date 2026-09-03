//! Project-directory → share remote mapping for `path share`.
//!
//! `~/.toolpath/config.toml` `[[project]]` rules name the remote a
//! project's sessions upload to. A rule selects sessions by `dir`, a
//! subtree matched against the session's own directory — the project
//! for path-keyed harnesses, the recorded cwd otherwise — or by
//! `origin`, the `owner/name` of the git remote called `origin` in the
//! repository enclosing that directory, or by both. Any matching
//! `origin` rule beats every `dir` rule; among `dir` rules the most
//! specific wins. The `--repo` flag beats config entirely (that
//! precedence lives in `cmd_share::resolve_destination`).
//!
//! `dir` matching is pure path logic, so such a rule still applies when
//! the checkout it names has been deleted. `origin` matching is the
//! opposite trade: it reads the repository, so it needs the checkout to
//! exist — and in exchange it follows a repo that moves, is renamed, or
//! is checked out somewhere else entirely, and covers every worktree of
//! it without naming any of them.
//!
//! The remote value grammar (bare `owner/name`, or a canonical Pathbase
//! repo web URL that also carries the server) lives in `crate::remote`;
//! this module only decides *which* rule applies to a directory.
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

use crate::config::{home_dir, home_relative};
use crate::remote::{RepoSpec, parse_remote, parse_repo_spec};

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
    let global = crate::config::config_dir()?.join(crate::config::CONFIG_FILE_NAME);
    resolve_remote_from(&global, home_dir().as_deref(), session_dir)
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

/// One `[[project]]` rule: a selector for the sessions it applies to,
/// and the settings that apply to them. `remote` is optional so future
/// per-project settings don't force one.
///
/// Two selectors. `dir` is a directory subtree (`~/`-expandable),
/// matched against the session's own directory. `origin` is the
/// `owner/name` of the git remote called `origin` in the repository
/// enclosing that directory — a name rather than a location, so it
/// survives moving or renaming the checkout and covers every worktree
/// of the repo without naming any of them. A rule carrying both must
/// satisfy both; a rule carrying neither is a config error.
///
/// Note the two senses of the word in this module: a rule's `origin`
/// key is a git repository, while `ConfiguredRemote::origin` is the
/// human-readable provenance of a resolved rule.
#[derive(Debug, Deserialize)]
struct ProjectRule {
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    remote: Option<String>,
}

impl ProjectRule {
    /// How this rule is named in provenance strings and errors.
    fn label(&self) -> String {
        match (&self.dir, &self.origin) {
            (Some(dir), Some(origin)) => format!("dir = {dir:?}, origin = {origin:?}"),
            (Some(dir), None) => format!("dir = {dir:?}"),
            (None, Some(origin)) => format!("origin = {origin:?}"),
            (None, None) => "no selector".to_string(),
        }
    }
}

/// The `owner/name` of the `origin` remote of the repository enclosing
/// `dir`, if there is one. Every failure along the way — not a
/// repository, no remote called `origin`, a URL with no recognizable
/// owner/name tail — is `None`: an `origin` rule then simply does not
/// match, the same as a `dir` rule pointing somewhere else.
fn git_origin_spec(dir: &Path) -> Option<RepoSpec> {
    let repo = git2::Repository::discover(dir).ok()?;
    let url = repo.find_remote("origin").ok()?.url()?.to_string();
    parse_git_url_spec(&url)
}

/// Pull `owner/name` out of a git remote URL. Handles the forms git
/// itself accepts — `https://host/owner/name.git`, scp-style
/// `git@host:owner/name`, `ssh://git@host:22/owner/name` — by taking
/// the last two segments once `:` is treated as a separator.
///
/// A local-path origin (`/srv/git/owner/name`) yields its last two
/// path components, which is harmless: matching one would require a
/// rule that names exactly those.
fn parse_git_url_spec(url: &str) -> Option<RepoSpec> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let normalized = trimmed.replace(':', "/");
    let mut segments = normalized.rsplit('/').filter(|s| !s.is_empty());
    let name = segments.next()?;
    let owner = segments.next()?;
    parse_repo_spec(&format!("{owner}/{name}")).ok()
}

/// GitHub and Pathbase both treat owner and name case-insensitively, so
/// a rule written `Empathic/Toolpath` matches an origin of
/// `empathic/toolpath`.
fn spec_eq(a: &RepoSpec, b: &RepoSpec) -> bool {
    a.owner.eq_ignore_ascii_case(&b.owner) && a.name.eq_ignore_ascii_case(&b.name)
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

    // An `origin` rule names the repository itself, a `dir` rule names
    // a place it happens to sit, so identity wins over location: any
    // matching `origin` rule beats every `dir` rule. Among `origin`
    // rules the first in the file wins — they match exactly, so a
    // second match is a duplicate, not a refinement. Among `dir` rules
    // the most specific wins, and among equally deep dirs the first.
    //
    // The repository lookup is done at most once, and only if some rule
    // actually asks for it: a config of pure `dir` rules touches no git
    // repository at all.
    let mut session_origin: Option<Option<RepoSpec>> = None;
    let mut best_origin: Option<&ProjectRule> = None;
    let mut best_dir: Option<(usize, &ProjectRule)> = None;

    for rule in &config.project {
        if rule.remote.is_none() || (rule.dir.is_none() && rule.origin.is_none()) {
            continue;
        }
        if let Some(want) = &rule.origin {
            let Ok(want) = parse_repo_spec(want) else {
                continue;
            };
            let have = session_origin.get_or_insert_with(|| git_origin_spec(session_dir));
            if !have.as_ref().is_some_and(|have| spec_eq(have, &want)) {
                continue;
            }
        }
        let mut dir_depth = None;
        if let Some(dir) = &rule.dir {
            let rule_dir = canonicalize_prefix(&expand_tilde(dir, home));
            if !session_dir.starts_with(&rule_dir) {
                continue;
            }
            dir_depth = Some(rule_dir.components().count());
        }
        // Everything the rule asked for matched.
        if rule.origin.is_some() {
            best_origin.get_or_insert(rule);
        } else if let Some(depth) = dir_depth
            && best_dir.is_none_or(|(d, _)| depth > d)
        {
            best_dir = Some((depth, rule));
        }
    }
    let Some(rule) = best_origin.or(best_dir.map(|(_, rule)| rule)) else {
        return Ok(None);
    };
    let origin = format!("{} ({})", home_relative(config_path, home), rule.label());
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

/// Parse `text` as the personal config and check every rule: it must
/// carry a selector (`dir`, `origin`, or both), its `origin` must be a
/// bare `owner/name`, and its `remote` must satisfy the remote grammar.
/// Returns the number of `[[project]]` rules. `file` names the file for
/// error messages. Used by `path config edit` so mistakes surface at
/// edit time instead of at the next `share`.
pub(crate) fn validate_config_text(text: &str, file: &str) -> Result<usize> {
    let config: GlobalConfig =
        toml::from_str(text).with_context(|| format!("failed to parse {file}"))?;
    for (i, rule) in config.project.iter().enumerate() {
        if rule.dir.is_none() && rule.origin.is_none() {
            bail!(
                "{file}: [[project]] rule {} has neither `dir` nor `origin`; \
                 a rule needs at least one to select the sessions it applies to",
                i + 1
            );
        }
        let where_ = format!("{file} ({})", rule.label());
        if let Some(origin) = rule.origin.as_deref() {
            parse_repo_spec(origin).map_err(|e| anyhow!("{where_}: invalid `origin`: {e}"))?;
        }
        if let Some(remote) = rule.remote.as_deref() {
            parse_remote(remote, &where_)?;
        }
    }
    Ok(config.project.len())
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
    fn validate_counts_rules_and_accepts_remote_less_ones() {
        let n = validate_config_text(
            "[[project]]\ndir = \"~/a\"\nremote = \"team/sessions\"\n\n\
             [[project]]\ndir = \"~/b\"\n",
            "test config",
        )
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(validate_config_text("", "test config").unwrap(), 0);
    }

    #[test]
    fn validate_rejects_bad_toml_and_bad_remotes_naming_the_origin() {
        let err = validate_config_text("[[project]\ndir = broken", "my.toml").unwrap_err();
        assert!(err.to_string().contains("my.toml"), "{err:#}");

        let err = validate_config_text(
            "[[project]]\ndir = \"~/a\"\nremote = \"ftp://x/u/a/b\"\n",
            "my.toml",
        )
        .unwrap_err();
        assert!(err.to_string().contains("my.toml"), "{err:#}");
        assert!(err.to_string().contains("~/a"), "{err:#}");
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

    /// A repository at `dir` whose `origin` remote is `url`.
    fn init_repo_with_origin(dir: &Path, url: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let repo = git2::Repository::init(dir).unwrap();
        repo.remote("origin", url).unwrap();
    }

    #[test]
    fn git_url_forms_all_yield_owner_and_name() {
        for url in [
            "https://github.com/empathic/toolpath.git",
            "https://github.com/empathic/toolpath",
            "https://github.com/empathic/toolpath/",
            "git@github.com:empathic/toolpath.git",
            "ssh://git@github.com/empathic/toolpath.git",
            "ssh://git@github.com:22/empathic/toolpath.git",
            "git://github.com/empathic/toolpath.git",
        ] {
            let spec = parse_git_url_spec(url).unwrap_or_else(|| panic!("no spec for {url}"));
            assert_eq!(
                format!("{}/{}", spec.owner, spec.name),
                "empathic/toolpath",
                "{url}"
            );
        }
        // Nothing that could be an owner/name tail.
        assert!(parse_git_url_spec("toolpath").is_none());
        assert!(parse_git_url_spec("").is_none());
    }

    #[test]
    fn origin_rule_matches_the_enclosing_repository() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let checkout = temp.path().join("anywhere-at-all");
        init_repo_with_origin(&checkout, "git@github.com:empathic/toolpath.git");
        write(
            &config,
            "[[project]]\norigin = \"empathic/toolpath\"\nremote = \"dev/pathstash\"\n",
        );

        // The rule names no path, so it matches from a subdirectory too.
        let nested = checkout.join("crates/path-cli");
        std::fs::create_dir_all(&nested).unwrap();
        for dir in [&checkout, &nested] {
            let found = resolve_remote_from(&config, None, dir).unwrap().unwrap();
            assert_eq!(repo_str(&found), "dev/pathstash");
            assert!(found.origin.contains("empathic/toolpath"), "{found:?}");
        }

        // A directory in no repository at all matches nothing.
        let outside = temp.path().join("not-a-repo");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(
            resolve_remote_from(&config, None, &outside)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn origin_match_is_case_insensitive() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let checkout = temp.path().join("co");
        init_repo_with_origin(&checkout, "https://github.com/empathic/toolpath.git");
        write(
            &config,
            "[[project]]\norigin = \"Empathic/ToolPath\"\nremote = \"dev/pathstash\"\n",
        );
        let found = resolve_remote_from(&config, None, &checkout)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "dev/pathstash");
    }

    #[test]
    fn origin_rule_beats_a_more_specific_dir_rule() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let checkout = temp.path().join("deep/nested/checkout");
        init_repo_with_origin(&checkout, "git@github.com:empathic/toolpath.git");
        // The dir rule is as specific as it gets — it names the checkout
        // exactly — and the origin rule still wins.
        write(
            &config,
            &format!(
                "[[project]]\ndir = {dir:?}\nremote = \"me/by-path\"\n\n\
                 [[project]]\norigin = \"empathic/toolpath\"\nremote = \"me/by-origin\"\n",
                dir = checkout.display().to_string(),
            ),
        );
        let found = resolve_remote_from(&config, None, &checkout)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "me/by-origin");
    }

    #[test]
    fn rule_with_both_selectors_needs_both_to_match() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let inside = temp.path().join("work/checkout");
        let elsewhere = temp.path().join("other/checkout");
        init_repo_with_origin(&inside, "git@github.com:empathic/toolpath.git");
        init_repo_with_origin(&elsewhere, "git@github.com:empathic/toolpath.git");
        write(
            &config,
            &format!(
                "[[project]]\ndir = {dir:?}\norigin = \"empathic/toolpath\"\n\
                 remote = \"me/scoped\"\n",
                dir = temp.path().join("work").display().to_string(),
            ),
        );
        let found = resolve_remote_from(&config, None, &inside)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "me/scoped");
        // Right origin, wrong subtree.
        assert!(
            resolve_remote_from(&config, None, &elsewhere)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn first_matching_origin_rule_wins() {
        let temp = TempDir::new().unwrap();
        let config = temp.path().join("config.toml");
        let checkout = temp.path().join("co");
        init_repo_with_origin(&checkout, "git@github.com:empathic/toolpath.git");
        write(
            &config,
            "[[project]]\norigin = \"empathic/toolpath\"\nremote = \"me/first\"\n\n\
             [[project]]\norigin = \"empathic/toolpath\"\nremote = \"me/second\"\n",
        );
        let found = resolve_remote_from(&config, None, &checkout)
            .unwrap()
            .unwrap();
        assert_eq!(repo_str(&found), "me/first");
    }

    #[test]
    fn validate_rejects_a_rule_with_no_selector() {
        let err = validate_config_text("[[project]]\nremote = \"me/x\"\n", "my.toml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("my.toml"), "{msg}");
        assert!(msg.contains("neither `dir` nor `origin`"), "{msg}");
    }

    #[test]
    fn validate_rejects_a_malformed_origin() {
        let err = validate_config_text(
            "[[project]]\norigin = \"toolpath\"\nremote = \"me/x\"\n",
            "my.toml",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid `origin`"), "{msg}");
        assert!(msg.contains("owner/name"), "{msg}");
    }

    #[test]
    fn validate_accepts_an_origin_only_rule() {
        let n = validate_config_text(
            "[[project]]\norigin = \"empathic/toolpath\"\nremote = \"dev/pathstash\"\n",
            "my.toml",
        )
        .unwrap();
        assert_eq!(n, 1);
    }
}
