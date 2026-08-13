//! Single-session derivation shared by `p import`, `share`, and the
//! other surfaces that turn one external artifact into a cached
//! toolpath document: `DerivedDoc` plus the per-provider
//! `derive_*_session` helpers and the Pathbase fetch used by
//! `p import pathbase` and `resume`.

use anyhow::Result;
use std::path::PathBuf;
use toolpath::v1::Graph;

use crate::artifact::{ArtifactRef, ArtifactType, claude_chain_stamp, stat_stamp};
use crate::cache::make_id;
use crate::config::Config;
use crate::providers;

pub(crate) struct DerivedDoc {
    pub(crate) cache_id: String,
    pub(crate) doc: Graph,
    /// Identity + source stamp for the sync manifest, captured *before*
    /// the source was read (so a write racing the derive re-syncs next
    /// run). `None` for sources the manifest doesn't track (github,
    /// pathbase) and for bulk `--all` derives that no longer know
    /// per-artifact sources.
    pub(crate) provenance: Option<ArtifactRef>,
}

/// Extract the inner identifier from a graph (without source prefix).
pub(crate) fn doc_inner_id(doc: &Graph) -> String {
    doc.graph.id.clone()
}

/// Derive a single Claude conversation given an explicit project + session.
/// Used by `cmd_share` after its picker has resolved the pair; mirrors the
/// `(Some(p), Some(s), _)` arm in [`derive_claude_with_manager`].
pub(crate) fn derive_claude_session(
    config: &Config,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    derive_claude_session_with(
        &toolpath_claude::ClaudeConvo::with_resolver(providers::claude_resolver(config)),
        project,
        session,
    )
}

/// [`derive_claude_session`] against a caller-supplied manager, so sync
/// derives from the same roots it enumerated (and tests can inject a
/// fixture resolver).
pub(crate) fn derive_claude_session_with(
    manager: &toolpath_claude::ClaudeConvo,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    // Fingerprint the whole session chain, and key the manifest record
    // by the chain head (the rotation-stable id sync enumerates), so a
    // caller passing a successor-segment id still records the artifact
    // sync will look for.
    let (modified, size) = claude_chain_stamp(manager, project, session);
    let artifact_id = manager
        .session_chain(project, session)
        .ok()
        .and_then(|chain| chain.into_iter().next())
        .unwrap_or_else(|| session.to_string());
    // The caller's project string often comes from claude's lossy dir
    // slugs ('/', '_', '.' all collapsed); leaving it out of the derive
    // lets path.base come from the session's own recorded cwd instead.
    // The cache is the archive: always derive maximally (thinking
    // included).
    let cfg = toolpath_claude::derive::DeriveConfig {
        project_path: None,
        include_thinking: true,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut path = toolpath_claude::derive::derive_path(&convo, &cfg);
    // Sessions whose entries carry no cwd would otherwise derive with no
    // base at all — fall back to the caller's project for those.
    if path.path.base.is_none() && project.starts_with('/') {
        path.path.base = Some(toolpath::v1::Base {
            uri: format!("file://{project}"),
            ref_str: None,
            branch: None,
        });
    }
    let cache_id = make_id(ArtifactType::Claude.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Claude,
            id: artifact_id,
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
}

/// Derive a single Gemini conversation given an explicit project + session.
pub(crate) fn derive_gemini_session(
    config: &Config,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    derive_gemini_session_with(
        &toolpath_gemini::GeminiConvo::with_resolver(providers::gemini_resolver(config)),
        project,
        session,
    )
}

/// [`derive_gemini_session`] against a caller-supplied manager.
pub(crate) fn derive_gemini_session_with(
    manager: &toolpath_gemini::GeminiConvo,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    let entry = manager
        .resolver()
        .list_session_entries(project)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|e| e.id == session || e.session_uuid.as_deref() == Some(session))
        });
    let (artifact_id, (modified, size)) = match &entry {
        Some(e) => (
            e.session_uuid.clone().unwrap_or_else(|| e.id.clone()),
            stat_stamp(&e.path),
        ),
        None => (session.to_string(), (None, None)),
    };
    // Maximal ingest: thinking is always derived into the cache.
    let cfg = toolpath_gemini::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking: true,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_gemini::derive::derive_path(&convo, &cfg);
    let cache_id = make_id(ArtifactType::Gemini.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Gemini,
            id: artifact_id,
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
}

