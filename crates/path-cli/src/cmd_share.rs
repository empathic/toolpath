//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use std::path::PathBuf;

use crate::artifact::{ArtifactRef, ArtifactType};
use crate::cmd_export::RepoSpec;
use crate::harness::{
    Harness, HarnessBundle, is_not_found_copilot, is_not_found_cursor, is_not_found_gemini,
    is_not_found_pi,
};
use crate::listing_cache::{CachedListing, CachedRow, ListingCache, ProviderListings};
use crate::sync::sources::{ArtifactSource, claude_source, codex_source, opencode_source};

#[derive(Args, Debug)]
pub struct ShareArgs {
    /// Pathbase server URL (defaults to the stored session's server)
    #[arg(long)]
    pub url: Option<String>,

    /// Force the anonymous endpoint, ignoring any stored credentials
    #[arg(long, conflicts_with_all = ["repo", "public"])]
    pub anon: bool,

    /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
    #[arg(long, value_parser = crate::cmd_export::parse_repo_spec)]
    pub repo: Option<RepoSpec>,

    /// Human-readable display label for the uploaded graph
    /// (defaults to the toolpath document id). Free-form; not used
    /// in the URL — graphs are addressed by UUID server-side.
    #[arg(long, alias = "slug")]
    pub name: Option<String>,

    /// Mark the uploaded graph public (default: unlisted, addressable only by UUID)
    #[arg(long)]
    pub public: bool,

    /// Narrow the picker to one harness, or skip the picker entirely
    /// when used with --session.
    #[arg(long, value_enum)]
    pub harness: Option<Harness>,

    /// Skip the picker. Requires --harness; requires --project for
    /// claude/gemini/pi.
    #[arg(long, requires = "harness")]
    pub session: Option<String>,

    /// Override cwd-as-project. Filters the picker to sessions tied to
    /// this project across all harnesses.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Skip writing the cache; derive in-memory only
    #[arg(long)]
    pub no_cache: bool,
}

/// One artifact surfaced by a provider — today always an agent session.
/// Rows feed both the unified `share` picker and `p cache sync`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArtifactRow {
    pub(crate) artifact_type: ArtifactType,
    /// Project path for keyed providers; `None` for codex/opencode.
    pub(crate) path: Option<String>,
    /// Recorded cwd from the session (codex/opencode only).
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) last_activity: Option<DateTime<Utc>>,
    /// Message count — populated only for harness artifact types
    /// (agent sessions); `None` for future non-session artifact kinds.
    pub(crate) message_count: Option<usize>,
    pub(crate) matches_cwd: bool,
}

/// Aggregate sessions across the harnesses in `bundle`, ranked so that
/// rows whose project (or recorded cwd) canonicalizes to `cwd` come
/// first, sorted by descending `last_activity`.
///
/// Filters: `harness_filter` keeps only rows from one harness; `project_filter`
/// keeps only rows whose project (for keyed) or cwd (for session-keyed)
/// canonicalizes to that path.
pub(crate) fn gather_artifacts(
    bundle: &HarnessBundle,
    cwd: &std::path::Path,
    harness_filter: Option<ArtifactType>,
    project_filter: Option<&std::path::Path>,
) -> Vec<ArtifactRow> {
    let canonical_cwd = canonicalize_or_self(cwd);
    let canonical_project = project_filter.map(canonicalize_or_self);

    let want = |h: ArtifactType| harness_filter.is_none_or(|f| f == h);

    // The listing cache: rows for artifacts whose stat stamp is
    // unchanged since the last gather are rebuilt from cached fields
    // instead of re-scanned. Loaded once here; the cache-backed
    // collectors (claude/codex/opencode — the expensive scans) each
    // take their section and return a refreshed one, written back
    // after the fan-out only if something actually changed.
    let mut listing_cache = ListingCache::load();
    let claude_cache = listing_cache.section(ArtifactType::Claude);
    let codex_cache = listing_cache.section(ArtifactType::Codex);
    let opencode_cache = listing_cache.section(ArtifactType::Opencode);

    // Enumerate providers concurrently: each is an independent
    // read-only scan of its own on-disk tree, and the slowest (a big
    // codex or claude history) otherwise serializes behind the rest.
    // Wall time becomes max-of-providers instead of sum. Claude is the
    // one provider that can't cross threads (`ClaudeConvo` caches its
    // chain index in a `RefCell`), so it scans inline on this thread
    // while the rest run in scoped threads. Concatenation happens in
    // the old sequential provider order, so the stable sort's
    // tie-breaking matches the previous behavior exactly.
    let mut rows = Vec::new();
    let mut refreshed: Vec<(ArtifactType, ProviderListings)> = Vec::new();
    let cwd_ref = &canonical_cwd;
    let project_ref = canonical_project.as_deref();
    type CollectOutput = (Vec<ArtifactRow>, Option<(ArtifactType, ProviderListings)>);
    std::thread::scope(|s| {
        let mut handles: Vec<std::thread::ScopedJoinHandle<'_, CollectOutput>> = Vec::new();

        macro_rules! spawn_collect {
            ($ty:expr, $mgr:expr, $collect:ident) => {
                if want($ty)
                    && let Some(mgr) = $mgr
                {
                    handles.push(s.spawn(move || {
                        let mut out = Vec::new();
                        $collect(mgr, cwd_ref, project_ref, &mut out);
                        (out, None)
                    }));
                }
            };
        }

        macro_rules! spawn_collect_cached {
            ($ty:expr, $mgr:expr, $collect:ident, $cache:expr) => {
                if want($ty)
                    && let Some(mgr) = $mgr
                {
                    let cache = $cache;
                    handles.push(s.spawn(move || {
                        let mut out = Vec::new();
                        let fresh = $collect(mgr, cwd_ref, project_ref, cache, &mut out);
                        (out, Some(($ty, fresh)))
                    }));
                }
            };
        }

        spawn_collect!(ArtifactType::Gemini, &bundle.gemini, collect_gemini);
        spawn_collect!(ArtifactType::Pi, &bundle.pi, collect_pi);
        spawn_collect_cached!(
            ArtifactType::Codex,
            &bundle.codex,
            collect_codex,
            &codex_cache
        );
        spawn_collect!(ArtifactType::Copilot, &bundle.copilot, collect_copilot);
        spawn_collect_cached!(
            ArtifactType::Opencode,
            &bundle.opencode,
            collect_opencode,
            &opencode_cache
        );
        spawn_collect!(ArtifactType::Cursor, &bundle.cursor, collect_cursor);

        if want(ArtifactType::Claude)
            && let Some(mgr) = &bundle.claude
        {
            let fresh = collect_claude(mgr, cwd_ref, project_ref, &claude_cache, &mut rows);
            refreshed.push((ArtifactType::Claude, fresh));
        }

        for handle in handles {
            match handle.join() {
                Ok((out, section)) => {
                    rows.extend(out);
                    if let Some(section) = section {
                        refreshed.push(section);
                    }
                }
                // A panicking collector degrades to "that provider is
                // missing from the picker", matching how collector-level
                // errors already warn-and-continue. Its cache section is
                // left untouched.
                Err(_) => eprintln!("warning: a session collector panicked; its rows are skipped"),
            }
        }
    });

    for (artifact_type, fresh) in refreshed {
        listing_cache.replace_section(artifact_type, fresh);
    }
    listing_cache.save_if_dirty();

    rows.sort_by(|a, b| {
        b.matches_cwd
            .cmp(&a.matches_cwd)
            .then_with(|| b.last_activity.cmp(&a.last_activity))
    });
    rows
}

