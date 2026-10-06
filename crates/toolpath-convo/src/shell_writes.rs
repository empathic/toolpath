//! Heredoc file writes and `apply_patch` heredocs read from a shell script's
//! text. Nothing is executed; anything not followed exactly is
//! [`ShellItem::Other`], and a write whose target cannot be resolved is
//! [`ShellItem::Unresolved`], never a guessed path. A command that plainly
//! writes a file through a redirect or `tee` in a form not followed is
//! [`ShellItem::Unmodeled`]: its targets, never its content.

/// The command a heredoc write went through.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// `cat <<TAG > path`.
    Cat,
    /// `tee [-a] path <<TAG`.
    Tee,
}

impl Via {
    /// The command name.
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Cat => "cat",
            Via::Tee => "tee",
        }
    }
}

/// How a script's exit status relates to one of its commands.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLink {
    /// The command is the whole script: the script's status is its own.
    Sole,
    /// A zero exit status of the script means the command ran and exited
    /// zero: it is not backgrounded, not in a pipeline, not right after
    /// `||`, and every list operator after it is `&&`.
    ImpliedBySuccess,
    /// The script's status says nothing about the command.
    Independent,
}

/// The directory relative paths resolve against at a point in a script.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ScriptDir {
    /// The directory the script started in.
    #[default]
    Start,
    /// Moved by literal `cd`s: absolute, or relative to the start directory.
    At(String),
    /// A directory change that cannot be followed.
    Unknown,
}

/// Why a heredoc write's target could not be resolved.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolvable {
    /// The target has an expansion, glob or brace.
    NotLiteral,
    /// A relative target after a directory change that cannot be followed.
    UnknownDir,
    /// A write form this reader does not model ([`ShellItem::Unmodeled`]).
    Unmodeled,
}

impl Unresolvable {
    /// The serialized name.
    pub fn as_str(self) -> &'static str {
        match self {
            Unresolvable::NotLiteral => "not_literal",
            Unresolvable::UnknownDir => "unknown_dir",
            Unresolvable::Unmodeled => "unmodeled",
        }
    }
}

/// One heredoc body redirected into one file.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocWrite {
    /// Target path with the script's literal `cd`s folded in; normalized.
    pub path: String,
    /// `>>` or `tee -a`.
    pub append: bool,
    /// The writing command.
    pub via: Via,
    /// The body as written, every line `\n`-terminated; never expanded.
    pub body: String,
    /// The heredoc delimiter, quotes removed.
    pub tag: String,
    /// Any part of the tag was quoted or escaped, so the body is literal.
    pub tag_quoted: bool,
    /// `<<-`.
    pub strip_tabs: bool,
    /// What the script's exit status says about this write.
    pub status_link: StatusLink,
}

impl HeredocWrite {
    /// Unquoted tag over a body with `$`, `` ` `` or `\`: the file may
    /// differ from [`HeredocWrite::body`].
    pub fn may_expand(&self) -> bool {
        !self.tag_quoted && self.body.contains(['$', '`', '\\'])
    }
}

/// A heredoc write whose target cannot be resolved to a path.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedWrite {
    /// The write, its `path` being the target as written (quotes removed).
    pub write: HeredocWrite,
    pub reason: Unresolvable,
}

/// A file an unmodeled command writes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmodeledTarget {
    /// The target as written, quotes removed.
    pub path: String,
    /// No expansion, glob or brace in the target.
    pub literal: bool,
    /// The target with the script's literal `cd`s folded in, normalized;
    /// `None` when it is not literal or the directory is unknown.
    pub resolved: Option<String>,
    /// `>>`, `&>>` or `tee -a`.
    pub append: bool,
    /// A `tee` file argument rather than a redirect.
    pub tee: bool,
}

/// A simple command that writes files through redirects (`>`, `>>`, `>|`,
/// `&>`, `&>>`, `>&FILE`, any descriptor) or `tee` file arguments, in a
/// form this reader does not model. Targets under `/dev/` are not files.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmodeledWrite {
    /// The command as [`ShellItem::Other`] would hold it.
    pub command: String,
    /// The command word, when it has one.
    pub program: Option<String>,
    /// In the order written.
    pub targets: Vec<UnmodeledTarget>,
    /// What the script's exit status says about this command.
    pub status_link: StatusLink,
}

/// A patch fed to `apply_patch` (or `applypatch`), through a heredoc or
/// as an argv ([`parse_argv`]).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocPatch {
    /// The directory the patch ran in.
    pub dir: ScriptDir,
    /// `apply_patch` or `applypatch`, as written.
    pub command: String,
    /// The patch text as written; not validated.
    pub body: String,
    /// The heredoc delimiter, quotes removed; empty for an argv patch.
    pub tag: String,
    /// Any part of the tag was quoted or escaped; always for an argv patch,
    /// whose text is literal.
    pub tag_quoted: bool,
    /// `<<-`.
    pub strip_tabs: bool,
    /// What the script's exit status says about this patch.
    pub status_link: StatusLink,
}

impl HeredocPatch {
    /// A path named inside the patch, relative ones joined onto
    /// [`HeredocPatch::dir`]; normalized. `None` for a relative path in an
    /// unknown directory.
    pub fn resolve(&self, path: &str) -> Option<String> {
        resolve(&self.dir, path)
    }

    /// The files the patch names ([`parse_patch`]).
    pub fn files(&self) -> Vec<PatchFile> {
        parse_patch(&self.body)
    }
}

/// What a V4A patch does to one file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOp {
    /// `*** Add File: PATH`.
    Add,
    /// `*** Update File: PATH`, optionally followed by `*** Move to: PATH`.
    Update,
    /// `*** Delete File: PATH`.
    Delete,
}

impl PatchOp {
    /// The serialized name, as `patch_apply_end` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            PatchOp::Add => "add",
            PatchOp::Update => "update",
            PatchOp::Delete => "delete",
        }
    }
}

/// One file of a V4A patch.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    pub op: PatchOp,
    /// The path as written in the patch, trimmed; never empty.
    pub path: String,
    /// `*** Move to:` of an update, as written.
    pub move_to: Option<String>,
    /// An added file's content, from its `+` lines, every line
    /// `\n`-terminated; `None` for any other operation.
    pub added: Option<String>,
}

/// The files of a V4A patch (`*** Begin Patch` … `*** End Patch`, the
/// format of Codex's `apply_patch`), in order. Update hunks are not unified
/// diffs, so an update carries no content. Text with no file header is
/// no files; nothing is validated.
pub fn parse_patch(patch: &str) -> Vec<PatchFile> {
    let mut out: Vec<PatchFile> = Vec::new();
    let mut adding = false;
    for line in patch.lines() {
        let file = |op: PatchOp, p: &str| PatchFile {
            op,
            path: p.trim().to_string(),
            move_to: None,
            added: (op == PatchOp::Add).then(String::new),
        };
        if let Some(p) = line.strip_prefix("*** Add File: ") {
            out.push(file(PatchOp::Add, p));
            adding = true;
        } else if let Some(p) = line.strip_prefix("*** Update File: ") {
            out.push(file(PatchOp::Update, p));
            adding = false;
        } else if let Some(p) = line.strip_prefix("*** Delete File: ") {
            out.push(file(PatchOp::Delete, p));
            adding = false;
        } else if let Some(p) = line.strip_prefix("*** Move to: ") {
            if let Some(f) = out.last_mut() {
                f.move_to = Some(p.trim().to_string());
            }
        } else if line.starts_with("*** ") {
            adding = false;
        } else if adding
            && let (Some(f), Some(c)) = (out.last_mut(), line.strip_prefix('+'))
            && let Some(a) = f.added.as_mut()
        {
            a.push_str(c);
            a.push('\n');
        }
    }
    out.retain(|f| !f.path.is_empty());
    out
}

/// One simple command of a script, as far as file writes go.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellItem {
    /// A heredoc write.
    Write(HeredocWrite),
    /// A heredoc write to a target that cannot be resolved.
    Unresolved(UnresolvedWrite),
    /// An `apply_patch` heredoc.
    Patch(HeredocPatch),
    /// A command that writes files in a form not followed.
    Unmodeled(UnmodeledWrite),
    /// Any other simple command as text (words, redirect targets, heredoc
    /// bodies); an unsplit script is one `Other` holding all of it.
    Other(String),
}

/// The result of [`parse_script`] or [`parse_argv`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedScript {
    /// In command order; literal `cd`s are folded into later paths, not items.
    pub items: Vec<ShellItem>,
    /// Simple commands in the script, `cd`s included; 0 when it was not split.
    pub simple_commands: usize,
    /// [`ParsedScript::dir_changes`] is not empty.
    pub may_change_dir: bool,
    /// The commands that may change the working directory of the shell the
    /// script runs in, in order: `At` (always absolute) for a literal
    /// `cd /DIR`, `Unknown` for any other. Over-reports, never
    /// under-reports; see the crate README.
    pub dir_changes: Vec<ScriptDir>,
    /// The directory the script's shell is left in when the script exits
    /// zero: `Start` when [`ParsedScript::dir_changes`] is empty, `At`
    /// (absolute, or relative to the start) when every change is a literal
    /// `cd` in an `&&` chain from the script's start whose [`StatusLink`]
    /// is not `Independent`, `Unknown` otherwise.
    pub dir_on_success: ScriptDir,
    /// Each simple command's words from its command word on (leading
    /// `NAME=value` assignments dropped), quotes removed, redirections and
    /// heredoc bodies left out; empty when the script was not split.
    pub command_words: Vec<Vec<String>>,
}

impl ParsedScript {
    /// The heredoc writes, in order.
    pub fn writes(&self) -> impl Iterator<Item = &HeredocWrite> {
        self.items.iter().filter_map(|i| match i {
            ShellItem::Write(w) => Some(w),
            _ => None,
        })
    }

    /// The heredoc writes whose target cannot be resolved, in order.
    pub fn unresolved(&self) -> impl Iterator<Item = &UnresolvedWrite> {
        self.items.iter().filter_map(|i| match i {
            ShellItem::Unresolved(u) => Some(u),
            _ => None,
        })
    }

    /// The commands writing files in a form not followed, in order.
    pub fn unmodeled(&self) -> impl Iterator<Item = &UnmodeledWrite> {
        self.items.iter().filter_map(|i| match i {
            ShellItem::Unmodeled(u) => Some(u),
            _ => None,
        })
    }

    /// The `apply_patch` heredocs, in order.
    pub fn patches(&self) -> impl Iterator<Item = &HeredocPatch> {
        self.items.iter().filter_map(|i| match i {
            ShellItem::Patch(p) => Some(p),
            _ => None,
        })
    }
}

