//! The stored form of read telemetry: one record per model call that names
//! its prompt by hash, and the prompt messages as a hash-linked chain, each
//! message stored once per distinct prompt prefix. A store keeps these
//! instead of the raw OTLP bodies and derives from them.

use crate::error::{OtelError, Result};
use crate::generation::{Generation, Message, Prompt};
use crate::hash::{canonical_json, sha256_hex};
use crate::profile::{self, ProfileSelection};
use crate::session::Session;
use crate::walk::{self, ReadOutcome, SkipReason};
use crate::{Derived, SkipCounts};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// The hash a [`StoredMessage`] is stored under, which also names the
/// prompt prefix it ends: 64 lowercase hex of
/// `sha256("toolpath-otel/message\0" ‖ parent ‖ "\0" ‖ message)`, where
/// `parent` is the previous message's hash as hex text (empty for the
/// first message) and `message` the RFC 8785 (JCS) form of the message's
/// JSON, the canonical form every hash in this crate is defined on. It
/// depends only on the prompt up to and including the message and is part
/// of no derived id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageHash(String);

impl MessageHash {
    /// The 64 lowercase hex characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageHash {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(MessageHash(s.to_string()))
        } else {
            Err(format!("not a message hash (64 lowercase hex): {s:?}"))
        }
    }
}

impl TryFrom<String> for MessageHash {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Self, String> {
        s.parse()
    }
}

impl From<MessageHash> for String {
    fn from(h: MessageHash) -> String {
        h.0
    }
}

/// One prompt message as the client sent it (OpenAI chat shape: `role`,
/// `content` verbatim, and `tool_calls`, `tool_call_id`, `name`,
/// `is_error`, `reasoning_details` when present), linked to the message
/// before it in the prompt. Stored once under its [`MessageHash`], however
/// many calls' prompts share the prefix it ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<MessageHash>,
    message: Message,
}

impl StoredMessage {
    pub(crate) fn new(parent: Option<MessageHash>, message: Message) -> Self {
        StoredMessage { parent, message }
    }

    /// The hash this message is stored under.
    pub fn hash(&self) -> MessageHash {
        chain_hash(self.parent.as_ref(), &self.message)
    }

    /// The hash of the message before it in the prompt (`None` for the
    /// first).
    pub fn parent(&self) -> Option<&MessageHash> {
        self.parent.as_ref()
    }
}

pub(crate) fn chain_hash(parent: Option<&MessageHash>, m: &Message) -> MessageHash {
    message_hash(parent, &canonical_json(m))
}

/// The [`MessageHash`] of a message whose canonical JSON is `canonical`.
pub(crate) fn message_hash(parent: Option<&MessageHash>, canonical: &str) -> MessageHash {
    MessageHash(sha256_hex(&[
        b"toolpath-otel/message\0",
        parent.map_or("", MessageHash::as_str).as_bytes(),
        b"\0",
        canonical.as_bytes(),
    ]))
}

/// The message hashes of a prompt, one per message: a prefix of a list
/// that prompts rebuilt from one store share.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PromptHashes {
    all: Arc<[MessageHash]>,
    len: usize,
}

impl PromptHashes {
    pub(crate) fn as_slice(&self) -> &[MessageHash] {
        &self.all[..self.len]
    }
}

/// Prompts longer than this are a looping message store.
const MAX_PROMPT: usize = 1 << 20;

/// One model call read from OTLP telemetry, in the crate's neutral form,
/// with its prompt named by the [`MessageHash`] of its last message.
///
/// A record is one of:
///
/// - a **generation**: ids, timing, the completion, usage, cost, models,
///   and its prompt;
/// - a **truncation marker**: a call of a known session whose prompt or
///   completion was cut off, so it was skipped. It carries ids only and
///   marks the session's path `meta.otel.truncated`.
///
/// Records serialize with serde (JSON); `docs/agents/formats/otel.md`
/// ("Generation records") documents the fields. A round-tripped record,
/// with its messages, derives byte for byte what the original does, and
/// every derived id (turn ids, the session key, the path and session ids)
/// depends only on record and message content, never on which delivery a
/// record came from or when it was read. A newer [`FORMAT`](Self::FORMAT)
/// fails to deserialize. A reader drops fields it does not know, so a field
/// added later that reaches step content comes with a new `FORMAT`, and one
/// path is derived by one version of this crate: two versions reading one
/// store could derive different steps for it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Wire")]
pub struct GenerationRecord {
    format: u32,
    /// The call, its `messages` empty: `prompt` names them.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "call_fields"
    )]
    generation: Option<Generation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<MessageHash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<Truncated>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Truncated {
    generation_id: Option<String>,
    session_id: String,
    /// The profile that read the call (dedupe by rank ranks by it).
    profile: String,
}

