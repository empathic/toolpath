//! RFC.md, "Relationship to Version Control Systems", row by row, through
//! `toolpath_git::derive` (what `path p import git` runs).
//!
//! | VCS concept    | Toolpath        | checked here |
//! |----------------|-----------------|--------------|
//! | Commit         | Step            | one step per commit, parents mirroring git's |
//! | Branch         | Path            | one path per branch, base = where it branched, `branch` set |
//! | Commit message | meta.intent     | what survives of a subject + body message |
//! | GPG signature  | meta.signatures | a commit signed with a throwaway SSH key |
//! | Author identity| meta.actors     | two authors: actor strings and their definitions |
//!
//! Plus "dead end preservation": a branch deleted before import.
//!
//! The repository is built in a temp dir with fixed authors, committers and
//! times, so every unsigned commit id is stable. `vcs@mapping` pins one row
//! per RFC claim with what the import actually produced; a row whose
//! `holds` is false is a gap in the import, pinned as it is.

use std::path::Path;
use std::process::Command;

use git2::{Oid, Repository, Signature, Time};
use serde_json::{Value, json};
use toolpath::v1::{Graph, PathOrRef};
use toolpath_git::{DeriveConfig, derive};

fn sig(name: &str, email: &str, minute: i64) -> Signature<'static> {
    Signature::new(name, email, &Time::new(1_767_225_600 + minute * 60, 0)).unwrap()
}

/// Commit `files` on top of `parents`, moving `branch` to it.
fn commit(
    repo: &Repository,
    branch: Option<&str>,
    parents: &[Oid],
    author: &Signature,
    message: &str,
    files: &[(&str, &str)],
) -> Oid {
    let mut index = repo.index().unwrap();
    if let Some(first) = parents.first() {
        index
            .read_tree(&repo.find_commit(*first).unwrap().tree().unwrap())
            .unwrap();
    } else {
        index.clear().unwrap();
    }
    for (name, body) in files {
        let blob = repo.blob(body.as_bytes()).unwrap();
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: body.len() as u32,
                id: blob,
                flags: name.len() as u16,
                flags_extended: 0,
                path: name.as_bytes().to_vec(),
            })
            .unwrap();
    }
    let tree = repo.find_tree(index.write_tree_to(repo).unwrap()).unwrap();
    let parent_commits: Vec<_> = parents
        .iter()
        .map(|p| repo.find_commit(*p).unwrap())
        .collect();
    let refs: Vec<&git2::Commit> = parent_commits.iter().collect();
    let oid = repo
        .commit(None, author, author, message, &tree, &refs)
        .unwrap();
    if let Some(b) = branch {
        repo.reference(&format!("refs/heads/{b}"), oid, true, "test")
            .unwrap();
    }
    oid
}

/// A commit signed with a throwaway SSH key (git's `gpgsig` header), or None
/// when ssh-keygen is not on PATH.
fn signed_commit(repo: &Repository, dir: &Path, parent: Oid, author: &Signature) -> Option<Oid> {
    let keygen = Command::new("ssh-keygen").arg("-?").output().ok()?;
    let _ = keygen;
    let key = dir.join("throwaway_ed25519");
    let ok = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "throwaway@test",
            "-f",
        ])
        .arg(&key)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let parent_commit = repo.find_commit(parent).unwrap();
    let tree = parent_commit.tree().unwrap();
    let buf = repo
        .commit_create_buffer(
            author,
            author,
            "Sign the release\n",
            &tree,
            &[&parent_commit],
        )
        .unwrap();
    let payload = dir.join("payload");
    std::fs::write(&payload, &*buf).unwrap();
    let ok = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", "git", "-f"])
        .arg(&key)
        .arg(&payload)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let armored = std::fs::read_to_string(dir.join("payload.sig")).unwrap();
    let oid = repo
        .commit_signed(std::str::from_utf8(&buf).unwrap(), &armored, None)
        .unwrap();
    repo.reference("refs/heads/main", oid, true, "signed")
        .unwrap();
    Some(oid)
}