/// Splits a shell script into [`ShellItem`]s.
pub fn parse_script(command: &str) -> ParsedScript {
    let (mut parsed, followed) = match lex(command) {
        Some((cmds, docs)) => (classify(command, &cmds, &docs), follow_dirs(&cmds)),
        None => (whole(command), None),
    };
    parsed.dir_changes = dir_changes(command);
    parsed.may_change_dir = !parsed.dir_changes.is_empty();
    parsed.dir_on_success = match followed {
        _ if parsed.dir_changes.is_empty() => ScriptDir::Start,
        // Both readers must see the same changes, or one missed a command.
        Some((changes, dir)) if changes == parsed.dir_changes => dir,
        _ => ScriptDir::Unknown,
    };
    parsed
}

/// The directory changes of split commands as [`dir_changes`] names them,
/// and the directory a zero exit status implies. `None` for a compound
/// command.
fn follow_dirs(cmds: &[Simple]) -> Option<(Vec<ScriptDir>, ScriptDir)> {
    let compound = cmds.iter().any(|s| {
        s.words
            .first()
            .is_some_and(|w| COMPOUND.contains(&w.text.as_str()))
    });
    if compound {
        return None;
    }
    let mut changes = Vec::new();
    let mut dir = ScriptDir::Start;
    for (i, s) in cmds.iter().enumerate() {
        if let Some(target) = cd_target(s) {
            changes.push(match &target {
                Some(t) if t.starts_with('/') => ScriptDir::At(normalize_path(t)),
                _ => ScriptDir::Unknown,
            });
            let from_start = cmds[..i].iter().all(|s| s.after == Sep::And);
            dir = match status_link(cmds, i) {
                StatusLink::Independent => ScriptDir::Unknown,
                _ if from_start => step_dir(dir, target),
                _ => ScriptDir::Unknown,
            };
        } else if command_word(s).is_some_and(|w| DIR_WORDS.contains(&w.text.as_str()) || opaque(w))
        {
            changes.push(ScriptDir::Unknown);
            dir = ScriptDir::Unknown;
        }
    }
    Some((changes, dir))
}

/// Reads a program's argv: a shell (`sh`, `bash`, `zsh`, `dash`, `ksh`)
/// given a `-c` script is [`parse_script`] of it, `apply_patch PATCH` is one
/// patch, and anything else is one [`ShellItem::Other`] of its words.
pub fn parse_argv(argv: &[&str]) -> ParsedScript {
    match argv {
        [cmd, patch] if matches!(*cmd, "apply_patch" | "applypatch") => ParsedScript {
            items: vec![ShellItem::Patch(HeredocPatch {
                dir: ScriptDir::Start,
                command: cmd.to_string(),
                body: patch.to_string(),
                tag: String::new(),
                tag_quoted: true,
                strip_tabs: false,
                status_link: StatusLink::Sole,
            })],
            simple_commands: 1,
            ..Default::default()
        },
        [program, args @ ..] if is_shell(program) => match shell_script(args) {
            Some(script) => parse_script(script),
            None => not_a_script(argv),
        },
        _ => not_a_script(argv),
    }
}

fn is_shell(program: &str) -> bool {
    let name = program.rsplit('/').next().unwrap_or(program);
    matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh")
}

/// The script after the first `-c` (possibly combined, `-lc`), when every
/// argument before it is a flag.
fn shell_script<'a>(args: &[&'a str]) -> Option<&'a str> {
    for (k, a) in args.iter().enumerate() {
        let flags = a
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphabetic()))?;
        if flags.contains('c') {
            return args.get(k + 1).copied();
        }
    }
    None
}

fn not_a_script(argv: &[&str]) -> ParsedScript {
    ParsedScript {
        items: vec![ShellItem::Other(argv.join(" "))],
        ..Default::default()
    }
}

fn whole(command: &str) -> ParsedScript {
    ParsedScript {
        items: vec![ShellItem::Other(command.to_string())],
        ..Default::default()
    }
}

/// `NAME=value` or `NAME+=value`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let name = name.strip_suffix('+').unwrap_or(name);
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Lexical path normalization (`.`, `..`, empty components); never above `/`.
pub fn normalize_path(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&p) if p != ".." => {
                    parts.pop();
                }
                _ if abs => {}
                _ => parts.push(".."),
            },
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    match (abs, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    /// No expansion (`$`, `~`), glob (`*`, `?`, `[`) or brace (`{`) outside
    /// quotes.
    literal: bool,
}

#[derive(Debug, Clone)]
struct Heredoc {
    tag: String,
    quoted: bool,
    strip_tabs: bool,
    /// `None` until the terminator line is read; stays `None` when the
    /// string ends first.
    body: Option<String>,
}

#[derive(Debug, Clone)]
struct Redir {
    fd: Option<u32>,
    op: &'static str,
    target: Word,
}

/// The list operator that ends a simple command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Sep {
    #[default]
    End,
    /// `;` or a newline.
    Seq,
    And,
    Or,
    /// `|` or `|&`.
    Pipe,
    Background,
}

#[derive(Debug, Default)]
struct Simple {
    words: Vec<Word>,
    redirs: Vec<Redir>,
    heredocs: Vec<usize>,
    herestring: bool,
    after: Sep,
}

impl Simple {
    fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.redirs.is_empty()
            && self.heredocs.is_empty()
            && !self.herestring
    }

    fn text(&self, docs: &[Heredoc]) -> String {
        let mut t: Vec<String> = self.words.iter().map(|w| w.text.clone()).collect();
        t.extend(
            self.redirs
                .iter()
                .map(|r| format!("{}{}", r.op, r.target.text)),
        );
        t.extend(self.heredocs.iter().filter_map(|&d| docs[d].body.clone()));
        t.join(" ")
    }
}

const METACHARS: [char; 8] = [';', '&', '|', '<', '>', '(', ')', '\n'];

/// Bash's blanks; `\r` is an ordinary word character.
fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

/// Simple commands and heredocs, or `None` for a command this parser does
/// not split (subshells, substitutions, backquotes, unbalanced quotes,
/// a redirect with no target).
fn lex(command: &str) -> Option<(Vec<Simple>, Vec<Heredoc>)> {
    let c: Vec<char> = command.chars().collect();
    let mut i = 0;
    let mut cmds: Vec<Simple> = vec![Simple::default()];
    let mut docs: Vec<Heredoc> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    fn split(cmds: &mut Vec<Simple>, sep: Sep) {
        match cmds.last_mut() {
            Some(last) if !last.is_empty() => last.after = sep,
            _ => return,
        }
        cmds.push(Simple::default());
    }
    while let Some(&ch) = c.get(i) {
        match ch {
            b if is_blank(b) => i += 1,
            '\\' if c.get(i + 1) == Some(&'\n') => i += 2,
            '\n' => {
                i += 1;
                for &d in &pending {
                    i = read_body(&c, i, &mut docs[d])?;
                }
                pending.clear();
                split(&mut cmds, Sep::Seq);
            }
            '#' => {
                while c.get(i).is_some_and(|&x| x != '\n') {
                    i += 1;
                }
            }
            '(' | ')' | '`' => return None,
            ';' => {
                i += 1;
                if c.get(i) == Some(&';') {
                    i += 1;
                }
                split(&mut cmds, Sep::Seq);
            }
            '|' => {
                i += 1;
                let sep = match c.get(i) {
                    Some('|') => Sep::Or,
                    _ => Sep::Pipe,
                };
                if matches!(c.get(i), Some('|' | '&')) {
                    i += 1;
                }
                split(&mut cmds, sep);
            }
            '&' if c.get(i + 1) == Some(&'>') => {
                let append = c.get(i + 2) == Some(&'>');
                i += if append { 3 } else { 2 };
                let target = read_target(&c, &mut i)?;
                let op = if append { "&>>" } else { "&>" };
                cmds.last_mut()?.redirs.push(Redir {
                    fd: None,
                    op,
                    target,
                });
            }
            '&' => {
                let and = c.get(i + 1) == Some(&'&');
                i += if and { 2 } else { 1 };
                split(&mut cmds, if and { Sep::And } else { Sep::Background });
            }
            '<' | '>' => redirect(&c, &mut i, None, cmds.last_mut()?, &mut docs, &mut pending)?,
            d if d.is_ascii_digit() && fd_ahead(&c, i) => {
                let start = i;
                while c.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
                let fd: String = c[start..i].iter().collect();
                let fd = fd.parse().ok();
                redirect(&c, &mut i, fd, cmds.last_mut()?, &mut docs, &mut pending)?;
            }
            _ => {
                let w = read_word(&c, &mut i)?;
                cmds.last_mut()?.words.push(w);
            }
        }
    }
    if cmds.last().is_some_and(Simple::is_empty) {
        cmds.pop();
    }
    Some((cmds, docs))
}

/// Digits immediately followed by `<` or `>`: a file-descriptor prefix.
fn fd_ahead(c: &[char], i: usize) -> bool {
    let mut j = i;
    while c.get(j).is_some_and(char::is_ascii_digit) {
        j += 1;
    }
    j > i && matches!(c.get(j), Some('<' | '>'))
}

fn redirect(
    c: &[char],
    i: &mut usize,
    fd: Option<u32>,
    cmd: &mut Simple,
    docs: &mut Vec<Heredoc>,
    pending: &mut Vec<usize>,
) -> Option<()> {
    const OPS: [&str; 10] = ["<<<", "<<-", "<<", "<>", "<&", "<", ">>", ">|", ">&", ">"];
    let op: &'static str = OPS.iter().copied().find(|op| {
        op.chars()
            .enumerate()
            .all(|(k, oc)| c.get(*i + k) == Some(&oc))
    })?;
    *i += op.len();
    match op {
        "<<" | "<<-" => {
            while c.get(*i).copied().is_some_and(is_blank) {
                *i += 1;
            }
            let (tag, quoted) = read_tag(c, i)?;
            docs.push(Heredoc {
                tag,
                quoted,
                strip_tabs: op == "<<-",
                body: None,
            });
            cmd.heredocs.push(docs.len() - 1);
            pending.push(docs.len() - 1);
        }
        "<<<" => {
            cmd.herestring = true;
            read_target(c, i)?;
        }
        _ => {
            let target = read_target(c, i)?;
            cmd.redirs.push(Redir { fd, op, target });
        }
    }
    Some(())
}

fn read_target(c: &[char], i: &mut usize) -> Option<Word> {
    while c.get(*i).copied().is_some_and(is_blank) {
        *i += 1;
    }
    match c.get(*i) {
        None => None,
        Some(x) if METACHARS.contains(x) => None,
        Some(_) => read_word(c, i),
    }
}

