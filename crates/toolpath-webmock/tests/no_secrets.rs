//! The committed fixture surfaces carry no credential and no machine identity.
//!
//! The patterns and the allowlist live in `.gitleaks.toml` at the repo root, the same file
//! `gitleaks` reads in `scripts/quality_gates.sh` (gate `secrets`, when the binary is on PATH).
//! This test is the half that always runs: CI on main has no flake and no gitleaks, and
//! `cargo test` is there either way. One file holds the rules; a rule or an allowlist entry is
//! added once, with its reason.
//!
//! A real Gemini API key reached two scratch cassettes on 2026-10-15 because `x-goog-api-key`
//! was not on the recorder's redaction list, and a grep by hand caught it. This test is the grep,
//! run every time, over everything committed as a fixture — including the base64 the cassettes
//! store response chunks in, which is where the pi recording hid the recording machine's path.
//!
//! Every finding is printed as `path:line  rule-id  <excerpt>` before the test fails, so the fix
//! is the listed line.

use base64::Engine;
use regex::{Regex, RegexSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// The committed fixture surfaces, relative to the workspace root. `crates/*/tests/fixtures` is
/// expanded by [`surfaces`].
const SURFACES: &[&str] = &[
    "crates/toolpath-webmock/cassettes",
    "crates/toolpath-goldens/snapshots",
    "goldens",
    "test-fixtures",
    "crates/*/tests/fixtures",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Expand the surfaces into existing directories.
fn surfaces(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for s in SURFACES {
        if let Some((head, tail)) = s.split_once("/*/") {
            let mut entries: Vec<_> = fs::read_dir(root.join(head))
                .expect(head)
                .filter_map(Result::ok)
                .map(|e| e.path().join(tail))
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            out.extend(entries);
        } else {
            let p = root.join(s);
            assert!(
                p.is_dir(),
                "fixture surface {s} is missing; update SURFACES"
            );
            out.push(p);
        }
    }
    out
}

/// What an allowlist regex is tested against.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    /// The matched text (gitleaks' `match`, and its default `secret` for rules without a
    /// capture group).
    Match,
    /// The whole line.
    Line,
}

struct Allow {
    regexes: Vec<Regex>,
    paths: Vec<Regex>,
    target: Target,
    /// `AND`: a path and a regex must both match; `OR` (default): either is enough.
    all: bool,
}

impl Allow {
    fn from_toml(t: &toml::Table) -> Self {
        let regexes = strings(t, "regexes").iter().map(|r| rx(r)).collect();
        let paths = strings(t, "paths").iter().map(|r| rx(r)).collect();
        let target = match t.get("regexTarget").and_then(|v| v.as_str()) {
            Some("line") => Target::Line,
            _ => Target::Match,
        };
        let all = t
            .get("condition")
            .and_then(|v| v.as_str())
            .is_some_and(|c| c.eq_ignore_ascii_case("and"));
        Allow {
            regexes,
            paths,
            target,
            all,
        }
    }

    fn allows(&self, rel: &str, line: &str, matched: &str) -> bool {
        let path_hit = self.paths.iter().any(|p| p.is_match(rel));
        let text = match self.target {
            Target::Match => matched,
            Target::Line => line,
        };
        let regex_hit = self.regexes.iter().any(|r| r.is_match(text));
        if self.all {
            (self.paths.is_empty() || path_hit) && (self.regexes.is_empty() || regex_hit)
        } else {
            path_hit || regex_hit
        }
    }
}

struct Rule {
    id: String,
    re: Regex,
    /// Only files whose relative path matches are scanned by this rule.
    path: Option<Regex>,
    allow: Vec<Allow>,
}

struct Config {
    rules: Vec<Rule>,
    allow: Vec<Allow>,
}

fn rx(s: &str) -> Regex {
    Regex::new(s).unwrap_or_else(|e| panic!("bad regex in .gitleaks.toml: {s}: {e}"))
}

fn strings(t: &toml::Table, key: &str) -> Vec<String> {
    t.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn tables<'a>(t: &'a toml::Table, key: &str) -> Vec<&'a toml::Table> {
    t.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_table()).collect())
        .unwrap_or_default()
}

