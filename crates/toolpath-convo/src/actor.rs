//! The actor conventions of an agent coding session.
//!
//! The base format says nothing about the shape of an actor string:
//! `step.actor` is an opaque string, and `toolpath` neither defines nor
//! validates a grammar for it. What shape an actor takes is a question each
//! producer answers for itself — `toolpath-git` names actors from commit
//! authors by its own convention, and nothing checks it.
//!
//! The `agent-coding-session` path kind is one such answer, and this module
//! is that answer as this deriver implements it: [`Actor`] is a `prefix:id`
//! reference, and the prefixes below are what a conversation attributes a
//! turn to.
//!
//! - `human:` — a person. `human:user` is what this deriver emits when the
//!   source names no one.
//! - `agent:` — a model or agent, the thing that produces text and decisions.
//!   `agent:unknown` is the kind spec's id for "a model ran, unnamed".
//! - `tool:` — the general machine prefix, for anything that is not a model:
//!   a formatter, a CI job, or an agent harness writing on its own behalf. A
//!   harness is one *kind* of tool actor, so there is no separate prefix for
//!   it, and it is always named: `tool:claude-code`.
//!
//! The prefix set itself stays open — [`Actor`] accepts any prefix within its
//! character set and privileges none — so a document that reaches this crate
//! carrying `ci:github-actions` round-trips as itself rather than being
//! relabelled. Only the three prefixes above carry meaning to this deriver.
//!
//! Every constructor here is total. A name the actor grammar cannot carry is
//! no name at all, so it falls back to the same placeholder an absent name
//! does; that keeps derived documents renderable and schema-valid whatever a
//! session file happens to hold.

use serde::{Deserialize, Serialize};

/// Prefix for a person.
pub const HUMAN_PREFIX: &str = "human";
/// Prefix for a model or agent.
pub const AGENT_PREFIX: &str = "agent";
/// Prefix for a machine actor that is not a model.
pub const TOOL_PREFIX: &str = "tool";

/// The id [`generic_human`] renders — "a person", not an identifier.
pub const GENERIC_HUMAN_ID: &str = "user";
/// The id [`unnamed_agent`] renders — "a model ran, unnamed". Defined by the
/// `agent-coding-session` kind spec, and a different claim from "no model was
/// involved", which is a [`harness`] actor.
pub const UNNAMED_AGENT_ID: &str = "unknown";
/// The id [`harness`] renders when the provider is not identified — the same
/// string `derive_path` already uses for an unidentified provider elsewhere
/// in a derived document.
pub const UNKNOWN_PROVIDER_ID: &str = "unknown";

/// Whether `s` is a legal actor segment: non-empty, and drawn from
/// `A`–`Z`, `a`–`z`, `0`–`9`, `_`, `.`, `-`.
fn is_actor_segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// An actor reference as an agent coding session writes it — the string a
/// derived step's `actor` holds and `meta.actors` keys on.
///
/// # Grammar
///
/// `prefix ":" id`, where each segment is one or more of `A`–`Z`, `a`–`z`,
/// `0`–`9`, `_`, `.`, `-`.
///
/// This is the `agent-coding-session` kind's constraint on actor strings as
/// implemented by this deriver, not a rule of the base format: `toolpath`
/// treats `step.actor` as an opaque string and asserts nothing about its
/// shape, and a producer of some other kind of path is free to name actors
/// however it likes.
///
/// Within that constraint the prefix set is **open**. `human:alex`,
/// `agent:gpt-5.5`, `tool:rustfmt`, `ci:github-actions` and `bot:dependabot`
/// are all actor references, and this type gives none of them special
/// meaning: it validates the shape and renders it back. Which prefixes a
/// conversation attributes turns to, and which ids stand in for an unnamed
/// actor, are this module's conventions — see [`is_agent`], [`generic_human`]
/// and [`unnamed_agent`].
///
/// # Sub-actors
///
/// The grammar admits a `/`-delimited suffix qualifying an actor —
/// `tool:rustfmt/1.5.0`, `agent:claude-code/tool:Write`. `Actor` models the
/// actor proper: [`FromStr`](std::str::FromStr) keeps the segment before the
/// first `/` and drops the suffix, so an id never contains `/`.
/// [`Actor::split_sub_actor`] exposes the split for callers that need it.
///
/// # Round-trip
///
/// [`Display`](std::fmt::Display) writes the document form and
/// [`FromStr`](std::str::FromStr) reads it; they are the only place the
/// grammar is implemented, and serde uses them, so an `Actor` on the wire is
/// the actor string rather than a nested object. Every `Actor` renders to a
/// reference that parses back to itself, and every suffix-free reference
/// parses to an `Actor` that renders back to it unchanged.
///
/// ```
/// use toolpath_convo::Actor;
///
/// let actor: Actor = "agent:gpt-5.5".parse().unwrap();
/// assert_eq!(actor.prefix(), "agent");
/// assert_eq!(actor.id(), "gpt-5.5");
/// assert_eq!(actor.to_string(), "agent:gpt-5.5");
///
/// // Any prefix in the character set is an actor reference.
/// assert_eq!("bot:dependabot".parse::<Actor>().unwrap().prefix(), "bot");
///
/// // Constructing from parts validates the same grammar.
/// assert_eq!(Actor::new("tool", "rustfmt").unwrap().to_string(), "tool:rustfmt");
/// assert!(Actor::new("tool", "").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Actor {
    prefix: String,
    id: String,
}