/// A heredoc delimiter word, quotes removed; quoted when any part of it
/// was quoted or escaped.
fn read_tag(c: &[char], i: &mut usize) -> Option<(String, bool)> {
    let mut tag = String::new();
    let mut quoted = false;
    while let Some(&ch) = c.get(*i) {
        if is_blank(ch) || METACHARS.contains(&ch) {
            break;
        }
        *i += 1;
        match ch {
            '\'' | '"' => {
                quoted = true;
                loop {
                    let &q = c.get(*i)?;
                    *i += 1;
                    if q == ch {
                        break;
                    }
                    tag.push(q);
                }
            }
            '\\' => {
                quoted = true;
                let &n = c.get(*i)?;
                *i += 1;
                tag.push(n);
            }
            _ => tag.push(ch),
        }
    }
    (!tag.is_empty()).then_some((tag, quoted))
}

fn read_word(c: &[char], i: &mut usize) -> Option<Word> {
    let start = *i;
    let mut text = String::new();
    let mut literal = true;
    while let Some(&ch) = c.get(*i) {
        if is_blank(ch) || METACHARS.contains(&ch) {
            break;
        }
        *i += 1;
        match ch {
            '`' => return None,
            '\'' => loop {
                let &q = c.get(*i)?;
                *i += 1;
                if q == '\'' {
                    break;
                }
                text.push(q);
            },
            '"' => loop {
                let &q = c.get(*i)?;
                *i += 1;
                match q {
                    '"' => break,
                    '`' => return None,
                    '\\' => {
                        let &n = c.get(*i)?;
                        *i += 1;
                        match n {
                            '$' | '`' | '"' | '\\' => text.push(n),
                            '\n' => {}
                            _ => {
                                text.push('\\');
                                text.push(n);
                            }
                        }
                    }
                    '$' => {
                        if c.get(*i) == Some(&'(') {
                            return None;
                        }
                        literal = false;
                        text.push('$');
                    }
                    _ => text.push(q),
                }
            },
            '\\' => match c.get(*i) {
                Some(&'\n') => *i += 1,
                Some(&n) => {
                    text.push(n);
                    *i += 1;
                }
                None => {}
            },
            '$' => {
                if c.get(*i) == Some(&'(') {
                    return None;
                }
                literal = false;
                text.push('$');
            }
            '*' | '?' | '[' | '{' => {
                literal = false;
                text.push(ch);
            }
            '~' if *i - 1 == start => {
                literal = false;
                text.push(ch);
            }
            _ => text.push(ch),
        }
    }
    Some(Word { text, literal })
}

/// Reads one heredoc body from line start `i`; returns the index after its
/// terminator. An unterminated body consumes the rest and stays `None`.
/// `None` for a `\`-newline continuation under an unquoted `<<-` tag.
fn read_body(c: &[char], mut i: usize, doc: &mut Heredoc) -> Option<usize> {
    let mut body = String::new();
    // Under an unquoted tag, bash joins a line ending in an odd number of
    // `\` with the next before comparing it to the tag.
    let mut joined: Option<String> = None;
    let mut held = String::new();
    while i < c.len() {
        let end = c[i..]
            .iter()
            .position(|&x| x == '\n')
            .map_or(c.len(), |p| i + p);
        let raw: String = c[i..end].iter().collect();
        let line = if doc.strip_tabs {
            raw.trim_start_matches('\t')
        } else {
            raw.as_str()
        };
        let trailing = line.chars().rev().take_while(|&x| x == '\\').count();
        if !doc.quoted && trailing % 2 == 1 && end < c.len() {
            if doc.strip_tabs {
                return None;
            }
            joined
                .get_or_insert_with(String::new)
                .push_str(&line[..line.len() - 1]);
            held.push_str(line);
            held.push('\n');
            i = end + 1;
            continue;
        }
        let logical = match joined.take() {
            Some(mut j) => {
                j.push_str(line);
                j
            }
            None => line.to_string(),
        };
        if logical == doc.tag {
            doc.body = Some(body);
            return Some((end + 1).min(c.len()));
        }
        body.push_str(&held);
        held.clear();
        body.push_str(line);
        body.push('\n');
        i = end + 1;
    }
    Some(c.len())
}

/// Words that open or close a compound command or group: a command
/// containing one yields no write.
const COMPOUND: [&str; 21] = [
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "select", "function", "{", "}", "[[", "]]", "!", "time", "coproc",
];

/// Command words that may change the shell's working directory, now or
/// (`trap`, `alias`) when something later runs.
const DIR_WORDS: [&str; 10] = [
    "cd", "pushd", "popd", "source", ".", "eval", "builtin", "command", "trap", "alias",
];

fn classify(command: &str, cmds: &[Simple], docs: &[Heredoc]) -> ParsedScript {
    let compound = cmds.iter().any(|s| {
        s.words
            .first()
            .is_some_and(|w| COMPOUND.contains(&w.text.as_str()))
    });
    if compound {
        return whole(command);
    }
    let mut dir = ScriptDir::Start;
    // `dir` rests on a `cd` that only an unbroken `&&` chain guarantees ran.
    let mut chained = false;
    let mut items = Vec::new();
    for (i, s) in cmds.iter().enumerate() {
        if let Some(target) = cd_target(s) {
            dir = if entered(cmds, i) && s.after == Sep::And {
                chained = true;
                step_dir(dir, target)
            } else {
                ScriptDir::Unknown
            };
        } else {
            let link = status_link(cmds, i);
            let item = match heredoc_write(s, docs, link) {
                Some((mut write, literal)) => {
                    let resolved = resolve(&dir, &write.path);
                    match (literal, resolved) {
                        (true, Some(path)) => {
                            write.path = path;
                            ShellItem::Write(write)
                        }
                        (literal, _) => ShellItem::Unresolved(UnresolvedWrite {
                            write,
                            reason: if literal {
                                Unresolvable::UnknownDir
                            } else {
                                Unresolvable::NotLiteral
                            },
                        }),
                    }
                }
                None => match heredoc_patch(s, docs, &dir, link) {
                    Some(p) => ShellItem::Patch(p),
                    None => match unmodeled_targets(s, &dir) {
                        targets if targets.is_empty() => ShellItem::Other(s.text(docs)),
                        targets => ShellItem::Unmodeled(UnmodeledWrite {
                            command: s.text(docs),
                            program: command_word(s).map(|w| w.text.clone()),
                            targets,
                            status_link: link,
                        }),
                    },
                },
            };
            items.push(item);
            if command_word(s).is_some_and(|w| DIR_WORDS.contains(&w.text.as_str()) || opaque(w)) {
                dir = ScriptDir::Unknown;
            }
        }
        if chained && s.after != Sep::And {
            dir = ScriptDir::Unknown;
            chained = false;
        }
    }
    let command_words = cmds
        .iter()
        .map(|s| {
            s.words
                .iter()
                .skip_while(|w| is_assignment(&w.text))
                .map(|w| w.text.clone())
                .collect()
        })
        .collect();
    ParsedScript {
        items,
        simple_commands: cmds.len(),
        command_words,
        ..Default::default()
    }
}

/// The first word that is not a `NAME=value` assignment.
fn command_word(s: &Simple) -> Option<&Word> {
    s.words.iter().find(|w| !is_assignment(&w.text))
}

/// A command word whose expansion could name any command, `cd` included.
fn opaque(w: &Word) -> bool {
    !w.literal && w.text != "["
}

/// The list reaches the command whenever it runs: it is not the right side
/// of `||` or of a pipe.
fn entered(cmds: &[Simple], i: usize) -> bool {
    i == 0 || !matches!(cmds[i - 1].after, Sep::Or | Sep::Pipe)
}

fn status_link(cmds: &[Simple], i: usize) -> StatusLink {
    let n = cmds.len();
    let ends = matches!(cmds[n - 1].after, Sep::End | Sep::Seq);
    if n == 1 && ends {
        StatusLink::Sole
    } else if ends && entered(cmds, i) && cmds[i..n - 1].iter().all(|s| s.after == Sep::And) {
        StatusLink::ImpliedBySuccess
    } else {
        StatusLink::Independent
    }
}

/// `Some(Some(dir))` for a literal `cd DIR`, `Some(None)` for any other
/// `cd`, `pushd` or `popd`, `None` for anything else.
fn cd_target(s: &Simple) -> Option<Option<String>> {
    match s.words.first()?.text.as_str() {
        "cd" => Some(match s.words.as_slice() {
            [_, w]
                if w.literal
                    && !w.text.is_empty()
                    && !w.text.starts_with(['-', '+'])
                    && !w.text.contains(char::is_control)
                    && s.redirs.is_empty()
                    && s.heredocs.is_empty() =>
            {
                Some(w.text.clone())
            }
            _ => None,
        }),
        "pushd" | "popd" => Some(None),
        _ => None,
    }
}

fn step_dir(dir: ScriptDir, target: Option<String>) -> ScriptDir {
    let Some(t) = target else {
        return ScriptDir::Unknown;
    };
    if t.starts_with('/') {
        return ScriptDir::At(normalize_path(&t));
    }
    match dir {
        ScriptDir::Start => ScriptDir::At(normalize_path(&t)),
        ScriptDir::At(d) => ScriptDir::At(normalize_path(&format!("{d}/{t}"))),
        ScriptDir::Unknown => ScriptDir::Unknown,
    }
}

fn resolve(dir: &ScriptDir, path: &str) -> Option<String> {
    if path.starts_with('/') {
        return Some(normalize_path(path));
    }
    match dir {
        ScriptDir::Start => Some(normalize_path(path)),
        ScriptDir::At(d) => Some(normalize_path(&format!("{d}/{path}"))),
        ScriptDir::Unknown => None,
    }
}

/// [`ParsedScript::dir_changes`]: the commands that run in the script's own
/// shell, skipping subshells (`( … )`, `$( … )`, backquotes, `<( … )`,
/// `>( … )`), quoted text and heredoc bodies. Anything this scan cannot
/// match is one `Unknown`.
fn dir_changes(command: &str) -> Vec<ScriptDir> {
    let c: Vec<char> = command.chars().collect();
    let mut scan = Scan {
        c: &c,
        i: 0,
        depth: 0,
        pending: Vec::new(),
        blocked: false,
        changes: Vec::new(),
    };
    match scan.list() {
        Some(()) => scan.changes,
        None => vec![ScriptDir::Unknown],
    }
}

