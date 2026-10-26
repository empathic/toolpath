//! One generated object model of the goldens work.
//!
//! The model is read from the repositories at a given revision, never written
//! by hand: the goldens snapshots, the known-defect pins, the qualifier records
//! (`.qual`), the PR piece records (`pr/NN-*.md`, `pr/stack.json`), the claims
//! the site makes (`docs/model/claims.json` names them; the quote is checked
//! against the site source) and the review threads exported from Clax. Every
//! object carries an `id` and a `source` (repo, revision, path, line where there
//! is one), and links to other objects by id. See `docs/model/README.md`.
//!
//! Reading is done through `git` at a revision (`git ls-tree`, `git show`), so a
//! model names exactly the tree it was generated from.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

/// The model's format version.
pub const FORMAT: &str = "toolpath-model/v1";

/// The seven harnesses, in toolpath.net's names.
pub const HARNESSES: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex CLI"),
    ("gemini", "Gemini CLI"),
    ("copilot", "Copilot CLI"),
    ("opencode", "opencode"),
    ("pi", "Pi"),
    ("cursor", "Cursor IDE"),
];

/// Operations no committed snapshot pins today.
pub const UNPINNED_OPERATIONS: &[&str] = &["operation:resume"];

/// A repository read at one revision.
pub struct Repo {
    /// `toolpath` or `pathbase`.
    pub name: String,
    /// A checkout (any worktree of the repository).
    pub dir: String,
    /// The full commit id the model reads.
    pub rev: String,
}

impl Repo {
    /// Resolve `rev` (a branch, tag or sha) in `dir`.
    pub fn open(name: &str, dir: &str, rev: &str) -> Result<Repo> {
        let full = git(
            dir,
            &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
        )?;
        Ok(Repo {
            name: name.into(),
            dir: dir.into(),
            rev: full.trim().into(),
        })
    }

    /// Every file under `prefix` at the revision.
    pub fn ls(&self, prefix: &str) -> Result<Vec<String>> {
        let out = git(
            &self.dir,
            &["ls-tree", "-r", "--name-only", &self.rev, "--", prefix],
        )?;
        Ok(out.lines().map(str::to_string).collect())
    }

    /// One file's text at the revision.
    pub fn read(&self, path: &str) -> Result<String> {
        git(&self.dir, &["show", &format!("{}:{path}", self.rev)])
    }

