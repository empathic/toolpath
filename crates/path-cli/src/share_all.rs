//! `path share --all`: share every session in scope, each through the
//! append engine (`share_append`), so a pass creates the sessions that
//! have no remote yet, appends what the others gained since, and touches
//! nothing that has not changed. See
//! `docs/superpowers/specs/2026-10-13-path-sync-client-design.md`,
//! "Bulk upload".
//!
//! A pass enumerates through the sync engine's sources, so sessions are
//! named as their records name them (a Claude chain by its head) and
//! come with their stat stamps. A session with a record on its
//! destination whose stamp matches the source's is skipped without being
//! read. Every other session is derived and pushed. Sessions run one at a
//! time; one failing is reported and the pass goes on.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::artifact::{ArtifactRef, ArtifactType};
use crate::harness::HarnessBundle;
use crate::share_append::{self, Destination, Outcome, plural};
use crate::sync::sources::{self, ArtifactSource};
use crate::sync::{Manifest, Remotes, SourceStamp};

/// What a pass will do with one session, decided before deriving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    /// No record on its destination: create.
    Create,
    /// A record whose stamp matches the source's: nothing to read.
    Unchanged,
    /// A record, and the stamp is unknown or differs: derive, append.
    Update,
}

/// The spec's table: no record → create; a record and a known stamp
/// equal to the record's → skip; otherwise derive and append.
pub(crate) fn plan(record_stamp: Option<Option<SourceStamp>>, stamp: sources::Stamp) -> Plan {
    match record_stamp {
        None => Plan::Create,
        Some(recorded) => match SourceStamp::known(stamp) {
            Some(now) if recorded == Some(now) => Plan::Unchanged,
            _ => Plan::Update,
        },
    }
}

/// One session in scope.
pub(crate) struct Item<'a> {
    pub(crate) artifact: ArtifactRef,
    /// The directory the session belongs to, when known: where its
    /// configured remote is looked up before it is derived.
    pub(crate) dir: Option<String>,
    pub(crate) source: &'a dyn ArtifactSource,
    pub(crate) dest: Destination,
    pub(crate) plan: Plan,
}

/// Every session of `types` in `bundle` under `project_under`, with its
/// destination (`destination` maps a session directory to one) and its
/// plan against the records in `remotes`. Sorted by destination, then
/// harness, then id, so a pass writes to one destination at a time.
pub(crate) fn gather<'a>(
    sources: &'a [Box<dyn ArtifactSource + 'a>],
    project_under: Option<&Path>,
    manifest: &Manifest,
    remotes: &Remotes,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
) -> Result<Vec<Item<'a>>> {
    let mut items = Vec::new();
    for source in sources {
        for artifact in source.enumerate(project_under) {
            let dir = artifact
                .path
                .clone()
                .or_else(|| {
                    manifest
                        .get(artifact.artifact_type.name())
                        .and_then(|records| records.get(&artifact.id))
                        .and_then(|r| r.path.clone())
                })
                .or_else(|| source.peek_dir(&artifact.id));
            if let Some(under) = project_under
                && !dir.as_deref().is_some_and(|d| source.in_scope(d, under))
            {
                continue;
            }
            let dest = destination(dir.as_deref().map(Path::new))?;
            let record = remotes
                .at(artifact.artifact_type, &artifact.id, &dest.probe())
                .map(|(_, r)| r.source_stamp);
            let plan = plan(record, (artifact.modified, artifact.size));
            items.push(Item {
                artifact,
                dir,
                source: source.as_ref(),
                dest,
                plan,
            });
        }
    }
    items.sort_by(|a, b| {
        (
            dest_key(&a.dest),
            a.artifact.artifact_type.name(),
            &a.artifact.id,
        )
            .cmp(&(
                dest_key(&b.dest),
                b.artifact.artifact_type.name(),
                &b.artifact.id,
            ))
    });
    Ok(items)
}

fn dest_key(d: &Destination) -> (String, String, String) {
    (d.base_url.clone(), d.owner.clone(), d.repo.clone())
}