struct Scan<'a> {
    c: &'a [char],
    i: usize,
    /// Subshell nesting; changes are recorded only at depth 0.
    depth: usize,
    /// Heredocs whose bodies start after the next newline.
    pending: Vec<Heredoc>,
    /// Inside a subshell opened on a line with pending heredocs: a newline
    /// here is not followed.
    blocked: bool,
    changes: Vec<ScriptDir>,
}

#[derive(Default)]
struct ScanWord {
    text: String,
    /// Parameter, command or tilde expansion.
    expands: bool,
    /// An unquoted glob or brace character.
    glob: bool,
}

/// One simple command: its command word and arguments.
struct ScanCmd {
    word: ScanWord,
    args: Vec<ScanWord>,
}

impl Scan<'_> {
    fn at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    /// A command list up to the end, or at depth > 0 up to and including
    /// its closing `)`.
    fn list(&mut self) -> Option<()> {
        // Reserved words and assignments leave the command word ahead.
        const LEAD: [&str; 15] = [
            "if", "then", "elif", "else", "fi", "do", "done", "while", "until", "!", "{", "}",
            "esac", "[[", "time",
        ];
        let mut at_cmd = true;
        let mut time_flags = false;
        let mut cmd: Option<ScanCmd> = None;
        while let Some(ch) = self.at(0) {
            match ch {
                b if is_blank(b) => self.i += 1,
                '\\' if self.at(1) == Some('\n') => self.i += 2,
                '\n' => {
                    if self.blocked {
                        return None;
                    }
                    self.i += 1;
                    for mut doc in std::mem::take(&mut self.pending) {
                        self.i = read_body(self.c, self.i, &mut doc)?;
                    }
                    self.end(&mut cmd);
                    at_cmd = true;
                }
                '#' => {
                    while self.at(0).is_some_and(|x| x != '\n') {
                        self.i += 1;
                    }
                }
                ';' => {
                    if matches!(self.at(1), Some(';' | '&')) {
                        return None;
                    }
                    self.i += 1;
                    self.end(&mut cmd);
                    at_cmd = true;
                }
                '&' if self.at(1) == Some('>') => {
                    self.i += if self.at(2) == Some('>') { 3 } else { 2 };
                    self.target()?;
                }
                '|' | '&' => {
                    self.i += 1;
                    if matches!(self.at(0), Some('|' | '&')) {
                        self.i += 1;
                    }
                    self.end(&mut cmd);
                    at_cmd = true;
                }
                '(' if at_cmd && cmd.is_none() => {
                    if self.at(1) == Some('(') {
                        self.i += 2;
                        self.parens(2)?;
                    } else {
                        self.i += 1;
                        self.subshell()?;
                    }
                    at_cmd = false;
                }
                '(' => {
                    // `name ()`: a function definition; its body follows.
                    self.i += 1;
                    while self.at(0).is_some_and(is_blank) {
                        self.i += 1;
                    }
                    let name_only = cmd.as_ref().is_some_and(|c| c.args.is_empty());
                    if self.at(0) != Some(')') || !name_only {
                        return None;
                    }
                    self.i += 1;
                    cmd = None;
                    at_cmd = true;
                }
                ')' if self.depth > 0 => {
                    self.i += 1;
                    self.end(&mut cmd);
                    return Some(());
                }
                ')' => return None,
                '<' | '>' => self.redirect()?,
                d if d.is_ascii_digit() && fd_ahead(self.c, self.i) => {
                    while self.at(0).is_some_and(|x| x.is_ascii_digit()) {
                        self.i += 1;
                    }
                    self.redirect()?;
                }
                _ => {
                    let w = self.word()?;
                    if let Some(c) = &mut cmd {
                        c.args.push(w);
                    } else if at_cmd {
                        if matches!(w.text.as_str(), "case" | "function") {
                            return None;
                        }
                        let lead = LEAD.contains(&w.text.as_str())
                            || is_assignment(&w.text)
                            || (time_flags && w.text.starts_with('-'));
                        time_flags = w.text == "time" || (time_flags && lead);
                        if !lead {
                            cmd = Some(ScanCmd {
                                word: w,
                                args: Vec::new(),
                            });
                            at_cmd = false;
                        }
                    }
                }
            }
        }
        if self.depth > 0 {
            return None;
        }
        self.end(&mut cmd);
        Some(())
    }

    /// Records the directory change of a finished simple command.
    fn end(&mut self, cmd: &mut Option<ScanCmd>) {
        let Some(ScanCmd { word, args }) = cmd.take() else {
            return;
        };
        if self.depth > 0 {
            return;
        }
        let plain = |w: &ScanWord| !w.expands && !w.glob;
        let change = if !word.expands && (!word.glob || word.text == "[") {
            match (word.text.as_str(), args.as_slice()) {
                ("cd", [dir])
                    if plain(dir)
                        && dir.text.starts_with('/')
                        && !dir.text.contains(char::is_control) =>
                {
                    Some(ScriptDir::At(normalize_path(&dir.text)))
                }
                (w, _) if DIR_WORDS.contains(&w) => Some(ScriptDir::Unknown),
                _ => None,
            }
        } else {
            Some(ScriptDir::Unknown)
        };
        self.changes.extend(change);
    }

    /// The rest of a `( … )`, `$( … )`, `<( … )` or `>( … )`, discarded.
    fn subshell(&mut self) -> Option<()> {
        let pending = std::mem::take(&mut self.pending);
        let blocked = self.blocked;
        self.blocked |= !pending.is_empty();
        self.depth += 1;
        let r = self.list();
        self.depth -= 1;
        self.blocked = blocked;
        // A heredoc opened inside and left unread has its body after the
        // next newline outside.
        let inner = std::mem::replace(&mut self.pending, pending);
        self.pending.extend(inner);
        r
    }

    /// Past the `)` that brings `open` unmatched parentheses to zero
    /// (arithmetic), honoring quotes.
    fn parens(&mut self, mut open: usize) -> Option<()> {
        while open > 0 {
            let ch = self.at(0)?;
            self.i += 1;
            match ch {
                '(' => open += 1,
                ')' => open -= 1,
                '\\' => self.i += 1,
                '\'' => self.single()?,
                '"' => self.double(&mut ScanWord::default())?,
                '`' => self.backquote()?,
                _ => {}
            }
        }
        Some(())
    }

    fn redirect(&mut self) -> Option<()> {
        if matches!(self.at(0), Some('<' | '>')) && self.at(1) == Some('(') {
            self.i += 2;
            return self.subshell();
        }
        const OPS: [&str; 10] = ["<<<", "<<-", "<<", "<>", "<&", "<", ">>", ">|", ">&", ">"];
        let op = OPS
            .iter()
            .find(|op| op.chars().enumerate().all(|(k, oc)| self.at(k) == Some(oc)))?;
        self.i += op.len();
        match *op {
            "<<" | "<<-" => {
                while self.at(0).is_some_and(is_blank) {
                    self.i += 1;
                }
                let (tag, quoted) = read_tag(self.c, &mut self.i)?;
                self.pending.push(Heredoc {
                    tag,
                    quoted,
                    strip_tabs: *op == "<<-",
                    body: None,
                });
                Some(())
            }
            _ => self.target(),
        }
    }

    /// A redirect's target word, or a process substitution.
    fn target(&mut self) -> Option<()> {
        while self.at(0).is_some_and(is_blank) {
            self.i += 1;
        }
        match self.at(0)? {
            '<' | '>' if self.at(1) == Some('(') => {
                self.i += 2;
                self.subshell()
            }
            x if METACHARS.contains(&x) => None,
            _ => self.word().map(drop),
        }
    }

    fn word(&mut self) -> Option<ScanWord> {
        let start = self.i;
        let mut w = ScanWord::default();
        while let Some(ch) = self.at(0) {
            if is_blank(ch) || METACHARS.contains(&ch) {
                break;
            }
            self.i += 1;
            match ch {
                '\'' => {
                    let from = self.i;
                    self.single()?;
                    w.text.extend(&self.c[from..self.i - 1]);
                }
                '"' => self.double(&mut w)?,
                '\\' => {
                    if let Some(n) = self.at(0) {
                        self.i += 1;
                        if n != '\n' {
                            w.text.push(n);
                        }
                    }
                }
                '`' => {
                    self.backquote()?;
                    w.expands = true;
                }
                '$' => self.dollar(&mut w)?,
                '*' | '?' | '[' | '{' => {
                    w.glob = true;
                    w.text.push(ch);
                }
                '~' if self.i - 1 == start => {
                    w.expands = true;
                    w.text.push(ch);
                }
                _ => w.text.push(ch),
            }
        }
        Some(w)
    }

    /// After a `$` outside single quotes.
    fn dollar(&mut self, w: &mut ScanWord) -> Option<()> {
        match self.at(0) {
            Some('(') if self.at(1) == Some('(') => {
                self.i += 2;
                self.parens(2)?;
            }
            Some('(') => {
                self.i += 1;
                self.subshell()?;
            }
            // `${ cmd; }` and `${| cmd; }` run in this shell.
            Some('{') if matches!(self.at(1), Some(' ' | '\t' | '\n' | '|')) => return None,
            Some('{') => {
                self.i += 1;
                self.braces()?;
            }
            Some('\'') => {
                self.i += 1;
                while let Some(ch) = self.at(0) {
                    self.i += 1;
                    match ch {
                        '\\' => self.i += 1,
                        '\'' => return Some(()),
                        _ => w.text.push(ch),
                    }
                }
                return None;
            }
            Some('"') => return Some(()),
            _ => w.text.push('$'),
        }
        w.expands = true;
        Some(())
    }

    /// Past the closing `'`.
    fn single(&mut self) -> Option<()> {
        while self.at(0)? != '\'' {
            self.i += 1;
        }
        self.i += 1;
        Some(())
    }

    /// Past the closing `"`.
    fn double(&mut self, w: &mut ScanWord) -> Option<()> {
        loop {
            let ch = self.at(0)?;
            self.i += 1;
            match ch {
                '"' => return Some(()),
                '\\' => {
                    w.text.push(self.at(0)?);
                    self.i += 1;
                }
                '`' => {
                    self.backquote()?;
                    w.expands = true;
                }
                '$' => self.dollar(w)?,
                _ => w.text.push(ch),
            }
        }
    }

    /// Past the closing backquote.
    fn backquote(&mut self) -> Option<()> {
        loop {
            let ch = self.at(0)?;
            self.i += 1;
            match ch {
                '\\' => self.i += 1,
                '`' => return Some(()),
                _ => {}
            }
        }
    }

    /// Past the `}` closing a `${`.
    fn braces(&mut self) -> Option<()> {
        loop {
            let ch = self.at(0)?;
            self.i += 1;
            match ch {
                '}' => return Some(()),
                '\\' => self.i += 1,
                '\'' => self.single()?,
                '"' => self.double(&mut ScanWord::default())?,
                '`' => self.backquote()?,
                '$' => self.dollar(&mut ScanWord::default())?,
                _ => {}
            }
        }
    }
}

