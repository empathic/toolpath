//! Append one session to its remote path on one destination: the sync
//! client's single write path, which `path share` and `path share --all`
//! both call. See
//! `docs/superpowers/specs/2026-10-13-path-sync-client-design.md`,
//! "Append: one session, one destination".
//!
//! Only settled steps are pushed (see [`settled_len`]), and a pushed step
//! is never sent again: the remote record says how far into the local
//! derive the remote chain reaches, so a pass sends the steps past that
//! point and nothing else. When the record no longer describes the
//! derive, the steps the server lacks are found by id; a frozen path is
//! continued in a new graph; a path the server no longer has is
//! recreated.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
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

/// What one append did. Each carrying `url` names the graph the
/// session's record now points into, as a page URL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    /// No record: a new graph with the `steps` settled steps.
    Created { url: String, steps: usize },
    /// `steps` new steps appended to the recorded path.
    Appended { url: String, steps: usize },
    /// The recorded path already holds every settled step.
    UpToDate { url: String },
    /// The session has no settled step yet; nothing sent, no record.
    NothingSettled,
    /// The recorded path is frozen: a new graph continuing it, holding
    /// the `steps` steps the frozen one lacks.
    Continued { url: String, steps: usize },
    /// The recorded path could not be appended to (`reason`): a new graph
    /// with all `steps` settled steps replaced it.
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
            Outcome::Appended { steps, .. } => format!("appended {}", plural(*steps, "step")),
            Outcome::UpToDate { .. } => "skipped: the remote holds every settled step".into(),
            Outcome::NothingSettled => "skipped: no settled steps yet".into(),
            Outcome::Continued { steps, .. } => format!(
                "continued: the remote path is frozen; a new graph continues it with {}",
                plural(*steps, "step")
            ),
            Outcome::Recreated { steps, reason, .. } => {
                format!("recreated ({}): {reason}", plural(*steps, "step"))
            }
        }
    }

    /// The graph the session's record points into, if it has one.
    pub(crate) fn url(&self) -> Option<&str> {
        match self {
            Outcome::Created { url, .. }
            | Outcome::Appended { url, .. }
            | Outcome::UpToDate { url }
            | Outcome::Continued { url, .. }
            | Outcome::Recreated { url, .. } => Some(url),
            Outcome::NothingSettled => None,
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

/// Push `session`'s settled steps to `dest` and record where they are,
/// in the remotes file under `config_dir`. An error means the session
/// failed: nothing was recorded, and the next pass starts from the
/// record as it was.
pub(crate) fn push(
    config_dir: &std::path::Path,
    dest: &Destination,
    session: &Session,
) -> Result<Outcome> {
    let k = settled_len(&session.path.steps);
    if k == 0 {
        return Ok(Outcome::NothingSettled);
    }
    let push = Push {
        config_dir,
        dest,
        session,
        k,
        head: settled_head(session.path, k),
    };
    let remotes = crate::sync::load_remotes(config_dir)?;
    match remotes.at(session.artifact_type, session.id, &dest.probe()) {
        None => push.create(),
        Some((url, record)) => push.update(url, record),
    }
}

/// One session's push in progress: `k` settled steps, `head` the
/// settled head.
struct Push<'a> {
    config_dir: &'a std::path::Path,
    dest: &'a Destination,
    session: &'a Session<'a>,
    k: usize,
    head: String,
}

impl Push<'_> {
    fn steps(&self) -> &[Step] {
        &self.session.path.steps
    }

    /// Append step 3: no record, so a new graph of `S[0..k)`.
    fn create(&self) -> Result<Outcome> {
        let (url, page) = self.create_prefix()?;
        self.record_created(url.as_deref(), self.head.clone())?;
        Ok(Outcome::Created {
            url: page,
            steps: self.k,
        })
    }

    /// Steps 4 to 7: a record at `url` says the remote holds
    /// `record.steps` steps of the derive.
    fn update(&self, url: &str, record: &RemoteRecord) -> Result<Outcome> {
        let prefix_holds = record.steps > 0
            && self.k >= record.steps
            && self.steps()[record.steps - 1].step.id == record.last_step;
        if !prefix_holds {
            return self.update_by_ids(url, record);
        }
        if self.k == record.steps {
            if record.source_stamp != SourceStamp::known(self.session.stamp) {
                self.record(url, record.head.clone())?;
            }
            return Ok(Outcome::UpToDate { url: page_url(url) });
        }
        let missing: Vec<usize> = (record.steps..self.k).collect();
        match self.append(url, &missing) {
            Ok(()) => {
                self.record(url, self.head.clone())?;
                Ok(Outcome::Appended {
                    url: page_url(url),
                    steps: missing.len(),
                })
            }
            Err(AppendError::Frozen) => self.continuation(url, record, &missing),
            Err(AppendError::Gone) => self.recreate("the remote path is gone"),
            // The prefix's last id matched but a new step does not fit
            // what is stored: let the ids decide.
            Err(AppendError::InvalidDocument(_)) => self.update_by_ids(url, record),
            Err(AppendError::Other(msg)) => Err(anyhow!(msg)),
        }
    }

    /// Step 5: the record does not describe the derive. Send every
    /// settled step the server lacks, by id, in document order.
    fn update_by_ids(&self, url: &str, record: &RemoteRecord) -> Result<Outcome> {
        let d = self.dest;
        let stored: HashSet<String> =
            match cmd_pathbase::path_step_ids(&d.base_url, &d.token, url, true) {
                Ok(ids) => ids.into_iter().collect(),
                Err(AppendError::Gone) => return self.recreate("the remote path is gone"),
                Err(e) => return Err(anyhow!("read the remote path's step ids: {e}")),
            };
        let missing: Vec<usize> = (0..self.k)
            .filter(|&i| !stored.contains(&self.steps()[i].step.id))
            .collect();
        if missing.is_empty() {
            // The server's extra steps stay where they are, and so does
            // the head it was last told.
            self.record(url, record.head.clone())?;
            return Ok(Outcome::UpToDate { url: page_url(url) });
        }
        match self.append(url, &missing) {
            Ok(()) => {
                self.record(url, self.head.clone())?;
                Ok(Outcome::Appended {
                    url: page_url(url),
                    steps: missing.len(),
                })
            }
            Err(AppendError::Frozen) => self.continuation(url, record, &missing),
            Err(AppendError::Gone) => self.recreate("the remote path is gone"),
            Err(AppendError::InvalidDocument(msg)) => self.recreate(&format!(
                "the derive no longer fits the remote path ({msg})"
            )),
            Err(AppendError::Other(msg)) => Err(anyhow!(msg)),
        }
    }

    /// Step 6: the path at `url` is frozen. Create a graph whose path
    /// continues it from the head it was last told and holds `missing`,
    /// the steps the frozen path lacks. A parent outside the new
    /// document is dropped from each step: its root steps descend from
    /// `parent.step` implicitly.
    fn continuation(&self, url: &str, record: &RemoteRecord, missing: &[usize]) -> Result<Outcome> {
        let uri = cmd_pathbase::graph_document_url(url)
            .ok_or_else(|| anyhow!("{url} is not a Pathbase path URL"))?;
        let owned: HashSet<&str> = missing
            .iter()
            .map(|&i| self.steps()[i].step.id.as_str())
            .collect();
        let head = if owned.contains(self.head.as_str()) {
            self.head.clone()
        } else {
            self.steps()[*missing.last().expect("a continuation has steps")]
                .step
                .id
                .clone()
        };
        let steps = missing
            .iter()
            .map(|&i| {
                let mut step = self.steps()[i].clone();
                step.step.parents.retain(|p| owned.contains(p.as_str()));
                step
            })
            .collect();
        let path = &self.session.path;
        let continuation = Path {
            path: PathIdentity {
                head: head.clone(),
                parent: Some(Parent {
                    uri,
                    path: path.path.id.clone(),
                    step: record.head.clone(),
                }),
                ..path.path.clone()
            },
            steps,
            meta: path.meta.clone(),
        };
        let (new_url, page) = self.create_graph(continuation)?;
        self.record_created(new_url.as_deref(), head)?;
        Ok(Outcome::Continued {
            url: page,
            steps: missing.len(),
        })
    }

    /// Step 7: a new graph of `S[0..k)` replaces the recorded one.
    fn recreate(&self, reason: &str) -> Result<Outcome> {
        let (url, page) = self.create_prefix()?;
        self.record_created(url.as_deref(), self.head.clone())?;
        Ok(Outcome::Recreated {
            url: page,
            steps: self.k,
            reason: reason.to_string(),
        })
    }

    fn append(&self, url: &str, steps: &[usize]) -> std::result::Result<(), AppendError> {
        let d = self.dest;
        cmd_pathbase::append_steps(
            &d.base_url,
            &d.token,
            url,
            self.session.path,
            steps,
            &self.head,
            BATCH_BUDGET,
        )
    }

    /// Create a graph of the settled prefix `S[0..k)`.
    fn create_prefix(&self) -> Result<(Option<String>, String)> {
        let path = &self.session.path;
        self.create_graph(Path {
            path: PathIdentity {
                head: self.head.clone(),
                ..path.path.clone()
            },
            steps: path.steps[..self.k].to_vec(),
            meta: path.meta.clone(),
        })
    }

    /// Create a graph holding `path`; its stored path's API URL (the
    /// record key), `None` from a server that does not list the stored
    /// paths, and the graph's page URL.
    fn create_graph(&self, path: Path) -> Result<(Option<String>, String)> {
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
        let url = created.paths.first().map(|stored| {
            cmd_pathbase::remote_path_url(
                &d.base_url,
                &d.owner,
                &d.repo,
                &created.id,
                &stored.server_id,
            )
        });
        Ok((url, created.url))
    }

    /// [`Self::record`] for a graph just created. A server that did not
    /// list the stored path leaves nothing to record: warn, so the user
    /// knows the next share will create another graph.
    fn record_created(&self, url: Option<&str>, head: String) -> Result<()> {
        match url {
            Some(url) => self.record(url, head),
            None => {
                eprintln!(
                    "warning: remote record not written: the server did not list the stored paths"
                );
                Ok(())
            }
        }
    }

    /// Record that the remote at `url` holds the derive through `S[k-1]`,
    /// last told `head`. Replaces the session's record on this
    /// destination.
    fn record(&self, url: &str, head: String) -> Result<()> {
        let record = RemoteRecord {
            steps: self.k,
            last_step: self.steps()[self.k - 1].step.id.clone(),
            head,
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

/// The page URL of the graph holding the path at API URL `url`, or
/// `url` itself when it is not one.
fn page_url(url: &str) -> String {
    cmd_pathbase::graph_document_url(url).unwrap_or_else(|| url.to_string())
}

/// How many leading steps are settled: followed by a later turn, so
/// final. The last message group (the steps from the first turn of the
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

/// The head among the first `k` steps: the path's head when settled,
/// else its nearest settled ancestor along first parents, else `S[k-1]`.
pub(crate) fn settled_head(path: &Path, k: usize) -> String {
    let settled: HashSet<&str> = path.steps[..k].iter().map(|s| s.step.id.as_str()).collect();
    let by_id: HashMap<&str, &Step> = path.steps.iter().map(|s| (s.step.id.as_str(), s)).collect();
    let mut cur = path.path.head.as_str();
    for _ in 0..=path.steps.len() {
        if settled.contains(cur) {
            return cur.to_string();
        }
        match by_id.get(cur).and_then(|s| s.step.parents.first()) {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    path.steps[k - 1].step.id.clone()
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
    /// ids `t0..t{n-1}`: `n - 1` of them settled.
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

    #[test]
    fn settled_head_walks_first_parents_back_into_the_settled_prefix() {
        let mut path = path_of(vec![
            turn_step("a", None, Some("g1"), &[]),
            event_step("e", Some("a")),
            turn_step("b", Some("e"), Some("g2"), &[]),
            event_step("e2", Some("b")),
        ]);
        assert_eq!(settled_len(&path.steps), 2);
        assert_eq!(settled_head(&path, 2), "e");
        path.path.head = "a".into();
        assert_eq!(settled_head(&path, 2), "a");
        path.path.head = "nowhere".into();
        assert_eq!(settled_head(&path, 2), "e");
    }

    // ── push ───────────────────────────────────────────────────────

    #[test]
    fn nothing_settled_sends_nothing_and_records_nothing() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let out = push_to(cfg.path(), &dest(&fake, "x"), &session_of(1), STAMP).unwrap();
        assert_eq!(out, Outcome::NothingSettled);
        assert!(fake.requests().is_empty());
        assert_eq!(record_count(cfg.path()), 0);
    }

    #[test]
    fn no_record_creates_a_graph_of_the_settled_steps() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        let out = push_to(cfg.path(), &d, &session_of(4), STAMP).unwrap();
        let (key, record) = record_on(cfg.path(), &d).unwrap();
        let stored = fake.path_at(&key).unwrap();
        assert_eq!(stored.step_ids(), strs(&["t0", "t1", "t2"]));
        assert_eq!(stored.head.as_deref(), Some("t2"));
        assert_eq!(
            record,
            RemoteRecord {
                steps: 3,
                last_step: "t2".into(),
                head: "t2".into(),
                source_stamp: SourceStamp::known(STAMP),
            }
        );
        let (graph, _) = ids_of(&key).unwrap();
        assert_eq!(
            out,
            Outcome::Created {
                url: format!("{}/u/alex/x/graphs/{graph}", fake.base()),
                steps: 3,
            }
        );
    }

    #[test]
    fn a_grown_session_appends_only_its_new_settled_steps() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &session_of(6), (None, Some(200))).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 3, .. }), "{out:?}");
        let writes = fake.writes();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].target.ends_with("/steps"), "{}", writes[0].target);
        assert_eq!(writes[0].step_ids(), strs(&["t2", "t3", "t4"]));
        assert_eq!(writes[0].heads(), strs(&["t4"]));
        assert!(!writes[0].body.contains("PathOpen") && !writes[0].body.contains("PathClose"));

        let (key, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(
            fake.path_at(&key).unwrap().step_ids(),
            ids(&session_of(6).steps[..5])
        );
        assert_eq!(
            (
                record.steps,
                record.last_step.as_str(),
                record.head.as_str()
            ),
            (5, "t4", "t4")
        );
        assert_eq!(record.source_stamp, SourceStamp::known((None, Some(200))));
        assert_eq!(fake.graphs().len(), 1);
    }

    #[test]
    fn an_unchanged_session_sends_nothing_and_refreshes_the_stamp() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        fake.clear_log();

        // The open turn grew, but nothing new settled.
        let mut grown = session_of(3);
        grown.steps[2] = turn_step("t2", Some("t1"), Some("g2"), &[false]);
        let out = push_to(cfg.path(), &d, &grown, (None, Some(150))).unwrap();
        assert!(matches!(out, Outcome::UpToDate { .. }), "{out:?}");
        assert!(fake.requests().is_empty());
        let (_, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(record.steps, 2);
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
    }

    /// The local file lost steps: the record's position is past the
    /// derive, the ids show the server has every settled step, and the
    /// next growth appends to the same path.
    #[test]
    fn a_truncated_session_continues_in_the_same_path_by_ids() {
        let fake = FakePathbase::start();
        let cfg = tempfile::tempdir().unwrap();
        let d = dest(&fake, "x");
        push_to(cfg.path(), &d, &session_of(6), STAMP).unwrap();
        let (key, before) = record_on(cfg.path(), &d).unwrap();
        fake.clear_log();

        let out = push_to(cfg.path(), &d, &session_of(3), STAMP).unwrap();
        assert!(matches!(out, Outcome::UpToDate { .. }), "{out:?}");
        let reqs = fake.requests();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].target.ends_with("/step-ids?through_parents=true"));
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, key);
        assert_eq!((record.steps, record.last_step.as_str()), (2, "t1"));
        assert_eq!(record.head, before.head, "the server's head is unchanged");

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
        assert!(matches!(out, Outcome::Appended { steps: 1, .. }), "{out:?}");
        assert_eq!(fake.writes()[0].step_ids(), strs(&["n2"]));
        assert_eq!(
            fake.path_at(&key).unwrap().step_ids(),
            strs(&["t0", "t1", "t2", "t3", "t4", "n2"])
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
        assert_eq!(fake.writes()[0].step_ids(), strs(&["late", "t2"]));
        let (same, record) = record_on(cfg.path(), &d).unwrap();
        assert_eq!(same, key);
        assert_eq!((record.steps, record.last_step.as_str()), (4, "t2"));
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
        assert_eq!(*steps, 4);
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
            ids(&session_of(5).steps[..4])
        );
        assert_eq!(record.steps, 4);
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
                record.head.as_str()
            ),
            (4, "t3", "t3")
        );
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), strs(&["t2", "t3"]));
        let parent = continuation.parent.unwrap();
        assert_eq!(
            parent.uri,
            format!("{}/u/alex/x/graphs/{old_graph}", fake.base())
        );
        assert_eq!(parent.path, "path-claude-s");
        assert_eq!(parent.step, "t1");
        assert_eq!(fake.path_at(&old).unwrap().step_ids(), strs(&["t0", "t1"]));

        fake.clear_log();
        let out = push_to(cfg.path(), &d, &session_of(7), STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 2, .. }), "{out:?}");
        assert_eq!(record_on(cfg.path(), &d).unwrap().0, new);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            strs(&["t2", "t3", "t4", "t5"])
        );
        assert_eq!(fake.graphs().len(), 2, "exactly one continuation");
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
        assert_eq!(steps[0]["step"]["id"], "t2");
        assert!(steps[0]["step"].get("parents").is_none(), "{}", steps[0]);
        assert_eq!(steps[1]["step"]["parents"], serde_json::json!(["t2"]));
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
        assert_eq!((*steps, reason.as_str()), (3, "the remote path is gone"));
        let (new, record) = record_on(cfg.path(), &d).unwrap();
        assert_ne!(new, old);
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            strs(&["t0", "t1", "t2"])
        );
        assert_eq!(record.steps, 3);

        let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
        assert!(matches!(out, Outcome::Appended { steps: 1, .. }), "{out:?}");
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
                    source_stamp: None,
                },
            )
            .unwrap();
            let out = push_to(cfg.path(), &d, &session_of(5), STAMP).unwrap();
            assert!(
                matches!(out, Outcome::Recreated { steps: 4, .. }),
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
            strs(&["t0", "t1"])
        );
        assert_eq!(record_on(cfg.path(), &x).unwrap().1.steps, 4);

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
        assert!(err.to_string().contains("does not serve appends"), "{err}");
        assert_eq!(fake.graphs().len(), 1);
    }
}
