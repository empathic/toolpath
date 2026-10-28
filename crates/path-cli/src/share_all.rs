//! `path share --all`: share every session in scope, each through the
//! append engine (`share_append`), so a pass creates the sessions that
//! have no remote yet, appends what the others gained since, and touches
//! nothing that has not changed. See
//! `docs/superpowers/specs/2026-10-13-path-sync-client-design.md`,
//! "Bulk upload".
//!
//! A pass enumerates through the sync engine's sources, so sessions are
//! named as their records name them (a Claude chain by its head) and
//! come with their stat stamps. A session with a record whose stamp
//! matches the source's is skipped without being read. Every other
//! session is derived, and only then given its destination, from the
//! working directory the derive recorded: a listing's directory can be
//! a lossy decoding (a Claude project slug). Sessions run one at a time;
//! one failing is reported and the pass goes on.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::artifact::{ArtifactRef, ArtifactType};
use crate::harness::HarnessBundle;
use crate::share_append::{self, Destination, Outcome, plural};
use crate::share_config::ProjectRules;
use crate::sync::sources::{self, ArtifactSource};
use crate::sync::{Manifest, Remotes, SourceStamp};

/// What a pass will do with one session, decided before deriving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    /// No record anywhere: derive, create.
    Create,
    /// A record whose stamp matches the source's: nothing to read.
    Unchanged,
    /// Records, none with a known stamp equal to the source's: derive,
    /// append.
    Update,
}

/// The spec's table, over the session's records that can stand for the
/// pass's destination (`record_stamps`, see [`RecordScope`]): none →
/// create; one with a known stamp equal to the source's → skip;
/// otherwise derive and append.
pub(crate) fn plan(record_stamps: &[Option<SourceStamp>], stamp: sources::Stamp) -> Plan {
    if record_stamps.is_empty() {
        return Plan::Create;
    }
    match SourceStamp::known(stamp) {
        Some(now) if record_stamps.contains(&Some(now)) => Plan::Unchanged,
        _ => Plan::Update,
    }
}

/// Which of a session's records can tell a pass it has nothing to send.
/// A destination resolved from config is known only after the derive,
/// so a record on any destination at the current stamp will do; one the
/// user named before the pass is satisfied only by records there.
#[derive(Debug, Clone)]
pub(crate) enum RecordScope {
    /// Destinations come from config: every record.
    Any,
    /// `--url` names the server: the records on the server of this
    /// probe (a path API URL there).
    Server(String),
    /// `--repo` names the destination: the record at this probe's
    /// server, owner and repo.
    Destination(String),
}

impl RecordScope {
    /// The stamps of the session's records within this scope.
    fn stamps(
        &self,
        remotes: &Remotes,
        artifact_type: ArtifactType,
        id: &str,
    ) -> Vec<Option<SourceStamp>> {
        match self {
            RecordScope::Any => remotes
                .of(artifact_type, id)
                .map(|r| r.source_stamp)
                .collect(),
            RecordScope::Server(probe) => remotes
                .on_server(artifact_type, id, probe)
                .map(|r| r.source_stamp)
                .collect(),
            RecordScope::Destination(probe) => remotes
                .at(artifact_type, id, probe)
                .map(|(_, r)| r.source_stamp)
                .into_iter()
                .collect(),
        }
    }
}

/// One session in scope.
pub(crate) struct Item<'a> {
    pub(crate) artifact: ArtifactRef,
    /// The directory the listing gives the session, when known: what
    /// `--project-under` scopes by, and where its configured remote is
    /// looked up when the derive records no working directory.
    pub(crate) dir: Option<String>,
    pub(crate) source: &'a dyn ArtifactSource,
    pub(crate) plan: Plan,
}

/// Every session of `sources` under any of `include` (everywhere when
/// it is empty), less those `exclusions` takes out by their listing's
/// directory, with its plan against the records in `remotes` within
/// `scope`, sorted by harness, then id.
pub(crate) fn gather<'a>(
    sources: &'a [Box<dyn ArtifactSource + 'a>],
    include: &[PathBuf],
    exclusions: Option<&ProjectRules>,
    manifest: &Manifest,
    remotes: &Remotes,
    scope: &RecordScope,
) -> Vec<Item<'a>> {
    let roots: Vec<Option<&Path>> = if include.is_empty() {
        vec![None]
    } else {
        include.iter().map(|p| Some(p.as_path())).collect()
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for source in sources {
        for project_under in &roots {
            gather_under(
                source.as_ref(),
                *project_under,
                manifest,
                &mut |artifact, dir| {
                    if !seen.insert((artifact.artifact_type, artifact.id.clone())) {
                        return;
                    }
                    if let (Some(rules), Some(d)) = (exclusions, dir.as_deref())
                        && rules.sync_disabled_by_listing(|root| source.in_scope(d, root))
                            == Some(true)
                    {
                        return;
                    }
                    let stamps = scope.stamps(remotes, artifact.artifact_type, &artifact.id);
                    let plan = plan(&stamps, (artifact.modified, artifact.size));
                    items.push(Item {
                        artifact,
                        dir,
                        source: source.as_ref(),
                        plan,
                    });
                },
            );
        }
    }
    items.sort_by(|a, b| {
        (a.artifact.artifact_type.name(), &a.artifact.id)
            .cmp(&(b.artifact.artifact_type.name(), &b.artifact.id))
    });
    items
}

