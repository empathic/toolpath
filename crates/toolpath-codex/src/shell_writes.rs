//! File writes inferred from Codex shell calls (heredocs and shell-run
//! `apply_patch`), stamped `structural.extra["codex"]`. Format and rules:
//! docs/agents/formats/codex.md, "Shell writes".

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value, json};
use toolpath_convo::shell_writes::{
    HeredocPatch, HeredocWrite, ParsedScript, PatchOp, ShellItem, StatusLink, Unresolvable,
    normalize_path as normalize, parse_argv, parse_patch, parse_script,
};
use toolpath_convo::{FileMutation, ToolCategory, ToolInvocation, unified_diff};

use crate::provider::{synth_add_diff, synth_delete_diff};

/// `extra.codex.source` of a change inferred from a shell heredoc write.
pub(crate) const SOURCE_HEREDOC: &str = "shell-heredoc";
/// `extra.codex.source` of a change from `apply_patch` run through a shell call.
pub(crate) const SOURCE_APPLY_PATCH: &str = "shell-apply-patch";
/// Key of the stamp under a change's `structural.extra`.
pub(crate) const EXTRA_KEY: &str = "codex";

// ── Evidence the builder collects per call ──────────────────────────

/// What the rollout says about one call.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallEvidence {
    /// Output texts with their `is_error` flag, kept apart from the merged `ToolInvocation.result`.
    pub outputs: Vec<(String, bool)>,
    pub exec_end: Option<ExecEnd>,
    /// `Some(all succeeded)` when a `patch_apply_end` names the call.
    pub patch_end: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExecEnd {
    pub exit_code: Option<i32>,
    pub status: Option<String>,
    pub cwd: Option<String>,
}

pub(crate) type Evidence = HashMap<String, CallEvidence>;

// ── Outcome ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure,
    Unknown,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Unknown => "unknown",
        }
    }
}

/// What an [`Outcome`] rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Basis {
    ExitCode,
    IsError,
    NoErrorReported,
    NoResult,
    StillRunning,
    ExecStatus,
    ErrorText,
    PatchApplyEnd,
}

impl Basis {
    fn as_str(self) -> &'static str {
        match self {
            Basis::ExitCode => "exit_code",
            Basis::IsError => "is_error",
            Basis::NoErrorReported => "no_error_reported",
            Basis::NoResult => "no_result",
            Basis::StillRunning => "still_running",
            Basis::ExecStatus => "exec_status",
            Basis::ErrorText => "error_text",
            Basis::PatchApplyEnd => "patch_apply_end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellOutcome {
    pub outcome: Outcome,
    pub basis: Basis,
    pub exit_code: Option<i64>,
    /// The `write_stdin` call that reported the exit of a still-running command.
    pub exit_via: Option<String>,
    pub exec_status: Option<String>,
}

impl ShellOutcome {
    fn new(outcome: Outcome, basis: Basis) -> Self {
        Self {
            outcome,
            basis,
            exit_code: None,
            exit_via: None,
            exec_status: None,
        }
    }

