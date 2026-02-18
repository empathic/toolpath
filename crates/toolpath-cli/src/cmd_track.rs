use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;
use toolpath::v1;

// ============================================================================
// CLI argument types
// ============================================================================

#[derive(Subcommand, Debug)]
pub enum TrackOp {
    /// Start a tracking session; reads initial content from stdin
    Init {
        /// Artifact path (used as the change-map key)
        #[arg(long)]
        file: String,

        /// Default actor (e.g. "human:alex")
        #[arg(long)]
        actor: String,

        /// Actor definition as JSON (e.g. '{"human:alex": {"name": "Alex"}}')
        #[arg(long)]
        actor_def: Option<String>,

        /// Path title (appears in meta.title)
        #[arg(long)]
        title: Option<String>,

        /// Path source tag (appears in meta.source)
        #[arg(long)]
        source: Option<String>,

        /// Base URI for the path (e.g. "github:org/repo")
        #[arg(long)]
        base_uri: Option<String>,

        /// Base ref (e.g. commit hash)
        #[arg(long)]
        base_ref: Option<String>,

        /// Directory for session state files (default: $TMPDIR)
        #[arg(long)]
        session_dir: Option<PathBuf>,
    },

    /// Record a new step; reads current content from stdin
    Step {
        /// Path to session state file
        #[arg(long)]
        session: PathBuf,

        /// Sequence number for this state
        #[arg(long)]
        seq: u64,

        /// Parent sequence number (state this was derived from)
        #[arg(long)]
        parent_seq: u64,

        /// Override actor for this step
        #[arg(long)]
        actor: Option<String>,

        /// Override timestamp (ISO 8601)
        #[arg(long)]
        time: Option<String>,
    },

    /// Cache content at a sequence number without creating a step;
    /// reads content from stdin. Used when navigating history (undo/redo).
    Visit {
        /// Path to session state file
        #[arg(long)]
        session: PathBuf,

        /// Sequence number to cache
        #[arg(long)]
        seq: u64,

        /// Inherit seq_to_step mapping from this ancestor seq
        #[arg(long)]
        inherit_from: Option<u64>,
    },

    /// Set intent on the current head step
    Note {
        /// Path to session state file
        #[arg(long)]
        session: PathBuf,

        /// Intent text
        #[arg(long)]
        intent: String,
    },

    /// Emit the session as a Toolpath Path document
    Export {
        /// Path to session state file
        #[arg(long)]
        session: PathBuf,
    },

    /// Export and delete the session state file
    Close {
        /// Path to session state file
        #[arg(long)]
        session: PathBuf,

        /// Write output to a file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// List active tracking sessions
    List {
        /// Directory to scan for sessions (default: $TMPDIR)
        #[arg(long)]
        session_dir: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ============================================================================
// Track state (stored in meta.track of a valid Path document)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrackState {
    version: u32,
    file: String,
    default_actor: String,
    buffer_cache: HashMap<u64, String>,
    seq_to_step: HashMap<u64, String>,
    step_counter: u64,
    created_at: String,
}

// ============================================================================
// Helpers
// ============================================================================

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read stdin")?;
    Ok(buf)
}

fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn session_dir(explicit: Option<&PathBuf>) -> PathBuf {
    explicit.cloned().unwrap_or_else(std::env::temp_dir)
}

/// Load a session file. The file is a valid `{"Path": {...}}` document with
/// tracking bookkeeping in `meta.track`. Returns the Path (with track state
/// removed from meta) and the extracted TrackState.
fn load_session(path: &std::path::Path) -> Result<(v1::Path, TrackState)> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read session file: {}", path.display()))?;
    let doc: v1::Document = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse session file: {}", path.display()))?;
    let v1::Document::Path(mut path_doc) = doc else {
        anyhow::bail!("session file is not a Path document: {}", path.display());
    };
    let track_value = path_doc
        .meta
        .as_mut()
        .and_then(|m| m.extra.remove("track"))
        .with_context(|| format!("session file missing meta.track: {}", path.display()))?;
    let state: TrackState = serde_json::from_value(track_value)
        .with_context(|| format!("failed to parse meta.track: {}", path.display()))?;
    // Clean up meta if now empty (track was the only content)
    if let Some(meta) = &path_doc.meta
        && meta_is_empty(meta)
    {
        path_doc.meta = None;
    }
    Ok((path_doc, state))
}

/// Save a session file. Injects TrackState into `meta.track` and writes as a
/// valid `{"Path": {...}}` document.
fn save_session(path: &std::path::Path, doc: &v1::Path, state: &TrackState) -> Result<()> {
    let mut doc = doc.clone();
    let meta = doc.meta.get_or_insert_with(v1::PathMeta::default);
    meta.extra.insert(
        "track".to_string(),
        serde_json::to_value(state).context("failed to serialize track state")?,
    );
    let wrapped = v1::Document::Path(doc);

    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)
        .context("failed to create temp file for atomic write")?;
    serde_json::to_writer_pretty(&tmp, &wrapped).context("failed to serialize session")?;
    tmp.persist(path)
        .with_context(|| format!("failed to persist session file: {}", path.display()))?;
    Ok(())
}

fn meta_is_empty(meta: &v1::PathMeta) -> bool {
    meta.title.is_none()
        && meta.source.is_none()
        && meta.intent.is_none()
        && meta.refs.is_empty()
        && meta.actors.is_none()
        && meta.signatures.is_empty()
        && meta.extra.is_empty()
}

fn compute_diff(old: &str, new: &str) -> Option<String> {
    let diff = TextDiff::from_lines(old, new);
    let unified = diff.unified_diff().context_radius(3).to_string();
    if unified.is_empty() {
        None
    } else {
        Some(unified)
    }
}

fn format_output(doc: v1::Document, pretty: bool) -> Result<String> {
    if pretty {
        doc.to_json_pretty()
    } else {
        doc.to_json()
    }
    .context("failed to serialize document")
}

// ============================================================================
// Subcommand implementations
// ============================================================================

