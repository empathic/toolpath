//! `cargo xtask contract` and `cargo xtask red-green`: CI that tells a PR's story.
//!
//! * `contract`: the PR record (`pr/<branch>.md`, the branch without its `pr/` prefix) lists its
//!   claims under `## Proves`, one per line: `- <claim> — <file>::<test>[, <file>::<test>…]`. Each named
//!   test is run from the test binary that holds that file, and the table claim → test → result goes to
//!   `$GITHUB_STEP_SUMMARY` (and stdout). A claim with no test, a test that does not exist, or a failing
//!   test fails the command. No record (a push to main, a PR without one) is reported and passes.
//! * `red-green --base <rev>`: the tests the change ADDS (head's `--list` minus base's), shown green at the
//!   head, and, for a new test in an integration-test file (`tests/*.rs`), run against the base with only
//!   that file taken from the head: red there (fails, or does not compile) is the evidence the test
//!   proves the change; green there is flagged "proves nothing new" (a warning). A new test inside a
//!   changed source file cannot be run without the change, and the table says so.
//!
//! Both build with the features the tree's own test gate uses, so they exercise the same binaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The features the tree's own test gate builds with: the `--features X` of the `cargo test` line in
/// `scripts/quality_gates.sh` (so an older base, whose crates lack a newer feature, builds as its CI did).
pub fn gate_features(root: &Path) -> Option<String> {
    let sh = std::fs::read_to_string(root.join("scripts/quality_gates.sh")).ok()?;
    features_of(&sh)
}

pub fn features_of(sh: &str) -> Option<String> {
    let line = sh.lines().find(|l| l.contains("cargo test --workspace"))?;
    let mut words = line.split_whitespace();
    words.find(|w| *w == "--features")?;
    words.next().map(str::to_string)
}

/// One `## Proves` line.
#[derive(Debug, PartialEq)]
pub struct Claim {
    pub text: String,
    pub tests: Vec<TestRef>,
}

/// `crates/x/src/y.rs::name` or `crates/x/tests/z.rs::name`.
#[derive(Debug, PartialEq, Clone)]
pub struct TestRef {
    pub file: String,
    pub name: String,
}

/// The record for a head branch: `pr/02-goldens-core` → `pr/02-goldens-core.md`.
pub fn record_for(root: &Path, branch: &str) -> Option<PathBuf> {
    let stem = branch.strip_prefix("pr/").unwrap_or(branch);
    if stem.is_empty() || stem.contains('/') || stem.contains("..") {
        return None;
    }
    let p = root.join("pr").join(format!("{stem}.md"));
    p.is_file().then_some(p)
}