#[test]
fn rfc_vcs_table_through_git_import() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::init(tmp.path().join("repo")).unwrap();
    repo.remote("origin", "https://github.com/example/repo.git")
        .unwrap();
    let alex = sig("Alex Kesling", "alex@empathic.dev", 0);
    let bob = sig("Bob Builder", "bob@example.com", 1);

    let c1 = commit(
        &repo,
        Some("main"),
        &[],
        &alex,
        "Initial commit\n",
        &[("a.txt", "1\n")],
    );
    let c2 = commit(
        &repo,
        Some("main"),
        &[c1],
        &bob,
        "Add b\n",
        &[("b.txt", "2\n")],
    );
    // A spike off c1, deleted before import: git keeps no branch for it.
    let spike = commit(
        &repo,
        Some("spike"),
        &[c1],
        &alex,
        "Spike that went nowhere\n",
        &[("spike.txt", "x\n")],
    );
    repo.find_branch("spike", git2::BranchType::Local)
        .unwrap()
        .delete()
        .unwrap();
    let f1 = commit(
        &repo,
        Some("feature"),
        &[c2],
        &alex,
        "Feature: first\n",
        &[("f.txt", "f1\n")],
    );
    let f2 = commit(
        &repo,
        Some("feature"),
        &[f1],
        &bob,
        "Feature: second\n",
        &[("f.txt", "f2\n")],
    );
    let c3 = commit(
        &repo,
        Some("main"),
        &[c2],
        &alex,
        "Main moves on\n\nThe body explains why:\nthe second line of the reason.\n",
        &[("a.txt", "3\n")],
    );
    let merge = commit(
        &repo,
        Some("main"),
        &[c3, f2],
        &alex,
        "Merge feature\n",
        &[("f.txt", "f2\n")],
    );
    let signed = signed_commit(&repo, tmp.path(), merge, &bob);
    repo.set_head("refs/heads/main").unwrap();

    let names: std::collections::HashMap<Oid, &str> = [
        (c1, "c1"),
        (c2, "c2"),
        (spike, "spike"),
        (f1, "f1"),
        (f2, "f2"),
        (c3, "c3"),
        (merge, "merge"),
    ]
    .into_iter()
    .chain(signed.map(|s| (s, "signed")))
    .collect();
    let by_step = |id: &str| -> String {
        names
            .iter()
            .find(|(oid, _)| id == format!("step-{}", &oid.to_string()[..8]))
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| id.to_string())
    };

    let config = DeriveConfig {
        remote: "origin".into(),
        title: None,
        base: None,
    };
    let graph: Graph = derive(&repo, &["main".into(), "feature".into()], &config).unwrap();
    let doc: Value = serde_json::to_value(&graph).unwrap();

    // Branch → Path
    let paths: Vec<&toolpath::v1::Path> = graph
        .paths
        .iter()
        .filter_map(|p| match p {
            PathOrRef::Path(p) => Some(p.as_ref()),
            _ => None,
        })
        .collect();
    let branch_rows: Vec<Value> = paths
        .iter()
        .map(|p| {
            let base = p.path.base.as_ref().unwrap();
            json!({
                "path": p.path.id,
                "branch": base.branch,
                "base_ref": base.ref_str.as_deref().map(|r| names.iter().find(|(o, _)| o.to_string() == r).map(|(_, n)| n.to_string()).unwrap_or(r.to_string())),
                "head": by_step(&p.path.head),
                "steps": p.steps.iter().map(|s| by_step(&s.step.id)).collect::<Vec<_>>(),
            })
        })
        .collect();

    // Commit → Step: parents mirror git's (minus the path's base commit).
    let main = paths.iter().find(|p| p.path.id == "path-main").unwrap();
    let merge_step = main
        .steps
        .iter()
        .find(|s| by_step(&s.step.id) == "merge")
        .expect("the merge commit is a step");
    let merge_parents: Vec<String> = merge_step.step.parents.iter().map(|p| by_step(p)).collect();

    // Commit message → meta.intent (c3 has a subject and a body)
    let c3_step = main.steps.iter().find(|s| by_step(&s.step.id) == "c3");
    let c3_intent = c3_step.and_then(|s| s.meta.as_ref()?.intent.clone());

    // Author identity → meta.actors
    let actors = doc["paths"][0]["meta"]["actors"].clone();

    let actor_names: Vec<&str> = actors
        .as_object()
        .map(|m| m.values().filter_map(|d| d["name"].as_str()).collect())
        .unwrap_or_default();
    let actors_ok = actor_names.contains(&"Alex Kesling") && actor_names.contains(&"Bob Builder");

    // GPG/SSH signature → meta.signatures
    let signed_step = signed.and_then(|s| {
        main.steps
            .iter()
            .find(|st| st.step.id == format!("step-{}", &s.to_string()[..8]))
    });
    let signatures_imported = signed_step
        .and_then(|s| s.meta.as_ref())
        .is_some_and(|m| !m.signatures.is_empty());

    // Dead ends: the deleted spike.
    let spike_imported = doc.to_string().contains(&spike.to_string()[..8]);

    let rows = json!([
        {
            "rfc": "Commit → Step",
            "holds": merge_parents == vec!["c3", "f2"],
            "observed": { "merge_step_parents": merge_parents },
        },
        {
            "rfc": "Branch → Path (base context)",
            "holds": paths.len() == 2 && paths.iter().all(|p| p.path.base.as_ref().is_some_and(|b| b.branch.is_some() && b.ref_str.is_some())),
            "observed": branch_rows,
        },
        {
            "rfc": "Commit message → meta.intent",
            "holds": c3_intent.as_deref() == Some("Main moves on\n\nThe body explains why:\nthe second line of the reason."),
            "observed": { "c3_intent": c3_intent, "note": "the RFC says machine-readable intent; the import keeps the subject line only" },
        },
        {
            "rfc": "GPG signature → meta.signatures",
            "holds": signatures_imported,
            "observed": {
                "signed_commit_made": signed.is_some(),
                "meta_signatures_on_its_step": signatures_imported,
            },
        },
        {
            "rfc": "Author identity → meta.actors",
            "holds": actors_ok,
            "observed": actors,
        },
        {
            "rfc": "Abandoned paths: dead ends preserved",
            "holds": spike_imported,
            "observed": { "deleted_branch_commit_in_document": spike_imported, "note": "git dropped the branch before import; the import walks branch tips only" },
        },
    ]);

    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| insta::assert_json_snapshot!("vcs@mapping", rows));

    // The rows that must hold today; the others are pinned above as gaps.
    assert!(
        rows[0]["holds"].as_bool().unwrap(),
        "Commit → Step: {}",
        rows[0]
    );
    assert!(
        rows[1]["holds"].as_bool().unwrap(),
        "Branch → Path: {}",
        rows[1]
    );
    assert!(
        rows[4]["holds"].as_bool().unwrap(),
        "Author → meta.actors: {}",
        rows[4]
    );
    if signed.is_none() {
        eprintln!(
            "vcs_mapping: ssh-keygen not on PATH; the signature row ran without a signed commit"
        );
    }
}
