//! Read-scoped memos. Every full-history request repeats the session's
//! prompt so far, so one read sees each message many times: a prompt that
//! extends one read before reuses its messages by a byte comparison, each
//! distinct message text is parsed once, and repeated large
//! values (tool lists) are validated and digested once. Nothing outlives
//! the read call.
//!
//! Every shortcut answers exactly what a whole-text serde_json parse
//! answers, or declines (`None`) so the caller runs that parse.

use super::scan;
use crate::generation::Message;
use crate::hash::{canonical_json, sha256_hex};
use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::marker::PhantomData;

/// The prompt form the shortcut handles: one `messages` key, written as
/// OpenRouter writes it.
const PROMPT_HEAD: &str = r#"{"messages":["#;
/// Prompt bytes that pick candidate earlier prompts.
const PROMPT_KEY_BYTES: usize = 256;
/// Earlier prompts compared per candidate key, and member texts kept per
/// member name.
const CANDIDATES: usize = 4;
/// Member texts shorter than this are scanned, not remembered.
const REMEMBER_BYTES: usize = 1024;

#[derive(Default)]
pub struct ReadCx<'a> {
    by_print: HashMap<u64, Vec<usize>>,
    messages: Vec<Memoized<'a>>,
    prompts: Vec<Prompt<'a>>,
    prompts_by_key: HashMap<u64, Vec<usize>>,
    values: HashMap<(u64, usize), Vec<usize>>,
    checked: Vec<Checked<'a>>,
    /// Large member value texts by member name, newest last.
    recent: HashMap<&'a str, Vec<&'a str>>,
}

struct Memoized<'a> {
    raw: &'a str,
    message: Message,
}

/// A prompt read through the shortcut: where each message ends, and its
/// memo entry.
struct Prompt<'a> {
    raw: &'a str,
    ends: Vec<usize>,
    entries: Vec<usize>,
}

/// A value text that parsed at a nesting depth; the parse is kept when
/// asked for, with its digest computed on first use.
struct Checked<'a> {
    raw: &'a str,
    value: Option<Value>,
    digest: OnceCell<String>,
}

/// Length and sampled bytes; equal texts always share it, and every memo
/// compares the whole text before reuse.
fn fingerprint(b: &[u8]) -> u64 {
    let n = b.len();
    let mid = n / 2;
    let mut h = DefaultHasher::new();
    h.write_usize(n);
    h.write(&b[..n.min(32)]);
    h.write(&b[mid.saturating_sub(16)..(mid + 16).min(n)]);
    h.write(&b[n.saturating_sub(32)..]);
    h.finish()
}

/// `raw` parsed as `T` the way a whole-text parse meets it `depth`
/// containers deep, so serde_json's recursion limit falls in the same place.
pub(crate) fn parse_at<T: for<'de> Deserialize<'de>>(raw: &str, depth: usize) -> Option<T> {
    let text = format!("{}{raw}{}", "[".repeat(depth), "]".repeat(depth));
    let mut de = serde_json::Deserializer::from_str(&text);
    let v = Nested::<T>(depth, PhantomData).deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(v)
}

/// `T` inside `.0` one-element arrays.
struct Nested<T>(usize, PhantomData<T>);

impl<'de, T: for<'a> Deserialize<'a>> DeserializeSeed<'de> for Nested<T> {
    type Value = T;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<T, D::Error> {
        if self.0 == 0 {
            return T::deserialize(d);
        }
        d.deserialize_seq(self)
    }
}

impl<'de, T: for<'a> Deserialize<'a>> Visitor<'de> for Nested<T> {
    type Value = T;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a one-element array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
        let v = seq
            .next_element_seed(Nested::<T>(self.0 - 1, PhantomData))?
            .ok_or_else(|| de::Error::invalid_length(0, &"one element"))?;
        match seq.next_element::<de::IgnoredAny>()? {
            None => Ok(v),
            Some(_) => Err(de::Error::invalid_length(2, &"one element")),
        }
    }
}