/// The record's repo path and text: from the tree, else from `records_ref` in git.
pub fn read_record(root: &Path, branch: &str, records_ref: &str) -> Option<(String, String)> {
    if let Some(p) = record_for(root, branch) {
        let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
        return std::fs::read_to_string(&p).ok().map(|t| (rel, t));
    }
    let stem = branch.strip_prefix("pr/").unwrap_or(branch);
    if stem.is_empty() || stem.contains('/') || stem.contains("..") || records_ref.starts_with('-')
    {
        return None;
    }
    let rel = format!("pr/{stem}.md");
    let out = Command::new("git")
        .current_dir(root)
        .args(["show", &format!("{records_ref}:{rel}")])
        .output()
        .ok()?;
    out.status.success().then(|| {
        (
            format!("{records_ref}:{rel}"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    })
}

/// The `## Proves` section of a record. `None` when the record has no such section.
pub fn parse_proves(md: &str) -> Option<Vec<Claim>> {
    let mut lines = md.lines().skip_while(|l| l.trim() != "## Proves");
    lines.next()?;
    let mut claims = Vec::new();
    for l in lines {
        if l.starts_with("## ") {
            break;
        }
        let Some(item) = l.strip_prefix("- ") else {
            continue;
        };
        let (text, refs) = match item.rsplit_once(" — ") {
            Some((t, r)) => (t.trim(), r.trim()),
            None => (item.trim(), ""),
        };
        let tests = refs
            .split(',')
            .map(|r| r.trim().trim_matches('`'))
            .filter(|r| !r.is_empty())
            .filter_map(|r| {
                let (file, name) = r.rsplit_once("::")?;
                Some(TestRef {
                    file: file.to_string(),
                    name: name.to_string(),
                })
            })
            .collect();
        claims.push(Claim {
            text: text.to_string(),
            tests,
        });
    }
    Some(claims)
}

/// A built test binary: the target's source file (repo-relative), its package dir and executable.
#[derive(Debug, Clone)]
pub struct TestExe {
    pub src: String,
    pub kind: String,
    pub pkg_dir: PathBuf,
    pub exe: PathBuf,
}

/// Build every test target (no run) and say where each binary is.
pub fn build_tests(root: &Path) -> Result<std::result::Result<Vec<TestExe>, String>> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args([
        "test",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ]);
    cmd.arg("--workspace");
    if let Some(f) = gate_features(root) {
        cmd.args(["--features", &f]);
    }
    let out = cmd.output().context("running cargo test --no-run")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(12).collect();
        return Ok(Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    let root_abs = root.canonicalize()?;
    let mut exes = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        let (Some(exe), Some(src), Some(manifest)) = (
            v["executable"].as_str(),
            v["target"]["src_path"].as_str(),
            v["manifest_path"].as_str(),
        ) else {
            continue;
        };
        let src = Path::new(src);
        let rel = src.strip_prefix(&root_abs).unwrap_or(src);
        exes.push(TestExe {
            src: rel.to_string_lossy().into_owned(),
            kind: v["target"]["kind"][0].as_str().unwrap_or("").to_string(),
            pkg_dir: Path::new(manifest).parent().unwrap_or(root).to_path_buf(),
            exe: PathBuf::from(exe),
        });
    }
    Ok(Ok(exes))
}

/// The test binary that holds `file`: the integration test whose source it is, else the lib (or, for
/// `src/main.rs` and `src/bin/…`, the bin) target of the package whose `src/` contains it.
pub fn exe_for<'a>(exes: &'a [TestExe], file: &str) -> Option<&'a TestExe> {
    if let Some(e) = exes.iter().find(|e| e.src == file) {
        return Some(e);
    }
    let (pkg, rest) = file.split_once("/src/")?;
    let want_bin = rest == "main.rs" || rest.starts_with("bin/");
    exes.iter().find(|e| {
        e.src.starts_with(&format!("{pkg}/src/"))
            && if want_bin {
                e.kind == "bin"
            } else {
                e.kind == "lib" || e.kind == "proc-macro"
            }
    })
}

/// Run `names` in one binary; `name → ok/FAILED/ignored` for every reported test whose full path is
/// `name` or ends with `::name`.
pub fn run_tests(exe: &TestExe, names: &[&str]) -> Result<BTreeMap<String, Vec<(String, String)>>> {
    let out = Command::new(&exe.exe)
        .current_dir(&exe.pkg_dir)
        .args(names)
        .args(["--test-threads", "4"])
        .output()
        .with_context(|| format!("running {}", exe.exe.display()))?;
    Ok(parse_results(&String::from_utf8_lossy(&out.stdout), names))
}

pub fn parse_results(stdout: &str, names: &[&str]) -> BTreeMap<String, Vec<(String, String)>> {
    let mut by: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for l in stdout.lines() {
        let Some(rest) = l.strip_prefix("test ") else {
            continue;
        };
        let Some((full, result)) = rest.rsplit_once(" ... ") else {
            continue;
        };
        for n in names {
            if full == *n || full.ends_with(&format!("::{n}")) {
                by.entry((*n).to_string())
                    .or_default()
                    .push((full.to_string(), result.trim().to_string()));
            }
        }
    }
    by
}

fn summary(text: &str) {
    print!("{text}");
    if let Ok(p) = std::env::var("GITHUB_STEP_SUMMARY")
        && !p.is_empty()
    {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
    }
}

/// The PR's head branch: `--branch`, else `GITHUB_HEAD_REF` (set on pull_request runs), else none.
fn head_branch(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "--branch")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            std::env::var("GITHUB_HEAD_REF")
                .ok()
                .filter(|s| !s.is_empty())
        })
}

