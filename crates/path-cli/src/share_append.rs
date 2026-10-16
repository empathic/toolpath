//! Append one session to its remote path on one destination: the sync
//! client's single write path, which `path share` and `path share --all`
//! both call. See
//! `docs/superpowers/specs/2026-10-13-path-sync-client-design.md`,
//! "Append: one session, one destination".
//!
//! Every derived step is pushed. The remote record says how far into the
//! local derive the remote chain reaches; a pass sends the steps past
//! that point, and re-sends the open region of what it sent last (see
//! [`settled_len`]), whose turns may since have gained tool results,
//! token counts or more turns: on a mutable path a re-sent step replaces
//! the stored one. A settled step is final, so once sent past its open
//! region it is not sent again. When the record no longer describes the
//! derive, the steps the server lacks are found by id; a frozen path is
//! continued in a new graph, its stored steps final as they are; a path
//! the server no longer has is recreated.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashSet;
use toolpath::v1::{Graph, Parent, Path, PathIdentity, Step, StructuralChange};

use crate::artifact::ArtifactType;
use crate::cmd_pathbase::{self, AppendError, BATCH_BUDGET};
use crate::sync::{RemoteRecord, SourceStamp, Stamp};

/// An authed Pathbase destination: where a session's path goes, and the
/// credentials to write there.
#[derive(Debug, Clone)]
pub(crate) struct Destination {
    pub(crate) base_url: String,
    pub(crate) token: String,
    pub(crate) owner: String,
    pub(crate) repo: String,
    /// Display label for a graph this creates; `None` names it after
    /// the document.
    pub(crate) name: Option<String>,
    pub(crate) public: bool,
}

impl Destination {
    /// A path API URL on this destination, standing in for any: what
    /// [`crate::sync::Remotes::at`] compares record keys against.
    pub(crate) fn probe(&self) -> String {
        let nil = uuid::Uuid::nil();
        cmd_pathbase::remote_path_url(&self.base_url, &self.owner, &self.repo, &nil, &nil)
    }

    /// `owner/repo`, for messages.
    pub(crate) fn display(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// One local session as derived: the id its records sit under, its
/// path, and its source's stamp taken before the derive read it.
pub(crate) struct Session<'a> {
    pub(crate) artifact_type: ArtifactType,
    pub(crate) id: &'a str,
    pub(crate) path: &'a Path,
    pub(crate) stamp: Stamp,
}

/// What one append did. Each carrying `url` names the graph it created,
/// by the page URL the server reported.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    /// No record: a new graph with the session's `steps` steps.
    Created { url: String, steps: usize },
    /// `steps` new steps appended to the recorded path, and `resent`
    /// steps it held sent again, replacing what it stored.
    Appended { steps: usize, resent: usize },
    /// The recorded path already holds every step as it is.
    UpToDate,
    /// The session derived to no step; nothing sent, no record.
    Empty,
    /// The recorded path is frozen: a new graph continuing it, holding
    /// the `steps` steps the frozen one lacks.
    Continued { url: String, steps: usize },
    /// The recorded path could not be appended to (`reason`): a new graph
    /// with all `steps` steps replaced it.
    Recreated {
        url: String,
        steps: usize,
        reason: String,
    },
}

impl Outcome {
    /// One line for the user: what happened, with counts and reason.
    pub(crate) fn describe(&self) -> String {
        match self {
            Outcome::Created { steps, .. } => format!("created ({})", plural(*steps, "step")),
            Outcome::Appended { steps, resent: 0 } => {
                format!("appended {}", plural(*steps, "step"))
            }
            Outcome::Appended { steps: 0, resent } => {
                format!("re-sent {}", plural(*resent, "open step"))
            }
            Outcome::Appended { steps, resent } => format!(
                "appended {}, re-sent {}",
                plural(*steps, "step"),
                plural(*resent, "open step")
            ),
            Outcome::UpToDate => "skipped: the remote holds every step".into(),
            Outcome::Empty => "skipped: the session has no steps".into(),
            Outcome::Continued { steps, .. } => format!(
                "continued: the remote path is frozen; a new graph continues it with {}",
                plural(*steps, "step")
            ),
            Outcome::Recreated { steps, reason, .. } => {
                format!("recreated ({}): {reason}", plural(*steps, "step"))
            }
        }
    }

    /// The graph this push created, if it created one.
    pub(crate) fn url(&self) -> Option<&str> {
        match self {
            Outcome::Created { url, .. }
            | Outcome::Continued { url, .. }
            | Outcome::Recreated { url, .. } => Some(url),
            Outcome::Appended { .. } | Outcome::UpToDate | Outcome::Empty => None,
        }
    }
}

pub(crate) fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Push `session`'s steps to `dest` and record where they are,
/// in the remotes file under `config_dir`. An error means the session
/// failed: nothing was recorded, and the next pass starts from the
/// record as it was. A server whose create response lists no stored
/// paths leaves nothing to record: with `record_required` (a bulk pass,
/// which would otherwise create a graph every time) that fails the
/// session and the graph is deleted again; without it, a warning.
pub(crate) fn push(
    config_dir: &std::path::Path,
    dest: &Destination,
    session: &Session,
    record_required: bool,
) -> Result<Outcome> {
    if session.path.steps.is_empty() {
        return Ok(Outcome::Empty);
    }
    let push = Push {
        config_dir,
        dest,
        session,
        record_required,
    };
    let remotes = crate::sync::load_remotes(config_dir)?;
    match remotes.at(session.artifact_type, session.id, &dest.probe()) {
        None => push.create(),
        Some((url, record)) => push.update(url, record),
    }
}

/// One session's push in progress.
struct Push<'a> {
    config_dir: &'a std::path::Path,
    dest: &'a Destination,
    session: &'a Session<'a>,
    record_required: bool,
}

/// What the server holds along the chain of a recorded path, split by
/// whose step each id is.
struct Stored {
    /// The path's own step ids.
    owned: HashSet<String>,
    /// The ids stored along its parent chain, as far as the caller can
    /// read it.
    inherited: HashSet<String>,
}

impl Stored {
    fn holds(&self, id: &str) -> bool {
        self.owned.contains(id) || self.inherited.contains(id)
    }
}

/// A graph just created: its stored path's API URL (the record key),
/// `None` from a server that does not list the stored paths, and the
/// graph's page URL.
struct Created {
    key: Option<String>,
    page: String,
}

/// How a pass found the steps it appends, which decides what a refused
/// append falls back to.
#[derive(Clone, Copy, PartialEq)]
enum Found {
    /// From the open region the record's steps end in, its prefix
    /// holding.
    PastRecord,
    /// By diffing the steps against the server's ids.
    ByIds,
}