/// Accepts exactly what parsing into a `Value` accepts, keeping nothing.
struct Strict;

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_str<E>(self, _: &str) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_unit<E>(self) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
        while seq.next_element::<Strict>()?.is_some() {}
        Ok(Strict)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
        while map.next_key::<Strict>()?.is_some() {
            map.next_value::<Strict>()?;
        }
        Ok(Strict)
    }
}

impl<'a> ReadCx<'a> {
    /// A message's memo entry, parsing it at prompt depth on first sight.
    fn entry(&mut self, raw: &'a str) -> Option<usize> {
        let print = fingerprint(raw.as_bytes());
        if let Some(&i) = self
            .by_print
            .get(&print)
            .and_then(|ix| ix.iter().find(|&&i| self.messages[i].raw == raw))
        {
            return Some(i);
        }
        let message: Message = parse_at(raw, 2)?;
        let i = self.messages.len();
        self.by_print.entry(print).or_default().push(i);
        self.messages.push(Memoized { raw, message });
        Some(i)
    }

    /// The messages of a `{"messages":[…]}` prompt text,
    /// as parsing the text into `{messages: Vec<Message>}` gives them.
    /// `None` when the text is not in that exact form or does not parse;
    /// the caller then parses the whole text.
    pub fn prompt(&mut self, raw: &'a str) -> Option<Vec<Message>> {
        if !raw.starts_with(PROMPT_HEAD) {
            return None;
        }
        let b = raw.as_bytes();
        let key = fingerprint(&b[..b.len().min(PROMPT_HEAD.len() + PROMPT_KEY_BYTES)]);
        // The earlier prompt sharing the most whole leading messages (newest
        // first; one with no more messages than the best so far cannot do
        // better); their bytes are equal, so their parses are too.
        let mut ends = Vec::new();
        let mut entries = Vec::new();
        for &p in self.prompts_by_key.get(&key).into_iter().flatten().rev() {
            let prev = &self.prompts[p];
            if prev.ends.len() <= ends.len() {
                continue;
            }
            let common = scan::common_prefix(b, prev.raw.as_bytes());
            let shared = prev.ends.partition_point(|&e| e <= common);
            if shared > ends.len() {
                ends = prev.ends[..shared].to_vec();
                entries = prev.entries[..shared].to_vec();
            }
        }
        let mut i = ends.last().copied().unwrap_or(PROMPT_HEAD.len());
        let mut first = ends.is_empty();
        loop {
            i = scan::ws(b, i);
            match *b.get(i)? {
                b']' => break,
                b',' if !first => i = scan::ws(b, i + 1),
                _ if first => {}
                _ => return None,
            }
            let end = scan::value_end(b, i)?;
            entries.push(self.entry(&raw[i..end])?);
            ends.push(end);
            i = end;
            first = false;
        }
        i = scan::ws(b, i + 1);
        if b.get(i) != Some(&b'}') || scan::ws(b, i + 1) != b.len() {
            return None;
        }
        let out = entries
            .iter()
            .map(|&e| self.messages[e].message.clone())
            .collect();
        let keyed = self.prompts_by_key.entry(key).or_default();
        if keyed.len() == CANDIDATES {
            keyed.remove(0);
        }
        keyed.push(self.prompts.len());
        self.prompts.push(Prompt { raw, ends, entries });
        Some(out)
    }

    /// The length of a remembered `key` member text that `rest` starts with.
    pub fn known(&self, key: &str, rest: &[u8]) -> Option<usize> {
        self.recent
            .get(key)?
            .iter()
            .rev()
            .find(|v| rest.starts_with(v.as_bytes()))
            .map(|v| v.len())
    }

    /// Remember a large member text that parsed, for [`ReadCx::known`].
    pub fn remember(&mut self, key: &'a str, value: &'a str) {
        if value.len() < REMEMBER_BYTES {
            return;
        }
        let texts = self.recent.entry(key).or_default();
        if texts.contains(&value) {
            return;
        }
        if texts.len() == CANDIDATES {
            texts.remove(0);
        }
        texts.push(value);
    }