/// Render the contract table; `Ok(true)` when every claim holds.
pub fn contract_table(
    record: &str,
    claims: &[Claim],
    results: &BTreeMap<(String, String), Vec<(String, String)>>,
) -> (String, bool) {
    let mut ok = true;
    let mut t = format!("## Contract: `{record}`\n\n| claim | test | result |\n|---|---|---|\n");
    for c in claims {
        if c.tests.is_empty() {
            ok = false;
            let _ = writeln!(t, "| {} | — | ❌ no test proves this claim |", c.text);
            continue;
        }
        for (i, r) in c.tests.iter().enumerate() {
            let claim = if i == 0 { c.text.as_str() } else { "″" };
            let found = results.get(&(r.file.clone(), r.name.clone()));
            let cell = match found.map(Vec::as_slice) {
                None | Some([]) => {
                    ok = false;
                    "❌ no such test".to_string()
                }
                Some(rs) if rs.iter().all(|(_, res)| res == "ok") => "✅ ok".to_string(),
                Some(rs) => {
                    ok = false;
                    let bad: Vec<String> = rs
                        .iter()
                        .filter(|(_, res)| res != "ok")
                        .map(|(f, res)| format!("{f}: {res}"))
                        .collect();
                    format!("❌ {}", bad.join("; "))
                }
            };
            let _ = writeln!(t, "| {claim} | `{}::{}` | {cell} |", r.file, r.name);
        }
    }
    (t, ok)
}

pub fn contract(root: &Path, args: &[String]) -> Result<()> {
    let Some(branch) = head_branch(args) else {
        summary("## Contract\n\nNot a pull request (no head branch): nothing to prove.\n");
        return Ok(());
    };
    // The records live on wip/bdelanghe; a PR branch cut from it does not carry them. Read the one in
    // the tree when there is one, else `$RECORDS_REF` (default origin/wip/bdelanghe; CI fetches it).
    let records_ref =
        std::env::var("RECORDS_REF").unwrap_or_else(|_| "origin/wip/bdelanghe".to_string());
    let Some((rel, text)) = read_record(root, &branch, &records_ref) else {
        summary(&format!(
            "## Contract\n\nNo PR record for `{branch}` (expected `pr/<name>.md` in the tree or on `{records_ref}`): no claims to check.\n"
        ));
        return Ok(());
    };
    let Some(claims) = parse_proves(&text) else {
        summary(&format!(
            "## Contract: `{rel}`\n\n❌ The record has no `## Proves` section: say what this PR proves and which tests prove it.\n"
        ));
        bail!("{rel}: no ## Proves section");
    };
    let exes = match build_tests(root)? {
        Ok(e) => e,
        Err(why) => {
            summary(&format!(
                "## Contract: `{rel}`\n\n❌ the tests do not build:\n\n```\n{why}\n```\n"
            ));
            bail!("tests do not build");
        }
    };
    // Group the named tests by binary and run each binary once.
    let mut per_exe: BTreeMap<PathBuf, (TestExe, Vec<TestRef>)> = BTreeMap::new();
    let mut results: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    for r in claims.iter().flat_map(|c| &c.tests) {
        match exe_for(&exes, &r.file) {
            Some(e) => per_exe
                .entry(e.exe.clone())
                .or_insert_with(|| (e.clone(), Vec::new()))
                .1
                .push(r.clone()),
            None => {
                results.insert((r.file.clone(), r.name.clone()), Vec::new());
            }
        }
    }
    for (exe, refs) in per_exe.values() {
        let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        let got = run_tests(exe, &names)?;
        for r in refs {
            results.insert(
                (r.file.clone(), r.name.clone()),
                got.get(&r.name).cloned().unwrap_or_default(),
            );
        }
    }
    let (table, ok) = contract_table(&rel, &claims, &results);
    summary(&table);
    if !ok {
        bail!("{rel}: a claim is unproven (see the table)");
    }
    Ok(())
}

