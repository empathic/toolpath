//! Characterization tests for #124 (`toolpath-convo::derive_path` now
//! stores file-change artifact keys base-relative rather than verbatim).
//!
//! These exercise the CLI's read side against documents that predate that
//! change (absolute keys) as well as the mixed-key transition period where
//! one document uses old-style absolute keys and another uses new-style
//! relative ones. Both `p validate` and `p merge` operate purely on the
//! file paths/stdin given to them -- neither touches `$TOOLPATH_CONFIG_DIR`
//! or the cache -- so, unlike the `resume`/`share`/`import` suites, no
//! `$HOME`/`$TOOLPATH_CONFIG_DIR` sandboxing or env lock is needed here.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn cmd() -> Command {
    Command::cargo_bin("path").unwrap()
}

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

/// A pre-#124 producer wrote file-change keys as the verbatim absolute
/// path. `p validate` must keep accepting such a document unchanged --
/// relativization is a `derive_path` write-side behavior, not a schema
/// requirement, so old documents stay valid forever.
#[test]
fn validate_accepts_pre_relativization_absolute_key_document() {
    let json = r#"{
        "graph": {"id": "g1"},
        "paths": [
            {
                "path": {
                    "id": "p1",
                    "base": {"uri": "file:///old/proj"},
                    "head": "s1"
                },
                "steps": [
                    {
                        "step": {
                            "id": "s1",
                            "actor": "human:alex",
                            "timestamp": "2026-01-01T00:00:00Z"
                        },
                        "change": {
                            "/old/proj/src/legacy.rs": {
                                "raw": "@@ -1,1 +1,1 @@\n-old\n+new"
                            }
                        }
                    }
                ]
            }
        ]
    }"#;
    let f = write_temp(json);

    cmd()
        .args(["p", "validate"])
        .arg("--input")
        .arg(f.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid"));
}

/// Characterizes the intended cross-version behavior during the
/// transition: `p merge` concatenates `paths` verbatim (see
/// `crates/path-cli/src/cmd_merge.rs::merge_into_graph`) with no
/// cross-document key unification. Two documents that both touch the
/// "same" file -- one keyed by the pre-#124 absolute form, the other by
/// the post-#124 base-relative form -- merge into a graph where BOTH keys
/// survive as distinct artifacts. This is not a bug: reconciling absolute
/// and relative keys for the same underlying file across independently
/// authored documents is out of scope for `merge`, which only concatenates.
#[test]
fn merge_does_not_unify_absolute_and_relative_keys_for_the_same_file() {
    let absolute_keyed = r#"{
        "graph": {"id": "g-absolute"},
        "paths": [
            {
                "path": {
                    "id": "p-absolute",
                    "base": {"uri": "file:///proj"},
                    "head": "s1"
                },
                "steps": [
                    {
                        "step": {
                            "id": "s1",
                            "actor": "human:alex",
                            "timestamp": "2026-01-01T00:00:00Z"
                        },
                        "change": {
                            "/proj/f.rs": {
                                "raw": "@@ -1,1 +1,1 @@\n-old\n+new"
                            }
                        }
                    }
                ]
            }
        ]
    }"#;
    let relative_keyed = r#"{
        "graph": {"id": "g-relative"},
        "paths": [
            {
                "path": {
                    "id": "p-relative",
                    "base": {"uri": "file:///proj"},
                    "head": "s1"
                },
                "steps": [
                    {
                        "step": {
                            "id": "s1",
                            "actor": "human:alex",
                            "timestamp": "2026-01-01T00:00:00Z"
                        },
                        "change": {
                            "f.rs": {
                                "raw": "@@ -1,1 +1,1 @@\n-old\n+new"
                            }
                        }
                    }
                ]
            }
        ]
    }"#;
    let f1 = write_temp(absolute_keyed);
    let f2 = write_temp(relative_keyed);

    cmd()
        .args(["p", "merge"])
        .arg(f1.path())
        .arg(f2.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"/proj/f.rs\""))
        .stdout(predicate::str::contains("\"f.rs\""));
}