/// A recognized heredoc write with its target as written, and whether that
/// target is literal.
fn heredoc_write(s: &Simple, docs: &[Heredoc], link: StatusLink) -> Option<(HeredocWrite, bool)> {
    if s.herestring {
        return None;
    }
    let [d] = s.heredocs.as_slice() else {
        return None;
    };
    let doc = &docs[*d];
    let body = doc.body.clone()?;
    let stdout = |r: &Redir| matches!(r.fd, None | Some(1)) && matches!(r.op, ">" | ">>" | ">|");
    let words: Vec<&str> = s.words.iter().map(|w| w.text.as_str()).collect();
    let (target, append, via) = match words.as_slice() {
        ["cat"] => {
            let [r] = s.redirs.as_slice() else {
                return None;
            };
            if !stdout(r) {
                return None;
            }
            (&r.target, r.op == ">>", Via::Cat)
        }
        ["tee", rest @ ..] => {
            let (append, file) = match rest {
                [_] => (false, 1),
                [flag, _] if matches!(*flag, "-a" | "--append") => (true, 2),
                _ => return None,
            };
            let w = &s.words[file];
            if w.text.starts_with('-') {
                return None;
            }
            match s.redirs.as_slice() {
                [] => {}
                [r] if stdout(r) && r.target.literal && r.target.text == "/dev/null" => {}
                _ => return None,
            }
            (w, append, Via::Tee)
        }
        _ => return None,
    };
    if target.text.is_empty() {
        return None;
    }
    let write = HeredocWrite {
        path: target.text.clone(),
        append,
        via,
        body,
        tag: doc.tag.clone(),
        tag_quoted: doc.quoted,
        strip_tabs: doc.strip_tabs,
        status_link: link,
    };
    Some((write, target.literal))
}

/// The files a command not otherwise modeled writes: output redirects to
/// a file, and `tee`'s file arguments.
fn unmodeled_targets(s: &Simple, dir: &ScriptDir) -> Vec<UnmodeledTarget> {
    let target = |w: &Word, append: bool, tee: bool| {
        let device = w.literal && w.text.starts_with("/dev/");
        (!w.text.is_empty() && !device).then(|| UnmodeledTarget {
            path: w.text.clone(),
            literal: w.literal,
            resolved: w.literal.then(|| resolve(dir, &w.text)).flatten(),
            append,
            tee,
        })
    };
    let mut out = Vec::new();
    let words = s.words.iter().skip_while(|w| is_assignment(&w.text));
    let mut words = words.peekable();
    if words.next_if(|w| w.literal && w.text == "tee").is_some() {
        let mut flags = true;
        let mut files = Vec::new();
        let mut append = false;
        for w in words {
            match w.text.as_str() {
                "--" if flags => flags = false,
                "--append" if flags => append = true,
                f if flags && f.starts_with('-') && f.len() > 1 => {
                    append |= !f.starts_with("--") && f.contains('a');
                }
                _ => files.push(w),
            }
        }
        out.extend(files.into_iter().filter_map(|w| target(w, append, true)));
    }
    for r in &s.redirs {
        let append = match r.op {
            ">" | ">|" | "&>" => false,
            ">>" | "&>>" => true,
            ">&" if !r
                .target
                .text
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-') =>
            {
                false
            }
            _ => continue,
        };
        out.extend(target(&r.target, append, false));
    }
    out
}