/// `src → test names` from every binary's `--list`.
fn list_all(exes: &[TestExe]) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in exes {
        let out = Command::new(&e.exe)
            .current_dir(&e.pkg_dir)
            .args(["--list", "--format", "terse"])
            .output()?;
        let names = m.entry(e.src.clone()).or_default();
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(n) = l.strip_suffix(": test") {
                names.insert(n.to_string());
            }
        }
    }
    Ok(m)
}

/// Tests at the head that the base does not have, per test-binary source.
pub fn added(
    head: &BTreeMap<String, BTreeSet<String>>,
    base: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<(String, String)> {
    let empty = BTreeSet::new();
    head.iter()
        .flat_map(|(src, names)| {
            let b = base.get(src).unwrap_or(&empty);
            names
                .difference(b)
                .map(move |n| (src.clone(), n.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn red_green(root: &Path, args: &[String]) -> Result<()> {
    let base = args
        .iter()
        .position(|a| a == "--base")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var("PR_BASE_SHA").ok().filter(|s| !s.is_empty()));
    let Some(base) = base else {
        summary("## Red → green\n\nNo base revision (not a pull request): nothing to compare.\n");
        return Ok(());
    };
    let head_exes = match build_tests(root)? {
        Ok(e) => e,
        Err(why) => bail!("the head's tests do not build:\n{why}"),
    };
    // The base build reuses the same output names (cargo hashes metadata, not source), so it would
    // overwrite the head's test binaries: keep copies of them first.
    let keep = std::env::temp_dir().join(format!("xtask-red-green-head-{}", std::process::id()));
    std::fs::create_dir_all(&keep)?;
    let head_exes: Vec<TestExe> = head_exes
        .into_iter()
        .enumerate()
        .map(|(i, mut e)| {
            let copy = keep.join(format!(
                "{i}-{}",
                e.exe.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::copy(&e.exe, &copy)?;
            e.exe = copy;
            Ok(e)
        })
        .collect::<Result<_>>()?;
    let head_list = list_all(&head_exes)?;
    // The base in its own worktree and its OWN target dir (under the head's, so CI caches both): a
    // shared one is unsound across two checkouts of the same packages, because cargo's fingerprints
    // are keyed by package, not by checkout, and would hand one tree's test binary to the other. The
    // worktree path is fixed because build scripts bake their CARGO_MANIFEST_DIR in.
    let head_target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    let wt = head_target.join("red-green-base");
    let target = head_target.join("red-green-target");
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_dir_all(&wt);
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "prune"])
        .status();
    let status = Command::new("git")
        .current_dir(root)
        .args(["worktree", "add", "--detach", "-q"])
        .arg(&wt)
        .arg(&base)
        .status()?;
    if !status.success() {
        bail!("git worktree add {base} failed (is the base fetched? checkout with fetch-depth 0)");
    }
    let result = (|| -> Result<(String, bool)> {
        let base_exes = with_target(&target, || build_tests(&wt))?
            .map_err(|why| anyhow::anyhow!("the base's tests do not build:\n{why}"))?;
        let base_list = list_all(&base_exes)?;
        let new = added(&head_list, &base_list);
        let mut t = format!(
            "## Red → green against `{}`\n\n{} test(s) added by this change.\n\n| test | base | head |\n|---|---|---|\n",
            if base.len() == 40 && base.bytes().all(|b| b.is_ascii_hexdigit()) {
                &base[..12]
            } else {
                base.as_str()
            },
            new.len()
        );
        let mut warn = false;
        // Head: run every new test in its binary.
        let mut head_res: BTreeMap<(String, String), String> = BTreeMap::new();
        let mut by_src: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (src, n) in &new {
            by_src.entry(src).or_default().push(n);
        }
        for (src, names) in &by_src {
            if let Some(e) = head_exes.iter().find(|e| e.src == *src) {
                let got = run_tests(e, names)?;
                for n in names {
                    let r = got
                        .get(*n)
                        .and_then(|v| v.iter().find(|(f, _)| f == n))
                        .map_or("not run".to_string(), |(_, r)| r.clone());
                    head_res.insert(((*src).to_string(), (*n).to_string()), r);
                }
            }
        }
        // Base: integration-test files only, with the head's copy of that one file.
        let mut base_res: BTreeMap<(String, String), String> = BTreeMap::new();
        for (src, names) in &by_src {
            let is_itest = src.contains("/tests/") && !src.contains("/src/");
            if !is_itest {
                for n in names {
                    base_res.insert(
                        ((*src).to_string(), (*n).to_string()),
                        "— inline in a changed source file".into(),
                    );
                }
                continue;
            }
            std::fs::create_dir_all(wt.join(src).parent().unwrap())?;
            std::fs::copy(root.join(src), wt.join(src))?;
            let built = with_target(&target, || build_tests(&wt))?;
            for n in names {
                let cell = match &built {
                    Err(_) => "🔴 does not compile without the change".to_string(),
                    Ok(exes) => match exes.iter().find(|e| e.src == *src) {
                        None => "🔴 no binary without the change".to_string(),
                        Some(e) => {
                            let got = run_tests(e, &[n])?;
                            match got.get(*n).and_then(|v| v.iter().find(|(f, _)| f == n)) {
                                Some((_, r)) if r == "ok" => {
                                    warn = true;
                                    "⚠️ already green: proves nothing new".to_string()
                                }
                                Some((_, r)) => format!("🔴 {r}"),
                                None => "🔴 not found".to_string(),
                            }
                        }
                    },
                };
                base_res.insert(((*src).to_string(), (*n).to_string()), cell);
            }
        }
        let mut ok = true;
        for (src, n) in &new {
            let k = (src.clone(), n.clone());
            let h = head_res.get(&k).map_or("not run", String::as_str);
            if h != "ok" {
                ok = false;
            }
            let hcell = if h == "ok" {
                "🟢 ok".to_string()
            } else {
                format!("❌ {h}")
            };
            let _ = writeln!(
                t,
                "| `{src}::{n}` | {} | {hcell} |",
                base_res.get(&k).map_or("", String::as_str)
            );
        }
        if new.is_empty() {
            t.push_str("| (none) | | |\n");
        }
        if warn {
            t.push_str("\n⚠️ A new test that already passes without the change does not prove the change.\n");
        }
        Ok((t, ok))
    })();
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .status();
    let _ = std::fs::remove_dir_all(&keep);
    let (table, ok) = result?;
    summary(&table);
    if !ok {
        bail!("a new test is not green at the head");
    }
    Ok(())
}

fn with_target<T>(target: &Path, f: impl FnOnce() -> T) -> T {
    // SAFETY: xtask is single-threaded here; nothing else reads the environment concurrently.
    let prev = std::env::var_os("CARGO_TARGET_DIR");
    unsafe { std::env::set_var("CARGO_TARGET_DIR", target) };
    let r = f();
    match prev {
        Some(v) => unsafe { std::env::set_var("CARGO_TARGET_DIR", v) },
        None => unsafe { std::env::remove_var("CARGO_TARGET_DIR") },
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proves_section_parses_claims_tests_and_claims_without_tests() {
        let md = "# t\n\n## Why\nx\n\n## Proves\n- totals keep breakdowns — crates/a/src/b.rs::t1, `crates/a/tests/c.rs::t2`\n- unproven claim\nnot an item\n\n## Next\n- ignored — x.rs::y\n";
        let c = parse_proves(md).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "totals keep breakdowns");
        assert_eq!(
            c[0].tests,
            [
                TestRef {
                    file: "crates/a/src/b.rs".into(),
                    name: "t1".into()
                },
                TestRef {
                    file: "crates/a/tests/c.rs".into(),
                    name: "t2".into()
                }
            ]
        );
        assert!(c[1].tests.is_empty());
        assert!(parse_proves("# no section\n").is_none());
    }

    #[test]
    fn record_is_the_branch_without_its_pr_prefix_and_never_a_path() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("pr")).unwrap();
        std::fs::write(d.path().join("pr/02-x.md"), "").unwrap();
        assert!(record_for(d.path(), "pr/02-x").is_some());
        assert!(record_for(d.path(), "02-x").is_some());
        assert!(record_for(d.path(), "pr/../x").is_none());
        assert!(record_for(d.path(), "pr/03-y").is_none());
    }

    #[test]
    fn results_match_the_full_path_or_its_last_segment_only() {
        let out = "running 3 tests\ntest a::tests::t1 ... ok\ntest a::tests::xt1 ... ok\ntest goldens::claude ... FAILED\n";
        let r = parse_results(out, &["t1", "claude"]);
        assert_eq!(r["t1"], [("a::tests::t1".to_string(), "ok".to_string())]);
        assert_eq!(r["claude"][0].1, "FAILED");
    }

    #[test]
    fn contract_fails_on_a_missing_test_a_failing_test_and_a_claim_without_tests() {
        let claims = vec![
            Claim {
                text: "a".into(),
                tests: vec![TestRef {
                    file: "f.rs".into(),
                    name: "ok1".into(),
                }],
            },
            Claim {
                text: "b".into(),
                tests: vec![TestRef {
                    file: "f.rs".into(),
                    name: "gone".into(),
                }],
            },
        ];
        let mut res = BTreeMap::new();
        res.insert(
            ("f.rs".into(), "ok1".into()),
            vec![("m::ok1".into(), "ok".into())],
        );
        let (t, ok) = contract_table("pr/x.md", &claims, &res);
        assert!(
            !ok && t.contains("no such test") && t.contains("✅ ok"),
            "{t}"
        );
        res.insert(
            ("f.rs".into(), "gone".into()),
            vec![("m::gone".into(), "FAILED".into())],
        );
        let (t, ok) = contract_table("pr/x.md", &claims, &res);
        assert!(!ok && t.contains("FAILED"), "{t}");
        let (_, ok) = contract_table(
            "pr/x.md",
            &[Claim {
                text: "c".into(),
                tests: vec![],
            }],
            &BTreeMap::new(),
        );
        assert!(!ok);
        res.insert(
            ("f.rs".into(), "gone".into()),
            vec![("m::gone".into(), "ok".into())],
        );
        assert!(contract_table("pr/x.md", &claims, &res).1);
    }

    #[test]
    fn features_come_from_the_test_gate_line_or_none() {
        let sh =
            "gate_test() {\n    RUSTFLAGS=x cargo test --workspace --features a/b,c/d 2>&1\n}\n";
        assert_eq!(features_of(sh).as_deref(), Some("a/b,c/d"));
        assert_eq!(features_of("    cargo test --workspace 2>&1\n"), None);
    }

    #[test]
    fn added_is_head_minus_base_per_binary() {
        let mut h = BTreeMap::new();
        h.insert(
            "a/tests/t.rs".to_string(),
            BTreeSet::from(["x".to_string(), "y".to_string()]),
        );
        h.insert(
            "b/src/lib.rs".to_string(),
            BTreeSet::from(["z".to_string()]),
        );
        let mut b = BTreeMap::new();
        b.insert(
            "a/tests/t.rs".to_string(),
            BTreeSet::from(["x".to_string()]),
        );
        assert_eq!(
            added(&h, &b),
            [
                ("a/tests/t.rs".to_string(), "y".to_string()),
                ("b/src/lib.rs".to_string(), "z".to_string())
            ]
        );
    }

    #[test]
    fn exe_for_picks_the_integration_test_else_the_lib_else_the_bin() {
        let e = |src: &str, kind: &str| TestExe {
            src: src.into(),
            kind: kind.into(),
            pkg_dir: PathBuf::new(),
            exe: PathBuf::from(src),
        };
        let exes = [
            e("crates/p/src/lib.rs", "lib"),
            e("crates/p/src/main.rs", "bin"),
            e("crates/p/tests/g.rs", "test"),
        ];
        assert_eq!(
            exe_for(&exes, "crates/p/tests/g.rs").unwrap().src,
            "crates/p/tests/g.rs"
        );
        assert_eq!(
            exe_for(&exes, "crates/p/src/goldens.rs").unwrap().kind,
            "lib"
        );
        assert_eq!(exe_for(&exes, "crates/p/src/main.rs").unwrap().kind, "bin");
        assert!(exe_for(&exes, "crates/q/src/x.rs").is_none());
    }
}