    fn code(code: i64, via: Option<&str>) -> Self {
        Self {
            outcome: if code == 0 {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            basis: Basis::ExitCode,
            exit_code: Some(code),
            exit_via: via.map(str::to_string),
            exec_status: None,
        }
    }
}

/// Exits `write_stdin` polls reported for still-running calls: call id → `(poll id, code)`.
pub(crate) type StdinExits = HashMap<String, (String, i64)>;

/// Binds each call that left a process running under session `S` to the
/// first poll of `S` reporting an exit after it, unless another call
/// reports running under `S` first.
pub(crate) fn stdin_exits<'t>(
    tools: impl IntoIterator<Item = &'t ToolInvocation>,
    evidence: &Evidence,
) -> StdinExits {
    let mut running: HashMap<String, String> = HashMap::new();
    let mut out = StdinExits::new();
    for tool in tools {
        let outputs = evidence
            .get(&tool.id)
            .map(|e| e.outputs.as_slice())
            .unwrap_or(&[]);
        if tool.name != "write_stdin" {
            if let Some(Status::Running(sid)) = outputs.first().and_then(|(t, _)| status(t)) {
                running.insert(sid, tool.id.clone());
            }
            continue;
        }
        let Some(sid) = session_id(&tool.input) else {
            continue;
        };
        let exit = outputs.iter().find_map(|(t, _)| match status(t) {
            Some(Status::Exited(code)) => Some(code),
            _ => None,
        });
        if let Some(code) = exit
            && let Some(call) = running.remove(&sid)
        {
            out.insert(call, (tool.id.clone(), code));
        }
    }
    out
}

fn session_id(input: &Value) -> Option<String> {
    match input.get("session_id")? {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// A shell call's outcome; precedence in docs/agents/formats/codex.md.
pub(crate) fn outcome(
    call_id: &str,
    evidence: &Evidence,
    stdin: &StdinExits,
    apply_patch: bool,
) -> ShellOutcome {
    let ev = evidence.get(call_id);
    let end = ev.and_then(|e| e.exec_end.as_ref());
    if let Some(end) = end {
        if let Some(code) = end.exit_code {
            return ShellOutcome::code(i64::from(code), None);
        }
        if let Some(s) = end.status.as_deref().filter(|s| *s != "completed") {
            let mut oc = ShellOutcome::new(Outcome::Failure, Basis::ExecStatus);
            oc.exec_status = Some(s.to_string());
            return oc;
        }
    }
    let outputs = ev.map(|e| e.outputs.as_slice()).unwrap_or(&[]);
    if outputs.iter().any(|(_, is_error)| *is_error) {
        return ShellOutcome::new(Outcome::Failure, Basis::IsError);
    }
    let Some((first, _)) = outputs.first() else {
        return match end {
            Some(_) => ShellOutcome::new(Outcome::Success, Basis::NoErrorReported),
            None => ShellOutcome::new(Outcome::Unknown, Basis::NoResult),
        };
    };
    match status(first) {
        Some(Status::Exited(code)) => ShellOutcome::code(code, None),
        Some(Status::Running(_)) => match stdin.get(call_id) {
            Some((via, code)) => ShellOutcome::code(*code, Some(via)),
            None => ShellOutcome::new(Outcome::Unknown, Basis::StillRunning),
        },
        None if apply_patch && body_of(first).starts_with("apply_patch verification failed") => {
            ShellOutcome::new(Outcome::Failure, Basis::ErrorText)
        }
        None => ShellOutcome::new(Outcome::Success, Basis::NoErrorReported),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Exited(i64),
    Running(String),
}

/// The exit status in an output, read only where each tool writes it.
fn status(text: &str) -> Option<Status> {
    if let Some((header, _)) = unified(text) {
        for line in header.lines() {
            if let Some(n) = line.strip_prefix("Process exited with code ") {
                return n.trim().parse().ok().map(Status::Exited);
            }
            if let Some(s) = line.strip_prefix("Process running with session ID ") {
                return Some(Status::Running(s.trim().to_string()));
            }
        }
        return None;
    }
    if text.trim_start().starts_with('{') {
        let v: Value = serde_json::from_str(text.trim()).ok()?;
        return v
            .get("metadata")?
            .get("exit_code")?
            .as_i64()
            .map(Status::Exited);
    }
    text.lines()
        .next()?
        .strip_prefix("Exit code: ")
        .and_then(|n| n.trim().parse().ok())
        .map(Status::Exited)
}

const HEADER_LINES: [&str; 5] = [
    "Chunk ID: ",
    "Wall time: ",
    "Process exited with code ",
    "Process running with session ID ",
    "Original token count: ",
];

/// A unified-exec output split into its header and body: the first run of
/// header lines, opening with `Chunk ID:` or `Wall time:` and naming a
/// process status, that ends at an `Output:` line.
fn unified(text: &str) -> Option<(&str, &str)> {
    let mut at = 0;
    let mut start: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "Output:" {
            if let Some(s) = start
                && text[s..at].contains("Process ")
            {
                return Some((&text[s..at], &text[at + line.len()..]));
            }
            start = None;
        } else if HEADER_LINES.iter().any(|h| bare.starts_with(h)) {
            if start.is_none() && HEADER_LINES[..2].iter().any(|h| bare.starts_with(h)) {
                start = Some(at);
            }
        } else {
            start = None;
        }
        at += line.len();
    }
    None
}

fn body_of(text: &str) -> String {
    if let Some((_, body)) = unified(text) {
        return body.to_string();
    }
    if text.trim_start().starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(text.trim())
        && let Some(s) = v.get("output").and_then(Value::as_str)
    {
        return s.to_string();
    }
    text.to_string()
}

// ── Calls ───────────────────────────────────────────────────────────

/// A Codex shell call's command, parsed, and its `workdir`.
pub(crate) fn parse_call(name: &str, input: &Value) -> Option<(ParsedScript, Option<String>)> {
    let workdir = input
        .get("workdir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let parsed = match name {
        "exec_command" => parse_script(input.get("cmd")?.as_str()?),
        "shell_command" => parse_script(input.get("command")?.as_str()?),
        "shell" => match input.get("command")? {
            Value::String(s) => parse_script(s),
            Value::Array(argv) => {
                let argv: Vec<&str> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
                parse_argv(&argv)
            }
            _ => return None,
        },
        _ => return None,
    };
    Some((parsed, workdir))
}

// ── Paths ───────────────────────────────────────────────────────────

/// Where a call's script starts: `exec_command_end.cwd`, else `workdir`, else the turn's cwd.
pub(crate) fn base_dir(
    ev: Option<&CallEvidence>,
    workdir: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    if let Some(c) = ev
        .and_then(|e| e.exec_end.as_ref())
        .and_then(|e| e.cwd.as_deref())
        .filter(|c| !c.is_empty())
    {
        return Some(normalize(c));
    }
    match workdir {
        Some(w) => Some(join(cwd, w)),
        None => cwd.map(normalize),
    }
}

pub(crate) fn join(dir: Option<&str>, path: &str) -> String {
    match dir {
        Some(d) if !path.starts_with('/') => normalize(&format!("{d}/{path}")),
        _ => normalize(path),
    }
}

// ── Tracking and folding ────────────────────────────────────────────

/// Whole-file content known so far, by resolved path; absent means unknown.
pub(crate) type Known = BTreeMap<String, String>;

/// `extra.codex` for the change keyed `path` from call `tool_id`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stamp {
    pub tool_id: String,
    pub path: String,
    pub value: Value,
}

/// Everything shell writes add to a derived path.
#[derive(Debug, Default)]
pub(crate) struct Stamps {
    pub files: Vec<Stamp>,
    /// `unresolved_shell_writes` entries, each naming its call by `tool_id`.
    pub unresolved: Vec<Value>,
}

#[derive(Debug, Default)]
pub(crate) struct TurnWrites {
    /// The turn's rebuilt mutations; `None` leaves the observed ones untouched.
    pub mutations: Option<Vec<FileMutation>>,
    pub stamps: Vec<Stamp>,
    /// `(call id, path)` of each inferred change, for `files_changed`.
    pub first_seen: Vec<(String, String)>,
    pub unresolved: Vec<Value>,
}

pub(crate) struct Ctx<'a> {
    pub evidence: &'a Evidence,
    pub stdin: &'a StdinExits,
    pub cwd: Option<&'a str>,
}

/// A turn's mutations with shell writes folded in, updating `known` call by call.
pub(crate) fn turn_writes(
    tools: &[ToolInvocation],
    observed: &[FileMutation],
    ctx: &Ctx<'_>,
    known: &mut Known,
) -> TurnWrites {
    let mut entries: Vec<Entry> = Vec::new();
    let mut unresolved: Vec<Value> = Vec::new();
    let mut any = false;
    let ids: HashSet<&str> = tools.iter().map(|t| t.id.as_str()).collect();
    for tool in tools {
        let ev = ctx.evidence.get(&tool.id);
        let patch_end = ev.and_then(|e| e.patch_end);
        let mine: Vec<&FileMutation> = observed
            .iter()
            .filter(|m| m.tool_id.as_deref() == Some(tool.id.as_str()))
            .collect();
        let mut reporter = None;
        let call;
        let parsed = parse_call(&tool.name, &tool.input);
        if let Some((parsed, workdir)) = &parsed {
            call = Call {
                tool,
                base: base_dir(ev, workdir.as_deref(), ctx.cwd),
                oc: outcome(
                    &tool.id,
                    ctx.evidence,
                    ctx.stdin,
                    parsed.patches().next().is_some(),
                ),
                reported: patch_end.is_some() && !mine.is_empty(),
            };
            any |= shell(&call, parsed, known, &mut entries, &mut unresolved);
            reporter = patch_end
                .zip(parsed.patches().next())
                .map(|(ok, patch)| Reporter {
                    call: &call,
                    patch,
                    ok,
                });
        } else if mine.is_empty() {
            forget_written(known, tool);
        }
        observe(
            mine.into_iter(),
            patch_end,
            ctx.cwd,
            known,
            &mut entries,
            reporter.as_ref(),
        );
    }
    let orphans = observed
        .iter()
        .filter(|m| m.tool_id.as_deref().is_none_or(|id| !ids.contains(id)));
    observe(orphans, None, ctx.cwd, known, &mut entries, None);
    let mut out = finish(entries);
    if !any {
        out.mutations = None;
    }
    out.unresolved = unresolved;
    out
}

struct Call<'a> {
    tool: &'a ToolInvocation,
    base: Option<String>,
    oc: ShellOutcome,
    /// A `patch_apply_end` names the call: Codex applied its patch itself.
    reported: bool,
}

impl Call<'_> {
    /// The call's success means this command ran and succeeded.
    fn certain(&self, link: StatusLink) -> bool {
        self.oc.outcome == Outcome::Success && link != StatusLink::Independent
    }
}

struct Entry {
    m: FileMutation,
    execs: Vec<Value>,
    outcomes: Vec<Outcome>,
    source: Option<&'static str>,
    first_call: Option<String>,
    /// Known content before the first shell write of the current run.
    before: Option<String>,
    all_append: bool,
}

impl Entry {
    fn new(m: FileMutation, before: Option<String>, all_append: bool) -> Self {
        Self {
            m,
            execs: Vec::new(),
            outcomes: Vec::new(),
            source: None,
            first_call: None,
            before,
            all_append,
        }
    }
}

/// Where an append's prior content came from.
#[derive(Clone, Copy)]
enum Base {
    /// Known earlier in the session.
    Tracked,
    /// An earlier write in the same script, which may not have run.
    Script,
    Unknown,
}

impl Base {
    fn as_str(self) -> &'static str {
        match self {
            Base::Tracked => "tracked",
            Base::Script => "script",
            Base::Unknown => "unknown",
        }
    }
}

/// A shell call whose `apply_patch` Codex intercepted and reported in `patch_apply_end`.
struct Reporter<'c> {
    call: &'c Call<'c>,
    patch: &'c HeredocPatch,
    /// `patch_apply_end.success`.
    ok: bool,
}

impl Reporter<'_> {
    /// The call's outcome, with `patch_apply_end` deciding unless the call itself failed.
    fn outcome(&self) -> ShellOutcome {
        let oc = &self.call.oc;
        if self.ok && oc.outcome == Outcome::Failure {
            return oc.clone();
        }
        ShellOutcome {
            outcome: if self.ok {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            basis: Basis::PatchApplyEnd,
            ..oc.clone()
        }
    }

    fn execution(&self, op: &str) -> Value {
        let oc = self.outcome();
        let mut v = patch_execution(self.call, self.patch, op);
        if let Some(m) = v.as_object_mut() {
            outcome_fields(m, &oc);
            m.insert("patch_apply_end".into(), json!(self.ok));
        }
        v
    }
}

