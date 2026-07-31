//! Rule loading from the vendored ruleset.
//!
//! Vendored from `gitleaks/gitleaks`, `config/gitleaks.toml`, commit
//! `b58d3f102cf3a2c84cb7f923d05c25c9b1aed84b` (2026-07-22). Gitleaks is
//! MIT-licensed (https://github.com/gitleaks/gitleaks/blob/master/LICENSE).
//! `gitleaks.toml` itself is kept byte-verbatim so it can be diffed against
//! upstream; do not hand-edit it.

use aho_corasick::AhoCorasick;
use serde::Deserialize;

const RAW_TOML: &str = include_str!("gitleaks.toml");

/// `regex`'s default 10 MiB program budget is not enough for three of
/// gitleaks' patterns - `generic-api-key` needs ~10.5 MiB,
/// `vault-batch-token` ~14.3 MiB, `pypi-upload-token` ~47.7 MiB. They are
/// valid RE2, just large; without this ceiling they fail to build and the
/// catch-all `generic-api-key` (the only rule that catches an unbranded
/// `SOMETHING_API_KEY = "..."`) is lost.
const REGEX_SIZE_LIMIT: usize = 64 << 20;

pub fn compile(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
}

/// Which text a gitleaks allowlist regex is tested against - upstream's
/// `regexTarget`, whose default is the extracted secret.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AllowTarget {
    #[default]
    Secret,
    Match,
    Line,
}

/// One gitleaks allowlist block: documented exceptions that suppress a
/// match (e.g. AWS's own `...EXAMPLE` key, or `${{ secrets.X }}`).
pub struct Allowlist {
    target: AllowTarget,
    regexes: Vec<regex::Regex>,
    /// Upstream tests stopwords against the secret whatever `regexTarget`
    /// says, and case-insensitively as a substring - hence an automaton
    /// rather than a regex (`generic-api-key` ships ~2000 of them).
    stopwords: Option<AhoCorasick>,
}

impl Allowlist {
    pub fn allows(&self, secret: &str, whole_match: &str, line: &str) -> bool {
        let target = match self.target {
            AllowTarget::Secret => secret,
            AllowTarget::Match => whole_match,
            AllowTarget::Line => line,
        };
        self.regexes.iter().any(|r| r.is_match(target))
            || self.stopwords.as_ref().is_some_and(|s| s.is_match(secret))
    }
}

/// One rule, with its allowlists already compiled. `regex` stays a `String`
/// (not a compiled `Regex`) because [`load_rules`] is a pure parse step
/// exercised on its own by the compile-guard test; the caller compiles it.
pub struct Rule {
    pub id: String,
    pub regex: String,
    pub entropy: Option<f64>,
    pub keywords: Vec<String>,
    /// Gitleaks' `secretGroup`: which capture group holds the credential
    /// when group 1 is context instead. `sonar-api-token`'s group 1 is the
    /// literal `(login|token)`, so without this the word "token" is
    /// redacted and `squ_...` survives. `Some(0)` means the whole match.
    pub secret_group: Option<usize>,
    pub allow: Vec<Allowlist>,
}

/// The vendored ruleset plus the file-level allowlist that applies to all
/// of it.
pub struct Ruleset {
    pub rules: Vec<Rule>,
    /// Gitleaks' global `[allowlist]`: placeholder shapes (`$VAR`,
    /// `${{ secrets.X }}`, `%VAR%`, printf verbs, `true|false|null`) that
    /// are never credentials whichever rule matched them.
    pub global: Vec<Allowlist>,
}

#[derive(Deserialize)]
struct RulesFile {
    allowlist: Option<RawAllowlist>,
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
    #[serde(rename = "secretGroup")]
    secret_group: Option<usize>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    allowlists: Vec<RawAllowlist>,
}

#[derive(Deserialize)]
struct RawAllowlist {
    condition: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(rename = "regexTarget", default)]
    regex_target: AllowTarget,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    stopwords: Vec<String>,
}

/// Allowlist *regexes* the vendored ruleset carries that this crate
/// deliberately refuses to honour.
///
/// Gitleaks scans source trees, where AWS's own documentation key quoted in
/// a README is noise worth silencing. `p redact` scans a transcript the user
/// is about to publish, and the question it answers there is "does this look
/// like a credential", not "is this particular literal live" - a session that
/// pasted a key-shaped string gets it removed either way, because nothing in
/// a transcript distinguishes a copied placeholder from a real key whose
/// owner happened to end it in `EXAMPLE`. False-positive cost is a marker in
/// a document; false-negative cost is a published credential.
///
/// Scoped to regexes on purpose. The stopword lists stay, because they are
/// the only thing keeping `generic-api-key` from flagging every
/// `name = "value"` pair in the transcript and drowning the plan a human has
/// to review.
const SUPPRESSED_ALLOWLIST_REGEXES: &[&str] = &[
    // `aws-access-token`, covering `AKIAIOSFODNN7EXAMPLE` and friends.
    r".+EXAMPLE$",
];