/// `apply_patch <<TAG` / `applypatch <<TAG` with no other word, redirect or
/// here-string.
fn heredoc_patch(
    s: &Simple,
    docs: &[Heredoc],
    dir: &ScriptDir,
    link: StatusLink,
) -> Option<HeredocPatch> {
    let [cmd] = s.words.as_slice() else {
        return None;
    };
    if !matches!(cmd.text.as_str(), "apply_patch" | "applypatch")
        || s.herestring
        || !s.redirs.is_empty()
    {
        return None;
    }
    let [d] = s.heredocs.as_slice() else {
        return None;
    };
    let doc = &docs[*d];
    Some(HeredocPatch {
        dir: dir.clone(),
        command: cmd.text.clone(),
        body: doc.body.clone()?,
        tag: doc.tag.clone(),
        tag_quoted: doc.quoted,
        strip_tabs: doc.strip_tabs,
        status_link: link,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_patch_reads_every_operation() {
        let files = parse_patch(
            "*** Begin Patch\n*** Add File: a.txt\n+one\n+two\n*** Update File: b.rs\n*** Move to: c.rs\n@@\n-x\n+y\n*** End of File\n*** Delete File: d.txt\n*** End Patch\n",
        );
        assert_eq!(files.len(), 3);
        assert_eq!(
            (files[0].op, files[0].added.as_deref()),
            (PatchOp::Add, Some("one\ntwo\n"))
        );
        assert_eq!(
            (files[1].op, files[1].added.as_deref()),
            (PatchOp::Update, None)
        );
        assert_eq!(files[1].move_to.as_deref(), Some("c.rs"));
        assert_eq!(
            (files[2].op, files[2].path.as_str()),
            (PatchOp::Delete, "d.txt")
        );
        assert_eq!(files[2].op.as_str(), "delete");
    }

    #[test]
    fn parse_patch_skips_text_without_file_headers_and_empty_paths() {
        assert!(parse_patch("not a patch\n+x\n").is_empty());
        assert!(parse_patch("*** Begin Patch\n*** Add File: \n+x\n*** End Patch\n").is_empty());
    }

    #[test]
    fn a_heredoc_patch_lists_its_files() {
        let parsed = parse_script(
            "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\nEOF\n",
        );
        let files = parsed.patches().next().unwrap().files();
        assert_eq!(files.len(), 1);
        assert_eq!(
            (files[0].path.as_str(), files[0].added.as_deref()),
            ("a.txt", Some("hi\n"))
        );
    }

    fn writes(cmd: &str) -> Vec<HeredocWrite> {
        parse_script(cmd).writes().cloned().collect()
    }

    fn one(cmd: &str) -> HeredocWrite {
        let w = writes(cmd);
        assert_eq!(w.len(), 1, "{cmd:?}: {w:?}");
        w.into_iter().next().unwrap()
    }

    #[test]
    fn codex_quoted_cat_heredoc() {
        let w = one("cat <<'EOF' > wc.py\nimport sys\nprint(len(sys.argv))\nEOF");
        assert_eq!(
            w,
            HeredocWrite {
                path: "wc.py".into(),
                append: false,
                via: Via::Cat,
                body: "import sys\nprint(len(sys.argv))\n".into(),
                tag: "EOF".into(),
                tag_quoted: true,
                strip_tabs: false,
                status_link: StatusLink::Sole,
            }
        );
        assert!(!w.may_expand());
    }

    #[test]
    fn redirect_before_the_heredoc_and_every_stdout_spelling() {
        for (cmd, path, append) in [
            ("cat > a.txt <<EOF\nx\nEOF\n", "a.txt", false),
            ("cat >a.txt <<EOF\nx\nEOF", "a.txt", false),
            ("cat >| a.txt << EOF\nx\nEOF", "a.txt", false),
            ("cat 1> a.txt <<EOF\nx\nEOF", "a.txt", false),
            ("cat <<\"EOF\" >> a.txt\nx\nEOF", "a.txt", true),
            ("cat >> ./dir/../a.txt <<EOF\nx\nEOF", "a.txt", true),
        ] {
            let w = one(cmd);
            assert_eq!(
                (w.path.as_str(), w.append, w.body.as_str()),
                (path, append, "x\n"),
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn tee_forms() {
        let w = one("tee notes.md <<'EOF'\nhello\nEOF");
        assert_eq!(
            (w.path.as_str(), w.append, w.via),
            ("notes.md", false, Via::Tee)
        );
        let w = one("tee -a notes.md > /dev/null <<'EOF'\nmore\nEOF");
        assert_eq!(
            (w.path.as_str(), w.append, w.body.as_str()),
            ("notes.md", true, "more\n")
        );
        let w = one("tee --append /abs/log <<EOF\nz\nEOF");
        assert_eq!((w.path.as_str(), w.append), ("/abs/log", true));
    }

    #[test]
    fn dash_strips_leading_tabs_from_body_and_terminator() {
        let w = one("cat <<-EOF > t.txt\n\tone\n\t\ttwo\n  three\n\tEOF\n");
        assert_eq!(w.body, "one\ntwo\n  three\n");
        assert!(w.strip_tabs);
    }

    #[test]
    fn unquoted_tag_is_recorded_verbatim_and_flagged_when_it_could_expand() {
        let w = one("cat <<EOF > env.sh\nexport HOME=$HOME\necho `date`\nEOF");
        assert_eq!(w.body, "export HOME=$HOME\necho `date`\n");
        assert!(!w.tag_quoted && w.may_expand());
        let plain = one("cat <<EOF > plain.txt\nno expansions here\nEOF");
        assert!(!plain.tag_quoted && !plain.may_expand());
        for quoted in ["'EOF'", "\"EOF\"", "\\EOF", "E'O'F"] {
            let w = one(&format!("cat <<{quoted} > q.sh\necho $HOME\nEOF"));
            assert!(w.tag_quoted && !w.may_expand(), "{quoted}");
        }
    }

    #[test]
    fn chained_commands_and_several_heredocs() {
        let p = parse_script(
            "mkdir -p src && cat > src/a.py <<'A' && cat > src/b.py <<'B'\na = 1\nA\nb = 2\nB\npython3 src/a.py",
        );
        assert_eq!(p.simple_commands, 4);
        let got: Vec<(&str, &str)> = p
            .writes()
            .map(|w| (w.path.as_str(), w.body.as_str()))
            .collect();
        assert_eq!(got, vec![("src/a.py", "a = 1\n"), ("src/b.py", "b = 2\n")]);
        assert_eq!(
            p.items.first(),
            Some(&ShellItem::Other("mkdir -p src".into()))
        );
        assert_eq!(
            p.items.last(),
            Some(&ShellItem::Other("python3 src/a.py".into()))
        );

        let p = parse_script("cat > a <<EOF\n1\nEOF\ncat >> a <<EOF\n2\nEOF\n");
        let got: Vec<(bool, &str)> = p.writes().map(|w| (w.append, w.body.as_str())).collect();
        assert_eq!(got, vec![(false, "1\n"), (true, "2\n")]);
        assert_eq!(p.simple_commands, 2);
    }

    #[test]
    fn a_single_write_is_the_sole_simple_command() {
        assert_eq!(parse_script("cat > a <<EOF\nx\nEOF").simple_commands, 1);
        assert_eq!(
            parse_script("cat > a <<EOF && ls\nx\nEOF").simple_commands,
            2
        );
        assert_eq!(
            parse_script("false || cat > a <<EOF\nx\nEOF").simple_commands,
            2
        );
    }

    #[test]
    fn literal_cd_moves_relative_targets() {
        assert_eq!(one("cd sub && cat > f <<EOF\nx\nEOF").path, "sub/f");
        assert_eq!(
            one("cd sub && cd ../other && cat > f <<EOF\nx\nEOF").path,
            "other/f"
        );
        assert_eq!(
            one("cd /work/project && cat > f <<EOF\nx\nEOF").path,
            "/work/project/f"
        );
        // A directory change we cannot follow drops relative targets only.
        assert!(writes("cd $X && cat > f <<EOF\nx\nEOF").is_empty());
        assert!(writes("cd - && cat > f <<EOF\nx\nEOF").is_empty());
        assert!(writes("pushd sub && cat > f <<EOF\nx\nEOF").is_empty());
        assert_eq!(one("cd $X && cat > /abs/f <<EOF\nx\nEOF").path, "/abs/f");
    }

    #[test]
    fn non_matching_shell_commands_write_nothing() {
        for cmd in [
            "ls -la",
            "cat wc.py",
            "echo hi > f.txt",
            "printf 'x' >> f.txt",
            "sed -i s/a/b/ f.txt",
            "python3 - <<'EOF'\nopen('f','w').write('x')\nEOF",
            "cat <<'EOF' | sh\necho hi\nEOF",
            "cat <<EOF\nto stdout\nEOF",
            "cat <<< 'here string' > f.txt",
            "cat > $OUT <<EOF\nx\nEOF",
            "cat > *.txt <<EOF\nx\nEOF",
            "cat > ~/f <<EOF\nx\nEOF",
            "cat <<EOF > f.txt 2>/dev/null\nx\nEOF",
            "cat -n > f.txt <<EOF\nx\nEOF",
            "FOO=1 cat > f.txt <<EOF\nx\nEOF",
            "sudo tee f.txt <<EOF\nx\nEOF",
            "tee a.txt b.txt <<EOF\nx\nEOF",
            "tee f.txt <<EOF > out.log\nx\nEOF",
            "cat > f.txt <<EOF\nnever terminated\n",
            "cat > f.txt <<EOF",
            "(cd sub && cat > f.txt <<EOF\nx\nEOF\n)",
            "{ cat > f.txt <<EOF\nx\nEOF\n}",
            "if true; then cat > f.txt <<EOF\nx\nEOF\nfi",
            "echo \"$(cat <<'EOF' > f.txt\nx\nEOF\n)\"",
            "cat > f.txt <<EOF `date`\nx\nEOF",
            "cat > 'unterminated <<EOF\nx\nEOF",
            "# cat > f.txt <<EOF",
        ] {
            assert!(writes(cmd).is_empty(), "{cmd:?}");
        }
    }

    #[test]
    fn claude_code_commit_message_heredoc_writes_nothing() {
        let cmd = "cd /work/project && git add wc.py test_wc.py && git commit -m \"$(cat <<'EOF'\nAdd word counting CLI and tests\n\nCo-Authored-By: Claude Code <dev@example.com>\nEOF\n)\"";
        let p = parse_script(cmd);
        assert_eq!(p.writes().count(), 0);
        assert_eq!(p.items, vec![ShellItem::Other(cmd.into())]);
    }

    #[test]
    fn other_items_carry_their_words_targets_and_bodies() {
        let p = parse_script("python3 wc.py < in.txt && python3 - <<'EOF'\nimport wc\nEOF");
        assert_eq!(
            p.items,
            vec![
                ShellItem::Other("python3 wc.py <in.txt".into()),
                ShellItem::Other("python3 - import wc\n".into()),
            ]
        );
    }

    fn unmodeled(cmd: &str) -> Vec<UnmodeledWrite> {
        parse_script(cmd).unmodeled().cloned().collect()
    }

    fn targets(cmd: &str) -> Vec<(String, Option<String>, bool, bool)> {
        unmodeled(cmd)
            .into_iter()
            .flat_map(|u| u.targets)
            .map(|t| (t.path, t.resolved, t.append, t.tee))
            .collect()
    }

    fn t(
        path: &str,
        resolved: Option<&str>,
        append: bool,
        tee: bool,
    ) -> (String, Option<String>, bool, bool) {
        (path.into(), resolved.map(Into::into), append, tee)
    }

    #[test]
    fn a_cat_with_two_heredocs_is_an_unmodeled_write() {
        let cmd = "cat > f.txt <<A <<B\none\nA\ntwo\nB";
        let p = parse_script(cmd);
        assert_eq!(p.writes().count() + p.unresolved().count(), 0);
        let u = unmodeled(cmd);
        assert_eq!(u.len(), 1, "{p:?}");
        assert_eq!(u[0].program.as_deref(), Some("cat"));
        assert_eq!(u[0].status_link, StatusLink::Sole);
        assert_eq!(u[0].command, "cat >f.txt one\n two\n");
        assert_eq!(targets(cmd), vec![t("f.txt", Some("f.txt"), false, false)]);
    }

    #[test]
    fn a_heredoc_piped_into_tee_is_an_unmodeled_write() {
        let cmd = "cat <<'EOF' | tee -a out/log.md\nhello\nEOF";
        let p = parse_script(cmd);
        assert_eq!(p.items[0], ShellItem::Other("cat hello\n".into()));
        let u = unmodeled(cmd);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].program.as_deref(), Some("tee"));
        assert_eq!(u[0].status_link, StatusLink::Independent);
        assert_eq!(
            targets(cmd),
            vec![t("out/log.md", Some("out/log.md"), true, true)]
        );
    }

    #[test]
    fn unmodeled_targets_are_every_file_a_redirect_or_tee_writes() {
        for (cmd, want) in [
            (
                "echo hi > f.txt",
                vec![t("f.txt", Some("f.txt"), false, false)],
            ),
            (
                "printf x >> f.txt",
                vec![t("f.txt", Some("f.txt"), true, false)],
            ),
            (
                "cargo test &> log.txt 2>&1",
                vec![t("log.txt", Some("log.txt"), false, false)],
            ),
            (
                "make 2>> err.log >&2",
                vec![t("err.log", Some("err.log"), true, false)],
            ),
            (
                "make >&build.log",
                vec![t("build.log", Some("build.log"), false, false)],
            ),
            (
                "cmd >| /abs/x",
                vec![t("/abs/x", Some("/abs/x"), false, false)],
            ),
            (
                "tee a.txt -ia -- -b.txt <<EOF\nx\nEOF",
                vec![
                    t("a.txt", Some("a.txt"), true, true),
                    t("-b.txt", Some("-b.txt"), true, true),
                ],
            ),
            (
                "tee f.txt <<EOF > out.log\nx\nEOF",
                vec![
                    t("f.txt", Some("f.txt"), false, true),
                    t("out.log", Some("out.log"), false, false),
                ],
            ),
            (
                "cat <<EOF > f.txt 2>/dev/null\nx\nEOF",
                vec![t("f.txt", Some("f.txt"), false, false)],
            ),
            (
                "FOO=1 cat > f.txt <<EOF\nx\nEOF",
                vec![t("f.txt", Some("f.txt"), false, false)],
            ),
            ("cmd > \"$OUT\"", vec![t("$OUT", None, false, false)]),
            ("cd $D && cmd > f", vec![t("f", None, false, false)]),
            (
                "cd sub && cmd > ../f",
                vec![t("../f", Some("f"), false, false)],
            ),
        ] {
            assert_eq!(targets(cmd), want, "{cmd:?}");
        }
        for cmd in [
            "ls -la",
            "cat wc.py",
            "cmd > /dev/null 2>&1",
            "cmd >&2",
            "cmd 2>&-",
            "tee <<EOF\nx\nEOF",
            "sort < in.txt",
            "sudo tee f.txt <<EOF\nx\nEOF",
            "(echo hi > f.txt)",
            "if true; then echo hi > f.txt; fi",
            "cat > f.txt <<EOF\nx\nEOF",
            "cat > $OUT <<EOF\nx\nEOF",
        ] {
            assert!(
                unmodeled(cmd).is_empty(),
                "{cmd:?}: {:?}",
                parse_script(cmd)
            );
        }
        assert_eq!(Unresolvable::Unmodeled.as_str(), "unmodeled");
    }

    #[test]
    fn comments_and_line_continuations() {
        let w =
            one("# write the file\ncat \\\n  > a.txt <<'EOF' # trailing\nx # not a comment\nEOF");
        assert_eq!(
            (w.path.as_str(), w.body.as_str()),
            ("a.txt", "x # not a comment\n")
        );
    }

    const PATCH: &str = "*** Begin Patch\n*** Add File: hello.txt\n+hi\n*** End Patch\n";

    fn patches(cmd: &str) -> Vec<HeredocPatch> {
        parse_script(cmd).patches().cloned().collect()
    }

    #[test]
    fn codex_apply_patch_heredoc_is_a_patch() {
        let p = parse_script(&format!("apply_patch <<'EOF'\n{PATCH}EOF\n"));
        assert_eq!(p.simple_commands, 1);
        assert_eq!(p.writes().count(), 0);
        assert_eq!(
            p.items,
            vec![ShellItem::Patch(HeredocPatch {
                dir: ScriptDir::Start,
                command: "apply_patch".into(),
                body: PATCH.into(),
                tag: "EOF".into(),
                tag_quoted: true,
                strip_tabs: false,
                status_link: StatusLink::Sole,
            })]
        );
        let p = &patches(&format!("applypatch <<PATCH\n{PATCH}PATCH"))[0];
        assert_eq!((p.command.as_str(), p.tag_quoted), ("applypatch", false));
    }

    #[test]
    fn a_patch_after_cd_resolves_relative_paths_against_it() {
        let p = &patches(&format!("cd sub && apply_patch <<'EOF'\n{PATCH}EOF"))[0];
        assert_eq!(p.dir, ScriptDir::At("sub".into()));
        assert_eq!(
            p.resolve("a/../hello.txt").as_deref(),
            Some("sub/hello.txt")
        );
        assert_eq!(p.resolve("/abs/x").as_deref(), Some("/abs/x"));
        let p = &patches(&format!("cd /w && apply_patch <<'EOF'\n{PATCH}EOF"))[0];
        assert_eq!(p.resolve("hello.txt").as_deref(), Some("/w/hello.txt"));
        let p = &patches(&format!("apply_patch <<'EOF'\n{PATCH}EOF"))[0];
        assert_eq!(p.resolve("./hello.txt").as_deref(), Some("hello.txt"));
        assert_eq!(
            parse_script(&format!("cd sub && apply_patch <<'EOF'\n{PATCH}EOF")).simple_commands,
            2
        );
    }

    #[test]
    fn other_apply_patch_spellings_are_not_patches() {
        for cmd in [
            format!("apply_patch --dry-run <<'EOF'\n{PATCH}EOF"),
            format!("apply_patch <<'EOF' > out.log\n{PATCH}EOF"),
            format!("apply_patch <<'EOF'\n{PATCH}"),
            "apply_patch <<< '*** Begin Patch'".to_string(),
            "apply_patch patch.txt".to_string(),
            format!("echo \"$(apply_patch <<'EOF'\n{PATCH}EOF\n)\""),
            format!("git apply <<'EOF'\n{PATCH}EOF"),
        ] {
            assert!(patches(&cmd).is_empty(), "{cmd:?}");
            assert!(parse_script(&cmd).writes().next().is_none(), "{cmd:?}");
        }
    }

    #[test]
    fn a_patch_after_an_unknown_directory_change_resolves_only_absolute_paths() {
        for cmd in [
            format!("cd $X && apply_patch <<'EOF'\n{PATCH}EOF"),
            format!("cd sub; apply_patch <<'EOF'\n{PATCH}EOF"),
        ] {
            let p = &patches(&cmd)[0];
            assert_eq!(p.dir, ScriptDir::Unknown, "{cmd:?}");
            assert_eq!(p.resolve("hello.txt"), None);
            assert_eq!(p.resolve("/abs/../x").as_deref(), Some("/x"));
        }
    }

    #[test]
    fn status_links() {
        let link = |cmd: &str| {
            let p = parse_script(cmd);
            p.items
                .iter()
                .find_map(|i| match i {
                    ShellItem::Write(w) => Some(w.status_link),
                    ShellItem::Patch(p) => Some(p.status_link),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{cmd:?}: {p:?}"))
        };
        for (cmd, want) in [
            ("cat > a <<EOF\nx\nEOF", StatusLink::Sole),
            ("cat > a <<EOF;\nx\nEOF", StatusLink::Sole),
            ("cat > a <<EOF &\nx\nEOF", StatusLink::Independent),
            ("cat > a <<EOF || true\nx\nEOF", StatusLink::Independent),
            ("false || cat > a <<EOF\nx\nEOF", StatusLink::Independent),
            ("cat > a <<EOF | tee log\nx\nEOF", StatusLink::Independent),
            ("cat > a <<EOF\nx\nEOF\nls", StatusLink::Independent),
            (
                "cat > a <<EOF && ls || true\nx\nEOF",
                StatusLink::Independent,
            ),
            (
                "mkdir -p d && cat > d/a <<EOF\nx\nEOF",
                StatusLink::ImpliedBySuccess,
            ),
            ("cat > a <<EOF && ls\nx\nEOF", StatusLink::ImpliedBySuccess),
            (
                "ls; cat > a <<EOF && ls\nx\nEOF",
                StatusLink::ImpliedBySuccess,
            ),
            ("cat > a <<EOF &&\nx\nEOF\nls", StatusLink::ImpliedBySuccess),
            (
                "cd sub && cat > a <<EOF\nx\nEOF",
                StatusLink::ImpliedBySuccess,
            ),
            (
                "apply_patch <<'EOF'\n*** Begin Patch\n*** End Patch\nEOF",
                StatusLink::Sole,
            ),
        ] {
            assert_eq!(link(cmd), want, "{cmd:?}");
        }
        assert_eq!(parse_script("cat > a <<EOF &\nx\nEOF").simple_commands, 1);
    }

    #[test]
    fn an_unquoted_body_continues_lines_before_the_terminator_test() {
        let cmd = "cat <<EOF > f\na\\\nEOF\nEOF\n";
        assert_eq!(parse_script(cmd).items.len(), 1);
        let w = one(cmd);
        assert_eq!(w.body, "a\\\nEOF\n");
        assert!(w.may_expand());
        let p = parse_script("cat <<'EOF' > f\na\\\nEOF\nEOF\n");
        assert_eq!(p.writes().next().unwrap().body, "a\\\n");
        assert_eq!(p.items.last(), Some(&ShellItem::Other("EOF".into())));
        let p = parse_script("cat <<EOF > f\nx\n\\\nEOF\nls");
        assert_eq!(p.writes().next().unwrap().body, "x\n");
        assert_eq!(p.items.last(), Some(&ShellItem::Other("ls".into())));
        assert_eq!(one("cat <<EOF > f\na\\\\\nEOF\n").body, "a\\\\\n");
        assert_eq!(
            parse_script("cat <<-EOF > f\n\ta\\\n\tEOF\n\tEOF\n").simple_commands,
            0
        );
    }

    #[test]
    fn carriage_returns_are_word_characters() {
        let w = one("cat > f <<'EOF'\r\nx\r\nEOF\r\n");
        assert_eq!(
            (w.path.as_str(), w.tag.as_str(), w.body.as_str()),
            ("f", "EOF\r", "x\r\n")
        );
        assert!(writes("cd sub\r && cat > f <<'EOF'\nx\nEOF").is_empty());
    }

    #[test]
    fn brace_expansion_is_not_literal() {
        for cmd in [
            "tee {a,b}.txt <<EOF\nx\nEOF",
            "cat > {a,b} <<EOF\nx\nEOF",
            "cat > a{1..3} <<EOF\nx\nEOF",
            "cd {a,b} && cat > f <<EOF\nx\nEOF",
        ] {
            assert!(writes(cmd).is_empty(), "{cmd:?}");
        }
        assert_eq!(one("cat > '{a,b}.txt' <<EOF\nx\nEOF").path, "{a,b}.txt");
    }

    #[test]
    fn a_cd_holds_only_along_an_unbroken_and_chain() {
        for cmd in [
            "cd sub; cat > f <<EOF\nx\nEOF",
            "cd sub\ncat > f <<EOF\nx\nEOF",
            "cd sub && ls; cat > f <<EOF\nx\nEOF",
            "cd sub && ls || true && cat > f <<EOF\nx\nEOF",
            "false || cd sub && cat > f <<EOF\nx\nEOF",
            "cd sub | cat && cat > f <<EOF\nx\nEOF",
            "cd sub; cd ../other && cat > f <<EOF\nx\nEOF",
        ] {
            assert!(writes(cmd).is_empty(), "{cmd:?}");
        }
        let got: Vec<String> = writes("cd sub && cat > a <<A && cat > b <<B\n1\nA\n2\nB")
            .into_iter()
            .map(|w| w.path)
            .collect();
        assert_eq!(got, ["sub/a", "sub/b"]);
        assert_eq!(one("cd sub; cat > /abs/f <<EOF\nx\nEOF").path, "/abs/f");
        assert_eq!(
            one("cd sub; cd /abs && cat > f <<EOF\nx\nEOF").path,
            "/abs/f"
        );
    }

    #[test]
    fn other_directory_changes_make_relative_targets_unresolvable() {
        for cmd in [
            "FOO=1 cd sub && cat > f <<EOF\nx\nEOF",
            "source env.sh && cat > f <<EOF\nx\nEOF",
            ". ./env.sh && cat > f <<EOF\nx\nEOF",
            "eval cd sub && cat > f <<EOF\nx\nEOF",
            "builtin cd sub && cat > f <<EOF\nx\nEOF",
        ] {
            assert!(writes(cmd).is_empty(), "{cmd:?}");
        }
        assert_eq!(
            one("source env.sh && cat > /abs/f <<EOF\nx\nEOF").path,
            "/abs/f"
        );
    }

    #[test]
    fn may_change_dir() {
        for cmd in [
            "cd sub",
            "cd $X && ls",
            "pushd sub",
            "ls; popd",
            "source x.sh",
            "FOO=1 cd sub",
            "if true; then cd sub; fi",
            "{ cd sub; }",
            "f() { cd sub; }; f",
            "exec 3>&1; cd sub",
            "time -p cd sub",
            "ls | cd sub",
            "trap 'cd /' EXIT",
            "alias go='cd sub'",
            "case x in a) cd sub;; esac",
            "function f { cd sub; }",
            "echo ${ cd sub; }",
            "echo ${| cd sub; }",
            "echo \"$(ls\" )",
            "echo $(ls",
            "ls )",
            "(cat <<EOF) > o\nit's\nEOF\ncd sub; echo don\\'t",
            "echo $(cat <<EOF)\nit's\nEOF\ncd sub; echo don\\'t",
        ] {
            assert!(parse_script(cmd).may_change_dir, "{cmd:?}");
        }
        for cmd in [
            "ls",
            "git add .",
            "echo cd",
            "cat > f <<EOF\ncd sub\nEOF",
            "(cd sub && make)",
            "echo \"$(cd sub && pwd)\"",
            "echo `cd sub`",
            "diff <(cd a && ls) >(cd b)",
            "x=$(cd sub; pwd) y=`cd x`",
            "arr=(\"a)\" cd) && ls",
            "(( x++ )) && echo $(( 1 << 2 ))",
            "echo ${x:-$(cd sub)} '$(cd' \"it's\" $'a\\'b'",
            "echo a # ; cd sub",
            "git commit -m \"$(cat <<'EOF'\nFix parser (cd into dirs)\nEOF\n)\"",
            "git commit -m \"$(cat <<'EOF'\nAdd flag\n\ncommand line parsing moved\nEOF\n)\"",
            "git commit -m \"$(cat <<'EOF'\nsource maps are emitted\n) it's\nEOF\n)\" && git log",
            "[ -f x ] && ls",
        ] {
            assert!(!parse_script(cmd).may_change_dir, "{cmd:?}");
        }
    }

    #[test]
    fn a_command_word_that_is_not_literal_may_change_dir() {
        for cmd in [
            "$CD sub",
            "\"$CD\" sub",
            "$(echo cd) sub",
            "`echo cd` sub",
            "~/bin/cd sub",
            "c? sub",
            "x=$(pwd); \\cd sub",
            "echo \"$(ls)\"; \\cd sub",
        ] {
            let p = parse_script(cmd);
            assert!(p.may_change_dir, "{cmd:?}");
            assert_eq!(p.dir_changes, [ScriptDir::Unknown], "{cmd:?}");
        }
        let u = parse_script("\"$CD\" sub && cat > f <<EOF\nx\nEOF");
        assert_eq!(u.writes().count(), 0);
        assert_eq!(
            u.unresolved().map(|u| u.reason).collect::<Vec<_>>(),
            [Unresolvable::UnknownDir]
        );
        assert_eq!(one("[ -f x ] && cat > f <<EOF\nx\nEOF").path, "f");
    }

    #[test]
    fn dir_changes_name_literal_absolute_cds() {
        let at = |d: &str| ScriptDir::At(d.to_string());
        for (cmd, want) in [
            ("ls", vec![]),
            (
                "cd /w && git add a && git commit -m \"$(cat <<'EOF'\nFix foo\nEOF\n)\"",
                vec![at("/w")],
            ),
            ("cd /w/./x/.. && cargo test 2>&1 | tail -5", vec![at("/w")]),
            ("cd /a; ls; cd /b", vec![at("/a"), at("/b")]),
            ("cd /a && cat > f <<EOF\nx\nEOF", vec![at("/a")]),
            ("FOO=1 cd /a", vec![at("/a")]),
            ("cd sub", vec![ScriptDir::Unknown]),
            ("cd", vec![ScriptDir::Unknown]),
            ("cd -", vec![ScriptDir::Unknown]),
            ("cd /a /b", vec![ScriptDir::Unknown]),
            ("cd $HOME", vec![ScriptDir::Unknown]),
            ("cd /a*", vec![ScriptDir::Unknown]),
            ("pushd /a", vec![ScriptDir::Unknown]),
            ("cd /a && source x.sh", vec![at("/a"), ScriptDir::Unknown]),
        ] {
            let p = parse_script(cmd);
            assert_eq!(p.dir_changes, want, "{cmd:?}");
            assert_eq!(p.may_change_dir, !want.is_empty(), "{cmd:?}");
        }
        assert!(parse_argv(&["apply_patch", "x"]).dir_changes.is_empty());
        assert_eq!(
            parse_argv(&["bash", "-lc", "cd /w && ls"]).dir_changes,
            [at("/w")]
        );
    }

    #[test]
    fn dir_on_success_follows_only_cds_that_success_implies_ran() {
        let at = |d: &str| ScriptDir::At(d.to_string());
        for (cmd, want) in [
            ("ls", ScriptDir::Start),
            ("cat > f <<EOF\nx\nEOF", ScriptDir::Start),
            ("(cd sub && make)", ScriptDir::Start),
            ("cd sub", at("sub")),
            ("cd sub\n", at("sub")),
            ("cd sub && cat > f <<EOF\nx\nEOF", at("sub")),
            ("cd sub && cd ../other/./x/..", at("other")),
            ("cd ..", at("..")),
            ("cd sub && cd /abs/d", at("/abs/d")),
            ("make && cd sub", at("sub")),
            ("cd $X && cd /abs", at("/abs")),
            ("cd sub; ls", ScriptDir::Unknown),
            ("ls; cd sub", ScriptDir::Unknown),
            ("false || cd sub", ScriptDir::Unknown),
            ("cd sub || true", ScriptDir::Unknown),
            ("cd sub && ls || true", ScriptDir::Unknown),
            ("cd sub | cat", ScriptDir::Unknown),
            ("cd sub &", ScriptDir::Unknown),
            ("cd", ScriptDir::Unknown),
            ("cd -", ScriptDir::Unknown),
            ("cd +1", ScriptDir::Unknown),
            ("cd +0 && ls", ScriptDir::Unknown),
            ("cd -2", ScriptDir::Unknown),
            ("cd ~/x", ScriptDir::Unknown),
            ("cd $HOME", ScriptDir::Unknown),
            ("cd -P sub", ScriptDir::Unknown),
            ("pushd sub", ScriptDir::Unknown),
            ("FOO=1 cd sub", ScriptDir::Unknown),
            ("builtin cd sub", ScriptDir::Unknown),
            ("cd sub && source x.sh", ScriptDir::Unknown),
            ("cd /abs && $CMD", ScriptDir::Unknown),
            ("if true; then cd sub; fi", ScriptDir::Unknown),
            ("cd sub && echo $(pwd)", ScriptDir::Unknown),
        ] {
            assert_eq!(parse_script(cmd).dir_on_success, want, "{cmd:?}");
        }
        assert_eq!(
            parse_argv(&["apply_patch", "x"]).dir_on_success,
            ScriptDir::Start
        );
        assert_eq!(
            parse_argv(&["bash", "-lc", "cd sub && ls"]).dir_on_success,
            at("sub")
        );
    }

    #[test]
    fn a_zsh_directory_stack_cd_is_not_a_path() {
        let p = parse_script("cd +1 && cat > f <<EOF\nx\nEOF");
        assert_eq!(p.writes().count(), 0);
        assert_eq!(
            p.unresolved().next().unwrap().reason,
            Unresolvable::UnknownDir
        );
    }

    #[test]
    fn command_words_are_each_simple_command_s_literal_words() {
        let words = |cmd: &str| parse_script(cmd).command_words;
        assert_eq!(
            words("cd sub && FOO=1 grep -rn x . 2>/dev/null | head -3"),
            [
                vec!["cd", "sub"],
                vec!["grep", "-rn", "x", "."],
                vec!["head", "-3"]
            ]
        );
        assert_eq!(
            words("cat > f <<'EOF'\ngrep x\nEOF\ngit -C d diff"),
            [vec!["cat"], vec!["git", "-C", "d", "diff"]]
        );
        assert_eq!(words("\"$CMD\" x"), [vec!["$CMD", "x"]]);
        assert!(words("if true; then grep x; fi").is_empty(), "not split");
        assert_eq!(
            parse_argv(&["bash", "-lc", "test -f x"]).command_words,
            [vec!["test", "-f", "x"]]
        );
        assert!(parse_argv(&["apply_patch", "x"]).command_words.is_empty());
    }

    #[test]
    fn unresolvable_targets_are_reported_with_their_reason() {
        for (cmd, target, reason) in [
            ("cat > $OUT <<EOF\nx\nEOF", "$OUT", Unresolvable::NotLiteral),
            (
                "tee -a {a,b}.txt <<EOF\nx\nEOF",
                "{a,b}.txt",
                Unresolvable::NotLiteral,
            ),
            ("cat > ~/f <<EOF\nx\nEOF", "~/f", Unresolvable::NotLiteral),
            (
                "cat > *.txt <<EOF\nx\nEOF",
                "*.txt",
                Unresolvable::NotLiteral,
            ),
            (
                "cd $X && cat > f <<EOF\nx\nEOF",
                "f",
                Unresolvable::UnknownDir,
            ),
            (
                "cd sub; cat >> ./f <<EOF\nx\nEOF",
                "./f",
                Unresolvable::UnknownDir,
            ),
            (
                "source e.sh && tee f <<EOF\nx\nEOF",
                "f",
                Unresolvable::UnknownDir,
            ),
        ] {
            let p = parse_script(cmd);
            assert_eq!(p.writes().count(), 0, "{cmd:?}");
            let u: Vec<&UnresolvedWrite> = p.unresolved().collect();
            assert_eq!(u.len(), 1, "{cmd:?}: {p:?}");
            assert_eq!(
                (
                    u[0].write.path.as_str(),
                    u[0].reason,
                    u[0].write.body.as_str()
                ),
                (target, reason, "x\n"),
                "{cmd:?}"
            );
        }
        assert_eq!(Unresolvable::NotLiteral.as_str(), "not_literal");
        assert_eq!(Unresolvable::UnknownDir.as_str(), "unknown_dir");
        let p = parse_script("cat > $OUT <<EOF || true\nx\nEOF");
        let u = p.unresolved().next().unwrap();
        assert_eq!(u.write.status_link, StatusLink::Independent);
        for cmd in [
            "FOO=1 cat > f <<EOF\nx\nEOF",
            "tee a b <<EOF\nx\nEOF",
            "cat > f",
        ] {
            assert_eq!(parse_script(cmd).unresolved().count(), 0, "{cmd:?}");
        }
    }

    #[test]
    fn parse_argv_reads_a_script_only_under_a_shell() {
        let script = "cat > f <<EOF\nx\nEOF";
        for argv in [
            vec!["bash", "-lc", script],
            vec!["/bin/zsh", "-c", script],
            vec!["sh", "-e", "-c", script],
            vec!["bash", "-c", script, "name", "arg"],
        ] {
            assert_eq!(parse_argv(&argv), parse_script(script), "{argv:?}");
        }
        for argv in [
            vec!["python3", "-c", script],
            vec!["node", "-e", script],
            vec!["bash", script],
            vec!["bash", "-o", "pipefail", "-c", script],
            vec!["ls", "-a"],
        ] {
            let p = parse_argv(&argv);
            assert_eq!(p.items, vec![ShellItem::Other(argv.join(" "))], "{argv:?}");
            assert_eq!(p.simple_commands, 0);
            assert!(!p.may_change_dir);
        }
        assert!(!parse_argv(&["cd", "sub"]).may_change_dir);
        let p = parse_argv(&["apply_patch", PATCH]);
        assert_eq!(p.simple_commands, 1);
        assert_eq!(
            p.items,
            vec![ShellItem::Patch(HeredocPatch {
                dir: ScriptDir::Start,
                command: "apply_patch".into(),
                body: PATCH.into(),
                tag: String::new(),
                tag_quoted: true,
                strip_tabs: false,
                status_link: StatusLink::Sole,
            })]
        );
    }

    #[test]
    fn writes_and_patches_share_one_script() {
        let cmd = format!("cat > a.txt <<'A'\nx\nA\napply_patch <<'EOF'\n{PATCH}EOF\nls");
        let p = parse_script(&cmd);
        assert_eq!(p.simple_commands, 3);
        assert_eq!(p.writes().count(), 1);
        assert_eq!(p.patches().count(), 1);
        assert_eq!(p.items.last(), Some(&ShellItem::Other("ls".into())));
    }

    #[test]
    fn normalize_is_lexical() {
        for (p, want) in [
            ("a/./b//c", "a/b/c"),
            ("./a", "a"),
            ("a/../../b", "../b"),
            ("/a/../../b", "/b"),
            ("/", "/"),
            (".", "."),
        ] {
            assert_eq!(normalize_path(p), want, "{p}");
        }
    }
}
