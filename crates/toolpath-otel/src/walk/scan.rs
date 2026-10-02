//! Delimiter-only JSON scanning: where values start and end, nothing more.
//! It validates nothing; callers parse every value it finds with
//! serde_json and fall back to a whole-text parse when a shape is not the
//! simple one recognised here, so a scan never decides what is accepted.

pub fn ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// End (exclusive) of the string whose opening quote is at `i`.
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    loop {
        let k = j + b.get(j..)?.iter().position(|&c| c == b'"' || c == b'\\')?;
        if b[k] == b'\\' {
            j = k + 2;
        } else {
            return Some(k + 1);
        }
    }
}

/// End (exclusive) of the value starting at `i`: a string by its closing
/// quote, a container by bracket depth outside strings, a scalar at the
/// next delimiter.
pub fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match *b.get(j)? {
                    b'"' => {
                        j = string_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let j = i + b[i..]
                .iter()
                .position(|c| matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'))
                .unwrap_or(b.len() - i);
            (j > i).then_some(j)
        }
    }
}

/// The members of a JSON object text, `(key, value text)` in order, when
/// it is one object whose keys are plain strings (no escapes or control
/// characters). `known(key, rest)` may name the length of a value text
/// that `rest` starts with (one read before); it is taken when a delimiter
/// follows, which a value's own bytes then fix as its end.
pub fn members(
    raw: &str,
    mut known: impl FnMut(&str, &[u8]) -> Option<usize>,
) -> Option<Vec<(&str, &str)>> {
    let b = raw.as_bytes();
    let mut i = ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i = ws(b, i + 1);
    let mut out = Vec::new();
    if b.get(i) == Some(&b'}') {
        i += 1;
    } else {
        loop {
            if b.get(i) != Some(&b'"') {
                return None;
            }
            let key_end = string_end(b, i)?;
            let key = &raw[i + 1..key_end - 1];
            if key.bytes().any(|c| c == b'\\' || c < 0x20) {
                return None;
            }
            i = ws(b, key_end);
            if b.get(i) != Some(&b':') {
                return None;
            }
            i = ws(b, i + 1);
            let end = match known(key, &b[i..]).map(|n| i + n) {
                Some(end) if matches!(b.get(ws(b, end)), Some(b',' | b'}')) => end,
                _ => value_end(b, i)?,
            };
            out.push((key, &raw[i..end]));
            i = ws(b, end);
            match b.get(i)? {
                b',' => i = ws(b, i + 1),
                b'}' => {
                    i += 1;
                    break;
                }
                _ => return None,
            }
        }
    }
    (ws(b, i) == b.len()).then_some(out)
}

/// Length of the common prefix of `a` and `b`.
pub fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    for step in [4096, 64] {
        while i + step <= n && a[i..i + step] == b[i..i + step] {
            i += step;
        }
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_end_at_their_delimiters() {
        let s = br#"{"a":"x\"]}","b":[1,{"c":"]"}]} 12,true]"#;
        assert_eq!(value_end(s, 0), Some(31));
        assert_eq!(value_end(s, 5), Some(12));
        assert_eq!(value_end(s, 32), Some(34));
        assert_eq!(value_end(s, 35), Some(39));
        assert_eq!(value_end(b"\"open", 0), None);
        assert_eq!(value_end(b"[[]", 0), None);
        assert_eq!(value_end(b",", 0), None);
    }

    #[test]
    fn members_recognise_plain_objects_only() {
        assert_eq!(
            members(r#" { "a" : 1 , "b":{"x":[2]} ,"a":"s"} "#, |_, _| None),
            Some(vec![("a", "1"), ("b", r#"{"x":[2]}"#), ("a", r#""s""#)])
        );
        assert_eq!(members("{}", |_, _| None), Some(vec![]));
        for odd in [
            r#"{"a\u0062":1}"#,
            r#"{"a":1,}"#,
            r#"{"a":1} x"#,
            r#"[1]"#,
            r#"{"a" 1}"#,
            "{\"a\u{1}\":1}",
        ] {
            assert_eq!(members(odd, |_, _| None), None, "{odd}");
        }
    }

    #[test]
    fn a_known_length_is_taken_only_before_a_delimiter() {
        let raw = r#"{"a":[1,2],"b":"xy"}"#;
        let want = Some(vec![("a", "[1,2]"), ("b", r#""xy""#)]);
        assert_eq!(members(raw, |k, _| (k == "a").then_some(5)), want);
        // A stale length that ends mid-value falls back to the scan.
        assert_eq!(members(raw, |k, _| (k == "a").then_some(3)), want);
        assert_eq!(members(raw, |k, _| (k == "b").then_some(2)), want);
    }

    #[test]
    fn common_prefix_counts_equal_leading_bytes() {
        let a = vec![7u8; 200];
        let mut b = a.clone();
        assert_eq!(common_prefix(&a, &b), 200);
        b[130] = 0;
        assert_eq!(common_prefix(&a, &b), 130);
        let long = vec![1u8; 10_000];
        let mut other = long.clone();
        other[9_000] = 2;
        assert_eq!(common_prefix(&long, &other), 9_000);
        assert_eq!(common_prefix(&a, &a[..5]), 5);
    }
}
