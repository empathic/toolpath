//! Rule loading from the vendored ruleset.
//!
//! Vendored from `gitleaks/gitleaks`, `config/gitleaks.toml`, commit
//! `b58d3f102cf3a2c84cb7f923d05c25c9b1aed84b` (2026-07-22). Gitleaks is
//! MIT-licensed (https://github.com/gitleaks/gitleaks/blob/master/LICENSE).
//! `gitleaks.toml` itself is kept byte-verbatim so it can be diffed against
//! upstream; do not hand-edit it.

use serde::Deserialize;

const RAW_TOML: &str = include_str!("gitleaks.toml");

/// One rule, with its allow-regexes already compiled. `regex` stays a
/// `String` (not a compiled `Regex`) because [`load_rules`] is a pure
/// parse step exercised on its own by the compile-guard test; the caller
/// compiles it.
pub struct Rule {
    pub id: String,
    pub regex: String,
    pub entropy: Option<f64>,
    pub keywords: Vec<String>,
    /// Gitleaks' per-rule `[[rules.allowlists]] regexes` - documented
    /// exceptions (e.g. AWS's own `...EXAMPLE` key) checked against the
    /// matched secret text. Compiled best-effort: an allowlist entry that
    /// fails under `regex` is dropped rather than failing the whole rule,
    /// since it is a false-positive refinement, not the detection itself.
    pub allow: Vec<regex::Regex>,
}

#[derive(Deserialize)]
struct RulesFile {
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    /// Absent on the one rule (`pkcs12-file`) that matches a file's *path*
    /// rather than its content - out of scope for a text detector, so
    /// [`load_rules`] drops any rule missing it.
    regex: Option<String>,
    entropy: Option<f64>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    allowlists: Vec<RawAllowlist>,
}

#[derive(Deserialize)]
struct RawAllowlist {
    #[serde(default)]
    regexes: Vec<String>,
}

/// Rust's `regex` crate (RE2-derived, no backreferences/lookaround)
/// rejects a minority of gitleaks patterns written for Go's RE2 dialect.
/// Confirmed by `every_vendored_rule_compiles_under_rust_regex` - see that
/// test for the failure each id below hit.
pub const EXCLUDED_RULE_IDS: &[(&str, &str)] = &[
    (
        "generic-api-key",
        "compiled form exceeds regex's 10 MiB size limit under Rust's (non-backtracking) engine",
    ),
    (
        "pypi-upload-token",
        "compiled form exceeds regex's 10 MiB size limit under Rust's (non-backtracking) engine",
    ),
    (
        "vault-batch-token",
        "compiled form exceeds regex's 10 MiB size limit under Rust's (non-backtracking) engine",
    ),
];

/// Hand-written rules filling gaps the vendored ruleset leaves open for
/// this crate's purposes: gitleaks matches a PEM block only with its full
/// closing footer, and matches a JWT only above a claim-length floor that
/// misses short tokens; URI-embedded credentials aren't a gitleaks rule at
/// all (it scans files, not structured connection strings).
fn supplemental_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "pem-private-key-header".to_string(),
            regex: r"-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----".to_string(),
            entropy: None,
            keywords: vec!["-----begin".to_string()],
            allow: Vec::new(),
        },
        Rule {
            id: "jwt-compact".to_string(),
            regex: r"\bey[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]{2,}\b".to_string(),
            entropy: None,
            keywords: vec!["ey".to_string()],
            allow: Vec::new(),
        },
        Rule {
            id: "uri-credential".to_string(),
            regex: r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:@/]+:([^\s:@/]+)@[^\s/]+".to_string(),
            entropy: None,
            keywords: vec!["://".to_string()],
            allow: Vec::new(),
        },
    ]
}

/// Parses the vendored TOML plus [`supplemental_rules`], dropping any rule
/// on [`EXCLUDED_RULE_IDS`]. Pure and deterministic - no I/O beyond the
/// `include_str!` baked in at compile time.
pub fn load_rules() -> Vec<Rule> {
    let parsed: RulesFile = toml::from_str(RAW_TOML).expect("vendored gitleaks.toml must parse");
    let mut rules: Vec<Rule> = parsed
        .rules
        .into_iter()
        .filter(|r| !EXCLUDED_RULE_IDS.iter().any(|(id, _)| *id == r.id))
        .filter_map(|r| {
            Some(Rule {
                id: r.id,
                regex: r.regex?,
                entropy: r.entropy,
                keywords: r.keywords,
                allow: r
                    .allowlists
                    .iter()
                    .flat_map(|a| a.regexes.iter())
                    .filter_map(|re| regex::Regex::new(re).ok())
                    .collect(),
            })
        })
        .collect();
    rules.extend(supplemental_rules());
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_rule_compiles_under_rust_regex() {
        let rules = load_rules();
        assert!(
            rules.len() >= 200,
            "expected the full ruleset, got {}",
            rules.len()
        );
        for r in &rules {
            regex::Regex::new(&r.regex)
                .unwrap_or_else(|e| panic!("rule {} failed to compile: {e}", r.id));
        }
    }
}
