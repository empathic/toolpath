//! `path p import otel` over binary inputs: OTLP protobuf bodies
//! (`.binpb`, `.pb`, `.protobuf`), Collector length-prefixed frames, and zstd
//! (`.zst`), read by extension from a directory and named directly as a file.
//! The expected documents always come from importing a JSON form of the same
//! data through the same binary.

use assert_cmd::Command;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Output;

const PROVIDERS: [&str; 4] = ["openai-chat", "openai-responses", "anthropic", "gemini"];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

/// `path p import otel --no-cache --input <input>` sandboxed under a scratch
/// home (cache and config, as `tests/otel.rs` does), so no test reads or
/// writes the real `~/.toolpath`.
fn import(input: &Path) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("path")
        .unwrap()
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", home.path().join(".toolpath"))
        .args(["p", "import", "otel", "--no-cache", "--input"])
        .arg(input)
        .output()
        .unwrap()
}

/// The documents a successful import prints (one per stdout line), sorted by
/// their JSON text so that the order in which sessions are emitted cannot
/// decide a comparison. An import that fails or prints nothing fails the test.
fn docs(input: &Path) -> Vec<Value> {
    let out = import(input);
    assert!(
        out.status.success(),
        "{}: {}",
        input.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut docs: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    docs.sort_by_key(|d| d.to_string());
    assert!(!docs.is_empty(), "{}: no documents", input.display());
    docs
}

/// The files of `dir` whose extension is `ext`, paired with their names.
fn files_with_ext(dir: &Path, ext: &str) -> Vec<(PathBuf, String)> {
    let mut files: Vec<(PathBuf, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .map(|p| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            (p, name)
        })
        .collect();
    files.sort();
    files
}

/// A scratch directory holding a copy of each `(source, name)` under `name`.
fn dir_with(files: &[(PathBuf, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (from, name) in files {
        std::fs::copy(from, dir.path().join(name)).unwrap();
    }
    dir
}

#[test]
fn capture_directory_mixing_binpb_and_json_imports_like_either_encoding() {
    for p in PROVIDERS {
        let capture = fixtures().join("semconv").join(p).join("event");
        let binpb = files_with_ext(&capture, "binpb");
        assert_eq!(binpb.len(), 2, "{p}: traces.binpb and logs.binpb");
        // `.json` includes manifest.json and expected.json: the directory
        // rule skips them as not-otlp, as it does in the capture itself.
        let json_only = dir_with(&files_with_ext(&capture, "json"));
        let binpb_only = dir_with(&binpb);
        let want = docs(json_only.path());
        assert_eq!(
            docs(binpb_only.path()),
            want,
            "{p}: .binpb traces and logs only"
        );
        assert_eq!(
            docs(&capture),
            want,
            "{p}: both encodings (spans and records dedupe)"
        );
    }
}

/// The first delivery of synthetic-fork.ndjson, its first span's id cut to
/// 7 bytes, as a protobuf body (`encode_protobuf` does not check id lengths).
fn bad_body() -> Vec<u8> {
    let text =
        std::fs::read_to_string(fixtures().join("openrouter/synthetic-fork.ndjson")).unwrap();
    let mut v: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["spanId"] = json!("a1a2a3a4a5a6a7");
    toolpath_otel::encode_protobuf(&v).unwrap()
}

fn assert_fails_naming_bad_binpb(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "import succeeded: {stderr}");
    assert!(stderr.contains("bad.binpb"), "{stderr}");
    assert!(stderr.contains("span_id"), "{stderr}");
    assert!(out.stdout.is_empty(), "partial output on stdout");
}

#[test]
fn binpb_with_one_bad_span_id_fails_a_directory_import_naming_the_file() {
    let good = fixtures().join("openrouter/synthetic-fork.ndjson");
    let dir = dir_with(&[(good, "good.ndjson".to_string())]);
    std::fs::write(dir.path().join("bad.binpb"), bad_body()).unwrap();
    assert_fails_naming_bad_binpb(&import(dir.path()));
}

/// Bytes that decode as neither an OTLP traces nor an OTLP logs request.
const JUNK: [u8; 3] = [0xff, 0x01, 0x02];

#[test]
fn a_pb_file_that_is_not_otlp_is_skipped_and_counted_from_a_directory() {
    let good = fixtures().join("openrouter/synthetic-fork.ndjson");
    let dir = dir_with(&[(good.clone(), "good.ndjson".to_string())]);
    std::fs::write(dir.path().join("junk.pb"), JUNK).unwrap();
    let out = import(dir.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stderr.contains("junk.pb"), "{stderr}");
    assert!(stderr.contains("not-otlp=1"), "{stderr}");
    let mut got: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    got.sort_by_key(|d| d.to_string());
    let alone = dir_with(&[(good, "good.ndjson".to_string())]);
    assert_eq!(got, docs(alone.path()));
}

#[test]
fn a_pb_file_that_is_not_otlp_fails_a_file_import() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.pb");
    std::fs::write(&junk, JUNK).unwrap();
    let out = import(&junk);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("junk.pb"), "{stderr}");
    assert!(stderr.contains("not an OTLP request"), "{stderr}");
}

#[test]
fn binpb_with_one_bad_span_id_fails_a_file_import_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.binpb");
    std::fs::write(&bad, bad_body()).unwrap();
    assert_fails_naming_bad_binpb(&import(&bad));
}

#[test]
fn every_binary_extension_is_read_from_a_directory() {
    let enc = fixtures().join("encodings");
    let source = docs(&fixtures().join("openrouter/synthetic-fork.ndjson"));
    for name in [
        "synthetic-fork.ndjson.zst",
        "synthetic-fork-frames.pb",
        "synthetic-fork-frames.pb.zst",
    ] {
        let dir = dir_with(&[(enc.join(name), name.to_string())]);
        assert_eq!(docs(dir.path()), source, "{name}");
    }
    // One protobuf body under each protobuf extension; the file named
    // directly is the reference.
    let first = docs(&enc.join("synthetic-fork-first.binpb"));
    for name in ["first.binpb", "first.protobuf", "first.pb"] {
        let dir = dir_with(&[(enc.join("synthetic-fork-first.binpb"), name.to_string())]);
        assert_eq!(docs(dir.path()), first, "{name}");
    }
}

#[test]
fn help_lists_the_binary_extensions() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("path")
        .unwrap()
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", home.path().join(".toolpath"))
        .args(["p", "import", "otel", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();
    for ext in [".pb", ".binpb", ".protobuf", ".zst"] {
        assert!(help.contains(ext), "{ext} missing from:\n{help}");
    }
}

/// A capture directory reads both encodings: each of the three generations'
/// second copy counts as `duplicate`, and `manifest.json`/`expected.json`
/// as `not-otlp`. Pinned for one span capture and one event capture.
#[test]
fn capture_directories_print_the_pinned_summary() {
    for mode in ["span", "event"] {
        let dir = fixtures().join("semconv/anthropic").join(mode);
        let out = import(&dir);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{mode}: {stderr}");
        assert!(
            stderr
                .lines()
                .any(|l| l == "otel: 1 session; duplicate=3 unclaimed=0 not-otlp=2"),
            "{mode}: {stderr}"
        );
    }
}