fn load_config(root: &Path) -> Config {
    let path = root.join(".gitleaks.toml");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc: toml::Table = text.parse().expect(".gitleaks.toml parses");
    let rules = tables(&doc, "rules")
        .into_iter()
        .map(|r| Rule {
            id: r["id"].as_str().expect("rule id").to_owned(),
            re: rx(r["regex"].as_str().expect("rule regex")),
            path: r.get("path").and_then(|v| v.as_str()).map(rx),
            allow: tables(r, "allowlists")
                .into_iter()
                .map(Allow::from_toml)
                .collect(),
        })
        .collect();
    let allow = tables(&doc, "allowlists")
        .into_iter()
        .map(Allow::from_toml)
        .collect();
    Config { rules, allow }
}

/// A run of base64 long enough to hide a path or a key. Cassettes store response chunks this
/// way (`"b64": "…"`), so the scan reads what the base64 says, not only how it looks.
fn base64_runs() -> Regex {
    Regex::new(r"[A-Za-z0-9+/]{64,}={0,2}").expect("base64 regex")
}

/// The text a base64 run encodes, when it is text (compressed or binary bodies are skipped).
fn decoded_text(run: &str) -> Option<String> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
    let bytes = STANDARD.decode(run).or_else(|_| {
        let trimmed = run.trim_end_matches('=');
        STANDARD_NO_PAD.decode(&trimmed[..trimmed.len() - trimmed.len() % 4])
    });
    let text = String::from_utf8_lossy(&bytes.ok()?).into_owned();
    let total = text.chars().count();
    let printable = text
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .filter(|&c| c != char::REPLACEMENT_CHARACTER)
        .count();
    (total > 0 && printable * 10 >= total * 9).then_some(text)
}

struct Scanner<'a> {
    cfg: &'a Config,
    set: RegexSet,
    b64: Regex,
    findings: Vec<String>,
}

impl Scanner<'_> {
    /// Run every rule over `text` (one line of a file, or what a base64 run in it decodes to).
    fn check(&mut self, rel: &str, line_no: usize, text: &str, via: &str) {
        for idx in self.set.matches(text) {
            let rule = &self.cfg.rules[idx];
            if rule.path.as_ref().is_some_and(|p| !p.is_match(rel)) {
                continue;
            }
            for m in rule.re.find_iter(text) {
                let hit = m.as_str();
                let allowed = rule
                    .allow
                    .iter()
                    .chain(self.cfg.allow.iter())
                    .any(|a| a.allows(rel, text, hit));
                if !allowed {
                    // The first eight characters and the length, never the whole value: this
                    // output lands in CI logs.
                    let head: String = hit.chars().take(8).collect();
                    self.findings.push(format!(
                        "{rel}:{line_no}{via}  {}  {head}… ({} chars)",
                        rule.id,
                        hit.chars().count()
                    ));
                }
            }
        }
    }

    fn file(&mut self, rel: &str, text: &str) {
        for (i, line) in text.lines().enumerate() {
            self.check(rel, i + 1, line, "");
            let runs: Vec<String> = self
                .b64
                .find_iter(line)
                .map(|m| m.as_str().to_owned())
                .collect();
            for run in runs {
                if let Some(decoded) = decoded_text(&run) {
                    self.check(rel, i + 1, &decoded, " (base64)");
                }
            }
        }
    }
}

/// `path:line  rule-id  excerpt` for every match the allowlists do not cover.
fn scan(root: &Path, cfg: &Config) -> Vec<String> {
    let mut scanner = Scanner {
        cfg,
        set: RegexSet::new(cfg.rules.iter().map(|r| r.re.as_str())).expect("regex set"),
        b64: base64_runs(),
        findings: Vec::new(),
    };
    for surface in surfaces(root) {
        for entry in WalkDir::new(&surface)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let rel = entry
                .path()
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            if cfg
                .allow
                .iter()
                .any(|a| a.regexes.is_empty() && a.paths.iter().any(|p| p.is_match(&rel)))
            {
                continue;
            }
            let bytes = fs::read(entry.path()).expect("read fixture");
            scanner.file(&rel, &String::from_utf8_lossy(&bytes));
        }
    }
    scanner.findings
}

#[test]
fn fixture_surfaces_carry_no_secret_or_machine_identity() {
    let root = root();
    let cfg = load_config(&root);
    let findings = scan(&root, &cfg);
    assert!(
        findings.is_empty(),
        "{} finding(s) in the committed fixtures (rules: .gitleaks.toml):\n  {}\n\
         Scrub the value (cassette headers become <redacted>; paths become <scratch>) or, for a \
         documented false positive, add an allowlist entry with its reason.",
        findings.len(),
        findings.join("\n  ")
    );
}

