//! The twice-around round trip: for every recording and target, the document the target's adapter
//! derives from its projection is projected to the same target a second time and read back
//! (`twice_around@<set>@<target>.snap`, pinned by tests/goldens.rs). A stable conversion changes
//! nothing on the second pass. Anything that does change is encoding error that accumulates with
//! every round, so each such key path must be declared, with its reason, in
//! `fixtures/twice-around.json` (and a declaration nothing needs any more must go).
//!
//! This test reads the committed snapshots, so it needs no pipeline run: the snapshots say what
//! drifts, the declarations say why, and the two must agree in both directions.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Declaration {
    /// The target harness the second pass drifts toward (`codex`, `copilot`, ...).
    target: String,
    /// `lost`, `gained` or `changed`.
    kind: String,
    /// The generalised key path.
    path: String,
    /// Why this moves on the second pass.
    why: String,
    /// What would have to change for it to stop, when it is a defect rather than the format.
    #[serde(default)]
    defect: Option<String>,
}

fn dir(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(sub)
}

fn declarations() -> Vec<Declaration> {
    let path = dir("fixtures/twice-around.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `(target, kind, path)` for every drifting key path in the committed snapshots, and the number
/// of snapshots read.
fn measured() -> (BTreeSet<(String, String, String)>, usize) {
    let mut out = BTreeSet::new();
    let mut files = 0;
    for entry in std::fs::read_dir(dir("snapshots")).expect("snapshots directory") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name
            .strip_prefix("twice_around@")
            .and_then(|r| r.strip_suffix(".snap"))
        else {
            continue;
        };
        // twice_around@<set>@<target>.snap
        let target = rest.rsplit('@').next().expect("a target").to_string();
        let text = std::fs::read_to_string(entry.path()).expect("snapshot");
        // insta: `---`, metadata, `---`, then the body.
        let body = text.splitn(3, "---\n").nth(2).expect("snapshot body");
        let v: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{name}: {e}"));
        files += 1;
        if v["outcome"] != "compared" {
            // an unreadable pass is its own finding: nothing is compared, so say so
            panic!("{name}: the second pass could not be read: {}", v["error"]);
        }
        for kind in ["lost", "gained", "changed"] {
            for p in v[kind].as_array().into_iter().flatten() {
                out.insert((
                    target.clone(),
                    kind.to_string(),
                    p.as_str().expect("a key path").to_string(),
                ));
            }
        }
    }
    (out, files)
}

#[test]
fn every_second_pass_drift_is_declared_with_a_reason() {
    let declared: BTreeSet<(String, String, String)> = declarations()
        .iter()
        .map(|d| (d.target.clone(), d.kind.clone(), d.path.clone()))
        .collect();
    let (measured, files) = measured();
    assert!(
        files >= 63,
        "expected one snapshot per recording and target, found {files}"
    );
    let undeclared: Vec<String> = measured
        .difference(&declared)
        .map(|(t, k, p)| format!("  -> {t}: {k} {p}"))
        .collect();
    assert!(
        undeclared.is_empty(),
        "the second pass moves these key paths and nothing declares why \
         (fixtures/twice-around.json):\n{}",
        undeclared.join("\n")
    );
    let stale: Vec<String> = declared
        .difference(&measured)
        .map(|(t, k, p)| format!("  -> {t}: {k} {p}"))
        .collect();
    assert!(
        stale.is_empty(),
        "fixtures/twice-around.json declares drift no snapshot shows any more; remove it:\n{}",
        stale.join("\n")
    );
}

#[test]
fn every_declaration_gives_a_reason() {
    for d in declarations() {
        assert!(
            d.why.trim().len() >= 20,
            "{} {} {}: the reason is too short to be one",
            d.target,
            d.kind,
            d.path
        );
        assert!(
            ["lost", "gained", "changed"].contains(&d.kind.as_str()),
            "{}: kind {:?}",
            d.path,
            d.kind
        );
        if let Some(defect) = &d.defect {
            assert!(!defect.trim().is_empty(), "{}: an empty defect", d.path);
        }
    }
}
