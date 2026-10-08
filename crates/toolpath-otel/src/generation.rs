//! The seam: neutral request/response records. Nothing here names OTLP or
//! a telemetry dialect; profiles fill these in and everything downstream
//! reads only these.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One API request and its response.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Generation {
    pub id: String,
    pub trace_id: String,
    pub start_ns: u64,
    pub end_ns: u64,
    /// The client's session id, when the client sent one.
    pub session_id: Option<String>,
    /// A session id the client put in the request body.
    pub request_session_id: Option<String>,
    pub user_id: Option<String>,
    /// Who sent the request (an API key name); partitions sessionless clustering.
    pub client_key: Option<String>,
    /// The full request history, OpenAI chat shape.
    pub messages: Vec<Message>,
    pub completion: Completion,
    pub usage: Usage,
    pub cost: Cost,
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub provider: Option<String>,
    pub finish_reason: Option<String>,
    /// Name of the profile that produced this generation (`"openrouter"`).
    /// Set by the walker; namespaces `source_meta` in step extras and meta.
    #[serde(default)]
    pub profile: String,
    /// Source-specific fields carried through to step extras verbatim.
    #[serde(default)]
    pub source_meta: Map<String, Value>,
    /// Server-side state: the generation id this request continues
    /// (OpenAI Responses `previous_response_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    /// Whether `messages` is the whole history or only what is new since
    /// the continuation target.
    #[serde(default, skip_serializing_if = "History::is_full")]
    pub history: History,
    /// Which side of the content was not captured (a skeleton).
    #[serde(default, skip_serializing_if = "Absent::is_none")]
    pub absent: Absent,
    /// Results for this generation's own completion calls observed outside
    /// any prompt, keyed by call id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_results: BTreeMap<String, ToolOutput>,
    /// The request history was compacted by the client.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compacted: bool,
}

impl Generation {
    /// `messages` holds only what is new since the continuation target:
    /// `history = Delta`, or a prompt-absent skeleton (which implies Delta).
    pub fn is_delta(&self) -> bool {
        self.history == History::Delta || self.absent.prompt
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    /// A string, a list of content parts, or null — kept verbatim.
    #[serde(default)]
    pub content: Value,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub reasoning_details: Vec<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: String,
    /// Usually a JSON-encoded string; kept verbatim.
    #[serde(default)]
    pub arguments: Value,
}

impl FunctionCall {
    /// Arguments as a JSON value. A string that is not valid JSON (a cut-off
    /// stream) stays a string rather than failing.
    pub fn parsed_arguments(&self) -> Value {
        match &self.arguments {
            Value::String(s) => {
                serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            other => other.clone(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// Reasoning as received (signatures, redacted blocks, summaries).
    /// Excluded from the comparison key; retained in step extras.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub reasoning_details: Vec<Value>,
}

/// Token classes are additive: `input_tokens` excludes cache reads and
/// writes, whatever the source reported (see [`CacheBasis`]).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// How the source counted cache in its input count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_basis: Option<CacheBasis>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
}

/// Whether `Generation.messages` is the whole history (`Full`) or only the
/// messages new since the continuation target (`Delta`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum History {
    #[default]
    Full,
    Delta,
}

impl History {
    fn is_full(&self) -> bool {
        *self == History::Full
    }
}

/// Which side of a generation's content was not captured: the generation
/// is a skeleton (usage, models, ids only) on that side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Absent {
    #[serde(default)]
    pub prompt: bool,
    #[serde(default)]
    pub completion: bool,
}

impl Absent {
    /// Either side is absent.
    pub fn any(&self) -> bool {
        self.prompt || self.completion
    }

