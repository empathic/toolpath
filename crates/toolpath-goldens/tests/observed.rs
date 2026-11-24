//! The format notes' facts, checked against the recordings.
//!
//! `docs/agents/formats/` is practitioner prose, kept by hand. Two kinds of
//! fact in it come from the recordings, and this test keeps them from going
//! stale:
//!
//! * the fields each harness writes, their shapes, and whether they are ever
//!   missing: generated per harness into
//!   `docs/agents/formats/observed/<harness>.md`
//!   (`toolpath_goldens::observed`). A recording that changes them fails
//!   here until the table is regenerated:
//!   `OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed`;
//! * the harness versions the recordings name: the harness's note must name
//!   every one (dotted versions; a bare schema number like pi's `3` is listed
//!   in the table only), so a recording of a new version fails here until the
//!   note says what was seen in it;
//! * the record kinds (entry types) the recordings contain: the harness's
//!   notes must name every one, so a new entry type fails here until it is
//!   described.
//!
//! Every note also links its generated table.

use std::path::{Path, PathBuf};

use toolpath_goldens::model::{Agent, Set};
use toolpath_goldens::observed::{Recording, kinds, render, versions};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// The hand-written note for each harness, under `docs/agents/formats/`.
fn note(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude-code/README.md",
        Agent::Codex => "codex.md",
        Agent::Pi => "pi.md",
        Agent::Copilot => "copilot-cli/README.md",
        Agent::Gemini => "gemini.md",
        Agent::Opencode => "opencode.md",
        // A target only (no recording, so never asked for); its store is
        // described in the Cursor note.
        Agent::Cursor | Agent::CursorAgent => "cursor.md",
    }
}

/// Every recording the baseline runs, by harness: `test-fixtures/<agent>/convo.*`
/// and `goldens/<name>/input.*` (the old tool's per-agent copies skipped, as
/// the goldens test does).
fn recordings() -> Vec<(Agent, String, String)> {
    let root = Path::new(ROOT);
    let mut files: Vec<PathBuf> = Vec::new();
    for (dir, stem) in [("test-fixtures", "convo"), ("goldens", "input")] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if dir == "goldens" && Agent::ALL.iter().any(|a| a.name() == name) {
                continue;
            }
            let Ok(inner) = std::fs::read_dir(e.path()) else {
                continue;
            };
            files.extend(
                inner
                    .filter_map(Result::ok)
                    .map(|f| f.path())
                    .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem)),
            );
        }
    }
    files.sort();
    files
        .iter()
        .map(|p| {
            let set = Set::from_input_file(p).expect("a recording the goldens read");
            let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
            (set.agent, rel, set.text())
        })
        .collect()
}

#[test]
fn every_note_matches_its_recordings() {
    let root = Path::new(ROOT);
    let formats = root.join("docs/agents/formats");
    let update = std::env::var_os("OBSERVED_UPDATE").is_some();
    let all = recordings();
    let mut problems = Vec::new();

    for agent in Agent::ALL {
        let mine: Vec<Recording> = all
            .iter()
            .filter(|(a, _, _)| *a == agent)
            .map(|(_, name, text)| Recording { name, text })
            .collect();
        if mine.is_empty() {
            problems.push(format!("{agent}: no recording in the baseline"));
            continue;
        }

        let generated = render(agent, &mine);
        let table = formats
            .join("observed")
            .join(format!("{}.md", agent.name()));
        if update {
            std::fs::create_dir_all(table.parent().unwrap()).unwrap();
            std::fs::write(&table, &generated).unwrap();
        } else {
            match std::fs::read_to_string(&table) {
                Ok(committed) if committed == generated => {}
                Ok(_) => problems.push(format!(
                    "{agent}: {} is stale: the recordings changed what this harness writes. \
                     Regenerate with OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed, \
                     then read the diff and update {} where it disagrees",
                    table.strip_prefix(root).unwrap().display(),
                    note(agent)
                )),
                Err(_) => problems.push(format!(
                    "{agent}: {} is missing (OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed)",
                    table.strip_prefix(root).unwrap().display()
                )),
            }
        }

        let note_path = formats.join(note(agent));
        let text = std::fs::read_to_string(&note_path)
            .unwrap_or_else(|e| panic!("{}: {e}", note_path.display()));
        // A harness documented as a folder (claude-code/, copilot-cli/): every
        // page of it counts for what the notes name.
        let mut all_notes = text.clone();
        if let Some(dir) = note_path.parent().filter(|d| *d != formats)
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for e in entries.filter_map(Result::ok) {
                if e.path() != note_path && e.path().extension().is_some_and(|x| x == "md") {
                    all_notes += &std::fs::read_to_string(e.path()).unwrap_or_default();
                }
            }
        }
        let link = format!("observed/{}.md", agent.name());
        if !text.contains(&link) {
            problems.push(format!(
                "{agent}: {} does not link its generated table (`{link}`)",
                note(agent)
            ));
        }
        for kind in kinds(&mine) {
            if kind == "document" || kind == "(no type)" {
                continue;
            }
            let name = kind.rsplit('/').next().unwrap_or(&kind);
            if !all_notes.contains(&format!("`{name}`"))
                && !all_notes.contains(&format!("\"{name}\""))
            {
                problems.push(format!(
                    "{agent}: the recordings contain record kind `{kind}`, which {} never names",
                    note(agent)
                ));
            }
        }
        for (path, version, rec) in versions(&mine) {
            if !version.contains('.') {
                continue;
            }
            if !text.contains(&version) {
                problems.push(format!(
                    "{agent}: {} does not name version {version} (`{path}` in {rec}): \
                     say what was seen in it, or that nothing changed",
                    note(agent)
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