/// Configuration for initializing a tracking session.
struct InitConfig {
    content: String,
    file: String,
    actor: String,
    actors: Option<HashMap<String, v1::ActorDefinition>>,
    title: Option<String>,
    source: Option<String>,
    base_uri: Option<String>,
    base_ref: Option<String>,
    session_dir: Option<PathBuf>,
}

/// Core init logic, decoupled from stdin. Returns the path to the session file.
fn init_session(config: InitConfig) -> Result<PathBuf> {
    let now = now_iso8601();
    let pid = std::process::id();
    let ts_compact = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let session_id = format!("track-{ts_compact}-{pid}");

    let base = config.base_uri.map(|uri| v1::Base {
        uri,
        ref_str: config.base_ref,
    });

    let mut path_doc = v1::Path::new(&session_id, base, "none");

    // Set up meta with title, source, actors
    let has_meta = config.title.is_some() || config.source.is_some() || config.actors.is_some();
    if has_meta {
        path_doc.meta = Some(v1::PathMeta {
            title: config.title,
            source: config.source,
            actors: config.actors,
            ..Default::default()
        });
    }

    let mut buffer_cache = HashMap::new();
    buffer_cache.insert(0u64, config.content);

    let state = TrackState {
        version: 1,
        file: config.file,
        default_actor: config.actor,
        buffer_cache,
        seq_to_step: HashMap::new(),
        step_counter: 0,
        created_at: now,
    };

    let dir = session_dir(config.session_dir.as_ref());
    let session_path = dir.join(format!("{session_id}.json"));
    save_session(&session_path, &path_doc, &state)?;
    Ok(session_path)
}

fn run_init(op: &TrackOp) -> Result<()> {
    let TrackOp::Init {
        file,
        actor,
        actor_def,
        title,
        source,
        base_uri,
        base_ref,
        session_dir,
    } = op
    else {
        unreachable!()
    };

    let content = read_stdin()?;
    let actors: Option<HashMap<String, v1::ActorDefinition>> = actor_def
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("failed to parse --actor-def JSON")?;

    let session_path = init_session(InitConfig {
        content,
        file: file.clone(),
        actor: actor.clone(),
        actors,
        title: title.clone(),
        source: source.clone(),
        base_uri: base_uri.clone(),
        base_ref: base_ref.clone(),
        session_dir: session_dir.clone(),
    })?;
    println!("{}", session_path.display());
    Ok(())
}

/// Result of recording a step: either a new step ID or a skip.
#[derive(Debug, PartialEq)]
enum StepResult {
    Created(String),
    Skip,
}

/// Core step logic, decoupled from stdin. Returns the step result and
/// saves the updated session to disk.
fn record_step(
    session_path: &std::path::Path,
    content: String,
    seq: u64,
    parent_seq: u64,
    actor_override: Option<String>,
    time_override: Option<String>,
) -> Result<StepResult> {
    let (mut path_doc, mut state) = load_session(session_path)?;

    // If we've already seen this seq, it's a revisit (undo/redo navigation),
    // not a new edit. Skip without mutating anything.
    if state.buffer_cache.contains_key(&seq) {
        return Ok(StepResult::Skip);
    }

    // Look up parent buffer content
    let parent_content = state
        .buffer_cache
        .get(&parent_seq)
        .with_context(|| format!("parent seq {parent_seq} not found in buffer cache"))?
        .clone();

    // Compute diff
    let diff = compute_diff(&parent_content, &content);

    // Cache this buffer state
    state.buffer_cache.insert(seq, content);

    let result = if let Some(diff_text) = diff {
        // Create a new step
        state.step_counter += 1;
        let step_id = format!("step-{:03}", state.step_counter);
        let actor = actor_override.unwrap_or_else(|| state.default_actor.clone());
        let timestamp = time_override.unwrap_or_else(now_iso8601);

        let mut step =
            v1::Step::new(&step_id, &actor, &timestamp).with_raw_change(&state.file, &diff_text);

        // Wire parent (empty string means "no step" — root state)
        if let Some(parent_step_id) = state.seq_to_step.get(&parent_seq)
            && !parent_step_id.is_empty()
        {
            step = step.with_parent(parent_step_id);
        }

        path_doc.steps.push(step);
        state.seq_to_step.insert(seq, step_id.clone());
        path_doc.path.head = step_id.clone();

        StepResult::Created(step_id)
    } else {
        // Empty diff — just cache the buffer, no step
        state.seq_to_step.insert(
            seq,
            state
                .seq_to_step
                .get(&parent_seq)
                .cloned()
                .unwrap_or_default(),
        );
        StepResult::Skip
    };

    save_session(session_path, &path_doc, &state)?;
    Ok(result)
}

fn run_step(
    session_path: PathBuf,
    seq: u64,
    parent_seq: u64,
    actor_override: Option<String>,
    time_override: Option<String>,
) -> Result<()> {
    let content = read_stdin()?;
    match record_step(
        &session_path,
        content,
        seq,
        parent_seq,
        actor_override,
        time_override,
    )? {
        StepResult::Created(id) => println!("{id}"),
        StepResult::Skip => println!("skip"),
    }
    Ok(())
}

fn run_visit(session_path: PathBuf, seq: u64, inherit_from: Option<u64>) -> Result<()> {
    let content = read_stdin()?;
    let (path_doc, mut state) = load_session(&session_path)?;
    let mut changed = false;

    use std::collections::hash_map::Entry;
    if let Entry::Vacant(e) = state.buffer_cache.entry(seq) {
        e.insert(content);
        changed = true;
    }

    // Inherit step mapping from an ancestor so that branches from this
    // visited seq wire to the correct parent step.
    // (contains_key + get is required here to avoid borrowing state.seq_to_step mutably
    // and immutably at the same time)
    #[allow(clippy::map_entry)]
    if !state.seq_to_step.contains_key(&seq)
        && let Some(ancestor) = inherit_from
    {
        let step_id = state
            .seq_to_step
            .get(&ancestor)
            .cloned()
            .unwrap_or_default();
        state.seq_to_step.insert(seq, step_id);
        changed = true;
    }

    if changed {
        save_session(&session_path, &path_doc, &state)?;
    }
    Ok(())
}