    /// Neither side is absent (the default).
    pub fn is_none(&self) -> bool {
        !self.any()
    }
}

/// A result for one of this generation's own completion calls, observed
/// outside any prompt (an `execute_tool` span, a tool event). Stitch uses it
/// only when no later prompt carries the call's tool message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl Usage {
    /// Records the source's `basis` and makes `input_tokens` exclusive: an
    /// inclusive count has cache reads and writes subtracted (saturating).
    pub(crate) fn with_basis(mut self, basis: CacheBasis) -> Self {
        if basis == CacheBasis::Inclusive {
            let cache = self
                .cached_input_tokens
                .unwrap_or(0)
                .saturating_add(self.cache_write_tokens.unwrap_or(0));
            self.input_tokens = self.input_tokens.map(|i| i.saturating_sub(cache));
        }
        self.cache_basis = Some(basis);
        self
    }
}

/// How a source counted cache in its input count. `Usage.input_tokens` is
/// exclusive either way; this records what the emitter reported.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBasis {
    /// The emitter's input count included cache reads and writes; they
    /// were subtracted.
    Inclusive,
    /// The emitter's input count excluded cache; it passes through.
    Exclusive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_tolerates_null_tool_calls_and_content() {
        let m: Message = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": null, "tool_calls": null
        }))
        .unwrap();
        assert!(m.tool_calls.is_empty());
        assert!(m.content.is_null());
    }

    #[test]
    fn unparseable_arguments_stay_a_string() {
        let f = FunctionCall {
            name: "Bash".into(),
            arguments: Value::String("{\"command\": \"ls".into()),
        };
        assert_eq!(
            f.parsed_arguments(),
            Value::String("{\"command\": \"ls".into())
        );
        let ok = FunctionCall {
            name: "Bash".into(),
            arguments: Value::String("{\"command\": \"ls\"}".into()),
        };
        assert_eq!(ok.parsed_arguments(), serde_json::json!({"command": "ls"}));
    }

    fn full_generation() -> Generation {
        let mut source_meta = Map::new();
        source_meta.insert("app".into(), Value::String("claude-code".into()));
        Generation {
            id: "gen-1".into(),
            trace_id: "trace-1".into(),
            start_ns: 1_000,
            end_ns: 2_000,
            session_id: Some("sess".into()),
            request_session_id: Some("req-sess".into()),
            user_id: Some("user".into()),
            client_key: Some("key-name".into()),
            messages: vec![
                Message {
                    role: "user".into(),
                    content: Value::String("hi".into()),
                    ..Default::default()
                },
                Message {
                    role: "assistant".into(),
                    content: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: "t1".into(),
                        function: FunctionCall {
                            name: "Bash".into(),
                            arguments: Value::String("{}".into()),
                        },
                    }],
                    reasoning_details: vec![serde_json::json!({"type": "reasoning.text"})],
                    ..Default::default()
                },
                Message {
                    role: "tool".into(),
                    content: serde_json::json!([{"type": "text", "text": "ok"}]),
                    tool_call_id: Some("t1".into()),
                    name: Some("Bash".into()),
                    is_error: Some(false),
                    ..Default::default()
                },
            ],
            completion: Completion {
                text: "done".into(),
                reasoning: Some("thought".into()),
                tool_calls: vec![ToolCall {
                    id: "t2".into(),
                    function: FunctionCall {
                        name: "Read".into(),
                        arguments: Value::String("{\"path\":\"a\"}".into()),
                    },
                }],
                ..Default::default()
            },
            usage: Usage {
                input_tokens: Some(1),
                output_tokens: Some(2),
                cached_input_tokens: Some(3),
                cache_write_tokens: Some(4),
                cache_write_5m_tokens: Some(5),
                cache_write_1h_tokens: Some(6),
                reasoning_tokens: Some(7),
                total_tokens: Some(8),
                ..Default::default()
            },
            cost: Cost {
                input: Some(0.25),
                output: Some(0.5),
                total: Some(0.75),
            },
            request_model: Some("anthropic/claude".into()),
            response_model: Some("anthropic/claude-4".into()),
            provider: Some("Anthropic".into()),
            finish_reason: Some("stop".into()),
            profile: "openrouter".into(),
            source_meta,
            ..Default::default()
        }
    }

    #[test]
    fn fully_populated_generation_round_trips() {
        let g = full_generation();
        let back: Generation = serde_json::from_value(serde_json::to_value(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn default_usage_serializes_empty_and_reads_back() {
        let v = serde_json::to_value(Usage::default()).unwrap();
        assert_eq!(v, serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<Usage>(v).unwrap(),
            Usage::default()
        );
    }

    #[test]
    fn null_reasoning_details_deserializes_empty() {
        let m: Message = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "x", "reasoning_details": null
        }))
        .unwrap();
        assert!(m.reasoning_details.is_empty());
    }

    #[test]
    fn generation_without_profile_reads_back_with_empty_profile() {
        let mut v = serde_json::to_value(full_generation()).unwrap();
        v.as_object_mut().unwrap().remove("profile");
        let g: Generation = serde_json::from_value(v).unwrap();
        assert_eq!(g.profile, "");
    }

    fn without_optional_keys() -> serde_json::Value {
        // A generation serialized before the optional keys existed.
        serde_json::json!({
            "id": "gen-1", "trace_id": "t", "start_ns": 1, "end_ns": 2,
            "session_id": null, "request_session_id": null, "user_id": null,
            "client_key": "k", "messages": [{"role": "user", "content": "hi"}],
            "completion": {"text": "ok", "reasoning": null, "tool_calls": []},
            "usage": {"input_tokens": 3}, "cost": {},
            "request_model": null, "response_model": null, "provider": null,
            "finish_reason": null, "source_meta": {}
        })
    }

    #[test]
    fn generation_without_optional_keys_reads_with_defaults() {
        let g: Generation = serde_json::from_value(without_optional_keys()).unwrap();
        assert_eq!(g.profile, "");
        assert_eq!(g.continues, None);
        assert_eq!(g.history, History::Full);
        assert_eq!(g.absent, Absent::default());
        assert!(g.tool_results.is_empty());
        assert!(!g.compacted);
        assert!(g.completion.reasoning_details.is_empty());
        assert_eq!(g.usage.cache_basis, None);
        assert!(!g.is_delta());
    }

    #[test]
    fn optional_fields_at_their_defaults_are_not_serialized() {
        let mut g: Generation = serde_json::from_value(without_optional_keys()).unwrap();
        g.profile = "semconv".into();
        let v = serde_json::to_value(&g).unwrap();
        for key in [
            "continues",
            "history",
            "absent",
            "tool_results",
            "compacted",
        ] {
            assert!(v.get(key).is_none(), "{key} serialized at its default");
        }
        assert!(v["completion"].get("reasoning_details").is_none());
        assert!(v["usage"].get("cache_basis").is_none());
        assert_eq!(v["profile"], "semconv");
    }

    #[test]
    fn optional_fields_round_trip() {
        let mut g = full_generation();
        g.profile = "semconv".into();
        g.continues = Some("resp_1".into());
        g.history = History::Delta;
        g.absent = Absent {
            prompt: true,
            completion: false,
        };
        g.tool_results.insert(
            "call_1".into(),
            ToolOutput {
                content: "ok".into(),
                is_error: true,
            },
        );
        g.compacted = true;
        g.completion.reasoning_details =
            vec![serde_json::json!({"type": "reasoning", "content": "r"})];
        g.usage.cache_basis = Some(CacheBasis::Exclusive);
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(v["history"], "delta");
        assert_eq!(v["usage"]["cache_basis"], "exclusive");
        assert_eq!(
            v["absent"],
            serde_json::json!({"prompt": true, "completion": false})
        );
        let back: Generation = serde_json::from_value(v).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn is_delta_truth_table() {
        let mut g: Generation = serde_json::from_value(without_optional_keys()).unwrap();
        assert!(!g.is_delta());
        g.absent.completion = true;
        assert!(
            !g.is_delta(),
            "a completion-only skeleton keeps its full prompt"
        );
        g.absent.prompt = true;
        assert!(g.is_delta(), "an absent prompt implies Delta");
        g.absent = Absent::default();
        g.history = History::Delta;
        assert!(g.is_delta());
    }

    #[test]
    fn absent_helpers() {
        assert!(Absent::default().is_none());
        assert!(!Absent::default().any());
        assert!(
            Absent {
                prompt: false,
                completion: true
            }
            .any()
        );
    }

    #[test]
    fn null_completion_reasoning_details_reads_empty() {
        let c: Completion = serde_json::from_value(serde_json::json!({
            "text": "", "reasoning": null, "tool_calls": [], "reasoning_details": null
        }))
        .unwrap();
        assert!(c.reasoning_details.is_empty());
    }
}