/// The sources for `types` in `bundle`.
pub(crate) fn sources_for<'a>(
    bundle: &'a HarnessBundle,
    types: &[ArtifactType],
) -> Vec<Box<dyn ArtifactSource + 'a>> {
    types
        .iter()
        .filter_map(|&t| sources::source_for(bundle, t))
        .collect()
}

/// What one session of a pass came to.
#[derive(Debug)]
pub(crate) enum SessionResult {
    Unchanged,
    Pushed(Outcome),
    Failed(String),
}

/// A finished pass: each session, the destination it went to, and what
/// happened.
#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) sessions: Vec<(ArtifactType, String, String, SessionResult)>,
}

impl Report {
    pub(crate) fn failed(&self) -> usize {
        self.sessions
            .iter()
            .filter(|(.., r)| matches!(r, SessionResult::Failed(_)))
            .count()
    }

    /// The closing summary: one line per kind of outcome that occurred,
    /// then one line per destination written to.
    pub(crate) fn render(&self) -> String {
        let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        let mut bump = |kind: &'static str, steps: usize| {
            let e = counts.entry(kind).or_default();
            e.0 += 1;
            e.1 += steps;
        };
        let mut by_dest: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, _, dest, result) in &self.sessions {
            match result {
                SessionResult::Unchanged => bump("unchanged", 0),
                SessionResult::Failed(_) => bump("failed", 0),
                SessionResult::Pushed(o) => {
                    match o {
                        Outcome::Created { steps, .. } => bump("created", *steps),
                        Outcome::Appended { steps, .. } => bump("appended", *steps),
                        Outcome::UpToDate { .. } => bump("up to date", 0),
                        Outcome::NothingSettled => bump("nothing settled", 0),
                        Outcome::Continued { steps, .. } => bump("continued", *steps),
                        Outcome::Recreated { steps, .. } => bump("recreated", *steps),
                    }
                    if matches!(
                        o,
                        Outcome::Created { .. }
                            | Outcome::Appended { .. }
                            | Outcome::Continued { .. }
                            | Outcome::Recreated { .. }
                    ) {
                        *by_dest.entry(dest.as_str()).or_default() += 1;
                    }
                }
            }
        }
        let mut out = format!("{} in scope\n", plural(self.sessions.len(), "session"));
        let order = [
            ("created", true),
            ("appended", true),
            ("continued", true),
            ("recreated", true),
            ("up to date", false),
            ("unchanged", false),
            ("nothing settled", false),
            ("failed", false),
        ];
        for (kind, with_steps) in order {
            let Some((n, steps)) = counts.get(kind) else {
                continue;
            };
            if with_steps {
                out.push_str(&format!(
                    "  {kind:<16}{n:>5}  ({})\n",
                    plural(*steps, "step")
                ));
            } else {
                out.push_str(&format!("  {kind:<16}{n:>5}\n"));
            }
        }
        if !by_dest.is_empty() {
            out.push_str("Written to:\n");
            for (dest, n) in by_dest {
                out.push_str(&format!("  {dest}  {}\n", plural(n, "session")));
            }
        }
        let failures: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(t, id, _, r)| match r {
                SessionResult::Failed(why) => Some((t, id, why)),
                _ => None,
            })
            .collect();
        if !failures.is_empty() {
            out.push_str("Failed:\n");
            for (t, id, why) in failures {
                out.push_str(&format!("  {} {id}: {why}\n", t.name()));
            }
        }
        out
    }
}