/// Derive a single Codex session given an explicit session id.
pub(crate) fn derive_codex_session(config: &Config, session: &str) -> Result<DerivedDoc> {
    derive_codex_session_with(
        &toolpath_codex::CodexConvo::with_resolver(providers::codex_resolver(config)),
        session,
    )
}

/// [`derive_codex_session`] against a caller-supplied manager.
pub(crate) fn derive_codex_session_with(
    manager: &toolpath_codex::CodexConvo,
    session: &str,
) -> Result<DerivedDoc> {
    let file = manager.resolver().find_rollout_file(session).ok();
    let (modified, size) = file.as_deref().map(stat_stamp).unwrap_or((None, None));
    let artifact_id = file
        .as_deref()
        .and_then(|f| f.file_stem())
        .and_then(|stem| stem.to_str())
        .map(|stem| toolpath_codex::session_id_from_stem(stem).to_string())
        .unwrap_or_else(|| session.to_string());
    let config = toolpath_codex::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_codex::derive::derive_path(&s, &config);
    let cache_id = make_id(ArtifactType::Codex.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Codex,
            id: artifact_id,
            path: None,
            modified,
            size,
        }),
    })
}

/// Derive a single Copilot session given an explicit session id.
pub(crate) fn derive_copilot_session(config: &Config, session: &str) -> Result<DerivedDoc> {
    derive_copilot_session_with(
        &toolpath_copilot::CopilotConvo::with_resolver(providers::copilot_resolver(config)),
        session,
    )
}

/// [`derive_copilot_session`] against a caller-supplied manager.
pub(crate) fn derive_copilot_session_with(
    manager: &toolpath_copilot::CopilotConvo,
    session: &str,
) -> Result<DerivedDoc> {
    let (modified, size) = manager
        .resolver()
        .events_file(session)
        .map(|p| stat_stamp(&p))
        .unwrap_or((None, None));
    let config = toolpath_copilot::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_copilot::derive::derive_path(&s, &config);
    let cache_id = make_id(ArtifactType::Copilot.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Copilot,
            id: session.to_string(),
            path: None,
            modified,
            size,
        }),
    })
}

/// Derive a single opencode session given an explicit session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_session(
    config: &Config,
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    derive_opencode_session_with(
        &toolpath_opencode::OpencodeConvo::with_resolver(providers::opencode_resolver(config)),
        session,
        no_snapshot_diffs,
    )
}

/// [`derive_opencode_session`] against a caller-supplied manager.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_session_with(
    manager: &toolpath_opencode::OpencodeConvo,
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    let modified = manager
        .io()
        .list_sessions(None)
        .ok()
        .and_then(|sessions| sessions.into_iter().find(|s| s.id == session))
        .and_then(|s| s.last_activity());
    let config = toolpath_opencode::derive::DeriveConfig {
        no_snapshot_diffs,
        ..Default::default()
    };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path =
        toolpath_opencode::derive::derive_path_with_resolver(&s, &config, manager.resolver());
    let cache_id = make_id(ArtifactType::Opencode.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Opencode,
            id: session.to_string(),
            path: None,
            modified,
            size: None,
        }),
    })
}

/// Derive a single cursor composer given an explicit composer id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_cursor_session(config: &Config, session: &str) -> Result<DerivedDoc> {
    derive_cursor_session_with(
        &toolpath_cursor::CursorConvo::with_resolver(providers::cursor_resolver(config)),
        session,
    )
}

/// [`derive_cursor_session`] against a caller-supplied manager.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_cursor_session_with(
    manager: &toolpath_cursor::CursorConvo,
    session: &str,
) -> Result<DerivedDoc> {
    let modified = manager
        .io()
        .read_composer_headers()
        .ok()
        .and_then(|h| {
            h.all_composers
                .into_iter()
                .find(|c| c.composer_id == session)
        })
        .and_then(|c| c.last_updated_at_utc());
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let cfg = toolpath_cursor::DeriveConfig::default();
    let path = toolpath_cursor::derive_path(&s, &cfg);
    let cache_id = make_id(ArtifactType::Cursor.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Cursor,
            id: session.to_string(),
            path: None,
            modified,
            size: None,
        }),
    })
}

/// Derive a single Pi session given an explicit project + session.
pub(crate) fn derive_pi_session(
    config: &Config,
    project: &str,
    session: &str,
    base: Option<PathBuf>,
) -> Result<DerivedDoc> {
    let mut resolver = providers::pi_resolver(config);
    if let Some(path) = base {
        resolver = resolver.with_sessions_dir(&path);
    }
    derive_pi_session_with(
        &toolpath_pi::PiConvo::with_resolver(resolver),
        project,
        session,
    )
}