fn call_fields<S: Serializer>(
    g: &Option<Generation>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    let mut v = serde_json::to_value(g).map_err(serde::ser::Error::custom)?;
    if let Value::Object(m) = &mut v {
        m.remove("messages");
    }
    v.serialize(s)
}

#[derive(Deserialize)]
struct Wire {
    format: u32,
    #[serde(default)]
    generation: Option<Generation>,
    #[serde(default)]
    prompt: Option<MessageHash>,
    #[serde(default)]
    truncated: Option<Truncated>,
}

impl TryFrom<Wire> for GenerationRecord {
    type Error = String;

    fn try_from(w: Wire) -> std::result::Result<Self, String> {
        if w.format == 0 || w.format > Self::FORMAT {
            return Err(format!(
                "generation record format {} (this build reads 1 to {})",
                w.format,
                Self::FORMAT
            ));
        }
        if w.generation.is_some() == w.truncated.is_some() {
            return Err(
                "a generation record holds exactly one of `generation`, `truncated`".into(),
            );
        }
        if w.generation
            .as_ref()
            .is_some_and(|g| !g.messages.is_empty())
        {
            return Err("a generation record names its messages by hash, in `prompt`".into());
        }
        Ok(GenerationRecord {
            format: w.format,
            generation: w.generation,
            prompt: w.prompt,
            truncated: w.truncated,
        })
    }
}

impl GenerationRecord {
    /// The record format this build writes, and the newest it reads.
    pub const FORMAT: u32 = 1;

    /// The format the record was written in.
    pub fn format(&self) -> u32 {
        self.format
    }

    /// The call's generation id (`None` for a truncation marker the
    /// telemetry gave no id).
    pub fn generation_id(&self) -> Option<&str> {
        match &self.generation {
            Some(g) => Some(&g.id),
            None => self.truncated.as_ref()?.generation_id.as_deref(),
        }
    }

    /// The client session id the call carries; group records into sessions
    /// by it. `None` when the client sends none: grouping is then the
    /// caller's, as for [`derive_path`](crate::derive_path).
    pub fn session_id(&self) -> Option<&str> {
        match &self.generation {
            Some(g) => g.session_id.as_deref(),
            None => Some(&self.truncated.as_ref()?.session_id),
        }
    }

    /// The call's start time in Unix nanoseconds (`None` for a truncation
    /// marker).
    pub fn start_ns(&self) -> Option<u64> {
        self.generation.as_ref().map(|g| g.start_ns)
    }

    /// The record is a truncation marker.
    pub fn is_truncated(&self) -> bool {
        self.truncated.is_some()
    }

    /// The hash of the call's last prompt message: follow
    /// [`StoredMessage::parent`] from it for the rest, last to first. `None`
    /// for an empty prompt and for a truncation marker.
    pub fn prompt(&self) -> Option<&MessageHash> {
        self.prompt.as_ref()
    }

    /// The record `read_generations` would make of `g`, its prompt added to
    /// `store`.
    #[cfg(test)]
    pub(crate) fn of(g: Generation, store: &mut BTreeMap<MessageHash, StoredMessage>) -> Self {
        let prompt = store_prompt(&g.messages, store);
        let mut g = g;
        g.messages = Default::default();
        GenerationRecord {
            format: Self::FORMAT,
            generation: Some(g),
            prompt,
            truncated: None,
        }
    }
}

/// What [`read_generations`] read: the records, and every message their
/// prompts name.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationBatch {
    /// One per model call read, in read order.
    pub records: Vec<GenerationRecord>,
    /// Every message the records' prompts name, once, by hash.
    pub messages: BTreeMap<MessageHash, StoredMessage>,
}