impl RawAllowlist {
    /// Compiled best-effort: an entry that fails under `regex` is dropped
    /// rather than failing the whole rule, since an allowlist is a
    /// false-positive refinement, not the detection itself.
    ///
    /// A `paths` criterion is unevaluable here - this detector scans a
    /// transcript field, not a file - so it is ignored, which under the
    /// default OR condition only ever makes this crate report *more* than
    /// upstream. `condition = "AND"` inverts that: every criterion the
    /// block sets must hold at once, so honouring the evaluable ones alone
    /// would suppress matches upstream reports. Such a block is dropped
    /// whole, except where it sets a single criterion and AND therefore
    /// means exactly what OR does.
    fn compile(self) -> Option<Allowlist> {
        let criteria = [&self.paths, &self.regexes, &self.stopwords]
            .into_iter()
            .filter(|c| !c.is_empty())
            .count();
        if self.condition.as_deref() == Some("AND") && criteria > 1 {
            return None;
        }
        let regexes: Vec<regex::Regex> = self
            .regexes
            .iter()
            .filter(|r| !SUPPRESSED_ALLOWLIST_REGEXES.contains(&r.as_str()))
            .filter_map(|r| compile(r).ok())
            .collect();
        let stopwords = if self.stopwords.is_empty() {
            None
        } else {
            AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build(&self.stopwords)
                .ok()
        };
        if regexes.is_empty() && stopwords.is_none() {
            return None;
        }
        Some(Allowlist {
            target: self.regex_target,
            regexes,
            stopwords,
        })
    }
}

/// Rules whose credential is the whole match even though upstream carries
/// no `secretGroup`. `jwt-base64`'s group 1 is the `(?P<alg>...)` header
/// discriminator (redacting it eats ten bytes from the middle of the
/// token) and `microsoft-teams-webhook`'s is a repeated `([a-z0-9]{4}-)`
/// chunk (redacting it leaves the whole webhook URL in place).
const WHOLE_MATCH_IS_THE_SECRET: &[&str] = &["jwt-base64", "microsoft-teams-webhook"];

/// Hand-written rules filling gaps the vendored ruleset leaves open for
/// this crate's purposes: gitleaks matches a PEM block only with its full
/// closing footer, and matches a JWT only above a claim-length floor that
/// misses short tokens; URI-embedded credentials aren't a gitleaks rule at
/// all (it scans files, not structured connection strings); and its
/// Anthropic rules are pinned to a live key's exact length.
fn supplemental_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "pem-private-key-header".to_string(),
            regex: r"-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----".to_string(),
            entropy: None,
            keywords: vec!["-----begin".to_string()],
            secret_group: None,
            allow: Vec::new(),
        },
        Rule {
            id: "jwt-compact".to_string(),
            regex: r"\bey[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]{2,}\b".to_string(),
            entropy: None,
            keywords: vec!["ey".to_string()],
            secret_group: None,
            allow: Vec::new(),
        },
        Rule {
            id: "uri-credential".to_string(),
            regex: r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:@/]+:([^\s:@/]+)@[^\s/]+".to_string(),
            entropy: None,
            keywords: vec!["://".to_string()],
            secret_group: None,
            allow: Vec::new(),
        },
        // Vendored `anthropic-api-key`/`anthropic-admin-api-key` demand
        // exactly 93 body characters and a trailing `AA` - correct for a
        // live key, and blind to every abbreviated or elided one a
        // transcript actually carries. The only other rule that reaches
        // such a key is `generic-api-key`, whose stopword list contains
        // `ant-`, so *every* `sk-ant-` key is suppressed there by the
        // prefix alone. Match on the shape instead of the length: the
        // `sk-ant-` prefix is specific enough that a false positive costs
        // one marker.
        Rule {
            id: "anthropic-api-key-loose".to_string(),
            regex: r"\bsk-ant-[A-Za-z0-9]+-[A-Za-z0-9_-]{16,}\b".to_string(),
            entropy: None,
            keywords: vec!["sk-ant-".to_string()],
            secret_group: None,
            allow: Vec::new(),
        },
    ]
}

