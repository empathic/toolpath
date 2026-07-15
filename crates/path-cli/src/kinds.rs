//! Bundled document-kind specs and semver-prefix matching.
//!
//! The binary ships a copy of every kind spec it knows about so that
//! `path kind` and `path query --kind` work offline. [`BUNDLED_KINDS`] is the
//! single source of truth for which `(name, version)` specs are baked in;
//! [`crate::schema`] (kind-aware validation) and the query layer both read it.
//!
//! A `meta.kind` value is a semver-versioned URI of the form
//! `…/kinds/<name>/v<major>.<minor>.<patch>`. A [`KindSelector`] matches a
//! *prefix* of `(name, major, minor, patch)`: a bare name matches any version,
//! `v1` matches `v1.*.*`, `v1.0` matches `v1.0.*`, and a full triple matches
//! exactly. Matching compares parsed integer tuples, so `v1` matches `v1.9.0`
//! but keeps `v10.0.0` separate.

/// A kind spec compiled into the binary.
pub struct BundledKind {
    /// Kind name, e.g. `agent-coding-session`.
    pub name: &'static str,
    /// Version segment, e.g. `v1.1.0`.
    pub version: &'static str,
    /// Full `meta.kind` URI this spec answers to.
    pub uri: &'static str,
    /// The bundled `schema.json` source.
    pub schema: &'static str,
}

/// Every kind spec baked into the binary, oldest version first.
///
/// The `schema.json` files live at `crates/path-cli/kinds/<name>/<version>/`
/// and publish under `https://toolpath.net/kinds/`.
pub const BUNDLED_KINDS: &[BundledKind] = &[
    BundledKind {
        name: "agent-coding-session",
        version: "v1.0.0",
        uri: toolpath::v1::PATH_KIND_AGENT_CODING_SESSION_V1_0_0,
        schema: include_str!("../kinds/agent-coding-session/v1.0.0/schema.json"),
    },
    BundledKind {
        name: "agent-coding-session",
        version: "v1.1.0",
        uri: toolpath::v1::PATH_KIND_AGENT_CODING_SESSION_V1_1_0,
        schema: include_str!("../kinds/agent-coding-session/v1.1.0/schema.json"),
    },
    BundledKind {
        name: "agent-coding-session",
        version: "v1.2.0",
        uri: toolpath::v1::PATH_KIND_AGENT_CODING_SESSION,
        schema: include_str!("../kinds/agent-coding-session/v1.2.0/schema.json"),
    },
];

/// A parsed `(major, minor, patch)` version triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

fn parse_version(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
}

/// Parse a full `meta.kind` URI into its `(name, version)`.
///
/// Returns `None` if it doesn't look like `…/<name>/v<major>.<minor>.<patch>`.
pub fn parse_kind_uri(uri: &str) -> Option<(String, Version)> {
    let tail = uri.rsplit_once("/kinds/").map_or(uri, |(_, t)| t);
    let tail = tail.trim_end_matches('/');
    let (name, ver) = tail.rsplit_once('/')?;
    let version = parse_version(ver)?;
    Some((name.to_string(), version))
}

/// A `--kind` / `path kind` selector: a name plus an optional version prefix.
#[derive(Debug, Clone)]
pub struct KindSelector {
    name: String,
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    /// A version segment was present but didn't parse as an integer (e.g. a
    /// typo like `/vgarbage` or `/v1.x`). Such a selector matches nothing —
    /// failing *closed* rather than silently widening to "any version".
    impossible: bool,
}

/// Parse a version segment: absent → `None` (wildcard); present and numeric →
/// `Some(n)`; present but non-numeric → `None` and flag the selector impossible.
fn parse_seg(part: Option<&str>, impossible: &mut bool) -> Option<u64> {
    match part {
        None => None,
        Some(s) => match s.parse() {
            Ok(n) => Some(n),
            Err(_) => {
                *impossible = true;
                None
            }
        },
    }
}

/// Parse a selector. Accepts a bare name (`agent-coding-session`), a
/// name + version prefix (`agent-coding-session/v1`, `.../v1.0`,
/// `.../v1.0.0`, with the `v` optional), or a full `meta.kind` URI.
///
/// A version part that's present but unparseable does **not** fail open: the
/// selector is marked impossible and matches nothing, so a typo'd pin fails
/// closed (and `path kind` reports "no bundled spec") rather than silently
/// widening to every version.
pub fn parse_kind_selector(s: &str) -> KindSelector {
    let tail = s.rsplit_once("/kinds/").map_or(s, |(_, t)| t);
    let tail = tail.trim_end_matches('/');
    match tail.split_once('/') {
        None => KindSelector {
            name: tail.to_string(),
            major: None,
            minor: None,
            patch: None,
            impossible: false,
        },
        Some((name, ver)) => {
            let ver = ver.strip_prefix('v').unwrap_or(ver);
            let mut parts = ver.split('.');
            let mut impossible = false;
            let major = parse_seg(parts.next(), &mut impossible);
            let minor = parse_seg(parts.next(), &mut impossible);
            let patch = parse_seg(parts.next(), &mut impossible);
            if parts.next().is_some() {
                impossible = true; // more than major.minor.patch
            }
            KindSelector {
                name: name.to_string(),
                major,
                minor,
                patch,
                impossible,
            }
        }
    }
}