/// [`derive_pi_session`] against a caller-supplied manager.
pub(crate) fn derive_pi_session_with(
    manager: &toolpath_pi::PiConvo,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    let file = toolpath_pi::reader::list_session_files(manager.resolver(), project)
        .ok()
        .and_then(|files| {
            files.into_iter().find(|f| {
                toolpath_pi::reader::peek_header(f).is_ok_and(|h| h.id == session)
                    || f.file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| stem.split_once('_'))
                        .is_some_and(|(_, rest)| rest == session)
            })
        });
    let (modified, size) = file.as_deref().map(stat_stamp).unwrap_or((None, None));
    let artifact_id = session.to_string();
    let config = toolpath_pi::DeriveConfig::default();
    let session = manager
        .read_session(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
    let cache_id = make_id(ArtifactType::Pi.name(), &doc_inner_id(&doc));
    Ok(DerivedDoc {
        cache_id,
        doc,
        provenance: Some(ArtifactRef {
            artifact_type: ArtifactType::Pi,
            id: artifact_id,
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
}

/// Fetch a Pathbase ref (`https://host/u/owner/repos/repo/graphs/<uuid>`
/// URL or bare `owner/repo/<uuid>` triple) and parse it as a toolpath
/// document. Used by `path import pathbase` and `path resume <url>`.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn pathbase_fetch_to_doc(
    config: &Config,
    target: &str,
    url_flag: Option<&str>,
) -> Result<DerivedDoc> {
    use crate::cmd_pathbase::{credentials_path, graphs_download, load_session, resolve_url};

    let (base, ref_) = parse_pathbase_ref(target, url_flag)?;
    let stored = load_session(&credentials_path(config)?)?;
    let base_url = base
        .or_else(|| stored.as_ref().map(|s| s.url.clone()))
        .unwrap_or_else(|| resolve_url(config, None));

    let token = stored.as_ref().map(|s| s.token.as_str());

    let PathRef { owner, repo, id } = ref_;
    let body = graphs_download(&base_url, token, &owner, &repo, &id)?;
    let cache_id = crate::cache::pathbase_cache_id(&owner, &repo, &id);
    let doc = Graph::from_json(&body)
        .map_err(|e| anyhow::anyhow!("server returned a non-toolpath document: {e}"))?;
    Ok(DerivedDoc {
        cache_id,
        doc,
        provenance: None,
    })
}

/// What the user pointed at on the import side. Pathbase 1.1+
/// addresses graphs by UUID, so `id` is always a parseable UUID string;
/// `parse_pathbase_ref` rejects non-UUID trailing segments.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, PartialEq)]
pub(crate) struct PathRef {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) id: String,
}

/// Parse a positional ref for `path import pathbase`. Returns `(override_base, ref)`.
///
/// Accepted shapes (trailing identifier must be a UUID):
/// - Full URL: `https://host/u/<owner>/repos/<repo>/graphs/<uuid>` —
///   host overrides the server URL. Also accepts the older
///   `https://host/<owner>/<repo>/{paths|graphs}/<uuid>` shape and the
///   short `https://host/<owner>/<repo>/<uuid>` form.
/// - `<owner>/<repo>/<uuid>` — bare triple, used with `--url` or the
///   stored session.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn parse_pathbase_ref(
    target: &str,
    url_flag: Option<&str>,
) -> Result<(Option<String>, PathRef)> {
    use crate::cmd_pathbase::normalize_url;

    let scheme = if target.starts_with("https://") {
        Some("https://")
    } else if target.starts_with("http://") {
        Some("http://")
    } else {
        None
    };

    if let Some(scheme) = scheme {
        let rest = &target[scheme.len()..];
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (h, p),
            None => anyhow::bail!("URL has no path segments: {target}"),
        };
        if host.is_empty() {
            anyhow::bail!("URL is missing a host: {target}");
        }
        let path = path.split(['?', '#']).next().unwrap_or("");
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let triple = extract_triple(&segs).ok_or_else(|| {
            anyhow::anyhow!("expected URL ending in /<owner>/<repo>/graphs/<uuid> (got {target})")
        })?;
        Ok((Some(format!("{scheme}{host}")), triple))
    } else {
        let base = url_flag.map(normalize_url);
        let segs: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
        let triple = extract_triple(&segs)
            .ok_or_else(|| anyhow::anyhow!("expected `<owner>/<repo>/<uuid>`, got `{target}`"))?;
        Ok((base, triple))
    }
}