fn canonicalize_or_self(p: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn paths_match(a: &std::path::Path, b: &std::path::Path) -> bool {
    canonicalize_or_self(a) == canonicalize_or_self(b)
}

// ── stat-stamp listing cache plumbing ──────────────────────────────
//
// The expensive providers (claude/codex/opencode) collect through
// `collect_with_cache`: enumerate stat-level `ArtifactRef`s via the
// same `ArtifactSource` machinery sync uses, rebuild rows from the
// listing cache for every artifact whose stamp still matches, and run
// the real metadata scan only for what is new or changed. See
// `docs/superpowers/specs/2026-08-04-listing-cache-design.md`.

/// How a provider's fresh scan orders its rows — reproduced on
/// cache-backed gathers so a warm gather's output is field-for-field
/// identical to a cold one, ties included.
enum ListingOrder {
    /// claude (and the other project-keyed providers): projects in
    /// enumeration order, sessions within each project by descending
    /// last activity.
    ByActivityWithinPathRuns,
    /// codex/opencode: descending last activity across the provider.
    ByActivity,
}

/// Stable-sort `rows[start..]` by descending last activity — the same
/// ordering (`sort_by_key(Reverse(last_activity))`, `None` last) every
/// provider's fresh listing applies.
fn sort_rows_by_activity(rows: &mut [ArtifactRow], start: usize) {
    rows[start..].sort_by_key(|r| std::cmp::Reverse(r.last_activity));
}

/// Rebuild a picker row from cached fields. `matches_cwd` is
/// deliberately not cached — it depends on the caller's cwd — so it is
/// recomputed here from the cached `path`/`cwd` with the same
/// canonicalized matching the fresh scans use.
fn row_from_cached(
    artifact_type: ArtifactType,
    cached: &CachedRow,
    canonical_cwd: &std::path::Path,
) -> ArtifactRow {
    let key = cached.path.as_deref().or(cached.cwd.as_deref());
    let matches_cwd = key.is_some_and(|k| paths_match(std::path::Path::new(k), canonical_cwd));
    ArtifactRow {
        artifact_type,
        path: cached.path.clone(),
        cwd: cached.cwd.clone(),
        session_id: cached.session_id.clone(),
        title: cached.title.clone(),
        last_activity: cached.last_activity,
        message_count: cached.message_count,
        matches_cwd,
    }
}

/// The cacheable complement of [`row_from_cached`].
fn cached_from_row(row: &ArtifactRow) -> CachedRow {
    CachedRow {
        path: row.path.clone(),
        cwd: row.cwd.clone(),
        session_id: row.session_id.clone(),
        title: row.title.clone(),
        last_activity: row.last_activity,
        message_count: row.message_count,
    }
}

/// Whether `row` survives the `--project` filter: its project (keyed
/// providers) or recorded cwd canonicalizes to the filter path. Rows
/// with neither are dropped under a filter, exactly like the fresh
/// scans. Applied after cache reconstruction — the cache itself is
/// filter-agnostic.
fn row_passes_project_filter(row: &ArtifactRow, project_filter: Option<&std::path::Path>) -> bool {
    let Some(filter) = project_filter else {
        return true;
    };
    let key = row.path.as_deref().or(row.cwd.as_deref());
    key.is_some_and(|k| paths_match(std::path::Path::new(k), filter))
}

/// The shared cache-backed collection loop: walk the enumerated refs
/// in listing order, rebuild rows from the cache on a stamp hit, call
/// `scan_miss` otherwise (a `None` means the scan failed — warned by
/// the provider closure — and the artifact is neither listed nor
/// cached, so the next gather retries it). Returns the refreshed
/// section, which contains exactly the enumerated artifacts —
/// anything that vanished upstream drops out, matching sync's
/// self-heal semantics.
#[allow(clippy::too_many_arguments)]
fn collect_with_cache(
    artifact_type: ArtifactType,
    refs: &[ArtifactRef],
    cache: &ProviderListings,
    order: ListingOrder,
    mut scan_miss: impl FnMut(&ArtifactRef) -> Option<ArtifactRow>,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) -> ProviderListings {
    let mut fresh = ProviderListings::new();
    let mut rows: Vec<ArtifactRow> = Vec::with_capacity(refs.len());
    let mut run_start = 0usize;
    let mut prev_path: Option<&Option<String>> = None;
    for r in refs {
        // Project-keyed enumerations are project-major; close each
        // run with the within-project activity sort the fresh scan
        // applies per project.
        if matches!(order, ListingOrder::ByActivityWithinPathRuns)
            && prev_path.is_some_and(|p| p != &r.path)
        {
            sort_rows_by_activity(&mut rows, run_start);
            run_start = rows.len();
        }
        prev_path = Some(&r.path);
        let row = match cache.get(&r.id) {
            Some(entry) if entry.matches(r) => {
                Some(row_from_cached(artifact_type, &entry.row, canonical_cwd))
            }
            _ => scan_miss(r),
        };
        if let Some(row) = row {
            fresh.insert(
                r.id.clone(),
                CachedListing {
                    modified: r.modified,
                    size: r.size,
                    row: cached_from_row(&row),
                },
            );
            rows.push(row);
        }
    }
    // Final run — for `ByActivity` this is the whole provider.
    sort_rows_by_activity(&mut rows, run_start);
    out.extend(
        rows.into_iter()
            .filter(|r| row_passes_project_filter(r, project_filter)),
    );
    fresh
}

fn collect_claude(
    mgr: &toolpath_claude::ClaudeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    cache: &ProviderListings,
    out: &mut Vec<ArtifactRow>,
) -> ProviderListings {
    // Chain heads with whole-chain stamps (`claude_chain_stamp`): an
    // append to any segment of a chain invalidates the head's entry.
    let refs = claude_source(mgr).enumerate();
    collect_with_cache(
        ArtifactType::Claude,
        &refs,
        cache,
        ListingOrder::ByActivityWithinPathRuns,
        |r| {
            let project = r.path.as_deref()?;
            match mgr.read_conversation_metadata(project, &r.id) {
                Ok(m) => Some(claude_row(m, canonical_cwd)),
                Err(e) => {
                    eprintln!("Warning: Failed to read metadata for {}: {e}", r.id);
                    None
                }
            }
        },
        canonical_cwd,
        project_filter,
        out,
    )
}

fn claude_row(
    m: toolpath_claude::ConversationMetadata,
    canonical_cwd: &std::path::Path,
) -> ArtifactRow {
    let matches_cwd = paths_match(std::path::Path::new(&m.project_path), canonical_cwd);
    ArtifactRow {
        artifact_type: ArtifactType::Claude,
        path: Some(m.project_path),
        cwd: None,
        session_id: m.session_id,
        title: m
            .first_user_message
            .unwrap_or_else(|| "(no prompt)".to_string()),
        last_activity: m.last_activity,
        message_count: Some(m.message_count),
        matches_cwd,
    }
}

fn collect_gemini(
    mgr: &toolpath_gemini::GeminiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_gemini(&e) => return,
        Err(e) => {
            eprintln!("warning: gemini aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: gemini project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            out.push(ArtifactRow {
                artifact_type: ArtifactType::Gemini,
                path: Some(m.project_path),
                cwd: None,
                session_id: m.session_uuid,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: Some(m.message_count),
                matches_cwd,
            });
        }
    }
}

