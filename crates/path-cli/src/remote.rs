//! The share-remote model: where a session upload goes.
//!
//! A remote is either a bare `owner/name` (a Pathbase repo on the
//! credentialed/default server), a canonical Pathbase repo web URL
//! (`https://<host>/u/<owner>/<name>`), which also carries the server,
//! or an object-storage destination (`s3://`, `s3a://`, `file://`, or a
//! folder path) — accepted and kept as written, parsed with
//! `store::Destination::parse` at use.
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

/// Where a configured share goes.
#[derive(Debug, Clone)]
pub(crate) enum Remote {
    /// A Pathbase repo, optionally pinned to a server.
    Pathbase {
        repo: RepoSpec,
        base_url: Option<String>,
    },
    /// An object-storage destination (`s3://bucket/prefix`, `file:///dir`,
    /// or a folder path), kept as written and parsed with
    /// `store::Destination::parse` at use.
    Object(String),
}

/// Parse a remote value: bare `owner/name` (Pathbase, default server), a
/// canonical Pathbase repo web URL whose authority becomes the server
/// base URL, or an object-storage destination — `s3://`, `s3a://`,
/// `file://`, or a folder path starting with `/`, `~`, `./`, or `../`.
/// `origin` names where the value came from, for error messages.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn parse_remote(value: &str, origin: &str) -> Result<Remote> {
    if let Some((scheme, _)) = value.split_once("://") {
        return match scheme {
            "http" | "https" => {
                let (base_url, repo) = parse_pathbase_repo_url(value, origin)?;
                Ok(Remote::Pathbase {
                    repo,
                    base_url: Some(base_url),
                })
            }
            "s3" | "s3a" | "file" => {
                crate::store::Destination::parse(value)
                    .map_err(|e| anyhow!("invalid destination in {origin}: {e:#}"))?;
                Ok(Remote::Object(value.to_string()))
            }
            other => bail!(
                "unsupported remote scheme `{other}` in {origin}: expected `owner/name`, a \
                 Pathbase repo URL like https://pathbase.dev/u/owner/name, or an object \
                 destination like s3://bucket/prefix or a folder path"
            ),
        };
    }
    if value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("./")
        || value.starts_with("../")
    {
        crate::store::Destination::parse(value)
            .map_err(|e| anyhow!("invalid destination in {origin}: {e:#}"))?;
        return Ok(Remote::Object(value.to_string()));
    }
    let repo = parse_repo_spec(value).map_err(|e| anyhow!("invalid remote in {origin}: {e}"))?;
    Ok(Remote::Pathbase {
        repo,
        base_url: None,
    })
}

/// Split a canonical Pathbase repo web URL into its server base URL and
/// `owner/name`. The path must be exactly `/u/<owner>/<name>` (trailing
/// slash tolerated) — graph URLs and other server pages are not remotes.
#[cfg(not(target_os = "emscripten"))]
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
        let Remote::Pathbase { repo, base_url } =
            parse_remote("http://127.0.0.1:8080/u/a/b", "test").unwrap()
        else {
            panic!("expected a Pathbase remote");
        };
        assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("a", "b"));
    }

    #[test]
    fn url_remote_tolerates_trailing_slash() {
        let Remote::Pathbase { repo, base_url } =
            parse_remote("https://pathbase.dev/u/team/sessions/", "test").unwrap()
        else {
            panic!("expected a Pathbase remote");
        };
        assert_eq!(base_url.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(repo.name, "sessions");
    }

    #[test]
    fn bare_remote_has_no_base_url() {
        let Remote::Pathbase { repo, base_url } = parse_remote("team/sessions", "test").unwrap()
        else {
            panic!("expected a Pathbase remote");
        };
        assert_eq!(base_url, None);
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
    fn object_remotes_are_recognized_by_scheme_or_path_shape() {
        for value in [
            "s3://team-bucket/traces",
            "s3a://team-bucket/traces",
            "file:///srv/traces",
            "/srv/traces",
            "~/Dropbox/traces",
            "./traces",
        ] {
            match parse_remote(value, "test").unwrap() {
                Remote::Object(d) => assert_eq!(d, value),
                other => panic!("{value} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn pathbase_remotes_still_parse() {
        assert!(matches!(
            parse_remote("team/sessions", "test").unwrap(),
            Remote::Pathbase { base_url: None, .. }
        ));
        assert!(matches!(
            parse_remote("https://pathbase.dev/u/team/sessions", "test").unwrap(),
            Remote::Pathbase {
                base_url: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn a_bare_relative_object_remote_is_rejected_like_a_destination() {
        // `team-bucket/traces` is ambiguous with `owner/name`, and as a
        // destination it is the bare-relative trap; it stays a Pathbase
        // repo spec, which is what it always was.
        assert!(matches!(
            parse_remote("team-bucket/traces", "test").unwrap(),
            Remote::Pathbase { .. }
        ));
        let err = parse_remote("gs://bucket", "test").unwrap_err().to_string();
        assert!(err.contains("unsupported remote scheme"), "{err}");
    }

    #[test]
    fn invalid_bare_remote_names_origin() {
        let err = parse_remote("not-owner-slash-name", "my-config").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner/name"), "got: {msg}");
        assert!(msg.contains("my-config"), "got: {msg}");
    }
}
