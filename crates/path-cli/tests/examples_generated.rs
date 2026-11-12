//! The documents in `examples/` that a producer can make are made by one: this
//! test runs the producer on a real input and fails when a committed example
//! differs from what it produces. `UPDATE_EXAMPLES=1` writes them instead
//! (`scripts/gen-examples.sh`).
//!
//! - `step-01-minimal.json` and `path-01-pr.path.json`: `path p track`, driven
//!   step by step over the contents of one file, with fixed ids and times.
//! - `path-02-local-session.path.json`: the document `path p derive claude`
//!   makes from the real recorded session `test-fixtures/claude/convo.jsonl`
//!   (the goldens pipeline, in process).
//! - `graph-01-release.json`: `path p merge` over the documents of three real
//!   recorded sessions (Codex, Gemini CLI, Pi).
//! - the `.path.jsonl` twins of the generated paths: the streaming writer.
//!
//! The rest are illustrative and say so in `examples/README.md`: no producer
//! emits structural AST operations, signatures, merge steps or step fragments.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toolpath_goldens::model::Set;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Run `path` with `stdin`, failing on a non-zero exit; stdout trimmed.
fn path(args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_path"))
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn path");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("path ran");
    assert!(
        out.status.success(),
        "path {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf-8")
        .trim_end()
        .to_string()
}

/// A `path p track` session over one file: `init` with the first content, then
/// one step per `(seq, parent_seq, actor, time, content, intent)`.
struct Track {
    session: String,
}

impl Track {
    fn init(dir: &Path, id: &str, file: &str, actor: &str, extra: &[&str], content: &str) -> Track {
        let mut args = vec![
            "p",
            "track",
            "init",
            "--id",
            id,
            "--file",
            file,
            "--actor",
            actor,
            "--session-dir",
            dir.to_str().expect("utf-8 dir"),
        ];
        args.extend_from_slice(extra);
        Track {
            session: path(&args, content),
        }
    }

    fn step(
        &self,
        seq: u64,
        parent: u64,
        actor: &str,
        time: &str,
        content: &str,
        intent: Option<&str>,
    ) {
        let (seq, parent) = (seq.to_string(), parent.to_string());
        path(
            &[
                "p",
                "track",
                "step",
                "--session",
                &self.session,
                "--seq",
                &seq,
                "--parent-seq",
                &parent,
                "--actor",
                actor,
                "--time",
                time,
            ],
            content,
        );
        if let Some(intent) = intent {
            path(
                &[
                    "p",
                    "track",
                    "note",
                    "--session",
                    &self.session,
                    "--intent",
                    intent,
                ],
                "",
            );
        }
    }

    fn annotate(&self, step: &str, refs: &[&str]) {
        let mut args = vec![
            "p",
            "track",
            "annotate",
            "--session",
            &self.session,
            "--step",
            step,
        ];
        for r in refs {
            args.extend_from_slice(&["--ref", r]);
        }
        path(&args, "");
    }

    fn export(&self) -> String {
        path(
            &[
                "p",
                "--pretty",
                "track",
                "export",
                "--session",
                &self.session,
            ],
            "",
        )
    }
}

const MAIN_BEFORE: &str = "fn main() {\n    println!(\"Hello world\");\n}\n";
const MAIN_AFTER: &str = "fn main() {\n    println!(\"Hello, world!\");\n}\n";

fn step_01_minimal(dir: &Path) -> String {
    let t = Track::init(
        dir,
        "path-step-001",
        "src/main.rs",
        "human:alex",
        &[],
        MAIN_BEFORE,
    );
    t.step(1, 0, "human:alex", "2026-01-29T10:00:00Z", MAIN_AFTER, None);
    t.export()
}

const V0: &str = "";
const V1_SCAFFOLD: &str = "pub fn validate_email(input: &str) -> bool {\n    todo!()\n}\n";
const V2_REGEX: &str = "use regex::Regex;\n\npub fn validate_email(input: &str) -> bool {\n    Regex::new(r\"^[^@]+@[^@]+$\").unwrap().is_match(input)\n}\n";
const V3_TYPED: &str = "#[derive(Debug)]\npub struct ValidationError { pub field: String, pub message: String }\n\npub fn validate_email(input: &str) -> Result<(), ValidationError> {\n    if !input.contains('@') { return Err(ValidationError { field: \"email\".to_string(), message: \"invalid\".to_string() }); }\n    Ok(())\n}\n";
const V4_FORMATTED: &str = "#[derive(Debug)]\npub struct ValidationError {\n    pub field: String,\n    pub message: String,\n}\n\npub fn validate_email(input: &str) -> Result<(), ValidationError> {\n    if !input.contains('@') {\n        return Err(ValidationError {\n            field: \"email\".to_string(),\n            message: \"invalid\".to_string(),\n        });\n    }\n    Ok(())\n}\n";
const V5_REFINED: &str = "#[derive(Debug)]\npub struct ValidationError {\n    pub field: String,\n    pub message: String,\n}\n\npub fn validate_email(input: &str) -> Result<(), ValidationError> {\n    if !input.contains('@') {\n        return Err(ValidationError {\n            field: \"email\".to_string(),\n            message: \"must contain @\".to_string(),\n        });\n    }\n    Ok(())\n}\n";

