//! Read-scoped memos. Every full-history request repeats the session's
//! prompt so far, so one read sees each message many times: a prompt that
//! extends one read before reuses its messages by a byte comparison, each
//! distinct message text is parsed once, each distinct prompt prefix is a
//! node of a trie (so prompts share their messages, and hash them once),
//! and repeated large
//! values (tool lists) are validated and digested once. Nothing outlives
//! the read call.
//!
//! Every shortcut answers exactly what a whole-text serde_json parse
//! answers, or declines (`None`) so the caller runs that parse.

use super::scan;
use crate::generation::{Message, Prompt};
use crate::hash::{canonical_json, sha256_hex};
use crate::record::{MessageHash, StoredMessage, message_hash};
use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

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
/// No trie node: the parent of a prompt's first message.
const NONE: usize = usize::MAX;

#[derive(Default)]
pub struct ReadCx<'a> {
    by_print: HashMap<u64, Vec<usize>>,
    messages: Vec<Memoized<'a>>,
    /// One per distinct prompt prefix; `children` finds a prefix's
    /// one-message extension.
    nodes: Vec<Node>,
    children: HashMap<(usize, usize), usize>,
    prompts: Vec<Seen<'a>>,
    prompts_by_key: HashMap<u64, Vec<usize>>,
    values: HashMap<(u64, usize), Vec<usize>>,
    checked: Vec<Checked<'a>>,
    /// Large member value texts by member name, newest last.
    recent: HashMap<&'a str, Vec<&'a str>>,
    /// Set by a profile's extract that read its prompt through
    /// [`ReadCx::prompt`]: the prompt's last node (`None`: no messages).
    pub(crate) tail: Option<Option<usize>>,
}

/// A prompt prefix: its last message's memo entry, and the prefix before
/// it.
#[derive(Clone, Copy)]
struct Node {
    parent: usize,
    entry: usize,
}

struct Memoized<'a> {
    raw: &'a str,
    message: Message,
}