/// Read OTLP/HTTP JSON request bodies into [`GenerationRecord`]s and the
/// messages their prompts name, and count what was read but not recorded.
///
/// Unlike [`derive_path`](crate::derive_path) this takes the bodies of any
/// number of sessions and any subset of a session's bodies. Reading one
/// delivery at a time and deriving from all the records derives what one
/// read of every body does, as long as no call's telemetry spans two
/// deliveries (OpenRouter Broadcast sends each call's trace whole in one).
/// Bodies without a readable call give no records and no error.
///
/// # Errors
///
/// [`OtelError::NotOtlp`] for a body that is not an OTLP object.
pub fn read_generations(
    requests: &[Value],
    profile: ProfileSelection,
) -> Result<Derived<GenerationBatch>> {
    let mut out = walk::read_records(requests, profile)?;
    let skipped = SkipCounts::from_outcome(&out);
    let mut batch = GenerationBatch {
        records: Vec::new(),
        messages: std::mem::take(&mut out.messages),
    };
    let mut prompts = std::mem::take(&mut out.prompts).into_iter();
    for entry in entries(out) {
        batch.records.push(match entry {
            Entry::Generation(g, _) => GenerationRecord {
                format: GenerationRecord::FORMAT,
                generation: Some(*g),
                prompt: prompts.next().expect("a prompt per generation"),
                truncated: None,
            },
            Entry::Truncated(t) => GenerationRecord {
                format: GenerationRecord::FORMAT,
                generation: None,
                prompt: None,
                truncated: Some(t),
            },
        });
    }
    Ok(Derived {
        output: batch,
        skipped,
    })
}

/// The message hash of the prompt `messages`, with its messages added to
/// `store`.
pub(crate) fn store_prompt(
    messages: &[Message],
    store: &mut BTreeMap<MessageHash, StoredMessage>,
) -> Option<MessageHash> {
    let mut prompt = None;
    for message in messages {
        let h = chain_hash(prompt.as_ref(), message);
        store.entry(h.clone()).or_insert_with(|| StoredMessage {
            parent: prompt.clone(),
            message: message.clone(),
        });
        prompt = Some(h);
    }
    prompt
}

/// A read call, before or after storage; a rebuilt generation carries its
/// prompt's message hashes.
pub(crate) enum Entry {
    Generation(Box<Generation>, Option<PromptHashes>),
    Truncated(Truncated),
}

/// A walk's calls as entries: truncation markers of known sessions first
/// (a walk marks a call truncated only before keeping any copy of it), then
/// the generations.
pub(crate) fn entries(out: ReadOutcome) -> impl Iterator<Item = Entry> {
    out.skipped
        .into_iter()
        .filter(|s| s.reason == SkipReason::Truncated)
        .filter_map(|s| {
            Some(Entry::Truncated(Truncated {
                generation_id: s.generation_id,
                session_id: s.session_id?,
                profile: s.profile.unwrap_or_default(),
            }))
        })
        .chain(
            out.generations
                .into_iter()
                .map(|g| Entry::Generation(Box::new(g), None)),
        )
}

/// `records` as entries, their prompts looked up in `messages`. A prompt
/// that is a prefix of another shares its messages, so each stored message
/// is cloned once per prompt no other prompt extends, not once per record.
pub(crate) fn rebuild<'m>(
    records: &[GenerationRecord],
    messages: impl Fn(&MessageHash) -> Option<&'m StoredMessage>,
) -> Result<Vec<Entry>> {
    let fetch =
        |h: &MessageHash| messages(h).ok_or_else(|| OtelError::MessageMissing(h.to_string()));
    let tails: Vec<&MessageHash> = records
        .iter()
        .filter(|r| r.generation.is_some())
        .filter_map(|r| r.prompt.as_ref())
        .collect();
    // A walk stops at the first message an earlier walk reached, so every
    // message is visited about once, and a loop ends.
    let mut seen: HashSet<MessageHash> = HashSet::new();
    let mut inner: HashSet<MessageHash> = HashSet::new();
    for &t in &tails {
        if !seen.insert(t.clone()) {
            continue;
        }
        let mut next = fetch(t)?.parent.as_ref();
        while let Some(h) = next {
            inner.insert(h.clone());
            if !seen.insert(h.clone()) {
                break;
            }
            next = fetch(h)?.parent.as_ref();
        }
    }
    type Shared = (Arc<[Message]>, Arc<[MessageHash]>, usize);
    let mut prefixes: HashMap<MessageHash, Shared> = HashMap::new();
    for &t in &tails {
        if inner.contains(t) || prefixes.contains_key(t) {
            continue;
        }
        let mut chain: Vec<(&MessageHash, &StoredMessage)> = Vec::new();
        let mut next = Some(t);
        while let Some(h) = next {
            if chain.len() == MAX_PROMPT {
                return Err(OtelError::MessageMissing(h.to_string()));
            }
            let m = fetch(h)?;
            chain.push((h, m));
            next = m.parent.as_ref();
        }
        chain.reverse();
        let all: Arc<[Message]> = chain.iter().map(|(_, m)| m.message.clone()).collect();
        let hashes: Arc<[MessageHash]> = chain.iter().map(|(h, _)| (*h).clone()).collect();
        for (i, (h, _)) in chain.into_iter().enumerate() {
            prefixes
                .entry(h.clone())
                .or_insert_with(|| (all.clone(), hashes.clone(), i + 1));
        }
    }
    records
        .iter()
        .map(|r| match &r.generation {
            Some(g) => {
                let mut g = Box::new(g.clone());
                let mut hashes = PromptHashes::default();
                if let Some(t) = &r.prompt {
                    let (all, all_hashes, len) = prefixes
                        .get(t)
                        .ok_or_else(|| OtelError::MessageMissing(t.to_string()))?;
                    g.messages = Prompt::shared(all.clone(), *len);
                    hashes = PromptHashes {
                        all: all_hashes.clone(),
                        len: *len,
                    };
                }
                Ok(Entry::Generation(g, Some(hashes)))
            }
            None => Ok(Entry::Truncated(
                r.truncated
                    .clone()
                    .expect("a record without a generation is a marker"),
            )),
        })
        .collect()
}