/// Run one pass over `items`: derive and push each that needs it,
/// sequentially, reporting each as it finishes on stderr. A session
/// whose derive or push fails is reported and the pass goes on.
/// `cached` returns a session's cached document when the cache is
/// current for it, sparing the derive.
pub(crate) fn run_pass(
    config_dir: &Path,
    items: &[Item],
    cached: &dyn Fn(&ArtifactRef) -> Option<toolpath::v1::Graph>,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
) -> Report {
    let mut report = Report::default();
    let total = items.len();
    for (n, item) in items.iter().enumerate() {
        let a = &item.artifact;
        let label = format!("[{}/{total}] {} {}", n + 1, a.artifact_type.name(), a.id);
        if item.plan == Plan::Unchanged {
            report.sessions.push((
                a.artifact_type,
                a.id.clone(),
                item.dest.display(),
                SessionResult::Unchanged,
            ));
            continue;
        }
        let (dest, result) = match push_item(config_dir, item, cached, destination) {
            Ok((dest, outcome)) => {
                let url = outcome.url().map(|u| format!(" → {u}")).unwrap_or_default();
                eprintln!("{label}: {}{url}", outcome.describe());
                (dest, SessionResult::Pushed(outcome))
            }
            Err(e) => {
                eprintln!("{label}: failed: {e:#}");
                (item.dest.display(), SessionResult::Failed(format!("{e:#}")))
            }
        };
        report
            .sessions
            .push((a.artifact_type, a.id.clone(), dest, result));
    }
    report
}

/// Derive (or read from the cache) and push one session. The derived
/// document's recorded working directory, when it has one, picks the
/// destination: it is exact where the listing's directory may be a
/// lossy decoding.
fn push_item(
    config_dir: &Path,
    item: &Item,
    cached: &dyn Fn(&ArtifactRef) -> Option<toolpath::v1::Graph>,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
) -> Result<(String, Outcome)> {
    let a = &item.artifact;
    let doc = match cached(a) {
        Some(doc) => doc,
        None => item.source.derive(a).context("derive")?.doc,
    };
    let path = doc
        .single_path()
        .context("the session did not derive to a single path")?;
    let dest = match crate::cmd_share::doc_session_dir(&doc) {
        Some(dir) if item.dir.as_deref() != dir.to_str() => destination(Some(&dir))?,
        _ => item.dest.clone(),
    };
    let outcome = share_append::push(
        config_dir,
        &dest,
        &share_append::Session {
            artifact_type: a.artifact_type,
            id: &a.id,
            path,
            stamp: (a.modified, a.size),
        },
    )?;
    Ok((dest.display(), outcome))
}

/// The heading before a pass: how many sessions are in scope and what
/// the pass will do with them, per harness.
pub(crate) fn render_plan(items: &[Item], project_under: Option<&Path>) -> String {
    let count = |p: Plan| items.iter().filter(|i| i.plan == p).count();
    let scope = match project_under {
        Some(p) => format!(" under {}", p.display()),
        None => String::new(),
    };
    let mut out = format!(
        "Found {}{scope}: {} new, {} to bring up to date, {} unchanged\n",
        plural(items.len(), "session"),
        count(Plan::Create),
        count(Plan::Update),
        count(Plan::Unchanged)
    );
    let mut per_harness: BTreeMap<&str, usize> = BTreeMap::new();
    let mut per_dest: BTreeMap<String, usize> = BTreeMap::new();
    for i in items {
        *per_harness
            .entry(i.artifact.artifact_type.name())
            .or_default() += 1;
        if i.plan != Plan::Unchanged {
            *per_dest.entry(i.dest.display()).or_default() += 1;
        }
    }
    let harnesses: Vec<String> = per_harness
        .iter()
        .map(|(h, n)| format!("{h} {n}"))
        .collect();
    out.push_str(&format!("  {}\n", harnesses.join(", ")));
    for (dest, n) in per_dest {
        out.push_str(&format!("  → {dest}: {}\n", plural(n, "session")));
    }
    out
}

/// The cached document for `artifact` when the manifest records it
/// materialized at exactly the source's current stamp.
pub(crate) fn cached_doc(
    manifest: &Manifest,
    artifact: &ArtifactRef,
) -> Option<toolpath::v1::Graph> {
    let rec = manifest
        .get(artifact.artifact_type.name())?
        .get(&artifact.id)?;
    let known = rec.modified.is_some() || rec.size.is_some();
    if !known || rec.modified != artifact.modified || rec.size != artifact.size {
        return None;
    }
    let path = crate::cache::cache_path(rec.cache_id.as_deref()?).ok()?;
    toolpath::v1::Graph::from_json(&std::fs::read_to_string(path).ok()?).ok()
}