    /// The full id of a (possibly short) commit id, if it names a commit.
    pub fn resolve(&self, sha: &str) -> Option<String> {
        git(
            &self.dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{sha}^{{commit}}"),
            ],
        )
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    /// True when `full` is the revision or one of its ancestors.
    pub fn contains(&self, full: &str) -> bool {
        Command::new("git")
            .args([
                "-C",
                &self.dir,
                "merge-base",
                "--is-ancestor",
                full,
                &self.rev,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn source(&self, path: &str, line: Option<usize>) -> Value {
        let mut m = json!({"repo": self.name, "rev": self.rev, "path": path});
        if let Some(l) = line {
            m["line"] = json!(l);
        }
        m
    }
}

fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// An insta snapshot: its header fields and its body text.
pub struct Snap {
    pub header: BTreeMap<String, String>,
    pub body: String,
}

/// Split an insta `.snap` into its YAML-ish header (flat `key: value` lines,
/// nested `info:` keys flattened as `info.<key>`) and its body.
pub fn parse_snap(text: &str) -> Snap {
    let mut header = BTreeMap::new();
    let mut parts = text.splitn(3, "---\n");
    let _ = parts.next();
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let mut section = String::new();
    for line in head.lines() {
        let indented = line.starts_with("  ");
        let Some((k, v)) = line.trim().split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        if !indented && v.is_empty() {
            section = k.to_string();
            continue;
        }
        let key = if indented && !section.is_empty() {
            format!("{section}.{k}")
        } else {
            k.to_string()
        };
        header.insert(key, v);
    }
    Snap { header, body }
}

/// The kind, set and target a snapshot file name encodes:
/// `<kind>@<set>[@<target>].snap`.
pub fn snap_name(file: &str) -> Option<(String, String, Option<String>)> {
    let stem = file.rsplit('/').next()?.strip_suffix(".snap")?;
    let mut it = stem.split('@');
    let kind = it.next()?.to_string();
    let set = it.next()?.to_string();
    let target = it.next().map(str::to_string);
    Some((kind, set, target))
}

/// The operation (toolpath.net's words) a snapshot kind is evidence for.
pub fn operation_of(kind: &str) -> &'static str {
    match kind {
        "ir" | "document" | "validate" | "opencode_db" | "formats" => "parse",
        "projection"
        | "round_trip"
        | "shed"
        | "known_defects"
        | "writer"
        | "validate_transforms" => "project",
        "stored" | "stats" | "ir_back" | "losses" => "share",
        _ => "other",
    }
}

/// The harness a set's name belongs to (`claude-session` is a claude set).
pub fn harness_of_set(set: &str) -> String {
    HARNESSES
        .iter()
        .map(|(h, _)| *h)
        .find(|h| set == *h || set.starts_with(&format!("{h}-")))
        .unwrap_or(set)
        .to_string()
}

/// Every 7–40 character hex word in `text` that looks like a commit id
/// (contains a digit and a letter, so `deadbeef`-free prose is skipped).
pub fn shas_in(text: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        let hex = w
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if hex
            && (7..=40).contains(&w.len())
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(|c| c.is_ascii_alphabetic())
        {
            out.insert(w.to_string());
        }
    }
    out.into_iter().collect()
}

fn short(v: &str, n: usize) -> String {
    if v.chars().count() <= n {
        v.to_string()
    } else {
        format!("{}…", v.chars().take(n).collect::<String>())
    }
}

/// The last key of a key path (`paths[].steps[].change.<artifact>.foo.bar` → `bar`).
pub fn leaf(path: &str) -> String {
    path.rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches("[]")
        .to_string()
}

/// Distinct scalar values of key `key` anywhere in the JSON lines of `body`.
pub fn values_of(body: &str, key: &str) -> BTreeSet<String> {
    fn walk(v: &Value, key: &str, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k == key && !x.is_object() && !x.is_array() {
                        out.insert(short(&x.to_string(), 60));
                    }
                    walk(x, key, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        walk(&v, key, &mut out);
    } else {
        for line in body.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                walk(&v, key, &mut out);
            }
        }
    }
    out
}

/// Inputs to one generation.
pub struct Inputs<'a> {
    pub toolpath: &'a Repo,
    pub pathbase: Option<&'a Repo>,
    /// `(artifact url, the Clax threads export)`.
    pub feedback: Vec<(String, Value)>,
}