/// Each session of `source` under `project_under`, with its listing's
/// directory.
fn gather_under(
    source: &dyn ArtifactSource,
    project_under: Option<&Path>,
    manifest: &Manifest,
    found: &mut dyn FnMut(ArtifactRef, Option<String>),
) {
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
        found(artifact, dir);
    }
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
    /// Its derived working directory is under a `sync = false` rule.
    Excluded,
    Pushed(Outcome),
    Failed(String),
}

/// A finished pass: each session, its destination (empty when it was
/// never resolved), and what happened.
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

    /// Per kind of outcome: sessions, steps, re-sent steps; and per
    /// destination written to, sessions.
    #[allow(clippy::type_complexity)]
    fn counts(
        &self,
    ) -> (
        BTreeMap<&'static str, (usize, usize, usize)>,
        BTreeMap<&str, usize>,
    ) {
        let mut counts: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
        let mut bump = |kind: &'static str, steps: usize, resent: usize| {
            let e = counts.entry(kind).or_default();
            e.0 += 1;
            e.1 += steps;
            e.2 += resent;
        };
        let mut by_dest: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, _, dest, result) in &self.sessions {
            match result {
                SessionResult::Unchanged => bump("unchanged", 0, 0),
                SessionResult::Excluded => bump("excluded", 0, 0),
                SessionResult::Failed(_) => bump("failed", 0, 0),
                SessionResult::Pushed(o) => {
                    match o {
                        Outcome::Created { steps, .. } => bump("created", *steps, 0),
                        Outcome::Appended { steps, resent } => bump("appended", *steps, *resent),
                        Outcome::UpToDate => bump("up to date", 0, 0),
                        Outcome::Empty => bump("empty", 0, 0),
                        Outcome::Continued { steps, .. } => bump("continued", *steps, 0),
                        Outcome::Recreated { steps, .. } => bump("recreated", *steps, 0),
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
        (counts, by_dest)
    }

    /// The pass in one line: how many sessions, then each kind of
    /// outcome that occurred, e.g. `12 sessions: 1 created (40 steps),
    /// 11 unchanged`.
    pub(crate) fn summary_line(&self) -> String {
        let (counts, _) = self.counts();
        let parts: Vec<String> = Self::ORDER
            .iter()
            .filter_map(|&(kind, with_steps)| {
                let (n, steps, _) = counts.get(kind)?;
                Some(if with_steps {
                    format!("{n} {kind} ({})", plural(*steps, "step"))
                } else {
                    format!("{n} {kind}")
                })
            })
            .collect();
        let total = plural(self.sessions.len(), "session");
        if parts.is_empty() {
            total
        } else {
            format!("{total}: {}", parts.join(", "))
        }
    }

    /// Each kind of outcome in report order, and whether it moved steps.
    const ORDER: [(&'static str, bool); 9] = [
        ("created", true),
        ("appended", true),
        ("continued", true),
        ("recreated", true),
        ("up to date", false),
        ("unchanged", false),
        ("excluded", false),
        ("empty", false),
        ("failed", false),
    ];

    /// The closing summary: one line per kind of outcome that occurred,
    /// then one line per destination written to.
    pub(crate) fn render(&self) -> String {
        let (counts, by_dest) = self.counts();
        let mut out = format!("{} in scope\n", plural(self.sessions.len(), "session"));
        for (kind, with_steps) in Self::ORDER {
            let Some((n, steps, resent)) = counts.get(kind) else {
                continue;
            };
            if with_steps {
                let resent = match resent {
                    0 => String::new(),
                    r => format!(", {r} re-sent"),
                };
                out.push_str(&format!(
                    "  {kind:<16}{n:>5}  ({}{resent})\n",
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
            .filter_map(|(t, id, dest, r)| match r {
                SessionResult::Failed(why) => Some((t, id, dest, why)),
                _ => None,
            })
            .collect();
        if !failures.is_empty() {
            out.push_str("Failed:\n");
            for (t, id, dest, why) in failures {
                out.push_str(&format!("  {} {id}{}: {why}\n", t.name(), arrow(dest)));
            }
        }
        out
    }
}

/// Run one pass over `items`: derive and push each that needs it,
/// sequentially, reporting each as it finishes on stderr when
/// `verbose`. A session whose derive, destination or push fails is
/// reported and the pass goes on. `cached` returns a session's cached
/// document when the cache is current for it, sparing the derive;
/// `exclusions` skips a session whose derived working directory a
/// `sync = false` rule covers; `destination` maps a session directory
/// to where it goes.
pub(crate) fn run_pass(
    config_dir: &Path,
    items: &[Item],
    cached: &dyn Fn(&ArtifactRef) -> Option<toolpath::v1::Graph>,
    exclusions: Option<&ProjectRules>,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
    verbose: bool,
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
                String::new(),
                SessionResult::Unchanged,
            ));
            continue;
        }
        let (dest, pushed) = push_item(config_dir, item, cached, exclusions, destination);
        let dest = dest.unwrap_or_default();
        let (line, result) = match pushed {
            Ok(None) => (
                format!("{label}: excluded by a `sync = false` rule"),
                SessionResult::Excluded,
            ),
            Ok(Some(outcome)) => {
                let url = outcome.url().map(|u| format!(" → {u}")).unwrap_or_default();
                (
                    format!("{label}{}: {}{url}", arrow(&dest), outcome.describe()),
                    SessionResult::Pushed(outcome),
                )
            }
            Err(e) => (
                format!("{label}{}: failed: {e:#}", arrow(&dest)),
                SessionResult::Failed(format!("{e:#}")),
            ),
        };
        if verbose {
            eprintln!("{line}");
        }
        report
            .sessions
            .push((a.artifact_type, a.id.clone(), dest, result));
    }
    report
}

/// ` → owner/repo` for a resolved destination, nothing otherwise.
fn arrow(dest: &str) -> String {
    if dest.is_empty() {
        String::new()
    } else {
        format!(" → {dest}")
    }
}

/// Derive (or read from the cache) and push one session to the
/// destination of the working directory its document records, or of
/// its listing's directory when it records none; `Ok(None)` when
/// `exclusions` covers that directory. The destination's display comes
/// back whenever it was resolved, failure or not.
fn push_item(
    config_dir: &Path,
    item: &Item,
    cached: &dyn Fn(&ArtifactRef) -> Option<toolpath::v1::Graph>,
    exclusions: Option<&ProjectRules>,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
) -> (Option<String>, Result<Option<Outcome>>) {
    let a = &item.artifact;
    let doc = match cached(a) {
        Some(doc) => doc,
        None => match item.source.derive(a).context("derive") {
            Ok(derived) => derived.doc,
            Err(e) => return (None, Err(e)),
        },
    };
    let Some(path) = doc.single_path() else {
        return (
            None,
            Err(anyhow::anyhow!(
                "the session did not derive to a single path"
            )),
        );
    };
    let dir =
        crate::cmd_share::doc_session_dir(&doc).or_else(|| item.dir.as_ref().map(PathBuf::from));
    if let (Some(rules), Some(dir)) = (exclusions, dir.as_deref())
        && rules.sync_disabled(dir)
    {
        return (None, Ok(None));
    }
    let dest = match destination(dir.as_deref()).context("resolve the destination") {
        Ok(dest) => dest,
        Err(e) => return (None, Err(e)),
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
        true,
    );
    (Some(dest.display()), outcome.map(Some))
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
    for i in items {
        *per_harness
            .entry(i.artifact.artifact_type.name())
            .or_default() += 1;
    }
    let harnesses: Vec<String> = per_harness
        .iter()
        .map(|(h, n)| format!("{h} {n}"))
        .collect();
    out.push_str(&format!("  {}\n", harnesses.join(", ")));
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

/// What a bulk pass covers and how much it reports.
pub(crate) struct Bulk<'a> {
    pub(crate) config_dir: &'a Path,
    pub(crate) bundle: &'a HarnessBundle,
    pub(crate) types: &'a [ArtifactType],
    /// Directory subtrees; empty is everywhere.
    pub(crate) include: &'a [PathBuf],
    /// The `sync = false` rules to honour, for `path sync`.
    pub(crate) exclusions: Option<&'a ProjectRules>,
    pub(crate) records: RecordScope,
    /// A line per session as it finishes.
    pub(crate) verbose: bool,
}

/// The bulk upload under both `path share --all` and `path sync`:
/// gather the sessions in scope, show them to `proceed`, which can stop
/// the pass by answering false (then `None`), and push each that needs
/// it to `destination`. The caller holds the upload lock.
pub(crate) fn bulk(
    b: &Bulk,
    destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
    proceed: &mut dyn FnMut(&[Item]) -> Result<bool>,
) -> Result<Option<Report>> {
    let sources = sources_for(b.bundle, b.types);
    let manifest = crate::sync::load_manifest(b.config_dir).unwrap_or_default();
    let remotes = crate::sync::load_remotes(b.config_dir)?;
    let items = gather(
        &sources,
        b.include,
        b.exclusions,
        &manifest,
        &remotes,
        &b.records,
    );
    if !proceed(&items)? {
        return Ok(None);
    }
    Ok(Some(run_pass(
        b.config_dir,
        &items,
        &|a| cached_doc(&manifest, a),
        b.exclusions,
        destination,
        b.verbose,
    )))
}

/// `path share --all`, as `username` with `token`.
pub(crate) fn run(args: AllArgs, base_url: String, token: String, username: String) -> Result<()> {
    let config_dir = crate::config::config_dir()?;
    let home = crate::config::home_dir();
    let bundle = HarnessBundle::from_environment();
    let types: Vec<ArtifactType> = match args.harness {
        Some(h) => vec![h],
        None => ArtifactType::ALL.to_vec(),
    };
    let include: Vec<PathBuf> = args.project_under.iter().cloned().collect();
    let _lock = match crate::sync::lock::lock_uploads(&config_dir, false)? {
        Some(lock) => lock,
        None => {
            eprintln!("Waiting for another upload pass to finish…");
            crate::sync::lock::lock_uploads(&config_dir, true)?.expect("a waited-for lock is held")
        }
    };
    let destination = ensuring_pathstash(&username, |dir| {
        let dest = Resolve {
            config_dir: &config_dir,
            home: home.as_deref(),
            base_url: &base_url,
            token: &token,
            username: &username,
            url: args.url.as_deref(),
            repo: args.repo.as_ref(),
            public: args.public,
        };
        dest.destination_for(dir)
    });
    let pass = Bulk {
        config_dir: &config_dir,
        bundle: &bundle,
        types: &types,
        include: &include,
        exclusions: None,
        records: record_scope(&args, &base_url, &username),
        verbose: true,
    };
    let report = bulk(&pass, &destination, &mut |items| {
        eprint!("{}", render_plan(items, args.project_under.as_deref()));
        let work = items.iter().filter(|i| i.plan != Plan::Unchanged).count();
        if work == 0 {
            eprintln!("Nothing to share.");
            return Ok(false);
        }
        if args.dry_run {
            return Ok(false);
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
        Ok(true)
    })?;
    let Some(report) = report else {
        return Ok(());
    };
    eprint!("{}", report.render());
    let failed = report.failed();
    if failed > 0 {
        anyhow::bail!("{} failed", plural(failed, "session"));
    }
    Ok(())
}

/// The records that can spare a session in this pass: those at the
/// repo `--repo` names, those on the server `--url` names, or any.
fn record_scope(args: &AllArgs, base_url: &str, username: &str) -> RecordScope {
    let probe = |owner: &str, repo: &str| {
        let nil = uuid::Uuid::nil();
        crate::cmd_pathbase::remote_path_url(base_url, owner, repo, &nil, &nil)
    };
    match (&args.repo, &args.url) {
        (Some(spec), _) => RecordScope::Destination(probe(&spec.owner, &spec.name)),
        (None, Some(_)) => RecordScope::Server(probe(username, "pathstash")),
        (None, None) => RecordScope::Any,
    }
}

/// `resolve`, creating `username`'s `pathstash` repo on a server the
/// first time a session resolves to it there. A failure to create it
/// fails that session's resolution, and the next session to need it
/// tries again.
pub(crate) fn ensuring_pathstash<'a>(
    username: &'a str,
    resolve: impl Fn(Option<&Path>) -> Result<Destination> + 'a,
) -> impl Fn(Option<&Path>) -> Result<Destination> + 'a {
    let ensured: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    move |dir| {
        let dest = resolve(dir)?;
        let pathstash = dest.owner == username && dest.repo == "pathstash";
        if pathstash && !ensured.borrow().contains(&dest.base_url) {
            crate::cmd_pathbase::repos_post(&dest.base_url, &dest.token, username, "pathstash")
                .context("create your pathstash repo")?;
            ensured.borrow_mut().insert(dest.base_url.clone());
        }
        Ok(dest)
    }
}

/// How a pass resolves each session's destination.
pub(crate) struct Resolve<'a> {
    pub(crate) config_dir: &'a Path,
    pub(crate) home: Option<&'a Path>,
    pub(crate) base_url: &'a str,
    pub(crate) token: &'a str,
    pub(crate) username: &'a str,
    /// `--url`: beats a configured remote's own server.
    pub(crate) url: Option<&'a str>,
    /// `--repo`: beats configured remotes.
    pub(crate) repo: Option<&'a crate::remote::RepoSpec>,
    pub(crate) public: bool,
}

impl Resolve<'_> {
    /// The destination for a session in `dir`: `--repo` when given,
    /// else the remote configured for the directory, else the user's
    /// `pathstash` on `base_url`.
    pub(crate) fn destination_for(&self, dir: Option<&Path>) -> Result<Destination> {
        let dest = |base: &str, owner: &str, repo: &str| Destination {
            base_url: base.to_string(),
            token: self.token.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            name: None,
            public: self.public,
        };
        if let Some(spec) = self.repo {
            return Ok(dest(self.base_url, &spec.owner, &spec.name));
        }
        if let Some(dir) = dir
            && let Some(found) =
                crate::share_config::resolve_remote_in(self.config_dir, self.home, dir)?
        {
            let base = match (self.url, &found.base_url) {
                (None, Some(remote)) => remote.as_str(),
                _ => self.base_url,
            };
            return Ok(dest(base, &found.repo.owner, &found.repo.name));
        }
        Ok(dest(self.base_url, self.username, "pathstash"))
    }
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
            self.pass_scoped(&RecordScope::Any, destination, between)
        }

        /// [`Self::pass_with`], planning against the records in `scope`.
        fn pass_scoped(
            &self,
            scope: &RecordScope,
            destination: &dyn Fn(Option<&Path>) -> Result<Destination>,
            between: &dyn Fn(&[Item]),
        ) -> (Vec<Plan>, Report) {
            let bundle = self.bundle();
            let sources = sources_for(&bundle, &[ArtifactType::Claude, ArtifactType::Codex]);
            let remotes = crate::sync::load_remotes(self.cfg()).unwrap();
            let items = gather(&sources, &[], None, &Manifest::default(), &remotes, scope);
            between(&items);
            let plans = items.iter().map(|i| i.plan).collect();
            (
                plans,
                run_pass(self.cfg(), &items, &|_| None, None, destination, true),
            )
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

        /// The Claude session's steps as it is now.
        fn claude_derive(&self, id: &str) -> Vec<toolpath::v1::Step> {
            let bundle = self.bundle();
            let doc = crate::derive::derive_claude_session_with(
                bundle.claude.as_ref().unwrap(),
                "/test/project",
                id,
            )
            .unwrap()
            .doc;
            doc.single_path().unwrap().steps.clone()
        }

        /// The step ids of the Claude session as it is now.
        fn claude_steps(&self, id: &str) -> Vec<String> {
            self.claude_derive(id)
                .iter()
                .map(|s| s.step.id.clone())
                .collect()
        }

        /// The settled step ids of the Claude session as it is now.
        fn claude_settled(&self, id: &str) -> Vec<String> {
            let steps = self.claude_derive(id);
            let k = share_append::settled_len(&steps);
            steps[..k].iter().map(|s| s.step.id.clone()).collect()
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
        let other = SourceStamp::known((None, Some(9)));
        assert_eq!(plan(&[], stamp), Plan::Create);
        assert_eq!(plan(&[], (None, None)), Plan::Create);
        assert_eq!(plan(&[known], stamp), Plan::Unchanged);
        assert_eq!(plan(&[known], (None, Some(11))), Plan::Update);
        // A record on any destination at the current stamp will do.
        assert_eq!(plan(&[other, known], stamp), Plan::Unchanged);
        assert_eq!(plan(&[other], stamp), Plan::Update);
        // Unknown on either side: derive every pass.
        assert_eq!(plan(&[None], (None, None)), Plan::Update);
        assert_eq!(plan(&[known], (None, None)), Plan::Update);
        assert_eq!(plan(&[None], stamp), Plan::Update);
    }

    /// Overlapping include roots list a session once; a root over
    /// nothing lists nothing.
    #[test]
    fn gather_takes_the_union_of_its_roots() {
        let h = Harnesses::new();
        h.claude(CLAUDE_ID, 6);
        let bundle = h.bundle();
        let sources = sources_for(&bundle, &[ArtifactType::Claude]);
        let remotes = crate::sync::load_remotes(h.cfg()).unwrap();
        let ids = |include: &[PathBuf]| -> Vec<String> {
            gather(
                &sources,
                include,
                None,
                &Manifest::default(),
                &remotes,
                &RecordScope::Any,
            )
            .into_iter()
            .map(|i| i.artifact.id)
            .collect()
        };
        let roots = [PathBuf::from("/test"), PathBuf::from("/test/project")];
        assert_eq!(ids(&roots), [CLAUDE_ID]);
        assert_eq!(ids(&[]), [CLAUDE_ID]);
        assert!(ids(&[PathBuf::from("/elsewhere")]).is_empty());
    }

    /// Sessions with no settled step, only the first prompt or only
    /// lines that are no turn at all, are shared at once, every step
    /// included.
    #[test]
    fn a_session_with_nothing_settled_is_created_with_every_step() {
        let lines = fixture("claude/convo.jsonl");
        for kept in [&lines[2..3], &lines[..2]] {
            let h = Harnesses::new();
            write_lines(&h.claude_file(CLAUDE_ID), kept);
            assert!(h.claude_settled(CLAUDE_ID).is_empty());
            let fake = FakePathbase::start();
            let d = dest(&fake, "x");
            let (plans, report) = h.pass(&d);
            assert_eq!(plans, [Plan::Create]);
            assert!(
                matches!(
                    only(&report),
                    SessionResult::Pushed(Outcome::Created { .. })
                ),
                "{report:?}"
            );
            let (key, record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
            let all = h.claude_steps(CLAUDE_ID);
            assert!(!all.is_empty());
            assert_eq!(fake.path_at(&key).unwrap().step_ids(), all);
            assert_eq!(record.steps, all.len());
            assert_eq!(report.failed(), 0);
        }
    }

    /// Shared once, then grown: the second pass sends the new steps and
    /// re-sends the ones that were open, and a third pass with nothing
    /// new is a no-op.
    #[test]
    fn a_grown_session_appends_new_steps_resends_open_ones_and_a_repeat_is_a_no_op() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        let first = h.claude_steps(CLAUDE_ID);
        let open = share_append::settled_len(&h.claude_derive(CLAUDE_ID));
        assert!(open < first.len());
        let (_, report) = h.pass(&d);
        assert!(matches!(
            only(&report),
            SessionResult::Pushed(Outcome::Created { .. })
        ));
        let (key, record) = h.record(ArtifactType::Claude, CLAUDE_ID, &d).unwrap();
        assert_eq!(fake.path_at(&key).unwrap().step_ids(), first);
        assert_eq!(record.steps, first.len());

        h.claude(CLAUDE_ID, 60);
        let grown = h.claude_steps(CLAUDE_ID);
        assert_eq!(
            grown[..first.len()],
            first[..],
            "the Claude derive is prefix-stable"
        );
        fake.clear_log();
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Update]);
        let SessionResult::Pushed(Outcome::Appended { steps, resent }) = only(&report) else {
            panic!("{report:?}");
        };
        assert_eq!(
            (*steps, *resent),
            (grown.len() - first.len(), first.len() - open)
        );
        let sent: Vec<String> = fake.writes().iter().flat_map(|w| w.step_ids()).collect();
        assert_eq!(sent, grown[open..]);
        assert_eq!(fake.path_at(&key).unwrap().step_ids(), grown);

        fake.clear_log();
        let (plans, report) = h.pass(&d);
        assert_eq!(plans, [Plan::Unchanged]);
        assert!(matches!(only(&report), SessionResult::Unchanged));
        assert!(fake.requests().is_empty(), "{:?}", fake.requests());
        assert_eq!(fake.graphs().len(), 1);
    }

    /// A session whose destination changed keeps a record on each, and
    /// each appends from its own: a record at the current stamp on
    /// either spares the derive.
    #[test]
    fn a_session_shared_to_two_destinations_appends_to_each_from_its_record() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        h.claude(CLAUDE_ID, 30);
        h.pass(&x);
        h.claude(CLAUDE_ID, 45);
        h.pass(&y);
        let (x_key, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &x).unwrap();
        let (y_key, _) = h.record(ArtifactType::Claude, CLAUDE_ID, &y).unwrap();
        assert_ne!(x_key, y_key);

        let (plans, _) = h.pass(&x);
        assert_eq!(
            plans,
            [Plan::Unchanged],
            "y's record holds the current stamp"
        );

        h.claude(CLAUDE_ID, 60);
        let (plans, _) = h.pass(&x);
        assert_eq!(plans, [Plan::Update]);
        let grown = h.claude_steps(CLAUDE_ID);
        assert_eq!(fake.path_at(&x_key).unwrap().step_ids(), grown);
        assert_ne!(fake.path_at(&y_key).unwrap().step_ids(), grown);
        h.claude(CLAUDE_ID, 70);
        let (plans, _) = h.pass(&y);
        assert_eq!(plans, [Plan::Update]);
        assert_eq!(
            fake.path_at(&y_key).unwrap().step_ids(),
            h.claude_steps(CLAUDE_ID)
        );
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
        let steps = h.claude_derive(CLAUDE_ID);
        let all = h.claude_steps(CLAUDE_ID);
        let continuation = fake.path_at(&new).unwrap();
        assert_eq!(continuation.step_ids(), all[old_record.steps..]);
        let frozen = fake.path_at(&old).unwrap().step_ids();
        let from = steps[old_record.steps]
            .step
            .parents
            .first()
            .filter(|p| frozen.contains(p))
            .unwrap_or(&old_record.head);
        assert_eq!(&continuation.parent.unwrap().step, from);
        assert_eq!(record.steps, all.len());

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
            h.claude_steps(CLAUDE_ID)
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
    /// the remote has them all, sends nothing, keeps the same path, and
    /// growth after that lands in it.
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
            matches!(only(&report), SessionResult::Pushed(Outcome::UpToDate)),
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
        assert_eq!(record.steps, h.claude_steps(CLAUDE_ID).len());

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
        assert_eq!(report.sessions[0].2, "alex/x", "the refusing repo is named");
        assert!(
            report
                .render()
                .contains(&format!("claude {CLAUDE_ID} → alex/x: ")),
            "{}",
            report.render()
        );
    }

    /// A destination the user names is satisfied only by its own
    /// record: `--repo` by the record at that repo, `--url` by a record
    /// on that server. Config resolution takes a record anywhere.
    #[test]
    fn a_named_destination_is_not_satisfied_by_another_destinations_record() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let other = FakePathbase::start();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        h.claude(CLAUDE_ID, 30);
        h.pass(&x);
        let to = |d: &Destination| {
            let d = d.clone();
            move |_: Option<&Path>| -> Result<Destination> { Ok(d.clone()) }
        };

        let (plans, _) = h.pass_scoped(&RecordScope::Any, &to(&y), &|_| {});
        assert_eq!(plans, [Plan::Unchanged]);
        let (plans, _) = h.pass_scoped(&RecordScope::Server(x.probe()), &to(&y), &|_| {});
        assert_eq!(plans, [Plan::Unchanged], "a record on the named server");
        let (plans, _) = h.pass_scoped(
            &RecordScope::Server(dest(&other, "x").probe()),
            &|_| Ok(dest(&other, "x")),
            &|_| {},
        );
        assert_eq!(plans, [Plan::Create], "no record on the named server");

        let (plans, report) = h.pass_scoped(&RecordScope::Destination(y.probe()), &to(&y), &|_| {});
        assert_eq!(plans, [Plan::Create]);
        assert!(
            matches!(
                only(&report),
                SessionResult::Pushed(Outcome::Created { .. })
            ),
            "{report:?}"
        );
        assert!(h.record(ArtifactType::Claude, CLAUDE_ID, &y).is_some());
        let (plans, _) = h.pass_scoped(&RecordScope::Destination(y.probe()), &to(&y), &|_| {});
        assert_eq!(plans, [Plan::Unchanged], "now recorded there");
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
                        steps: 2,
                        resent: 1,
                    }),
                ),
                (
                    ArtifactType::Codex,
                    "c".into(),
                    "me/y".into(),
                    SessionResult::Pushed(Outcome::Appended {
                        steps: 3,
                        resent: 0,
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
                    String::new(),
                    SessionResult::Failed("derive: boom".into()),
                ),
                (
                    ArtifactType::Pi,
                    "f".into(),
                    "me/z".into(),
                    SessionResult::Failed("forbidden".into()),
                ),
            ],
        };
        assert_eq!(
            report.render(),
            "6 sessions in scope\n  \
             created             1  (4 steps)\n  \
             appended            2  (5 steps, 1 re-sent)\n  \
             unchanged           1\n  \
             failed              2\n\
             Written to:\n  me/x  2 sessions\n  me/y  1 session\n\
             Failed:\n  pi e: derive: boom\n  pi f → me/z: forbidden\n"
        );
    }

    /// A Claude session listed under a lossy project slug goes where its
    /// recorded working directory's remote says, and once recorded there
    /// it is skipped without a derive.
    #[test]
    fn a_claude_session_goes_to_the_remote_of_its_recorded_directory() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let (x, y) = (dest(&fake, "x"), dest(&fake, "y"));
        h.claude(CLAUDE_ID, 30);
        // The fixture's cwd is `…/toolpath-elicit.nu6ZWB/claude`; the
        // listing decodes the `-test-project` slug instead.
        let destination = |dir: Option<&Path>| -> Result<Destination> {
            Ok(match dir {
                Some(d) if d.ends_with("toolpath-elicit.nu6ZWB/claude") => y.clone(),
                _ => x.clone(),
            })
        };
        let (plans, report) = h.pass_with(&destination, &|_| {});
        assert_eq!(plans, [Plan::Create]);
        assert_eq!(report.sessions[0].2, "alex/y", "{report:?}");
        assert!(h.record(ArtifactType::Claude, CLAUDE_ID, &y).is_some());
        assert!(h.record(ArtifactType::Claude, CLAUDE_ID, &x).is_none());

        fake.clear_log();
        let (plans, _) = h.pass_with(&destination, &|_| {});
        assert_eq!(plans, [Plan::Unchanged]);
        assert!(fake.requests().is_empty(), "{:?}", fake.requests());
    }

    /// A destination that cannot be resolved (a configured remote that
    /// does not parse) fails that session; the pass shares the rest.
    #[test]
    fn a_bad_destination_fails_only_its_session() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let x = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        let destination = |dir: Option<&Path>| -> Result<Destination> {
            match dir {
                Some(d) if d.ends_with("claude") => anyhow::bail!("remote `::` does not parse"),
                _ => Ok(x.clone()),
            }
        };
        let (_, report) = h.pass_with(&destination, &|_| {});
        assert_eq!(report.failed(), 1, "{report:?}");
        let SessionResult::Failed(why) = outcomes(&report)[0] else {
            panic!("{report:?}");
        };
        assert!(why.contains("does not parse"), "{why}");
        assert!(matches!(
            outcomes(&report)[1],
            SessionResult::Pushed(Outcome::Created { .. })
        ));
    }

    /// The pathstash repo is made the first time a session resolves to
    /// it, once per server, and never when none does. Failing to make it
    /// fails the sessions that need it, not the pass.
    #[test]
    fn the_pathstash_repo_is_made_when_a_session_first_needs_it() {
        let repo_posts = |fake: &FakePathbase| {
            fake.requests()
                .iter()
                .filter(|r| r.method == "POST" && r.target.ends_with("/repos"))
                .count()
        };
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        let pathstash = dest(&fake, "pathstash");
        let both = ensuring_pathstash("alex", |_| Ok(pathstash.clone()));
        let (_, report) = h.pass_with(&both, &|_| {});
        assert_eq!(report.failed(), 0, "{report:?}");
        assert_eq!(repo_posts(&fake), 1);

        let h = Harnesses::new();
        let fake = FakePathbase::start();
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        let elsewhere = ensuring_pathstash("alex", |_| Ok(dest(&fake, "x")));
        h.pass_with(&elsewhere, &|_| {});
        assert_eq!(repo_posts(&fake), 0);

        // Every write to this server fails; the other server is fine.
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        let other = FakePathbase::start();
        h.claude(CLAUDE_ID, 30);
        h.codex(30);
        fake.force(
            "POST",
            "/repos",
            500,
            r#"{"code":"internal_error","error":"down"}"#,
        );
        let pathstash = dest(&fake, "pathstash");
        let x = dest(&other, "x");
        let claude_to_pathstash = ensuring_pathstash("alex", |dir: Option<&Path>| {
            Ok(match dir {
                Some(d) if d.ends_with("claude") => pathstash.clone(),
                _ => x.clone(),
            })
        });
        let (_, report) = h.pass_with(&claude_to_pathstash, &|_| {});
        assert_eq!(report.failed(), 1, "{report:?}");
        assert!(matches!(outcomes(&report)[0], SessionResult::Failed(_)));
        assert!(matches!(
            outcomes(&report)[1],
            SessionResult::Pushed(Outcome::Created { .. })
        ));
    }

    /// Against a server whose create response lists no stored paths, a
    /// pass would create a new graph every time: it fails the session
    /// instead, and leaves no graph.
    #[test]
    fn a_server_that_lists_no_stored_paths_fails_the_session() {
        let h = Harnesses::new();
        let fake = FakePathbase::start();
        fake.unlist_created_paths();
        let d = dest(&fake, "x");
        h.claude(CLAUDE_ID, 30);
        let (_, report) = h.pass(&d);
        let SessionResult::Failed(why) = only(&report) else {
            panic!("{report:?}");
        };
        assert!(why.contains("incremental upload unsupported"), "{why}");
        assert!(fake.graphs().is_empty());
        assert_eq!(h.record(ArtifactType::Claude, CLAUDE_ID, &d), None);

        // The cleanup delete fails: the summary names the graph left.
        fake.force(
            "DELETE",
            "/graphs/",
            403,
            r#"{"code":"forbidden","error":"no"}"#,
        );
        let (_, report) = h.pass(&d);
        let graph = fake.graphs()[0].id;
        let left = format!("{}/u/alex/x/graphs/{graph}", fake.base());
        assert!(report.render().contains(&left), "{}", report.render());
    }
}
