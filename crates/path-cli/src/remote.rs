//! The share-remote model: where a session upload goes.
//!
//! A remote is either a bare `owner/name` (a Pathbase repo on the
//! credentialed/default server) or a canonical Pathbase repo web URL
//! (`https://<host>/u/<owner>/<name>`), which also carries the server.
//! The URL scheme is the extension point for future backends (an
//! `s3://` bucket, say) — unknown schemes are rejected today.
//!
//! This module is scheme parsing and types only, shared by the `--repo`
//! flag parsers in the cmd modules and by `share_config`'s rule
//! resolution — cmd modules consume it, never the other way around.

use anyhow::{Result, anyhow, bail};

/// `owner/name` pair naming a Pathbase repo.
#[derive(Debug, Clone)]
pub(crate) struct RepoSpec {
    pub(crate) owner: String,
    pub(crate) name: String,
}

/// Parse a bare `owner/name`. Also the clap `value_parser` for `--repo`,
/// hence the `String` error type.
pub(crate) fn parse_repo_spec(s: &str) -> std::result::Result<RepoSpec, String> {
    let (owner, name) = s
        .split_once('/')
        .ok_or_else(|| format!("expected owner/name, got `{s}`"))?;
    if owner.is_empty() || name.is_empty() {
        return Err(format!("expected owner/name, got `{s}`"));
    }
    Ok(RepoSpec {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// Parse a remote value: bare `owner/name`, or a canonical Pathbase
/// repo web URL whose authority becomes the server base URL. `origin`
/// names where the value came from, for error messages.
pub(crate) fn parse_remote(value: &str, origin: &str) -> Result<(RepoSpec, Option<String>)> {
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
    let repo = parse_repo_spec(value).map_err(|e| anyhow!("invalid remote in {origin}: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_spec_accepts_owner_slash_name() {
        let spec = parse_repo_spec("alex/pathstash").unwrap();
        assert_eq!(spec.owner, "alex");
        assert_eq!(spec.name, "pathstash");
    }

    #[test]
    fn parse_repo_spec_rejects_missing_slash() {
        assert!(parse_repo_spec("alex").is_err());
        assert!(parse_repo_spec("/pathstash").is_err());
        assert!(parse_repo_spec("alex/").is_err());
    }

    #[test]
    fn url_remote_keeps_port_and_scheme() {
        let (repo, base) = parse_remote("http://127.0.0.1:8080/u/a/b", "test").unwrap();
        assert_eq!(base.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("a", "b"));
    }

    #[test]
    fn url_remote_tolerates_trailing_slash() {
        let (repo, base) = parse_remote("https://pathbase.dev/u/team/sessions/", "test").unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(repo.name, "sessions");
    }

    #[test]
    fn bare_remote_has_no_base_url() {
        let (repo, base) = parse_remote("team/sessions", "test").unwrap();
        assert_eq!(base, None);
        assert_eq!(repo.owner, "team");
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
    fn invalid_bare_remote_names_origin() {
        let err = parse_remote("not-owner-slash-name", "my-config").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner/name"), "got: {msg}");
        assert!(msg.contains("my-config"), "got: {msg}");
    }
}