/// Options for [`run`], from `path share --all`.
pub(crate) struct AllArgs {
    pub(crate) harness: Option<ArtifactType>,
    pub(crate) project_under: Option<PathBuf>,
    pub(crate) url: Option<String>,
    pub(crate) repo: Option<crate::remote::RepoSpec>,
    pub(crate) public: bool,
    pub(crate) dry_run: bool,
    pub(crate) yes: bool,
}

/// `path share --all`.
pub(crate) fn run(
    args: AllArgs,
    base_url: String,
    auth: crate::cmd_pathbase::AuthMode,
) -> Result<()> {
    let crate::cmd_pathbase::AuthMode::Authed { token, username } = auth else {
        anyhow::bail!(
            "`share --all` uploads to your account and needs login: run `path auth login --url {base_url}`"
        );
    };
    let config_dir = crate::config::config_dir()?;
    let bundle = HarnessBundle::from_environment();
    let types: Vec<ArtifactType> = match args.harness {
        Some(h) => vec![h],
        None => ArtifactType::ALL.to_vec(),
    };
    let sources = sources_for(&bundle, &types);
    let manifest = crate::sync::load_manifest(&config_dir).unwrap_or_default();
    let remotes = crate::sync::load_remotes(&config_dir)?;

    let destination = |dir: Option<&Path>| -> Result<Destination> {
        destination_for(&args, &base_url, &token, &username, dir)
    };
    let items = gather(
        &sources,
        args.project_under.as_deref(),
        &manifest,
        &remotes,
        &destination,
    )?;
    eprint!("{}", render_plan(&items, args.project_under.as_deref()));
    let work = items.iter().filter(|i| i.plan != Plan::Unchanged).count();
    if work == 0 {
        eprintln!("Nothing to share.");
        return Ok(());
    }
    if args.dry_run {
        return Ok(());
    }
    if !args.yes {
        let answer = crate::cmd_pathbase::prompt_line(&format!(
            "Share {}? [y/N] ",
            plural(work, "session")
        ))?;
        if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("Aborted.");
            std::process::exit(130);
        }
    }
    // The default destination is created on first use, once per pass.
    let pathstash = |d: &Destination| d.owner == username && d.repo == "pathstash";
    if args.repo.is_none() && items.iter().any(|i| pathstash(&i.dest)) {
        crate::cmd_pathbase::repos_post(&base_url, &token, &username, "pathstash")?;
    }
    let report = run_pass(
        &config_dir,
        &items,
        &|a| cached_doc(&manifest, a),
        &destination,
    );
    eprint!("{}", report.render());
    let failed = report.failed();
    if failed > 0 {
        anyhow::bail!("{} failed", plural(failed, "session"));
    }
    Ok(())
}