fn run_note(session_path: PathBuf, intent: String) -> Result<()> {
    let (mut path_doc, state) = load_session(&session_path)?;
    if path_doc.path.head == "none" {
        anyhow::bail!("no head step to annotate");
    }

    let head_id = path_doc.path.head.clone();
    let step = path_doc
        .steps
        .iter_mut()
        .find(|s| s.step.id == head_id)
        .context("head step not found in session")?;

    step.meta.get_or_insert_with(v1::StepMeta::default).intent = Some(intent);
    save_session(&session_path, &path_doc, &state)?;
    Ok(())
}

fn run_export(session_path: PathBuf, pretty: bool) -> Result<()> {
    let (path_doc, _state) = load_session(&session_path)?;
    let doc = v1::Document::Path(path_doc);
    let json = format_output(doc, pretty)?;
    println!("{json}");
    Ok(())
}

fn run_close(session_path: PathBuf, pretty: bool, output: Option<PathBuf>) -> Result<()> {
    let (path_doc, _state) = load_session(&session_path)?;
    let doc = v1::Document::Path(path_doc);
    let json = format_output(doc, pretty)?;

    if let Some(out) = output {
        std::fs::write(&out, &json)
            .with_context(|| format!("failed to write to {}", out.display()))?;
    } else {
        println!("{json}");
    }

    std::fs::remove_file(&session_path)
        .with_context(|| format!("failed to remove session file: {}", session_path.display()))?;
    Ok(())
}