impl Push<'_> {
    fn steps(&self) -> &[Step] {
        &self.session.path.steps
    }

    /// The path's head, which every write tells the server.
    fn head(&self) -> &str {
        &self.session.path.path.head
    }

    /// Append step 3: no record, so a new graph of the whole derive.
    fn create(&self) -> Result<Outcome> {
        let created = self.create_all()?;
        self.record_created(&created, self.head().to_string(), false)?;
        Ok(Outcome::Created {
            url: created.page,
            steps: self.steps().len(),
        })
    }

    /// Steps 4 to 7: a record at `url` says the remote holds
    /// `record.steps` steps of the derive.
    ///
    /// The last pass sent `S[0..r)`, `r = record.steps`, and the steps
    /// of it that were open then, `S[settled_len(S[0..r))..r)`, may have
    /// changed since; the steps before them were settled, so final, and
    /// were sent as they are. A pass therefore sends
    /// `S[settled_len(S[0..r))..len(S))`: the open region it last sent,
    /// and everything after it. An unchanged source stamp with nothing
    /// new means nothing changed, and nothing is sent.
    fn update(&self, url: &str, record: &RemoteRecord) -> Result<Outcome> {
        let n = self.steps().len();
        let prefix_holds = record.steps > 0
            && n >= record.steps
            && self.steps()[record.steps - 1].step.id == record.last_step;
        if !prefix_holds {
            return self.update_by_ids(url, record);
        }
        let stamp = SourceStamp::known(self.session.stamp);
        if n == record.steps && stamp.is_some() && record.source_stamp == stamp {
            return Ok(Outcome::UpToDate);
        }
        let from = settled_len(&self.steps()[..record.steps]);
        if !record.has_parent {
            let send: Vec<usize> = (from..n).collect();
            return self.append_new(url, record, None, &send, Found::PastRecord);
        }
        // A continuation keeps only edges among its own steps, and which
        // those are only the server knows. A step it inherits is final.
        let stored = match self.stored(url, true) {
            Ok(stored) => stored,
            Err(AppendError::Gone) => return self.recreate("the remote path is gone"),
            Err(e) => return Err(anyhow!("read the remote path's step ids: {e}")),
        };
        let send: Vec<usize> = (from..n)
            .filter(|&i| !stored.inherited.contains(&self.steps()[i].step.id))
            .collect();
        self.append_new(url, record, Some(&stored), &send, Found::PastRecord)
    }

    /// Step 5: the record does not describe the derive. Send every step
    /// the server lacks, by id, in document order, and re-send the open
    /// region `S[settled_len(S)..)` where the path owns it.
    fn update_by_ids(&self, url: &str, record: &RemoteRecord) -> Result<Outcome> {
        let stored = match self.stored(url, record.has_parent) {
            Ok(stored) => stored,
            Err(AppendError::Gone) => return self.recreate("the remote path is gone"),
            Err(e) => return Err(anyhow!("read the remote path's step ids: {e}")),
        };
        let open = settled_len(self.steps());
        let send: Vec<usize> = (0..self.steps().len())
            .filter(|&i| {
                let id = &self.steps()[i].step.id;
                !stored.holds(id) || (i >= open && stored.owned.contains(id))
            })
            .collect();
        self.append_new(url, record, Some(&stored), &send, Found::ByIds)
    }

    /// Send `send` to the recorded path at `url` and advance the record,
    /// or take the step the refusal calls for. `stored` is what the server
    /// was read to hold, when it was read; without it, the steps it holds
    /// are the record's first `record.steps`. Nothing to send: the
    /// server's extra steps stay where they are, and so does the head it
    /// was last told.
    fn append_new(
        &self,
        url: &str,
        record: &RemoteRecord,
        stored: Option<&Stored>,
        send: &[usize],
        found: Found,
    ) -> Result<Outcome> {
        if send.is_empty() {
            self.record(url, record.head.clone(), record.has_parent)?;
            return Ok(Outcome::UpToDate);
        }
        let new = send
            .iter()
            .filter(|&&i| match stored {
                Some(stored) => !stored.holds(&self.steps()[i].step.id),
                None => i >= record.steps,
            })
            .count();
        let owned = stored.filter(|_| record.has_parent).map(|s| &s.owned);
        match self.append(url, owned, send) {
            Ok(()) => {
                self.record(url, self.head().to_string(), record.has_parent)?;
                Ok(Outcome::Appended {
                    steps: new,
                    resent: send.len() - new,
                })
            }
            Err(AppendError::Frozen) => self.continuation(url, record),
            Err(AppendError::Gone) => self.recreate("the remote path is gone"),
            // Past the record, a new step does not fit what is stored:
            // let the ids decide.
            Err(AppendError::InvalidDocument(_)) if found == Found::PastRecord => {
                self.update_by_ids(url, record)
            }
            Err(AppendError::InvalidDocument(msg)) => self.recreate(&format!(
                "the derive no longer fits the remote path ({msg})"
            )),
            Err(AppendError::InheritedStepRedefined(msg)) => self.recreate(&inherited(&msg)),
            Err(AppendError::Other(msg)) => Err(anyhow!(msg)),
        }
    }

    /// Step 6: the path at `url` is frozen. Create a graph whose path
    /// continues it and holds the steps the server does not have along
    /// the frozen path's chain, found by id whichever append was refused,
    /// so steps an unrecorded earlier append stored are not claimed
    /// again; a step the frozen chain holds stays as it was stored. Its
    /// parent names the graph by the URL the server reports for it.
    fn continuation(&self, url: &str, record: &RemoteRecord) -> Result<Outcome> {
        let d = self.dest;
        let stored = match self.stored(url, record.has_parent) {
            Ok(stored) => stored,
            Err(AppendError::Gone) => return self.recreate("the remote path is gone"),
            Err(e) => return Err(anyhow!("read the frozen path's step ids: {e}")),
        };
        let new: Vec<usize> = (0..self.steps().len())
            .filter(|&i| !stored.holds(&self.steps()[i].step.id))
            .collect();
        let Some(&first) = new.first() else {
            self.record(url, record.head.clone(), record.has_parent)?;
            return Ok(Outcome::UpToDate);
        };
        let uri = match cmd_pathbase::graph_url(&d.base_url, &d.token, url) {
            Ok(uri) => uri,
            Err(AppendError::Gone) => return self.recreate("the remote path is gone"),
            Err(e) => return Err(anyhow!("read the frozen graph's URL: {e}")),
        };
        let head = if new.iter().any(|&i| self.steps()[i].step.id == self.head()) {
            self.head().to_string()
        } else {
            self.steps()[*new.last().expect("new is not empty")]
                .step
                .id
                .clone()
        };
        let path = &self.session.path;
        let continuation = Path {
            path: PathIdentity {
                head: head.clone(),
                parent: Some(Parent {
                    uri,
                    path: path.path.id.clone(),
                    step: self.parent_step(first, &stored, record),
                }),
                ..path.path.clone()
            },
            // The new path owns nothing yet: only edges among its steps.
            steps: self.batch(Some(&HashSet::new()), &new),
            meta: path.meta.clone(),
        };
        let created = match self.create_graph(continuation) {
            Ok(created) => created,
            // The ids stopped at an unreadable parent and a step stored
            // above it is among these.
            Err(e) if cmd_pathbase::refusal_code(&e) == Some("inherited_step_redefined") => {
                return self.recreate(&inherited(&format!("{e:#}")));
            }
            Err(e) => return Err(e),
        };
        self.record_created(&created, head, true)?;
        Ok(Outcome::Continued {
            url: created.page,
            steps: new.len(),
        })
    }

    /// The step of the frozen path the continuation's root steps descend
    /// from: the first new step's own parent when the frozen path owns
    /// it (a session that regrew from an earlier step continues from
    /// there), else the head the frozen path was last told. The server
    /// takes only a step the parent path owns.
    fn parent_step(&self, first: usize, stored: &Stored, record: &RemoteRecord) -> String {
        self.steps()[first]
            .step
            .parents
            .first()
            .filter(|p| stored.owned.contains(p.as_str()))
            .cloned()
            .unwrap_or_else(|| record.head.clone())
    }

    /// Step 7: a new graph of the whole derive replaces the recorded one.
    fn recreate(&self, reason: &str) -> Result<Outcome> {
        let created = self.create_all()?;
        self.record_created(&created, self.head().to_string(), false)?;
        Ok(Outcome::Recreated {
            url: created.page,
            steps: self.steps().len(),
            reason: reason.to_string(),
        })
    }

    /// Append `new` to the path at `url`, its parents kept only among
    /// `owned` and the batch's earlier steps when `owned` is given.
    fn append(
        &self,
        url: &str,
        owned: Option<&HashSet<String>>,
        new: &[usize],
    ) -> std::result::Result<(), AppendError> {
        let d = self.dest;
        cmd_pathbase::append_steps(
            &d.base_url,
            &d.token,
            url,
            self.session.path,
            self.batch(owned, new),
            self.head(),
            BATCH_BUDGET,
        )
    }

    /// The steps `new`, in order. With `owned`, a path that stores edges
    /// only among its own steps: each keeps the parents in `owned` or
    /// earlier in the batch, and a step left with none descends from the
    /// path's `parent.step` implicitly.
    fn batch(&self, owned: Option<&HashSet<String>>, new: &[usize]) -> Vec<Step> {
        let Some(owned) = owned else {
            return new.iter().map(|&i| self.steps()[i].clone()).collect();
        };
        let mut earlier: HashSet<&str> = HashSet::new();
        let mut steps = Vec::with_capacity(new.len());
        for &i in new {
            let mut step = self.steps()[i].clone();
            step.step
                .parents
                .retain(|p| owned.contains(p) || earlier.contains(p.as_str()));
            earlier.insert(self.steps()[i].step.id.as_str());
            steps.push(step);
        }
        steps
    }

    /// What the server holds along the chain of the path at `url`. A path
    /// with a parent lists its own ids first, then the chain's; the
    /// boundary between them is read from its own ids alone.
    fn stored(&self, url: &str, has_parent: bool) -> std::result::Result<Stored, AppendError> {
        let d = self.dest;
        let read = cmd_pathbase::path_step_ids(&d.base_url, &d.token, url, true)?;
        if let Some(at) = &read.truncated_at {
            eprintln!(
                "note: the remote path's history is readable only down to path {} of {}; \
                 a step stored above it may make the server refuse the append",
                at.path, at.uri
            );
        }
        let all: HashSet<String> = read.step_ids.into_iter().collect();
        if !has_parent {
            return Ok(Stored {
                owned: all,
                inherited: HashSet::new(),
            });
        }
        let owned: HashSet<String> =
            cmd_pathbase::path_step_ids(&d.base_url, &d.token, url, false)?
                .step_ids
                .into_iter()
                .collect();
        let inherited = all.difference(&owned).cloned().collect();
        Ok(Stored { owned, inherited })
    }

    /// Create a graph of the whole derive.
    fn create_all(&self) -> Result<Created> {
        self.create_graph(self.session.path.clone())
    }

    /// Create a graph holding `path`. A server that does not list the
    /// stored path leaves nothing to record: when a record is required,
    /// the graph is deleted and the session fails, naming the graph when
    /// it could not be deleted; otherwise a warning says the next share
    /// will create another graph.
    fn create_graph(&self, path: Path) -> Result<Created> {
        let d = self.dest;
        let doc = Graph::from_path(path);
        let body = doc.to_json().context("serialize document")?;
        let name = d
            .name
            .clone()
            .unwrap_or_else(|| crate::cmd_export::derive_name(&doc));
        let created = cmd_pathbase::create_graph(
            &d.base_url,
            &d.token,
            &d.owner,
            &d.repo,
            Some(&name),
            &doc,
            &body,
            d.public,
        )?;
        let Some(stored) = created.paths.first() else {
            if self.record_required {
                let unsupported =
                    "server does not report stored paths; incremental upload unsupported";
                let deleted = cmd_pathbase::graphs_delete(
                    &d.base_url,
                    &d.token,
                    &d.owner,
                    &d.repo,
                    &created.id,
                );
                if let Err(e) = deleted {
                    eprintln!(
                        "warning: the graph just created could not be deleted; \
                         delete {} by hand: {e:#}",
                        created.url
                    );
                    bail!(
                        "{unsupported}; the graph it created could not be deleted: {}",
                        created.url
                    );
                }
                bail!("{unsupported}");
            }
            eprintln!(
                "warning: remote record not written: the server did not list the stored paths"
            );
            return Ok(Created {
                key: None,
                page: created.url,
            });
        };
        Ok(Created {
            key: Some(cmd_pathbase::remote_path_url(
                &d.base_url,
                &d.owner,
                &d.repo,
                &created.id,
                &stored.server_id,
            )),
            page: created.url,
        })
    }

    /// [`Self::record`] for a graph just created, when it can be
    /// recorded.
    fn record_created(&self, created: &Created, head: String, has_parent: bool) -> Result<()> {
        match &created.key {
            Some(url) => self.record(url, head, has_parent),
            None => Ok(()),
        }
    }

    /// Record that the remote at `url` holds the whole derive, was last
    /// told `head`, and has a parent or not. Replaces the session's record
    /// on this destination.
    fn record(&self, url: &str, head: String, has_parent: bool) -> Result<()> {
        let n = self.steps().len();
        let record = RemoteRecord {
            steps: n,
            last_step: self.steps()[n - 1].step.id.clone(),
            head,
            has_parent,
            source_stamp: SourceStamp::known(self.session.stamp),
        };
        crate::sync::record_remote(
            self.config_dir,
            self.session.artifact_type,
            self.session.id,
            url,
            record,
        )
        .context("write the remote record")
    }
}