fn observe<'m>(
    mutations: impl Iterator<Item = &'m FileMutation>,
    patch_ok: Option<bool>,
    cwd: Option<&str>,
    known: &mut Known,
    entries: &mut Vec<Entry>,
    reporter: Option<&Reporter>,
) {
    for m in mutations {
        let key = join(cwd, &m.path);
        match (m.operation.as_deref(), m.after.as_deref()) {
            (Some("add"), Some(a)) if patch_ok != Some(false) => {
                known.insert(key, a.to_string());
            }
            _ => {
                known.remove(&key);
            }
        }
        if let Some(to) = &m.rename_to {
            known.remove(&join(cwd, to));
        }
        let i = match entries
            .iter()
            .rposition(|e| e.m.path == m.path && !e.execs.is_empty())
        {
            Some(i) => i,
            None if reporter.is_some() => {
                entries.push(Entry::new(FileMutation::default(), None, false));
                entries.len() - 1
            }
            None => {
                entries.push(Entry::new(m.clone(), None, false));
                continue;
            }
        };
        let e = &mut entries[i];
        e.m = m.clone();
        e.source = None;
        if let Some(r) = reporter {
            e.source = Some(SOURCE_APPLY_PATCH);
            e.all_append = false;
            e.execs
                .push(r.execution(m.operation.as_deref().unwrap_or("update")));
            e.outcomes.push(r.outcome().outcome);
        }
    }
}

/// Forgets what a tool with no `patch_apply_end` changes may have written:
/// the files it names, or everything when it may write where it does not say.
fn forget_written(known: &mut Known, tool: &ToolInvocation) {
    match tool.category {
        Some(ToolCategory::FileRead | ToolCategory::FileSearch | ToolCategory::Network) => {}
        Some(ToolCategory::FileWrite) if !names_a_file(&tool.input) => known.clear(),
        Some(ToolCategory::Delegation) => known.clear(),
        _ => forget_mentioned(known, &tool.input.to_string()),
    }
}

fn names_a_file(input: &Value) -> bool {
    match input {
        Value::String(s) => !parse_patch(s).is_empty(),
        Value::Object(o) => ["path", "file_path", "filePath", "filename", "absolute_path"]
            .iter()
            .any(|k| {
                o.get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(|p| !p.is_empty())
            }),
        _ => false,
    }
}

/// Whether any write or patch became a change. `script` holds what this
/// call's own writes intend, whatever the outcome.
fn shell(
    c: &Call,
    parsed: &ParsedScript,
    known: &mut Known,
    entries: &mut Vec<Entry>,
    unresolved: &mut Vec<Value>,
) -> bool {
    let mut script = Known::new();
    let mut any = false;
    for item in &parsed.items {
        match item {
            ShellItem::Write(w) => {
                heredoc(c, w, known, &mut script, entries);
                any = true;
            }
            ShellItem::Unresolved(u) => {
                if u.reason == Unresolvable::NotLiteral {
                    known.clear();
                    script.clear();
                } else {
                    forget_mentioned(known, &u.write.path);
                    forget_mentioned(&mut script, &u.write.path);
                }
                unresolved.push(write_attempt(c, &u.write, u.reason.as_str()));
            }
            ShellItem::Patch(p) if c.reported => {
                forget_mentioned(known, &p.body);
                forget_mentioned(&mut script, &p.body);
            }
            ShellItem::Patch(p) => {
                any |= shell_patch(c, p, known, &mut script, entries, unresolved)
            }
            ShellItem::Other(text) => {
                forget_mentioned(known, text);
                forget_mentioned(&mut script, text);
            }
            _ => {}
        }
    }
    any
}

fn heredoc(
    c: &Call,
    w: &HeredocWrite,
    known: &mut Known,
    script: &mut Known,
    entries: &mut Vec<Entry>,
) {
    let path = join(c.base.as_deref(), &w.path);
    let (prior, base) = match (script.get(&path), known.get(&path)) {
        (Some(s), _) => (Some(s.clone()), Base::Script),
        (None, Some(k)) => (Some(k.clone()), Base::Tracked),
        (None, None) => (None, Base::Unknown),
    };
    let after = if w.append {
        prior.as_ref().map(|p| format!("{p}{}", w.body))
    } else {
        Some(w.body.clone())
    };
    set(script, &path, after.clone());
    let known_after = match (c.certain(w.status_link), w.append) {
        (false, _) => None,
        (true, false) => Some(w.body.clone()),
        (true, true) => known.get(&path).map(|k| format!("{k}{}", w.body)),
    };
    set(known, &path, known_after);
    let exec = heredoc_execution(c, w, base);
    let i = match entries.iter().rposition(|e| e.m.path == path) {
        Some(i) => {
            let e = &mut entries[i];
            if e.source != Some(SOURCE_HEREDOC) {
                e.before = prior;
                e.all_append = true;
            }
            i
        }
        None => {
            entries.push(Entry::new(FileMutation::default(), prior, true));
            entries.len() - 1
        }
    };
    let e = &mut entries[i];
    e.source = Some(SOURCE_HEREDOC);
    e.all_append &= w.append;
    e.m = heredoc_mutation(
        &path,
        &c.tool.id,
        e.before.as_deref(),
        after.as_deref(),
        e.all_append,
    );
    e.first_call.get_or_insert_with(|| c.tool.id.clone());
    e.execs.push(exec);
    e.outcomes.push(c.oc.outcome);
}

/// An unknown prior diffs from empty; an append onto unknown content is structural only.
fn heredoc_mutation(
    path: &str,
    tool_id: &str,
    before: Option<&str>,
    after: Option<&str>,
    all_append: bool,
) -> FileMutation {
    FileMutation {
        path: path.to_string(),
        tool_id: Some(tool_id.to_string()),
        operation: all_append.then(|| "append".to_string()),
        raw_diff: after.map(|a| unified_diff(path, before.unwrap_or(""), a)),
        before: after.and(before).map(str::to_string),
        after: after.map(str::to_string),
        ..Default::default()
    }
}

fn set(map: &mut Known, key: &str, value: Option<String>) {
    match value {
        Some(v) => {
            map.insert(key.to_string(), v);
        }
        None => {
            map.remove(key);
        }
    }
}

/// A shell patch's files, shaped as `patch_apply_end` would give them.
fn shell_patch(
    c: &Call,
    p: &HeredocPatch,
    known: &mut Known,
    script: &mut Known,
    entries: &mut Vec<Entry>,
    unresolved: &mut Vec<Value>,
) -> bool {
    let files = p.files();
    if files.is_empty() {
        forget_mentioned(known, &p.body);
        forget_mentioned(script, &p.body);
        return false;
    }
    let certain = c.certain(p.status_link);
    let base = c.base.as_deref();
    let mut any = false;
    for f in files {
        let resolved = p.resolve(&f.path).zip(match f.move_to.as_deref() {
            Some(to) => p.resolve(to).map(Some),
            None => Some(None),
        });
        let Some((target, to)) = resolved else {
            for name in std::iter::once(&f.path).chain(f.move_to.as_ref()) {
                forget_mentioned(known, name);
                forget_mentioned(script, name);
            }
            unresolved.push(patch_attempt(
                c,
                p,
                &f.path,
                f.op.as_str(),
                Unresolvable::UnknownDir.as_str(),
            ));
            continue;
        };
        let path = join(base, &target);
        let rename_to = to.map(|t| join(base, &t));
        let prior = script.get(&path).or_else(|| known.get(&path)).cloned();
        set(script, &path, f.added.clone());
        set(known, &path, f.added.clone().filter(|_| certain));
        if let Some(to) = &rename_to {
            known.remove(to);
            script.remove(to);
        }
        let m = FileMutation {
            path: path.clone(),
            tool_id: Some(c.tool.id.clone()),
            operation: Some(f.op.as_str().to_string()),
            raw_diff: match f.op {
                PatchOp::Add => f.added.as_deref().map(synth_add_diff),
                PatchOp::Delete => prior.as_deref().map(synth_delete_diff),
                _ => None,
            },
            before: prior.filter(|_| f.op == PatchOp::Delete),
            after: f.added.clone(),
            rename_to,
        };
        let exec = patch_execution(c, p, f.op.as_str());
        let i = match entries.iter().rposition(|e| e.m.path == path) {
            Some(i) => i,
            None => {
                entries.push(Entry::new(FileMutation::default(), None, false));
                entries.len() - 1
            }
        };
        let e = &mut entries[i];
        e.m = m;
        e.source = Some(SOURCE_APPLY_PATCH);
        e.all_append = false;
        e.first_call.get_or_insert_with(|| c.tool.id.clone());
        e.execs.push(exec);
        e.outcomes.push(c.oc.outcome);
        any = true;
    }
    any
}

