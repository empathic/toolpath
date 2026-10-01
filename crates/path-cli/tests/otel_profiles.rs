//! `path p import otel` profiles over the committed captures (spec: CLI;
//! The `--profile` flag).

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

const SEMCONV: [&str; 4] = ["openai-chat", "openai-responses", "anthropic", "gemini"];
/// The content attributes a metadata-only semconv instrumentation leaves out.
const CONTENT: [&str; 4] = [
    "gen_ai.input.messages",
    "gen_ai.output.messages",
    "gen_ai.system_instructions",
    "gen_ai.tool.definitions",
];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}
fn semconv_dir(name: &str) -> PathBuf {
    fixtures().join("semconv").join(name).join("span")
}
/// The SYNTHETIC copy of the Responses capture that carries
/// `gen_ai.request.previous_response.id`.
fn continuation_dir() -> PathBuf {
    fixtures()
        .join("semconv")
        .join("openai-responses")
        .join("span-continuation")
}
fn openinference_dir() -> PathBuf {
    fixtures().join("openinference").join("openai-chat")
}
fn expected(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("expected.json")).unwrap()).unwrap()
}
/// Sessions a real capture imports as. The real Responses capture carries no
/// continuation attribute (pinned instrumentation), so its requests stand
/// alone: `sessions_without_continuation` when the capture records it.
fn expected_sessions(dir: &Path) -> usize {
    let exp = expected(dir);
    exp.get("sessions_without_continuation")
        .unwrap_or(&exp["sessions"])
        .as_u64()
        .unwrap() as usize
}

/// The `path` binary, sandboxed: no real home, config dir or Claude dir.
fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env_remove("CLAUDE_CONFIG_DIR")
        .env("HOME", home)
        .env("TOOLPATH_CONFIG_DIR", home.join(".toolpath"));
    c
}

/// `path p import otel --no-cache <args>`: (status ok, stdout documents, stderr).
fn import(home: &Path, args: &[&str]) -> (bool, Vec<Value>, String) {
    let out = cmd(home)
        .args(["p", "import", "otel", "--no-cache"])
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let docs = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (out.status.success(), docs, stderr)
}

fn profile_of(doc: &Value) -> &str {
    doc["paths"][0]["meta"]["otel"]["profile"]
        .as_str()
        .unwrap_or("<none>")
}

fn structurals(doc: &Value) -> Vec<&Value> {
    doc["paths"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["change"].as_object().unwrap().values())
        .map(|c| &c["structural"])
        .collect()
}

#[test]
fn semconv_profile_imports_each_capture_directory() {
    let home = tempfile::tempdir().unwrap();
    for name in SEMCONV {
        let dir = semconv_dir(name);
        let (ok, docs, stderr) = import(
            home.path(),
            &["--profile", "semconv", "--input", dir.to_str().unwrap()],
        );
        assert!(ok, "{name}: {stderr}");
        assert_eq!(docs.len(), expected_sessions(&dir), "{name}: {stderr}");
        assert!(docs.iter().all(|d| profile_of(d) == "semconv"), "{name}");
        // manifest.json and expected.json are JSON but not OTLP: skipped and counted.
        assert!(stderr.contains("not-otlp=2"), "{name}: {stderr}");
        assert!(
            !stderr.contains("manifest.json") && !stderr.contains("expected.json"),
            "{name}: {stderr}"
        );
    }
}

#[test]
fn auto_reads_the_semconv_captures() {
    // semconv joined the auto list after openrouter.
    let home = tempfile::tempdir().unwrap();
    for name in SEMCONV {
        let dir = semconv_dir(name);
        let (ok, docs, stderr) = import(home.path(), &["--input", dir.to_str().unwrap()]);
        assert!(ok, "{name}: {stderr}");
        assert_eq!(docs.len(), expected_sessions(&dir), "{name}: {stderr}");
        assert!(docs.iter().all(|d| profile_of(d) == "semconv"), "{name}");
    }
}

#[test]
fn responses_continuation_copy_imports_as_one_session() {
    // SYNTHETIC: the copy sets the continuation attribute to the id the
    // client really sent, so the three requests chain into one session
    // (spec: the server-side-state row), under the named profile and auto.
    let home = tempfile::tempdir().unwrap();
    let dir = continuation_dir();
    let want = expected(&dir)["sessions"].as_u64().unwrap() as usize;
    assert_eq!(want, 1, "the continuation copy expects one session");
    for args in [
        vec!["--profile", "semconv", "--input", dir.to_str().unwrap()],
        vec!["--input", dir.to_str().unwrap()],
    ] {
        let (ok, docs, stderr) = import(home.path(), &args);
        assert!(ok, "{args:?}: {stderr}");
        assert_eq!(docs.len(), want, "{args:?}: {stderr}");
        assert_eq!(profile_of(&docs[0]), "semconv", "{args:?}");
        assert!(stderr.contains("not-otlp=2"), "{args:?}: {stderr}");
    }
}