/// The reason a refusal of an inherited id gives for recreating.
fn inherited(msg: &str) -> String {
    format!("a step is stored along the remote path's parents ({msg})")
}

/// How many leading steps are settled: followed by a later turn, so
/// final; the rest is the open region, which a pass re-sends until it
/// settles. The last message group (the steps from the first turn of the
/// trailing run of turns sharing the last turn's `group_id`, or the last
/// turn alone when it has none) is open: its turns and token counts may
/// still change. So is every step after it, and every trailing step with
/// a tool call that has no result yet. A path with no turn has no
/// settled step.
pub(crate) fn settled_len(steps: &[Step]) -> usize {
    let Some(last) = steps.iter().rposition(|s| turn(s).is_some()) else {
        return 0;
    };
    let mut k = last;
    if let Some(group) = group_id(&steps[last]) {
        for i in (0..last).rev() {
            if turn(&steps[i]).is_none() {
                continue;
            }
            if group_id(&steps[i]) != Some(group) {
                break;
            }
            k = i;
        }
    }
    while k > 0 && has_open_tool_call(&steps[k - 1]) {
        k -= 1;
    }
    k
}

/// The step's `conversation.append` change, when it is a turn.
fn turn(step: &Step) -> Option<&StructuralChange> {
    step.change
        .values()
        .filter_map(|c| c.structural.as_ref())
        .find(|s| s.change_type == "conversation.append")
}

fn group_id(step: &Step) -> Option<&str> {
    turn(step)?.extra.get("group_id")?.as_str()
}