impl Actor {
    /// Build an actor reference from its two segments, rejecting anything the
    /// grammar cannot render back.
    pub fn new(prefix: impl Into<String>, id: impl Into<String>) -> Result<Self, ParseActorError> {
        let prefix = prefix.into();
        if !is_actor_segment(&prefix) {
            return Err(ParseActorError::InvalidPrefix(prefix));
        }
        let id = id.into();
        if !is_actor_segment(&id) {
            return Err(ParseActorError::InvalidId(id));
        }
        Ok(Self { prefix, id })
    }

    /// The segment before the `:` — `"human"`, `"agent"`, `"tool"`, `"ci"`,
    /// or anything else the writer chose.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The segment after the `:`. This is the value that belongs in
    /// `ActorDefinition::name`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Split an actor string into the actor proper and its optional
    /// `/`-delimited sub-actor suffix.
    ///
    /// ```
    /// use toolpath_convo::Actor;
    ///
    /// assert_eq!(
    ///     Actor::split_sub_actor("agent:claude-code/tool:Write"),
    ///     ("agent:claude-code", Some("tool:Write"))
    /// );
    /// assert_eq!(Actor::split_sub_actor("tool:rustfmt"), ("tool:rustfmt", None));
    /// ```
    pub fn split_sub_actor(actor: &str) -> (&str, Option<&str>) {
        match actor.split_once('/') {
            Some((base, sub)) => (base, Some(sub)),
            None => (actor, None),
        }
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.prefix, self.id)
    }
}

impl std::str::FromStr for Actor {
    type Err = ParseActorError;

    /// Parse an actor reference.
    ///
    /// Any sub-actor suffix is dropped first (see
    /// [`Actor::split_sub_actor`]), then the remainder splits on its first
    /// `:`. Both segments must be non-empty and within the grammar's
    /// character set; the prefix is otherwise unconstrained.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (actor, _sub) = Actor::split_sub_actor(s);
        let (prefix, id) = actor
            .split_once(':')
            .ok_or(ParseActorError::MissingPrefix)?;
        Actor::new(prefix, id)
    }
}

