//! One `path sync` pass over a single session: replay what is staged,
//! or else reconcile with the server, segment the live derive, and
//! stage, send, and acknowledge at most one new operation.
//!
//! The pass never sends a mutation it has not first written to the
//! journal, and never records an upload it has not seen acknowledged.

use super::activity::Activity;
use super::api::{ApiFailure, Applied, GraphMeta, GraphState, SyncApi};
use super::journal::{self, OperationKind, PendingOperation};
use super::segment::{SegmentKind, Segmentation, segment};
use super::sources::Stamp;
use super::state::{self, CurrentGraph, SessionState};
use crate::artifact::ArtifactType;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::path::Path;
use toolpath::v1::Graph;

/// Where one session's uploads go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Destination {
    /// `owner/name`.
    pub(crate) repo: String,
    pub(crate) base_url: String,
}

impl Destination {
    /// `<server>/u/<owner>/<name>`, the key upload records match on.
    pub(crate) fn repo_url(&self) -> String {
        format!("{}/u/{}", self.base_url.trim_end_matches('/'), self.repo)
    }
}

/// One session as the pass sees it.
#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) harness: ArtifactType,
    pub(crate) id: String,
    /// Provider project key for path-keyed harnesses.
    pub(crate) project: Option<String>,
    /// Directory the manifest recorded for it, for status lines.
    pub(crate) path: Option<String>,
    /// Stamp of the derive the cache holds.
    pub(crate) stamp: Stamp,
}

/// What happened to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Created(String),
    Updated(String),
    Continued(String),
    Frozen(String),
    /// Nothing to send: unchanged and active, or frozen with no new steps.
    Unchanged,
    /// What a dry run would have done.
    Planned(String),
    /// A staged operation could not be settled this pass.
    Pending(String),
    /// Reported and left alone: conflicts, regressions, unsupported shapes.
    Failed(String),
}

impl Outcome {
    pub(crate) fn is_failure(&self) -> bool {
        matches!(self, Outcome::Failed(_) | Outcome::Pending(_))
    }
}

pub(crate) struct PassContext<'a> {
    pub(crate) config_dir: &'a Path,
    pub(crate) api: &'a dyn SyncApi,
    pub(crate) now: DateTime<Utc>,
    pub(crate) dry_run: bool,
}

