//! Every claim the site and the RFC make carries a qualifier record at its own
//! text. `docs/model/claims.json` names the claims (file + quote); each needs a
//! live record on that file tagged `claim:<id>` whose span covers the quote. A
//! claim sentence with no record fails here, without regenerating the model.
//! Kinds: `pass` (a committed test or snapshot proves it, cited in the record),
//! `concern` (partly or not proven, the gap said), `blocker` (false today).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whitespace-collapsed text and, per character, the 1-based line it came from.
fn flatten(text: &str) -> (String, Vec<u64>) {
    let (mut out, mut lines, mut line, mut prev_space) = (String::new(), vec![], 1u64, false);
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                lines.push(line);
            }
            prev_space = true;
        } else {
            out.push(ch);
            lines.push(line);
            prev_space = false;
        }
        if ch == '\n' {
            line += 1;
        }
    }
    (out, lines)
}

/// Live records of every `.qual` file under `dirs`, keyed by subject.
fn records(dirs: &[&str]) -> BTreeMap<String, Vec<Value>> {
    let mut all: Vec<Value> = vec![];
    for d in dirs {
        let p = root().join(d).join(".qual");
        if let Ok(text) = std::fs::read_to_string(&p) {
            all.extend(text.lines().filter_map(|l| serde_json::from_str(l).ok()));
        }
    }
    let superseded: BTreeSet<String> = all
        .iter()
        .filter_map(|v| v["body"]["supersedes"].as_str().map(str::to_string))
        .collect();
    let mut by_subject: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for v in all {
        if superseded.contains(v["id"].as_str().unwrap_or_default()) {
            continue;
        }
        by_subject
            .entry(v["subject"].as_str().unwrap_or_default().to_string())
            .or_default()
            .push(v);
    }
    by_subject
}

#[test]
fn every_claim_has_a_live_record_at_its_text() {
    let claims: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("docs/model/claims.json")).expect("claims.json"),
    )
    .expect("claims.json is JSON");
    let dirs: BTreeSet<String> = claims["claims"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["file"].as_str())
        .map(|f| {
            Path::new(f)
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| ".".to_string())
        })
        .collect();
    let recs = records(&dirs.iter().map(String::as_str).collect::<Vec<_>>());
    let mut bad = vec![];
    let mut seen = BTreeSet::new();
    for c in claims["claims"].as_array().expect("claims list") {
        let id = c["id"].as_str().unwrap_or_default();
        let file = c["file"].as_str().unwrap_or_default();
        let quote: String = c["quote"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !seen.insert(id.to_string()) {
            bad.push(format!("{id}: listed twice"));
            continue;
        }
        let text = std::fs::read_to_string(root().join(file)).unwrap_or_default();
        let (flat, lines) = flatten(&text);
        let Some(at) = flat.find(&quote) else {
            bad.push(format!("{id}: the quote is no longer in {file}"));
            continue;
        };
        // `find` gives a byte offset; `lines` is per character.
        let start = flat[..at].chars().count();
        let (first, last) = (lines[start], lines[start + quote.chars().count() - 1]);
        let tag = format!("claim:{id}");
        let covering = recs.get(file).into_iter().flatten().find(|v| {
            let tagged = v["body"]["tags"]
                .as_array()
                .is_some_and(|t| t.iter().any(|x| x.as_str() == Some(&tag)));
            let start = v["body"]["span"]["start"]["line"].as_u64().unwrap_or(0);
            let end = v["body"]["span"]["end"]["line"].as_u64().unwrap_or(start);
            tagged && start <= first && last <= end
        });
        match covering.and_then(|v| v["body"]["kind"].as_str()) {
            None => bad.push(format!(
                "{id}: no live qualifier record tagged {tag} covers {file}:{first}-{last}"
            )),
            Some("pass" | "concern" | "blocker" | "fail") => {}
            Some(k) => bad.push(format!(
                "{id}: record kind {k} is not pass, concern or blocker"
            )),
        }
    }
    assert!(
        bad.is_empty(),
        "claims without a record:\n{}",
        bad.join("\n")
    );
}