impl Serialize for Actor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Why an actor reference could not be parsed or built. See [`Actor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseActorError {
    /// The string carries no `prefix:` separator.
    MissingPrefix,
    /// The prefix is empty or holds a character outside the grammar's set.
    InvalidPrefix(String),
    /// The id is empty or holds a character outside the grammar's set.
    InvalidId(String),
}

impl std::fmt::Display for ParseActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseActorError::MissingPrefix => f.write_str("actor has no `prefix:` separator"),
            ParseActorError::InvalidPrefix(p) => {
                write!(f, "actor prefix `{p}` is empty or has illegal characters")
            }
            ParseActorError::InvalidId(id) => {
                write!(f, "actor id `{id}` is empty or has illegal characters")
            }
        }
    }
}

impl std::error::Error for ParseActorError {}

/// Build an actor from segments this module knows are within the grammar.
fn constant(prefix: &str, id: &str) -> Actor {
    // Both arguments are module constants or already-validated ids; the
    // `is_actor_reference_grammar_holds_for_constants` test pins that.
    Actor::new(prefix, id).expect("constant actor reference is within the grammar")
}

/// The person a source names no more precisely than "the user".
pub fn generic_human() -> Actor {
    constant(HUMAN_PREFIX, GENERIC_HUMAN_ID)
}

/// A person, named where the source names one.
pub fn human(name: Option<&str>) -> Actor {
    name.and_then(|n| Actor::new(HUMAN_PREFIX, n).ok())
        .unwrap_or_else(generic_human)
}

/// A model ran, but the source did not name it.
pub fn unnamed_agent() -> Actor {
    constant(AGENT_PREFIX, UNNAMED_AGENT_ID)
}

/// A model, named where the source names one.
pub fn agent(model: Option<&str>) -> Actor {
    model
        .and_then(|m| Actor::new(AGENT_PREFIX, m).ok())
        .unwrap_or_else(unnamed_agent)
}

/// The harness itself, as an actor — the author of anything a provider wrote
/// on its own behalf: API errors, rate-limit notices, its own bookkeeping.
pub fn harness(provider: &str) -> Actor {
    Actor::new(TOOL_PREFIX, provider).unwrap_or_else(|_| constant(TOOL_PREFIX, UNKNOWN_PROVIDER_ID))
}

/// Whether `actor` is a person.
pub fn is_human(actor: &Actor) -> bool {
    actor.prefix() == HUMAN_PREFIX
}

/// Whether `actor` is a model or agent.
pub fn is_agent(actor: &Actor) -> bool {
    actor.prefix() == AGENT_PREFIX
}

/// Whether `actor` is a machine actor that is not a model — a harness among
/// them.
pub fn is_tool(actor: &Actor) -> bool {
    actor.prefix() == TOOL_PREFIX
}

/// The model `actor` names, if it names one — the value that belongs in
/// `ActorDefinition::model`. `None` for anything that is not an agent, and
/// for the unnamed-agent placeholder, which is a sentinel rather than a name.
pub fn model_name(actor: &Actor) -> Option<&str> {
    if is_agent(actor) && actor.id() != UNNAMED_AGENT_ID {
        Some(actor.id())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every reference the round-trip properties below run over. The prefix
    /// set is open, so novel prefixes belong in the corpus alongside the
    /// conventional ones.
    fn actor_refs() -> Vec<&'static str> {
        vec![
            "human:user",
            "human:alex",
            "agent:unknown",
            "agent:gpt-5.5",
            "agent:claude-opus-4-6",
            "tool:rustfmt",
            "tool:claude-code",
            "ci:github-actions",
            "bot:dependabot",
            "SERVICE:Some_Thing.v2-beta",
        ]
    }

    #[test]
    fn test_actor_display_of_parse_is_identity() {
        for canonical in actor_refs() {
            assert_eq!(canonical.parse::<Actor>().unwrap().to_string(), canonical);
        }
    }

    #[test]
    fn test_actor_parse_of_display_is_identity() {
        for reference in actor_refs() {
            let actor: Actor = reference.parse().unwrap();
            let rendered = actor.to_string();
            assert_eq!(
                rendered.parse::<Actor>().unwrap(),
                actor,
                "round trip through {rendered}"
            );
        }
    }

    #[test]
    fn test_actor_serde_is_the_actor_string() {
        for reference in actor_refs() {
            let actor: Actor = reference.parse().unwrap();
            let json = serde_json::to_string(&actor).unwrap();
            assert_eq!(json, format!("\"{reference}\""));
            assert_eq!(serde_json::from_str::<Actor>(&json).unwrap(), actor);
        }
        assert!(serde_json::from_str::<Actor>("\"no-prefix\"").is_err());
    }

    #[test]
    fn test_actor_prefix_set_is_open() {
        // No prefix is privileged: an unconventional one parses, keeps its
        // spelling, and renders back unchanged.
        let actor: Actor = "bot:dependabot".parse().unwrap();
        assert_eq!(actor.prefix(), "bot");
        assert_eq!(actor.id(), "dependabot");
        assert_eq!(actor.to_string(), "bot:dependabot");
        assert_eq!(Actor::new("bot", "dependabot").unwrap(), actor);
    }

    #[test]
    fn test_actor_parse_drops_the_sub_actor_suffix() {
        let actor: Actor = "agent:claude-code/tool:Write".parse().unwrap();
        assert_eq!(actor, Actor::new("agent", "claude-code").unwrap());
        let actor: Actor = "tool:rustfmt/1.5.0".parse().unwrap();
        assert_eq!(actor, Actor::new("tool", "rustfmt").unwrap());
        assert_eq!(
            Actor::split_sub_actor("agent:claude-code/tool:Write"),
            ("agent:claude-code", Some("tool:Write"))
        );
        assert_eq!(Actor::split_sub_actor("human:alex"), ("human:alex", None));
    }

    #[test]
    fn test_actor_parse_errors() {
        assert_eq!("".parse::<Actor>(), Err(ParseActorError::MissingPrefix));
        assert_eq!("alex".parse::<Actor>(), Err(ParseActorError::MissingPrefix));
        assert_eq!(
            "tool:".parse::<Actor>(),
            Err(ParseActorError::InvalidId(String::new()))
        );
        assert_eq!(
            ":alex".parse::<Actor>(),
            Err(ParseActorError::InvalidPrefix(String::new()))
        );
        assert_eq!(
            "hu man:alex".parse::<Actor>(),
            Err(ParseActorError::InvalidPrefix("hu man".into()))
        );
        assert_eq!(
            "human:a/b".parse::<Actor>(),
            // The suffix splits off first, so this is `human:a`.
            Ok(Actor::new("human", "a").unwrap())
        );
        assert_eq!(
            "human:al ex".parse::<Actor>(),
            Err(ParseActorError::InvalidId("al ex".into()))
        );
        assert_eq!(
            "human:a:b".parse::<Actor>(),
            Err(ParseActorError::InvalidId("a:b".into()))
        );
    }

    #[test]
    fn test_actor_new_validates_both_segments() {
        assert!(Actor::new("tool", "rustfmt").is_ok());
        assert!(Actor::new("", "alex").is_err());
        assert!(Actor::new("human", "").is_err());
        assert!(Actor::new("human", "a/b").is_err());
        assert!(Actor::new("human", "a:b").is_err());
    }

    #[test]
    fn test_actor_accessors() {
        let actor = Actor::new("agent", "gpt-5.5").unwrap();
        assert_eq!(actor.prefix(), "agent");
        assert_eq!(actor.id(), "gpt-5.5");
    }

    #[test]
    fn is_actor_reference_grammar_holds_for_constants() {
        assert_eq!(generic_human().to_string(), "human:user");
        assert_eq!(unnamed_agent().to_string(), "agent:unknown");
        assert_eq!(harness("claude-code").to_string(), "tool:claude-code");
        assert_eq!(harness("").to_string(), "tool:unknown");
    }

    #[test]
    fn names_the_grammar_cannot_carry_fall_back_to_the_placeholder() {
        assert_eq!(human(Some("alex")).to_string(), "human:alex");
        assert_eq!(human(None).to_string(), "human:user");
        assert_eq!(human(Some("")).to_string(), "human:user");
        assert_eq!(human(Some("Ada Lovelace")).to_string(), "human:user");

        assert_eq!(agent(Some("gpt-5.5")).to_string(), "agent:gpt-5.5");
        assert_eq!(agent(None).to_string(), "agent:unknown");
        assert_eq!(agent(Some("vendor/model")).to_string(), "agent:unknown");
    }

    #[test]
    fn model_name_is_the_agent_id_unless_it_is_the_placeholder() {
        assert_eq!(model_name(&agent(Some("gpt-5.5"))), Some("gpt-5.5"));
        assert_eq!(model_name(&unnamed_agent()), None);
        assert_eq!(model_name(&generic_human()), None);
        assert_eq!(model_name(&harness("codex")), None);
    }

    #[test]
    fn predicates_read_the_prefix() {
        assert!(is_human(&generic_human()));
        assert!(is_agent(&unnamed_agent()));
        assert!(is_tool(&harness("pi")));
        let novel: Actor = "bot:dependabot".parse().unwrap();
        assert!(!is_human(&novel) && !is_agent(&novel) && !is_tool(&novel));
    }
}