/// Run the pass for one session. `live` loads the derived document the
/// cache holds for `session.stamp`; `restat` re-reads the source stamp
/// right before a freeze is staged.
pub(crate) fn sync_session(
    ctx: &PassContext<'_>,
    session: &Session,
    destination: &Destination,
    live: &dyn Fn() -> Result<Graph>,
    restat: &dyn Fn() -> Stamp,
) -> Result<Outcome> {
    let repo_url = destination.repo_url();
    let mut record = super::load_manifest(ctx.config_dir)?
        .get(session.harness.name())
        .and_then(|records| records.get(&session.id))
        .cloned();
    let mut activity = match record.as_mut().and_then(|r| r.activity.take()) {
        Some(mut activity) => {
            activity.observe(ctx.now, session.stamp);
            activity
        }
        None => Activity::first(ctx.now, session.stamp),
    };
    if !ctx.dry_run {
        super::record_activity(
            ctx.config_dir,
            session.harness,
            &session.id,
            session.path.as_deref(),
            activity.clone(),
        )?;
    }
    let mut state = state::load(ctx.config_dir, session.harness, &session.id)?;
    let acknowledged: Option<Stamp> = record
        .as_ref()
        .and_then(|r| {
            r.uploads
                .iter()
                .find(|u| u.url.starts_with(&format!("{repo_url}/graphs/")))
        })
        .map(|u| (u.modified, u.size));

    // 1. Settle what an earlier pass staged for this destination.
    for op in journal::list(ctx.config_dir)? {
        if op.harness != session.harness || op.session != session.id {
            continue;
        }
        if op.base_url.trim_end_matches('/') != destination.base_url.trim_end_matches('/')
            || op.repo != destination.repo
        {
            return Ok(Outcome::Pending(format!(
                "operation {} is staged for {}/u/{}, not the configured destination",
                op.key, op.base_url, op.repo
            )));
        }
        if ctx.dry_run {
            return Ok(Outcome::Planned(format!(
                "would replay staged {}",
                describe(&op.kind)
            )));
        }
        let body = journal::body(ctx.config_dir, &op)?;
        match ctx.api.execute(&op, &body) {
            Ok(applied) => {
                return acknowledge(ctx, session, destination, &op, applied, &mut state);
            }
            Err(e) if e.is_definitive() => {
                journal::retire(ctx.config_dir, &op.key)?;
                match e {
                    ApiFailure::Frozen | ApiFailure::GenerationConflict => {}
                    other => {
                        return Ok(Outcome::Failed(format!(
                            "{} rejected: {other}",
                            describe(&op.kind)
                        )));
                    }
                }
            }
            Err(e) => return Ok(Outcome::Pending(format!("{}: {e}", describe(&op.kind)))),
        }
    }

    // 2. Adopt a graph `share` uploaded before sync tracked this session.
    if state.current.is_none()
        && state.frozen.is_none()
        && let Some(upload) = record.as_ref().and_then(|r| {
            r.uploads
                .iter()
                .find(|u| u.url.starts_with(&format!("{repo_url}/graphs/")))
        })
    {
        match adopt(ctx, destination, &upload.graph_id, &upload.url, &mut state) {
            Ok(()) => {}
            Err(ApiFailure::NotFound) => {
                return Ok(Outcome::Failed(format!(
                    "graph {} was deleted on the server; re-share it explicitly",
                    upload.url
                )));
            }
            Err(e) => return Ok(Outcome::Failed(format!("reading {}: {e}", upload.url))),
        }
        if !ctx.dry_run {
            state::save(ctx.config_dir, session.harness, &session.id, &state)?;
        }
    }

    // 3. Reconcile the current graph with the server.
    if let Some(current) = state.current.clone() {
        let meta = match ctx.api.meta(&destination.repo, &current.graph_id) {
            Ok(meta) => meta,
            Err(ApiFailure::NotFound) => {
                return Ok(Outcome::Failed(format!(
                    "graph {} was deleted on the server; re-share it explicitly",
                    current.url
                )));
            }
            Err(e) => return Ok(Outcome::Failed(format!("reading {}: {e}", current.url))),
        };
        if meta.generation != current.generation || meta.state != current.state {
            if !owned_content_matches(ctx.api, destination, &current, &meta)? {
                return Ok(Outcome::Failed(format!(
                    "{} changed on the server since it was last acknowledged; not overwriting",
                    current.url
                )));
            }
            let current = state.current.as_mut().expect("checked above");
            current.generation = meta.generation;
            current.state = meta.state;
            if meta.state == GraphState::Frozen {
                let path_id = meta.paths.first().map(|p| p.id.clone()).unwrap_or_default();
                state.freeze_current(&path_id);
            }
            if !ctx.dry_run {
                state::save(ctx.config_dir, session.harness, &session.id, &state)?;
            }
        }
    }

    // 4. Decide.
    let changed =
        acknowledged != Some(session.stamp) || state.current.is_none() && state.frozen.is_none();
    let idle = activity.idle_at(ctx.now);
    let current_mutable = state
        .current
        .as_ref()
        .is_some_and(|c| c.state == GraphState::Mutable);
    if !changed {
        if current_mutable && idle {
            return freeze(ctx, session, destination, &mut activity, &mut state, restat);
        }
        return Ok(Outcome::Unchanged);
    }
    let doc = live()?;
    let boundary = state.frozen.as_ref().map(|f| f.boundary());
    let baseline: Option<HashSet<String>> = state
        .current
        .as_ref()
        .filter(|c| c.state == GraphState::Mutable)
        .map(|c| c.owned_ids.iter().cloned().collect());
    let (kind, doc, owned_ids) = match segment(&doc, boundary.as_ref(), baseline.as_ref()) {
        Segmentation::NoNewSteps => {
            if current_mutable && idle {
                return freeze(ctx, session, destination, &mut activity, &mut state, restat);
            }
            if !ctx.dry_run && current_mutable {
                acknowledge_stamp(ctx, session, &repo_url, &state)?;
            }
            return Ok(Outcome::Unchanged);
        }
        Segmentation::Unsupported(m) => return Ok(Outcome::Failed(format!("unsupported: {m}"))),
        Segmentation::SourceRegression { missing } => {
            return Ok(Outcome::Failed(format!(
                "source lost {} acknowledged step(s) ({}); run `path share --force` to replace the graph",
                missing.len(),
                missing
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Segmentation::Document {
            kind,
            doc,
            owned_ids,
        } => (kind, doc, owned_ids),
    };
    let path = doc
        .single_path()
        .expect("segment returns single-path documents");
    let head = path.path.head.clone();
    let path_id = path.path.id.clone();
    let base_from = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.from.as_ref())
        .map(|r| r.to_string());
    let document = serde_json::to_value(&doc)?;
    let freeze_after = idle && restat() == session.stamp;
    if idle && !freeze_after {
        activity.observe(ctx.now, restat());
    }
    let (kind, body) = match kind {
        SegmentKind::Independent => (
            OperationKind::Create,
            serde_json::json!({ "document": document, "freeze_after": freeze_after }),
        ),
        SegmentKind::OwnedUpdate => {
            let current = state
                .current
                .as_ref()
                .expect("owned update has a current graph");
            (
                OperationKind::Update {
                    graph_id: current.graph_id.clone(),
                },
                serde_json::json!({
                    "document": document,
                    "expected_generation": current.generation,
                    "freeze_after": freeze_after,
                }),
            )
        }
        SegmentKind::Continuation => {
            let frozen = state
                .frozen
                .as_ref()
                .expect("continuation has a frozen boundary");
            (
                OperationKind::Continuation {
                    source_graph_id: frozen.graph_id.clone(),
                    source_path: frozen.path_id.clone(),
                },
                serde_json::json!({
                    "document": document,
                    "source_path": frozen.path_id,
                    "freeze_after": freeze_after,
                }),
            )
        }
    };
    let body = serde_json::to_vec(&body)?;
    let op = PendingOperation {
        key: uuid::Uuid::new_v4().to_string(),
        created_at: ctx.now,
        base_url: destination.base_url.clone(),
        repo: destination.repo.clone(),
        expected_generation: state
            .current
            .as_ref()
            .filter(|_| matches!(kind, OperationKind::Update { .. }))
            .map(|c| c.generation),
        kind,
        freeze_after,
        harness: session.harness,
        session: session.id.clone(),
        project: session.project.clone(),
        modified: session.stamp.0,
        size: session.stamp.1,
        owned_ids,
        head: Some(head),
        base_from,
        body_sha256: journal::sha256_hex(&body),
    };
    let _ = path_id;
    if ctx.dry_run {
        return Ok(Outcome::Planned(format!(
            "would {}{}",
            describe(&op.kind),
            if freeze_after { " and freeze" } else { "" }
        )));
    }
    journal::stage(ctx.config_dir, &op, &body)?;
    send(ctx, session, destination, &op, &body, &mut state)
}

fn describe(kind: &OperationKind) -> String {
    match kind {
        OperationKind::Create => "create a graph".into(),
        OperationKind::Update { graph_id } => format!("update graph {graph_id}"),
        OperationKind::Freeze { graph_id } => format!("freeze graph {graph_id}"),
        OperationKind::Continuation {
            source_graph_id, ..
        } => format!("continue graph {source_graph_id}"),
    }
}

/// Stage and send a freeze for the current mutable graph, after one
/// last look at the source.
fn freeze(
    ctx: &PassContext<'_>,
    session: &Session,
    destination: &Destination,
    activity: &mut Activity,
    state: &mut SessionState,
    restat: &dyn Fn() -> Stamp,
) -> Result<Outcome> {
    let fresh = restat();
    if fresh != session.stamp {
        activity.observe(ctx.now, fresh);
        if !ctx.dry_run {
            super::record_activity(
                ctx.config_dir,
                session.harness,
                &session.id,
                session.path.as_deref(),
                activity.clone(),
            )?;
        }
        return Ok(Outcome::Unchanged);
    }
    let current = state
        .current
        .as_ref()
        .expect("freeze needs a current graph");
    if ctx.dry_run {
        return Ok(Outcome::Planned(format!(
            "would freeze graph {}",
            current.graph_id
        )));
    }
    let body =
        serde_json::to_vec(&serde_json::json!({ "expected_generation": current.generation }))?;
    let op = PendingOperation {
        key: uuid::Uuid::new_v4().to_string(),
        created_at: ctx.now,
        base_url: destination.base_url.clone(),
        repo: destination.repo.clone(),
        kind: OperationKind::Freeze {
            graph_id: current.graph_id.clone(),
        },
        expected_generation: Some(current.generation),
        freeze_after: true,
        harness: session.harness,
        session: session.id.clone(),
        project: session.project.clone(),
        modified: session.stamp.0,
        size: session.stamp.1,
        owned_ids: Vec::new(),
        head: Some(current.head.clone()),
        base_from: current.base_from.clone(),
        body_sha256: journal::sha256_hex(&body),
    };
    journal::stage(ctx.config_dir, &op, &body)?;
    send(ctx, session, destination, &op, &body, state)
}

fn send(
    ctx: &PassContext<'_>,
    session: &Session,
    destination: &Destination,
    op: &PendingOperation,
    body: &[u8],
    state: &mut SessionState,
) -> Result<Outcome> {
    match ctx.api.execute(op, body) {
        Ok(applied) => acknowledge(ctx, session, destination, op, applied, state),
        Err(e) if e.is_definitive() => {
            journal::retire(ctx.config_dir, &op.key)?;
            Ok(Outcome::Failed(format!(
                "{} rejected: {e}",
                describe(&op.kind)
            )))
        }
        Err(e) => Ok(Outcome::Pending(format!("{}: {e}", describe(&op.kind)))),
    }
}

/// Record a settled operation: session state, then the manifest, then
/// the journal entry, so a crash in between leaves a replayable
/// operation rather than a forgotten upload.
fn acknowledge(
    ctx: &PassContext<'_>,
    session: &Session,
    destination: &Destination,
    op: &PendingOperation,
    applied: Applied,
    state: &mut SessionState,
) -> Result<Outcome> {
    let meta = applied.meta;
    let path_id = meta.paths.first().map(|p| p.id.clone()).unwrap_or_default();
    let repo_url = destination.repo_url();
    let outcome = match &op.kind {
        OperationKind::Freeze { .. } => {
            if let Some(current) = state.current.as_mut() {
                current.generation = meta.generation;
                current.state = meta.state;
            }
            state.freeze_current(&path_id);
            Outcome::Frozen(meta.url.clone())
        }
        OperationKind::Continuation { .. } if !applied.created => {
            // Someone else created the main-line child; adopt it only
            // if what it owns is a prefix of what we were about to send.
            let stored = ctx
                .api
                .stored_document(&destination.repo, &meta.id)
                .map_err(|e| anyhow::anyhow!("reading existing continuation {}: {e}", meta.url))?;
            let existing: Vec<String> = stored
                .single_path()
                .map(|p| p.steps.iter().map(|s| s.step.id.clone()).collect())
                .unwrap_or_default();
            let ours: HashSet<&str> = op.owned_ids.iter().map(String::as_str).collect();
            if !existing.iter().all(|id| ours.contains(id.as_str())) {
                journal::retire(ctx.config_dir, &op.key)?;
                return Ok(Outcome::Failed(format!(
                    "{} already continues this session with steps this source does not have",
                    meta.url
                )));
            }
            let head = stored
                .single_path()
                .map(|p| p.path.head.clone())
                .unwrap_or_default();
            state.current = Some(CurrentGraph {
                graph_id: meta.id.clone(),
                url: meta.url.clone(),
                repo_url: repo_url.clone(),
                state: meta.state,
                generation: meta.generation,
                owned_ids: existing,
                head,
                base_from: op.base_from.clone(),
            });
            if meta.state == GraphState::Frozen {
                state.freeze_current(&path_id);
            }
            Outcome::Continued(meta.url.clone())
        }
        _ => {
            state.current = Some(CurrentGraph {
                graph_id: meta.id.clone(),
                url: meta.url.clone(),
                repo_url: repo_url.clone(),
                state: meta.state,
                generation: meta.generation,
                owned_ids: op.owned_ids.clone(),
                head: op.head.clone().unwrap_or_default(),
                base_from: op.base_from.clone(),
            });
            if meta.state == GraphState::Frozen {
                state.freeze_current(&path_id);
            }
            match &op.kind {
                OperationKind::Create => Outcome::Created(meta.url.clone()),
                OperationKind::Update { .. } => Outcome::Updated(meta.url.clone()),
                _ => Outcome::Continued(meta.url.clone()),
            }
        }
    };
    state::save(ctx.config_dir, session.harness, &session.id, state)?;
    super::record_upload(
        ctx.config_dir,
        session.harness,
        &session.id,
        session.project.as_deref(),
        &repo_url,
        super::UploadRecord {
            graph_id: meta.id.clone(),
            url: meta.url.clone(),
            modified: op.modified,
            size: op.size,
            uploaded_at: ctx.now,
        },
    )?;
    journal::retire(ctx.config_dir, &op.key)?;
    Ok(outcome)
}

/// A changed source whose derive added nothing ownable still counts as
/// acknowledged at this stamp, so the next pass can skip it.
fn acknowledge_stamp(
    ctx: &PassContext<'_>,
    session: &Session,
    repo_url: &str,
    state: &SessionState,
) -> Result<()> {
    let Some(current) = state.current.as_ref() else {
        return Ok(());
    };
    super::record_upload(
        ctx.config_dir,
        session.harness,
        &session.id,
        session.project.as_deref(),
        repo_url,
        super::UploadRecord {
            graph_id: current.graph_id.clone(),
            url: current.url.clone(),
            modified: session.stamp.0,
            size: session.stamp.1,
            uploaded_at: ctx.now,
        },
    )
}

/// Take over a graph that `share` created: what it owns comes from the
/// stored document, its state from the server.
fn adopt(
    ctx: &PassContext<'_>,
    destination: &Destination,
    graph_id: &str,
    url: &str,
    state: &mut SessionState,
) -> Result<(), ApiFailure> {
    let meta = ctx.api.meta(&destination.repo, graph_id)?;
    let stored = ctx.api.stored_document(&destination.repo, graph_id)?;
    let Some(path) = stored.single_path() else {
        return Err(ApiFailure::Rejected {
            code: "multi_path".into(),
            message: format!(
                "{url} has {} paths; sync tracks single-path graphs",
                stored.paths.len()
            ),
        });
    };
    state.current = Some(CurrentGraph {
        graph_id: meta.id.clone(),
        url: meta.url.clone(),
        repo_url: destination.repo_url(),
        state: meta.state,
        generation: meta.generation,
        owned_ids: path.steps.iter().map(|s| s.step.id.clone()).collect(),
        head: path.path.head.clone(),
        base_from: path
            .path
            .base
            .as_ref()
            .and_then(|b| b.from.as_ref())
            .map(|r| r.to_string()),
    });
    if meta.state == GraphState::Frozen {
        state.freeze_current(&path.path.id);
    }
    Ok(())
}

/// Whether the server's owned content is still what we acknowledged.
/// Head and count are a cheap first check; the stored document settles it.
fn owned_content_matches(
    api: &dyn SyncApi,
    destination: &Destination,
    current: &CurrentGraph,
    meta: &GraphMeta,
) -> Result<bool> {
    let Some(path) = meta.paths.first() else {
        return Ok(current.owned_ids.is_empty());
    };
    if path.head.as_deref() != Some(current.head.as_str())
        || path.step_count != current.owned_ids.len() as u64
    {
        return Ok(false);
    }
    let stored = api
        .stored_document(&destination.repo, &current.graph_id)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", current.url))?;
    let Some(stored_path) = stored.single_path() else {
        return Ok(false);
    };
    let ours: HashSet<&str> = current.owned_ids.iter().map(String::as_str).collect();
    Ok(stored_path.steps.len() == ours.len()
        && stored_path
            .steps
            .iter()
            .all(|s| ours.contains(s.step.id.as_str())))
}

#[cfg(test)]
mod tests {
    use super::super::api::{Lineage, MetaBase, MetaPath};
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use toolpath::v1::{Base, Path as TpPath, Step};

    #[derive(Debug, Clone)]
    struct FakeGraph {
        state: GraphState,
        generation: i64,
        path_id: String,
        owned_ids: Vec<String>,
        head: String,
        base_from: Option<String>,
        continuation: Option<String>,
        doc: Graph,
    }

    #[derive(Default)]
    struct Fake {
        graphs: RefCell<HashMap<String, FakeGraph>>,
        replies: RefCell<HashMap<String, (i64, Applied)>>,
        next: RefCell<u32>,
        fail_next: RefCell<Option<ApiFailure>>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn meta_of(&self, id: &str, g: &FakeGraph) -> GraphMeta {
            GraphMeta {
                id: id.into(),
                url: format!("https://h/u/me/stash/graphs/{id}"),
                state: g.state,
                generation: g.generation,
                base: g.base_from.as_ref().map(|from| MetaBase {
                    from: from.clone(),
                    source_graph_id: String::new(),
                    source_path_id: g.path_id.clone(),
                    source_step_id: String::new(),
                }),
                lineage: Lineage {
                    continuation_graph_id: g.continuation.clone(),
                    ..Default::default()
                },
                paths: vec![MetaPath {
                    id: g.path_id.clone(),
                    server_id: "srv".into(),
                    head: Some(g.head.clone()),
                    step_count: g.owned_ids.len() as u64,
                }],
            }
        }
        fn graph(&self, id: &str) -> FakeGraph {
            self.graphs.borrow()[id].clone()
        }
    }

    fn ids_of(doc: &Graph) -> (String, Vec<String>, String, Option<String>) {
        let p = doc.single_path().unwrap();
        (
            p.path.id.clone(),
            p.steps.iter().map(|s| s.step.id.clone()).collect(),
            p.path.head.clone(),
            p.path
                .base
                .as_ref()
                .and_then(|b| b.from.as_ref())
                .map(|r| r.to_string()),
        )
    }

    impl SyncApi for Fake {
        fn meta(&self, _repo: &str, graph_id: &str) -> Result<GraphMeta, ApiFailure> {
            self.calls.borrow_mut().push(format!("meta {graph_id}"));
            let graphs = self.graphs.borrow();
            graphs
                .get(graph_id)
                .map(|g| self.meta_of(graph_id, g))
                .ok_or(ApiFailure::NotFound)
        }
        fn execute(&self, op: &PendingOperation, body: &[u8]) -> Result<Applied, ApiFailure> {
            self.calls
                .borrow_mut()
                .push(format!("execute {}", describe(&op.kind)));
            if let Some(f) = self.fail_next.borrow_mut().take() {
                return Err(f);
            }
            if let Some((_, replay)) = self.replies.borrow().get(&op.key) {
                return Ok(replay.clone());
            }
            let body: serde_json::Value = serde_json::from_slice(body).unwrap();
            let freeze_after = body["freeze_after"].as_bool().unwrap_or(false);
            let mut graphs = self.graphs.borrow_mut();
            let applied = match &op.kind {
                OperationKind::Create => {
                    let doc: Graph = serde_json::from_value(body["document"].clone()).unwrap();
                    let (path_id, owned_ids, head, base_from) = ids_of(&doc);
                    *self.next.borrow_mut() += 1;
                    let id = format!("g{}", self.next.borrow());
                    let g = FakeGraph {
                        state: if freeze_after {
                            GraphState::Frozen
                        } else {
                            GraphState::Mutable
                        },
                        generation: if freeze_after { 1 } else { 0 },
                        path_id,
                        owned_ids,
                        head,
                        base_from,
                        continuation: None,
                        doc,
                    };
                    let meta = self.meta_of(&id, &g);
                    graphs.insert(id, g);
                    Applied {
                        meta,
                        created: true,
                    }
                }
                OperationKind::Update { graph_id } => {
                    let g = graphs.get_mut(graph_id).ok_or(ApiFailure::NotFound)?;
                    if g.state == GraphState::Frozen {
                        return Err(ApiFailure::Frozen);
                    }
                    if body["expected_generation"].as_i64() != Some(g.generation) {
                        return Err(ApiFailure::GenerationConflict);
                    }
                    let doc: Graph = serde_json::from_value(body["document"].clone()).unwrap();
                    let (_, owned_ids, head, base_from) = ids_of(&doc);
                    if base_from != g.base_from {
                        return Err(ApiFailure::Rejected {
                            code: "base_retargeted".into(),
                            message: String::new(),
                        });
                    }
                    if owned_ids != g.owned_ids || head != g.head {
                        g.generation += 1;
                    }
                    g.owned_ids = owned_ids;
                    g.head = head;
                    g.doc = doc;
                    if freeze_after {
                        g.state = GraphState::Frozen;
                        g.generation += 1;
                    }
                    let g = g.clone();
                    Applied {
                        meta: self.meta_of(graph_id, &g),
                        created: false,
                    }
                }
                OperationKind::Freeze { graph_id } => {
                    let g = graphs.get_mut(graph_id).ok_or(ApiFailure::NotFound)?;
                    if g.state == GraphState::Mutable {
                        if body["expected_generation"].as_i64() != Some(g.generation) {
                            return Err(ApiFailure::GenerationConflict);
                        }
                        g.state = GraphState::Frozen;
                        g.generation += 1;
                    }
                    let g = g.clone();
                    Applied {
                        meta: self.meta_of(graph_id, &g),
                        created: false,
                    }
                }
                OperationKind::Continuation {
                    source_graph_id,
                    source_path,
                } => {
                    let source = graphs
                        .get(source_graph_id)
                        .ok_or(ApiFailure::NotFound)?
                        .clone();
                    if source.state == GraphState::Mutable {
                        return Err(ApiFailure::Rejected {
                            code: "source_not_frozen".into(),
                            message: String::new(),
                        });
                    }
                    if let Some(existing) = &source.continuation {
                        let g = graphs[existing].clone();
                        return Ok(Applied {
                            meta: self.meta_of(existing, &g),
                            created: false,
                        });
                    }
                    let doc: Graph = serde_json::from_value(body["document"].clone()).unwrap();
                    let (path_id, owned_ids, head, base_from) = ids_of(&doc);
                    let expected = format!(
                        "https://h/u/me/stash/graphs/{source_graph_id}#{source_path}/{}",
                        source.head
                    );
                    if base_from.as_deref() != Some(expected.as_str()) || owned_ids.is_empty() {
                        return Err(ApiFailure::Rejected {
                            code: "invalid_base".into(),
                            message: format!("{base_from:?} != {expected}"),
                        });
                    }
                    *self.next.borrow_mut() += 1;
                    let id = format!("g{}", self.next.borrow());
                    let g = FakeGraph {
                        state: if freeze_after {
                            GraphState::Frozen
                        } else {
                            GraphState::Mutable
                        },
                        generation: if freeze_after { 1 } else { 0 },
                        path_id,
                        owned_ids,
                        head,
                        base_from,
                        continuation: None,
                        doc,
                    };
                    let meta = self.meta_of(&id, &g);
                    graphs.get_mut(source_graph_id).unwrap().continuation = Some(id.clone());
                    graphs.insert(id, g);
                    Applied {
                        meta,
                        created: true,
                    }
                }
            };
            self.replies
                .borrow_mut()
                .insert(op.key.clone(), (0, applied.clone()));
            Ok(applied)
        }
        fn stored_document(&self, _repo: &str, graph_id: &str) -> Result<Graph, ApiFailure> {
            self.calls.borrow_mut().push(format!("stored {graph_id}"));
            self.graphs
                .borrow()
                .get(graph_id)
                .map(|g| g.doc.clone())
                .ok_or(ApiFailure::NotFound)
        }
    }

    fn step(id: &str, parents: &[&str]) -> Step {
        let mut s = Step::new(id, "agent:test", "2026-09-10T00:00:00Z");
        s.step.parents = parents.iter().map(|p| p.to_string()).collect();
        s
    }

    fn chain(ids: &[&str]) -> Graph {
        let mut path = TpPath::new(
            "p",
            Some(Base::vcs("github:o/r", "abc")),
            ids[ids.len() - 1],
        );
        let mut prev: Option<&str> = None;
        for id in ids {
            path.steps.push(step(
                id,
                prev.map(|p| vec![p]).unwrap_or_default().as_slice(),
            ));
            prev = Some(id);
        }
        Graph::from_path(path)
    }

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }
    const H: i64 = 3600;

    struct Harness {
        dir: tempfile::TempDir,
        api: Fake,
        dest: Destination,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                dir: tempfile::TempDir::new().unwrap(),
                api: Fake::default(),
                dest: Destination {
                    repo: "me/stash".into(),
                    base_url: "https://h".into(),
                },
            }
        }
        fn run(&self, now: DateTime<Utc>, stamp_secs: i64, doc: Graph, dry_run: bool) -> Outcome {
            self.run_with(now, stamp_secs, doc, dry_run, stamp_secs)
        }
        fn run_with(
            &self,
            now: DateTime<Utc>,
            stamp_secs: i64,
            doc: Graph,
            dry_run: bool,
            restat_secs: i64,
        ) -> Outcome {
            let ctx = PassContext {
                config_dir: self.dir.path(),
                api: &self.api,
                now,
                dry_run,
            };
            let session = Session {
                harness: ArtifactType::Codex,
                id: "s1".into(),
                project: None,
                path: Some("/work".into()),
                stamp: (Some(t(stamp_secs)), Some(stamp_secs as u64)),
            };
            sync_session(&ctx, &session, &self.dest, &|| Ok(doc.clone()), &|| {
                (Some(t(restat_secs)), Some(restat_secs as u64))
            })
            .unwrap()
        }
        fn state(&self) -> SessionState {
            state::load(self.dir.path(), ArtifactType::Codex, "s1").unwrap()
        }
        fn pending(&self) -> usize {
            journal::list(self.dir.path()).unwrap().len()
        }
    }

    #[test]
    fn a_new_active_session_is_created_mutable_and_then_left_alone() {
        let h = Harness::new();
        assert!(matches!(
            h.run(t(0), 0, chain(&["a", "b"]), false),
            Outcome::Created(_)
        ));
        let s = h.state();
        let current = s.current.unwrap();
        assert_eq!(current.state, GraphState::Mutable);
        assert_eq!(current.owned_ids, ["a", "b"]);
        assert_eq!(h.pending(), 0);
        // Same stamp, still active: nothing to send, no metadata drift.
        assert_eq!(
            h.run(t(H), 0, chain(&["a", "b"]), false),
            Outcome::Unchanged
        );
        assert_eq!(h.api.graph("g1").generation, 0);
    }

    #[test]
    fn a_changed_session_is_updated_in_place() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        assert!(matches!(
            h.run(t(60), 60, chain(&["a", "b"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.api.graph("g1").owned_ids, ["a", "b"]);
        assert_eq!(h.state().current.unwrap().generation, 1);
        assert_eq!(h.state().current.unwrap().owned_ids, ["a", "b"]);
    }

    #[test]
    fn two_idle_hours_freeze_without_re_uploading_and_then_new_steps_continue() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a", "b"]), false);
        assert_eq!(
            h.run(t(2 * H - 1), 0, chain(&["a", "b"]), false),
            Outcome::Unchanged
        );
        assert!(matches!(
            h.run(t(2 * H), 0, chain(&["a", "b"]), false),
            Outcome::Frozen(_)
        ));
        assert_eq!(h.api.graph("g1").state, GraphState::Frozen);
        let s = h.state();
        assert!(s.current.is_none());
        assert_eq!(s.frozen.as_ref().unwrap().step_ids, ["a", "b"]);
        // Still idle, still frozen: no new graph.
        assert_eq!(
            h.run(t(3 * H), 0, chain(&["a", "b"]), false),
            Outcome::Unchanged
        );
        // Late change to a frozen step alone is ignored.
        let mut edited = chain(&["a", "b"]);
        edited.single_path_mut_steps()[0].step.actor = "human:late".into();
        assert_eq!(h.run(t(3 * H + 1), 1, edited, false), Outcome::Unchanged);
        assert_eq!(h.api.graphs.borrow().len(), 1);
        // Resumed work continues from the frozen head.
        assert!(matches!(
            h.run(t(4 * H), 2, chain(&["a", "b", "c"]), false),
            Outcome::Continued(_)
        ));
        let g2 = h.api.graph("g2");
        assert_eq!(g2.owned_ids, ["c"]);
        assert_eq!(
            g2.base_from.as_deref(),
            Some("https://h/u/me/stash/graphs/g1#p/b")
        );
        assert_eq!(g2.state, GraphState::Mutable);
        assert_eq!(h.state().current.unwrap().owned_ids, ["c"]);
        // Then a further step updates the continuation, not the frozen base.
        assert!(matches!(
            h.run(t(4 * H + 60), 3, chain(&["a", "b", "c", "d"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.api.graph("g2").owned_ids, ["c", "d"]);
        assert_eq!(h.api.graph("g1").owned_ids, ["a", "b"]);
    }

    #[test]
    fn an_old_session_first_seen_idle_is_created_frozen() {
        let h = Harness::new();
        let out = h.run(t(3 * H), 0, chain(&["a"]), false);
        assert!(matches!(out, Outcome::Created(_)));
        assert_eq!(h.api.graph("g1").state, GraphState::Frozen);
        let s = h.state();
        assert!(s.current.is_none());
        assert_eq!(s.frozen.unwrap().head, "a");
    }

    #[test]
    fn activity_between_the_stamp_and_the_freeze_defers_it() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        assert_eq!(
            h.run_with(t(3 * H), 0, chain(&["a"]), false, 5),
            Outcome::Unchanged
        );
        assert_eq!(h.api.graph("g1").state, GraphState::Mutable);
        assert!(!h.api.calls.borrow().iter().any(|c| c.contains("freeze")));
    }

    #[test]
    fn dry_run_derives_and_plans_but_never_writes() {
        let h = Harness::new();
        assert!(
            matches!(h.run(t(0), 0, chain(&["a"]), true), Outcome::Planned(ref m) if m.contains("create"))
        );
        assert!(h.api.graphs.borrow().is_empty());
        assert_eq!(h.pending(), 0);
        assert_eq!(h.state(), SessionState::default());
        assert!(
            super::super::load_manifest(h.dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_lost_response_is_replayed_with_the_same_key_and_never_duplicates() {
        let h = Harness::new();
        *h.api.fail_next.borrow_mut() = Some(ApiFailure::Ambiguous("timeout".into()));
        // The fake still applied nothing on that call, so the replay creates once.
        assert!(matches!(
            h.run(t(0), 0, chain(&["a"]), false),
            Outcome::Pending(_)
        ));
        assert_eq!(h.pending(), 1);
        let key = journal::list(h.dir.path()).unwrap()[0].key.clone();
        assert!(matches!(
            h.run(t(60), 0, chain(&["a"]), false),
            Outcome::Created(_)
        ));
        assert_eq!(h.pending(), 0);
        assert_eq!(h.api.graphs.borrow().len(), 1);
        assert!(h.api.replies.borrow().contains_key(&key));
        // A crash after the server applied it: replay returns the original result.
        h.api.replies.borrow_mut().clear();
        let op_key = {
            *h.api.fail_next.borrow_mut() = Some(ApiFailure::Ambiguous("timeout".into()));
            h.run(t(120), 120, chain(&["a", "b"]), false);
            journal::list(h.dir.path()).unwrap()[0].key.clone()
        };
        // Simulate the server having applied that PUT despite the lost response.
        {
            let mut graphs = h.api.graphs.borrow_mut();
            let g = graphs.get_mut("g1").unwrap();
            g.owned_ids = vec!["a".into(), "b".into()];
            g.head = "b".into();
            g.generation = 1;
            let meta = h.api.meta_of("g1", g);
            h.api.replies.borrow_mut().insert(
                op_key,
                (
                    0,
                    Applied {
                        meta,
                        created: false,
                    },
                ),
            );
        }
        assert!(matches!(
            h.run(t(180), 120, chain(&["a", "b"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.state().current.unwrap().generation, 1);
        assert_eq!(h.pending(), 0);
    }

    #[test]
    fn a_put_rejected_as_frozen_reclassifies_into_a_continuation() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        // Another client froze it meanwhile.
        h.api.graphs.borrow_mut().get_mut("g1").unwrap().state = GraphState::Frozen;
        h.api.graphs.borrow_mut().get_mut("g1").unwrap().generation = 1;
        let out = h.run(t(60), 60, chain(&["a", "b"]), false);
        assert!(matches!(out, Outcome::Continued(_)), "{out:?}");
        assert_eq!(h.api.graph("g2").owned_ids, ["b"]);
        assert_eq!(h.pending(), 0);
    }

    #[test]
    fn remote_drift_is_a_conflict_not_an_overwrite() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        {
            let mut graphs = h.api.graphs.borrow_mut();
            let g = graphs.get_mut("g1").unwrap();
            g.owned_ids = vec!["a".into(), "zzz".into()];
            g.head = "zzz".into();
            g.generation = 7;
        }
        let out = h.run(t(60), 60, chain(&["a", "b"]), false);
        assert!(
            matches!(out, Outcome::Failed(ref m) if m.contains("changed on the server")),
            "{out:?}"
        );
        assert_eq!(h.api.graph("g1").owned_ids, ["a", "zzz"]);
    }

    #[test]
    fn a_display_only_generation_bump_is_adopted() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        h.api.graphs.borrow_mut().get_mut("g1").unwrap().generation = 3;
        assert!(matches!(
            h.run(t(60), 60, chain(&["a", "b"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.state().current.unwrap().generation, 4);
    }

    #[test]
    fn losing_an_owned_step_is_reported_and_a_deleted_graph_is_not_recreated() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a", "b"]), false);
        let out = h.run(t(60), 60, chain(&["a"]), false);
        assert!(
            matches!(out, Outcome::Failed(ref m) if m.contains("lost")),
            "{out:?}"
        );
        h.api.graphs.borrow_mut().clear();
        let out = h.run(t(120), 120, chain(&["a", "b", "c"]), false);
        assert!(
            matches!(out, Outcome::Failed(ref m) if m.contains("deleted")),
            "{out:?}"
        );
        assert!(h.api.graphs.borrow().is_empty());
    }

    #[test]
    fn an_existing_continuation_is_adopted_when_it_is_a_prefix_of_ours() {
        let h = Harness::new();
        h.run(t(0), 0, chain(&["a"]), false);
        h.run(t(3 * H), 0, chain(&["a"]), false);
        assert_eq!(h.api.graph("g1").state, GraphState::Frozen);
        // Another client already continued with ["b"].
        {
            let base = Some("https://h/u/me/stash/graphs/g1#p/a".to_string());
            let mut doc = chain(&["b"]);
            doc.single_path_mut_steps()[0].step.parents.clear();
            let mut graphs = h.api.graphs.borrow_mut();
            graphs.insert(
                "gx".into(),
                FakeGraph {
                    state: GraphState::Mutable,
                    generation: 0,
                    path_id: "p".into(),
                    owned_ids: vec!["b".into()],
                    head: "b".into(),
                    base_from: base,
                    continuation: None,
                    doc,
                },
            );
            graphs.get_mut("g1").unwrap().continuation = Some("gx".into());
        }
        let out = h.run(t(4 * H), 1, chain(&["a", "b", "c"]), false);
        assert!(matches!(out, Outcome::Continued(_)), "{out:?}");
        assert_eq!(h.state().current.as_ref().unwrap().graph_id, "gx");
        assert_eq!(h.state().current.as_ref().unwrap().owned_ids, ["b"]);
        // The next pass brings it up to date.
        assert!(matches!(
            h.run(t(4 * H + 60), 2, chain(&["a", "b", "c"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.api.graph("gx").owned_ids, ["b", "c"]);
    }

    #[test]
    fn a_graph_share_uploaded_is_adopted_from_the_manifest() {
        let h = Harness::new();
        let doc = chain(&["a"]);
        let (path_id, owned_ids, head, base_from) = ids_of(&doc);
        h.api.graphs.borrow_mut().insert(
            "shared".into(),
            FakeGraph {
                state: GraphState::Mutable,
                generation: 0,
                path_id,
                owned_ids,
                head,
                base_from,
                continuation: None,
                doc,
            },
        );
        super::super::record_upload(
            h.dir.path(),
            ArtifactType::Codex,
            "s1",
            None,
            &h.dest.repo_url(),
            super::super::UploadRecord {
                graph_id: "shared".into(),
                url: "https://h/u/me/stash/graphs/shared".into(),
                modified: Some(t(0)),
                size: Some(0),
                uploaded_at: t(0),
            },
        )
        .unwrap();
        assert!(matches!(
            h.run(t(60), 60, chain(&["a", "b"]), false),
            Outcome::Updated(_)
        ));
        assert_eq!(h.api.graph("shared").owned_ids, ["a", "b"]);
    }

    trait StepsMut {
        fn single_path_mut_steps(&mut self) -> &mut Vec<Step>;
    }
    impl StepsMut for Graph {
        fn single_path_mut_steps(&mut self) -> &mut Vec<Step> {
            match &mut self.paths[0] {
                toolpath::v1::PathOrRef::Path(p) => &mut p.steps,
                _ => unreachable!(),
            }
        }
    }
}
