//! The seam (spec): only `src/profile/` may name an attribute key or
//! OpenRouter. Scans the string literals in non-test code of every other
//! source file.

use std::path::{Path, PathBuf};

const KEY_PREFIXES: [&str; 7] = [
    "gen_ai.",
    "trace.metadata",
    "session.id",
    "service.name",
    "llm.",
    "openinference",
    "ai.",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// String literals of `src` outside comments, doc comments, and
/// everything from the first `#[cfg(test)]` on (test modules sit at the
/// end of each file in this crate).
fn literals(src: &str) -> Vec<String> {
    let code = src.split("#[cfg(test)]").next().unwrap_or("");
    let chars: Vec<char> = code.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        if c == '/' && at(i + 1) == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == 'r'
            && matches!(at(i + 1), Some('#' | '"'))
            && (prev.is_none_or(|p| !is_ident(p))
                || (prev == Some('b') && (i < 2 || !is_ident(chars[i - 2]))))
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while at(j) == Some('#') {
                hashes += 1;
                j += 1;
            }
            if at(j) != Some('"') {
                i += 1;
                continue;
            }
            j += 1;
            let start = j;
            while j < chars.len()
                && !(chars[j] == '"' && (0..hashes).all(|k| at(j + 1 + k) == Some('#')))
            {
                j += 1;
            }
            out.push(chars[start..j.min(chars.len())].iter().collect());
            i = j + 1 + hashes;
        } else if c == '"' {
            let mut j = i + 1;
            let mut s = String::new();
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    s.push('\\');
                    if let Some(e) = at(j + 1) {
                        s.push(e);
                    }
                    j += 2;
                } else {
                    s.push(chars[j]);
                    j += 1;
                }
            }
            out.push(s);
            i = j + 1;
        } else if c == '\'' {
            // A char literal ('x', '\n', '"') or a lifetime ('a).
            if at(i + 1) == Some('\\') {
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == Some('\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

fn violations(src: &str) -> Vec<String> {
    literals(src)
        .into_iter()
        .filter(|l| {
            KEY_PREFIXES.iter().any(|p| l.starts_with(p))
                || l.to_ascii_lowercase().contains("openrouter")
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_attribute_key_or_vendor_outside_profiles() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let (profile_dir, tests_dir) = (src.join("profile"), src.join("tests"));
    let mut hits = Vec::new();
    for f in files
        .iter()
        .filter(|f| !f.starts_with(&profile_dir) && !f.starts_with(&tests_dir))
    {
        for v in violations(&std::fs::read_to_string(f).unwrap()) {
            hits.push(format!("{}: {v:?}", f.display()));
        }
    }
    assert!(files.len() > 10, "scanned {} files", files.len());
    assert!(hits.is_empty(), "seam violations:\n{}", hits.join("\n"));
}

#[test]
fn comments_doc_comments_and_test_modules_do_not_trip_the_scan() {
    let src = r####"
//! Reads OpenRouter's "gen_ai.prompt".
/// See "session.id" in OpenRouter docs.
fn f() -> &'static str { let _c = '"'; "plain" } /* "openrouter" */
#[cfg(test)]
mod tests { const K: &str = "gen_ai.response.id"; }
"####;
    assert!(violations(src).is_empty(), "{:?}", violations(src));
    assert_eq!(literals(src), vec!["plain".to_string()]);
}

#[test]
fn real_literals_trip_the_scan() {
    let src = r####"
const A: &str = "gen_ai.response.id";
const B: &str = r#"x OpenRouter y"#;
const C: &[u8] = b"service.name";
const D: &str = "toolpath-otel/v1";
"####;
    assert_eq!(
        violations(src),
        vec!["gen_ai.response.id", "x OpenRouter y", "service.name"]
    );
}