/// Generate the model.
pub fn generate(inp: &Inputs) -> Result<Value> {
    let tp = inp.toolpath;
    let mut repos: Vec<&Repo> = vec![tp];
    if let Some(pb) = inp.pathbase {
        repos.push(pb);
    }

    // ---- snapshots and sets -------------------------------------------------
    let mut snaps: Vec<(String, &Repo, String, Snap)> = vec![]; // id, repo, path, parsed
    for repo in &repos {
        let dir = if repo.name == "toolpath" {
            "crates/toolpath-goldens/snapshots"
        } else {
            "crates/pathbase-goldens/snapshots"
        };
        for path in repo.ls(dir)? {
            if !path.ends_with(".snap") {
                continue;
            }
            let Some((kind, set, target)) = snap_name(&path) else {
                continue;
            };
            let id = match &target {
                Some(t) => format!("snapshot:{}/{kind}@{set}@{t}", repo.name),
                None => format!("snapshot:{}/{kind}@{set}", repo.name),
            };
            snaps.push((id, repo, path.clone(), parse_snap(&repo.read(&path)?)));
        }
    }

    let manifest: Value = tp
        .read("goldens/manifest.json")
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut harness_versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut set_version: BTreeMap<String, Value> = BTreeMap::new();
    for g in manifest["goldens"].as_array().into_iter().flatten() {
        let (Some(name), Some(h)) = (g["name"].as_str(), g["harness"]["name"].as_str()) else {
            continue;
        };
        let v = g["harness"]["version"].as_str().unwrap_or("unknown");
        harness_versions
            .entry(h.to_string())
            .or_default()
            .insert(v.to_string());
        set_version.insert(name.to_string(), json!({"version": v, "version_source": g["harness"]["version_source"].as_str().unwrap_or("captured")}));
    }

    let mut sets: BTreeMap<String, Value> = BTreeMap::new();
    let mut snapshot_objs = vec![];
    for (id, repo, path, s) in &snaps {
        let (kind, set, target) = snap_name(path).expect("named above");
        let harness = s
            .header
            .get("info.agent")
            .cloned()
            .unwrap_or_else(|| harness_of_set(&set));
        if repo.name == "toolpath" && kind == "ir" {
            let mut o = json!({
                "id": format!("set:{set}"),
                "harness": format!("harness:{harness}"),
                "input_file": s.header.get("input_file"),
                "input_sha256": s.header.get("info.input_sha256"),
                "source": repo.source(path, None),
            });
            if let Some(v) = set_version.get(&set) {
                o["harness_version"] = v.clone();
            }
            sets.insert(set.clone(), o);
        }
        let mut o = json!({
            "id": id,
            "kind": kind,
            "set": format!("set:{set}"),
            "operation": format!("operation:{}", operation_of(&kind)),
            "source": repo.source(path, None),
        });
        if let Some(t) = &target {
            o["target"] = json!(format!("harness:{}", harness_of_set(t)));
        }
        snapshot_objs.push(o);
    }

    // ---- qualifier records -> concerns ------------------------------------------
    let mut quals: Vec<(Value, &Repo, String)> = vec![];
    for repo in &repos {
        for path in repo.ls(".")? {
            if !path.ends_with(".qual") {
                continue;
            }
            for line in repo.read(&path)?.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    quals.push((v, repo, path.clone()));
                }
            }
        }
    }
    let superseded: BTreeMap<String, String> = quals
        .iter()
        .filter_map(|(v, _, _)| {
            Some((
                v["body"]["supersedes"].as_str()?.to_string(),
                v["body"]["kind"].as_str()?.to_string(),
            ))
        })
        .collect();
    let mut concerns = vec![];
    for (v, repo, path) in &quals {
        let full = v["id"].as_str().unwrap_or_default();
        let kind = v["body"]["kind"].as_str().unwrap_or("note");
        let status = match (kind, superseded.get(full).map(String::as_str)) {
            (_, Some("resolve")) => "resolved",
            (_, Some(_)) => "superseded",
            ("concern", None) => "open",
            (k, None) => k,
        };
        let line = v["body"]["span"]["start"]["line"]
            .as_u64()
            .map(|l| l as usize);
        let mut src = repo.source(v["subject"].as_str().unwrap_or(path), line);
        src["record"] = json!(path);
        concerns.push(json!({
            "id": format!("concern:{}", &full[..full.len().min(8)]),
            "kind": kind,
            "status": status,
            "subject": v["subject"],
            "summary": v["body"]["summary"],
            "tags": v["body"]["tags"],
            "supersedes": v["body"]["supersedes"].as_str().map(|s| format!("concern:{}", &s[..s.len().min(8)])),
            "created_at": v["created_at"],
            "source": src,
        }));
    }

    // ---- pieces (pr/NN records) ---------------------------------------------------
    let mut pieces = vec![];
    let mut piece_text: Vec<(String, String)> = vec![]; // id, text
    for repo in &repos {
        let stack: Value = repo
            .read("pr/stack.json")
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        for path in repo.ls("pr")? {
            let file = path.trim_start_matches("pr/");
            let Some(stem) = file.strip_suffix(".md") else {
                continue;
            };
            let Some((nn, _slug)) = stem.split_once('-') else {
                continue;
            };
            if !nn.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let text = repo.read(&path)?;
            let title = text
                .lines()
                .next()
                .unwrap_or("")
                .trim_start_matches("# ")
                .to_string();
            let status_line = text
                .lines()
                .find(|l| l.starts_with("Status:"))
                .unwrap_or("");
            let status_word = status_line
                .trim_start_matches("Status:")
                .trim()
                .split([' ', '(', '·'])
                .next()
                .unwrap_or("")
                .to_string();
            let gh = status_line
                .split('#')
                .nth(1)
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|_| status_word == "opened" || status_word == "merged");
            let id = format!("piece:{}/{nn}", repo.name);
            let commits: Vec<Value> = shas_in(&text)
                .into_iter()
                .filter_map(|s| {
                    let full = repo.resolve(&s)?;
                    Some(json!({"cited": s, "commit": full, "on_wip": repo.contains(&full)}))
                })
                .collect();
            let jj: Vec<Value> = stack["stack"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["bookmarks"].as_array().into_iter().flatten().any(|b| b.as_str() == Some(&format!("pr/{stem}"))))
                .map(|e| json!({"change_id": e["change_id"], "commit_id": e["commit_id"], "note": "ids from the clone and operation that cut the stack (pr/README.md)"}))
                .collect();
            let mut o = json!({
                "id": id,
                "repo": repo.name,
                "number": nn,
                "title": title,
                "status": status_word,
                "status_line": status_line,
                "commits": commits,
                "jj": jj,
                "source": repo.source(&path, None),
            });
            if let Some(n) = gh {
                o["github"] = json!({"repo": format!("empathic/{}", repo.name), "number": n,
                    "url": format!("https://github.com/empathic/{}/pull/{n}", repo.name),
                    "changeling": format!("https://changeling.io/gh/empathic/{}/pull/{n}", repo.name)});
            }
            piece_text.push((id, text));
            pieces.push(o);
        }
    }

    // ---- findings ----------------------------------------------------------------
    let mut findings = vec![];
    // losses and gains across round trips, one finding per (target, key path)
    let mut agg: BTreeMap<(String, String, String), Vec<(String, String)>> = BTreeMap::new(); // (kind,target,path) -> [(set, snapshot id)]
    let projection_body: BTreeMap<(String, String), &str> = snaps
        .iter()
        .filter_map(|(_, r, p, s)| {
            let (k, set, t) = snap_name(p)?;
            (r.name == "toolpath" && k == "projection")
                .then(|| ((set, t.unwrap_or_default()), s.body.as_str()))
        })
        .collect();
    for (id, repo, path, s) in &snaps {
        let Some((kind, set, target)) = snap_name(path) else {
            continue;
        };
        if repo.name != "toolpath" || kind != "round_trip" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&s.body) else {
            continue;
        };
        let target = target.unwrap_or_default();
        for (field, fk) in [("lost", "loss"), ("gained", "gain")] {
            for p in v[field]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                agg.entry((fk.into(), target.clone(), p.into()))
                    .or_default()
                    .push((set.clone(), id.clone()));
            }
        }
    }
    for ((fk, target, path), hits) in &agg {
        let sets: Vec<String> = hits.iter().map(|(s, _)| format!("set:{s}")).collect();
        let snaps_: Vec<&String> = hits.iter().map(|(_, i)| i).collect();
        let mut o = json!({
            "id": format!("finding:{fk}:{target}:{path}"),
            "kind": fk,
            "operation": "operation:project",
            "target": format!("harness:{}", harness_of_set(target)),
            "key_path": path,
            "sets": sets,
            "evidence": snaps_,
        });
        if fk == "gain" {
            let key = leaf(path);
            let mut per_set: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (set, _) in hits {
                if let Some(body) = projection_body.get(&(set.clone(), target.clone())) {
                    let vals = values_of(body, &key);
                    if !vals.is_empty() {
                        per_set.insert(set.clone(), vals.into_iter().take(3).collect());
                    }
                }
            }
            let distinct: BTreeSet<&String> = per_set.values().flatten().collect();
            let constant =
                per_set.len() >= 3 && distinct.len() == 1 && per_set.values().all(|v| v.len() == 1);
            let scalar = distinct
                .iter()
                .all(|v| v.as_str() != "null" && v.as_str() != "\"\"");
            if constant && scalar {
                o["kind"] = json!("invented_value");
                o["id"] = json!(format!("finding:invented_value:{target}:{path}"));
            }
            o["values"] = json!(per_set);
            o["values_note"] = json!(
                "values of the leaf key found in the projection snapshot for each set; the same constant in 3 or more sets marks an invented value"
            );
        }
        findings.push(o);
    }
    // what writing a set back into its own harness drops, with an example value
    for (id, repo, path, s) in &snaps {
        let Some((kind, set, _)) = snap_name(path) else {
            continue;
        };
        if repo.name != "toolpath" || kind != "shed" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&s.body) else {
            continue;
        };
        for d in v["same_format"]["dropped"].as_array().into_iter().flatten() {
            let p = d["path"].as_str().unwrap_or_default();
            findings.push(json!({
                "id": format!("finding:shed:{set}:{p}"),
                "kind": "loss",
                "operation": "operation:project",
                "target": format!("harness:{}", harness_of_set(&set)),
                "key_path": p,
                "example": d["example"],
                "sets": [format!("set:{set}")],
                "evidence": [id],
            }));
        }
        for r in v["same_format"]["renamed"].as_array().into_iter().flatten() {
            findings.push(json!({
                "id": format!("finding:rename:{set}:{}", r["from"].as_str().unwrap_or_default()),
                "kind": "rename",
                "operation": "operation:project",
                "target": format!("harness:{}", harness_of_set(&set)),
                "from": r["from"], "to": r["to"],
                "sets": [format!("set:{set}")],
                "evidence": [id],
            }));
        }
    }
    // known defects, linked to the pieces and concerns that name them
    for (id, repo, path, s) in &snaps {
        let Some((kind, set, _)) = snap_name(path) else {
            continue;
        };
        if repo.name != "toolpath" || kind != "known_defects" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&s.body) else {
            continue;
        };
        for d in v.as_array().into_iter().flatten() {
            let name = d["name"].as_str().unwrap_or_default();
            let by_piece: Vec<&String> = piece_text
                .iter()
                .filter(|(_, t)| t.contains(name))
                .map(|(i, _)| i)
                .collect();
            // A concern owns a defect when it names it, names a renamed key it
            // pins, or names both the defect's kind and its set.
            let renamed: Vec<String> = d["finding"]["renames"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|r| [r["from"].as_str(), r["to"].as_str()])
                .flatten()
                .map(leaf)
                .collect();
            let fkind = d["finding"]["kind"].as_str().unwrap_or("\u{0}");
            let owns = |q: &Value| {
                let t = q.to_string();
                t.contains(name)
                    || renamed.iter().any(|r| t.contains(r.as_str()))
                    || (t.contains(fkind) && t.contains(set.as_str()))
            };
            let by_concern: Vec<String> = quals
                .iter()
                .filter(|(q, _, _)| owns(q))
                .filter_map(|(q, _, _)| {
                    q["id"]
                        .as_str()
                        .map(|s| format!("concern:{}", &s[..s.len().min(8)]))
                })
                .collect();
            findings.push(json!({
                "id": format!("finding:known_defect:{name}"),
                "kind": "known_defect",
                "name": name,
                "operation": if d["target"] == "document" { "operation:parse" } else { "operation:project" },
                "target": d["target"],
                "finding": d["finding"],
                "sets": [format!("set:{set}")],
                "evidence": [id],
                "pieces": by_piece,
                "concerns": by_concern,
            }));
        }
    }
    // Pathbase share-side losses
    for (id, repo, path, s) in &snaps {
        let Some((kind, set, _)) = snap_name(path) else {
            continue;
        };
        if repo.name != "pathbase" || kind != "losses" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&s.body) else {
            continue;
        };
        for l in v.as_array().into_iter().flatten() {
            let k = l["kind"].as_str().unwrap_or("changed");
            let p = l["path"].as_str().unwrap_or_default();
            findings.push(json!({
                "id": format!("finding:share_{k}:{set}:{p}"),
                "kind": match k { "gained" => "gain", "lost" => "loss", _ => "change" },
                "operation": "operation:share",
                "key_path": p,
                "count": l["count"],
                "sets": [format!("set:{set}")],
                "evidence": [id],
            }));
        }
    }
    // declared-unpinned: open concerns tagged as a declared limit or naming an unpinned edge
    for c in &concerns {
        let tags = c["tags"].to_string();
        let summary = c["summary"].as_str().unwrap_or_default();
        if c["status"] == "open"
            && (tags.contains("declared-limit") || summary.to_lowercase().contains("unpinned"))
        {
            findings.push(json!({
                "id": format!("finding:declared_unpinned:{}", c["id"].as_str().unwrap_or_default().trim_start_matches("concern:")),
                "kind": "declared_unpinned",
                "summary": summary,
                "concerns": [c["id"]],
                "evidence": [],
            }));
        }
    }

    // ---- claims ------------------------------------------------------------------------
    let declared: Value = serde_json::from_str(
        &tp.read("docs/model/claims.json")
            .context("docs/model/claims.json")?,
    )?;
    let mut claims = vec![];
    for c in declared["claims"].as_array().into_iter().flatten() {
        let file = c["file"].as_str().unwrap_or_default();
        let quote = c["quote"].as_str().unwrap_or_default();
        let text = tp.read(file).unwrap_or_default();
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let line = text
            .lines()
            .position(|l| {
                l.contains(
                    quote
                        .split_whitespace()
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .as_str(),
                )
            })
            .map(|i| i + 1);
        let found = flat.contains(&quote.split_whitespace().collect::<Vec<_>>().join(" "));
        let ops: Vec<String> = c["operations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|o| format!("operation:{o}"))
            .collect();
        let kinds: Vec<&str> = c["evidence_kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let evidence: Vec<&Value> = snapshot_objs
            .iter()
            .filter(|s| kinds.contains(&s["kind"].as_str().unwrap_or("")))
            .map(|s| &s["id"])
            .collect();
        let against: Vec<&Value> = findings
            .iter()
            .filter(|f| ops.iter().any(|o| f["operation"] == json!(o)))
            .filter(|f| {
                matches!(
                    f["kind"].as_str(),
                    Some("known_defect" | "invented_value" | "declared_unpinned")
                )
            })
            .map(|f| &f["id"])
            .collect();
        // Operations no committed snapshot pins (resume) keep a claim from
        // reading as pinned even when its other operations are.
        let unbacked_ops: Vec<&String> = ops
            .iter()
            .filter(|o| UNPINNED_OPERATIONS.contains(&o.as_str()))
            .collect();
        let snapshot_status = if !found {
            "quote_not_found"
        } else if evidence.is_empty() {
            "unbacked"
        } else if against.is_empty() && unbacked_ops.is_empty() {
            "pinned"
        } else {
            "partly"
        };
        // The status is the claim's own qualifier record (tag `claim:<id>` on
        // the claim's file): pass -> pinned, concern -> partly, blocker or fail
        // -> false. The snapshot reading above is kept beside it, not used.
        let claim_id = c["id"].as_str().unwrap_or_default();
        let tag = format!("claim:{claim_id}");
        let record = quals.iter().rev().find(|(v, _, _)| {
            let full = v["id"].as_str().unwrap_or_default();
            v["subject"] == json!(file)
                && !superseded.contains_key(full)
                && v["body"]["tags"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|x| x == &json!(tag)))
        });
        let status = match (found, record.map(|(v, _, _)| v["body"]["kind"].as_str())) {
            (false, _) => "quote_not_found",
            (true, None) => "no_record",
            (true, Some(Some("pass"))) => "pinned",
            (true, Some(Some("blocker" | "fail"))) => "false",
            (true, Some(_)) => "partly",
        };
        let record_id = record.map(|(v, _, _)| {
            let full = v["id"].as_str().unwrap_or_default();
            format!("concern:{}", &full[..full.len().min(8)])
        });
        claims.push(json!({
            "id": format!("claim:{claim_id}"),
            "text": quote,
            "operations": ops,
            "status": status,
            "record": record_id,
            "snapshot_status": snapshot_status,
            "evidence_kinds": kinds,
            "evidence": evidence,
            "findings_against": against,
            "operations_without_evidence": unbacked_ops,
            "note": c["note"],
            "source": tp.source(file, line),
        }));
    }

    // ---- feedback ----------------------------------------------------------------------
    let mut vocab: Vec<(String, String)> = vec![]; // (token, object id)
    for (h, n) in HARNESSES {
        vocab.push((h.to_string(), format!("harness:{h}")));
        vocab.push((n.to_lowercase(), format!("harness:{h}")));
    }
    for k in [
        "shed",
        "known_defects",
        "round_trip",
        "projection",
        "validate",
    ] {
        vocab.push((k.into(), format!("snapshot-kind:{k}")));
    }
    for f in &findings {
        if let Some(n) = f["name"].as_str() {
            vocab.push((n.into(), f["id"].as_str().unwrap_or_default().into()));
        }
    }
    let piece_by_pr: BTreeMap<String, String> = pieces
        .iter()
        .filter_map(|p| {
            Some((
                format!("#{}", p["github"]["number"].as_u64()?),
                p["id"].as_str()?.to_string(),
            ))
        })
        .collect();
    let piece_by_nn: BTreeMap<String, String> = pieces
        .iter()
        .filter(|p| p["repo"] == "toolpath")
        .filter_map(|p| {
            Some((
                format!("pr/{}", p["number"].as_str()?),
                p["id"].as_str()?.to_string(),
            ))
        })
        .collect();
    let mut feedback = vec![];
    for (url, export) in &inp.feedback {
        for t in export["threads"].as_array().into_iter().flatten() {
            let comments: Vec<Value> = t["comments"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| json!({"author": c["author_kind"], "at": c["created_at"], "text": c["body"]}))
                .collect();
            let asked: String = comments
                .iter()
                .filter(|c| c["author"] != "agent")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
                .to_lowercase();
            let replies: String = comments
                .iter()
                .filter(|c| c["author"] == "agent")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let mut about = BTreeSet::new();
            for (tok, id) in &vocab {
                if asked.contains(tok.as_str()) {
                    about.insert(id.clone());
                }
            }
            if let Some(q) = t["anchor"]["quote"].as_str() {
                let ql = q.to_lowercase();
                for (tok, id) in &vocab {
                    if ql.contains(tok.as_str()) {
                        about.insert(id.clone());
                    }
                }
            }
            let mut resolved_by = BTreeSet::new();
            for s in shas_in(&replies) {
                for repo in &repos {
                    if let Some(full) = repo.resolve(&s) {
                        resolved_by.insert(format!("commit:{}/{}", repo.name, &full[..12]));
                    }
                }
            }
            for (k, id) in piece_by_pr.iter().chain(piece_by_nn.iter()) {
                if replies.contains(k.as_str()) {
                    resolved_by.insert(id.clone());
                }
            }
            feedback.push(json!({
                "id": format!("feedback:{}", t["id"].as_str().unwrap_or_default()),
                "url": url,
                "thread": t["id"],
                "status": t["status"],
                "created_at": t["created_at"],
                "anchor": t["anchor"]["quote"],
                "on_version": t["version_n"],
                "comments": comments,
                "about": about,
                "resolved_by": resolved_by,
            }));
        }
    }

    // ---- assemble ----------------------------------------------------------------------
    let harnesses: Vec<Value> = HARNESSES
        .iter()
        .map(|(h, n)| {
            json!({
                "id": format!("harness:{h}"),
                "name": n,
                "versions_seen": harness_versions.get(*h).cloned().unwrap_or_default(),
                "versions_source": "goldens/manifest.json (harness.version of each captured set)",
            })
        })
        .collect();
    let operations = json!([
        {"id": "operation:parse", "name": "parse", "says": "a harness session becomes a Toolpath document", "evidence_kinds": ["ir", "document", "validate", "opencode_db"]},
        {"id": "operation:project", "name": "project", "says": "a document becomes the on-disk layout a target harness expects", "evidence_kinds": ["projection", "round_trip", "shed", "known_defects"]},
        {"id": "operation:resume", "name": "resume", "says": "a projection followed by a handoff into the target harness", "evidence_kinds": [], "note": "no committed snapshot pins resume; the resume matrix is a scratch review"},
        {"id": "operation:share", "name": "share", "says": "Pathbase stores the document and serves it back", "evidence_kinds": ["stored", "stats", "ir_back", "losses"]}
    ]);
    let mut sources = Map::new();
    for r in &repos {
        sources.insert(r.name.clone(), json!({"rev": r.rev}));
    }
    let model = json!({
        "format": FORMAT,
        "sources": sources,
        "harnesses": harnesses,
        "operations": operations,
        "sets": sets.into_values().collect::<Vec<_>>(),
        "snapshots": snapshot_objs,
        "findings": findings,
        "claims": claims,
        "pieces": pieces,
        "concerns": concerns,
        "feedback": feedback,
    });
    Ok(model)
}

