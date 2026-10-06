//! A turn's file mutations: observed ones from write/edit/patch calls and
//! ones inferred from shell heredocs, with whole-file content carried
//! along the turn's ancestry.

use super::mutations::{fallback_path, file_mutations, patch_mutations};
use super::shell::{
    Basis, Outcome, ShellCall, ShellOutcome, call_outcome, shell_call, shell_outcome,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::rc::Rc;
use toolpath_convo::shell_writes::{
    HeredocPatch, HeredocWrite, ParsedScript, ScriptDir, ShellItem, StatusLink, UnmodeledTarget,
    UnmodeledWrite, Unresolvable, normalize_path as normalize,
};
use toolpath_convo::{FileMutation, ToolCategory, ToolInvocation, ToolResult, unified_diff};

/// `extra.otel.source` of a change inferred from a shell heredoc write.
pub const SOURCE: &str = "shell-heredoc";
/// `extra.otel.source` of a change inferred from a shell `apply_patch <<EOF`.
pub const SOURCE_PATCH: &str = "shell-apply-patch";
/// `reason` of a relative target after a persistent shell may have moved.
const DIR_MOVED: &str = "shell_dir_moved";
/// `reason` of a relative target of a call naming a directory not read.
const UNKNOWN_WORKDIR: &str = "unknown_workdir";

/// Whole-file content known at a point in the conversation, by
/// [`track_key`]; absent means unknown. `Rc` values keep per-turn copies cheap.
pub type Known = BTreeMap<String, Rc<str>>;

/// Claude Code's note, on a result's last line, that it moved its shell
/// back to its original directory.
const CWD_RESET: &str = "Shell cwd was reset to ";
/// Claude Code tools that move its shell and the directory it resets to.
const WORKTREE_TOOLS: [&str; 2] = ["EnterWorktree", "ExitWorktree"];
/// Commands whose exit 1 Claude Code reports as no error.
const EXIT_1_NOT_AN_ERROR: [&str; 8] =
    ["grep", "rg", "egrep", "fgrep", "find", "diff", "test", "["];

/// What a turn starts from. `known` comes from its ancestry only; `dir`
/// also from earlier generations' calls on other branches (see
/// `docs/agents/formats/otel.md`).
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    pub known: Known,
    /// Where a shell that may keep its working directory between calls
    /// (Claude Code `Bash`) is. `Start`: the turn's working directory,
    /// whether or not a `cd` persisted. `At`: an absolute directory under
    /// it, if `cd`s persisted, which the trace cannot show (a subagent's
    /// shell, or `CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR`, resets after
    /// every call), so relative targets there are unresolved and carry it
    /// as a `likely_path`. `Unknown`: unresolved, with no hint.
    pub dir: ScriptDir,
    /// A worktree tool may have moved the directory a reset returns the
    /// shell to, so the directory is not `Start` until a reset note names it.
    pub start_moved: bool,
}

impl ShellState {
    /// The persistent shell's directory as an absolute path, if known.
    pub fn shell_dir<'a>(&'a self, cwd: Option<&'a str>) -> Option<&'a str> {
        match &self.dir {
            ScriptDir::Start => cwd,
            ScriptDir::At(d) => Some(d),
            _ => None,
        }
    }
}

/// Shell tools whose working directory persists from one call to the next.
const PERSISTENT_CWD: [&str; 1] = ["Bash"];

/// Whether a call's answer can decide where it leaves a persistent shell,
/// which turns off its ancestry read ([`TurnWrites::shell_dir_touched`]):
/// a worktree tool, or a `Bash` call the classifier names a shell call
/// (`category`, as [`turn_writes`] reads it) that changes directory, whose
/// input cannot be read, or whose `result` carries a reset note. A `Bash`
/// call without a `cd` is taken never to be answered with one.
pub fn may_move_shell(
    tool: &str,
    category: Option<ToolCategory>,
    input: &Value,
    result: Option<&str>,
) -> bool {
    if WORKTREE_TOOLS.contains(&tool) {
        return true;
    }
    if category != Some(ToolCategory::Shell) || !PERSISTENT_CWD.contains(&tool) {
        return false;
    }
    result.is_some_and(|r| r.contains(CWD_RESET.trim_end()))
        || shell_call(input).is_none_or(|sc| !sc.parsed.dir_changes.is_empty())
}

/// `extra.otel` of a turn's file changes, by the mutation's `path`.
pub type Stamps = BTreeMap<String, Value>;

/// The result of [`turn_writes`].
#[derive(Debug, Default)]
pub struct TurnWrites {
    /// In tool-call order; paths from shell writes appear once per turn.
    pub mutations: Vec<FileMutation>,
    /// `extra.otel` for the inferred changes.
    pub stamps: Stamps,
    /// Shell writes whose target cannot be resolved: recorded, never a change.
    pub unresolved: Vec<Value>,
    /// A call may have moved a shell whose directory persists (Claude Code
    /// `Bash`): a directory change, a reset note, a worktree tool, or an
    /// unreadable `Bash` input.
    pub shell_dir_touched: bool,
}

/// Mutations of one turn's calls, updating `state` as each call runs.
/// Shell writes to one path fold into one mutation (changes are keyed by
/// path); a later observed write takes it over, keeping the executions.
pub fn turn_writes(
    tools: &[ToolInvocation],
    cwd: Option<&str>,
    state: &mut ShellState,
) -> TurnWrites {
    let mut entries: Vec<Entry> = Vec::new();
    let mut unresolved: Vec<Value> = Vec::new();
    let mut touched = false;
    for tool in tools {
        let moves = match tool.category {
            _ if WORKTREE_TOOLS.contains(&tool.name.as_str()) => {
                let failed = tool.result.as_ref().is_some_and(|r| r.is_error);
                if !failed {
                    state.dir = ScriptDir::Unknown;
                    state.start_moved = true;
                }
                !failed
            }
            Some(ToolCategory::FileWrite) => {
                observed(tool, cwd, &mut state.known, &mut entries);
                false
            }
            Some(ToolCategory::Shell) => shell(tool, cwd, state, &mut entries, &mut unresolved),
            _ => false,
        };
        if moves {
            // Calls of one turn may run concurrently.
            if touched {
                state.dir = ScriptDir::Unknown;
            }
            touched = true;
        }
    }
    let mut out = finish(entries, unresolved);
    out.shell_dir_touched = touched;
    out
}

/// The key content is tracked under: the path joined onto `cwd` when
/// relative, lexically normalized.
pub fn track_key(path: &str, cwd: Option<&str>) -> String {
    match cwd {
        Some(c) if !path.starts_with('/') => normalize(&format!("{c}/{path}")),
        _ => normalize(path),
    }
}

/// The change key and tracking key of a heredoc target. A relative target
/// is joined onto the call's `workdir` only when that differs from `cwd`.
pub fn resolve(path: &str, workdir: Option<&str>, cwd: Option<&str>) -> (String, String) {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        match workdir.filter(|wd| cwd.is_none_or(|c| normalize(wd) != normalize(c))) {
            Some(wd) => normalize(&format!("{wd}/{path}")),
            None => path.to_string(),
        }
    };
    let key = track_key(&path, cwd);
    (path, key)
}

struct Entry {
    m: FileMutation,
    execs: Vec<Value>,
    outcomes: Vec<Outcome>,
    /// Where the change's content comes from: a shell source ([`SOURCE`],
    /// [`SOURCE_PATCH`]), or `None` for an observed write.
    source: Option<&'static str>,
    /// Known content before the first shell write of the current run.
    before: Option<Rc<str>>,
    all_append: bool,
}

fn observed(tool: &ToolInvocation, cwd: Option<&str>, known: &mut Known, entries: &mut Vec<Entry>) {
    // Left to convo's fallback: no mutation here, but its change replaces
    // a shell write's to the same path.
    if let Some(path) = fallback_path(tool) {
        known.remove(&track_key(path, cwd));
        if let Some(e) = entries
            .iter_mut()
            .rev()
            .find(|e| e.m.path == path && !e.execs.is_empty())
        {
            e.source = None;
        }
        return;
    }
    let ok = shell_outcome(&tool.name, tool.result.as_ref()).outcome == Outcome::Success;
    // A notebook edit's `after` is one cell's source.
    let cell = tool.input.get("new_source").is_some();
    for m in file_mutations(tool) {
        let key = track_key(&m.path, cwd);
        // Whole-file content: a Write (`after` without `before`) or a
        // patch's added file. Edits carry fragments, so they forget.
        let whole = m.operation.as_deref() == Some("add")
            || (!cell && m.operation.is_none() && m.before.is_none() && m.after.is_some());
        match m.after.as_deref() {
            Some(a) if ok && whole => {
                known.insert(key, a.into());
            }
            _ => {
                known.remove(&key);
            }
        }
        if let Some(to) = &m.rename_to {
            known.remove(&track_key(to, cwd));
        }
        match entries
            .iter_mut()
            .rev()
            .find(|e| e.m.path == m.path && !e.execs.is_empty())
        {
            Some(e) => {
                e.m = m;
                e.source = None;
            }
            None => entries.push(Entry {
                m,
                execs: Vec::new(),
                outcomes: Vec::new(),
                source: None,
                before: None,
                all_append: false,
            }),
        }
    }
}

struct Call<'a> {
    tool: &'a ToolInvocation,
    workdir: Option<&'a str>,
    cwd: Option<&'a str>,
    oc: ShellOutcome,
    /// A success read from the error flag alone may be an exit 1
    /// ([`EXIT_1_NOT_AN_ERROR`]).
    lenient: bool,
    /// Why relative targets cannot be resolved, if they cannot.
    relative_unresolved: Option<&'static str>,
    /// Where earlier `cd`s left a persistent shell if they persisted,
    /// relative to `cwd`.
    tracked: Option<&'a str>,
}

impl Call<'_> {
    /// `path` (relative to the call's start) joined onto [`Call::tracked`].
    /// `None` when absolute, or when it climbs above that directory, which
    /// may have been reached through a symlink.
    fn likely(&self, path: &str) -> Option<String> {
        let dir = self.tracked.filter(|_| self.workdir.is_none())?;
        let path = normalize(path);
        if path.starts_with('/') || path == ".." || path.starts_with("../") {
            return None;
        }
        Some(normalize(&format!("{dir}/{path}")))
    }

    /// The call's success means this command ran and succeeded.
    fn certain(&self, link: StatusLink) -> bool {
        self.exited_zero() && link != StatusLink::Independent
    }

    /// The call exited zero.
    fn exited_zero(&self) -> bool {
        self.oc.outcome == Outcome::Success && !self.lenient
    }
}

/// Some simple command is one whose exit 1 Claude Code reports as no error.
fn lenient(parsed: &ParsedScript) -> bool {
    parsed
        .command_words
        .iter()
        .any(|words| match words.as_slice() {
            [first, ..] if EXIT_1_NOT_AN_ERROR.contains(&first.as_str()) => true,
            [git, rest @ ..] if git == "git" => {
                let mut args = rest.iter();
                while let Some(a) = args.next() {
                    match a.as_str() {
                        "-C" | "-c" => {
                            args.next();
                        }
                        a if a.starts_with('-') => {}
                        sub => return matches!(sub, "grep" | "diff"),
                    }
                }
                false
            }
            _ => false,
        })
}

