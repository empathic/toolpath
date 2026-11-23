//! RFC.md "Signature Algorithm" › "Canonicalization Rules", "What Gets Signed"
//! and "Canonicalization Example", proven against the canonicalizer the crate
//! signs with (`toolpath::v1::sign::jcs`, RFC 8785 via serde_json_canonicalizer).

use std::path::Path as FsPath;

use serde_json::{Value, json};
use toolpath::v1::Graph;
use toolpath::v1::sign::{digest, jcs, path_author_input, path_reviewer_input, step_author_input};

fn vectors_dir() -> std::path::PathBuf {
    FsPath::new(env!("CARGO_MANIFEST_DIR")).join("tests/jcs-vectors")
}

/// RFC 8785's reference vectors (cyberphone/json-canonicalization, see
/// tests/jcs-vectors/SOURCE.md): every input canonicalizes to its output, byte for byte.
#[test]
fn rfc8785_reference_vectors() {
    let dir = vectors_dir();
    let mut n = 0;
    for entry in std::fs::read_dir(dir.join("input")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_owned();
        let input: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let want = std::fs::read_to_string(dir.join("output").join(&name)).unwrap();
        assert_eq!(jcs(&input), want, "vector {name:?}");
        n += 1;
    }
    assert_eq!(n, 6, "all six vectors ran");
}

/// Rule 1, "Object keys sorted lexicographically by UTF-8 bytes". RFC 8785
/// (§3.2.3) sorts by UTF-16 code units instead. The two orders differ for a
/// key above U+FFFF against one in U+E000–U+FFFF. The canonicalizer follows
/// RFC 8785, as the reference vector `weird.json` requires (😂 U+1F602 sorts
/// before דּ U+FB33). So the RFC.md rule text is wrong; the implementation and
/// the cited standard agree.
#[test]
fn key_order_is_utf16_not_utf8() {
    let v = json!({ "\u{E000}": 1, "\u{1F600}": 2 });
    let out = jcs(&v);
    let smile = out.find('\u{1F600}').unwrap();
    let private = out.find('\u{E000}').unwrap();
    assert!(
        smile < private,
        "UTF-16 order puts U+1F600 (D83D…) first: {out}"
    );
    // UTF-8 byte order would be the opposite:
    assert!("\u{E000}".as_bytes() < "\u{1F600}".as_bytes());
}

/// Rules 3 and 5: only `"`, `\` and U+0000–U+001F are escaped; the five
/// controls with short forms use them (`\b \t \n \f \r`), the rest `\u00xx`
/// in lowercase hex; everything else (incl. U+007F, `/`, non-ASCII) is literal.
#[test]
fn escape_set_is_minimal() {
    for c in 0u32..=0x7F {
        let ch = char::from_u32(c).unwrap();
        let out = jcs(&Value::String(ch.to_string()));
        let body = &out[1..out.len() - 1];
        let want = match ch {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            '\u{8}' => "\\b".into(),
            '\t' => "\\t".into(),
            '\n' => "\\n".into(),
            '\u{c}' => "\\f".into(),
            '\r' => "\\r".into(),
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32),
            c => c.to_string(),
        };
        assert_eq!(body, want, "U+{c:04X}");
    }
    assert_eq!(
        jcs(&json!("€😂")),
        "\"€😂\"",
        "non-ASCII stays literal UTF-8"
    );
}

/// Rule 4: ECMAScript number serialization (RFC 8785 §3.2.2.3).
#[test]
fn numbers_serialize_as_es6() {
    let cases = [
        ("0", "0"),
        ("-0", "0"),
        ("1.0", "1"),
        ("4.50", "4.5"),
        ("100", "100"),
        ("2e-3", "0.002"),
        ("0.000001", "0.000001"),
        ("1e-7", "1e-7"),
        ("1e21", "1e+21"),
        ("1E30", "1e+30"),
        ("123456789012345680000", "123456789012345680000"),
        ("333333333.33333329", "333333333.3333333"),
    ];
    for (input, want) in cases {
        let v: Value = serde_json::from_str(input).unwrap();
        assert_eq!(jcs(&v), want, "{input}");
    }
}