    /// Whether a value text parses (into a `Value`) at `depth`; memoized
    /// by text.
    pub fn check(&mut self, raw: &'a str, depth: usize) -> bool {
        self.checked_entry(raw, depth, false).is_some()
    }

    /// The sha256 hex of the canonical JSON of a value text parsed at
    /// `depth`; `None` when it does not parse. Memoized by text.
    pub fn value_digest(&mut self, raw: &'a str, depth: usize) -> Option<String> {
        let i = self.checked_entry(raw, depth, true)?;
        let c = &self.checked[i];
        let v = c.value.as_ref()?;
        Some(
            c.digest
                .get_or_init(|| sha256_hex(&[canonical_json(v).as_bytes()]))
                .clone(),
        )
    }

    fn checked_entry(&mut self, raw: &'a str, depth: usize, keep: bool) -> Option<usize> {
        let key = (fingerprint(raw.as_bytes()), depth);
        let found = self
            .values
            .get(&key)
            .and_then(|ix| ix.iter().copied().find(|&i| self.checked[i].raw == raw));
        if let Some(i) = found {
            if keep && self.checked[i].value.is_none() {
                self.checked[i].value = Some(parse_at(raw, depth)?);
            }
            return Some(i);
        }
        let value = if keep {
            Some(parse_at::<Value>(raw, depth)?)
        } else {
            parse_at::<Strict>(raw, depth)?;
            None
        };
        let i = self.checked.len();
        self.values.entry(key).or_default().push(i);
        self.checked.push(Checked {
            raw,
            value,
            digest: OnceCell::new(),
        });
        Some(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_at_counts_depth_like_an_enclosing_parse() {
        let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
        let mut both = (false, false);
        for n in 120..130 {
            let enclosed = serde_json::from_str::<Value>(&format!("[[{}]]", deep(n))).ok();
            let at = parse_at::<Value>(&deep(n), 2);
            assert_eq!(at, enclosed.map(|v| v[0][0].clone()), "{n}");
            assert_eq!(
                parse_at::<Strict>(&deep(n), 2).is_some(),
                at.is_some(),
                "{n}"
            );
            both = (both.0 || at.is_some(), both.1 || at.is_none());
        }
        assert_eq!(both, (true, true), "the limit falls inside the range");
    }

    #[test]
    fn strict_accepts_what_value_accepts() {
        for text in [
            r#"{"a":[1,2.5,-0.0,1e2,"\u00e9\ud83d\ude00"],"a":null}"#,
            r#""\ud800""#,
            r#""\x""#,
            "1e400",
            "18446744073709551616",
            "-",
            "\"\u{1}\"",
            "[1,]",
            "{\"k\":1",
            "tru",
            " 1 ",
        ] {
            assert_eq!(
                parse_at::<Strict>(text, 1).is_some(),
                serde_json::from_str::<Value>(text).is_ok(),
                "{text}"
            );
        }
    }

    #[test]
    fn value_digest_equals_the_direct_digest_and_reuses() {
        let texts = [r#"{"b":1,"a":[1.0,"é"]}"#, "1", "1.0", "-0.0", "0.0"];
        let mut cx = ReadCx::default();
        for t in texts {
            let v: Value = serde_json::from_str(t).unwrap();
            let direct = sha256_hex(&[canonical_json(&v).as_bytes()]);
            assert!(cx.check(t, 1));
            assert_eq!(
                cx.value_digest(t, 1).as_deref(),
                Some(direct.as_str()),
                "{t}"
            );
            assert_eq!(
                cx.value_digest(t, 1).as_deref(),
                Some(direct.as_str()),
                "{t} again"
            );
        }
        assert!(!cx.check("[1,]", 1));
        assert_eq!(cx.value_digest("[1,]", 1), None);
    }
}