fn outcome_fields(m: &mut Map<String, Value>, oc: &ShellOutcome) {
    m.insert("outcome".into(), json!(oc.outcome.as_str()));
    m.insert("outcome_basis".into(), json!(oc.basis.as_str()));
    if let Some(code) = oc.exit_code {
        m.insert("exit_code".into(), json!(code));
    }
    if let Some(via) = &oc.exit_via {
        m.insert("exit_via".into(), json!(via));
    }
    if let Some(s) = &oc.exec_status {
        m.insert("exec_status".into(), json!(s));
    }
}

fn link_fields(m: &mut Map<String, Value>, link: StatusLink) {
    m.insert("sole_command".into(), json!(link == StatusLink::Sole));
    m.insert(
        "implied_by_success".into(),
        json!(link != StatusLink::Independent),
    );
}

fn redirect(w: &HeredocWrite) -> Value {
    json!(if w.append { "append" } else { "write" })
}

fn heredoc_execution(c: &Call, w: &HeredocWrite, base: Base) -> Value {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert("redirect".into(), redirect(w));
    m.insert("via".into(), json!(w.via.as_str()));
    outcome_fields(&mut m, &c.oc);
    link_fields(&mut m, w.status_link);
    m.insert("tag".into(), json!(w.tag));
    m.insert("tag_quoted".into(), json!(w.tag_quoted));
    if w.strip_tabs {
        m.insert("strip_tabs".into(), json!(true));
    }
    if w.may_expand() {
        m.insert("may_expand".into(), json!(true));
    }
    m.insert("body".into(), json!(w.body));
    if w.append {
        m.insert("append_base".into(), json!(base.as_str()));
    }
    Value::Object(m)
}

fn patch_execution(c: &Call, p: &HeredocPatch, op: &str) -> Value {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert("via".into(), json!(p.command));
    m.insert("operation".into(), json!(op));
    outcome_fields(&mut m, &c.oc);
    link_fields(&mut m, p.status_link);
    if !p.tag.is_empty() {
        m.insert("tag".into(), json!(p.tag));
        m.insert("tag_quoted".into(), json!(p.tag_quoted));
    }
    if p.strip_tabs {
        m.insert("strip_tabs".into(), json!(true));
    }
    if !p.tag_quoted && p.body.contains(['$', '`', '\\']) {
        m.insert("may_expand".into(), json!(true));
    }
    Value::Object(m)
}

fn attempt(c: &Call, path: &str, reason: &str, via: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert("path_as_written".into(), json!(path));
    m.insert("reason".into(), json!(reason));
    m.insert("via".into(), json!(via));
    outcome_fields(&mut m, &c.oc);
    m
}

fn write_attempt(c: &Call, w: &HeredocWrite, reason: &str) -> Value {
    let mut m = attempt(c, &w.path, reason, w.via.as_str());
    m.insert("redirect".into(), redirect(w));
    link_fields(&mut m, w.status_link);
    m.insert("body".into(), json!(w.body));
    Value::Object(m)
}

fn patch_attempt(c: &Call, p: &HeredocPatch, path: &str, op: &str, reason: &str) -> Value {
    let mut m = attempt(c, path, reason, &p.command);
    m.insert("operation".into(), json!(op));
    link_fields(&mut m, p.status_link);
    Value::Object(m)
}

/// Forgets tracked files `text` names: an unmodeled command may have changed them.
fn forget_mentioned(known: &mut Known, text: &str) {
    known.retain(|k, _| {
        let name = k.rsplit('/').next().unwrap_or(k);
        name.is_empty() || !text.contains(name)
    });
}

fn summary(outcomes: &[Outcome]) -> Outcome {
    if outcomes.contains(&Outcome::Failure) {
        Outcome::Failure
    } else if outcomes.contains(&Outcome::Unknown) {
        Outcome::Unknown
    } else {
        Outcome::Success
    }
}

fn finish(entries: Vec<Entry>) -> TurnWrites {
    let mut out = TurnWrites::default();
    let mut mutations = Vec::with_capacity(entries.len());
    for e in entries {
        if !e.execs.is_empty() {
            let mut v = Map::new();
            if let Some(source) = e.source {
                v.insert("source".into(), json!(source));
            }
            v.insert("outcome".into(), json!(summary(&e.outcomes).as_str()));
            v.insert("executions".into(), Value::Array(e.execs));
            if let Some(tool_id) = &e.m.tool_id {
                out.stamps.push(Stamp {
                    tool_id: tool_id.clone(),
                    path: e.m.path.clone(),
                    value: Value::Object(v),
                });
            }
            if let Some(call) = e.first_call {
                out.first_seen.push((call, e.m.path.clone()));
            }
        }
        mutations.push(e.m);
    }
    out.mutations = Some(mutations);
    out
}