/// The RFC's "Canonicalization Example": the step it prints canonicalizes,
/// without `meta`, to exactly the string it prints.
#[test]
fn rfc_canonicalization_example() {
    let doc: Value = serde_json::from_str(
        r#"{
          "step": { "timestamp": "2026-01-29T10:00:00Z", "id": "step-001", "actor": "human:alex" },
          "change": { "src/main.rs": { "raw": "@@ -1,1 +1,1 @@\n-hello\n+world" } },
          "meta": { "intent": "Fix greeting" }
        }"#,
    )
    .unwrap();
    let step: toolpath::v1::Step = serde_json::from_value(doc).unwrap();
    let want = r#"{"change":{"src/main.rs":{"raw":"@@ -1,1 +1,1 @@\n-hello\n+world"}},"step":{"actor":"human:alex","id":"step-001","timestamp":"2026-01-29T10:00:00Z"}}"#;
    assert_eq!(step_author_input(&step), want);
}

/// "What Gets Signed": the step input excludes `meta`; the path input is the
/// path identity plus the step ids in `steps` order; the reviewer input is
/// head, path id and the review time. All are canonical and SHA-256'd.
#[test]
fn what_gets_signed() {
    let g = Graph::from_json(
        &std::fs::read_to_string(
            FsPath::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/path-03-signed-pr.path.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let path = g.single_path().unwrap();

    // meta never reaches the step input
    let mut stripped = path.steps[0].clone();
    stripped.meta = None;
    assert_eq!(
        step_author_input(&path.steps[0]),
        step_author_input(&stripped)
    );
    assert!(!step_author_input(&path.steps[0]).contains("\"meta\""));

    // path input: path identity + ordered step ids, nothing from steps' content
    let input: Value = serde_json::from_str(&path_author_input(path)).unwrap();
    assert_eq!(
        input.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["path", "step_ids"]
    );
    let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
    assert_eq!(input["step_ids"], json!(ids));
    assert_eq!(input["path"]["id"], json!(path.path.id));
    let mut reordered = path.clone();
    reordered.steps.reverse();
    assert_ne!(
        path_author_input(path),
        path_author_input(&reordered),
        "order is signed"
    );

    assert_eq!(
        path_reviewer_input("pr-42", "step-004", "2026-01-29T16:00:00Z"),
        r#"{"head":"step-004","path_id":"pr-42","reviewed_at":"2026-01-29T16:00:00Z"}"#
    );
    // SHA-256, hex of the RFC example
    let hex: String = digest("abc").iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// jsonl.rs: "signatures computed over canonical JSON remain valid across
/// conversions". Every signable input of every example path is identical
/// after a write-then-read JSONL round trip.
#[test]
fn signed_inputs_survive_jsonl_round_trip() {
    let mut n = 0;
    for (stem, canonical) in example_graphs() {
        let a = canonical.single_path().unwrap();
        let g = Graph::from_jsonl_str(&canonical.to_jsonl_string().unwrap()).unwrap();
        assert_signed_inputs_equal(&stem, a, g.single_path().unwrap()).unwrap();
        n += 1;
    }
    assert!(n >= 4, "examples checked: {n}");
}

/// The committed `examples/*.path.jsonl` should be the same documents as their
/// `.path.json` twins, or a signature over one won't verify against the other.
/// `path-02-local-session` was not (its JSONL `PathOpen` lacked `base.ref`)
/// until 8813140; add a stem here only with a qualifier concern on the file.
#[test]
fn committed_jsonl_examples_match_their_json() {
    const KNOWN_DIFFERENT: &[&str] = &[];
    let ex = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for (stem, canonical) in example_graphs() {
        let stream = ex.join(format!("{stem}.path.jsonl"));
        if !stream.exists() {
            continue;
        }
        let g = Graph::from_jsonl_str(&std::fs::read_to_string(&stream).unwrap()).unwrap();
        let same = assert_signed_inputs_equal(
            &stem,
            canonical.single_path().unwrap(),
            g.single_path().unwrap(),
        );
        assert_eq!(
            same.is_ok(),
            !KNOWN_DIFFERENT.contains(&stem.as_str()),
            "{stem}: {same:?}"
        );
    }
}

fn example_graphs() -> Vec<(String, Graph)> {
    let ex = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&ex).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".path.json") else {
            continue;
        };
        let g = Graph::from_json(&std::fs::read_to_string(&p).unwrap()).unwrap();
        if g.single_path().is_some() {
            out.push((stem.to_string(), g));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn assert_signed_inputs_equal(
    stem: &str,
    a: &toolpath::v1::Path,
    b: &toolpath::v1::Path,
) -> Result<(), String> {
    if path_author_input(a) != path_author_input(b) {
        return Err(format!(
            "{stem}: path input\n {}\n {}",
            path_author_input(a),
            path_author_input(b)
        ));
    }
    if a.steps.len() != b.steps.len() {
        return Err(format!("{stem}: step count"));
    }
    for (x, y) in a.steps.iter().zip(&b.steps) {
        if step_author_input(x) != step_author_input(y) {
            return Err(format!("{stem}: step {}", x.step.id));
        }
    }
    Ok(())
}

/// Property checks over generated values: output re-parses to an equal value,
/// is idempotent, has no whitespace outside strings, and has every object's
/// keys in UTF-16 order.
#[test]
fn properties_over_generated_values() {
    let mut rng = XorShift(0x9e3779b97f4a7c15);
    for _ in 0..2000 {
        let v = gen_value(&mut rng, 4);
        let out = jcs(&v);
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(normalize(&back), normalize(&v), "re-parses equal: {out}");
        assert_eq!(jcs(&back), out, "idempotent");
        assert!(no_whitespace_outside_strings(&out), "whitespace in {out}");
        assert_eq!(out, oracle(&v), "key order and structure");
    }
}

/// An independent writer for structure and key order: objects with keys in
/// UTF-16 code-unit order, no whitespace; scalars as `serde_json` writes them
/// (numbers come from `jcs`, proven separately above).
fn oracle(v: &Value) -> String {
    match v {
        Value::Number(_) => jcs(v),
        Value::Array(a) => format!("[{}]", a.iter().map(oracle).collect::<Vec<_>>().join(",")),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), oracle(&m[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        x => serde_json::to_string(x).unwrap(),
    }
}

fn no_whitespace_outside_strings(s: &str) -> bool {
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c.is_whitespace() {
            return false;
        }
    }
    true
}

/// Numbers compare by value: `1.0` and `1` are the same JSON number.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), normalize(x))).collect())
        }
        x => x.clone(),
    }
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn gen_string(r: &mut XorShift) -> String {
    const POOL: &[char] = &[
        'a',
        'Z',
        '0',
        ' ',
        '"',
        '\\',
        '/',
        '\n',
        '\t',
        '\u{0}',
        '\u{1f}',
        '\u{7f}',
        'é',
        '€',
        '\u{E000}',
        '\u{FB33}',
        '😂',
        '\u{1F600}',
        'ö',
    ];
    (0..r.below(6))
        .map(|_| POOL[r.below(POOL.len() as u64) as usize])
        .collect()
}

fn gen_number(r: &mut XorShift) -> Value {
    match r.below(4) {
        0 => json!(r.below(1_000_000) as i64 - 500_000),
        1 => json!((r.next() >> 11) as f64 / (1u64 << 30) as f64),
        2 => json!(f64::from_bits(r.next() & 0x7fef_ffff_ffff_ffff)),
        _ => json!(10f64.powi(r.below(60) as i32 - 30)),
    }
}

fn gen_value(r: &mut XorShift, depth: u32) -> Value {
    let pick = if depth == 0 { r.below(4) } else { r.below(6) };
    match pick {
        0 => Value::Null,
        1 => Value::Bool(r.below(2) == 1),
        2 => gen_number(r),
        3 => Value::String(gen_string(r)),
        4 => Value::Array((0..r.below(4)).map(|_| gen_value(r, depth - 1)).collect()),
        _ => Value::Object(
            (0..r.below(5))
                .map(|_| (gen_string(r), gen_value(r, depth - 1)))
                .collect(),
        ),
    }
}