/// Extra allowlist entries this crate adds to a vendored rule, because the
/// corpus it scans (agent transcripts) contains a lookalike that gitleaks'
/// corpus (source trees) does not.
fn local_allowlists(id: &str) -> Vec<Allowlist> {
    // `sourcegraph-access-token` accepts a bare 40-hex string, which is
    // byte-identical to a git SHA-1. Transcripts are full of commit hashes
    // and near a word like "token" one scores 1.000, so the unprefixed form
    // is dropped; the `sgp_`-prefixed forms stay covered.
    if id != "sourcegraph-access-token" {
        return Vec::new();
    }
    vec![Allowlist {
        target: AllowTarget::Secret,
        regexes: vec![compile(r"^[a-fA-F0-9]{40}$").expect("literal pattern")],
        stopwords: None,
    }]
}

/// Parses the vendored TOML plus [`supplemental_rules`]. Pure and
/// deterministic - no I/O beyond the `include_str!` baked in at compile
/// time.
pub fn load_rules() -> Ruleset {
    let parsed: RulesFile = toml::from_str(RAW_TOML).expect("vendored gitleaks.toml must parse");
    let mut rules: Vec<Rule> = parsed
        .rules
        .into_iter()
        .filter_map(|r| {
            let mut allow: Vec<Allowlist> = r
                .allowlists
                .into_iter()
                .filter_map(|a| a.compile())
                .collect();
            allow.extend(local_allowlists(&r.id));
            Some(Rule {
                regex: r.regex?,
                entropy: r.entropy,
                secret_group: r.secret_group.or_else(|| {
                    WHOLE_MATCH_IS_THE_SECRET
                        .contains(&r.id.as_str())
                        .then_some(0)
                }),
                keywords: r.keywords,
                id: r.id,
                allow,
            })
        })
        .collect();
    rules.extend(supplemental_rules());
    Ruleset {
        rules,
        global: parsed
            .allowlist
            .into_iter()
            .filter_map(|a| a.compile())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vendored file, at gitleaks commit
    /// `b58d3f102cf3a2c84cb7f923d05c25c9b1aed84b`, holds 222 `[[rules]]`,
    /// one of which (`pkcs12-file`) matches a path rather than content and
    /// is dropped, plus this crate's four supplemental rules. Asserting
    /// the exact count makes a vendor bump a deliberate edit here instead
    /// of a silent drop in detection coverage.
    const EXPECTED_RULE_COUNT: usize = 225;

    #[test]
    fn every_vendored_rule_compiles_under_rust_regex() {
        let ruleset = load_rules();
        assert_eq!(ruleset.rules.len(), EXPECTED_RULE_COUNT);
        for r in &ruleset.rules {
            compile(&r.regex).unwrap_or_else(|e| panic!("rule {} failed to compile: {e}", r.id));
        }
    }

    /// The three largest patterns are the reason [`REGEX_SIZE_LIMIT`]
    /// exists; if a vendor bump renames one, the limit could be lowered
    /// back to the default without anyone noticing.
    #[test]
    fn the_oversized_rules_are_present_and_need_the_raised_size_limit() {
        let ruleset = load_rules();
        for id in ["generic-api-key", "pypi-upload-token", "vault-batch-token"] {
            let rule = ruleset
                .rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} is no longer in the vendored ruleset"));
            assert!(
                regex::Regex::new(&rule.regex).is_err(),
                "{id} now fits the default size limit"
            );
            compile(&rule.regex).unwrap_or_else(|e| panic!("{id} exceeds even 64 MiB: {e}"));
        }
    }

    #[test]
    fn secret_group_is_read_from_the_vendored_file() {
        let ruleset = load_rules();
        let sonar = ruleset
            .rules
            .iter()
            .find(|r| r.id == "sonar-api-token")
            .expect("sonar-api-token is vendored");
        assert_eq!(sonar.secret_group, Some(2));
    }

    #[test]
    fn unannotated_whole_match_rules_get_group_zero() {
        let ruleset = load_rules();
        for id in WHOLE_MATCH_IS_THE_SECRET {
            let rule = ruleset
                .rules
                .iter()
                .find(|r| &r.id == id)
                .unwrap_or_else(|| panic!("{id} is no longer in the vendored ruleset"));
            assert_eq!(rule.secret_group, Some(0), "{id}");
        }
    }

    #[test]
    fn the_global_allowlist_is_loaded_and_suppresses_placeholders() {
        let ruleset = load_rules();
        assert!(!ruleset.global.is_empty());
        for placeholder in ["${{ secrets.MY_TOKEN }}", "$MY_TOKEN", "%MY_TOKEN%", "true"] {
            assert!(
                ruleset.global.iter().any(|a| a.allows(placeholder, "", "")),
                "global allowlist missed {placeholder}"
            );
        }
    }

    #[test]
    fn generic_api_key_stopwords_are_loaded() {
        let ruleset = load_rules();
        let rule = ruleset
            .rules
            .iter()
            .find(|r| r.id == "generic-api-key")
            .expect("generic-api-key is vendored");
        assert!(
            rule.allow
                .iter()
                .any(|a| a.allows("my-adapter-value", "", ""))
        );
        assert!(
            !rule
                .allow
                .iter()
                .any(|a| a.allows("Zt9xQw3pLm7RvB2k", "", ""))
        );
    }

    /// The `condition = "AND"` block on `generic-api-key` needs a matching
    /// file path, which a text detector never has. Applying its `line`
    /// regexes anyway would silently suppress every `LICENSE = "..."` line.
    #[test]
    fn path_conditioned_allowlists_are_dropped() {
        let ruleset = load_rules();
        let rule = ruleset
            .rules
            .iter()
            .find(|r| r.id == "generic-api-key")
            .expect("generic-api-key is vendored");
        assert!(
            !rule
                .allow
                .iter()
                .any(|a| a.allows("", "", r#"LICENSE = "MIT-and-then-some""#)),
            "the bitbake AND-conditioned allowlist is still being applied"
        );
    }

    /// Pins the [`SUPPRESSED_ALLOWLIST_REGEXES`] decision on both sides: the
    /// documentation-key exception is gone, and the stopwords that share its
    /// rule set are untouched.
    #[test]
    fn documentation_key_allowlists_are_suppressed_but_stopwords_survive() {
        // A vendor bump that rewrites the pattern would silently stop
        // matching this list, and doc-shaped keys would survive again.
        for pattern in SUPPRESSED_ALLOWLIST_REGEXES {
            assert!(
                RAW_TOML.contains(pattern),
                "{pattern} is no longer in the vendored ruleset"
            );
        }
        let ruleset = load_rules();
        let aws = ruleset
            .rules
            .iter()
            .find(|r| r.id == "aws-access-token")
            .expect("aws-access-token is vendored");
        assert!(
            aws.allow.is_empty(),
            "the `.+EXAMPLE$` allowlist is still being applied"
        );
        let generic = ruleset
            .rules
            .iter()
            .find(|r| r.id == "generic-api-key")
            .expect("generic-api-key is vendored");
        assert!(
            generic
                .allow
                .iter()
                .any(|a| a.allows("my-adapter-value", "", "")),
            "suppressing regexes took the stopword machinery with it"
        );
    }

    /// The vendored Anthropic rules are length-pinned, so the supplement is
    /// the only thing covering a key that was shortened on its way into a
    /// transcript.
    #[test]
    fn the_loose_anthropic_rule_covers_what_the_vendored_one_misses() {
        let ruleset = load_rules();
        let find = |id: &str| {
            ruleset
                .rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} is missing"))
        };
        let loose = compile(&find("anthropic-api-key-loose").regex).expect("literal pattern");
        let vendored = compile(&find("anthropic-api-key").regex).expect("vendored pattern");

        let short = "sk-ant-api03-EXAMPLEONLYnotarealkey000000000000000000000AA";
        assert_eq!(loose.find(short).map(|m| m.as_str()), Some(short));
        assert!(
            vendored.find(short).is_none(),
            "the vendored rule now covers short keys; the supplement may be redundant"
        );
        assert!(loose.is_match("sk-ant-admin01-Zt9xQw3pLm7RvB2kNc5YdA8j"));
        // The prefix on its own is a format name, not a credential.
        assert!(!loose.is_match("sk-ant-api03-short"));
    }

    #[test]
    fn line_targeted_allowlists_test_the_line_not_the_secret() {
        let ruleset = load_rules();
        let rule = ruleset
            .rules
            .iter()
            .find(|r| r.id == "generic-api-key")
            .expect("generic-api-key is vendored");
        let secret = "Zt9xQw3pLm7RvB2k";
        let line = "RUN --mount=type=secret,id=npm npm install";
        assert!(rule.allow.iter().any(|a| a.allows(secret, "", line)));
        assert!(
            !rule
                .allow
                .iter()
                .any(|a| a.allows(secret, "", "npm install"))
        );
    }
}