fn collect_pi(
    mgr: &toolpath_pi::PiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_pi(&e) => return,
        Err(e) => {
            eprintln!("warning: pi aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_sessions(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: pi project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            // Pi's SessionMeta.timestamp is the session *start*, so it
            // never moves as the session grows; prefer the file's mtime
            // as the change-detecting last_activity, falling back to
            // the header timestamp when the stat fails.
            let last_activity = std::fs::metadata(&m.file_path)
                .and_then(|md| md.modified())
                .ok()
                .map(DateTime::<Utc>::from)
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                        .ok()
                        .map(|d| d.with_timezone(&Utc))
                });
            out.push(ArtifactRow {
                artifact_type: ArtifactType::Pi,
                path: Some(project.clone()),
                cwd: None,
                session_id: m.id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity,
                message_count: Some(m.entry_count),
                matches_cwd,
            });
        }
    }
}

fn collect_codex(
    mgr: &toolpath_codex::CodexConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    cache: &ProviderListings,
    out: &mut Vec<ArtifactRow>,
) -> ProviderListings {
    let refs = codex_source(mgr).enumerate();
    // One lazy directory walk maps session id → rollout path for the
    // misses; a per-miss `find_rollout_file` would re-walk the whole
    // date-bucketed tree every time.
    let mut files: Option<std::collections::HashMap<String, PathBuf>> = None;
    collect_with_cache(
        ArtifactType::Codex,
        &refs,
        cache,
        ListingOrder::ByActivity,
        |r| {
            let files = files.get_or_insert_with(|| {
                mgr.io()
                    .list_rollout_files()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| {
                        let stem = p.file_stem()?.to_str()?;
                        Some((toolpath_codex::session_id_from_stem(stem).to_string(), p))
                    })
                    .collect()
            });
            let file = files.get(&r.id)?;
            match mgr.io().read_metadata(file) {
                Ok(m) => Some(codex_row(m, canonical_cwd)),
                Err(e) => {
                    eprintln!("Warning: failed to read {}: {e}", file.display());
                    None
                }
            }
        },
        canonical_cwd,
        project_filter,
        out,
    )
}

fn codex_row(m: toolpath_codex::SessionMetadata, canonical_cwd: &std::path::Path) -> ArtifactRow {
    let cwd_str = m.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
    let matches_cwd = m
        .cwd
        .as_deref()
        .map(|p| paths_match(p, canonical_cwd))
        .unwrap_or(false);
    ArtifactRow {
        artifact_type: ArtifactType::Codex,
        path: None,
        cwd: cwd_str,
        session_id: m.id,
        title: m
            .first_user_message
            .unwrap_or_else(|| "(no prompt)".to_string()),
        last_activity: m.last_activity,
        message_count: Some(m.line_count),
        matches_cwd,
    }
}

fn collect_copilot(
    mgr: &toolpath_copilot::CopilotConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let metas = match mgr.list_sessions() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_copilot(&e) => return,
        Err(e) => {
            eprintln!("warning: copilot aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        // Copilot stores cwd as a String (from session.start `context.cwd`).
        let stored = m.cwd.as_deref().map(std::path::PathBuf::from);
        if let Some(filter) = project_filter {
            match &stored {
                Some(p) if paths_match(p, filter) => {}
                _ => continue,
            }
        }
        let matches_cwd = stored
            .as_deref()
            .map(|p| paths_match(p, canonical_cwd))
            .unwrap_or(false);
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Copilot,
            path: None,
            cwd: m.cwd,
            session_id: m.id,
            title: m
                .first_user_message
                .unwrap_or_else(|| "(no prompt)".to_string()),
            last_activity: m.last_activity,
            message_count: Some(m.line_count),
            matches_cwd,
        });
    }
}

fn collect_opencode(
    mgr: &toolpath_opencode::OpencodeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    cache: &ProviderListings,
    out: &mut Vec<ArtifactRow>,
) -> ProviderListings {
    let refs = opencode_source(mgr).enumerate();
    // opencode metadata is one DB pass over every session (it loads
    // each session's messages), so the first miss triggers the full
    // scan once and later misses read from it. A fully-warm gather
    // never opens the message tables at all.
    let mut metas: Option<std::collections::HashMap<String, ArtifactRow>> = None;
    collect_with_cache(
        ArtifactType::Opencode,
        &refs,
        cache,
        ListingOrder::ByActivity,
        |r| {
            let metas = metas.get_or_insert_with(|| match mgr.io().list_session_metadata(None) {
                Ok(ms) => ms
                    .into_iter()
                    .map(|m| (m.id.clone(), opencode_row(m, canonical_cwd)))
                    .collect(),
                Err(e) => {
                    eprintln!("warning: opencode aggregation failed: {e}");
                    std::collections::HashMap::new()
                }
            });
            metas.get(&r.id).cloned()
        },
        canonical_cwd,
        project_filter,
        out,
    )
}

fn opencode_row(
    m: toolpath_opencode::SessionMetadata,
    canonical_cwd: &std::path::Path,
) -> ArtifactRow {
    let matches_cwd = paths_match(&m.directory, canonical_cwd);
    let cwd_str = m.directory.to_string_lossy().into_owned();
    let title = match (&m.first_user_message, m.title.is_empty()) {
        (Some(s), _) if !s.is_empty() => s.clone(),
        (_, false) => m.title.clone(),
        _ => "(no prompt)".to_string(),
    };
    ArtifactRow {
        artifact_type: ArtifactType::Opencode,
        path: None,
        cwd: Some(cwd_str),
        session_id: m.id,
        title,
        last_activity: m.last_activity,
        message_count: Some(m.message_count),
        matches_cwd,
    }
}

fn collect_cursor(
    mgr: &toolpath_cursor::CursorConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let metas = match mgr.io().list_session_metadata() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_cursor(&e) => return,
        Err(e) => {
            eprintln!("warning: cursor aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        // Cursor stores each composer's workspace as the absolute
        // path of the folder Cursor.app was open on. Sessions
        // without a workspace (numeric/remote workspace ids) are
        // dropped from the picker — we can't tell what they're
        // tied to.
        let Some(workspace) = m.workspace_path.as_ref() else {
            continue;
        };
        if let Some(filter) = project_filter
            && !paths_match(workspace, filter)
        {
            continue;
        }
        let matches_cwd = paths_match(workspace, canonical_cwd);
        let cwd_str = workspace.to_string_lossy().into_owned();
        let title = match (&m.first_user_message, &m.name) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, Some(n)) if !n.is_empty() => n.clone(),
            _ => "(no prompt)".to_string(),
        };
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Cursor,
            path: None,
            cwd: Some(cwd_str),
            session_id: m.id,
            title,
            last_activity: m.last_activity,
            message_count: Some(m.message_count),
            matches_cwd,
        });
    }
}