/// A prompt read through the shortcut: where each message ends, and the
/// node of the prefix it ends.
struct Seen<'a> {
    raw: &'a str,
    ends: Vec<usize>,
    nodes: Vec<usize>,
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

    /// The node of `entry` after the prefix `parent`.
    fn node(&mut self, parent: usize, entry: usize) -> usize {
        let nodes = &mut self.nodes;
        *self.children.entry((parent, entry)).or_insert_with(|| {
            nodes.push(Node { parent, entry });
            nodes.len() - 1
        })
    }

    /// The last node of a `{"messages":[…]}` prompt text (`Some(None)` for
    /// no messages), whose messages ([`ReadCx::messages`]) are what parsing
    /// the text into `{messages: Vec<Message>}` gives. `None` when the text
    /// is not in that exact form or does not parse; the caller then parses
    /// the whole text.
    pub fn prompt(&mut self, raw: &'a str) -> Option<Option<usize>> {
        if !raw.starts_with(PROMPT_HEAD) {
            return None;
        }
        let b = raw.as_bytes();
        let key = fingerprint(&b[..b.len().min(PROMPT_HEAD.len() + PROMPT_KEY_BYTES)]);
        // The earlier prompt sharing the most whole leading messages (newest
        // first; one with no more messages than the best so far cannot do
        // better); their bytes are equal, so their parses are too.
        let mut ends = Vec::new();
        let mut nodes = Vec::new();
        for &p in self.prompts_by_key.get(&key).into_iter().flatten().rev() {
            let prev = &self.prompts[p];
            if prev.ends.len() <= ends.len() {
                continue;
            }
            let common = scan::common_prefix(b, prev.raw.as_bytes());
            let shared = prev.ends.partition_point(|&e| e <= common);
            if shared > ends.len() {
                ends = prev.ends[..shared].to_vec();
                nodes = prev.nodes[..shared].to_vec();
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
            let entry = self.entry(&raw[i..end])?;
            nodes.push(self.node(nodes.last().copied().unwrap_or(NONE), entry));
            ends.push(end);
            i = end;
            first = false;
        }
        i = scan::ws(b, i + 1);
        if b.get(i) != Some(&b'}') || scan::ws(b, i + 1) != b.len() {
            return None;
        }
        let tail = nodes.last().copied();
        let keyed = self.prompts_by_key.entry(key).or_default();
        if keyed.len() == CANDIDATES {
            keyed.remove(0);
        }
        keyed.push(self.prompts.len());
        self.prompts.push(Seen { raw, ends, nodes });
        Some(tail)
    }

    /// The nodes of the prompt ending at `tail`, first to last.
    fn chain(&self, tail: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        let mut n = tail.unwrap_or(NONE);
        while n != NONE {
            out.push(n);
            n = self.nodes[n].parent;
        }
        out.reverse();
        out
    }

    /// The messages of the prompt ending at `tail`, copied.
    #[cfg(test)]
    pub fn messages(&self, tail: Option<usize>) -> Vec<Message> {
        self.chain(tail)
            .into_iter()
            .map(|n| self.messages[self.nodes[n].entry].message.clone())
            .collect()
    }

    /// The prompts ending at `tails`, sharing storage: one list per prompt
    /// no other prompt extends, each message copied once into it, and
    /// every prompt a prefix of one of them.
    pub fn share(&self, tails: &[Option<usize>]) -> Vec<Prompt> {
        // A walk stops at the first node an earlier walk reached.
        let mut seen = vec![false; self.nodes.len()];
        let mut inner = vec![false; self.nodes.len()];
        for &t in tails.iter().flatten() {
            if std::mem::replace(&mut seen[t], true) {
                continue;
            }
            let mut n = self.nodes[t].parent;
            while n != NONE {
                inner[n] = true;
                if std::mem::replace(&mut seen[n], true) {
                    break;
                }
                n = self.nodes[n].parent;
            }
        }
        let mut lists: Vec<Arc<[Message]>> = Vec::new();
        let mut at: Vec<(usize, usize)> = vec![(NONE, 0); self.nodes.len()];
        for &t in tails.iter().flatten() {
            if inner[t] || at[t].0 != NONE {
                continue;
            }
            let chain = self.chain(Some(t));
            lists.push(
                chain
                    .iter()
                    .map(|&n| self.messages[self.nodes[n].entry].message.clone())
                    .collect(),
            );
            for (depth, n) in chain.into_iter().enumerate() {
                if at[n].0 == NONE {
                    at[n] = (lists.len() - 1, depth + 1);
                }
            }
        }
        tails
            .iter()
            .map(|t| match t {
                Some(t) => Prompt::shared(lists[at[*t].0].clone(), at[*t].1),
                None => Prompt::default(),
            })
            .collect()
    }

    /// The [`MessageHash`] of the prompt ending at each of `tails`, with
    /// every message those prompts name added to `store`. Each distinct
    /// prefix is hashed, and each distinct message canonicalized, once.
    pub fn hashes(
        &self,
        tails: &[Option<usize>],
        store: &mut BTreeMap<MessageHash, StoredMessage>,
    ) -> Vec<Option<MessageHash>> {
        let mut canonical: Vec<Option<String>> = vec![None; self.messages.len()];
        let mut hash: Vec<Option<MessageHash>> = vec![None; self.nodes.len()];
        for &t in tails.iter().flatten() {
            let mut todo = Vec::new();
            let mut n = t;
            while n != NONE && hash[n].is_none() {
                todo.push(n);
                n = self.nodes[n].parent;
            }
            for n in todo.into_iter().rev() {
                let Node { parent, entry } = self.nodes[n];
                let message = &self.messages[entry].message;
                let parent = (parent != NONE).then(|| hash[parent].clone().expect("parent first"));
                let c = canonical[entry].get_or_insert_with(|| canonical_json(message));
                let h = message_hash(parent.as_ref(), c);
                store
                    .entry(h.clone())
                    .or_insert_with(|| StoredMessage::new(parent, message.clone()));
                hash[n] = Some(h);
            }
        }
        tails
            .iter()
            .map(|t| t.and_then(|t| hash[t].clone()))
            .collect()
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