/// A PR with a dead end: the agent's regex attempt (step-002) forks from the
/// scaffold and is abandoned; the typed version is formatted and refined.
fn path_01_pr(dir: &Path) -> String {
    let t = Track::init(
        dir,
        "path-pr-42",
        "src/auth/validator.rs",
        "human:alex",
        &[
            "--title",
            "Add email validation",
            "--source",
            "github:myorg/myrepo/pull/42",
            "--base-uri",
            "github:myorg/myrepo",
            "--base-ref",
            "abc123def456789",
            "--base-branch",
            "main",
        ],
        V0,
    );
    t.step(
        1,
        0,
        "human:alex",
        "2026-01-29T10:00:00Z",
        V1_SCAFFOLD,
        Some("Scaffold the validator"),
    );
    t.annotate(
        "step-001",
        &[r#"{"rel":"fixes","href":"issue://github/myorg/myrepo/issues/42"}"#],
    );
    let agent = "agent:claude-code/session-abc123";
    t.step(
        2,
        1,
        agent,
        "2026-01-29T10:03:00Z",
        V2_REGEX,
        Some("Regex-based validation (abandoned)"),
    );
    t.step(
        3,
        1,
        agent,
        "2026-01-29T10:05:00Z",
        V3_TYPED,
        Some("Add email validation with custom error type"),
    );
    t.step(
        4,
        3,
        "tool:rustfmt/1.7.0",
        "2026-01-29T10:05:30Z",
        V4_FORMATTED,
        Some("Auto-format"),
    );
    t.step(
        5,
        4,
        "human:alex",
        "2026-01-29T10:15:00Z",
        V5_REFINED,
        Some("Refine error messages"),
    );
    t.export()
}

/// The document `path p derive` makes from a real recorded session.
fn derived(fixture: &str, scratch: &Path) -> serde_json::Value {
    let input = std::fs::read(root().join(fixture)).expect("read fixture");
    let set = Set::new(fixture, input).expect("known agent");
    toolpath_goldens::pipeline::run(&set, scratch)
        .unwrap_or_else(|e| panic!("{fixture}: {e}"))
        .document
}

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).expect("serialize")
}

fn path_02_local_session(scratch: &Path) -> String {
    pretty(&derived("test-fixtures/claude/convo.jsonl", scratch))
}

fn graph_01_release(dir: &Path, scratch: &Path) -> String {
    let mut files = Vec::new();
    for (name, fixture) in [
        ("codex", "test-fixtures/codex/convo.jsonl"),
        ("gemini", "test-fixtures/gemini/convo.jsonl"),
        ("pi", "test-fixtures/pi/convo.jsonl"),
    ] {
        let file = dir.join(format!("{name}.path.json"));
        std::fs::write(&file, pretty(&derived(fixture, scratch))).expect("write input");
        files.push(file.to_str().expect("utf-8").to_string());
    }
    let mut args = vec![
        "p",
        "--pretty",
        "merge",
        "--title",
        "Three harnesses, one graph",
    ];
    args.extend(files.iter().map(String::as_str));
    path(&args, "")
}

/// The streaming twin of a single-path document, by the JSONL writer; with
/// `sort`, each line's keys sorted (see [`sorted`]).
fn jsonl_twin(document: &str, sort: bool) -> String {
    let graph: toolpath::v1::Graph = serde_json::from_str(document).expect("graph");
    let jsonl = graph.to_jsonl_string().expect("jsonl");
    if !sort {
        return jsonl;
    }
    jsonl
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
            serde_json::to_string(&v).expect("serialize")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A producer's JSON with object keys sorted. A derived session's types carry
/// hash maps whose key order changes per process (the goldens sort them the
/// same way); key order is not part of the format, so this changes no content.
/// The `path p track` examples need none: they are byte for byte its output.
fn sorted(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("producer output is JSON");
    pretty(&v)
}

#[test]
fn generated_examples_match_their_producers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let scratch = tempfile::tempdir().expect("scratch");
    let step01 = step_01_minimal(tmp.path());
    let path01 = path_01_pr(tmp.path());
    let path02 = sorted(&path_02_local_session(scratch.path()));
    let graph01 = sorted(&graph_01_release(tmp.path(), scratch.path()));
    let generated = [
        ("step-01-minimal.json", step01),
        ("path-01-pr.path.jsonl", jsonl_twin(&path01, false)),
        ("path-01-pr.path.json", path01),
        (
            "path-02-local-session.path.jsonl",
            jsonl_twin(&path02, true),
        ),
        ("path-02-local-session.path.json", path02),
        ("graph-01-release.json", graph01),
    ];

    let update = std::env::var("UPDATE_EXAMPLES").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut stale = Vec::new();
    for (name, content) in generated {
        let file = root().join("examples").join(name);
        let want = format!("{}\n", content.trim_end());
        if update {
            std::fs::write(&file, &want).unwrap_or_else(|e| panic!("write {name}: {e}"));
        } else if std::fs::read_to_string(&file).ok().as_deref() != Some(want.as_str()) {
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "examples differ from what their producers make now: {stale:?}\n\
         regenerate with scripts/gen-examples.sh (UPDATE_EXAMPLES=1) and review the diff"
    );
}