pub fn run(args: ShareArgs) -> Result<()> {
    let harness = args.harness.map(|h| h.artifact_type());

    if args.session.is_some() && harness.is_none() {
        anyhow::bail!("--session requires --harness");
    }

    // Build upload args + base URL once and reuse for both the explicit
    // path and the picker path. `needs_auth` decides whether preflight
    // can fall back to anon on credential failure.
    let upload_args = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
    };
    let base_url = crate::cmd_export::resolve_upload_base_url(&upload_args);
    let needs_auth = upload_args.repo.is_some() || upload_args.public || upload_args.name.is_some();

    if let (Some(h), Some(session)) = (harness, &args.session) {
        // Explicit-args: validate creds before derive so a credential
        // failure doesn't waste the derive/cache work.
        let auth = crate::cmd_pathbase::preflight_auth(&base_url, upload_args.anon, needs_auth)?;
        return share_explicit(h, session.as_str(), &args, auth, base_url);
    }

    let cwd = std::env::current_dir()?;
    let bundle = HarnessBundle::from_environment();
    let project_filter = args.project.as_deref();
    let rows = gather_artifacts(&bundle, &cwd, harness, project_filter);

    if rows.is_empty() {
        return bail_no_sessions(&bundle, project_filter);
    }

    if !crate::fuzzy::available() {
        eprintln!(
            "Interactive `path share` needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path import <harness>      # writes a cache entry, prints its id\n  \
             path export pathbase --input <id>"
        );
        anyhow::bail!("fzf unavailable; run `path import <harness>` then `path export pathbase`");
    }

    // We have rows AND fzf available — now validate credentials before
    // making the user pick a session. If preflight returns Anon (either
    // explicit --anon, no creds + no auth flags, or auth probe failed
    // and fell back), the picker still fires with that knowledge baked in.
    let auth = crate::cmd_pathbase::preflight_auth(&base_url, upload_args.anon, needs_auth)?;

    let lines: Vec<String> = rows.iter().map(format_picker_row).collect();
    let header = format!("share an agent session (Enter = upload to {base_url})");
    let opts = crate::fuzzy::PickOptions {
        with_nth: "4",
        prompt: "share> ",
        preview: Some("{exe} show --ansi {1} --project {2} --session {3}"),
        // Stacked layout: preview above the list, list below. Fits narrow
        // terminals better than the default side-by-side and gives the
        // session preview the full terminal width to render `path show`.
        preview_window: "up:60%:wrap-word",
        header: Some(&header),
        tiebreak: "index",
        multi: false,
    };
    let line = match crate::fuzzy::pick(&lines, &opts)? {
        crate::fuzzy::PickResult::Selected(v) => match v.into_iter().next() {
            Some(l) => l,
            // Selected with an empty payload should not happen (fzf exits 0
            // only when at least one row was confirmed), but treat it like
            // no-match for safety.
            None => return Ok(()),
        },
        // No row matched the query — exit 0, same as today, no extra noise.
        crate::fuzzy::PickResult::NoMatch => return Ok(()),
        // Esc / Ctrl-C: deliberate user cancel. Signal to the shell with
        // exit 130 so it's distinguishable from a successful share.
        crate::fuzzy::PickResult::Cancelled => std::process::exit(130),
    };
    let (h, key, session, title) = parse_picker_row(&line)
        .ok_or_else(|| anyhow::anyhow!("internal: failed to parse picker row"))?;

    let explicit = ShareArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
        harness: h.harness(),
        session: None, // unused by share_explicit
        project: if h.path_keyed() {
            Some(PathBuf::from(&key))
        } else {
            None
        },
        no_cache: args.no_cache,
    };
    // Show the conversation title in the confirmation line; the session id
    // is opaque and doesn't help the user verify they picked the right
    // thing. `{:?}` adds the surrounding quotes per the spec.
    eprintln!("Picked {} session {:?}", h.name(), title);
    share_explicit(h, &session, &explicit, auth, base_url)
}