/// `files_changed` with each inferred path at its first call's position.
pub(crate) fn merge_files_changed(
    order: Vec<String>,
    inferred: &[(String, String)],
    anchors: &HashMap<String, usize>,
) -> Vec<String> {
    if inferred.is_empty() {
        return order;
    }
    let mut at: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for (call, path) in inferred {
        let i = anchors
            .get(call)
            .copied()
            .unwrap_or(order.len())
            .min(order.len());
        at.entry(i).or_default().push(path);
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(order.len() + inferred.len());
    for i in 0..=order.len() {
        for p in at.get(&i).into_iter().flatten() {
            if seen.insert((*p).to_string()) {
                out.push((*p).to_string());
            }
        }
        if let Some(p) = order.get(i)
            && seen.insert(p.clone())
        {
            out.push(p.clone());
        }
    }
    out
}

/// Writes each file stamp onto its `file.write` change, and each unresolved
/// attempt onto the conversation change of the step holding its call.
pub(crate) fn apply_stamps(path: &mut toolpath::v1::Path, conversation_key: &str, stamps: &Stamps) {
    if stamps.files.is_empty() && stamps.unresolved.is_empty() {
        return;
    }
    let by_key: HashMap<(&str, &str), &Value> = stamps
        .files
        .iter()
        .map(|s| ((s.path.as_str(), s.tool_id.as_str()), &s.value))
        .collect();
    for step in &mut path.steps {
        for (key, change) in step.change.iter_mut() {
            let Some(st) = change.structural.as_mut() else {
                continue;
            };
            if key == conversation_key {
                let calls: HashSet<&str> = st
                    .extra
                    .get("tool_uses")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.get("id").and_then(Value::as_str))
                    .collect();
                let mine: Vec<Value> = stamps
                    .unresolved
                    .iter()
                    .filter(|a| {
                        a.get("tool_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| calls.contains(id))
                    })
                    .cloned()
                    .collect();
                if !mine.is_empty() {
                    let slot = st
                        .extra
                        .entry(EXTRA_KEY.to_string())
                        .or_insert_with(|| json!({}));
                    if let Some(obj) = slot.as_object_mut() {
                        obj.insert("unresolved_shell_writes".into(), Value::Array(mine));
                    }
                }
                continue;
            }
            if st.change_type != "file.write" {
                continue;
            }
            let Some(tool_id) = st.extra.get("tool_id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(v) = by_key.get(&(key.as_str(), tool_id)) {
                st.extra.insert(EXTRA_KEY.to_string(), (*v).clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::ToolResult;

    const OK: &str = "Chunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\n";
    const FAIL: &str = "Chunk ID: 2\nWall time: 0.0 seconds\nProcess exited with code 1\nOriginal token count: 4\nOutput:\nsh: nope: not found\n";

    fn tool(id: &str, name: &str, input: Value) -> ToolInvocation {
        ToolInvocation {
            id: id.into(),
            name: name.into(),
            input,
            result: Some(ToolResult {
                content: String::new(),
                is_error: false,
            }),
            category: crate::provider::tool_category(name),
        }
    }

    fn exec(id: &str, cmd: &str) -> ToolInvocation {
        tool(id, "exec_command", json!({"cmd": cmd, "workdir": "/w"}))
    }

    fn out(ev: &mut Evidence, id: &str, text: &str) {
        ev.entry(id.into())
            .or_default()
            .outputs
            .push((text.into(), false));
    }

    fn end(ev: &mut Evidence, id: &str, code: Option<i32>, status: &str, cwd: Option<&str>) {
        ev.entry(id.into()).or_default().exec_end = Some(ExecEnd {
            exit_code: code,
            status: Some(status.into()),
            cwd: cwd.map(str::to_string),
        });
    }

    fn run(
        tools: &[ToolInvocation],
        observed: &[FileMutation],
        ev: &Evidence,
        known: &mut Known,
    ) -> TurnWrites {
        let stdin = StdinExits::new();
        let ctx = Ctx {
            evidence: ev,
            stdin: &stdin,
            cwd: Some("/w"),
        };
        turn_writes(tools, observed, &ctx, known)
    }

    fn muts(t: &TurnWrites) -> &[FileMutation] {
        t.mutations.as_deref().expect("mutations rebuilt")
    }

    fn only(t: &TurnWrites) -> (&FileMutation, &Value) {
        assert_eq!(muts(t).len(), 1, "{:?}", t.mutations);
        assert_eq!(t.stamps.len(), 1, "{:?}", t.stamps);
        (&muts(t)[0], &t.stamps[0].value)
    }

    // ── outcome ──

    #[test]
    fn exec_command_end_exit_code_is_the_outcome() {
        let mut ev = Evidence::new();
        end(&mut ev, "c", Some(0), "completed", None);
        out(&mut ev, "c", FAIL);
        let oc = outcome("c", &ev, &StdinExits::new(), false);
        assert_eq!(
            (oc.outcome, oc.basis, oc.exit_code),
            (Outcome::Success, Basis::ExitCode, Some(0))
        );
        end(&mut ev, "c", Some(2), "failed", None);
        let oc = outcome("c", &ev, &StdinExits::new(), false);
        assert_eq!((oc.outcome, oc.exit_code), (Outcome::Failure, Some(2)));
    }

    #[test]
    fn a_declined_exec_without_exit_code_is_a_failure_on_its_status() {
        let mut ev = Evidence::new();
        end(&mut ev, "c", None, "declined", None);
        let oc = outcome("c", &ev, &StdinExits::new(), false);
        assert_eq!(
            (oc.outcome, oc.basis),
            (Outcome::Failure, Basis::ExecStatus)
        );
        assert_eq!(oc.exec_status.as_deref(), Some("declined"));
    }

    #[test]
    fn unified_exec_header_markers() {
        let mut ev = Evidence::new();
        out(&mut ev, "ok", OK);
        out(&mut ev, "bad", FAIL);
        out(
            &mut ev,
            "run",
            "Chunk ID: 3\nWall time: 1.0 seconds\nProcess running with session ID 33850\nOriginal token count: 0\nOutput:\n",
        );
        out(&mut ev, "quoted", "Output:\nProcess exited with code 9\n");
        let s = StdinExits::new();
        assert_eq!(outcome("ok", &ev, &s, false).exit_code, Some(0));
        assert_eq!(outcome("bad", &ev, &s, false).outcome, Outcome::Failure);
        let r = outcome("run", &ev, &s, false);
        assert_eq!(
            (r.outcome, r.basis),
            (Outcome::Unknown, Basis::StillRunning)
        );
        let q = outcome("quoted", &ev, &s, false);
        assert_eq!(
            (q.outcome, q.basis, q.exit_code),
            (Outcome::Success, Basis::NoErrorReported, None)
        );
    }

    #[test]
    fn a_running_process_takes_the_exit_a_write_stdin_poll_reported() {
        let mut ev = Evidence::new();
        out(
            &mut ev,
            "c",
            "Chunk ID: 3\nWall time: 1.0 seconds\nProcess running with session ID 7\nOriginal token count: 0\nOutput:\n",
        );
        out(&mut ev, "poll", FAIL);
        let poll = tool("poll", "write_stdin", json!({"session_id": 7, "chars": ""}));
        let stdin = stdin_exits([&exec("c", "sleep 9"), &poll], &ev);
        let oc = outcome("c", &ev, &stdin, false);
        assert_eq!(
            (oc.outcome, oc.exit_code, oc.exit_via.as_deref()),
            (Outcome::Failure, Some(1), Some("poll"))
        );
    }

    const RUNNING_7: &str = "Chunk ID: 3\nWall time: 1.0 seconds\nProcess running with session ID 7\nOriginal token count: 0\nOutput:\n";

    #[test]
    fn a_reused_session_id_binds_each_call_to_the_first_poll_after_it() {
        let mut ev = Evidence::new();
        for id in ["a", "b", "c"] {
            out(&mut ev, id, RUNNING_7);
        }
        out(&mut ev, "early", FAIL);
        out(&mut ev, "pa", OK);
        out(&mut ev, "pa2", FAIL);
        let poll = |id: &str| tool(id, "write_stdin", json!({"session_id": 7, "chars": ""}));
        let tools = [
            poll("early"),
            exec("a", "sleep 1"),
            poll("pa"),
            poll("pa2"),
            exec("b", "sleep 2"),
            exec("c", "sleep 3"),
        ];
        let stdin = stdin_exits(&tools, &ev);
        let a = outcome("a", &ev, &stdin, false);
        assert_eq!(
            (a.outcome, a.exit_code, a.exit_via.as_deref()),
            (Outcome::Success, Some(0), Some("pa"))
        );
        // b is superseded by c before any poll; c has no poll after it.
        for id in ["b", "c"] {
            let r = outcome(id, &ev, &stdin, false);
            assert_eq!(
                (r.outcome, r.basis, r.exit_code),
                (Outcome::Unknown, Basis::StillRunning, None),
                "{id}"
            );
        }
    }

    #[test]
    fn an_echoed_command_does_not_hide_or_fake_the_status() {
        let echo = "Command: /bin/bash -lc 'cat <<EOF > f\nProcess exited with code 3\nOutput:\nEOF'\nChunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\nhi\n";
        assert_eq!(status(echo), Some(Status::Exited(0)));
        assert_eq!(body_of(echo), "hi\n");
        let running = format!("Command: sleep 9\n{RUNNING_7}");
        assert_eq!(status(&running), Some(Status::Running("7".into())));
    }

    #[test]
    fn shell_json_and_shell_command_markers() {
        let mut ev = Evidence::new();
        out(
            &mut ev,
            "j",
            r#"{"output":"hi\n","metadata":{"exit_code":3,"duration_seconds":0.1}}"#,
        );
        out(
            &mut ev,
            "e",
            "Exit code: 0\nWall time: 0.1 seconds\nOutput:\nhi\n",
        );
        let s = StdinExits::new();
        assert_eq!(outcome("j", &ev, &s, false).exit_code, Some(3));
        assert_eq!(outcome("e", &ev, &s, false).exit_code, Some(0));
    }

    #[test]
    fn error_flag_no_result_and_verification_failure() {
        let mut ev = Evidence::new();
        ev.entry("err".into())
            .or_default()
            .outputs
            .push(("x".into(), true));
        out(
            &mut ev,
            "vf",
            "apply_patch verification failed: Failed to find expected lines in /w/a.rs:\n  x\n",
        );
        let s = StdinExits::new();
        assert_eq!(outcome("err", &ev, &s, false).basis, Basis::IsError);
        let n = outcome("none", &ev, &s, false);
        assert_eq!((n.outcome, n.basis), (Outcome::Unknown, Basis::NoResult));
        assert_eq!(outcome("vf", &ev, &s, true).basis, Basis::ErrorText);
        // Only for an apply_patch script; elsewhere it is ordinary output.
        assert_eq!(outcome("vf", &ev, &s, false).outcome, Outcome::Success);
    }

    // ── calls and patches ──

    fn items(name: &str, input: Value) -> Vec<ShellItem> {
        parse_call(name, &input).unwrap().0.items
    }

    #[test]
    fn parse_call_reads_each_codex_shell_tool() {
        let w = |items: Vec<ShellItem>| match &items[..] {
            [ShellItem::Write(w)] => w.path.clone(),
            other => panic!("{other:?}"),
        };
        let heredoc = "cat <<'EOF' > a\nx\nEOF";
        assert_eq!(w(items("exec_command", json!({"cmd": heredoc}))), "a");
        assert_eq!(w(items("shell_command", json!({"command": heredoc}))), "a");
        assert_eq!(
            w(items("shell", json!({"command": ["bash", "-lc", heredoc]}))),
            "a"
        );
        assert_eq!(
            parse_call("exec_command", &json!({"cmd": "ls", "workdir": "/w"}))
                .unwrap()
                .1
                .as_deref(),
            Some("/w")
        );
        assert!(parse_call("write_stdin", &json!({"chars": "ls\n"})).is_none());
        assert!(parse_call("apply_patch", &json!("*** Begin Patch")).is_none());
    }

    #[test]
    fn shell_argv_apply_patch_is_a_patch() {
        let body = "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\n";
        match &items("shell", json!({"command": ["apply_patch", body]}))[..] {
            [ShellItem::Patch(p)] => {
                assert_eq!(
                    (p.command.as_str(), p.body.as_str(), p.tag.as_str()),
                    ("apply_patch", body, "")
                );
                assert_eq!(p.status_link, StatusLink::Sole);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &items("shell", json!({"command": ["rg", "x", "a.txt"]}))[..],
            [ShellItem::Other(t)] if t == "rg x a.txt"
        ));
    }

    // ── turn_writes ──

    #[test]
    fn heredoc_write_success_full_stamp() {
        let mut ev = Evidence::new();
        out(&mut ev, "c1", OK);
        let mut known = Known::new();
        let t = run(
            &[exec("c1", "cat <<'EOF' > wc.py\nprint(1)\nEOF")],
            &[],
            &ev,
            &mut known,
        );
        let (m, v) = only(&t);
        assert_eq!(m.path, "/w/wc.py");
        assert_eq!(m.tool_id.as_deref(), Some("c1"));
        assert_eq!(m.operation, None);
        assert_eq!(m.after.as_deref(), Some("print(1)\n"));
        assert_eq!(m.before, None);
        assert!(m.raw_diff.as_deref().unwrap().contains("+print(1)"));
        assert_eq!(
            *v,
            json!({
                "source": "shell-heredoc",
                "outcome": "success",
                "executions": [{
                    "tool_id": "c1", "tool": "exec_command",
                    "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
                    "redirect": "write", "via": "cat",
                    "sole_command": true, "implied_by_success": true,
                    "tag": "EOF", "tag_quoted": true, "body": "print(1)\n"
                }]
            })
        );
        assert_eq!(
            known.get("/w/wc.py").map(String::as_str),
            Some("print(1)\n")
        );
        assert_eq!(
            t.first_seen,
            vec![("c1".to_string(), "/w/wc.py".to_string())]
        );
        assert!(t.unresolved.is_empty());
    }

    #[test]
    fn failed_and_unanswered_writes_are_recorded_and_forget() {
        let mut ev = Evidence::new();
        out(&mut ev, "f", FAIL);
        let mut known = Known::from([("/w/a.txt".to_string(), "old\n".to_string())]);
        let t = run(
            &[
                exec("f", "mkdir -p x && cat <<'EOF' > a.txt\nnew\nEOF"),
                exec("u", "cat <<'EOF' > b.txt\nb\nEOF"),
            ],
            &[],
            &ev,
            &mut known,
        );
        assert_eq!(muts(&t).len(), 2);
        let f = &t.stamps[0].value;
        assert_eq!(f["outcome"], "failure");
        assert_eq!(f["executions"][0]["exit_code"], 1);
        assert_eq!(f["executions"][0]["sole_command"], false);
        assert_eq!(f["executions"][0]["implied_by_success"], true);
        // The intended content is recorded, diffed against what was known.
        assert_eq!(muts(&t)[0].before.as_deref(), Some("old\n"));
        assert_eq!(t.stamps[1].value["outcome"], "unknown");
        assert_eq!(
            t.stamps[1].value["executions"][0]["outcome_basis"],
            "no_result"
        );
        assert!(known.is_empty());
    }

    #[test]
    fn a_successful_write_the_status_does_not_vouch_for_is_not_known() {
        let mut ev = Evidence::new();
        out(&mut ev, "c", OK);
        let mut known = Known::new();
        let t = run(
            &[exec("c", "cat <<'EOF' > a.txt\nx\nEOF\nfalse; true")],
            &[],
            &ev,
            &mut known,
        );
        let (_, v) = only(&t);
        assert_eq!(v["outcome"], "success");
        assert_eq!(v["executions"][0]["implied_by_success"], false);
        assert!(known.is_empty());
    }

    #[test]
    fn append_onto_tracked_script_and_unknown_content() {
        let mut ev = Evidence::new();
        for id in ["a", "b", "c"] {
            out(&mut ev, id, OK);
        }
        let mut known = Known::from([("/w/log".to_string(), "1\n".to_string())]);
        let t = run(
            &[
                exec("a", "cat >> log <<'EOF'\n2\nEOF"),
                exec("b", "cat >> other <<'EOF'\nx\nEOF"),
                exec("c", "cat > s <<'EOF'\n1\nEOF\ncat >> s <<'EOF'\n2\nEOF"),
            ],
            &[],
            &ev,
            &mut known,
        );
        let m = muts(&t);
        assert_eq!(m[0].operation.as_deref(), Some("append"));
        assert_eq!(
            (m[0].before.as_deref(), m[0].after.as_deref()),
            (Some("1\n"), Some("1\n2\n"))
        );
        assert_eq!(t.stamps[0].value["executions"][0]["append_base"], "tracked");
        assert_eq!(t.stamps[0].value["executions"][0]["body"], "2\n");
        assert_eq!(
            (
                m[1].raw_diff.as_ref(),
                m[1].before.as_ref(),
                m[1].after.as_ref()
            ),
            (None, None, None)
        );
        assert_eq!(m[1].operation.as_deref(), Some("append"));
        assert_eq!(t.stamps[1].value["executions"][0]["append_base"], "unknown");
        assert_eq!(m[2].after.as_deref(), Some("1\n2\n"));
        assert_eq!(m[2].operation, None);
        assert_eq!(t.stamps[2].value["executions"][1]["append_base"], "script");
        assert_eq!(known.get("/w/log").map(String::as_str), Some("1\n2\n"));
        assert!(!known.contains_key("/w/other"));
    }

    #[test]
    fn paths_resolve_against_exec_end_cwd_then_workdir_then_turn_cwd() {
        let mut ev = Evidence::new();
        end(&mut ev, "e", Some(0), "completed", Some("/real"));
        let tools = [
            exec("e", "cat <<'EOF' > a\n1\nEOF"),
            tool(
                "w",
                "exec_command",
                json!({"cmd": "cat <<'EOF' > b\n1\nEOF", "workdir": "sub"}),
            ),
            tool(
                "t",
                "shell_command",
                json!({"command": "cd d && cat <<'EOF' > ../c\n1\nEOF"}),
            ),
            exec("abs", "cat <<'EOF' > /etc/x\n1\nEOF"),
        ];
        let t = run(&tools, &[], &ev, &mut Known::new());
        let paths: Vec<&str> = muts(&t).iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["/real/a", "/w/sub/b", "/w/c", "/etc/x"]);
    }

    #[test]
    fn same_path_twice_in_a_turn_folds_into_one_change() {
        let mut ev = Evidence::new();
        out(&mut ev, "a", OK);
        out(&mut ev, "b", FAIL);
        let t = run(
            &[
                exec("a", "cat <<'EOF' > f\n1\nEOF"),
                exec("b", "cat >> f <<'EOF'\n2\nEOF"),
            ],
            &[],
            &ev,
            &mut Known::new(),
        );
        let (m, v) = only(&t);
        assert_eq!(m.tool_id.as_deref(), Some("b"));
        assert_eq!(m.operation, None);
        assert_eq!(m.after.as_deref(), Some("1\n2\n"));
        assert_eq!(v["outcome"], "failure");
        assert_eq!(v["executions"].as_array().unwrap().len(), 2);
        assert_eq!(t.first_seen, vec![("a".to_string(), "/w/f".to_string())]);
    }

    #[test]
    fn unresolvable_targets_are_attempts_never_changes() {
        let mut ev = Evidence::new();
        for id in ["n", "d", "p"] {
            out(&mut ev, id, FAIL);
        }
        let mut known = Known::from([
            ("/w/rel.txt".to_string(), "r\n".to_string()),
            ("/w/keep".to_string(), "k\n".to_string()),
        ]);
        let t = run(
            &[
                exec("d", "cd sub; cat <<'EOF' > rel.txt\nr\nEOF"),
                exec(
                    "p",
                    "cd sub; apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: q.txt\n+q\n*** Delete File: /w/abs.txt\n*** End Patch\nEOF",
                ),
            ],
            &[],
            &ev,
            &mut known,
        );
        assert_eq!(t.unresolved.len(), 2);
        assert_eq!(
            t.unresolved[0],
            json!({
                "tool_id": "d", "tool": "exec_command", "path_as_written": "rel.txt",
                "reason": "unknown_dir", "via": "cat", "redirect": "write", "body": "r\n",
                "outcome": "failure", "outcome_basis": "exit_code", "exit_code": 1,
                "sole_command": false, "implied_by_success": true
            })
        );
        assert_eq!(t.unresolved[1]["path_as_written"], "q.txt");
        assert_eq!(t.unresolved[1]["operation"], "add");
        assert_eq!(t.unresolved[1]["via"], "apply_patch");
        // The absolute file in the same patch still resolves.
        let (m, _) = only(&t);
        assert_eq!(
            (m.path.as_str(), m.operation.as_deref()),
            ("/w/abs.txt", Some("delete"))
        );
        assert!(!known.contains_key("/w/rel.txt"));
        assert!(known.contains_key("/w/keep"));

        // A non-literal target forgets everything and changes nothing.
        let t = run(
            &[exec("n", "cat > $OUT <<'EOF'\nx\nEOF")],
            &[],
            &ev,
            &mut known,
        );
        assert!(t.mutations.is_none() && t.stamps.is_empty());
        assert_eq!(t.unresolved[0]["reason"], "not_literal");
        assert_eq!(t.unresolved[0]["path_as_written"], "$OUT");
        assert!(known.is_empty());
    }

    #[test]
    fn shell_apply_patch_without_patch_apply_end_is_inferred() {
        let mut ev = Evidence::new();
        out(
            &mut ev,
            "p",
            r#"{"output":"Success. Updated the following files:\nA a.txt\n","metadata":{"exit_code":0,"duration_seconds":0.1}}"#,
        );
        let script = "cd sub && apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: a.txt\n+hi\n*** Update File: b.rs\n@@\n-x\n+y\n*** Delete File: /w/c.txt\n*** End Patch\nEOF\n";
        let mut known = Known::from([("/w/c.txt".to_string(), "gone\n".to_string())]);
        let t = run(
            &[tool(
                "p",
                "shell",
                json!({"command": ["bash", "-lc", script], "workdir": "/w"}),
            )],
            &[],
            &ev,
            &mut known,
        );
        let m = muts(&t);
        let ops: Vec<(&str, Option<&str>)> = m
            .iter()
            .map(|m| (m.path.as_str(), m.operation.as_deref()))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("/w/sub/a.txt", Some("add")),
                ("/w/sub/b.rs", Some("update")),
                ("/w/c.txt", Some("delete"))
            ]
        );
        assert_eq!(m[0].after.as_deref(), Some("hi\n"));
        assert_eq!(m[0].raw_diff.as_deref(), Some("@@ -0,0 +1,1 @@\n+hi\n"));
        assert_eq!(m[1].raw_diff, None);
        assert_eq!(m[2].before.as_deref(), Some("gone\n"));
        assert_eq!(m[2].raw_diff.as_deref(), Some("@@ -1,1 +0,0 @@\n-gone\n"));
        for s in &t.stamps {
            let e = &s.value["executions"][0];
            assert_eq!(s.value["source"], "shell-apply-patch");
            assert_eq!(e["via"], "apply_patch");
            assert_eq!(e["exit_code"], 0);
            assert_eq!(e["sole_command"], false);
            assert_eq!(e["implied_by_success"], true);
        }
        assert_eq!(known.get("/w/sub/a.txt").map(String::as_str), Some("hi\n"));
        assert!(!known.contains_key("/w/c.txt"));
    }

    #[test]
    fn a_verification_failure_is_a_recorded_failure() {
        let mut ev = Evidence::new();
        out(
            &mut ev,
            "p",
            "apply_patch verification failed: Failed to find expected lines in /w/b.rs:\n    x\n",
        );
        let t = run(
            &[tool(
                "p",
                "shell",
                json!({"command": ["apply_patch", "*** Begin Patch\n*** Update File: b.rs\n@@\n-x\n+y\n*** End Patch\n"], "workdir": "/w"}),
            )],
            &[],
            &ev,
            &mut Known::new(),
        );
        let (m, v) = only(&t);
        assert_eq!(m.path, "/w/b.rs");
        assert_eq!(v["outcome"], "failure");
        assert_eq!(v["executions"][0]["outcome_basis"], "error_text");
        assert!(v["executions"][0].get("tag").is_none());
        assert_eq!(v["executions"][0]["sole_command"], true);
    }

    fn reported_patch(ok: bool) -> (Evidence, Vec<FileMutation>, ToolInvocation) {
        let mut ev = Evidence::new();
        out(
            &mut ev,
            "p",
            "Success. Updated the following files:\nA a.txt\n",
        );
        ev.entry("p".into()).or_default().patch_end = Some(ok);
        let observed = vec![FileMutation {
            path: "/w/a.txt".into(),
            tool_id: Some("p".into()),
            operation: Some("add".into()),
            after: Some("hi\n".into()),
            raw_diff: Some("@@ -0,0 +1,1 @@\n+hi\n".into()),
            ..Default::default()
        }];
        let call = exec(
            "p",
            "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\nEOF",
        );
        (ev, observed, call)
    }

    #[test]
    fn a_reported_shell_patch_is_the_observed_change_stamped_with_its_outcome() {
        let (ev, observed, call) = reported_patch(true);
        let mut known = Known::new();
        let t = run(&[call], &observed, &ev, &mut known);
        assert!(
            t.mutations.is_none(),
            "the turn's mutations are left exactly as observed"
        );
        assert!(t.first_seen.is_empty() && t.unresolved.is_empty());
        assert_eq!(t.stamps.len(), 1, "{:?}", t.stamps);
        assert_eq!(
            (t.stamps[0].tool_id.as_str(), t.stamps[0].path.as_str()),
            ("p", "/w/a.txt")
        );
        assert_eq!(
            t.stamps[0].value,
            json!({
                "source": "shell-apply-patch",
                "outcome": "success",
                "executions": [{
                    "tool_id": "p", "tool": "exec_command", "via": "apply_patch",
                    "operation": "add", "patch_apply_end": true,
                    "outcome": "success", "outcome_basis": "patch_apply_end",
                    "sole_command": true, "implied_by_success": true,
                    "tag": "EOF", "tag_quoted": true
                }]
            })
        );
        assert_eq!(known.get("/w/a.txt").map(String::as_str), Some("hi\n"));
    }

    #[test]
    fn a_reported_shell_patch_that_failed_is_a_recorded_failure() {
        let (ev, observed, call) = reported_patch(false);
        let mut known = Known::new();
        let t = run(&[call], &observed, &ev, &mut known);
        assert!(t.mutations.is_none());
        let v = &t.stamps[0].value;
        assert_eq!(v["outcome"], "failure");
        assert_eq!(v["executions"][0]["outcome"], "failure");
        assert_eq!(v["executions"][0]["outcome_basis"], "patch_apply_end");
        assert_eq!(v["executions"][0]["patch_apply_end"], false);
        assert!(known.is_empty());

        // A failing exit code still wins over a successful patch_apply_end.
        let (mut ev, observed, call) = reported_patch(true);
        end(&mut ev, "p", Some(1), "failed", None);
        let t = run(&[call], &observed, &ev, &mut Known::new());
        let e = &t.stamps[0].value["executions"][0];
        assert_eq!(
            (
                e["outcome"].as_str(),
                e["outcome_basis"].as_str(),
                e["exit_code"].as_i64()
            ),
            (Some("failure"), Some("exit_code"), Some(1))
        );
    }

    #[test]
    fn a_patch_apply_end_with_no_changes_falls_back_to_shell_inference() {
        // patch_apply_end fired (ok) but named no files in `changes`, so there
        // is no observed mutation for this call. The shell-inferred write must
        // still be recorded instead of being silently dropped (review C1).
        let (ev, _observed, call) = reported_patch(true);
        let mut known = Known::new();
        let t = run(&[call], &[], &ev, &mut known);
        let (m, v) = only(&t);
        assert_eq!(m.path, "/w/a.txt");
        assert_eq!(m.operation.as_deref(), Some("add"));
        assert_eq!(v["outcome"], "success");
        assert_eq!(v["executions"][0]["tool_id"], "p");
        assert_eq!(v["executions"][0]["operation"], "add");
        assert_eq!(known.get("/w/a.txt").map(String::as_str), Some("hi\n"));
    }

    #[test]
    fn file_writing_tools_without_patch_apply_end_forget_tracked_content() {
        let mut ev = Evidence::new();
        for id in ["a", "b", "c"] {
            out(&mut ev, id, OK);
        }
        let mut known = Known::from([
            ("/w/f".to_string(), "A\n".to_string()),
            ("/w/keep".to_string(), "k\n".to_string()),
        ]);
        let t = run(
            &[
                tool("r", "read_file", json!({"path": "/w/keep"})),
                tool("w", "write_file", json!({"path": "/w/f", "content": "B\n"})),
                exec("a", "cat >> f <<'EOF'\nC\nEOF"),
            ],
            &[],
            &ev,
            &mut known,
        );
        let (m, v) = only(&t);
        assert_eq!((m.before.as_ref(), m.after.as_ref()), (None, None));
        assert_eq!(v["executions"][0]["append_base"], "unknown");
        assert!(known.contains_key("/w/keep"));

        // An MCP tool forgets what it names; a file write with no path, everything.
        let mut known = Known::from([
            ("/w/alpha.txt".to_string(), "A\n".to_string()),
            ("/w/beta.txt".to_string(), "B\n".to_string()),
        ]);
        run(
            &[tool("m", "fs:touch", json!({"target": "/w/alpha.txt"}))],
            &[],
            &ev,
            &mut known,
        );
        assert!(!known.contains_key("/w/alpha.txt") && known.contains_key("/w/beta.txt"));
        run(
            &[tool("e", "edit", json!({"old": "x", "new": "y"}))],
            &[],
            &ev,
            &mut known,
        );
        assert!(known.is_empty());
    }

    #[test]
    fn an_observed_write_later_in_the_turn_takes_the_change() {
        let mut ev = Evidence::new();
        out(&mut ev, "s", OK);
        ev.entry("p".into()).or_default().patch_end = Some(true);
        let observed = vec![FileMutation {
            path: "/w/f".into(),
            tool_id: Some("p".into()),
            operation: Some("update".into()),
            raw_diff: Some("@@ -1 +1 @@\n-1\n+2\n".into()),
            ..Default::default()
        }];
        let t = run(
            &[
                exec("s", "cat <<'EOF' > f\n1\nEOF"),
                tool(
                    "p",
                    "apply_patch",
                    json!("*** Begin Patch\n*** Update File: /w/f\n@@\n-1\n+2\n*** End Patch"),
                ),
            ],
            &observed,
            &ev,
            &mut Known::new(),
        );
        let (m, v) = only(&t);
        assert_eq!(m.tool_id.as_deref(), Some("p"));
        assert_eq!(m.operation.as_deref(), Some("update"));
        assert!(v.get("source").is_none());
        assert_eq!(v["executions"][0]["tool_id"], "s");
    }

    #[test]
    fn non_matching_commands_write_nothing_and_forget_what_they_name() {
        let mut ev = Evidence::new();
        for id in ["a", "b", "c", "d"] {
            out(&mut ev, id, OK);
        }
        let mut known = Known::from([
            ("/w/notes.md".to_string(), "n\n".to_string()),
            ("/w/keep.txt".to_string(), "k\n".to_string()),
        ]);
        let t = run(
            &[
                exec("a", "sed -i 's/a/b/' notes.md"),
                exec("b", "git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n)\""),
                exec("c", "cat <<'EOF'\nto stdout\nEOF"),
                tool(
                    "d",
                    "write_stdin",
                    json!({"session_id": 1, "chars": "ls\n"}),
                ),
            ],
            &[],
            &ev,
            &mut known,
        );
        assert!(t.mutations.is_none() && t.unresolved.is_empty());
        assert!(!known.contains_key("/w/notes.md"));
        assert!(known.contains_key("/w/keep.txt"));
    }

    #[test]
    fn a_turn_without_shell_writes_is_left_alone_but_seeds_known() {
        let observed = vec![FileMutation {
            path: "/w/a.rs".into(),
            tool_id: Some("p".into()),
            operation: Some("add".into()),
            after: Some("fn main() {}\n".into()),
            ..Default::default()
        }];
        let mut ev = Evidence::new();
        ev.entry("p".into()).or_default().patch_end = Some(true);
        let mut known = Known::new();
        let t = run(
            &[tool("p", "apply_patch", json!("…"))],
            &observed,
            &ev,
            &mut known,
        );
        assert!(t.mutations.is_none());
        assert_eq!(
            known.get("/w/a.rs").map(String::as_str),
            Some("fn main() {}\n")
        );
        // A failed patch seeds nothing.
        ev.get_mut("p").unwrap().patch_end = Some(false);
        let mut known = Known::new();
        run(
            &[tool("p", "apply_patch", json!("…"))],
            &observed,
            &ev,
            &mut known,
        );
        assert!(known.is_empty());
    }

    // ── files_changed ──

    #[test]
    fn merge_places_inferred_paths_at_their_call() {
        let order = vec!["/p/x".to_string(), "/p/y".to_string()];
        let anchors = HashMap::from([
            ("c0".to_string(), 0),
            ("c1".to_string(), 1),
            ("c2".to_string(), 2),
        ]);
        let inferred = vec![
            ("c0".to_string(), "/p/a".to_string()),
            ("c1".to_string(), "/p/y".to_string()),
            ("c2".to_string(), "/p/z".to_string()),
        ];
        assert_eq!(
            merge_files_changed(order.clone(), &inferred, &anchors),
            vec!["/p/a", "/p/x", "/p/y", "/p/z"]
        );
        assert_eq!(merge_files_changed(order.clone(), &[], &anchors), order);
    }
}