/// Pull (owner, repo, uuid) from a slash-split URL path. Accepts all of:
///
/// - `/u/<owner>/repos/<repo>/graphs/<uuid>` — current canonical form.
/// - `/<owner>/<repo>/{paths|graphs}/<uuid>` — short SvelteKit route.
/// - `/<owner>/<repo>/<uuid>` — bare triple.
///
/// The trailing segment must parse as a UUID (the only addressing
/// scheme Pathbase 1.1+ accepts for graphs).
#[cfg(not(target_os = "emscripten"))]
fn extract_triple(segs: &[&str]) -> Option<PathRef> {
    let n = segs.len();
    if n < 3 {
        return None;
    }
    let id = segs[n - 1];
    if uuid::Uuid::parse_str(id).is_err() {
        return None;
    }

    // Look back through the canonical layouts in order of specificity.
    let (owner, repo) =
        if n >= 6 && segs[n - 6] == "u" && segs[n - 4] == "repos" && segs[n - 2] == "graphs" {
            (segs[n - 5], segs[n - 3])
        } else if n >= 4 && (segs[n - 2] == "paths" || segs[n - 2] == "graphs") {
            (segs[n - 4], segs[n - 3])
        } else {
            (segs[n - 3], segs[n - 2])
        };

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(PathRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        id: id.to_string(),
    })
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    const UUID: &str = "fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537";

    #[test]
    fn parse_pathbase_ref_full_url_canonical_form() {
        let url = format!("https://pathbase.dev/u/alex/repos/pathstash/graphs/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_with_url_flag() {
        let target = format!("alex/pathstash/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&target, Some("https://other.example/")).unwrap();
        assert_eq!(base.as_deref(), Some("https://other.example"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_no_flag() {
        let target = format!("alex/pathstash/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&target, None).unwrap();
        assert_eq!(base, None);
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_url_with_trailing_slash() {
        let url = format!("https://pathbase.dev/alex/pathstash/{UUID}/");
        let (base, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(ref_.id, UUID);
    }

    #[test]
    fn parse_pathbase_ref_short_route_with_graphs_delimiter() {
        let url = format!("https://pathbase.dev/alex/pathstash/graphs/{UUID}");
        let (_, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_legacy_paths_delimiter_still_parses() {
        // Pre-1.1 share URLs used `/<owner>/<repo>/paths/<id>`. Keep
        // parsing them for back-compat — `id` still has to be a UUID
        // because that's the only addressing scheme the new server
        // understands; legacy slug-style refs no longer resolve.
        let url = format!("https://pathbase.dev/anon/pathstash/paths/{UUID}");
        let (_, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(
            ref_,
            PathRef {
                owner: "anon".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_rejects_non_uuid_trailing_segment() {
        // Pathbase 1.1+ addresses graphs by UUID; a slug-style ref
        // can no longer be resolved, so fail at the parse step.
        assert!(parse_pathbase_ref("alex/pathstash/my-path", None).is_err());
        assert!(parse_pathbase_ref("https://pathbase.dev/alex/pathstash/my-path", None).is_err());
    }

    #[test]
    fn parse_pathbase_ref_rejects_too_few_segments() {
        assert!(parse_pathbase_ref("https://pathbase.dev/just-one", None).is_err());
        assert!(parse_pathbase_ref("just/two", None).is_err());
    }

    #[test]
    #[cfg(not(target_os = "emscripten"))]
    fn pathbase_fetch_to_doc_url_input() {
        use crate::cmd_pathbase::tests::MockServer;
        let body = r#"{"graph":{"id":"g1"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[{"step":{"id":"s1","actor":"agent:claude-code","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#;
        let server = MockServer::start("HTTP/1.1 200 OK", body);
        let url = format!("{}/u/alex/repos/pathstash/graphs/{UUID}", server.base());

        let cfg_dir = tempfile::tempdir().unwrap();
        let config = Config {
            toolpath_config_dir: Some(cfg_dir.path().to_path_buf()),
            ..Config::default()
        };
        let derived = pathbase_fetch_to_doc(&config, &url, None).unwrap();

        assert_eq!(derived.cache_id, format!("pathbase-alex-pathstash-{UUID}"));
        assert!(derived.doc.into_single_path().is_some());
    }
}