fn bail_no_sessions(
    bundle: &HarnessBundle,
    project_filter: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(p) = project_filter {
        anyhow::bail!(
            "No agent sessions found in project {}. Run without --project to see sessions across all projects.",
            p.display()
        );
    }

    let mut summary = String::from("No agent sessions found.\n");
    // Pad harness names so the path column lines up: "opencode:" is the
    // longest at 9 chars (8 + colon).
    let home = home_dir();
    summary.push_str(&format_status_line(
        "claude",
        &harness_status_claude(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "gemini",
        &harness_status_gemini(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "codex",
        &harness_status_codex(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "copilot",
        &harness_status_copilot(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "opencode",
        &harness_status_opencode(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "cursor",
        &harness_status_cursor(bundle, home.as_deref()),
    ));
    summary.push_str(&format_status_line(
        "pi",
        &harness_status_pi(bundle, home.as_deref()),
    ));
    eprint!("{summary}");
    anyhow::bail!("no shareable sessions");
}

/// Cross-platform `$HOME` lookup matching the providers' internal helpers.
/// Returns `None` only when neither `$HOME` nor `$USERPROFILE` is set.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// Human-readable status of a harness's on-disk store: either the (possibly
/// home-relative) path with a "(0 sessions)" hint, or the path with a
/// "not found" hint when the directory/database is absent.
#[derive(Debug, PartialEq, Eq)]
struct HarnessStatus {
    /// Display path (tilde-prefixed when under `$HOME`).
    path: String,
    /// True when the path exists on disk.
    exists: bool,
}

impl HarnessStatus {
    fn render(&self) -> String {
        if self.exists {
            format!("{} (0 sessions)", self.path)
        } else {
            format!("{} not found", self.path)
        }
    }

    /// Status when the resolver itself failed (e.g. no $HOME).
    fn unresolved() -> Self {
        Self {
            path: "<no home directory>".to_string(),
            exists: false,
        }
    }
}

/// Format a single status line, padding the harness name so that the path
/// column lines up across all five rows. The longest name is "opencode" (8).
fn format_status_line(name: &str, status: &HarnessStatus) -> String {
    format!("  {:<9} {}\n", format!("{name}:"), status.render())
}

fn harness_status_claude(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.claude else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().projects_dir() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_gemini(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.gemini else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().tmp_dir() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_codex(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.codex else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().sessions_root() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_copilot(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.copilot else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().session_state_dir() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_opencode(
    bundle: &HarnessBundle,
    home: Option<&std::path::Path>,
) -> HarnessStatus {
    let Some(mgr) = &bundle.opencode else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().db_path() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_pi(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.pi else {
        return HarnessStatus::unresolved();
    };
    let p = mgr.resolver().sessions_dir().to_path_buf();
    HarnessStatus {
        path: home_relative(&p, home),
        exists: p.exists(),
    }
}

fn harness_status_cursor(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.cursor else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().db_path() {
        Ok(p) => HarnessStatus {
            path: home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

/// Display `path` as `~/relative/part` when it's under `home`, otherwise
/// return its absolute lossy form. Pure helper — does no filesystem I/O.
fn home_relative(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
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

fn share_explicit(
    harness: ArtifactType,
    session: &str,
    args: &ShareArgs,
    auth: crate::cmd_pathbase::AuthMode,
    base_url: String,
) -> Result<()> {
    let project = match (harness.path_keyed(), args.project.as_ref()) {
        (true, Some(p)) => Some(p.to_string_lossy().into_owned()),
        (true, None) => anyhow::bail!(
            "--project required when --harness is {} and --session is set",
            harness.name()
        ),
        (false, _) => None,
    };

    // Fast path: when the manifest shows this exact source state is
    // already in the cache, upload the cached doc instead of re-deriving
    // — a derive would reproduce it byte-for-byte anyway.
    if !args.no_cache
        && let Some(cache_id) = crate::sync::fresh_cache_id(
            &HarnessBundle::from_environment(),
            harness,
            project.as_deref(),
            session,
        )
    {
        let doc_path = crate::cache::cache_path(&cache_id)?;
        let body = std::fs::read_to_string(&doc_path)
            .with_context(|| format!("Failed to read {}", doc_path.display()))?;
        eprintln!(
            "Cache is current for {} session {cache_id}; uploading without re-deriving",
            harness.name()
        );
        let summary = format!("{} session {}", harness.name(), cache_id);
        let upload = crate::cmd_export::PathbaseUploadArgs {
            url: args.url.clone(),
            anon: args.anon,
            repo: args.repo.clone(),
            name: args.name.clone(),
            public: args.public,
        };
        return crate::cmd_export::run_pathbase_inner(auth, base_url, upload, &body, &summary);
    }

    let derived = derive_session(harness, project.as_deref(), session)?;
    let summary = format!("{} session {}", harness.name(), derived.cache_id);

    if !args.no_cache {
        // The cache entry should always reflect what was just uploaded.
        // `path share` is "ship the current state of this session"; if
        // the conversation has grown since a prior share, the in-memory
        // body has the new turns but a stale cache file would not — and
        // the upload uses the fresh body, not the cache. Always
        // overwrite so cache and upload agree (use `--no-cache` to skip
        // the cache write entirely).
        let path = crate::cache::write_cached(&derived.cache_id, &derived.doc, true)?;
        if let Some(stub) = &derived.provenance
            && let Err(e) = crate::sync::record_artifact(stub, &derived.cache_id)
        {
            eprintln!("warning: sync manifest not updated: {e}");
        }
        eprintln!(
            "Cached {} session → {} ({})",
            harness.name(),
            derived.cache_id,
            path.display()
        );
    }

    let body = derived.doc.to_json()?;
    let upload = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
    };
    crate::cmd_export::run_pathbase_inner(auth, base_url, upload, &body, &summary)
}

/// Build the TSV line fed to the picker. Three hidden parser-only
/// columns lead the row (harness key, project/cwd, session id); a
/// fourth column carries the pre-formatted display string from
/// `fuzzy::render_row`; a fifth carries the raw title so
/// `parse_picker_row` can recover it without reparsing the display.
///
/// The display column is space-padded rather than tab-separated so the
/// columns line up consistently across pickers — terminal tab stops
/// produce ugly variable gaps in both fzf and skim.
fn format_picker_row(row: &ArtifactRow) -> String {
    let key = row
        .path
        .clone()
        .or_else(|| row.cwd.clone())
        .unwrap_or_default();
    let scope = if row.matches_cwd { "·" } else { " " };
    let leading = format!("{scope} {}", row.artifact_type.padded_name());
    let display = render_row(
        Some(&leading),
        row.last_activity,
        &row.message_count
            .map(|c| count(c, "msgs"))
            .unwrap_or_default(),
        Some(&project_short(&key)),
        &row.title,
    );
    let title = clean_for_picker_display(&row.title);
    format!(
        "{}\t{}\t{}\t{}\t{}",
        row.artifact_type.name(),
        tab_safe(&key),
        tab_safe(&row.session_id),
        display,
        tab_safe(&title),
    )
}

/// Inverse of [`format_picker_row`] — pulls (harness, key, session,
/// title) back out of the line the picker returned. Returns `None` if
/// the line is malformed.
fn parse_picker_row(line: &str) -> Option<(ArtifactType, String, String, String)> {
    let mut parts = line.split('\t');
    let h = ArtifactType::parse(parts.next()?)?;
    let key = parts.next()?.to_string();
    let session = parts.next()?.to_string();
    if session.is_empty() {
        return None;
    }
    // Skip the pre-formatted display column (col 4) to reach the raw
    // title at col 5.
    let title = parts.nth(1).unwrap_or("").to_string();
    Some((h, key, session, title))
}

use crate::fuzzy::{clean_for_picker_display, count, project_short, render_row, tab_safe};

fn derive_session(
    harness: ArtifactType,
    project: Option<&str>,
    session: &str,
) -> Result<crate::derive::DerivedDoc> {
    match harness {
        ArtifactType::Claude => {
            crate::derive::derive_claude_session(project.expect("path_keyed"), session)
        }
        ArtifactType::Gemini => {
            crate::derive::derive_gemini_session(project.expect("path_keyed"), session)
        }
        ArtifactType::Copilot => crate::derive::derive_copilot_session(session),
        ArtifactType::Pi => {
            crate::derive::derive_pi_session(project.expect("path_keyed"), session, None)
        }
        ArtifactType::Codex => crate::derive::derive_codex_session(session),
        ArtifactType::Opencode => crate::derive::derive_opencode_session(session, false),
        ArtifactType::Cursor => crate::derive::derive_cursor_session(session),
        ArtifactType::Git => {
            anyhow::bail!("share only handles agent sessions; git artifacts go through `p import`")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use tempfile::TempDir;

    /// Pin `$TOOLPATH_CONFIG_DIR` to a tempdir for the guard's
    /// lifetime, so gathers read and write a scratch listing cache
    /// instead of the developer's real `~/.toolpath`. Holds the shared
    /// env lock to serialize with other env-mutating tests.
    struct ScopedConfigDir {
        temp: TempDir,
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    fn scoped_config_dir() -> ScopedConfigDir {
        let lock = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = TempDir::new().unwrap();
        let prev = std::env::var_os(crate::config::CONFIG_DIR_ENV);
        unsafe {
            std::env::set_var(crate::config::CONFIG_DIR_ENV, temp.path().join(".toolpath"));
        }
        ScopedConfigDir {
            temp,
            prev,
            _lock: lock,
        }
    }

    impl ScopedConfigDir {
        /// The tempdir root, for provider fixtures that should live
        /// and die with the pinned config dir.
        fn root(&self) -> &Path {
            self.temp.path()
        }

        fn listing_cache_file(&self) -> std::path::PathBuf {
            self.temp
                .path()
                .join(".toolpath")
                .join(crate::config::LISTING_CACHE_FILE_NAME)
        }
    }

    impl Drop for ScopedConfigDir {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(crate::config::CONFIG_DIR_ENV, v),
                    None => std::env::remove_var(crate::config::CONFIG_DIR_ENV),
                }
            }
        }
    }

    fn write_claude_session(claude_dir: &Path, project_slug: &str, session: &str, prompt: &str) {
        write_claude_session_at(claude_dir, project_slug, session, prompt, "2024-01-02");
    }

    /// Like [`write_claude_session`] but with a caller-chosen day, so
    /// sibling fixtures get distinct `last_activity` values. (Rows with
    /// identical timestamps tie-break on claude's chain-head
    /// enumeration order, which is not stable run to run — true before
    /// the listing cache too.)
    fn write_claude_session_at(
        claude_dir: &Path,
        project_slug: &str,
        session: &str,
        prompt: &str,
        day: &str,
    ) {
        let project_dir = claude_dir.join("projects").join(project_slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let user = format!(
            r#"{{"type":"user","uuid":"u-{session}","timestamp":"{day}T00:00:00Z","cwd":"/test/project","message":{{"role":"user","content":"{prompt}"}}}}"#
        );
        let asst = format!(
            r#"{{"type":"assistant","uuid":"a-{session}","timestamp":"{day}T00:00:01Z","message":{{"role":"assistant","content":"hi"}}}}"#
        );
        std::fs::write(
            project_dir.join(format!("{session}.jsonl")),
            format!("{user}\n{asst}\n"),
        )
        .unwrap();
    }

    fn claude_only_bundle(home: &Path) -> HarnessBundle {
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    #[test]
    fn gather_artifacts_includes_claude_rows_for_a_project() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_artifacts(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Claude);
        assert_eq!(rows[0].session_id, "abc-session-one");
        assert_eq!(rows[0].path.as_deref(), Some("/test/project"));
        assert!(rows[0].matches_cwd, "cwd should match the project path");
    }

    #[test]
    fn gather_artifacts_marks_non_matching_project_rows() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/some/other/place");
        let rows = gather_artifacts(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].matches_cwd);
    }

    #[test]
    fn gather_artifacts_skips_harness_with_no_home_dir() {
        let _cfg = scoped_config_dir();
        // Empty bundle => no rows, no panic.
        let bundle = HarnessBundle::default();
        let rows = gather_artifacts(&bundle, Path::new("/anywhere"), None, None);
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_artifacts_filters_by_harness() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "hi",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_artifacts(&bundle, cwd, Some(ArtifactType::Codex), None);
        assert!(rows.is_empty(), "filter to codex must drop claude rows");
    }

    fn codex_only_bundle(home: &Path) -> HarnessBundle {
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let resolver = toolpath_codex::PathResolver::new().with_codex_dir(&codex_dir);
        HarnessBundle {
            codex: Some(toolpath_codex::CodexConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn write_codex_session(codex_dir: &Path, id: &str, cwd: &str) {
        // Date-bucketed layout: ~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl
        let dir = codex_dir.join("sessions/2026/05/07");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("rollout-2026-05-07T00-00-00-{id}.jsonl"));
        let meta = format!(
            r#"{{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-05-07T00:00:00Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"test","source":"cli","model_provider":"openai"}}}}"#
        );
        let user = r#"{"timestamp":"2026-05-07T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
        std::fs::write(file, format!("{meta}\n{user}\n")).unwrap();
    }

    #[test]
    fn gather_artifacts_includes_codex_rows_with_cwd_match() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_codex_session(
            &temp.path().join(".codex"),
            "00000000-0000-0000-0000-0000000000aa",
            "/work/proj",
        );
        let bundle = codex_only_bundle(temp.path());
        let rows = gather_artifacts(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Codex);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    fn copilot_only_bundle(home: &Path) -> HarnessBundle {
        let copilot_dir = home.join(".copilot");
        std::fs::create_dir_all(&copilot_dir).unwrap();
        let resolver = toolpath_copilot::PathResolver::new().with_copilot_dir(&copilot_dir);
        HarnessBundle {
            copilot: Some(toolpath_copilot::CopilotConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn write_copilot_session(copilot_dir: &Path, id: &str, cwd: &str) {
        // ~/.copilot/session-state/<id>/events.jsonl (cwd under session.start.context)
        let dir = copilot_dir.join("session-state").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let start = format!(
            r#"{{"type":"session.start","timestamp":"2026-07-01T00:00:00Z","data":{{"copilotVersion":"1.0.67","context":{{"cwd":"{cwd}"}}}}}}"#
        );
        let user =
            r#"{"type":"user.message","timestamp":"2026-07-01T00:00:01Z","data":{"content":"hi"}}"#;
        std::fs::write(dir.join("events.jsonl"), format!("{start}\n{user}\n")).unwrap();
    }

    #[test]
    fn gather_sessions_includes_copilot_rows_with_cwd_match() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_copilot_session(&temp.path().join(".copilot"), "sess-aa", "/work/proj");
        let bundle = copilot_only_bundle(temp.path());
        let rows = gather_artifacts(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Copilot);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_filters_to_copilot() {
        let _cfg = scoped_config_dir();
        let temp = TempDir::new().unwrap();
        write_copilot_session(&temp.path().join(".copilot"), "sess-aa", "/work/proj");
        let bundle = copilot_only_bundle(temp.path());
        // Filtering to a different harness drops the copilot row.
        let rows = gather_artifacts(
            &bundle,
            Path::new("/work/proj"),
            Some(ArtifactType::Codex),
            None,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_artifacts_ranks_cwd_matches_first() {
        let _cfg = scoped_config_dir();
        // Two claude sessions: one in cwd (older), one elsewhere (newer).
        // Despite the elsewhere row being newer, the cwd-match must come first.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        write_claude_session(&claude_dir, "-cwd-project", "in-cwd-session", "hi");
        // Bump activity on the not-in-cwd session by writing a later timestamp.
        let not_dir = claude_dir.join("projects").join("-other-project");
        std::fs::create_dir_all(&not_dir).unwrap();
        std::fs::write(
            not_dir.join("not-in-cwd-session.jsonl"),
            r#"{"type":"user","uuid":"u-x","timestamp":"2030-01-01T00:00:00Z","cwd":"/other/project","message":{"role":"user","content":"later"}}"#.to_string()
                + "\n",
        )
        .unwrap();
        let bundle = claude_only_bundle(temp.path());
        let rows = gather_artifacts(&bundle, Path::new("/cwd/project"), None, None);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "in-cwd-session");
        assert!(rows[0].matches_cwd);
        assert!(!rows[1].matches_cwd);
    }

    // ── listing cache ──────────────────────────────────────────────

    /// A bundle with both cache-backed file providers: two claude
    /// sessions in one project plus one codex rollout.
    fn claude_codex_bundle(home: &Path) -> HarnessBundle {
        let claude_dir = home.join(".claude");
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::create_dir_all(&codex_dir).unwrap();
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(
                toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir),
            )),
            codex: Some(toolpath_codex::CodexConvo::with_resolver(
                toolpath_codex::PathResolver::new().with_codex_dir(&codex_dir),
            )),
            ..Default::default()
        }
    }

    fn write_cache_fixtures(home: &Path) {
        write_claude_session(
            &home.join(".claude"),
            "-test-project",
            "sess-aaa",
            "First topic",
        );
        write_claude_session_at(
            &home.join(".claude"),
            "-test-project",
            "sess-bbb",
            "Second topic",
            "2024-01-03",
        );
        write_codex_session(
            &home.join(".codex"),
            "00000000-0000-0000-0000-0000000000aa",
            "/work/proj",
        );
    }

    fn append_line(file: &Path, line: &str) {
        let mut body = std::fs::read_to_string(file).unwrap();
        body.push_str(line);
        body.push('\n');
        std::fs::write(file, body).unwrap();
    }

    #[test]
    fn warm_gather_reproduces_cold_gather_rows() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);

        // Cold: nothing cached, everything scanned.
        let cold = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert_eq!(cold.len(), 3);
        assert!(
            cfg.listing_cache_file().exists(),
            "cold gather must write the listing cache"
        );

        // Warm, through a fresh bundle (new managers, like a new CLI
        // invocation): rows must be field-for-field identical.
        let warm = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert_eq!(warm, cold);

        // And the warm pass replaced nothing: sections carry one entry
        // per artifact.
        let cache = ListingCache::load();
        assert_eq!(cache.section(ArtifactType::Claude).len(), 2);
        assert_eq!(cache.section(ArtifactType::Codex).len(), 1);
    }

    #[test]
    fn warm_gather_reads_rows_from_the_listing_cache() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );

        // Tamper with a cached title while the source stamps stay
        // put. A stamp hit must surface the cached row verbatim —
        // proof the warm path reads the cache instead of re-scanning.
        let file = cfg.listing_cache_file();
        let json = std::fs::read_to_string(&file).unwrap();
        assert!(json.contains("First topic"));
        std::fs::write(&file, json.replace("First topic", "From the cache")).unwrap();

        let warm = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert!(
            warm.iter()
                .any(|r| r.session_id == "sess-aaa" && r.title == "From the cache"),
            "stamp-matched rows must come from the cache"
        );
    }

    #[test]
    fn appended_claude_session_invalidates_its_cached_row() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        let cold = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        let cold_row = cold.iter().find(|r| r.session_id == "sess-aaa").unwrap();
        assert_eq!(cold_row.message_count, Some(2));

        // The session continues: a later user turn bumps the file's
        // mtime and size, so the chain stamp no longer matches.
        append_line(
            &home.join(".claude/projects/-test-project/sess-aaa.jsonl"),
            r#"{"type":"user","uuid":"u-2","timestamp":"2024-01-02T00:05:00Z","cwd":"/test/project","message":{"role":"user","content":"And another thing"}}"#,
        );

        let warm = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        let row = warm.iter().find(|r| r.session_id == "sess-aaa").unwrap();
        assert_eq!(
            row.message_count,
            Some(3),
            "changed session must be re-scanned"
        );
        // The untouched sibling still matches its cold row.
        assert_eq!(
            warm.iter().find(|r| r.session_id == "sess-bbb"),
            cold.iter().find(|r| r.session_id == "sess-bbb"),
        );
    }

    #[test]
    fn rotated_claude_chain_invalidates_under_its_head_id() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_claude_session(&home.join(".claude"), "-test-project", "sess-aaa", "Topic");
        let bundle = || claude_codex_bundle(home);
        let cold = gather_artifacts(&bundle(), Path::new("/test/project"), None, None);
        assert_eq!(cold.len(), 1);
        assert_eq!(cold[0].message_count, Some(2));

        // The session rotates: appends land in a successor file whose
        // first entry bridges back to sess-aaa. The chain keeps the
        // head id; the whole-chain stamp must invalidate the entry
        // even though sess-aaa.jsonl itself never changed.
        std::fs::write(
            home.join(".claude/projects/-test-project/sess-ccc.jsonl"),
            concat!(
                r#"{"type":"user","uuid":"u-b0","timestamp":"2024-01-02T01:00:00Z","sessionId":"sess-aaa","cwd":"/test/project","message":{"role":"user","content":"bridge"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u-b1","timestamp":"2024-01-02T01:00:01Z","sessionId":"sess-ccc","cwd":"/test/project","message":{"role":"user","content":"after rotation"}}"#,
                "\n",
            ),
        )
        .unwrap();

        let warm = gather_artifacts(&bundle(), Path::new("/test/project"), None, None);
        assert_eq!(warm.len(), 1, "successor segments are not separate rows");
        assert_eq!(warm[0].session_id, "sess-aaa");
        assert!(
            warm[0].message_count.unwrap() > 2,
            "post-rotation turns must reach the row"
        );
        let cache = ListingCache::load();
        let section = cache.section(ArtifactType::Claude);
        assert!(section.contains_key("sess-aaa"));
        assert!(
            !section.contains_key("sess-ccc"),
            "cache keys by chain head id"
        );
    }

    #[test]
    fn appended_codex_rollout_invalidates_its_cached_row() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        let cold = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/work/proj"),
            None,
            None,
        );
        let cold_row = cold
            .iter()
            .find(|r| r.artifact_type == ArtifactType::Codex)
            .unwrap();
        assert_eq!(cold_row.message_count, Some(2));

        append_line(
            &home.join(
                ".codex/sessions/2026/05/07/rollout-2026-05-07T00-00-00-00000000-0000-0000-0000-0000000000aa.jsonl",
            ),
            r#"{"timestamp":"2026-05-07T00:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
        );

        let warm = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/work/proj"),
            None,
            None,
        );
        let row = warm
            .iter()
            .find(|r| r.artifact_type == ArtifactType::Codex)
            .unwrap();
        assert_eq!(row.message_count, Some(3));
    }

    #[test]
    fn deleted_artifact_drops_row_and_cache_entry() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert_eq!(ListingCache::load().section(ArtifactType::Codex).len(), 1);

        // The rollout vanishes upstream. Enumeration is authoritative:
        // no row, and the stale cache entry self-heals away.
        std::fs::remove_file(home.join(
            ".codex/sessions/2026/05/07/rollout-2026-05-07T00-00-00-00000000-0000-0000-0000-0000000000aa.jsonl",
        ))
        .unwrap();

        let warm = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert!(
            warm.iter().all(|r| r.artifact_type != ArtifactType::Codex),
            "deleted artifacts must not produce rows"
        );
        assert!(
            ListingCache::load().section(ArtifactType::Codex).is_empty(),
            "deleted artifacts must drop out of the cache"
        );
    }

    #[test]
    fn corrupt_listing_cache_falls_back_to_fresh_scan() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        let cold = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );

        std::fs::write(cfg.listing_cache_file(), "definitely not json").unwrap();
        let rows = gather_artifacts(
            &claude_codex_bundle(home),
            Path::new("/test/project"),
            None,
            None,
        );
        assert_eq!(rows, cold, "a corrupt cache must never block the picker");
    }

    #[test]
    fn project_filter_applies_to_cached_rows() {
        let cfg = scoped_config_dir();
        let home = cfg.root();
        write_cache_fixtures(home);
        let bundle = || claude_codex_bundle(home);
        let cwd = Path::new("/test/project");

        // A filtered cold gather still warms the cache for everyone
        // (the filter is applied after reconstruction, not baked in).
        let cold_filtered = gather_artifacts(&bundle(), cwd, None, Some(Path::new("/work/proj")));
        assert_eq!(cold_filtered.len(), 1);
        assert_eq!(cold_filtered[0].artifact_type, ArtifactType::Codex);
        let cache = ListingCache::load();
        assert_eq!(cache.section(ArtifactType::Claude).len(), 2);
        assert_eq!(cache.section(ArtifactType::Codex).len(), 1);

        // Warm filtered gathers reproduce the cold filtered rows, and
        // an unfiltered warm gather surfaces everything from cache.
        let warm_filtered = gather_artifacts(&bundle(), cwd, None, Some(Path::new("/work/proj")));
        assert_eq!(warm_filtered, cold_filtered);
        let warm_all = gather_artifacts(&bundle(), cwd, None, None);
        assert_eq!(warm_all.len(), 3);
    }

    #[test]
    #[cfg(unix)]
    fn paths_match_canonicalizes_through_symlink() {
        // `paths_match` is the function that produces `ArtifactRow.matches_cwd`
        // (collect_* all delegate to it). Without canonicalization, a user who
        // navigated to a project via a symlink would see their cwd-row sink
        // in the picker because the symlink path string ≠ the project path
        // string. Verify both arguments are canonicalized.
        //
        // Note: we test `paths_match` directly rather than going through
        // `gather_artifacts` because Claude's project-dir slug encoding is
        // lossy (sanitize_project_path: '/', '_', '.' → '-'; unsanitize: only
        // '-' → '/'). On macOS, tempdir paths contain '.' and end up under
        // /private/var/..., so the unsanitized slug never round-trips back to
        // the real on-disk path. This direct test covers the canonicalization
        // bug regardless of platform-specific tempdir layouts.
        let temp = TempDir::new().unwrap();
        let real_project = temp.path().join("real-project");
        std::fs::create_dir_all(&real_project).unwrap();
        let symlink_path = temp.path().join("symlink-to-project");
        std::os::unix::fs::symlink(&real_project, &symlink_path).unwrap();

        // Sanity-check the setup: the symlink and its target are different
        // string-paths but resolve to the same canonical path.
        assert_ne!(real_project, symlink_path);
        assert_eq!(
            std::fs::canonicalize(&real_project).unwrap(),
            std::fs::canonicalize(&symlink_path).unwrap(),
        );

        // The actual property under test.
        assert!(
            paths_match(&real_project, &symlink_path),
            "paths_match must canonicalize both sides so symlink == target"
        );
        // And symmetric.
        assert!(
            paths_match(&symlink_path, &real_project),
            "paths_match must be symmetric across the symlink"
        );
    }

    #[test]
    fn parse_picker_row_roundtrips_keyed() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Claude,
            path: Some("/tmp/proj".to_string()),
            cwd: None,
            session_id: "sess-abc".to_string(),
            title: "Hello\tworld".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (harness, key, session, title) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, ArtifactType::Claude);
        assert_eq!(key, "/tmp/proj");
        assert_eq!(session, "sess-abc");
        // tab_safe replaces the tab with a space, but the title content
        // otherwise round-trips.
        assert_eq!(title, "Hello world");
    }

    #[test]
    fn parse_picker_row_roundtrips_session_keyed() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Codex,
            path: None,
            cwd: Some("/work/proj".to_string()),
            session_id: "0190abcd".to_string(),
            title: "(no prompt)".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: false,
        };
        let line = format_picker_row(&row);
        let (harness, key, session, title) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, ArtifactType::Codex);
        assert_eq!(key, "/work/proj"); // codex has no project; cwd carried as the keyed slot
        assert_eq!(session, "0190abcd");
        assert_eq!(title, "(no prompt)");
    }

    #[test]
    fn parse_picker_row_carries_title_with_unicode() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Gemini,
            path: Some("/work/proj".to_string()),
            cwd: None,
            session_id: "11111111-2222-3333-4444-555555555555".to_string(),
            title: "Add the share command — finally".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (_, _, _, title) = parse_picker_row(&line).unwrap();
        assert_eq!(title, "Add the share command — finally");
    }

    #[test]
    fn home_relative_strips_home_prefix() {
        let home = Path::new("/Users/alex");
        assert_eq!(
            home_relative(Path::new("/Users/alex/.claude/projects"), Some(home)),
            "~/.claude/projects"
        );
    }

    #[test]
    fn home_relative_returns_tilde_for_home_itself() {
        let home = Path::new("/Users/alex");
        assert_eq!(home_relative(home, Some(home)), "~");
    }

    #[test]
    fn home_relative_passes_through_paths_outside_home() {
        let home = Path::new("/Users/alex");
        assert_eq!(
            home_relative(Path::new("/tmp/elsewhere"), Some(home)),
            "/tmp/elsewhere"
        );
    }

    #[test]
    fn home_relative_passes_through_when_no_home() {
        assert_eq!(home_relative(Path::new("/foo/bar"), None), "/foo/bar");
    }

    #[test]
    fn harness_status_renders_existing_path_with_zero_sessions() {
        let s = HarnessStatus {
            path: "~/.claude/projects".to_string(),
            exists: true,
        };
        assert_eq!(s.render(), "~/.claude/projects (0 sessions)");
    }

    #[test]
    fn harness_status_renders_missing_path_as_not_found() {
        let s = HarnessStatus {
            path: "~/.gemini/tmp".to_string(),
            exists: false,
        };
        assert_eq!(s.render(), "~/.gemini/tmp not found");
    }

    #[test]
    fn format_status_line_pads_for_alignment() {
        let s = HarnessStatus {
            path: "~/.codex/sessions".to_string(),
            exists: true,
        };
        // "claude:" (7) needs 2 trailing spaces; "opencode:" (9) needs 0;
        // "pi:" (3) needs 6. The visible-path column should always start at
        // the same offset.
        let claude_line = format_status_line("claude", &s);
        let opencode_line = format_status_line("opencode", &s);
        let pi_line = format_status_line("pi", &s);
        let offset = |line: &str| line.find('~').unwrap();
        assert_eq!(offset(&claude_line), offset(&opencode_line));
        assert_eq!(offset(&claude_line), offset(&pi_line));
    }

    #[test]
    fn harness_status_for_missing_claude_dir_reports_not_found() {
        // Bundle whose claude resolver points at a directory that doesn't
        // exist on disk; the status should still resolve a path and report
        // it as missing rather than going through the `unresolved` branch.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude"); // never created
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let bundle = HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        };
        let status = harness_status_claude(&bundle, None);
        assert!(!status.exists, "missing dir must report exists=false");
        assert!(
            status.path.contains("projects"),
            "path must include the projects subdir (got {:?})",
            status.path
        );
    }

    #[test]
    fn harness_status_for_present_claude_dir_reports_existence() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        std::fs::create_dir_all(claude_dir.join("projects")).unwrap();
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let bundle = HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        };
        let status = harness_status_claude(&bundle, None);
        assert!(status.exists);
    }

    #[test]
    fn harness_status_for_empty_bundle_is_unresolved() {
        let bundle = HarnessBundle::default();
        // Every harness slot is None, so each status hits the unresolved branch.
        for status in [
            harness_status_claude(&bundle, None),
            harness_status_gemini(&bundle, None),
            harness_status_codex(&bundle, None),
            harness_status_opencode(&bundle, None),
            harness_status_pi(&bundle, None),
        ] {
            assert_eq!(status, HarnessStatus::unresolved());
            assert!(!status.exists);
        }
    }
}