/// Whether the step is a turn with a tool call that has no result.
fn has_open_tool_call(step: &Step) -> bool {
    turn(step)
        .and_then(|t| t.extra.get("tool_uses"))
        .and_then(|v| v.as_array())
        .is_some_and(|calls| {
            calls
                .iter()
                .any(|c| c.get("result").is_none_or(|r| r.is_null()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_pathbase::{FakePathbase, ids_of};
    use std::collections::HashMap as Map;
    use toolpath::v1::{ArtifactChange, StepIdentity};

    const KEY: &str = "agent://claude/s";

    /// A turn step: `role`, message `group`, and tool calls each with
    /// or without a result.
    fn turn_step(id: &str, parent: Option<&str>, group: Option<&str>, tools: &[bool]) -> Step {
        let mut extra = Map::new();
        extra.insert("role".into(), serde_json::json!("assistant"));
        extra.insert("text".into(), serde_json::json!(format!("text of {id}")));
        if let Some(g) = group {
            extra.insert("group_id".into(), serde_json::json!(g));
        }
        if !tools.is_empty() {
            let calls: Vec<serde_json::Value> = tools
                .iter()
                .enumerate()
                .map(|(i, done)| {
                    let mut c = serde_json::json!({"id": format!("{id}-t{i}"), "name": "Bash", "input": {}});
                    if *done {
                        c["result"] = serde_json::json!({"content": "ok", "is_error": false});
                    }
                    c
                })
                .collect();
            extra.insert("tool_uses".into(), serde_json::Value::Array(calls));
        }
        step(id, parent, "conversation.append", extra)
    }

    fn event_step(id: &str, parent: Option<&str>) -> Step {
        step(id, parent, "conversation.event", Map::new())
    }

    fn step(
        id: &str,
        parent: Option<&str>,
        kind: &str,
        extra: Map<String, serde_json::Value>,
    ) -> Step {
        let mut change = Map::new();
        change.insert(
            KEY.to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: kind.to_string(),
                    extra,
                }),
            },
        );
        Step {
            step: StepIdentity {
                id: id.to_string(),
                parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
                actor: "agent:claude".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            change,
            meta: None,
        }
    }

    fn path_of(steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        Path {
            path: PathIdentity {
                id: "path-claude-s".into(),
                base: None,
                head,
                graph_ref: None,
                parent: None,
            },
            steps,
            meta: Some(toolpath::v1::PathMeta {
                title: Some("s".into()),
                ..Default::default()
            }),
        }
    }

    /// A linear session of `n` turns, each its own message group, the
    /// ids `t0..t{n-1}`: `n - 1` of them settled, the last one open.
    fn session_of(n: usize) -> Path {
        path_of(
            (0..n)
                .map(|i| {
                    let parent = (i > 0).then(|| format!("t{}", i - 1));
                    turn_step(
                        &format!("t{i}"),
                        parent.as_deref(),
                        Some(&format!("g{i}")),
                        &[],
                    )
                })
                .collect(),
        )
    }

    fn ids(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| s.step.id.clone()).collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn dest(fake: &FakePathbase, repo: &str) -> Destination {
        Destination {
            base_url: fake.base(),
            token: "tok".into(),
            owner: "alex".into(),
            repo: repo.into(),
            name: None,
            public: false,
        }
    }

    const STAMP: Stamp = (None, Some(100));

    fn push_to(
        cfg: &std::path::Path,
        d: &Destination,
        path: &Path,
        stamp: Stamp,
    ) -> Result<Outcome> {
        push(
            cfg,
            d,
            &Session {
                artifact_type: ArtifactType::Claude,
                id: "s",
                path,
                stamp,
            },
            false,
        )
    }

    /// The session's record on `d`, with its key.
    fn record_on(cfg: &std::path::Path, d: &Destination) -> Option<(String, RemoteRecord)> {
        let remotes = crate::sync::load_remotes(cfg).unwrap();
        remotes
            .at(ArtifactType::Claude, "s", &d.probe())
            .map(|(k, r)| (k.to_string(), r.clone()))
    }

    fn record_count(cfg: &std::path::Path) -> usize {
        crate::sync::load_remotes(cfg)
            .unwrap()
            .sessions
            .get("claude")
            .and_then(|s| s.get("s"))
            .map_or(0, |r| r.len())
    }

    // ── settle ─────────────────────────────────────────────────────

    #[test]
    fn settled_len_of_nothing_and_of_events_only_is_zero() {
        assert_eq!(settled_len(&[]), 0);
        assert_eq!(
            settled_len(&[event_step("e0", None), event_step("e1", Some("e0"))]),
            0
        );
    }

    #[test]
    fn a_lone_open_turn_settles_nothing() {
        assert_eq!(settled_len(&session_of(1).steps), 0);
    }

    #[test]
    fn the_last_message_group_and_what_follows_it_are_unsettled() {
        let steps = vec![
            turn_step("u", None, None, &[]),
            event_step("e0", Some("u")),
            turn_step("a1", Some("e0"), Some("g"), &[true]),
            event_step("e1", Some("a1")),
            turn_step("a2", Some("e1"), Some("g"), &[]),
            event_step("e2", Some("a2")),
        ];
        // `e0` precedes the open group and a turn follows it: settled.
        assert_eq!(settled_len(&steps), 2);
    }

    #[test]
    fn a_turn_without_a_group_is_its_own_group() {
        let steps = vec![
            turn_step("a", None, Some("g1"), &[]),
            turn_step("b", Some("a"), Some("g1"), &[]),
            turn_step("u", Some("b"), None, &[]),
        ];
        assert_eq!(settled_len(&steps), 2);
    }

    #[test]
    fn an_earlier_run_of_the_same_group_id_stays_settled() {
        let steps = vec![
            turn_step("a", None, Some("g"), &[]),
            turn_step("b", Some("a"), Some("h"), &[]),
            turn_step("c", Some("b"), Some("g"), &[]),
        ];
        assert_eq!(settled_len(&steps), 2);
    }

    /// The last turn is a bare tool call awaiting its result: its group
    /// is open, and so is the call.
    #[test]
    fn a_trailing_tool_call_without_a_result_is_unsettled() {
        let awaiting = vec![
            turn_step("u", None, None, &[]),
            turn_step("a", Some("u"), Some("g1"), &[false]),
        ];
        assert_eq!(settled_len(&awaiting), 1);

        // The call ends a step the group rule leaves settled.
        let behind = vec![
            turn_step("u", None, None, &[]),
            turn_step("a", Some("u"), Some("g1"), &[true, false]),
            turn_step("u2", Some("a"), None, &[]),
        ];
        assert_eq!(settled_len(&behind), 1);

        // Once answered, it settles.
        let answered = vec![
            turn_step("u", None, None, &[]),
            turn_step("a", Some("u"), Some("g1"), &[true, true]),
            turn_step("u2", Some("a"), None, &[]),
        ];
        assert_eq!(settled_len(&answered), 2);
    }

    // ── push ───────────────────────────────────────────────────────

    #[test]
    fn an_empty_derive_sends_nothing_and_records_nothing() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let out = push_to(cfg.path(), &dest(&fake, "x"), &path_of(vec![]), STAMP).unwrap();
        assert_eq!(out, Outcome::Empty);
        assert!(fake.requests().is_empty());
        assert_eq!(record_count(cfg.path()), 0);
    }

    #[test]
    fn no_record_creates_a_graph_of_every_step_the_open_one_included() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let out = push_to(cfg.path(), &d, &session_of(4), STAMP).unwrap();
        let (key, record) = record_on(cfg.path(), &d).unwrap();
        let stored = fake.path_at(&key).unwrap();
        assert_eq!(stored.step_ids(), strs(&["t0", "t1", "t2", "t3"]));
        assert_eq!(stored.head.as_deref(), Some("t3"));
        assert_eq!(
            record,
            RemoteRecord {
                steps: 4,
                last_step: "t3".into(),
                head: "t3".into(),
                has_parent: false,
                source_stamp: SourceStamp::known(STAMP),
            }
        );
        let (graph, _) = ids_of(&key).unwrap();
        assert_eq!(
            out,
            Outcome::Created {
                url: format!("{}/u/alex/x/graphs/{graph}", fake.base()),
                steps: 4,
            }
        );
    }

    /// A lone open turn is shared at once.
    #[test]
    fn a_session_with_nothing_settled_is_created() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let out = push_to(cfg.path(), &d, &session_of(1), STAMP).unwrap();
        assert!(matches!(out, Outcome::Created { steps: 1, .. }), "{out:?}");
        let (key, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(fake.path_at(&key).unwrap().step_ids(), strs(&["t0"]));
        assert_eq!((record.steps, record.last_step.as_str()), (1, "t0"));
    }

    /// The last pass sent `t2` open: this pass re-sends it with the new
    /// steps, and never the settled `t0, t1` again.
    #[test]
    fn a_grown_session_appends_its_new_steps_and_resends_the_open_one() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &session_of(6), (None, Some(200))).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 3,
                resent: 1
            }
        );
        let writes = fake.writes();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].target.ends_with("/steps"), "{}", writes[0].target);
        assert_eq!(writes[0].step_ids(), strs(&["t2", "t3", "t4", "t5"]));
        assert_eq!(writes[0].heads(), strs(&["t5"]));
        assert!(!writes[0].body.contains("PathOpen") && !writes[0].body.contains("PathClose"));

        let (key, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(
            fake.path_at(&key).unwrap().step_ids(),
            ids(&session_of(6).steps)
        );
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str()
            ),
            (6, "t5", "t5")
        );
        assert_eq!(record.source_stamp, SourceStamp::known((None, Some(200))));
        assert_eq!(fake.graphs().len(), 1);
    }

    /// The source's stamp is the one recorded and nothing is new: nothing
    /// changed, so nothing is sent and the record stays as it is.
    #[test]
    fn an_unchanged_session_sends_nothing() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let before = record_on(cfg.path(), &d).unwrap();
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        assert_eq!(out, Outcome::UpToDate);
        assert!(fake.requests().is_empty());
        assert_eq!(record_on(cfg.path(), &d).unwrap(), before);
    }

    /// A tool result arrives with no new step: the source changed, so the
    /// open step is re-sent alone and the server stores its new payload.
    #[test]
    fn a_tool_result_arriving_resends_its_step_and_the_server_keeps_it() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let with_call = |answered: bool| {
            let mut path = session_of(3);
            path.steps[2] = turn_step("t2", Some("t1"), Some("g2"), &[answered]);
            path
        };
        push_to(cfg.path(), &d, &with_call(false), STAMP).unwrap();
        let (key, _) = record_on(cfg.path(), &d).unwrap();
        let result_of = |fake: &FakePathbase| {
            fake.path_at(&key).unwrap().step("t2").unwrap()["change"][KEY]["structural"]
                ["tool_uses"][0]
                .get("result")
                .cloned()
        };
        assert_eq!(result_of(&fake), None);
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &with_call(true), (None, Some(150))).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 0,
                resent: 1
            }
        );
        let writes = fake.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].step_ids(), strs(&["t2"]));
        assert_eq!(
            result_of(&fake),
            Some(serde_json::json!({"content": "ok", "is_error": false}))
        );
        assert_eq!(
            fake.path_at(&key).unwrap().step_ids(),
            strs(&["t0", "t1", "t2"])
        );
        let (_, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!((record.steps, record.last_step.as_str()), (3, "t2"));
        assert_eq!(record.source_stamp, SourceStamp::known((None, Some(150))));
    }

    /// The source's stamp unknown (an opencode row, a rerooted chain):
    /// the record carries none, and pushes still work from the derive.
    #[test]
    fn an_unknown_stamp_is_recorded_as_absent() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), (None, None)).unwrap();
        assert_eq!(record_on(cfg.path(), &d).unwrap().1.source_stamp, None);
        let out = push_to(cfg.path(), &d, &session_of(4), (None, None)).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 1, .. }), "{out:?}");
        assert_eq!(record_on(cfg.path(), &d).unwrap().1.source_stamp, None);
        // Without a stamp a pass cannot tell the open step is unchanged.
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &session_of(4), (None, None)).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 0,
                resent: 1
            }
        );
        assert_eq!(fake.writes()[0].step_ids(), strs(&["t3"]));
    }

    /// The local file lost steps: the record's position is past the
    /// derive, the ids show the server has every step, only the open one
    /// is re-sent, and the next growth appends to the same path.
    #[test]
    fn a_truncated_session_continues_in_the_same_path_by_ids() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(6), STAMP).unwrap();
        let (key, _) = record_on(cfg.path(), &d).unwrap();
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 0,
                resent: 1
            }
        );
        let reqs = fake.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].target.ends_with("/step-ids?through_parents=true"));
        assert_eq!(reqs[1].step_ids(), strs(&["t2"]));
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, key);
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str()
            ),
            (3, "t2", "t2")
        );

        // Grow along a different line than the one lost.
        let mut regrown = session_of(2);
        regrown
            .steps
            .push(turn_step("n2", Some("t1"), Some("h2"), &[]));
        regrown
            .steps
            .push(turn_step("n3", Some("n2"), Some("h3"), &[]));
        regrown.path.head = "n3".into();
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &regrown, STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(fake.writes()[0].step_ids(), strs(&["n2", "n3"]));
        assert_eq!(
            fake.path_at(&key).unwrap().step_ids(),
            strs(&["t0", "t1", "t2", "t3", "t4", "t5", "n2", "n3"])
        );
    }

    /// The derive changed under the prefix (a reorder that moved the
    /// recorded last step): the ids diff sends exactly what is missing.
    #[test]
    fn a_mismatch_appends_the_missing_steps_in_document_order() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (key, _) = record_on(cfg.path(), &d).unwrap();

        let path = path_of(vec![
            turn_step("t0", None, Some("g0"), &[]),
            event_step("late", Some("t0")),
            turn_step("t1", Some("t0"), Some("g1"), &[]),
            turn_step("t2", Some("t1"), Some("g2"), &[]),
            turn_step("t3", Some("t2"), Some("g3"), &[]),
        ]);
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &path, STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(fake.writes()[0].step_ids(), strs(&["late", "t3"]));
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, key);
        assert_eq!((record.steps, record.last_step.as_str()), (5, "t3"));
    }

    #[test]
    fn an_invalid_document_on_the_ids_append_recreates() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        fake.force(
            "POST",
            "/steps",
            400,
            r#"{"code":"invalid_document","error":"step t2 cites unstored parent zz"}"#,
        );
        let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let Outcome::Recreated { steps, reason, .. } = &out else {
            panic!("{out:?}");
        };
        assert_eq!(*steps, 5);
        assert!(reason.contains("unstored parent"), "{reason}");
        // The fast append was refused, the ids were read, the diff was
        // refused, and a fresh graph was made.
        let targets: Vec<String> = fake.requests().iter().map(|r| r.target.clone()).collect();
        assert!(
            targets.iter().any(|t| t.contains("/step-ids")),
            "{targets:?}"
        );
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, old);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            ids(&session_of(5).steps)
        );
        assert_eq!(record.steps, 5);
        assert_eq!(record_count(cfg.path()), 1);
    }

    #[test]
    fn a_frozen_path_is_continued_once_and_then_appended_to() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        let (old_graph, _) = ids_of(&old).unwrap();
        fake.freeze(old_graph);

        let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 2, .. }),
            "{out:?}"
        );
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, old);
        assert_eq!(record_count(cfg.path()), 1, "the record is re-keyed");
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str(),
                record.has_parent
            ),
            (5, "t4", "t4", true)
        );
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["t3", "t4"]));
        let parent = continuation.parent.unwrap();
        assert_eq!(
            parent.uri,
            format!("{}/u/alex/x/graphs/{old_graph}", fake.base())
        );
        assert_eq!(parent.path, "path-claude-s");
        assert_eq!(parent.step, "t2");
        assert_eq!(
            fake.path_at(&old).unwrap().step_ids(),
            strs(&["t0", "t1", "t2"])
        );

        fake.clear_log();
        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 2,
                resent: 1
            }
        );
        assert_eq!(fake.writes()[0].step_ids(), strs(&["t4", "t5", "t6"]));
        assert_eq!(record_on(cfg.path(), &d).unwrap().0, new);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            strs(&["t3", "t4", "t5", "t6"])
        );
        assert_eq!(fake.graphs().len(), 2, "exactly one continuation");
    }

    /// The path is frozen while `t2` is stored open: the pass that
    /// re-sends it is refused, `t2` stays as the frozen path stored it,
    /// and the continuation holds what the frozen chain lacks, its own
    /// open step re-sent to it on the next pass.
    #[test]
    fn a_freeze_during_a_resend_continues_with_the_open_step_and_resends_it_there() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let session = |n: usize, answered: bool| {
            let mut path = session_of(n);
            let last = n - 1;
            path.steps[last] = turn_step(
                &format!("t{last}"),
                Some(&format!("t{}", last - 1)),
                Some(&format!("g{last}")),
                &[answered],
            );
            path
        };
        push_to(cfg.path(), &d, &session(3, false), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        fake.freeze(ids_of(&old).unwrap().0);
        fake.clear_log();

        let mut grown = session(5, false);
        grown.steps[2] = turn_step("t2", Some("t1"), Some("g2"), &[true]);
        let out = push_to(cfg.path(), &d, &grown, (None, Some(200))).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 2, .. }),
            "{out:?}"
        );
        let refused = &fake.requests()[0];
        assert_eq!(refused.step_ids(), strs(&["t2", "t3", "t4"]));
        let tool_result = |key: &str, id: &str| {
            fake.path_at(key).unwrap().step(id).unwrap()["change"][KEY]["structural"]["tool_uses"]
                [0]
            .get("result")
            .cloned()
        };
        assert_eq!(tool_result(&old, "t2"), None, "a frozen step is final");
        let (cont, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(fake.path_at(&cont).unwrap().step_ids(), strs(&["t3", "t4"]));
        assert_eq!(tool_result(&cont, "t4"), None);
        assert_eq!((record.steps, record.has_parent), (5, true));

        fake.clear_log();
        let out = push_to(cfg.path(), &d, &session(5, true), (None, Some(300))).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 0,
                resent: 1
            }
        );
        assert_eq!(fake.writes()[0].step_ids(), strs(&["t4"]));
        assert!(tool_result(&cont, "t4").is_some());
        assert_eq!(fake.graphs().len(), 2);
    }

    /// A root step of the continuation names a parent the new document
    /// does not hold: the parent is dropped, the step descends from
    /// `parent.step` implicitly.
    #[test]
    fn a_continuation_drops_parents_outside_its_document() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        fake.freeze(ids_of(&old).unwrap().0);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let body = fake.writes().last().unwrap().body.clone();
        let upload: serde_json::Value = serde_json::from_str(&body).unwrap();
        let steps = &upload["document"]["paths"][0]["steps"];
        assert_eq!(steps[0]["step"]["id"], "t3");
        assert!(steps[0]["step"].get("parents").is_none(), "{}", steps[0]);
        assert_eq!(steps[1]["step"]["parents"], serde_json::json!(["t3"]));
    }

    #[test]
    fn a_deleted_graph_is_recreated_once_and_then_appended_to() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        fake.delete(ids_of(&old).unwrap().0);

        let out = push_to(cfg.path(), &d, &session_of(4), STAMP).unwrap();
        let Outcome::Recreated { steps, reason, .. } = &out else {
            panic!("{out:?}");
        };
        assert_eq!((*steps, reason.as_str()), (4, "the remote path is gone"));
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, old);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            strs(&["t0", "t1", "t2", "t3"])
        );
        assert_eq!(record.steps, 4);

        let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 1,
                resent: 1
            }
        );
        assert_eq!(fake.graphs().len(), 1);
    }

    /// A record naming a path the server never had (or no longer
    /// serves), found by the fast append and by the ids read alike.
    #[test]
    fn a_record_the_server_does_not_serve_is_recreated() {
        for (record_steps, last) in [(2, "t1"), (9, "t8")] {
            let fake = FakePathbase::start();
            let cfg = tempfile::tempdir().unwrap();
            let d = dest(&fake, "x");
            let stale = format!(
                "{}/api/v1/u/alex/repos/x/graphs/{}/paths/{}",
                fake.base(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            );
            crate::sync::record_remote(
                cfg.path(),
                ArtifactType::Claude,
                "s",
                &stale,
                RemoteRecord {
                    steps: record_steps,
                    last_step: last.into(),
                    head: last.into(),
                    has_parent: false,
                    source_stamp: None,
                },
            )
            .unwrap();
            let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
            assert!(
                matches!(out, Outcome::Recreated { steps: 5, .. }),
                "{out:?}"
            );
            let (new, _) = record_on(cfg.path(), &d).unwrap();
            assert_ne!(new, stale);
            assert_eq!(record_count(cfg.path()), 1);
        }
    }

    #[test]
    fn two_destinations_keep_two_records_and_append_independently() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        push_to(cfg.path(), &x, &session_of(3), STAMP).unwrap();
        push_to(cfg.path(), &y, &session_of(3), STAMP).unwrap();
        assert_eq!(record_count(cfg.path()), 2);
        let mut repos: Vec<(String, String)> = fake
            .graphs()
            .into_iter()
            .map(|g| (g.owner, g.repo))
            .collect();
        repos.sort();
        assert_eq!(
            repos,
            [("alex".into(), "x".into()), ("alex".into(), "y".into())]
        );
        let (y_key, y_record) = record_on(cfg.path(), &y).unwrap();

        let out = push_to(cfg.path(), &x, &session_of(5), STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(
            record_on(cfg.path(), &y).unwrap(),
            (y_key.clone(), y_record)
        );
        assert_eq!(
            fake.path_at(&y_key).unwrap().step_ids(),
            strs(&["t0", "t1", "t2"])
        );
        assert_eq!(record_on(cfg.path(), &x).unwrap().1.steps, 5);

        let out = push_to(cfg.path(), &y, &session_of(5), STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
    }

    /// A refusal the client cannot answer fails the session and leaves
    /// its record as it was.
    #[test]
    fn an_unauthorized_append_fails_and_keeps_the_record() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let before = record_on(cfg.path(), &d).unwrap();
        fake.force(
            "POST",
            "/steps",
            401,
            r#"{"code":"unauthorized","error":"no"}"#,
        );
        let err = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap_err();
        assert!(err.to_string().contains("path auth login"), "{err}");
        assert_eq!(record_on(cfg.path(), &d).unwrap(), before);
    }

    /// A server without the append route answers `404` with no error
    /// code: that is a failure, not a deleted graph to recreate.
    #[test]
    fn a_server_without_the_append_route_fails_instead_of_recreating() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        fake.force("POST", "/steps", 404, "");
        let err = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not support incremental sharing"),
            "{err}"
        );
        assert_eq!(fake.graphs().len(), 1);
    }

    /// A step's parents as sent in the last write.
    fn sent_parents(fake: &FakePathbase, id: &str) -> Vec<String> {
        let write = fake.writes().last().unwrap().clone();
        let steps: Vec<serde_json::Value> =
            match serde_json::from_str::<serde_json::Value>(&write.body) {
                Ok(upload) if upload.get("document").is_some() => {
                    upload["document"]["paths"][0]["steps"]
                        .as_array()
                        .unwrap()
                        .clone()
                }
                _ => write
                    .body
                    .lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v.get("Step").cloned())
                    .collect(),
            };
        let step = steps.iter().find(|s| s["step"]["id"] == id).unwrap();
        step["step"]["parents"]
            .as_array()
            .map(|ps| ps.iter().map(|p| p.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    /// The session was truncated back to `t1` and regrew along `n2`
    /// while the path was frozen: the ids diff finds `n2` missing, its
    /// append is refused as frozen, and the continuation descends from
    /// `n2`'s own parent rather than the abandoned line's tip.
    #[test]
    fn a_frozen_path_found_by_the_ids_diff_is_continued_from_the_new_steps_parent() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(6), STAMP).unwrap();
        let (old, before) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(before.head, "t5");
        fake.freeze(ids_of(&old).unwrap().0);

        let mut regrown = session_of(2);
        regrown
            .steps
            .push(turn_step("n2", Some("t1"), Some("h2"), &[]));
        regrown
            .steps
            .push(turn_step("n3", Some("n2"), Some("h3"), &[]));
        regrown.path.head = "n3".into();
        let out = push_to(cfg.path(), &d, &regrown, STAMP).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 2, .. }),
            "{out:?}"
        );
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, old);
        assert_eq!(
            (record.steps, record.last_step.as_str(), record.has_parent),
            (4, "n3", true)
        );
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["n2", "n3"]));
        assert_eq!(continuation.parent.unwrap().step, "t1");
        assert!(sent_parents(&fake, "n2").is_empty());
        assert_eq!(sent_parents(&fake, "n3"), strs(&["n2"]));
    }

    /// An earlier pass appended `t2..t4` and died before recording it,
    /// then the path was frozen: the continuation holds only what the
    /// server lacks, from the frozen path's real head.
    #[test]
    fn a_fast_path_freeze_after_an_unrecorded_append_continues_from_the_stored_ids() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        let unrecorded = session_of(6);
        cmd_pathbase::append_steps(
            &d.base_url,
            &d.token,
            &old,
            &unrecorded,
            unrecorded.steps[2..5].to_vec(),
            "t4",
            BATCH_BUDGET,
        )
        .unwrap();
        fake.freeze(ids_of(&old).unwrap().0);

        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 2, .. }),
            "{out:?}"
        );
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["t5", "t6"]));
        assert_eq!(continuation.parent.unwrap().step, "t4");
        assert_eq!((record.steps, record.has_parent), (7, true));
    }

    /// A continuation stores edges only among its own steps, which the
    /// server lists: a later batch's parents into inherited history are
    /// stripped, whether the batch comes from the fast path (a merge
    /// citing the inherited `t0`) or from the ids diff (a regrowth from
    /// the inherited `t1`).
    #[test]
    fn a_continuation_strips_inherited_parents_from_every_later_batch() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        fake.freeze(ids_of(&old).unwrap().0);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (cont, record) = record_on(cfg.path(), &d).unwrap();
        assert!(record.has_parent);

        let mut merged = session_of(7);
        merged.steps[5].step.parents = strs(&["t4", "t0"]);
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &merged, STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(sent_parents(&fake, "t5"), strs(&["t4"]));

        let mut regrown = session_of(2);
        regrown
            .steps
            .push(turn_step("n2", Some("t1"), Some("h2"), &[]));
        regrown
            .steps
            .push(turn_step("n3", Some("n2"), Some("h3"), &[]));
        regrown.path.head = "n3".into();
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &regrown, STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(fake.writes()[0].step_ids(), strs(&["n2", "n3"]));
        assert!(sent_parents(&fake, "n2").is_empty());
        assert_eq!(record_on(cfg.path(), &d).unwrap().0, cont);
    }

    /// The step-ids walk stopped at an unreadable parent: a pass past
    /// the record still appends only what follows it.
    #[test]
    fn a_partial_step_id_list_still_appends_past_the_record() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        let old_graph = ids_of(&old).unwrap().0;
        fake.freeze(old_graph);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (cont, _) = record_on(cfg.path(), &d).unwrap();
        fake.hide(old_graph);

        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 2,
                resent: 1
            }
        );
        assert_eq!(
            fake.path_at(&cont).unwrap().step_ids(),
            strs(&["t3", "t4", "t5", "t6"])
        );
    }

    /// The ids diff of a continuation whose parent the caller can no
    /// longer read cannot tell the hidden parent's steps from missing
    /// ones: it sends them, the server refuses an id stored along the
    /// parents, and the session starts a fresh graph.
    #[test]
    fn an_id_stored_above_an_unreadable_parent_recreates() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (old, _) = record_on(cfg.path(), &d).unwrap();
        let old_graph = ids_of(&old).unwrap().0;
        fake.freeze(old_graph);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (cont, _) = record_on(cfg.path(), &d).unwrap();
        fake.hide(old_graph);

        // A step reappears between `t2` and `t3`: the prefix check fails.
        let mut path = session_of(7);
        path.steps.insert(3, event_step("late", Some("t2")));
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &path, STAMP).unwrap();
        let Outcome::Recreated { steps, reason, .. } = &out else {
            panic!("{out:?}");
        };
        assert_eq!(*steps, 8);
        assert!(reason.contains("parents"), "{reason}");
        let refused = fake
            .requests()
            .into_iter()
            .find(|r| {
                r.target
                    .starts_with(&format!("{}/steps", &cont[fake.base().len()..]))
            })
            .unwrap();
        assert_eq!(refused.step_ids()[0], "t0");
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, cont);
        assert!(!record.has_parent);
        assert_eq!(fake.path_at(&new).unwrap().parent, None);
    }

    /// A frozen continuation is continued again: the new path descends
    /// from a step the continuation owns, and its ids through parents
    /// reach both frozen paths.
    #[test]
    fn a_frozen_continuation_is_continued_again() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (first, _) = record_on(cfg.path(), &d).unwrap();
        fake.freeze(ids_of(&first).unwrap().0);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (second, _) = record_on(cfg.path(), &d).unwrap();
        let second_graph = ids_of(&second).unwrap().0;
        fake.freeze(second_graph);

        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 2, .. }),
            "{out:?}"
        );
        let (third, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str(),
                record.has_parent
            ),
            (7, "t6", "t6", true)
        );
        let continuation = fake.path_at(&third).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["t5", "t6"]));
        let parent = continuation.parent.unwrap();
        assert_eq!(
            (parent.uri, parent.step.as_str()),
            (
                format!("{}/u/alex/x/graphs/{second_graph}", fake.base()),
                "t4"
            )
        );
        let read = cmd_pathbase::path_step_ids(&d.base_url, &d.token, &third, true).unwrap();
        assert_eq!(
            read.step_ids,
            strs(&["t5", "t6", "t3", "t4", "t0", "t1", "t2"])
        );
        assert_eq!(fake.graphs().len(), 3);
    }

    /// An earlier pass stored every step and died before recording it,
    /// then the path was frozen: the refused append finds nothing
    /// missing, so the record catches up and no graph is made.
    #[test]
    fn a_frozen_path_holding_every_step_only_refreshes_the_record() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (key, before) = record_on(cfg.path(), &d).unwrap();
        let unrecorded = session_of(5);
        cmd_pathbase::append_steps(
            &d.base_url,
            &d.token,
            &key,
            &unrecorded,
            unrecorded.steps[2..].to_vec(),
            "t4",
            BATCH_BUDGET,
        )
        .unwrap();
        fake.freeze(ids_of(&key).unwrap().0);

        let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        assert_eq!(out, Outcome::UpToDate);
        assert_eq!(fake.graphs().len(), 1);
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, key);
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str(),
                record.has_parent
            ),
            (5, "t4", before.head.as_str(), false)
        );
    }

    /// The continuation's create is refused for an id stored above a
    /// parent the caller cannot read: the session starts a fresh graph.
    #[test]
    fn a_continuation_refused_for_an_inherited_id_recreates() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let (first, _) = record_on(cfg.path(), &d).unwrap();
        let first_graph = ids_of(&first).unwrap().0;
        fake.freeze(first_graph);
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (second, _) = record_on(cfg.path(), &d).unwrap();
        fake.hide(first_graph);
        fake.freeze(ids_of(&second).unwrap().0);

        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        let Outcome::Recreated { steps, reason, .. } = &out else {
            panic!("{out:?}");
        };
        assert_eq!(*steps, 7);
        assert!(reason.contains("parents"), "{reason}");
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert!(!record.has_parent);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            ids(&session_of(7).steps)
        );
    }

    /// The blocker's scenario: a continuation owns `t5, t6`, then the
    /// session is truncated below the boundary and regrows along
    /// `n2..n5`. The new steps land on the continuation with the edges
    /// among them intact, and once it is frozen the next continuation
    /// descends from the regrown line.
    #[test]
    fn a_continuation_truncated_below_its_boundary_and_regrown_keeps_its_edges() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        let (first, _) = record_on(cfg.path(), &d).unwrap();
        fake.freeze(ids_of(&first).unwrap().0);
        push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        let (cont, _) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(fake.path_at(&cont).unwrap().step_ids(), strs(&["t5", "t6"]));

        let regrown = |n: usize| {
            let mut path = session_of(2);
            for i in 2..n {
                let parent = if i == 2 {
                    "t1".into()
                } else {
                    format!("n{}", i - 1)
                };
                path.steps.push(turn_step(
                    &format!("n{i}"),
                    Some(&parent),
                    Some(&format!("h{i}")),
                    &[],
                ));
            }
            path.path.head = format!("n{}", n - 1);
            path
        };
        fake.clear_log();
        let out = push_to(cfg.path(), &d, &regrown(6), STAMP).unwrap();
        assert_eq!(
            out,
            Outcome::Appended {
                steps: 4,
                resent: 0
            }
        );
        assert_eq!(fake.writes()[0].step_ids(), strs(&["n2", "n3", "n4", "n5"]));
        assert!(sent_parents(&fake, "n2").is_empty());
        assert_eq!(sent_parents(&fake, "n3"), strs(&["n2"]));
        assert_eq!(sent_parents(&fake, "n5"), strs(&["n4"]));
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, cont);
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str(),
                record.has_parent
            ),
            (6, "n5", "n5", true)
        );
        assert_eq!(
            fake.path_at(&cont).unwrap().step_ids(),
            strs(&["t5", "t6", "n2", "n3", "n4", "n5"])
        );

        let cont_graph = ids_of(&cont).unwrap().0;
        fake.freeze(cont_graph);
        let out = push_to(cfg.path(), &d, &regrown(7), STAMP).unwrap();
        assert!(
            matches!(out, Outcome::Continued { steps: 1, .. }),
            "{out:?}"
        );
        let (next, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(
            (record.steps, record.last_step.as_str(), record.has_parent),
            (7, "n6", true)
        );
        let continuation = fake.path_at(&next).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["n6"]));
        let parent = continuation.parent.unwrap();
        assert_eq!(
            (parent.uri, parent.step.as_str()),
            (
                format!("{}/u/alex/x/graphs/{cont_graph}", fake.base()),
                "n5"
            )
        );
    }

    /// A release Pathbase answers a route it lacks with its web app's
    /// page: the session fails naming the route, nothing is recreated.
    #[test]
    fn a_server_answering_the_append_with_a_page_fails_instead_of_recreating() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        let before = record_on(cfg.path(), &d).unwrap();
        fake.force("POST", "/steps", 200, "<!doctype html><html></html>");
        let err = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not support incremental sharing") && msg.contains("append route"),
            "{msg}"
        );
        assert_eq!(fake.graphs().len(), 1);
        assert_eq!(record_on(cfg.path(), &d).unwrap(), before);
    }

    /// A server that does not list the stored paths: a single share
    /// warns and records nothing; a bulk pass fails the session and
    /// leaves no graph behind.
    #[test]
    fn a_create_without_stored_paths_records_nothing() {
        let fake = FakePathbase::start();
        fake.unlist_created_paths();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let out = push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        assert!(matches!(out, Outcome::Created { .. }), "{out:?}");
        assert_eq!(record_count(cfg.path()), 0);
        assert_eq!(fake.graphs().len(), 1);

        let session = Session {
            artifact_type: ArtifactType::Claude,
            id: "s",
            path: &session_of(3),
            stamp: STAMP,
        };
        let err = push(cfg.path(), &d, &session, true).unwrap_err();
        assert!(
            err.to_string().contains("does not report stored paths"),
            "{err}"
        );
        assert_eq!(record_count(cfg.path()), 0);
        assert_eq!(fake.graphs().len(), 1, "the bulk pass's graph is deleted");
    }

    /// The bulk pass's graph cannot be deleted again: the session's
    /// error names it, so the user can delete it.
    #[test]
    fn a_failed_cleanup_delete_names_the_graph_left_behind() {
        let fake = FakePathbase::start();
        fake.unlist_created_paths();
        fake.force(
            "DELETE",
            "/graphs/",
            403,
            r#"{"code":"forbidden","error":"no"}"#,
        );
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let session = Session {
            artifact_type: ArtifactType::Claude,
            id: "s",
            path: &session_of(3),
            stamp: STAMP,
        };
        let err = push(cfg.path(), &d, &session, true).unwrap_err();
        let left = fake.graphs();
        assert_eq!(left.len(), 1);
        let url = format!("{}/u/alex/x/graphs/{}", fake.base(), left[0].id);
        assert!(err.to_string().contains(&url), "{err}");
        assert!(err.to_string().contains("could not be deleted"), "{err}");
    }
}