/// Reasons the model breaks its own rules; empty when it holds.
pub fn check(model: &Value) -> Vec<String> {
    let mut bad = vec![];
    for p in model["pieces"].as_array().into_iter().flatten() {
        let st = p["status"].as_str().unwrap_or("");
        if st == "opened" || st == "merged" {
            continue;
        }
        for c in p["commits"].as_array().into_iter().flatten() {
            if c["on_wip"] != json!(true) {
                bad.push(format!(
                    "{}: names commit {} which is not on the revision the model read",
                    p["id"], c["cited"]
                ));
            }
        }
    }
    for c in model["claims"].as_array().into_iter().flatten() {
        if c["status"] == "pinned"
            && c["evidence_kinds"]
                .as_array()
                .is_some_and(|k| !k.is_empty())
            && c["evidence"].as_array().is_none_or(Vec::is_empty)
        {
            bad.push(format!("{}: pinned without snapshot evidence", c["id"]));
        }
        if c["status"] == "no_record" {
            bad.push(format!(
                "{}: no qualifier record tagged with it on {}",
                c["id"], c["source"]["path"]
            ));
        }
        if c["status"] == "quote_not_found" {
            bad.push(format!(
                "{}: the quote is not in {}",
                c["id"], c["source"]["path"]
            ));
        }
    }
    for f in model["findings"].as_array().into_iter().flatten() {
        if f["kind"] == "known_defect"
            && f["pieces"].as_array().is_none_or(Vec::is_empty)
            && f["concerns"].as_array().is_none_or(Vec::is_empty)
        {
            bad.push(format!(
                "{}: a known defect no piece or concern names",
                f["id"]
            ));
        }
    }
    bad
}