impl KindSelector {
    /// Whether this selector matches a parsed `(name, version)`.
    pub fn matches(&self, name: &str, v: Version) -> bool {
        !self.impossible
            && self.name == name
            && self.major.is_none_or(|m| m == v.major)
            && self.minor.is_none_or(|m| m == v.minor)
            && self.patch.is_none_or(|p| p == v.patch)
    }

    /// Whether this selector matches a `meta.kind` URI. A URI that doesn't
    /// parse as a versioned kind never matches.
    pub fn matches_uri(&self, uri: &str) -> bool {
        parse_kind_uri(uri).is_some_and(|(name, v)| self.matches(&name, v))
    }
}

/// Resolve a selector to the newest bundled kind spec it matches.
pub fn resolve(selector: &str) -> Option<&'static BundledKind> {
    let sel = parse_kind_selector(selector);
    BUNDLED_KINDS
        .iter()
        .filter(|k| parse_kind_uri(k.uri).is_some_and(|(name, v)| sel.matches(&name, v)))
        .max_by_key(|k| parse_kind_uri(k.uri).map(|(_, v)| v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn bundled_kind_uris_parse() {
        for k in BUNDLED_KINDS {
            let (name, ver) = parse_kind_uri(k.uri).expect("bundled URI parses");
            assert_eq!(name, k.name);
            assert_eq!(
                format!("v{}.{}.{}", ver.major, ver.minor, ver.patch),
                k.version
            );
        }
    }

    #[test]
    fn parse_uri_extracts_name_and_version() {
        let (name, ver) =
            parse_kind_uri("https://toolpath.net/kinds/agent-coding-session/v1.1.0").unwrap();
        assert_eq!(name, "agent-coding-session");
        assert_eq!(ver, v(1, 1, 0));
    }

    #[test]
    fn parse_uri_rejects_unversioned() {
        assert!(parse_kind_uri("https://toolpath.net/kinds/agent-coding-session").is_none());
    }

    #[test]
    fn bare_name_matches_any_version() {
        let sel = parse_kind_selector("agent-coding-session");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
        assert!(sel.matches("agent-coding-session", v(2, 5, 9)));
        assert!(!sel.matches("other-kind", v(1, 0, 0)));
    }

    #[test]
    fn major_prefix_keeps_v1_and_v10_distinct() {
        let sel = parse_kind_selector("agent-coding-session/v1");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
        assert!(sel.matches("agent-coding-session", v(1, 9, 0)));
        assert!(!sel.matches("agent-coding-session", v(10, 0, 0)));
    }

    #[test]
    fn minor_prefix_pins_major_minor() {
        let sel = parse_kind_selector("agent-coding-session/v1.0");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
        assert!(sel.matches("agent-coding-session", v(1, 0, 9)));
        assert!(!sel.matches("agent-coding-session", v(1, 1, 0)));
    }

    #[test]
    fn full_triple_matches_exactly() {
        let sel = parse_kind_selector("agent-coding-session/v1.0.0");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
        assert!(!sel.matches("agent-coding-session", v(1, 0, 1)));
    }

    #[test]
    fn v_prefix_is_optional() {
        let sel = parse_kind_selector("agent-coding-session/1.0");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
    }

    #[test]
    fn full_uri_selector_matches_exactly() {
        let sel = parse_kind_selector("https://toolpath.net/kinds/agent-coding-session/v1.0.0");
        assert!(sel.matches("agent-coding-session", v(1, 0, 0)));
        assert!(!sel.matches("agent-coding-session", v(1, 1, 0)));
    }

    #[test]
    fn resolve_picks_newest_for_bare_name() {
        let k = resolve("agent-coding-session").expect("bundled");
        assert_eq!(k.version, "v1.2.0");
    }

    #[test]
    fn resolve_pins_exact_version() {
        let k = resolve("agent-coding-session/v1.0.0").expect("bundled");
        assert_eq!(k.version, "v1.0.0");
    }

    #[test]
    fn resolve_unknown_is_none() {
        assert!(resolve("no-such-kind").is_none());
    }

    #[test]
    fn unparseable_version_fails_closed_not_open() {
        // A typo'd version pin must match nothing — never silently widen to
        // "any version". (Regression for the fail-open selector.)
        for bad in [
            "agent-coding-session/vgarbage",
            "agent-coding-session/v1.x",
            "agent-coding-session/v1.2.3.4",
        ] {
            let sel = parse_kind_selector(bad);
            assert!(
                !sel.matches("agent-coding-session", v(1, 1, 0)),
                "`{bad}` must not match"
            );
            assert!(
                resolve(bad).is_none(),
                "`{bad}` must not resolve to a schema"
            );
        }
    }
}