/// The destination for a session in `dir`: `--repo` when given, else the
/// remote configured for the directory, else the user's `pathstash` on
/// `base_url`.
fn destination_for(
    args: &AllArgs,
    base_url: &str,
    token: &str,
    username: &str,
    dir: Option<&Path>,
) -> Result<Destination> {
    let dest = |base: &str, owner: &str, repo: &str| Destination {
        base_url: base.to_string(),
        token: token.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
        name: None,
        public: args.public,
    };
    if let Some(spec) = &args.repo {
        return Ok(dest(base_url, &spec.owner, &spec.name));
    }
    if let Some(dir) = dir
        && let Some(found) = crate::share_config::resolve_remote(dir)?
    {
        let base = match (&args.url, &found.base_url) {
            (None, Some(remote)) => remote.as_str(),
            _ => base_url,
        };
        return Ok(dest(base, &found.repo.owner, &found.repo.name));
    }
    Ok(dest(base_url, username, "pathstash"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_pathbase::{FakePathbase, ids_of};
    use crate::sync::RemoteRecord;
    use tempfile::TempDir;

    const CLAUDE_ID: &str = "89f4d66a-510b-48b3-94b8-d6a8f981b143";
    const CODEX_ID: &str = "019e03b2-11a2-7320-9098-5d831813e66c";

    fn fixture(name: &str) -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-fixtures")
            .join(name);
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    /// Temp Claude and Codex homes and the config dir.
    struct Harnesses {
        home: TempDir,
        cfg: TempDir,
    }

    impl Harnesses {
        fn new() -> Self {
            Self {
                home: TempDir::new().unwrap(),
                cfg: TempDir::new().unwrap(),
            }
        }

        fn bundle(&self) -> HarnessBundle {
            let claude = toolpath_claude::PathResolver::new()
                .with_claude_dir(self.home.path().join(".claude"));
            let codex =
                toolpath_codex::PathResolver::new().with_codex_dir(self.home.path().join(".codex"));
            HarnessBundle {
                claude: Some(toolpath_claude::ClaudeConvo::with_resolver(claude)),
                codex: Some(toolpath_codex::CodexConvo::with_resolver(codex)),
                ..Default::default()
            }
        }

        fn claude_file(&self, id: &str) -> PathBuf {
            self.home
                .path()
                .join(".claude/projects/-test-project")
                .join(format!("{id}.jsonl"))
        }

        /// The Claude fixture's first `n` lines as session `id`.
        fn claude(&self, id: &str, n: usize) {
            write_lines(&self.claude_file(id), &fixture("claude/convo.jsonl")[..n]);
        }

        /// The Codex fixture's first `n` lines.
        fn codex(&self, n: usize) {
            let file = self
                .home
                .path()
                .join(".codex/sessions/2026/05/07")
                .join(format!("rollout-2026-05-07T18-27-42-{CODEX_ID}.jsonl"));
            write_lines(&file, &fixture("codex/convo.jsonl")[..n]);
        }

        fn cfg(&self) -> &Path {
            self.cfg.path()
        }

        /// One pass to `dest` for every session, uncached.
        fn pass(&self, dest: &Destination) -> (Vec<Plan>, Report) {
            self.pass_with(&|_| Ok(dest.clone()), &|_| {})
        }

        /// One pass, `between` running after enumeration, before the
        /// pushes.
        fn pass_with(
            &self,
            destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
            between: &dyn Fn(&[Item]),
        ) -> (Vec<Plan>, Report) {
            let bundle = self.bundle();
            let sources = sources_for(&bundle, &[ArtifactType::Claude, ArtifactType::Codex]);
            let remotes = crate::sync::load_remotes(self.cfg()).unwrap();
            let items =
                gather(&sources, None, &Manifest::default(), &remotes, destination).unwrap();
            between(&items);
            let plans = items.iter().map(|i| i.plan).collect();
            (plans, run_pass(self.cfg(), &items, &|_| None, destination))
        }

        fn record(
            &self,
            t: ArtifactType,
            id: &str,
            dest: &Destination,
        ) -> Option<(String, RemoteRecord)> {
            crate::sync::load_remotes(self.cfg())
                .unwrap()
                .at(t, id, &dest.probe())
                .map(|(k, r)| (k.to_string(), r.clone()))
        }

        /// The settled step ids of the Claude session as it is now.
        fn claude_settled(&self, id: &str) -> Vec<String> {
            let bundle = self.bundle();
            let doc = crate::derive::derive_claude_session_with(
                bundle.claude.as_ref().unwrap(),
                "/test/project",
                id,
            )
            .unwrap()
            .doc;
            let path = doc.single_path().unwrap();
            let k = share_append::settled_len(&path.steps);
            path.steps[..k].iter().map(|s| s.step.id.clone()).collect()
        }
    }

    fn write_lines(file: &Path, lines: &[String]) {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, lines.join("\n") + "\n").unwrap();
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

    fn outcomes(report: &Report) -> Vec<&SessionResult> {
        report.sessions.iter().map(|(.., r)| r).collect()
    }

    fn only(report: &Report) -> &SessionResult {
        assert_eq!(report.sessions.len(), 1, "{report:?}");
        &report.sessions[0].3
    }

    #[test]
    fn plan_follows_the_table() {
        let stamp = (None, Some(10));
        let known = SourceStamp::known(stamp);
        assert_eq!(plan(None, stamp), Plan::Create);
        assert_eq!(plan(None, (None, None)), Plan::Create);
        assert_eq!(plan(Some(known), stamp), Plan::Unchanged);
        assert_eq!(plan(Some(known), (None, Some(11))), Plan::Update);
        // Unknown on either side: derive every pass.
        assert_eq!(plan(Some(None), (None, None)), Plan::Update);
        assert_eq!(plan(Some(known), (None, None)), Plan::Update);
        assert_eq!(plan(Some(None), stamp), Plan::Update);
    }

    /// Sessions with no settled step: only the first prompt, and only
    /// lines that are no turn at all.
    #[test]
    fn an_empty_session_is_skipped_and_writes_no_record() {
        let lines = fixture("claude/convo.jsonl");
        for kept in [&lines[2..3], &lines[..2]] {
            let h = Harnesses::new();
            write_lines(&h.claude_file(CLAUDE_ID), kept);
            assert!(h.claude_settled(CLAUDE_ID).is_empty());
            let fake = FakePathbase::start();
            let d = dest(&fake, "x");
            let (plans, report) = h.pass(&d);
            assert_eq!(plans, [Plan::Create]);
            assert!(matches!(
                only(&report),
                SessionResult::Pushed(Outcome::NothingSettled)
            ));
            assert!(fake.requests().is_empty());
            assert_eq!(h.record(ArtifactType::Claude, CLAUDE_ID, &d), None);
            assert_eq!(report.failed(), 0);
        }
    }

    /// Shared once, then grown: the second pass sends only the steps
    /// that settled since, and a third pass with nothing new is a no-op.
    #[test]
    fn a_grown_session_appends_only_new_settled_steps_and_a_repeat_is_a_no_op() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        let first = h.claude_settled(CLAUDE_ID);
        let (_, report) = h.pass(&d);
        assert!(matches!(
            only(&report),
            SessionResult::Pushed(Outcome::Created { .. })
        ));
        let (key, record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        assert_eq!(fake.path_at(&key).unwrap().step_ids(), first);
        assert_eq!(record.steps, first.len());

        h.claude(CLAUDE_ID, 60);
        let grown = h.claude_settled(CLAUDE_ID);
        assert_eq!(
            grown[..first.len()],
            first[..],
            "the Claude derive is prefix-stable"
        );
        fake.clear_log();
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Update]);
        let SessionResult::Pushed(Outcome::Appended { steps, .. }) = only(&report) else {
            panic!("{report:?}");
        };
        assert_eq!(*steps, grown.len() - first.len());
        let sent: Vec<String> = fake.writes().iter().flat_map(|w| w.step_ids()).collect();
        assert_eq!(sent, grown[first.len()..]);
        assert_eq!(fake.path_at(&key).unwrap().step_ids(), grown);

        fake.clear_log();
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Unchanged]);
        assert!(matches!(only(&report), SessionResult::Unchanged));
        assert!(fake.requests().is_empty(), "{:?}", fake.requests());
        assert_eq!(fake.graphs().len(), 1);
    }

    #[test]
    fn a_session_shared_to_two_destinations_keeps_both_up_to_date() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        h.claude(CLAUDE_ID, 30);
        h.pass(&x);
        h.pass(&y);
        let (x_key, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &x).unwrap();
        let (y_key, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &y).unwrap();
        assert_ne!(x_key, y_key);

        h.claude(CLAUDE_ID, 60);
        let (plans, _) = h.pass(&x);
        assert_eq!(plans, [Plan::Update]);
        let grown = h.claude_settled(CLAUDE_ID);
        assert_eq!(fake.path_at(&x_key).unwrap().step_ids(), grown);
        assert_ne!(fake.path_at(&y_key).unwrap().step_ids(), grown);
        // y's record still holds the old stamp, so y is brought up too.
        let (plans, _) = h.pass(&y);
        assert_eq!(plans, [Plan::Update]);
        assert_eq!(fake.path_at(&y_key).unwrap().step_ids(), grown);
        assert_eq!(fake.graphs().len(), 2);
    }

    #[test]
    fn a_frozen_remote_gets_one_continuation_and_the_next_pass_appends_to_it() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        h.pass(&d);
        let (old, old_record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        fake.freeze(ids_of(&old).unwrap().0);

        h.claude(CLAUDE_ID, 50);
        let (_, report) = h.pass(&d);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::Continued { .. })
            ),
            "{report:?}"
        );
        let (new, record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        assert_ne!(new, old);
        let settled = h.claude_settled(CLAUDE_ID);
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), settled[old_record.steps..]);
        assert_eq!(continuation.parent.unwrap().step, old_record.head);
        assert_eq!(record.steps, settled.len());

        h.claude(CLAUDE_ID, 70);
        let (_, report) = h.pass(&d);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::Appended { .. })
            ),
            "{report:?}"
        );
        assert_eq!(
            h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap().0,
            new
        );
        assert_eq!(fake.graphs().len(), 2, "exactly one continuation");
    }

    #[test]
    fn a_deleted_remote_is_recreated_once_and_then_appended_to() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        h.pass(&d);
        let (old, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        fake.delete(ids_of(&old).unwrap().0);

        h.claude(CLAUDE_ID, 50);
        let (_, report) = h.pass(&d);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::Recreated { .. })
            ),
            "{report:?}"
        );
        let (new, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        assert_eq!(
            fake.path_at(&new).unwrap().step_ids(),
            h.claude_settled(CLAUDE_ID)
        );

        h.claude(CLAUDE_ID, 70);
        let (_, report) = h.pass(&d);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::Appended { .. })
            ),
            "{report:?}"
        );
        assert_eq!(fake.graphs().len(), 1);
    }

    /// The local copy lost steps (truncated): the pass finds by id that
    /// the remote has them all, keeps the same path, and growth after
    /// that lands in it.
    #[test]
    fn a_truncated_local_copy_continues_in_the_same_path() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 60);
        h.pass(&d);
        let (key, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        let stored = fake.path_at(&key).unwrap().step_ids();

        h.claude(CLAUDE_ID, 30);
        fake.clear_log();
        let (_, report) = h.pass(&d);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::UpToDate { .. })
            ),
            "{report:?}"
        );
        assert!(fake.writes().is_empty());
        assert!(
            fake.requests()
                .iter()
                .any(|r| r.target.contains("/step-ids"))
        );
        let (same, record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        assert_eq!(same, key);
        assert_eq!(record.steps, h.claude_settled(CLAUDE_ID).len());

        h.claude(CLAUDE_ID, 80);
        h.pass(&d);
        assert_eq!(
            h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap().0,
            key
        );
        let after = fake.path_at(&key).unwrap().step_ids();
        assert_eq!(after[..stored.len()], stored[..]);
        assert_eq!(fake.graphs().len(), 1);
    }

    #[test]
    fn mixed_harnesses_share_in_one_pass() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 40);
        h.codex(45);
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Create, Plan::Create]);
        let kinds: Vec<ArtifactType> = report.sessions.iter().map(|(t, ..)| *t).collect();
        assert_eq!(kinds, [ArtifactType::Claude, ArtifactType::Codex]);
        for r in outcomes(&report) {
            assert!(
                matches!(r, SessionResult::Pushed(Outcome::Created { .. })),
                "{r:?}"
            );
        }
        assert!(h.record(ArtifactType::Claude, CLAUDE_ID, &d).is_some());
        assert!(h.record(ArtifactType::Codex, CODEX_ID, &d).is_some());
        assert_eq!(fake.graphs().len(), 2);
        let (plans, _) = h.pass(&d);
        assert_eq!(plans, [Plan::Unchanged, Plan::Unchanged]);
    }

    /// A session that cannot be derived is reported; the pass shares the
    /// others and counts the failure.
    #[test]
    #[cfg(unix)]
    fn a_failing_derive_does_not_stop_the_pass() {
        use std::os::unix::fs::PermissionsExt;
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 40);
        h.codex(45);
        let unreadable = h.claude_file(CLAUDE_ID);
        let (_, report) = h.pass_with(&|_| Ok(d.clone()), &|_| {
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        });
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(report.failed(), 1, "{report:?}");
        assert!(matches!(outcomes(&report)[0], SessionResult::Failed(_)));
        assert!(matches!(
            outcomes(&report)[1],
            SessionResult::Pushed(Outcome::Created { .. })
        ));
        assert_eq!(h.record(ArtifactType::Claude, CLAUDE_ID, &d), None);
        assert!(report.render().contains("Failed:"), "{}", report.render());

        // Readable again, the next pass creates it.
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Create, Plan::Unchanged]);
        assert_eq!(report.failed(), 0);
    }

    /// A push the server refuses fails that session only.
    #[test]
    fn a_failing_push_does_not_stop_the_pass() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        h.pass(&d);
        h.claude(CLAUDE_ID, 50);
        h.codex(45);
        let claude_graph = ids_of(&h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap().0)
            .unwrap()
            .0;
        fake.force(
            "POST",
            &format!("{claude_graph}/paths"),
            401,
            r#"{"code":"unauthorized","error":"no"}"#,
        );
        let (_, report) = h.pass(&d);
        assert_eq!(report.failed(), 1, "{report:?}");
        assert!(matches!(outcomes(&report)[0], SessionResult::Failed(_)));
        assert!(matches!(outcomes(&report)[1], SessionResult::Pushed(_)));
    }

    /// Each session goes to the destination its directory resolves to.
    #[test]
    fn sessions_go_to_their_directorys_destination() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        // Codex records its cwd; the Claude derive records its own.
        let destination = |dir: Option<&Path>| -> Result<Destination> {
            Ok(match dir {
                Some(d) if d.to_string_lossy().ends_with("/codex") => y.clone(),
                _ => x.clone(),
            })
        };
        let (_, report) = h.pass_with(&destination, &|_| {});
        let dests: Vec<&str> = report
            .sessions
            .iter()
            .map(|(_, _, d, _)| d.as_str())
            .collect();
        assert_eq!(dests, ["alex/x", "alex/y"]);
        assert!(h.record(ArtifactType::Codex, CODEX_ID, &y).is_some());
        assert!(h.record(ArtifactType::Claude, CLAUDE_ID, &x).is_some());
    }

    #[test]
    fn share_all_refuses_an_anonymous_run_before_any_request() {
        let err = run(
            AllArgs {
                harness: None,
                project_under: None,
                url: None,
                repo: None,
                public: false,
                dry_run: false,
                yes: true,
            },
            "http://127.0.0.1:9".into(),
            crate::cmd_pathbase::AuthMode::Anon,
        )
        .unwrap_err();
        assert!(err.to_string().contains("path auth login"), "{err}");
    }

    #[test]
    fn the_summary_counts_each_outcome_and_lists_failures() {
        let report = Report {
            sessions: vec![
                (
                    ArtifactType::Claude,
                    "a".into(),
                    "me/x".into(),
                    SessionResult::Pushed(Outcome::Created {
                        url: "u".into(),
                        steps: 4,
                    }),
                ),
                (
                    ArtifactType::Claude,
                    "b".into(),
                    "me/x".into(),
                    SessionResult::Pushed(Outcome::Appended {
                        url: "u".into(),
                        steps: 2,
                    }),
                ),
                (
                    ArtifactType::Codex,
                    "c".into(),
                    "me/y".into(),
                    SessionResult::Pushed(Outcome::Appended {
                        url: "u".into(),
                        steps: 3,
                    }),
                ),
                (
                    ArtifactType::Codex,
                    "d".into(),
                    "me/y".into(),
                    SessionResult::Unchanged,
                ),
                (
                    ArtifactType::Pi,
                    "e".into(),
                    "me/y".into(),
                    SessionResult::Failed("derive: boom".into()),
                ),
            ],
        };
        assert_eq!(
            report.render(),
            "5 sessions in scope\n  \
             created             1  (4 steps)\n  \
             appended            2  (5 steps)\n  \
             unchanged           1\n  \
             failed              1\n\
             Written to:\n  me/x  2 sessions\n  me/y  1 session\n\
             Failed:\n  pi e: derive: boom\n"
        );
    }
}