/// Every rule in `.gitleaks.toml` catches the shape it claims, through base64 too, and the
/// placeholders pass. The samples are assembled at run time so this source never contains a
/// secret-shaped string (the repo-level gitleaks scan reads it too).
#[test]
fn rules_catch_their_shapes_and_placeholders_pass() {
    let cfg = load_config(&root());
    let x = |n: usize| "x".repeat(n);
    let samples: Vec<(&str, String)> = vec![
        ("anthropic-key", format!("sk-ant-api03-{}", x(40))),
        ("google-api-key", format!("AIza{}", "A".repeat(35))),
        ("google-api-key-aq", format!("AQ.{}", x(52))),
        ("github-token", format!("ghp_{}", x(36))),
        ("github-token", format!("github_pat_{}", x(40))),
        ("openai-key", format!("sk-proj-{}", x(40))),
        ("slack-token", format!("xoxb-{}", "1".repeat(12))),
        ("aws-access-key-id", format!("AKIA{}", "A".repeat(16))),
        (
            "private-key-block",
            ["-----BEGIN ", "RSA ", "PRIVATE", " KEY-----"].concat(),
        ),
        ("jwt", format!("eyJ{}.eyJ{}.{}", x(10), x(10), x(10))),
        ("bearer-token", format!("Bearer {}", x(32))),
        (
            "credential-header-not-redacted",
            format!("\"x-goog-api-key\": \"AIza{}\"", "A".repeat(35)),
        ),
        (
            "machine-identity-claude-tmp",
            ["/private/tmp/", "claude-501", "/x"].concat(),
        ),
        (
            "machine-identity-encoded-home",
            ["-Users-", "bobby-", "x"].concat(),
        ),
        ("machine-identity-user-at", ["bobby", "@", "host"].concat()),
        (
            "machine-identity-user-home",
            ["/Users", "/someone", "/x"].concat(),
        ),
    ];
    for rule in &cfg.rules {
        let covered = samples
            .iter()
            .any(|(id, s)| id == &rule.id && rule.re.is_match(s));
        assert!(
            covered,
            "rule {} has no sample that it matches; add one here",
            rule.id
        );
    }
    for (id, s) in &samples {
        let rule = cfg
            .rules
            .iter()
            .find(|r| &r.id == id)
            .unwrap_or_else(|| panic!("sample names rule {id}, which .gitleaks.toml lacks"));
        assert!(rule.re.is_match(s), "rule {id} must match its sample");
    }

    // The same samples wrapped the way a cassette stores a response chunk are found through the
    // base64, and reported as such.
    let mut scanner = Scanner {
        cfg: &cfg,
        set: RegexSet::new(cfg.rules.iter().map(|r| r.re.as_str())).unwrap(),
        b64: base64_runs(),
        findings: Vec::new(),
    };
    let surface = SURFACES[0];
    for (id, s) in &samples {
        let padded = format!(
            "event: text\ndata: {{\"text\": \"{s}\"}}\n{}",
            " ".repeat(64)
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(padded);
        scanner.findings.clear();
        scanner.file(
            &format!("{surface}/x/cassette.json"),
            &format!("  \"b64\": \"{b64}\""),
        );
        assert!(
            scanner
                .findings
                .iter()
                .any(|f| f.contains("(base64)") && f.contains(id)),
            "rule {id} must be found through base64: {:?}",
            scanner.findings
        );
    }

    // The placeholders the recorder writes, and the shapes a cassette legitimately carries.
    let header = cfg
        .rules
        .iter()
        .find(|r| r.id == "credential-header-not-redacted")
        .expect("header rule");
    for ok in [
        "\"authorization\": \"<redacted>\"",
        "\"x-goog-api-key\": \"<redacted>\"",
        "\"set-cookie\": \"<redacted>\"",
    ] {
        assert!(
            !header.re.is_match(ok),
            "{ok} is the placeholder; must pass"
        );
    }
    let bearer = cfg
        .rules
        .iter()
        .find(|r| r.id == "bearer-token")
        .expect("bearer rule");
    assert!(!bearer.re.is_match("Bearer <redacted>"));
    assert!(!bearer.re.is_match("Bearer $TOKEN"));
    let openai = cfg
        .rules
        .iter()
        .find(|r| r.id == "openai-key")
        .expect("openai rule");
    assert!(
        !openai.re.is_match(&format!("sk-ant-api03-{}", x(40))),
        "an Anthropic key is reported once, by its own rule"
    );
}