#[test]
fn openinference_profile_imports_its_capture() {
    let home = tempfile::tempdir().unwrap();
    let dir = openinference_dir();
    let (ok, docs, stderr) = import(
        home.path(),
        &[
            "--profile",
            "openinference",
            "--input",
            dir.to_str().unwrap(),
        ],
    );
    assert!(ok, "{stderr}");
    assert_eq!(docs.len(), expected_sessions(&dir), "{stderr}");
    assert_eq!(profile_of(&docs[0]), "openinference");
    assert!(stderr.contains("not-otlp=2"), "{stderr}");
}

#[test]
fn openinference_is_not_consulted_under_auto() {
    let home = tempfile::tempdir().unwrap();
    let dir = openinference_dir();
    let (ok, docs, stderr) = import(home.path(), &["--input", dir.to_str().unwrap()]);
    assert!(!ok, "auto must not read OpenInference spans: {stderr}");
    assert!(docs.is_empty());
    assert!(stderr.contains("no otel generations"), "{stderr}");
    assert!(
        stderr.contains("unclaimed by the auto profiles; try --profile openinference)"),
        "the error must name the explicit-only profile: {stderr}"
    );
}

#[test]
fn unknown_profile_lists_every_value() {
    let home = tempfile::tempdir().unwrap();
    let dir = semconv_dir("openai-chat");
    let (ok, _, stderr) = import(
        home.path(),
        &["--profile", "bogus", "--input", dir.to_str().unwrap()],
    );
    assert!(!ok);
    assert!(stderr.contains("bogus"), "{stderr}");
    assert!(
        stderr.contains("auto, openrouter, semconv, openinference"),
        "{stderr}"
    );
}

#[test]
fn m0_under_auto_is_still_openrouter() {
    let home = tempfile::tempdir().unwrap();
    let dir = fixtures().join("openrouter");
    let (ok, docs, stderr) = import(home.path(), &["--input", dir.to_str().unwrap()]);
    assert!(ok, "{stderr}");
    assert_eq!(docs.len(), 5, "{stderr}");
    assert!(docs.iter().all(|d| profile_of(d) == "openrouter"));
}

/// Remove every content attribute from every span of an OTLP/JSON body.
fn strip_content(body: &mut Value) {
    for rs in body["resourceSpans"].as_array_mut().unwrap() {
        for ss in rs["scopeSpans"].as_array_mut().unwrap() {
            for sp in ss["spans"].as_array_mut().unwrap() {
                if let Some(attrs) = sp["attributes"].as_array_mut() {
                    attrs.retain(|kv| !CONTENT.contains(&kv["key"].as_str().unwrap_or("")));
                }
            }
        }
    }
}

#[test]
fn metadata_only_captures_validate_as_skeleton_paths() {
    // Spec: "each capture with content stripped by the test → valid skeleton path …
    // validates against the kind schema". Tools are not asserted.
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    for name in SEMCONV {
        let mut body: Value =
            serde_json::from_slice(&std::fs::read(semconv_dir(name).join("traces.json")).unwrap())
                .unwrap();
        strip_content(&mut body);
        let text = body.to_string();
        assert!(
            CONTENT.iter().all(|k| !text.contains(k)),
            "{name}: content left"
        );
        let input = work.path().join(format!("{name}.json"));
        std::fs::write(&input, text).unwrap();

        // Default auto: a metadata-only span is still claimed by semconv.
        let (ok, docs, stderr) = import(home.path(), &["--input", input.to_str().unwrap()]);
        assert!(ok, "{name}: {stderr}");
        assert!(!docs.is_empty(), "{name}");
        for (i, doc) in docs.iter().enumerate() {
            assert_eq!(profile_of(doc), "semconv", "{name}");
            let st = structurals(doc);
            assert!(!st.is_empty(), "{name}: no turns");
            assert!(
                st.iter().all(|s| s["otel"]["absent"]
                    == serde_json::json!({"prompt": true, "completion": true})),
                "{name}: every turn is a skeleton"
            );
            assert!(
                st.iter()
                    .any(|s| s["token_usage"]["output_tokens"].as_u64().is_some()),
                "{name}: usage kept"
            );
            let steps = doc["paths"][0]["steps"].as_array().unwrap();
            assert!(
                steps
                    .iter()
                    .all(|s| s["step"]["actor"].as_str().unwrap().starts_with("agent:")),
                "{name}: models kept as agent actors"
            );

            let file = work.path().join(format!("{name}-{i}.path.json"));
            std::fs::write(&file, serde_json::to_vec(doc).unwrap()).unwrap();
            cmd(home.path())
                .args(["p", "validate", "--input", file.to_str().unwrap()])
                .assert()
                .success();
        }
    }
}