/// Which copy of a generation id a session keeps; the others count as
/// `duplicate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    /// The better-ranked profile's copy, else the first: what one read of
    /// every body keeps (the walker's rule), whatever the order.
    Rank,
    /// The first in `entries` order. [`derive_jsonl`](crate::derive_jsonl)'s
    /// records are in the order the store received them, so this is the
    /// copy it fed first, and a later copy never replaces a fed one.
    Arrival,
}

/// The one session `entries` hold, one copy of each generation id kept as
/// `pick` says. `duplicate` counts the copies dropped here.
///
/// # Errors
///
/// [`OtelError::MixedSessions`] when the generations carry more than one
/// client session id; [`OtelError::NoGenerations`] when there are none.
pub(crate) fn session_of(
    entries: Vec<Entry>,
    profile: ProfileSelection,
    pick: Pick,
    mut skipped: SkipCounts,
) -> Result<(Session, SkipCounts)> {
    let consulted = profile::consulted(profile);
    let rank = |name: &str| match pick {
        Pick::Rank => consulted
            .iter()
            .position(|p| p.name() == name)
            .unwrap_or(usize::MAX),
        // Every copy ranks alike, so the first kept stays.
        Pick::Arrival => 0,
    };
    let mut seen: HashMap<String, (usize, usize)> = HashMap::new();
    let mut kept: Vec<Option<(Generation, Option<PromptHashes>)>> = Vec::new();
    let mut truncated: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        match entry {
            Entry::Generation(g, hashes) => {
                let r = rank(g.profile_name());
                match seen.get(&g.id) {
                    Some(&(k, _)) if k <= r => {
                        skipped.duplicate += 1;
                        continue;
                    }
                    Some(&(_, slot)) => {
                        kept[slot] = None;
                        skipped.duplicate += 1;
                    }
                    None => {}
                }
                seen.insert(g.id.clone(), (r, kept.len()));
                kept.push(Some((*g, hashes)));
            }
            Entry::Truncated(t) => {
                // A copy after a kept one is a duplicate, never a truncation.
                let duplicate = t
                    .generation_id
                    .as_ref()
                    .and_then(|id| seen.get(id))
                    .is_some_and(|&(k, _)| k <= rank(&t.profile));
                if !duplicate {
                    truncated.insert(t.session_id);
                }
            }
        }
    }
    let mut hashes: HashMap<String, PromptHashes> = HashMap::new();
    let mut generations: Vec<Generation> = Vec::new();
    for (g, h) in kept.into_iter().flatten() {
        if let Some(h) = h {
            hashes.insert(g.id.clone(), h);
        }
        generations.push(g);
    }
    let ids: BTreeSet<&str> = generations
        .iter()
        .filter_map(|g| g.session_id.as_deref())
        .collect();
    if ids.len() > 1 {
        return Err(OtelError::MixedSessions(
            ids.into_iter().map(str::to_string).collect(),
        ));
    }
    let truncated = ids.first().is_some_and(|id| truncated.contains(*id));
    let mut session =
        Session::from_generations(generations).ok_or(OtelError::NoGenerations { skipped })?;
    session.truncated = truncated;
    if hashes.len() == session.generations.len() {
        session.prompt_hashes = Some(Arc::new(hashes));
    }
    Ok((session, skipped))
}

#[cfg(test)]
mod tests;
