//! Every cassette directory says which golden session it was recorded from, and the claim is
//! checked: the golden's sha256 is the sha256 of the file in this tree, and the file list is
//! the directory. A cassette with no provenance, or provenance that names a golden this tree
//! does not have, fails here.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn cassette_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(root.join("crates/toolpath-webmock/cassettes"))
        .expect("cassettes")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn sha256_hex(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    format!("{:x}", Sha256::digest(bytes))
}

fn str_at<'a>(v: &'a Value, keys: &[&str], what: &str) -> &'a str {
    let mut cur = v;
    for k in keys {
        cur = cur
            .get(k)
            .unwrap_or_else(|| panic!("{what}: provenance.json lacks {}", keys.join(".")));
    }
    cur.as_str()
        .unwrap_or_else(|| panic!("{what}: {} is not a string", keys.join(".")))
}

#[test]
fn every_cassette_directory_names_its_golden_and_the_hash_matches() {
    let root = root();
    let dirs = cassette_dirs(&root);
    assert!(!dirs.is_empty(), "no cassette directories");
    for dir in dirs {
        let agent = dir.file_name().unwrap().to_string_lossy().into_owned();
        let prov_path = dir.join("provenance.json");
        let prov: Value = serde_json::from_slice(
            &fs::read(&prov_path).unwrap_or_else(|e| panic!("{agent}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{agent}: provenance.json: {e}"));

        assert_eq!(str_at(&prov, &["cassettes"], &agent), agent);

        // The golden set: present in this tree, and the recorded hash is its hash now.
        let input = str_at(&prov, &["golden", "input"], &agent);
        let want = str_at(&prov, &["golden", "input_sha256"], &agent);
        let got = sha256_hex(&root.join(input));
        assert_eq!(
            got, want,
            "{agent}: {input} is not the bytes the cassettes were recorded from"
        );
        if let Some(twin) = prov["golden"]["same_bytes_as"].as_str() {
            assert_eq!(
                sha256_hex(&root.join(twin)),
                want,
                "{agent}: {twin} drifted"
            );
        }

        // The session id in the golden is the one the provenance says.
        let set_sid = str_at(&prov, &["golden", "session_id"], &agent);
        let text = fs::read_to_string(root.join(input)).unwrap();
        assert!(
            text.contains(set_sid),
            "{agent}: session id {set_sid} is not in {input}"
        );

        // A commit the projection came from, and the probe that made the cassette's turn.
        let rev = str_at(&prov, &["projection", "toolpath_rev"], &agent);
        assert!(
            rev.len() >= 7 && rev.chars().all(|c| c.is_ascii_hexdigit()),
            "{agent}: toolpath_rev {rev:?} is not a commit"
        );
        str_at(&prov, &["projection", "cell"], &agent);
        str_at(&prov, &["probe", "command"], &agent);
        str_at(&prov, &["probe", "prompt"], &agent);

        // The listed files are the directory, nothing more and nothing less.
        let mut listed: Vec<String> = prov["files"]
            .as_array()
            .unwrap_or_else(|| panic!("{agent}: files is not a list"))
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        listed.sort();
        let mut on_disk: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "provenance.json")
            .collect();
        on_disk.sort();
        assert_eq!(
            listed, on_disk,
            "{agent}: provenance.json files differ from the directory"
        );
        for f in &on_disk {
            if f.ends_with(".json") {
                let key = f.trim_end_matches(".json");
                assert!(
                    fs::read_to_string(dir.join("index.jsonl"))
                        .unwrap()
                        .contains(key),
                    "{agent}: {f} is not in index.jsonl"
                );
            }
        }
    }
}