/// Counts per object type, for a summary line.
pub fn counts(model: &Value) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for k in [
        "harnesses",
        "operations",
        "sets",
        "snapshots",
        "findings",
        "claims",
        "pieces",
        "concerns",
        "feedback",
    ] {
        out.insert(k.to_string(), model[k].as_array().map_or(0, Vec::len));
    }
    for f in model["findings"].as_array().into_iter().flatten() {
        *out.entry(format!("findings.{}", f["kind"].as_str().unwrap_or("?")))
            .or_default() += 1;
    }
    out
}

/// Fail with every broken rule.
pub fn ensure(model: &Value) -> Result<()> {
    let bad = check(model);
    if bad.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("{}", bad.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_names_and_operations() {
        assert_eq!(
            snap_name("x/round_trip@codex@claude.snap"),
            Some(("round_trip".into(), "codex".into(), Some("claude".into())))
        );
        assert_eq!(operation_of("stored"), "share");
        assert_eq!(harness_of_set("claude-session"), "claude");
    }

    #[test]
    fn header_and_body() {
        let s =
            parse_snap("---\nsource: a.rs\ninfo:\n  agent: codex\ninput_file: f\n---\n{\"a\":1}\n");
        assert_eq!(
            s.header.get("info.agent").map(String::as_str),
            Some("codex")
        );
        assert_eq!(s.header.get("input_file").map(String::as_str), Some("f"));
        assert_eq!(s.body.trim(), "{\"a\":1}");
    }

    #[test]
    fn shas_and_values() {
        assert_eq!(
            shas_in("fixed in 83042e2 (see #359, deadbeef)"),
            vec!["83042e2".to_string()]
        );
        let v = values_of(
            "{\"x\":{\"permissionMode\":\"default\"}}\n{\"permissionMode\":\"default\"}",
            "permissionMode",
        );
        assert_eq!(
            v.into_iter().collect::<Vec<_>>(),
            vec!["\"default\"".to_string()]
        );
        assert_eq!(
            leaf("paths[].steps[].change.<artifact>.raw.permissionMode"),
            "permissionMode"
        );
    }

    #[test]
    fn check_names_each_rule() {
        let m = json!({
            "pieces": [{"id": "piece:toolpath/30", "status": "planned", "commits": [{"cited": "abc1234", "on_wip": false}]}],
            "claims": [
                {"id": "claim:x", "status": "pinned", "evidence_kinds": ["ir"], "evidence": []},
                {"id": "claim:z", "status": "no_record", "evidence_kinds": [], "evidence": [], "source": {"path": "site/index.md"}},
                {"id": "claim:w", "status": "pinned", "evidence_kinds": [], "evidence": []},
            ],
            "findings": [{"id": "finding:known_defect:y", "kind": "known_defect", "pieces": [], "concerns": []}],
        });
        let bad = check(&m);
        assert_eq!(bad.len(), 4, "{bad:?}");
    }
}