fn run_list(session_dir_opt: Option<PathBuf>, json: bool) -> Result<()> {
    let dir = session_dir(session_dir_opt.as_ref());
    let mut sessions: Vec<SessionSummary> = Vec::new();

    let entries = std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("track-")
            && name_str.ends_with(".json")
            && let Ok((path_doc, state)) = load_session(&entry.path())
        {
            sessions.push(SessionSummary {
                session_file: entry.path().to_string_lossy().to_string(),
                session_id: path_doc.path.id,
                file: state.file,
                actor: state.default_actor,
                steps: path_doc.steps.len(),
                created_at: state.created_at,
            });
        }
    }

    if json {
        let out =
            serde_json::to_string_pretty(&sessions).context("failed to serialize session list")?;
        println!("{out}");
    } else if sessions.is_empty() {
        println!("No active tracking sessions.");
    } else {
        for s in &sessions {
            println!(
                "{} | {} | {} | {} steps | {}",
                s.session_id, s.file, s.actor, s.steps, s.created_at,
            );
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SessionSummary {
    session_file: String,
    session_id: String,
    file: String,
    actor: String,
    steps: usize,
    created_at: String,
}

// ============================================================================
// Entry point
// ============================================================================

pub fn run(op: TrackOp, pretty: bool) -> Result<()> {
    match op {
        ref init @ TrackOp::Init { .. } => run_init(init),
        TrackOp::Step {
            session,
            seq,
            parent_seq,
            actor,
            time,
        } => run_step(session, seq, parent_seq, actor, time),
        TrackOp::Visit {
            session,
            seq,
            inherit_from,
        } => run_visit(session, seq, inherit_from),
        TrackOp::Note { session, intent } => run_note(session, intent),
        TrackOp::Export { session } => run_export(session, pretty),
        TrackOp::Close { session, output } => run_close(session, pretty, output),
        TrackOp::List { session_dir, json } => run_list(session_dir, json),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_session(dir: &std::path::Path, content: &str) -> PathBuf {
        let path_doc = v1::Path::new("track-test-1", None, "none");
        let state = TrackState {
            version: 1,
            file: "test.txt".to_string(),
            default_actor: "human:tester".to_string(),
            buffer_cache: {
                let mut m = HashMap::new();
                m.insert(0, content.to_string());
                m
            },
            seq_to_step: HashMap::new(),
            step_counter: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let path = dir.join("track-test-1.json");
        save_session(&path, &path_doc, &state).unwrap();
        path
    }

    // ── Pure helpers ─────────────────────────────────────────────────────

    #[test]
    fn test_compute_diff_identical() {
        assert!(compute_diff("hello\n", "hello\n").is_none());
    }

    #[test]
    fn test_compute_diff_changed() {
        let diff = compute_diff("hello\n", "world\n");
        assert!(diff.is_some());
        let d = diff.unwrap();
        assert!(d.contains("-hello"));
        assert!(d.contains("+world"));
    }

    #[test]
    fn test_compute_diff_empty_strings() {
        assert!(compute_diff("", "").is_none());
    }

    #[test]
    fn test_compute_diff_from_empty() {
        let diff = compute_diff("", "new content\n").unwrap();
        assert!(diff.contains("+new content"));
    }

    #[test]
    fn test_compute_diff_to_empty() {
        let diff = compute_diff("old content\n", "").unwrap();
        assert!(diff.contains("-old content"));
    }

    #[test]
    fn test_multiline_diff() {
        let old = "line one\nline two\nline three\n";
        let new = "line one\nline TWO\nline three\nline four\n";
        let diff = compute_diff(old, new).unwrap();
        assert!(diff.contains("-line two"));
        assert!(diff.contains("+line TWO"));
        assert!(diff.contains("+line four"));
    }

    #[test]
    fn test_now_iso8601_format() {
        let ts = now_iso8601();
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
        assert_eq!(ts.len(), 20);
    }

    #[test]
    fn test_session_dir_explicit() {
        let p = PathBuf::from("/custom/dir");
        assert_eq!(session_dir(Some(&p)), PathBuf::from("/custom/dir"));
    }

    #[test]
    fn test_session_dir_default() {
        let d = session_dir(None);
        assert_eq!(d, std::env::temp_dir());
    }

    #[test]
    fn test_format_output_pretty_and_compact() {
        let step = v1::Step::new("s1", "human:alex", "2026-01-01T00:00:00Z");
        let doc = v1::Document::Step(step);

        let pretty = format_output(doc.clone(), true).unwrap();
        assert!(pretty.contains('\n'));

        let compact = format_output(doc, false).unwrap();
        assert!(!compact.contains('\n'));
    }

    // ── Session persistence ──────────────────────────────────────────────

    #[test]
    fn test_save_and_load_session() {
        let dir = TempDir::new().unwrap();
        let path = make_session(dir.path(), "initial content\n");
        let (path_doc, state) = load_session(&path).unwrap();
        assert_eq!(path_doc.path.id, "track-test-1");
        assert_eq!(state.file, "test.txt");
        assert_eq!(state.buffer_cache[&0], "initial content\n");
    }

    #[test]
    fn test_load_session_nonexistent() {
        let result = load_session(std::path::Path::new("/nonexistent/session.json"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("failed to read session file")
        );
    }

    #[test]
    fn test_load_session_corrupt_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not valid json {{{").unwrap();
        let result = load_session(&path);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("failed to parse session file")
        );
    }

    #[test]
    fn test_session_with_base() {
        let dir = TempDir::new().unwrap();
        let base = v1::Base {
            uri: "github:org/repo".to_string(),
            ref_str: Some("main".to_string()),
        };
        let path_doc = v1::Path::new("track-base-test", Some(base), "none");
        let state = TrackState {
            version: 1,
            file: "f.rs".to_string(),
            default_actor: "human:dev".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let path = dir.path().join("track-base-test.json");
        save_session(&path, &path_doc, &state).unwrap();
        let (loaded_doc, _) = load_session(&path).unwrap();
        let base = loaded_doc.path.base.unwrap();
        assert_eq!(base.uri, "github:org/repo");
        assert_eq!(base.ref_str, Some("main".to_string()));
    }

    #[test]
    fn test_session_without_base() {
        let dir = TempDir::new().unwrap();
        let path_doc = v1::Path::new("track-no-base", None, "none");
        let state = TrackState {
            version: 1,
            file: "f.rs".to_string(),
            default_actor: "human:dev".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let path = dir.path().join("track-no-base.json");
        save_session(&path, &path_doc, &state).unwrap();
        let (loaded_doc, _) = load_session(&path).unwrap();
        assert!(loaded_doc.path.base.is_none());
    }

    #[test]
    fn test_session_file_is_valid_toolpath_document() {
        // The session file should be readable by any Toolpath tool
        let dir = TempDir::new().unwrap();
        let path = make_session(dir.path(), "hello\n");

        let data = std::fs::read_to_string(&path).unwrap();
        let doc = v1::Document::from_json(&data).unwrap();
        match doc {
            v1::Document::Path(p) => {
                assert_eq!(p.path.id, "track-test-1");
                // meta.track is present (tracking bookkeeping)
                assert!(p.meta.as_ref().unwrap().extra.contains_key("track"));
            }
            _ => panic!("Expected Path"),
        }
    }

    // ── init_session ──────────────────────────────────────────────────────

    fn simple_init(dir: &std::path::Path, content: &str, file: &str, actor: &str) -> PathBuf {
        init_session(InitConfig {
            content: content.to_string(),
            file: file.to_string(),
            actor: actor.to_string(),
            actors: None,
            title: None,
            source: None,
            base_uri: None,
            base_ref: None,
            session_dir: Some(dir.to_path_buf()),
        })
        .unwrap()
    }

    #[test]
    fn test_init_creates_session_file() {
        let dir = TempDir::new().unwrap();
        let session_path = simple_init(dir.path(), "hello\n", "test.txt", "human:alex");

        assert!(session_path.exists());
        let (path_doc, state) = load_session(&session_path).unwrap();
        assert!(path_doc.path.id.starts_with("track-"));
        assert_eq!(state.file, "test.txt");
        assert_eq!(state.default_actor, "human:alex");
        assert_eq!(state.buffer_cache[&0], "hello\n");
        assert_eq!(state.version, 1);
        assert_eq!(path_doc.path.head, "none");
        assert!(path_doc.steps.is_empty());
        assert_eq!(state.step_counter, 0);
    }

    #[test]
    fn test_init_with_base() {
        let dir = TempDir::new().unwrap();
        let session_path = init_session(InitConfig {
            content: "content".to_string(),
            file: "f.rs".to_string(),
            actor: "human:dev".to_string(),
            actors: None,
            title: None,
            source: None,
            base_uri: Some("github:org/repo".to_string()),
            base_ref: Some("abc123".to_string()),
            session_dir: Some(dir.path().to_path_buf()),
        })
        .unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        let base = path_doc.path.base.unwrap();
        assert_eq!(base.uri, "github:org/repo");
        assert_eq!(base.ref_str, Some("abc123".to_string()));
    }

    #[test]
    fn test_init_session_roundtrips() {
        let dir = TempDir::new().unwrap();
        let session_path = simple_init(dir.path(), "initial\n", "test.txt", "human:alex");

        let (path_doc, state) = load_session(&session_path).unwrap();
        assert!(path_doc.path.id.starts_with("track-"));
        assert_eq!(state.buffer_cache[&0], "initial\n");
    }

    #[test]
    fn test_init_with_actor_def() {
        let dir = TempDir::new().unwrap();
        let mut actors = HashMap::new();
        actors.insert(
            "human:alex".to_string(),
            v1::ActorDefinition {
                name: Some("Alex".to_string()),
                identities: vec![v1::Identity {
                    system: "email".to_string(),
                    id: "alex@example.com".to_string(),
                }],
                ..Default::default()
            },
        );
        let session_path = init_session(InitConfig {
            content: "content\n".to_string(),
            file: "f.rs".to_string(),
            actor: "human:alex".to_string(),
            actors: Some(actors),
            title: None,
            source: None,
            base_uri: None,
            base_ref: None,
            session_dir: Some(dir.path().to_path_buf()),
        })
        .unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        let meta = path_doc.meta.as_ref().unwrap();
        let actors = meta.actors.as_ref().unwrap();
        let def = &actors["human:alex"];
        assert_eq!(def.name.as_deref(), Some("Alex"));
        assert_eq!(def.identities[0].id, "alex@example.com");

        // The session IS the document — actors survive as-is
        let doc = v1::Document::Path(path_doc);
        let json = doc.to_json_pretty().unwrap();
        assert!(json.contains("alex@example.com"));
        let parsed = v1::Document::from_json(&json).unwrap();
        match parsed {
            v1::Document::Path(p) => {
                let a = p.meta.unwrap().actors.unwrap();
                assert_eq!(a["human:alex"].name.as_deref(), Some("Alex"));
            }
            _ => panic!("Expected Path"),
        }
    }

    // ── record_step ──────────────────────────────────────────────────────

    #[test]
    fn test_record_step_creates_root_step() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        let result = record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        assert_eq!(result, StepResult::Created("step-001".to_string()));

        let (path_doc, state) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 1);
        assert_eq!(path_doc.steps[0].step.id, "step-001");
        assert!(path_doc.steps[0].step.parents.is_empty());
        assert_eq!(path_doc.steps[0].step.actor, "human:tester");
        assert_eq!(path_doc.path.head, "step-001");
        assert_eq!(state.buffer_cache[&1], "world\n");
        assert_eq!(state.seq_to_step[&1], "step-001");

        // Verify the diff is present in the change
        let change = &path_doc.steps[0].change["test.txt"];
        let raw = change.raw.as_ref().unwrap();
        assert!(raw.contains("-hello"));
        assert!(raw.contains("+world"));
    }

    #[test]
    fn test_record_step_skip_on_identical() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        let result = record_step(&session_path, "hello\n".to_string(), 1, 0, None, None).unwrap();

        assert_eq!(result, StepResult::Skip);

        let (path_doc, state) = load_session(&session_path).unwrap();
        assert!(path_doc.steps.is_empty());
        assert_eq!(path_doc.path.head, "none");
        // Buffer should still be cached
        assert_eq!(state.buffer_cache[&1], "hello\n");
    }

    #[test]
    fn test_record_step_with_parent() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "original\n");

        // First step: seq 1 from seq 0
        let r1 = record_step(
            &session_path,
            "edit-1\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r1, StepResult::Created("step-001".to_string()));

        // Second step: seq 2 from seq 1 (linear chain)
        let r2 = record_step(
            &session_path,
            "edit-2\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r2, StepResult::Created("step-002".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 2);
        assert!(path_doc.steps[0].step.parents.is_empty()); // root
        assert_eq!(path_doc.steps[1].step.parents, vec!["step-001"]); // child
        assert_eq!(path_doc.path.head, "step-002");
    }

    #[test]
    fn test_record_step_actor_override() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            Some("tool:formatter".to_string()),
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps[0].step.actor, "tool:formatter");
    }

    #[test]
    fn test_record_step_time_override() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-06-15T12:00:00Z".to_string()),
        )
        .unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps[0].step.timestamp, "2026-06-15T12:00:00Z");
    }

    #[test]
    fn test_record_step_missing_parent_seq() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        let result = record_step(
            &session_path,
            "world\n".to_string(),
            1,
            99, // parent_seq 99 doesn't exist
            None,
            None,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("parent seq 99 not found")
        );
    }

    #[test]
    fn test_record_step_branching_dag() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "original\n");

        // Step 1: seq 1 from seq 0
        record_step(
            &session_path,
            "branch-a\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        // Step 2: seq 2 also from seq 0 (fork!)
        record_step(
            &session_path,
            "branch-b\n".to_string(),
            2,
            0,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 2);
        // Both are roots (parent seq 0 has no step)
        assert!(path_doc.steps[0].step.parents.is_empty());
        assert!(path_doc.steps[1].step.parents.is_empty());
        assert_eq!(path_doc.path.head, "step-002");
    }

    #[test]
    fn test_record_step_branching_with_dead_ends() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "original\n");

        // Linear: seq 0 → seq 1 → seq 2
        record_step(
            &session_path,
            "edit-1\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        record_step(
            &session_path,
            "edit-2\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();

        // Branch from seq 1: seq 3 (creates a fork, making step-002 a dead end)
        record_step(
            &session_path,
            "branch-b\n".to_string(),
            3,
            1,
            None,
            Some("2026-01-01T00:03:00Z".to_string()),
        )
        .unwrap();

        // Dead ends work directly on the session — no conversion needed
        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 3);
        assert_eq!(path_doc.steps[1].step.parents, vec!["step-001"]);
        assert_eq!(path_doc.steps[2].step.parents, vec!["step-001"]); // fork

        let dead = v1::query::dead_ends(&path_doc.steps, &path_doc.path.head);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].step.id, "step-002");
    }

    #[test]
    fn test_record_step_skip_preserves_seq_mapping() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");

        // Step 1
        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        // Undo back to "hello\n" at seq 2 (parent_seq 0) — skip
        let r = record_step(&session_path, "hello\n".to_string(), 2, 0, None, None).unwrap();
        assert_eq!(r, StepResult::Skip);

        // Edit from seq 2 to seq 3 — should be a root step (seq 2 mapped to "")
        let r3 = record_step(
            &session_path,
            "goodbye\n".to_string(),
            3,
            2,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r3, StepResult::Created("step-002".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 2);
        // step-002 is a root (seq 2 mapped to empty string → no parent)
        assert!(path_doc.steps[1].step.parents.is_empty());
    }

    #[test]
    fn test_record_step_revisit_seq_skips_without_mutation() {
        // Simulates: edit A→B (seq 1), edit B→C (seq 2), undo to B (seq 1 again).
        // The revisit of seq 1 should skip without creating a reverse-diff step
        // or overwriting the seq_to_step mapping.
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        // seq 1: A→B
        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        // seq 2: B→C
        record_step(
            &session_path,
            "C\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();

        let (before_doc, before_state) = load_session(&session_path).unwrap();
        assert_eq!(before_doc.steps.len(), 2);
        assert_eq!(before_state.seq_to_step[&1], "step-001");
        assert_eq!(before_doc.path.head, "step-002");

        // Undo to seq 1 (revisit) — should skip, no mutation
        let r = record_step(&session_path, "B\n".to_string(), 1, 2, None, None).unwrap();
        assert_eq!(r, StepResult::Skip);

        let (after_doc, after_state) = load_session(&session_path).unwrap();
        assert_eq!(after_doc.steps.len(), 2); // no new step
        assert_eq!(after_state.seq_to_step[&1], "step-001"); // mapping preserved
        assert_eq!(after_doc.path.head, "step-002"); // head unchanged
    }

    #[test]
    fn test_record_step_undo_then_branch() {
        // Simulates: edit A→B (seq 1), edit B→C (seq 2), undo to B (seq 1),
        // new edit B→D (seq 3). The branch should parent off step-001, not
        // a spurious reverse-diff step.
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        record_step(
            &session_path,
            "C\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();

        // Undo to seq 1 — skip
        let r = record_step(&session_path, "B\n".to_string(), 1, 2, None, None).unwrap();
        assert_eq!(r, StepResult::Skip);

        // New edit from seq 1 → seq 3
        let r3 = record_step(
            &session_path,
            "D\n".to_string(),
            3,
            1,
            None,
            Some("2026-01-01T00:03:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r3, StepResult::Created("step-003".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps.len(), 3);
        // step-003 parents off step-001 (the original B edit), not some reverse-diff
        assert_eq!(path_doc.steps[2].step.parents, vec!["step-001"]);
        // step-002 (C) is now a dead end
        assert_eq!(path_doc.path.head, "step-003");
    }

    #[test]
    fn test_record_step_undo_to_initial_then_branch() {
        // Undo all the way back to seq 0 (initial state), then branch.
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        // Undo to seq 0 — skip (seq 0 was cached at init time)
        let r = record_step(&session_path, "A\n".to_string(), 0, 1, None, None).unwrap();
        assert_eq!(r, StepResult::Skip);

        // New edit from seq 0 → seq 2
        let r2 = record_step(
            &session_path,
            "C\n".to_string(),
            2,
            0,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r2, StepResult::Created("step-002".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        // step-002 is a root (seq 0 has no step), just like step-001
        assert!(path_doc.steps[1].step.parents.is_empty());
    }

    // ── visit + inherit ────────────────────────────────────────────────

    /// Helper: simulate a visit by directly manipulating the session
    /// (run_visit reads stdin, so we test the logic it performs).
    fn simulate_visit(
        session_path: &std::path::Path,
        seq: u64,
        content: &str,
        inherit_from: Option<u64>,
    ) {
        let (path_doc, mut state) = load_session(session_path).unwrap();
        if !state.buffer_cache.contains_key(&seq) {
            state.buffer_cache.insert(seq, content.to_string());
        }
        if !state.seq_to_step.contains_key(&seq) {
            if let Some(ancestor) = inherit_from {
                let step_id = state
                    .seq_to_step
                    .get(&ancestor)
                    .cloned()
                    .unwrap_or_default();
                state.seq_to_step.insert(seq, step_id);
            }
        }
        save_session(session_path, &path_doc, &state).unwrap();
    }

    #[test]
    fn test_visit_caches_content_and_inherits_mapping() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        // seq 1: A→B
        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        // seq 2: B→C
        record_step(
            &session_path,
            "C\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();

        // Navigate to seq 1 (already cached), inherit from seq 1
        simulate_visit(&session_path, 1, "B\n", Some(1));

        let (_, state) = load_session(&session_path).unwrap();
        // seq 1 already had a step mapping; visit should not overwrite
        assert_eq!(state.seq_to_step[&1], "step-001");
    }

    #[test]
    fn test_visit_intermediate_inherits_ancestor_step() {
        // Simulates: mundo jumping to an intermediate seq between step boundaries
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        // seq 1: step-001 (A→B)
        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        // seq 3: step-002 (B→C) — pretend seq 2 was skipped by TextChanged batching
        {
            let (path_doc, mut state) = load_session(&session_path).unwrap();
            state.buffer_cache.insert(3, "C\n".to_string());
            save_session(&session_path, &path_doc, &state).unwrap();
        }
        record_step(
            &session_path,
            "D\n".to_string(),
            4,
            3,
            None,
            Some("2026-01-01T00:03:00Z".to_string()),
        )
        .unwrap();

        // Mundo navigates to seq 2 (intermediate, no step). Inherit from seq 1.
        simulate_visit(&session_path, 2, "B-mid\n", Some(1));

        let (_, state) = load_session(&session_path).unwrap();
        assert_eq!(state.buffer_cache[&2], "B-mid\n");
        assert_eq!(state.seq_to_step[&2], "step-001");

        // Now branch from seq 2 — should parent off step-001
        let r = record_step(
            &session_path,
            "E\n".to_string(),
            5,
            2,
            None,
            Some("2026-01-01T00:04:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r, StepResult::Created("step-003".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert_eq!(path_doc.steps[2].step.parents, vec!["step-001"]);
    }

    #[test]
    fn test_visit_inherit_from_initial_state() {
        // Navigate to an intermediate seq when only seq 0 (init) exists
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        // Visit seq 3, inherit from 0 (initial state, no step)
        simulate_visit(&session_path, 3, "X\n", Some(0));

        let (_, state) = load_session(&session_path).unwrap();
        // seq_to_step[0] doesn't exist, so inherits empty string
        assert_eq!(state.seq_to_step[&3], "");

        // Branch from seq 3 — root step (empty parent)
        let r = record_step(
            &session_path,
            "Y\n".to_string(),
            4,
            3,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r, StepResult::Created("step-001".to_string()));

        let (path_doc, _) = load_session(&session_path).unwrap();
        assert!(path_doc.steps[0].step.parents.is_empty());
    }

    #[test]
    fn test_visit_does_not_overwrite_existing_mapping() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "A\n");

        record_step(
            &session_path,
            "B\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        // seq 1 already has seq_to_step["step-001"]. Visit with different inherit should not overwrite.
        simulate_visit(&session_path, 1, "B\n", Some(0));

        let (_, state) = load_session(&session_path).unwrap();
        assert_eq!(state.seq_to_step[&1], "step-001"); // preserved
    }

    // ── run_note ─────────────────────────────────────────────────────────

    #[test]
    fn test_note_sets_intent() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");
        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        run_note(session_path.clone(), "Fix the greeting".to_string()).unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        let intent = path_doc.steps[0]
            .meta
            .as_ref()
            .and_then(|m| m.intent.as_ref());
        assert_eq!(intent, Some(&"Fix the greeting".to_string()));
    }

    #[test]
    fn test_note_overwrites_previous_intent() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");
        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        run_note(session_path.clone(), "First intent".to_string()).unwrap();
        run_note(session_path.clone(), "Updated intent".to_string()).unwrap();

        let (path_doc, _) = load_session(&session_path).unwrap();
        let intent = path_doc.steps[0]
            .meta
            .as_ref()
            .and_then(|m| m.intent.as_ref());
        assert_eq!(intent, Some(&"Updated intent".to_string()));
    }

    #[test]
    fn test_note_no_head_step_errors() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");
        // No steps recorded → head is "none"

        let result = run_note(session_path, "some intent".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no head step"));
    }

    // ── run_close ────────────────────────────────────────────────────────

    #[test]
    fn test_close_deletes_session_file() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");
        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        assert!(session_path.exists());

        run_close(session_path.clone(), false, None).unwrap();
        assert!(!session_path.exists());
    }

    #[test]
    fn test_close_writes_to_output_file() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "hello\n");
        record_step(
            &session_path,
            "world\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();

        let output_path = dir.path().join("output.json");
        run_close(session_path.clone(), true, Some(output_path.clone())).unwrap();

        // Session file deleted
        assert!(!session_path.exists());

        // Output file written with valid Toolpath document (no track state)
        let json = std::fs::read_to_string(&output_path).unwrap();
        assert!(!json.contains("\"track\""));
        assert!(!json.contains("buffer_cache"));
        let doc = v1::Document::from_json(&json).unwrap();
        match doc {
            v1::Document::Path(p) => {
                assert_eq!(p.path.id, "track-test-1");
                assert_eq!(p.path.head, "step-001");
                assert_eq!(p.steps.len(), 1);
                // No title/source set → no meta block
                assert!(p.meta.is_none());
            }
            _ => panic!("Expected Path"),
        }
    }

    #[test]
    fn test_close_nonexistent_session_errors() {
        let result = run_close(PathBuf::from("/nonexistent/session.json"), false, None);
        assert!(result.is_err());
    }

    // ── run_list ─────────────────────────────────────────────────────────

    #[test]
    fn test_list_finds_sessions() {
        let dir = TempDir::new().unwrap();

        // Create two session files (as valid Path documents)
        let path_doc_1 = v1::Path::new("track-20260101T000000-111", None, "none");
        let state_1 = TrackState {
            version: 1,
            file: "a.txt".to_string(),
            default_actor: "human:alice".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        save_session(
            &dir.path().join("track-20260101T000000-111.json"),
            &path_doc_1,
            &state_1,
        )
        .unwrap();

        let mut path_doc_2 = v1::Path::new("track-20260101T000000-222", None, "step-001");
        path_doc_2.steps.push(v1::Step::new(
            "step-001",
            "human:bob",
            "2026-01-01T00:01:00Z",
        ));
        let state_2 = TrackState {
            version: 1,
            file: "b.txt".to_string(),
            default_actor: "human:bob".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 1,
            created_at: "2026-01-01T00:00:01Z".to_string(),
        };
        save_session(
            &dir.path().join("track-20260101T000000-222.json"),
            &path_doc_2,
            &state_2,
        )
        .unwrap();

        // Non-track file — should be ignored
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();

        // run_list prints to stdout — call it and verify it doesn't error
        run_list(Some(dir.path().to_path_buf()), false).unwrap();
        run_list(Some(dir.path().to_path_buf()), true).unwrap();
    }

    #[test]
    fn test_list_empty_directory() {
        let dir = TempDir::new().unwrap();
        // Should not error, just print "No active tracking sessions."
        run_list(Some(dir.path().to_path_buf()), false).unwrap();
    }

    #[test]
    fn test_list_nonexistent_directory() {
        let result = run_list(Some(PathBuf::from("/nonexistent/dir")), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_ignores_corrupt_sessions() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("track-corrupt.json"), "not json").unwrap();
        // Should not error — corrupt files are silently skipped
        run_list(Some(dir.path().to_path_buf()), false).unwrap();
    }

    // ── Session is always a valid document ──────────────────────────────

    #[test]
    fn test_session_with_base_is_valid_document() {
        let dir = TempDir::new().unwrap();
        let base = v1::Base {
            uri: "github:org/repo".to_string(),
            ref_str: Some("abc123".to_string()),
        };
        let mut path_doc = v1::Path::new("track-test-doc", Some(base), "step-001");
        path_doc.steps.push(
            v1::Step::new("step-001", "human:alex", "2026-01-01T00:01:00Z")
                .with_raw_change("src/main.rs", "@@ changed"),
        );
        let state = TrackState {
            version: 1,
            file: "src/main.rs".to_string(),
            default_actor: "human:alex".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let session_path = dir.path().join("track-test-doc.json");
        save_session(&session_path, &path_doc, &state).unwrap();

        // Read raw file as Toolpath document — should work
        let data = std::fs::read_to_string(&session_path).unwrap();
        let doc = v1::Document::from_json(&data).unwrap();
        match &doc {
            v1::Document::Path(p) => {
                assert_eq!(p.path.id, "track-test-doc");
                assert_eq!(p.path.head, "step-001");
                assert_eq!(p.path.base.as_ref().unwrap().uri, "github:org/repo");
                assert_eq!(p.steps.len(), 1);
            }
            _ => panic!("Expected Path"),
        }

        // Load and export (track state stripped)
        let (exported, _) = load_session(&session_path).unwrap();
        assert!(exported.meta.is_none()); // no title/source → meta removed
        assert_eq!(exported.path.id, "track-test-doc");
        assert_eq!(exported.steps.len(), 1);

        // Roundtrip through JSON
        let doc = v1::Document::Path(exported);
        let json = doc.to_json_pretty().unwrap();
        let parsed = v1::Document::from_json(&json).unwrap();
        match parsed {
            v1::Document::Path(p) => assert_eq!(p.path.id, "track-test-doc"),
            _ => panic!("Expected Path"),
        }
    }

    #[test]
    fn test_session_no_base_exports_correctly() {
        let dir = TempDir::new().unwrap();
        let path_doc = v1::Path::new("track-no-base", None, "none");
        let state = TrackState {
            version: 1,
            file: "f.rs".to_string(),
            default_actor: "human:dev".to_string(),
            buffer_cache: HashMap::new(),
            seq_to_step: HashMap::new(),
            step_counter: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let session_path = dir.path().join("track-no-base.json");
        save_session(&session_path, &path_doc, &state).unwrap();

        let (exported, _) = load_session(&session_path).unwrap();
        assert!(exported.path.base.is_none());
        assert_eq!(exported.path.head, "none");
        assert!(exported.steps.is_empty());
    }

    #[test]
    fn test_multi_step_session_roundtrips_after_export() {
        let dir = TempDir::new().unwrap();
        let session_path = make_session(dir.path(), "original\n");

        // Build a multi-step DAG via record_step
        record_step(
            &session_path,
            "edit-1\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        record_step(
            &session_path,
            "edit-2\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();
        record_step(
            &session_path,
            "branch\n".to_string(),
            3,
            1,
            None,
            Some("2026-01-01T00:03:00Z".to_string()),
        )
        .unwrap();

        // Load = export-ready (track state already stripped)
        let (path_doc, _) = load_session(&session_path).unwrap();
        let doc = v1::Document::Path(path_doc.clone());

        // Serialize and parse back
        let json = doc.to_json_pretty().unwrap();
        let parsed = v1::Document::from_json(&json).unwrap();
        match parsed {
            v1::Document::Path(p) => {
                assert_eq!(p.steps.len(), 3);
                assert_eq!(p.path.head, "step-003");
                // Verify DAG structure preserved
                assert!(p.steps[0].step.parents.is_empty());
                assert_eq!(p.steps[1].step.parents, vec!["step-001"]);
                assert_eq!(p.steps[2].step.parents, vec!["step-001"]);
            }
            _ => panic!("Expected Path"),
        }

        // Dead ends still work after roundtrip
        let dead = v1::query::dead_ends(&path_doc.steps, &path_doc.path.head);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].step.id, "step-002");
    }

    // ── End-to-end flow ──────────────────────────────────────────────────

    #[test]
    fn test_full_editing_session_flow() {
        let dir = TempDir::new().unwrap();

        // 1. Init
        let init_path = init_session(InitConfig {
            content: "line 1\nline 2\nline 3\n".to_string(),
            file: "src/lib.rs".to_string(),
            actor: "human:alex".to_string(),
            actors: None,
            title: None,
            source: None,
            base_uri: Some("github:org/repo".to_string()),
            base_ref: Some("main".to_string()),
            session_dir: Some(dir.path().to_path_buf()),
        })
        .unwrap();

        // 2. First edit
        let r1 = record_step(
            &init_path,
            "line 1\nline 2 modified\nline 3\n".to_string(),
            1,
            0,
            None,
            Some("2026-01-01T00:01:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r1, StepResult::Created("step-001".to_string()));

        // 3. Second edit
        let r2 = record_step(
            &init_path,
            "line 1\nline 2 modified\nline 3\nline 4\n".to_string(),
            2,
            1,
            None,
            Some("2026-01-01T00:02:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r2, StepResult::Created("step-002".to_string()));

        // 4. Note
        run_note(init_path.clone(), "Add line 4".to_string()).unwrap();

        // 5. Undo back to seq 1 and make different edit (branch)
        let r3 = record_step(
            &init_path,
            "line 1\nline 2 modified\nline 3\nline FOUR\n".to_string(),
            3,
            1,
            None,
            Some("2026-01-01T00:03:00Z".to_string()),
        )
        .unwrap();
        assert_eq!(r3, StepResult::Created("step-003".to_string()));

        // Verify the session is a valid Toolpath document mid-session
        let data = std::fs::read_to_string(&init_path).unwrap();
        let mid_doc = v1::Document::from_json(&data).unwrap();
        match &mid_doc {
            v1::Document::Path(p) => {
                assert_eq!(p.steps.len(), 3);
                assert_eq!(p.path.head, "step-003");
                // Can run queries on the live session
                let dead = v1::query::dead_ends(&p.steps, &p.path.head);
                assert_eq!(dead.len(), 1);
                assert_eq!(dead[0].step.id, "step-002");
            }
            _ => panic!("Expected Path"),
        }

        // 6. Close with output file
        let output = dir.path().join("result.json");
        run_close(init_path.clone(), true, Some(output.clone())).unwrap();

        // Session file deleted
        assert!(!init_path.exists());

        // Parse and verify the output document
        let json = std::fs::read_to_string(&output).unwrap();
        assert!(!json.contains("buffer_cache"));
        assert!(!json.contains("seq_to_step"));
        let doc = v1::Document::from_json(&json).unwrap();
        match doc {
            v1::Document::Path(p) => {
                assert_eq!(p.steps.len(), 3);
                assert_eq!(p.path.head, "step-003");
                assert!(p.path.base.is_some());

                // step-001: root
                assert!(p.steps[0].step.parents.is_empty());
                // step-002: child of step-001
                assert_eq!(p.steps[1].step.parents, vec!["step-001"]);
                // step-003: also child of step-001 (branch!)
                assert_eq!(p.steps[2].step.parents, vec!["step-001"]);

                // Note was set on step-002 (it was head when we noted)
                let intent = p.steps[1].meta.as_ref().and_then(|m| m.intent.as_ref());
                assert_eq!(intent, Some(&"Add line 4".to_string()));

                // Dead ends: step-002 is not on ancestry of step-003
                let dead = v1::query::dead_ends(&p.steps, &p.path.head);
                assert_eq!(dead.len(), 1);
                assert_eq!(dead[0].step.id, "step-002");

                // No title/source set → no meta block
                assert!(p.meta.is_none());
            }
            _ => panic!("Expected Path"),
        }
    }
}