/// Where an append's prior content came from.
#[derive(Clone, Copy)]
enum Base {
    /// Known along the ancestry or earlier in the turn.
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

/// `script` holds what this call's own writes intend, whatever the outcome:
/// a later write in the same script builds on it. Calls of one turn do not
/// share it, since they may run in parallel. Returns whether the call may
/// have moved a persistent shell.
fn shell(
    tool: &ToolInvocation,
    cwd: Option<&str>,
    state: &mut ShellState,
    entries: &mut Vec<Entry>,
    unresolved: &mut Vec<Value>,
) -> bool {
    let persistent = PERSISTENT_CWD.contains(&tool.name.as_str());
    let Some(sc) = shell_call(&tool.input) else {
        state.known.clear();
        if persistent {
            state.dir = ScriptDir::Unknown;
            after_reset(tool.result.as_ref(), cwd, state);
        }
        return persistent;
    };
    let dir = if persistent {
        state.dir.clone()
    } else {
        ScriptDir::Start
    };
    let start = cwd.map(normalize);
    let tracked = match (&dir, start.as_deref()) {
        (ScriptDir::At(d), Some(c)) => d.strip_prefix(c).and_then(|r| r.strip_prefix('/')),
        _ => None,
    };
    let oc = call_outcome(&tool.name, &sc, tool.result.as_ref());
    let call = Call {
        tool,
        workdir: sc.workdir.as_deref(),
        cwd,
        lenient: oc.outcome == Outcome::Success
            && oc.basis == Basis::NoErrorReported
            && lenient(&sc.parsed),
        oc,
        relative_unresolved: if sc.workdir_unknown {
            Some(UNKNOWN_WORKDIR)
        } else if dir != ScriptDir::Start {
            Some(DIR_MOVED)
        } else {
            None
        },
        tracked,
    };
    items(&call, &sc, &mut state.known, entries, unresolved);
    if !persistent {
        return false;
    }
    let moved = !sc.parsed.dir_changes.is_empty();
    if moved {
        state.dir = moved_to(state, &call, &sc.parsed);
    }
    after_reset(tool.result.as_ref(), cwd, state) || moved
}

fn items(
    call: &Call,
    sc: &ShellCall,
    known: &mut Known,
    entries: &mut Vec<Entry>,
    unresolved: &mut Vec<Value>,
) {
    let mut script = Known::new();
    for item in &sc.parsed.items {
        match item {
            ShellItem::Write(w) => match call
                .relative_unresolved
                .filter(|_| !w.path.starts_with('/'))
            {
                None => heredoc(call, w, known, &mut script, entries),
                Some(reason) => {
                    forget_mentioned(known, &w.path);
                    forget_mentioned(&mut script, &w.path);
                    unresolved.push(write_attempt(call, w, reason));
                }
            },
            ShellItem::Unresolved(u) => {
                if u.reason == Unresolvable::NotLiteral {
                    known.clear();
                    script.clear();
                } else {
                    forget_mentioned(known, &u.write.path);
                    forget_mentioned(&mut script, &u.write.path);
                }
                unresolved.push(write_attempt(call, &u.write, u.reason.as_str()));
            }
            ShellItem::Patch(p) => shell_patch(call, p, known, &mut script, entries, unresolved),
            ShellItem::Other(text) => {
                forget_mentioned(known, text);
                forget_mentioned(&mut script, text);
            }
            ShellItem::Unmodeled(u) => {
                forget_mentioned(known, &u.command);
                forget_mentioned(&mut script, &u.command);
                for t in &u.targets {
                    let path = unmodeled_path(call, t);
                    if let Some(p) = &path {
                        let key = track_key(p, call.cwd);
                        known.remove(&key);
                        script.remove(&key);
                    } else if !t.literal {
                        known.clear();
                        script.clear();
                    }
                    unresolved.push(unmodeled_attempt(call, u, t, path));
                }
            }
            _ => {}
        }
    }
}

/// The change key of an unmodeled target, when it resolves.
fn unmodeled_path(c: &Call, t: &UnmodeledTarget) -> Option<String> {
    let r = t.resolved.as_deref()?;
    if c.relative_unresolved.is_some() && !r.starts_with('/') {
        return None;
    }
    Some(resolve(r, c.workdir, c.cwd).0)
}

/// Applies Claude Code's reset note, if the result mentions one: the shell
/// is where it says, and that is `Start` only when it names `cwd`. A
/// mention not on the last line leaves the directory unknown.
fn after_reset(result: Option<&ToolResult>, cwd: Option<&str>, state: &mut ShellState) -> bool {
    let Some(content) = result.map(|r| r.content.as_str()) else {
        return false;
    };
    if !content.contains(CWD_RESET.trim_end()) {
        return false;
    }
    let last = content.trim_end().rsplit('\n').next().unwrap_or_default();
    let named = last.strip_prefix(CWD_RESET).map(normalize);
    if named.is_some() && named == cwd.map(normalize) {
        state.dir = ScriptDir::Start;
        state.start_moved = false;
    } else {
        state.dir = ScriptDir::Unknown;
    }
    true
}

/// Where a persistent shell is after `call`, which may have changed its
/// directory, if `cd` persists. `At` only when certain: the call exited
/// zero and every `cd` in it ran ([`ParsedScript::dir_on_success`]), or
/// every `cd` names where it already is. Claude Code moves a shell that
/// leaves the project back, so a directory outside `cwd` is `Unknown`.
fn moved_to(state: &ShellState, call: &Call, parsed: &ParsedScript) -> ScriptDir {
    let Some(cwd) = call.cwd.map(normalize) else {
        return ScriptDir::Unknown;
    };
    let from = state.shell_dir(Some(&cwd)).map(normalize);
    let stays = from.is_some()
        && parsed
            .dir_changes
            .iter()
            .all(|d| matches!(d, ScriptDir::At(d) if Some(d) == from.as_ref()));
    if stays {
        return state.dir.clone();
    }
    if !call.exited_zero() {
        return ScriptDir::Unknown;
    }
    let climbs = |t: &str| t == ".." || t.starts_with("../");
    let to = match (&parsed.dir_on_success, from) {
        (ScriptDir::At(t), _) if t.starts_with('/') => t.clone(),
        // The shell keeps `pwd -P`: `..` from a directory reached through a
        // symlink leaves the link's target.
        (ScriptDir::At(t), Some(from)) if from == cwd || !climbs(t) => {
            normalize(&format!("{from}/{t}"))
        }
        _ => return ScriptDir::Unknown,
    };
    if to == cwd && !state.start_moved {
        ScriptDir::Start
    } else if to.strip_prefix(&cwd).is_some_and(|r| r.starts_with('/')) {
        ScriptDir::At(to)
    } else {
        ScriptDir::Unknown
    }
}

fn heredoc(
    c: &Call,
    w: &HeredocWrite,
    known: &mut Known,
    script: &mut Known,
    entries: &mut Vec<Entry>,
) {
    let (path, key) = resolve(&w.path, c.workdir, c.cwd);
    let (prior, base) = match (script.get(&key), known.get(&key)) {
        (Some(s), _) => (Some(s.clone()), Base::Script),
        (None, Some(k)) => (Some(k.clone()), Base::Tracked),
        (None, None) => (None, Base::Unknown),
    };
    let after: Option<Rc<str>> = if w.append {
        prior.as_ref().map(|p| format!("{p}{}", w.body).into())
    } else {
        Some(w.body.as_str().into())
    };
    set(script, &key, after.clone());
    let known_after: Option<Rc<str>> = match (c.certain(w.status_link), w.append) {
        (false, _) => None,
        (true, false) => Some(w.body.as_str().into()),
        (true, true) => known.get(&key).map(|k| format!("{k}{}", w.body).into()),
    };
    set(known, &key, known_after);
    let exec = execution(c, w, base);
    let i = match entries.iter().rposition(|e| e.m.path == path) {
        Some(i) => {
            let e = &mut entries[i];
            // A run of heredoc writes starts from the content known
            // before its first write.
            if e.source != Some(SOURCE) {
                e.before = prior;
                e.all_append = true;
            }
            i
        }
        None => {
            entries.push(Entry {
                m: FileMutation::default(),
                execs: Vec::new(),
                outcomes: Vec::new(),
                source: None,
                before: prior,
                all_append: true,
            });
            entries.len() - 1
        }
    };
    let e = &mut entries[i];
    e.source = Some(SOURCE);
    e.all_append &= w.append;
    e.m = shell_mutation(
        &path,
        &c.tool.id,
        e.before.as_deref(),
        after.as_deref(),
        e.all_append,
    );
    e.execs.push(exec);
    e.outcomes.push(c.oc.outcome);
}

fn set(map: &mut Known, key: &str, value: Option<Rc<str>>) {
    match value {
        Some(v) => {
            map.insert(key.to_string(), v);
        }
        None => {
            map.remove(key);
        }
    }
}

/// `apply_patch <<EOF` in a shell call: the same mutations as the
/// `apply_patch` tool ([`patch_mutations`]). A marker-less body records nothing
/// and forgets what it names.
fn shell_patch(
    c: &Call,
    patch: &HeredocPatch,
    known: &mut Known,
    script: &mut Known,
    entries: &mut Vec<Entry>,
    unresolved: &mut Vec<Value>,
) {
    let muts = patch_mutations(&patch.body);
    if muts.is_empty() {
        forget_mentioned(known, &patch.body);
        forget_mentioned(script, &patch.body);
        return;
    }
    let certain = c.certain(patch.status_link);
    for pm in muts {
        let resolved = patch.resolve(&pm.path).zip(match pm.rename_to.as_deref() {
            Some(to) => patch.resolve(to).map(Some),
            None => Some(None),
        });
        let resolved = match resolved {
            None => Err(Unresolvable::UnknownDir.as_str()),
            Some((t, to)) => match c.relative_unresolved {
                Some(reason)
                    if !t.starts_with('/') || to.as_ref().is_some_and(|t| !t.starts_with('/')) =>
                {
                    Err(reason)
                }
                _ => Ok((t, to)),
            },
        };
        let (target, to) = match resolved {
            Ok(r) => r,
            Err(reason) => {
                for name in std::iter::once(&pm.path).chain(pm.rename_to.as_ref()) {
                    forget_mentioned(known, name);
                    forget_mentioned(script, name);
                }
                let mut a = patch_attempt(c, patch, &pm.path, pm.operation.as_deref(), reason);
                let likely = patch.resolve(&pm.path).and_then(|t| c.likely(&t));
                if let Some(p) = likely.filter(|_| reason == DIR_MOVED) {
                    a.insert("likely_path".into(), json!(p));
                }
                unresolved.push(Value::Object(a));
                continue;
            }
        };
        let (path, key) = resolve(&target, c.workdir, c.cwd);
        let rename_to = to.map(|t| resolve(&t, c.workdir, c.cwd));
        let added = pm.operation.as_deref() == Some("add");
        let content = pm.after.as_deref().filter(|_| added).map(Rc::from);
        set(script, &key, content.clone());
        set(known, &key, content.filter(|_| certain));
        if let Some((_, to_key)) = &rename_to {
            known.remove(to_key);
            script.remove(to_key);
        }
        let exec = patch_execution(c, patch, pm.operation.as_deref());
        let m = FileMutation {
            path: path.clone(),
            rename_to: rename_to.map(|(to, _)| to),
            tool_id: Some(c.tool.id.clone()),
            ..pm
        };
        match entries.iter_mut().rev().find(|e| e.m.path == path) {
            Some(e) => {
                e.m = m;
                e.source = Some(SOURCE_PATCH);
                e.all_append = false;
                e.execs.push(exec);
                e.outcomes.push(c.oc.outcome);
            }
            None => entries.push(Entry {
                m,
                execs: vec![exec],
                outcomes: vec![c.oc.outcome],
                source: Some(SOURCE_PATCH),
                before: None,
                all_append: false,
            }),
        }
    }
}

fn patch_execution(c: &Call, p: &HeredocPatch, operation: Option<&str>) -> Value {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert("via".into(), json!(p.command));
    if let Some(op) = operation {
        m.insert("operation".into(), json!(op));
    }
    outcome_fields(&mut m, c);
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

fn link_fields(m: &mut Map<String, Value>, link: StatusLink) {
    m.insert("sole_command".into(), json!(link == StatusLink::Sole));
    m.insert(
        "implied_by_success".into(),
        json!(link != StatusLink::Independent),
    );
}

fn attempt(c: &Call, path: &str, reason: &str, via: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert("path_as_written".into(), json!(path));
    m.insert("reason".into(), json!(reason));
    m.insert("via".into(), json!(via));
    outcome_fields(&mut m, c);
    m
}

fn write_attempt(c: &Call, w: &HeredocWrite, reason: &str) -> Value {
    let mut m = attempt(c, &w.path, reason, w.via.as_str());
    m.insert(
        "redirect".into(),
        json!(if w.append { "append" } else { "write" }),
    );
    link_fields(&mut m, w.status_link);
    m.insert("body".into(), json!(w.body));
    if reason == DIR_MOVED
        && let Some(p) = c.likely(&w.path)
    {
        m.insert("likely_path".into(), json!(p));
    }
    Value::Object(m)
}

fn unmodeled_attempt(
    c: &Call,
    u: &UnmodeledWrite,
    t: &UnmodeledTarget,
    path: Option<String>,
) -> Value {
    let via = if t.tee { "tee" } else { "redirect" };
    let mut m = attempt(c, &t.path, Unresolvable::Unmodeled.as_str(), via);
    match path {
        Some(p) => {
            m.insert("path".into(), json!(p));
        }
        None => {
            if c.relative_unresolved == Some(DIR_MOVED)
                && let Some(p) = t.resolved.as_deref().and_then(|r| c.likely(r))
            {
                m.insert("likely_path".into(), json!(p));
            }
        }
    }
    if let Some(program) = &u.program {
        m.insert("command".into(), json!(program));
    }
    m.insert(
        "redirect".into(),
        json!(if t.append { "append" } else { "write" }),
    );
    link_fields(&mut m, u.status_link);
    Value::Object(m)
}

fn patch_attempt(
    c: &Call,
    p: &HeredocPatch,
    path: &str,
    operation: Option<&str>,
    reason: &str,
) -> Map<String, Value> {
    let mut m = attempt(c, path, reason, &p.command);
    if let Some(op) = operation {
        m.insert("operation".into(), json!(op));
    }
    link_fields(&mut m, p.status_link);
    m
}

fn outcome_fields(m: &mut Map<String, Value>, c: &Call) {
    m.insert("outcome".into(), json!(c.oc.outcome.as_str()));
    m.insert("outcome_basis".into(), json!(c.oc.basis.as_str()));
    if let Some(code) = c.oc.exit_code {
        m.insert("exit_code".into(), json!(code));
    }
    if c.lenient {
        m.insert("success_may_hide_exit_1".into(), json!(true));
    }
}

/// A shell write's mutation. An unknown prior diffs from empty, as a Write
/// does; an append onto unknown content is structural only.
fn shell_mutation(
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

fn execution(c: &Call, w: &HeredocWrite, base: Base) -> Value {
    let mut m = Map::new();
    m.insert("tool_id".into(), json!(c.tool.id));
    m.insert("tool".into(), json!(c.tool.name));
    m.insert(
        "redirect".into(),
        json!(if w.append { "append" } else { "write" }),
    );
    m.insert("via".into(), json!(w.via.as_str()));
    outcome_fields(&mut m, c);
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

/// Forgets every tracked file whose name `text` mentions: a command we do
/// not model may have changed it. Over-forgetting only costs a later
/// append its diff.
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

fn finish(entries: Vec<Entry>, unresolved: Vec<Value>) -> TurnWrites {
    let mut out = TurnWrites {
        unresolved,
        ..Default::default()
    };
    for e in entries {
        if !e.execs.is_empty() {
            let mut v = Map::new();
            if let Some(source) = e.source {
                v.insert("source".into(), json!(source));
            }
            v.insert("outcome".into(), json!(summary(&e.outcomes).as_str()));
            v.insert("executions".into(), Value::Array(e.execs));
            out.stamps.insert(e.m.path.clone(), Value::Object(v));
        }
        out.mutations.push(e.m);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller's classifier: the provider crates' agreed category, and
    /// Gemini CLI's `run_shell_command` (its sessions infer as `unknown`) a
    /// shell call.
    fn tool_category(name: &str) -> Option<ToolCategory> {
        match name {
            "run_shell_command" => Some(ToolCategory::Shell),
            _ => crate::tests::classifier::provider_tool_category("unknown", name),
        }
    }
    use toolpath_convo::ToolResult;

    const OK: &str = "Chunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\n";
    const FAIL: &str = "Chunk ID: 2\nWall time: 0.0 seconds\nProcess exited with code 1\nOriginal token count: 4\nOutput:\nsh: nope: not found\n";

    fn call(id: &str, name: &str, input: Value, result: Option<(&str, bool)>) -> ToolInvocation {
        ToolInvocation {
            id: id.into(),
            name: name.into(),
            input,
            result: result.map(|(c, e)| ToolResult {
                content: c.into(),
                is_error: e,
            }),
            category: tool_category(name),
        }
    }

    fn exec(id: &str, cmd: &str, result: Option<&str>) -> ToolInvocation {
        call(
            id,
            "exec_command",
            json!({"cmd": cmd, "workdir": "/w"}),
            result.map(|r| (r, false)),
        )
    }

    fn run(tools: &[ToolInvocation], known: &mut Known) -> TurnWrites {
        let mut state = ShellState {
            known: std::mem::take(known),
            ..Default::default()
        };
        let t = turn_writes(tools, Some("/w"), &mut state);
        *known = state.known;
        t
    }

    fn run_state(tools: &[ToolInvocation], state: &mut ShellState) -> TurnWrites {
        turn_writes(tools, Some("/w"), state)
    }

    fn only(t: &TurnWrites) -> (&FileMutation, &Value) {
        assert_eq!(t.mutations.len(), 1, "{:?}", t.mutations);
        let m = &t.mutations[0];
        (m, &t.stamps[&m.path])
    }

    #[test]
    fn a_successful_write_is_recorded_with_its_outcome() {
        let mut known = Known::new();
        let t = run(
            &[exec("c1", "cat <<'EOF' > wc.py\nprint(1)\nEOF", Some(OK))],
            &mut known,
        );
        let (m, s) = only(&t);
        assert_eq!(m.path, "wc.py");
        assert_eq!(m.tool_id.as_deref(), Some("c1"));
        assert_eq!(m.after.as_deref(), Some("print(1)\n"));
        assert_eq!(m.before, None);
        assert_eq!(m.operation, None);
        assert_eq!(
            m.raw_diff.as_deref(),
            Some(unified_diff("wc.py", "", "print(1)\n").as_str())
        );
        assert_eq!(
            s,
            &json!({
                "source": "shell-heredoc",
                "outcome": "success",
                "executions": [{
                    "tool_id": "c1", "tool": "exec_command", "redirect": "write", "via": "cat",
                    "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
                    "sole_command": true, "implied_by_success": true,
                    "tag": "EOF", "tag_quoted": true, "body": "print(1)\n"
                }]
            })
        );
        assert_eq!(known.get("/w/wc.py").map(|s| &**s), Some("print(1)\n"));
    }

    #[test]
    fn a_failed_write_is_recorded_and_forgets_the_file() {
        let mut known = Known::from([("/w/wc.py".to_string(), Rc::from("old\n"))]);
        let t = run(
            &[exec(
                "c1",
                "cat > wc.py <<'EOF'\nnew\nEOF\nnope",
                Some(FAIL),
            )],
            &mut known,
        );
        let (m, s) = only(&t);
        assert_eq!(
            (m.before.as_deref(), m.after.as_deref()),
            (Some("old\n"), Some("new\n"))
        );
        assert_eq!(s["outcome"], "failure");
        assert_eq!(s["executions"][0]["outcome"], "failure");
        assert_eq!(s["executions"][0]["exit_code"], 1);
        assert_eq!(s["executions"][0]["sole_command"], false);
        assert_eq!(s["executions"][0]["implied_by_success"], false);
        assert!(known.is_empty());
    }

    #[test]
    fn a_write_with_no_result_is_recorded_as_unknown() {
        let mut known = Known::new();
        let t = run(&[exec("c1", "cat > a.txt <<EOF\nx\nEOF", None)], &mut known);
        let (m, s) = only(&t);
        assert_eq!(m.after.as_deref(), Some("x\n"));
        assert_eq!(s["outcome"], "unknown");
        assert_eq!(s["executions"][0]["outcome_basis"], "no_result");
        assert!(s["executions"][0].get("exit_code").is_none());
        assert!(known.is_empty());
    }

    #[test]
    fn append_onto_tracked_content_diffs_against_it() {
        let mut known = Known::from([("/w/log.txt".to_string(), Rc::from("a\n"))]);
        let t = run(
            &[exec("c1", "cat >> log.txt <<'EOF'\nb\nEOF", Some(OK))],
            &mut known,
        );
        let (m, s) = only(&t);
        assert_eq!(
            (m.before.as_deref(), m.after.as_deref()),
            (Some("a\n"), Some("a\nb\n"))
        );
        assert_eq!(m.operation.as_deref(), Some("append"));
        assert_eq!(
            m.raw_diff.as_deref(),
            Some(unified_diff("log.txt", "a\n", "a\nb\n").as_str())
        );
        assert_eq!(s["executions"][0]["redirect"], "append");
        assert_eq!(s["executions"][0]["body"], "b\n");
        assert_eq!(s["executions"][0]["append_base"], "tracked");
        assert_eq!(known.get("/w/log.txt").map(|s| &**s), Some("a\nb\n"));
    }

    #[test]
    fn append_onto_unknown_content_is_structural() {
        let mut known = Known::new();
        let t = run(
            &[exec("c1", "tee -a log.txt <<'EOF'\nb\nEOF", Some(OK))],
            &mut known,
        );
        let (m, s) = only(&t);
        assert_eq!(m.operation.as_deref(), Some("append"));
        assert_eq!(
            (
                m.before.as_deref(),
                m.after.as_deref(),
                m.raw_diff.as_deref()
            ),
            (None, None, None)
        );
        assert_eq!(s["executions"][0]["via"], "tee");
        assert_eq!(s["executions"][0]["body"], "b\n");
        assert_eq!(s["executions"][0]["append_base"], "unknown");
        assert!(known.is_empty());
    }

    #[test]
    fn unquoted_tag_is_flagged() {
        let t = run(
            &[exec("c1", "cat <<EOF > env.sh\necho $HOME\nEOF", Some(OK))],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(m.after.as_deref(), Some("echo $HOME\n"));
        assert_eq!(s["executions"][0]["tag_quoted"], false);
        assert_eq!(s["executions"][0]["may_expand"], true);
    }

    #[test]
    fn chained_writes_to_two_files_and_twice_to_one() {
        let cmd =
            "mkdir -p src && cat > src/a.py <<'A' && cat > src/b.py <<'B'\na = 1\nA\nb = 2\nB";
        let t = run(&[exec("c1", cmd, Some(OK))], &mut Known::new());
        let paths: Vec<&str> = t.mutations.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.py", "src/b.py"]);
        assert!(
            t.stamps
                .values()
                .all(|s| s["executions"][0]["sole_command"] == false)
        );

        let cmd = "cat > a <<EOF\n1\nEOF\ncat >> a <<EOF\n2\nEOF";
        let t = run(&[exec("c2", cmd, Some(OK))], &mut Known::new());
        let (m, s) = only(&t);
        assert_eq!(
            (m.before.as_deref(), m.after.as_deref()),
            (None, Some("1\n2\n"))
        );
        assert_eq!(m.operation, None);
        let redirects: Vec<&str> = s["executions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["redirect"].as_str().unwrap())
            .collect();
        assert_eq!(redirects, vec!["write", "append"]);
        assert_eq!(s["executions"][1]["append_base"], "script");
        assert_eq!(s["executions"][0]["body"], "1\n");
    }

    #[test]
    fn non_matching_shell_commands_produce_nothing() {
        let cmds = [
            "ls -la",
            "cat wc.py",
            "echo hi > f.txt",
            "python3 - <<'EOF'\nprint(1)\nEOF",
            "cd /work/project && git add wc.py && git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n)\"",
        ];
        let tools: Vec<ToolInvocation> = cmds
            .iter()
            .enumerate()
            .map(|(i, c)| exec(&format!("c{i}"), c, Some(OK)))
            .collect();
        let t = run(&tools, &mut Known::new());
        assert!(t.mutations.is_empty() && t.stamps.is_empty());
        // Claude Code, opencode and pi spell it `command`.
        for name in ["Bash", "bash"] {
            let t = run(
                &[call(
                    "b",
                    name,
                    json!({"command": "ls"}),
                    Some(("x", false)),
                )],
                &mut Known::new(),
            );
            assert!(t.mutations.is_empty());
        }
    }

    #[test]
    fn gemini_run_shell_command_heredoc_write() {
        let t = run(
            &[call(
                "g",
                "run_shell_command",
                json!({"command": "cat <<'EOF' > out.txt\nhello\nEOF"}),
                Some(("hello\n", false)),
            )],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(m.path, "out.txt");
        assert_eq!(m.after.as_deref(), Some("hello\n"));
        assert_eq!(s["source"], "shell-heredoc");
        assert_eq!(s["outcome"], "success");
        // No exit-code evidence for `run_shell_command`: error flag only.
        assert_eq!(s["executions"][0]["outcome_basis"], "no_error_reported");
        assert!(s["executions"][0].get("exit_code").is_none());
    }

    #[test]
    fn gemini_dir_path_joins_onto_relative_targets() {
        let t = run(
            &[call(
                "g",
                "run_shell_command",
                json!({"command": "cat > f.txt <<'EOF'\nx\nEOF", "dir_path": "sub"}),
                Some(("", false)),
            )],
            &mut Known::new(),
        );
        assert_eq!(only(&t).0.path, "sub/f.txt");
        assert!(t.unresolved.is_empty());
    }

    #[test]
    fn an_unread_directory_key_leaves_relative_targets_unresolved() {
        let script = "cat > f.txt <<'EOF'\nx\nEOF\ncat > /abs/g.txt <<'EOF'\ny\nEOF\napply_patch <<'EOF'\n*** Begin Patch\n*** Add File: h.txt\n+z\n*** End Patch\nEOF";
        for input in [
            json!({"command": script, "working_dir": "sub"}),
            json!({"command": script, "path": "sub"}),
            json!({"command": script, "workdir": "/w/a", "dir_path": "/w/b"}),
        ] {
            let t = run(
                &[call(
                    "g",
                    "run_shell_command",
                    input.clone(),
                    Some(("", false)),
                )],
                &mut Known::new(),
            );
            assert_eq!(only(&t).0.path, "/abs/g.txt", "{input}");
            let reasons: Vec<(&Value, &Value)> = t
                .unresolved
                .iter()
                .map(|u| (&u["path_as_written"], &u["reason"]))
                .collect();
            assert_eq!(
                reasons,
                [
                    (&json!("f.txt"), &json!("unknown_workdir")),
                    (&json!("h.txt"), &json!("unknown_workdir"))
                ],
                "{input}"
            );
        }
        // A key naming the same directory twice is still read.
        let t = run(
            &[call(
                "g",
                "run_shell_command",
                json!({"command": "cat > f <<EOF\nx\nEOF", "workdir": "/w/a", "dir_path": "/w/a/"}),
                Some(("", false)),
            )],
            &mut Known::new(),
        );
        assert_eq!(only(&t).0.path, "/w/a/f");
    }

    #[test]
    fn a_background_execution_is_recorded_with_its_outcome_unknown() {
        let mut known = Known::new();
        let t = run(
            &[call(
                "g",
                "run_shell_command",
                json!({"command": "cat > f.txt <<'EOF'\nx\nEOF", "is_background": true}),
                Some(("Background PIDs: 42", false)),
            )],
            &mut known,
        );
        let (m, s) = only(&t);
        assert_eq!(m.path, "f.txt");
        assert_eq!(s["outcome"], "unknown");
        assert_eq!(s["executions"][0]["outcome"], "unknown");
        assert_eq!(s["executions"][0]["outcome_basis"], "background");
        assert!(known.is_empty());
    }

    #[test]
    fn claude_bash_with_cd_and_error_flag() {
        let t = run(
            &[call(
                "b",
                "Bash",
                json!({"command": "cd /w/sub && cat > f.txt <<'EOF'\nx\nEOF"}),
                Some(("Exit code 1\nsh: sub: No such file or directory", true)),
            )],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(m.path, "/w/sub/f.txt");
        assert_eq!(s["outcome"], "failure");
        assert_eq!(s["executions"][0]["outcome_basis"], "is_error");
        // Claude Code results carry no exit status we read.
        assert!(s["executions"][0].get("exit_code").is_none());
    }

    #[test]
    fn workdir_other_than_the_session_cwd_is_joined() {
        let t = turn_writes(
            &[call(
                "c",
                "exec_command",
                json!({"cmd": "cat > a <<EOF\nx\nEOF", "workdir": "/w/pkg"}),
                Some((OK, false)),
            )],
            Some("/w"),
            &mut ShellState::default(),
        );
        assert_eq!(t.mutations[0].path, "/w/pkg/a");
        let t = turn_writes(
            &[call(
                "c",
                "exec_command",
                json!({"cmd": "cat > a <<EOF\nx\nEOF", "workdir": "/w/"}),
                Some((OK, false)),
            )],
            Some("/w"),
            &mut ShellState::default(),
        );
        assert_eq!(t.mutations[0].path, "a");
    }

    fn multi_edit(id: &str, path: &str) -> ToolInvocation {
        call(
            id,
            "MultiEdit",
            json!({"file_path": path, "edits": [{"old_string": "x", "new_string": "z"}]}),
            Some(("ok", false)),
        )
    }

    #[test]
    fn a_multi_edit_left_to_the_fallback_forgets_the_file() {
        let mut known = Known::from([("/w/a.txt".to_string(), Rc::from("x\n"))]);
        let t = run(&[multi_edit("e1", "a.txt")], &mut known);
        assert!(t.mutations.is_empty(), "{:?}", t.mutations);
        assert_eq!(known.get("/w/a.txt"), None);
    }

    #[test]
    fn a_multi_edit_after_a_shell_write_takes_it_over() {
        let mut known = Known::new();
        let t = run(
            &[
                exec("c1", "cat <<'EOF' > a.txt\nx\nEOF", Some(OK)),
                multi_edit("e1", "a.txt"),
            ],
            &mut known,
        );
        let s = &t.stamps["a.txt"];
        assert_eq!(s.get("source"), None, "{s}");
        assert_eq!(s["executions"].as_array().unwrap().len(), 1);
        assert_eq!(known.get("/w/a.txt"), None);
    }

    #[test]
    fn a_notebook_edit_is_not_whole_file_content() {
        let mut known = Known::new();
        run(
            &[call(
                "n1",
                "NotebookEdit",
                json!({"notebook_path": "nb.ipynb", "cell_id": "c", "new_source": "print(1)"}),
                Some(("ok", false)),
            )],
            &mut known,
        );
        assert_eq!(known.get("/w/nb.ipynb"), None);
    }

    #[test]
    fn a_command_mentioning_a_tracked_file_forgets_it() {
        let mut known = Known::new();
        run(
            &[
                exec("c1", "cat > wc.py <<'EOF'\nprint(1)\nEOF", Some(OK)),
                exec("c2", "cat > keep.txt <<'EOF'\nk\nEOF", Some(OK)),
                exec("c3", "sed -i s/1/2/ wc.py", Some(OK)),
            ],
            &mut known,
        );
        assert_eq!(known.keys().collect::<Vec<_>>(), vec!["/w/keep.txt"]);
        let t = run(
            &[exec("c4", "cat >> wc.py <<'EOF'\nx\nEOF", Some(OK))],
            &mut known,
        );
        assert_eq!(t.stamps["wc.py"]["executions"][0]["append_base"], "unknown");
    }

    #[test]
    fn an_observed_write_seeds_a_later_append_in_the_same_turn() {
        let t = run(
            &[
                call(
                    "w",
                    "Write",
                    json!({"file_path": "/w/n.md", "content": "a\n"}),
                    Some(("File created successfully at: /w/n.md", false)),
                ),
                exec("c", "cat >> /w/n.md <<'EOF'\nb\nEOF", Some(OK)),
            ],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(
            (m.before.as_deref(), m.after.as_deref()),
            (Some("a\n"), Some("a\nb\n"))
        );
        assert_eq!(m.tool_id.as_deref(), Some("c"));
        assert_eq!(s["source"], "shell-heredoc");
    }

    #[test]
    fn a_later_observed_write_takes_the_change_and_keeps_the_executions() {
        let t = run(
            &[
                exec("c", "cat > /w/n.md <<'EOF'\nb\nEOF", Some(OK)),
                call(
                    "w",
                    "Write",
                    json!({"file_path": "/w/n.md", "content": "final\n"}),
                    Some(("ok", false)),
                ),
            ],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(
            (m.tool_id.as_deref(), m.after.as_deref()),
            (Some("w"), Some("final\n"))
        );
        assert!(s.get("source").is_none());
        assert_eq!(s["executions"][0]["tool_id"], "c");
    }

    #[test]
    fn a_write_then_an_append_in_one_call_keep_the_intended_content_whatever_the_outcome() {
        let cmd = "cat > f <<'A'\nx\nA\ncat >> f <<'B'\ny\nB\npytest";
        for result in [Some(FAIL), None] {
            let mut known = Known::new();
            let t = run(&[exec("c1", cmd, result)], &mut known);
            let (m, s) = only(&t);
            assert_eq!(
                (m.before.as_deref(), m.after.as_deref()),
                (None, Some("x\ny\n"))
            );
            assert_eq!(
                m.raw_diff.as_deref(),
                Some(unified_diff("f", "", "x\ny\n").as_str())
            );
            assert_eq!(m.operation, None);
            let bodies: Vec<&str> = s["executions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["body"].as_str().unwrap())
                .collect();
            assert_eq!(bodies, ["x\n", "y\n"]);
            assert_eq!(s["executions"][1]["append_base"], "script");
            assert_ne!(s["outcome"], "success");
            assert!(known.is_empty());
        }
    }

    #[test]
    fn each_write_execution_records_its_body() {
        let t = run(
            &[
                exec("c1", "cat > f <<EOF\nx\nEOF", Some(OK)),
                exec("c2", "cat > f <<EOF\ny\nEOF\nfalse", Some(FAIL)),
            ],
            &mut Known::new(),
        );
        let (m, s) = only(&t);
        assert_eq!(m.after.as_deref(), Some("y\n"));
        assert_eq!(s["outcome"], "failure");
        let e = s["executions"].as_array().unwrap();
        assert_eq!(
            (e[0]["body"].as_str(), e[0]["outcome"].as_str()),
            (Some("x\n"), Some("success"))
        );
        assert_eq!(
            (e[1]["body"].as_str(), e[1]["outcome"].as_str()),
            (Some("y\n"), Some("failure"))
        );
    }

    #[test]
    fn a_write_whose_status_the_call_does_not_carry_is_not_known() {
        for cmd in [
            "cat > f <<EOF || true\nx\nEOF",
            "cat > f <<EOF &\nx\nEOF",
            "cat > f <<EOF\nx\nEOF\nls",
        ] {
            let mut known = Known::new();
            let t = run(&[exec("c1", cmd, Some(OK))], &mut known);
            let (_, s) = only(&t);
            let e = &s["executions"][0];
            assert_eq!(
                (
                    e["sole_command"].as_bool(),
                    e["implied_by_success"].as_bool()
                ),
                (Some(false), Some(false)),
                "{cmd:?}"
            );
            assert_eq!(s["outcome"], "success");
            assert!(known.is_empty(), "{cmd:?}");
        }
        let mut known = Known::new();
        run(
            &[exec(
                "c1",
                "mkdir -p d && cat > d/f <<EOF\nx\nEOF",
                Some(OK),
            )],
            &mut known,
        );
        assert_eq!(known.get("/w/d/f").map(|s| &**s), Some("x\n"));
    }

    fn bash(id: &str, cmd: &str) -> ToolInvocation {
        call(id, "Bash", json!({"command": cmd}), Some(("", false)))
    }

    fn failed_bash(id: &str, cmd: &str) -> ToolInvocation {
        call(
            id,
            "Bash",
            json!({"command": cmd}),
            Some(("no such dir", true)),
        )
    }

    fn at(d: &str) -> ScriptDir {
        ScriptDir::At(d.to_string())
    }

    fn write_to(id: &str, target: &str) -> ToolInvocation {
        bash(id, &format!("cat > {target} <<'EOF'\nx\nEOF"))
    }

    fn changed(t: &TurnWrites) -> Vec<&str> {
        t.mutations.iter().map(|m| m.path.as_str()).collect()
    }

    fn likely_path(t: &TurnWrites) -> Value {
        t.unresolved[0]
            .get("likely_path")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// `b2` writing `f` (relative): no change, one attempt; its `likely_path`.
    fn moved_write(state: &mut ShellState) -> Value {
        let t = run_state(&[write_to("b2", "f")], state);
        assert!(t.mutations.is_empty(), "{:?}", t.mutations);
        assert_eq!(t.unresolved.len(), 1);
        assert_eq!(t.unresolved[0]["reason"], "shell_dir_moved");
        likely_path(&t)
    }

    fn bash_with(id: &str, cmd: &str, content: &str) -> ToolInvocation {
        call(id, "Bash", json!({"command": cmd}), Some((content, false)))
    }

    #[test]
    fn a_bash_cd_in_its_own_call_leaves_later_relative_targets_unresolved_with_a_likely_path() {
        let mut state = ShellState::default();
        let t = run_state(&[bash("b1", "cd sub")], &mut state);
        assert!(t.mutations.is_empty() && t.unresolved.is_empty());
        assert!(t.shell_dir_touched);
        assert_eq!(state.dir, at("/w/sub"));
        for (id, file) in [("b2", "f.txt"), ("b3", "g.txt")] {
            let t = run_state(&[write_to(id, file)], &mut state);
            assert!(t.mutations.is_empty(), "{:?}", t.mutations);
            let a = &t.unresolved[0];
            assert_eq!(a["reason"], "shell_dir_moved");
            assert_eq!(a["path_as_written"], file);
            assert_eq!(a["likely_path"], format!("sub/{file}"));
            assert_eq!(a["outcome"], "success");
            assert_eq!(a["body"], "x\n");
        }
        assert!(state.known.is_empty());
        assert_eq!(state.dir, at("/w/sub"));
        let t = run_state(&[write_to("b4", "/w/abs.rs")], &mut state);
        assert_eq!(changed(&t), ["/w/abs.rs"]);
        assert!(
            t.stamps["/w/abs.rs"]["executions"][0]
                .get("dir_basis")
                .is_none()
        );
    }

    #[test]
    fn a_cd_in_the_same_call_resolves_and_a_later_call_gets_a_likely_path() {
        let mut state = ShellState::default();
        let t = run_state(
            &[bash("b1", "cd sub && cat > f <<'EOF'\nx\nEOF")],
            &mut state,
        );
        assert_eq!(changed(&t), ["sub/f"]);
        assert_eq!(state.dir, at("/w/sub"));
        let t = run_state(
            &[bash("b2", "cd deep && cat > g <<'EOF'\ny\nEOF")],
            &mut state,
        );
        assert!(t.mutations.is_empty());
        assert_eq!(likely_path(&t), "sub/deep/g");
        assert_eq!(state.dir, at("/w/sub/deep"));
        let t = run_state(
            &[bash("b3", "cd /w/x && cat > h <<'EOF'\nz\nEOF")],
            &mut state,
        );
        assert_eq!(changed(&t), ["/w/x/h"], "an absolute cd in the call");
    }

    #[test]
    fn a_cd_that_may_not_have_run_leaves_later_relative_targets_unresolved() {
        for (why, first) in [
            ("failed", failed_bash("b1", "cd sub")),
            (
                "unanswered",
                call("b1", "Bash", json!({"command": "cd sub"}), None),
            ),
            ("seq", bash("b1", "cd sub; x")),
            ("or", bash("b1", "false || cd sub")),
            ("pipe", bash("b1", "cd sub | cat")),
            ("no arg", bash("b1", "cd")),
            ("dash", bash("b1", "cd -")),
            ("zsh stack", bash("b1", "cd +1")),
            ("zsh stack from the bottom", bash("b1", "cd -0")),
            ("pushd", bash("b1", "pushd sub")),
            ("not literal", bash("b1", "cd $D")),
            ("subshell escape", bash("b1", "cd sub && echo $(pwd)")),
            ("leaves the project", bash("b1", "cd ../..")),
            // Claude Code reports exit 1 of these as no error, and the
            // shell keeps its directory only on exit 0.
            ("grep", bash("b1", "cd sub && grep -rn foo .")),
            ("rg", bash("b1", "cd sub && rg foo")),
            ("egrep", bash("b1", "cd sub && egrep foo x")),
            ("fgrep", bash("b1", "cd sub && fgrep foo x")),
            ("find", bash("b1", "cd sub && find . -name x")),
            ("diff", bash("b1", "cd sub && diff a b")),
            ("test", bash("b1", "cd sub && test -f x")),
            ("bracket", bash("b1", "cd sub && [ -f x ]")),
            ("git grep", bash("b1", "cd sub && git -C . grep foo")),
            ("git diff", bash("b1", "cd sub && git -c a=b diff --stat")),
            ("earlier grep", bash("b1", "grep -q x f && cd sub")),
            (
                "background",
                call(
                    "b1",
                    "Bash",
                    json!({"command": "cd sub", "run_in_background": true}),
                    Some(("", false)),
                ),
            ),
            (
                "reset elsewhere",
                bash_with("b1", "cd sub", "Shell cwd was reset to /elsewhere"),
            ),
            (
                "unreadable",
                call("b1", "Bash", json!("{\"command\": \"cd sub"), None),
            ),
        ] {
            let mut state = ShellState::default();
            run_state(&[first], &mut state);
            assert_eq!(state.dir, ScriptDir::Unknown, "{why}");
            let t = run_state(&[write_to("b2", "f.txt")], &mut state);
            assert!(t.mutations.is_empty(), "{why}: {:?}", t.mutations);
            assert_eq!(t.unresolved.len(), 1, "{why}");
            assert_eq!(t.unresolved[0]["reason"], "shell_dir_moved", "{why}");
            assert_eq!(t.unresolved[0]["path_as_written"], "f.txt", "{why}");
            assert_eq!(t.unresolved[0]["outcome"], "success", "{why}");
            assert_eq!(likely_path(&t), Value::Null, "{why}");
        }
    }

    #[test]
    fn lenient_commands_are_matched_by_command_word() {
        for (cmd, want) in [
            ("cd sub && FOO=1 grep x f", ScriptDir::Unknown),
            ("cd sub && \"test\" -f x", ScriptDir::Unknown),
            ("cd sub && echo grep test", at("/w/sub")),
            ("cd sub && git log --grep x", at("/w/sub")),
        ] {
            let mut state = ShellState::default();
            run_state(&[bash("b1", cmd)], &mut state);
            assert_eq!(state.dir, want, "{cmd:?}");
        }
    }

    #[test]
    fn dot_dot_above_a_tracked_directory_is_unknown() {
        // `pwd -P`: `..` from a directory reached through a symlink leaves
        // the link's target.
        let mut state = ShellState::default();
        run_state(&[bash("b1", "cd sub/deep")], &mut state);
        run_state(&[bash("b2", "cd x/../y")], &mut state);
        assert_eq!(state.dir, at("/w/sub/deep/y"));
        let t = run_state(&[bash("b3", "cat > ../f <<'EOF'\nx\nEOF")], &mut state);
        assert_eq!(likely_path(&t), Value::Null, "no hint above it either");
        run_state(&[bash("b4", "cd ..")], &mut state);
        assert_eq!(state.dir, ScriptDir::Unknown);
        assert_eq!(moved_write(&mut state), Value::Null);
        // Within one call `cd` is logical.
        let mut state = ShellState::default();
        run_state(&[bash("b1", "cd sub/deep && cd ../..")], &mut state);
        assert_eq!(state.dir, ScriptDir::Start);
        let t = run_state(&[write_to("b5", "f")], &mut state);
        assert_eq!(changed(&t), ["f"]);
    }

    #[test]
    fn an_absolute_cd_under_the_working_directory_recovers_an_unknown_directory() {
        let mut state = ShellState::default();
        run_state(&[bash("b1", "cd sub; ls")], &mut state);
        assert_eq!(state.dir, ScriptDir::Unknown);
        run_state(&[bash("b2", "cd rel")], &mut state);
        assert_eq!(state.dir, ScriptDir::Unknown, "a relative cd does not");
        run_state(&[failed_bash("b3", "cd /w/pkg")], &mut state);
        assert_eq!(state.dir, ScriptDir::Unknown, "a failed cd does not");
        run_state(&[bash("b4", "cd /w && grep -q x f")], &mut state);
        assert_eq!(state.dir, ScriptDir::Unknown, "nor one grep may undo");
        run_state(&[bash("b5", "cd /elsewhere")], &mut state);
        assert_eq!(
            state.dir,
            ScriptDir::Unknown,
            "outside the project it may be reset"
        );
        run_state(&[bash("b6", "cd /w/pkg")], &mut state);
        assert_eq!(state.dir, at("/w/pkg"));
        assert_eq!(moved_write(&mut state), "pkg/f");
        run_state(&[bash("b7", "cd /w")], &mut state);
        assert_eq!(state.dir, ScriptDir::Start);
        let t = run_state(&[write_to("b8", "f")], &mut state);
        assert_eq!(changed(&t), ["f"]);
    }

    #[test]
    fn a_reset_note_on_any_bash_call_puts_the_shell_where_it_says() {
        for (content, want) in [
            ("out\nShell cwd was reset to /w", ScriptDir::Start),
            ("Shell cwd was reset to /w/\n", ScriptDir::Start),
            ("Shell cwd was reset to /other", ScriptDir::Unknown),
            (
                "Shell cwd was reset to /w\nquoted, not the note",
                ScriptDir::Unknown,
            ),
        ] {
            let mut state = ShellState::default();
            run_state(&[bash("b1", "cd sub")], &mut state);
            let t = run_state(&[bash_with("b2", "ls", content)], &mut state);
            assert_eq!(state.dir, want, "{content:?}");
            assert!(t.shell_dir_touched, "{content:?}");
        }
        let mut state = ShellState::default();
        run_state(
            &[bash_with("b1", "cd sub", "Shell cwd was reset to /w")],
            &mut state,
        );
        assert_eq!(state.dir, ScriptDir::Start);
        let t = run_state(&[write_to("b2", "f")], &mut state);
        assert_eq!(changed(&t), ["f"]);
        let mut state = ShellState::default();
        let t = run_state(
            &[bash_with("b1", "ls", "Shell cwd was reset to /other")],
            &mut state,
        );
        assert_eq!(state.dir, ScriptDir::Unknown);
        assert!(t.shell_dir_touched);
    }

    #[test]
    fn entering_or_leaving_a_worktree_moves_the_shell_and_where_it_returns() {
        let tool =
            |id: &str, name: &str, result: Option<(&str, bool)>| call(id, name, json!({}), result);
        for (name, result) in [
            (
                "EnterWorktree",
                Some(("Created worktree at /w/.wt/a", false)),
            ),
            ("EnterWorktree", None),
            ("ExitWorktree", Some(("Exited worktree", false))),
        ] {
            let mut state = ShellState::default();
            let t = run_state(&[tool("t1", name, result)], &mut state);
            assert!(t.shell_dir_touched, "{name}");
            assert_eq!(state.dir, ScriptDir::Unknown, "{name}");
            assert_eq!(moved_write(&mut state), Value::Null, "{name}");
            run_state(&[bash("b3", "cd /w")], &mut state);
            assert_eq!(state.dir, ScriptDir::Unknown, "{name}: /w may not be home");
            run_state(&[bash("b4", "cd /w/sub")], &mut state);
            assert_eq!(moved_write(&mut state), "sub/f", "{name}");
            run_state(
                &[bash_with("b5", "ls", "Shell cwd was reset to /w")],
                &mut state,
            );
            assert_eq!(state.dir, ScriptDir::Start, "{name}");
            run_state(&[bash("b6", "cd sub && cd ..")], &mut state);
            assert_eq!(state.dir, ScriptDir::Start, "{name}");
        }
        let mut state = ShellState::default();
        let t = run_state(
            &[tool("t1", "EnterWorktree", Some(("not a git repo", true)))],
            &mut state,
        );
        assert!(!t.shell_dir_touched);
        assert_eq!(state.dir, ScriptDir::Start);
    }

    #[test]
    fn a_lenient_command_s_success_does_not_make_a_write_s_content_known() {
        let mut state = ShellState::default();
        let t = run_state(
            &[bash("b1", "grep -q x f && cat > g <<'EOF'\nx\nEOF")],
            &mut state,
        );
        assert_eq!(changed(&t), ["g"]);
        let e = &t.stamps["g"]["executions"][0];
        assert_eq!(e["outcome"], "success");
        assert_eq!(e["success_may_hide_exit_1"], true);
        assert!(state.known.is_empty());
        let t = run_state(&[bash("b2", "true && cat > g <<'EOF'\nx\nEOF")], &mut state);
        assert!(
            t.stamps["g"]["executions"][0]
                .get("success_may_hide_exit_1")
                .is_none()
        );
        assert_eq!(state.known.get("/w/g").map(|s| &**s), Some("x\n"));
        // An exit status is not lenient.
        let mut known = Known::new();
        run(
            &[exec(
                "c1",
                "grep -q x f && cat > g <<'EOF'\nx\nEOF",
                Some(OK),
            )],
            &mut known,
        );
        assert_eq!(known.get("/w/g").map(|s| &**s), Some("x\n"));
    }

    #[test]
    fn two_directory_changes_in_one_turn_leave_the_directory_unknown() {
        // Calls of one turn may run concurrently.
        let mut state = ShellState::default();
        let t = run_state(
            &[bash("b1", "cd a"), bash("b2", "cd b"), write_to("b3", "f")],
            &mut state,
        );
        assert_eq!(state.dir, ScriptDir::Unknown);
        assert!(t.mutations.is_empty());
        assert_eq!(t.unresolved[0]["reason"], "shell_dir_moved");
    }

    #[test]
    fn a_shell_apply_patch_after_a_tracked_cd_gets_a_likely_path() {
        let mut state = ShellState::default();
        run_state(&[bash("b1", "cd sub")], &mut state);
        let patch = "*** Begin Patch\n*** Add File: a.py\n+x\n*** End Patch\n";
        let t = run_state(
            &[bash("b2", &format!("apply_patch <<'EOF'\n{patch}EOF"))],
            &mut state,
        );
        assert!(t.mutations.is_empty());
        assert_eq!(t.unresolved[0]["path_as_written"], "a.py");
        assert_eq!(t.unresolved[0]["operation"], "add");
        assert_eq!(likely_path(&t), "sub/a.py");
    }

    #[test]
    fn only_a_persistent_shell_carries_a_cd() {
        let mut state = ShellState::default();
        let t = run_state(
            &[bash("b1", "ls"), bash("b2", "cat > a <<EOF\nx\nEOF")],
            &mut state,
        );
        assert_eq!(t.mutations[0].path, "a");
        assert_eq!(state.dir, ScriptDir::Start);
        for (name, input) in [
            ("exec_command", json!({"cmd": "cd sub", "workdir": "/w"})),
            ("bash", json!({"command": "cd sub"})),
        ] {
            let mut state = ShellState::default();
            let t = run_state(&[call("c1", name, input, Some(("", false)))], &mut state);
            assert_eq!(state.dir, ScriptDir::Start, "{name}");
            assert!(!t.shell_dir_touched, "{name}");
        }
        for cmd in [
            "(cd sub && make)",
            "echo \"$(cd sub && pwd)\"",
            "cd /w && git add a && git commit -m \"$(cat <<'EOF'\nFix (cd into dirs)\nEOF\n)\"",
            "cd /w/./x/.. ; ls",
        ] {
            let mut state = ShellState::default();
            let t = run_state(&[bash("b", cmd)], &mut state);
            assert_eq!(state.dir, ScriptDir::Start, "{cmd:?}");
            assert_eq!(t.shell_dir_touched, cmd.starts_with("cd"), "{cmd:?}");
        }
        for (cmd, want) in [
            ("cd /w/sub && ls", at("/w/sub")),
            ("cd /w; cd /x", ScriptDir::Unknown),
            ("$CD sub", ScriptDir::Unknown),
            ("f() { cd sub; }", ScriptDir::Unknown),
        ] {
            let mut state = ShellState::default();
            let t = run_state(&[bash("b", cmd)], &mut state);
            assert_eq!(state.dir, want, "{cmd:?}");
            assert!(t.shell_dir_touched, "{cmd:?}");
        }
    }

    #[test]
    fn unresolvable_targets_are_recorded_as_attempts_never_as_changes() {
        let mut known = Known::from([
            ("/w/a.txt".to_string(), Rc::from("a\n")),
            ("/w/sub/f".to_string(), Rc::from("f\n")),
        ]);
        let t = run(
            &[
                exec("c1", "cd $X && cat > f <<EOF\nx\nEOF", Some(FAIL)),
                exec("c2", "tee -a {a,b}.txt <<'EOF'\ny\nEOF", None),
            ],
            &mut known,
        );
        assert!(t.mutations.is_empty() && t.stamps.is_empty());
        assert_eq!(
            t.unresolved,
            vec![
                json!({
                    "tool_id": "c1", "tool": "exec_command", "path_as_written": "f",
                    "reason": "unknown_dir", "via": "cat", "redirect": "write",
                    "outcome": "failure", "outcome_basis": "exit_code", "exit_code": 1,
                    "sole_command": false, "implied_by_success": true, "body": "x\n"
                }),
                json!({
                    "tool_id": "c2", "tool": "exec_command", "path_as_written": "{a,b}.txt",
                    "reason": "not_literal", "via": "tee", "redirect": "append",
                    "outcome": "unknown", "outcome_basis": "no_result",
                    "sole_command": true, "implied_by_success": true, "body": "y\n"
                }),
            ]
        );
        assert!(known.is_empty(), "{known:?}");
    }

    #[test]
    fn a_codex_shell_argv_that_is_not_a_shell_records_nothing() {
        let script = "cat > f <<EOF\nx\nEOF";
        let shell = |argv: Value| {
            call(
                "s",
                "shell",
                json!({"command": argv, "workdir": "/w"}),
                Some(("{}", false)),
            )
        };
        let mut known = Known::from([("/w/f".to_string(), Rc::from("old\n"))]);
        let t = run(&[shell(json!(["python3", "-c", script]))], &mut known);
        assert!(t.mutations.is_empty() && t.unresolved.is_empty());
        assert!(known.is_empty(), "the program text names f");
        let t = run(&[shell(json!(["bash", "-lc", script]))], &mut Known::new());
        assert_eq!(t.mutations[0].path, "f");
    }

    #[test]
    fn a_turn_without_shell_writes_is_exactly_file_mutations() {
        let tools = vec![
            call(
                "w",
                "Write",
                json!({"file_path": "a.rs", "content": "x"}),
                None,
            ),
            call(
                "e",
                "Edit",
                json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}),
                None,
            ),
            call(
                "b",
                "Bash",
                json!({"command": "cargo test"}),
                Some(("ok", false)),
            ),
        ];
        let t = run(&tools, &mut Known::new());
        let want: Vec<FileMutation> = tools.iter().flat_map(file_mutations).collect();
        assert_eq!(
            serde_json::to_value(&t.mutations).unwrap(),
            serde_json::to_value(&want).unwrap()
        );
        assert!(t.stamps.is_empty());
    }

    #[test]
    fn unmodeled_shell_writes_are_recorded_as_unresolved_attempts() {
        let mut known = Known::from([
            ("/w/f.txt".to_string(), Rc::from("old\n")),
            ("/w/keep".to_string(), Rc::from("k\n")),
        ]);
        let t = run(
            &[
                exec("two", "cat > f.txt <<A <<B\none\nA\ntwo\nB", Some(OK)),
                exec(
                    "tee",
                    "cat <<'EOF' | tee -a /w/log.md\nhello\nEOF",
                    Some(FAIL),
                ),
            ],
            &mut known,
        );
        assert!(t.mutations.is_empty() && t.stamps.is_empty());
        assert_eq!(
            t.unresolved,
            vec![
                json!({
                    "tool_id": "two", "tool": "exec_command", "path_as_written": "f.txt",
                    "path": "f.txt", "reason": "unmodeled", "via": "redirect",
                    "command": "cat", "redirect": "write",
                    "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
                    "sole_command": true, "implied_by_success": true
                }),
                json!({
                    "tool_id": "tee", "tool": "exec_command", "path_as_written": "/w/log.md",
                    "path": "/w/log.md", "reason": "unmodeled", "via": "tee",
                    "command": "tee", "redirect": "append",
                    "outcome": "failure", "outcome_basis": "exit_code", "exit_code": 1,
                    "sole_command": false, "implied_by_success": false
                }),
            ]
        );
        assert!(!known.contains_key("/w/f.txt"));
        assert!(known.contains_key("/w/keep"));

        let t = run(&[exec("n", "make > \"$LOG\"", Some(OK))], &mut known);
        assert_eq!(t.unresolved[0]["path_as_written"], "$LOG");
        assert!(t.unresolved[0].get("path").is_none());
        assert!(known.is_empty());
    }

    #[test]
    fn an_unmodeled_relative_write_after_a_cd_records_a_likely_path_not_a_path() {
        let mut state = ShellState::default();
        run_state(&[bash("b1", "cd sub")], &mut state);
        assert_eq!(state.dir, at("/w/sub"));
        let t = run_state(
            &[bash("b2", "cat <<'EOF' | tee f.txt /w/abs.md\nx\nEOF")],
            &mut state,
        );
        assert!(t.mutations.is_empty(), "{:?}", t.mutations);
        assert_eq!(t.unresolved.len(), 2, "{:?}", t.unresolved);
        let rel = &t.unresolved[0];
        assert_eq!(rel["reason"], "unmodeled");
        assert_eq!(rel["path_as_written"], "f.txt");
        assert!(rel.get("path").is_none(), "{rel}");
        assert_eq!(rel["likely_path"], "sub/f.txt");
        let abs = &t.unresolved[1];
        assert_eq!(abs["path"], "/w/abs.md");
        assert!(abs.get("likely_path").is_none(), "{abs}");
    }
}

/// `apply_patch <<EOF` run through a shell call (Codex).
#[cfg(test)]
mod patch_tests {
    use super::*;

    fn tool_category(name: &str) -> Option<ToolCategory> {
        crate::tests::classifier::provider_tool_category("unknown", name)
    }
    use toolpath_convo::ToolResult;

    const OK: &str = "Chunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\nSuccess. Updated the following files:\nA hello.txt\n";
    const FAIL: &str = "Chunk ID: 2\nWall time: 0.0 seconds\nProcess exited with code 1\nOriginal token count: 4\nOutput:\nFailed to find context\n";
    const ADD: &str = "*** Begin Patch\n*** Add File: hello.txt\n+hi\n*** End Patch\n";

    fn exec(id: &str, cmd: &str, workdir: &str, result: Option<&str>) -> ToolInvocation {
        ToolInvocation {
            id: id.into(),
            name: "exec_command".into(),
            input: json!({"cmd": cmd, "workdir": workdir}),
            result: result.map(|r| ToolResult {
                content: r.into(),
                is_error: false,
            }),
            category: tool_category("exec_command"),
        }
    }

    fn heredoc(patch: &str) -> String {
        format!("apply_patch <<'EOF'\n{patch}EOF\n")
    }

    fn run(tools: &[ToolInvocation], known: &mut Known) -> TurnWrites {
        let mut state = ShellState {
            known: std::mem::take(known),
            ..Default::default()
        };
        let t = turn_writes(tools, Some("/w"), &mut state);
        *known = state.known;
        t
    }

    #[test]
    fn a_successful_shell_apply_patch_is_the_observed_patch_marked_as_inferred() {
        let mut known = Known::new();
        let t = run(&[exec("c1", &heredoc(ADD), "/w", Some(OK))], &mut known);
        assert_eq!(t.mutations.len(), 1);
        let m = &t.mutations[0];
        // What the same patch sent to the `apply_patch` tool records.
        let mut observed = patch_mutations(ADD).remove(0);
        observed.tool_id = Some("c1".into());
        assert_eq!(
            serde_json::to_value(m).unwrap(),
            serde_json::to_value(&observed).unwrap()
        );
        assert_eq!(
            t.stamps["hello.txt"],
            json!({
                "source": "shell-apply-patch",
                "outcome": "success",
                "executions": [{
                    "tool_id": "c1", "tool": "exec_command", "via": "apply_patch",
                    "operation": "add",
                    "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
                    "sole_command": true, "implied_by_success": true,
                    "tag": "EOF", "tag_quoted": true
                }]
            })
        );
        assert_eq!(known.get("/w/hello.txt").map(|s| &**s), Some("hi\n"));
    }

    #[test]
    fn failed_and_unanswered_patches_are_recorded_and_forget() {
        let patch = "*** Begin Patch\n*** Update File: a.py\n@@\n-x\n+y\n*** Delete File: b.py\n*** End Patch\n";
        for (result, outcome, basis) in [
            (Some(FAIL), "failure", "exit_code"),
            (None, "unknown", "no_result"),
        ] {
            let mut known = Known::from([
                ("/w/a.py".to_string(), Rc::from("x\n")),
                ("/w/b.py".to_string(), Rc::from("b\n")),
                ("/w/hello.txt".to_string(), Rc::from("h\n")),
            ]);
            let t = run(&[exec("c", &heredoc(patch), "/w", result)], &mut known);
            let ops: Vec<(&str, Option<&str>)> = t
                .mutations
                .iter()
                .map(|m| (m.path.as_str(), m.operation.as_deref()))
                .collect();
            assert_eq!(
                ops,
                vec![("a.py", Some("update")), ("b.py", Some("delete"))]
            );
            for file in ["a.py", "b.py"] {
                let s = &t.stamps[file];
                assert_eq!(s["source"], "shell-apply-patch");
                assert_eq!(s["outcome"], outcome, "{file}");
                assert_eq!(s["executions"][0]["outcome_basis"], basis);
            }
            assert_eq!(known.keys().collect::<Vec<_>>(), vec!["/w/hello.txt"]);
        }
    }

    #[test]
    fn patch_paths_follow_cd_and_a_differing_workdir() {
        let cmd = format!("cd sub && {}", heredoc(ADD));
        let t = run(&[exec("c", &cmd, "/w", Some(OK))], &mut Known::new());
        assert_eq!(t.mutations[0].path, "sub/hello.txt");
        assert_eq!(
            t.stamps["sub/hello.txt"]["executions"][0]["sole_command"],
            false
        );
        let t = run(
            &[exec("c", &heredoc(ADD), "/w/pkg", Some(OK))],
            &mut Known::new(),
        );
        assert_eq!(t.mutations[0].path, "/w/pkg/hello.txt");
        let moved = "*** Begin Patch\n*** Update File: a.py\n*** Move to: b.py\n@@\n-x\n+y\n*** End Patch\n";
        let t = run(
            &[exec(
                "c",
                &format!("cd sub && {}", heredoc(moved)),
                "/w",
                Some(OK),
            )],
            &mut Known::new(),
        );
        assert_eq!(
            (
                t.mutations[0].path.as_str(),
                t.mutations[0].rename_to.as_deref()
            ),
            ("sub/a.py", Some("sub/b.py"))
        );
    }

    #[test]
    fn an_added_file_seeds_a_later_heredoc_append() {
        let mut known = Known::new();
        run(&[exec("c1", &heredoc(ADD), "/w", Some(OK))], &mut known);
        let t = run(
            &[exec(
                "c2",
                "cat >> hello.txt <<'EOF'\nthere\nEOF",
                "/w",
                Some(OK),
            )],
            &mut known,
        );
        let m = &t.mutations[0];
        assert_eq!(
            (m.before.as_deref(), m.after.as_deref()),
            (Some("hi\n"), Some("hi\nthere\n"))
        );
        assert_eq!(
            t.stamps["hello.txt"]["executions"][0]["append_base"],
            "tracked"
        );
    }

    #[test]
    fn a_patch_without_file_markers_records_nothing_and_forgets_what_it_names() {
        let mut known = Known::from([("/w/notes.md".to_string(), Rc::from("n\n"))]);
        let t = run(
            &[exec(
                "c",
                &heredoc("rewrite notes.md somehow\n"),
                "/w",
                Some(OK),
            )],
            &mut known,
        );
        assert!(t.mutations.is_empty() && t.stamps.is_empty());
        assert!(known.is_empty());
    }

    #[test]
    fn a_patch_in_an_unknown_directory_resolves_only_absolute_paths() {
        let patch = "*** Begin Patch\n*** Add File: rel.txt\n+r\n*** Add File: /w/abs.txt\n+a\n*** End Patch\n";
        let cmd = format!("cd sub; {}", heredoc(patch));
        let t = run(&[exec("c", &cmd, "/w", Some(OK))], &mut Known::new());
        let paths: Vec<&str> = t.mutations.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["/w/abs.txt"]);
        assert_eq!(t.unresolved.len(), 1);
        let u = &t.unresolved[0];
        assert_eq!(
            (
                u["path_as_written"].as_str(),
                u["reason"].as_str(),
                u["operation"].as_str(),
                u["via"].as_str()
            ),
            (
                Some("rel.txt"),
                Some("unknown_dir"),
                Some("add"),
                Some("apply_patch")
            )
        );
    }

    #[test]
    fn an_argv_apply_patch_is_a_shell_patch_without_a_tag() {
        let t = run(
            &[ToolInvocation {
                id: "s".into(),
                name: "shell".into(),
                input: json!({"command": ["apply_patch", ADD], "workdir": "/w"}),
                result: None,
                category: tool_category("shell"),
            }],
            &mut Known::new(),
        );
        let e = &t.stamps["hello.txt"]["executions"][0];
        assert_eq!(e["via"], "apply_patch");
        assert!(e.get("tag").is_none() && e.get("tag_quoted").is_none());
    }

    #[test]
    fn a_heredoc_write_and_a_patch_of_one_file_in_one_call_fold_into_one_change() {
        let update = "*** Begin Patch\n*** Update File: hello.txt\n@@\n-hi\n+ho\n*** End Patch\n";
        let cmd = format!("cat > hello.txt <<'A'\nhi\nA\n{}", heredoc(update));
        let t = run(&[exec("c", &cmd, "/w", Some(OK))], &mut Known::new());
        assert_eq!(t.mutations.len(), 1);
        assert_eq!(t.mutations[0].operation.as_deref(), Some("update"));
        let s = &t.stamps["hello.txt"];
        assert_eq!(s["source"], "shell-apply-patch");
        let via: Vec<&str> = s["executions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["via"].as_str().unwrap())
            .collect();
        assert_eq!(via, vec!["cat", "apply_patch"]);
    }
}

/// End to end: generations → `derive_path`, as `p import otel` runs it.
#[cfg(test)]
mod derive_tests {
    use crate::derive::canonical_step_json;
    use crate::generation::Generation;
    use crate::session::Session;
    use crate::tests::otel::derive_path;
    use serde_json::{Value, json};
    use toolpath::v1::{Path, Step};
    use toolpath_convo::DeriveConfig;

    const OK: &str = "Chunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\n";

    fn system() -> Value {
        json!({"role": "developer", "content": "<environment_context><cwd>/w</cwd></environment_context>"})
    }

    fn user() -> Value {
        json!({"role": "user", "content": "write it"})
    }

    fn calls(id: &str, cmd: &str) -> Value {
        let args = json!({"cmd": cmd, "workdir": "/w"}).to_string();
        json!([{"id": id, "type": "function", "function": {"name": "exec_command", "arguments": args}}])
    }

    fn echo(id: &str, cmd: &str) -> Value {
        json!({"role": "assistant", "content": "", "tool_calls": calls(id, cmd)})
    }

    fn answer(id: &str, content: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": content})
    }

    fn generation(
        id: &str,
        start: u64,
        messages: Vec<Value>,
        call: Option<(&str, &str)>,
    ) -> Generation {
        let mut g = Generation {
            id: id.into(),
            trace_id: format!("trace-{id}"),
            start_ns: start,
            end_ns: start + 1,
            session_id: Some("s".into()),
            messages: serde_json::from_value(Value::Array(messages)).unwrap(),
            ..Default::default()
        };
        match call {
            Some((cid, cmd)) => {
                g.completion.tool_calls = serde_json::from_value(calls(cid, cmd)).unwrap()
            }
            None => g.completion.text = "done".into(),
        }
        g
    }

    fn derive(gens: Vec<Generation>) -> Path {
        derive_path(
            &Session::new("s".into(), Some("s".into()), gens),
            &DeriveConfig::default(),
        )
    }

    /// The one step whose change to `file` came from call `tool_id`.
    fn step_by_tool<'p>(p: &'p Path, file: &str, tool_id: &str) -> &'p Step {
        let hits: Vec<&Step> = p
            .steps
            .iter()
            .filter(|s| {
                s.change
                    .get(file)
                    .and_then(|c| c.structural.as_ref())
                    .is_some_and(|sc| sc.extra.get("tool_id") == Some(&json!(tool_id)))
            })
            .collect();
        assert_eq!(hits.len(), 1, "{file} {tool_id}");
        hits[0]
    }

    fn otel(step: &Step, file: &str) -> Value {
        let sc = step.change[file].structural.as_ref().unwrap();
        assert_eq!(sc.change_type, "file.write");
        sc.extra["otel"].clone()
    }

    const WRITE: &str = "cat <<'EOF' > wc.py\nprint(1)\nEOF";
    const APPEND: &str = "cat >> wc.py <<'EOF'\nprint(2)\nEOF";

    #[test]
    fn a_codex_heredoc_becomes_an_inferred_file_change() {
        let p = derive(vec![
            generation("g1", 1, vec![system(), user()], Some(("c1", WRITE))),
            generation(
                "g2",
                2,
                vec![system(), user(), echo("c1", WRITE), answer("c1", OK)],
                None,
            ),
        ]);
        let step = step_by_tool(&p, "wc.py", "c1");
        let change = &step.change["wc.py"];
        assert!(change.raw.as_deref().unwrap().contains("+print(1)"));
        let extra = &change.structural.as_ref().unwrap().extra;
        assert_eq!(extra["tool"], "exec_command");
        assert_eq!(extra["after"], "print(1)\n");
        let o = otel(step, "wc.py");
        assert_eq!(
            (o["source"].as_str(), o["outcome"].as_str()),
            (Some("shell-heredoc"), Some("success"))
        );
    }

    #[test]
    fn a_failed_and_an_unanswered_write_are_both_recorded() {
        let fail = "Chunk ID: 1\nProcess exited with code 1\nOutput:\n";
        let p = derive(vec![
            generation("g1", 1, vec![system(), user()], Some(("c1", WRITE))),
            generation(
                "g2",
                2,
                vec![system(), user(), echo("c1", WRITE), answer("c1", fail)],
                Some(("c2", APPEND)),
            ),
        ]);
        let first = otel(step_by_tool(&p, "wc.py", "c1"), "wc.py");
        assert_eq!(first["outcome"], "failure");
        assert_eq!(first["executions"][0]["exit_code"], 1);
        // c2 is never answered, and c1's failure left wc.py unknown.
        let second = otel(step_by_tool(&p, "wc.py", "c2"), "wc.py");
        assert_eq!(second["outcome"], "unknown");
        assert_eq!(second["executions"][0]["outcome_basis"], "no_result");
        assert_eq!(second["executions"][0]["append_base"], "unknown");
    }

    #[test]
    fn an_append_in_a_later_turn_diffs_against_the_tracked_write() {
        let p = derive(vec![
            generation("g1", 1, vec![system(), user()], Some(("c1", WRITE))),
            generation(
                "g2",
                2,
                vec![system(), user(), echo("c1", WRITE), answer("c1", OK)],
                Some(("c2", APPEND)),
            ),
            generation(
                "g3",
                3,
                vec![
                    system(),
                    user(),
                    echo("c1", WRITE),
                    answer("c1", OK),
                    echo("c2", APPEND),
                    answer("c2", OK),
                ],
                None,
            ),
        ]);
        let second = step_by_tool(&p, "wc.py", "c2");
        let extra = &second.change["wc.py"].structural.as_ref().unwrap().extra;
        assert_eq!(extra["before"], "print(1)\n");
        assert_eq!(extra["after"], "print(1)\nprint(2)\n");
        assert_eq!(extra["operation"], "append");
        assert_eq!(extra["otel"]["executions"][0]["append_base"], "tracked");
    }

    #[test]
    fn a_codex_shell_apply_patch_becomes_an_inferred_file_change() {
        let patch = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: hello.txt\n+hi\n*** End Patch\nEOF\n";
        let p = derive(vec![
            generation("g1", 1, vec![system(), user()], Some(("c1", patch))),
            generation(
                "g2",
                2,
                vec![system(), user(), echo("c1", patch), answer("c1", OK)],
                None,
            ),
        ]);
        let step = step_by_tool(&p, "hello.txt", "c1");
        let extra = &step.change["hello.txt"].structural.as_ref().unwrap().extra;
        assert_eq!(extra["tool"], "exec_command");
        assert_eq!(extra["operation"], "add");
        assert_eq!(extra["after"], "hi\n");
        let o = otel(step, "hello.txt");
        assert_eq!(
            (o["source"].as_str(), o["outcome"].as_str()),
            (Some("shell-apply-patch"), Some("success"))
        );
    }

    #[test]
    fn a_step_keeps_its_change_key_when_a_later_generation_first_names_the_cwd() {
        let g1 = generation("g1", 1, vec![user()], Some(("c1", WRITE)));
        let g2 = generation(
            "g2",
            2,
            vec![user(), echo("c1", WRITE), answer("c1", OK)],
            None,
        );
        let g3 = generation(
            "g3",
            3,
            vec![
                user(),
                echo("c1", WRITE),
                answer("c1", OK),
                json!({"role": "assistant", "content": "done"}),
                system(),
            ],
            None,
        );
        let early = derive(vec![g1.clone(), g2.clone()]);
        let late = derive(vec![g1, g2, g3]);
        let s = step_by_tool(&early, "/w/wc.py", "c1");
        let later = late.steps.iter().find(|x| x.step.id == s.step.id).unwrap();
        assert_eq!(canonical_step_json(s), canonical_step_json(later));
    }

    /// `cd sub` in its own call, then two heredocs in later calls: both
    /// files are in `sub`.
    fn cd_then_two_writes() -> Vec<Generation> {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let calls = [
            ("b1", "cd sub"),
            ("b2", "cat <<'EOF' > f.txt\nx\nEOF"),
            ("b3", "cat <<'EOF' > g.txt\ny\nEOF"),
        ];
        let mut m = vec![sys, user()];
        let mut gens = Vec::new();
        for (i, (id, cmd)) in calls.into_iter().enumerate() {
            let n = i as u64 + 1;
            gens.push(bash_gen(&format!("g{n}"), n, m.clone(), Some((id, cmd))));
            m.extend([bash_echo(id, cmd), answer(id, "")]);
        }
        gens.push(bash_gen("g4", 4, m, None));
        gens
    }

    #[test]
    fn a_claude_bash_cd_in_an_earlier_turn_leaves_later_relative_heredocs_a_likely_path() {
        let p = derive(cd_then_two_writes());
        assert!(
            p.steps.iter().all(|s| !s
                .change
                .keys()
                .any(|k| k.ends_with("f.txt") || k.ends_with("g.txt"))),
            "neither f.txt nor sub/f.txt is a change"
        );
        let a = attempts(&p);
        assert_eq!(a.len(), 2, "{a:?}");
        for (a, (file, id, body)) in a
            .iter()
            .zip([("f.txt", "b2", "x\n"), ("g.txt", "b3", "y\n")])
        {
            assert_eq!(a["tool_id"], id);
            assert_eq!(a["path_as_written"], file);
            assert_eq!(a["reason"], "shell_dir_moved");
            assert_eq!(a["likely_path"], format!("sub/{file}"));
            assert_eq!(a["outcome"], "success");
            assert_eq!(a["body"], body);
        }
    }

    /// The step recording call `tool_id`'s unresolved shell write.
    fn step_with_attempt<'p>(p: &'p Path, tool_id: &str) -> &'p Step {
        p.steps
            .iter()
            .find(|s| {
                s.change.values().any(|c| {
                    c.structural
                        .as_ref()
                        .and_then(|sc| sc.extra.get("otel")?.get("unresolved_shell_writes"))
                        .is_some_and(|u| {
                            u.as_array()
                                .unwrap()
                                .iter()
                                .any(|a| a["tool_id"] == tool_id)
                        })
                })
            })
            .expect(tool_id)
    }

    #[test]
    fn a_likely_path_step_does_not_change_when_later_generations_arrive() {
        let gens = cd_then_two_writes();
        let early = derive(gens[..3].to_vec());
        let late = derive(gens);
        let s = step_with_attempt(&early, "b2");
        let later = late.steps.iter().find(|x| x.step.id == s.step.id).unwrap();
        assert_eq!(canonical_step_json(s), canonical_step_json(later));
    }

    #[test]
    fn a_cd_on_a_sibling_branch_does_not_move_a_relative_write() {
        // g2 retries g1's prompt: b1's cd is not on b2's ancestry.
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let write = "cat > f.txt <<'EOF'\nx\nEOF";
        let p = derive(vec![
            bash_gen("g1", 1, vec![sys.clone(), user()], Some(("b1", "cd sub"))),
            bash_gen("g2", 2, vec![sys.clone(), user()], Some(("b2", write))),
            bash_gen(
                "g3",
                3,
                vec![sys, user(), bash_echo("b2", write), answer("b2", "")],
                None,
            ),
        ]);
        assert!(
            p.steps
                .iter()
                .all(|s| !s.change.keys().any(|k| k.ends_with("f.txt"))),
            "neither sub/f.txt nor a guess"
        );
        let a = attempts(&p);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["tool_id"], "b2");
        assert_eq!(a[0]["reason"], "shell_dir_moved");
        assert!(a[0].get("likely_path").is_none());
    }

    #[test]
    fn a_cd_back_to_the_working_directory_resolves_against_it_again() {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let calls = [
            ("b1", "cd sub"),
            ("b2", "cat > f.txt <<'EOF'\nx\nEOF"),
            ("b3", "cd /w"),
            ("b4", "cat > g.txt <<'EOF'\ny\nEOF"),
        ];
        let mut m = vec![sys, user()];
        let mut gens = Vec::new();
        for (i, (id, cmd)) in calls.into_iter().enumerate() {
            let n = i as u64 + 1;
            gens.push(bash_gen(&format!("g{n}"), n, m.clone(), Some((id, cmd))));
            m.extend([bash_echo(id, cmd), answer(id, "")]);
        }
        gens.push(bash_gen("g5", 5, m, None));
        let p = derive(gens);
        let a = attempts(&p);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["likely_path"], "sub/f.txt");
        step_by_tool(&p, "g.txt", "b4");
    }

    #[test]
    fn a_cd_on_the_ancestry_gives_writes_on_every_branch_after_it_a_likely_path() {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let (f, g) = ("cat > f.txt <<'EOF'\nx\nEOF", "cat > g.txt <<'EOF'\ny\nEOF");
        let after_cd = vec![
            sys.clone(),
            user(),
            bash_echo("b1", "cd sub"),
            answer("b1", ""),
        ];
        let p = derive(vec![
            bash_gen("g1", 1, vec![sys, user()], Some(("b1", "cd sub"))),
            bash_gen("g2", 2, after_cd.clone(), Some(("b2", f))),
            bash_gen("g3", 3, after_cd, Some(("b3", g))),
        ]);
        let hints: Vec<Value> = attempts(&p)
            .iter()
            .map(|a| a["likely_path"].clone())
            .collect();
        assert_eq!(hints, [json!("sub/f.txt"), json!("sub/g.txt")]);
    }

    #[test]
    fn a_worktree_entered_on_a_sibling_branch_keeps_the_working_directory_unknown() {
        // Off the ancestry, it may have run: `cd /w` does not prove where
        // a reset puts the shell now.
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let enter = json!([{"id": "t1", "type": "function", "function": {"name": "EnterWorktree", "arguments": "{}"}}]);
        let mut g1 = generation("g1", 1, vec![sys.clone(), user()], None);
        g1.completion.text = String::new();
        g1.completion.tool_calls = serde_json::from_value(enter.clone()).unwrap();
        let entered = vec![
            sys.clone(),
            user(),
            json!({"role": "assistant", "content": "", "tool_calls": enter}),
            answer("t1", "Created worktree"),
        ];
        let edited = json!({"role": "user", "content": "write it again"});
        let write = "cat > f.txt <<'EOF'\nx\nEOF";
        let p = derive(vec![
            g1,
            bash_gen("g2", 2, entered, None),
            bash_gen(
                "g3",
                3,
                vec![sys.clone(), edited.clone()],
                Some(("b1", "cd /w")),
            ),
            bash_gen(
                "g4",
                4,
                vec![sys, edited, bash_echo("b1", "cd /w"), answer("b1", "")],
                Some(("b2", write)),
            ),
        ]);
        assert!(
            p.steps
                .iter()
                .all(|s| !s.change.keys().any(|k| k.ends_with("f.txt")))
        );
        let a = attempts(&p);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["tool_id"], "b2");
        assert_eq!(a[0]["reason"], "shell_dir_moved");
    }

    #[test]
    fn a_worktree_entered_on_a_sibling_branch_is_not_undone_by_a_cd_in_one_turn() {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let enter = json!([{"id": "t1", "type": "function", "function": {"name": "EnterWorktree", "arguments": "{}"}}]);
        let mut g1 = generation("g1", 1, vec![sys.clone(), user()], None);
        g1.completion.text = String::new();
        g1.completion.tool_calls = serde_json::from_value(enter.clone()).unwrap();
        let entered = vec![
            sys.clone(),
            user(),
            json!({"role": "assistant", "content": "", "tool_calls": enter}),
            answer("t1", "Created worktree"),
        ];
        let edited = json!({"role": "user", "content": "write it again"});
        let write = "cat > f.txt <<'EOF'\nx\nEOF";
        let mut calls = bash_calls("b1", "cd /w");
        calls
            .as_array_mut()
            .unwrap()
            .extend(bash_calls("b2", write).as_array().unwrap().clone());
        let mut g3 = generation("g3", 3, vec![sys.clone(), edited.clone()], None);
        g3.completion.text = String::new();
        g3.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
        let p = derive(vec![
            g1,
            bash_gen("g2", 2, entered, None),
            g3,
            bash_gen(
                "g4",
                4,
                vec![
                    sys,
                    edited,
                    json!({"role": "assistant", "content": "", "tool_calls": calls}),
                    answer("b1", ""),
                    answer("b2", ""),
                ],
                None,
            ),
        ]);
        assert!(
            p.steps
                .iter()
                .all(|s| !s.change.keys().any(|k| k.ends_with("f.txt")))
        );
        let a = attempts(&p);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["tool_id"], "b2");
        assert_eq!(a[0]["reason"], "shell_dir_moved");
    }

    fn bash_calls(id: &str, cmd: &str) -> Value {
        let args = json!({"command": cmd}).to_string();
        json!([{"id": id, "type": "function", "function": {"name": "Bash", "arguments": args}}])
    }

    fn bash_gen(
        id: &str,
        start: u64,
        messages: Vec<Value>,
        call: Option<(&str, &str)>,
    ) -> Generation {
        let mut g = generation(id, start, messages, None);
        if let Some((cid, cmd)) = call {
            g.completion.text = String::new();
            g.completion.tool_calls = serde_json::from_value(bash_calls(cid, cmd)).unwrap();
        }
        g
    }

    fn bash_echo(id: &str, cmd: &str) -> Value {
        json!({"role": "assistant", "content": "", "tool_calls": bash_calls(id, cmd)})
    }

    fn attempts(p: &Path) -> Vec<Value> {
        p.steps
            .iter()
            .flat_map(|s| s.change.values())
            .filter_map(|c| c.structural.as_ref()?.extra.get("otel"))
            .filter_map(|o| o.get("unresolved_shell_writes"))
            .flat_map(|u| u.as_array().unwrap().clone())
            .collect()
    }

    /// A Bash `cd`, then a fork (a compaction restarting the prompt from a
    /// summary, or a rewind to an edited first message), then a relative
    /// write: the shell is still where the `cd` left it.
    fn forked(cd: &str, restart: Value) -> Vec<Generation> {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let write = "cat > f.txt <<'EOF'\nx\nEOF";
        vec![
            bash_gen("g1", 1, vec![sys.clone(), user()], Some(("b1", cd))),
            bash_gen(
                "g2",
                2,
                vec![sys.clone(), user(), bash_echo("b1", cd), answer("b1", "")],
                None,
            ),
            bash_gen(
                "g3",
                3,
                vec![sys.clone(), restart.clone()],
                Some(("b2", write)),
            ),
            bash_gen(
                "g4",
                4,
                vec![sys, restart, bash_echo("b2", write), answer("b2", "")],
                None,
            ),
        ]
    }

    fn summary() -> Value {
        json!({"role": "user", "content": "This session is being continued from a previous conversation. Summary: ..."})
    }

    #[test]
    fn a_bash_cd_before_a_fork_leaves_a_relative_write_after_it_unresolved() {
        let edited = json!({"role": "user", "content": "write it again"});
        for (name, restart) in [("compaction", summary()), ("rewind", edited)] {
            let p = derive(forked("cd sub", restart));
            assert!(
                p.steps
                    .iter()
                    .all(|s| !s.change.keys().any(|k| k.ends_with("f.txt"))),
                "{name}"
            );
            let a = attempts(&p);
            assert_eq!(a.len(), 1, "{name}: {a:?}");
            assert_eq!(a[0]["tool_id"], "b2", "{name}");
            assert_eq!(a[0]["path_as_written"], "f.txt", "{name}");
            assert_eq!(a[0]["reason"], "shell_dir_moved", "{name}");
        }
    }

    #[test]
    fn a_bash_cd_before_a_fork_into_a_root_naming_no_cwd_leaves_a_relative_write_unresolved() {
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        let root = json!({"role": "user", "content": "a new root without a system prompt"});
        let write = "cat > f.txt <<'EOF'\nx\nEOF";
        let p = derive(vec![
            bash_gen("g1", 1, vec![sys.clone(), user()], Some(("b1", "cd sub"))),
            bash_gen(
                "g2",
                2,
                vec![sys, user(), bash_echo("b1", "cd sub"), answer("b1", "")],
                None,
            ),
            bash_gen("g3", 3, vec![root.clone()], Some(("b2", write))),
            bash_gen(
                "g4",
                4,
                vec![root, bash_echo("b2", write), answer("b2", "")],
                None,
            ),
        ]);
        assert!(
            p.steps
                .iter()
                .all(|s| !s.change.keys().any(|k| k.ends_with("f.txt")))
        );
        let a = attempts(&p);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["tool_id"], "b2");
        assert_eq!(a[0]["reason"], "shell_dir_moved");
    }

    #[test]
    fn a_bash_cd_to_the_working_directory_before_a_fork_keeps_relative_writes() {
        let p = derive(forked("cd /w && ls", summary()));
        assert!(attempts(&p).is_empty());
        assert!(p.steps.iter().any(|s| s.change.contains_key("f.txt")));
    }

    #[test]
    fn a_bash_cd_issued_after_a_write_leaves_it_resolved() {
        // The cd's generation sorts after the write's turn: it ran later.
        let mut gens = forked("ls", summary());
        let sys = json!({"role": "system", "content": "Primary working directory: /w"});
        gens.push(bash_gen(
            "g5",
            5,
            vec![
                sys,
                user(),
                bash_echo("b1", "ls"),
                answer("b1", ""),
                json!({"role": "assistant", "content": "done"}),
                json!({"role": "user", "content": "more"}),
            ],
            Some(("b3", "cd sub")),
        ));
        let p = derive(gens);
        assert!(attempts(&p).is_empty());
        assert!(p.steps.iter().any(|s| s.change.contains_key("f.txt")));
    }

    #[test]
    fn a_settled_shell_write_step_does_not_change() {
        let g1 = generation("g1", 1, vec![system(), user()], Some(("c1", WRITE)));
        let g2 = generation(
            "g2",
            2,
            vec![system(), user(), echo("c1", WRITE), answer("c1", OK)],
            Some(("c2", APPEND)),
        );
        let g3 = generation(
            "g3",
            3,
            vec![
                system(),
                user(),
                echo("c1", WRITE),
                answer("c1", OK),
                echo("c2", APPEND),
                answer("c2", OK),
            ],
            None,
        );
        let early = derive(vec![g1.clone(), g2.clone()]);
        let late = derive(vec![g1, g2, g3]);
        let s = step_by_tool(&early, "wc.py", "c1");
        let later = late.steps.iter().find(|x| x.step.id == s.step.id).unwrap();
        assert_eq!(canonical_step_json(s), canonical_step_json(later));
    }
}
