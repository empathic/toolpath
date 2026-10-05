//! Minimal profile for OpenInference (Arize) flattened, indexed span
//! attributes. Explicit-only (`--profile openinference`).

use super::{Ident, Profile, TraceView, Unit};
use crate::generation::{
    Absent, CacheBasis, Completion, FunctionCall, Generation, History, Message, ToolCall, Usage,
};
use crate::otlp::{Attrs, KeyValue, Resource, Scope, Span, any_value_to_json, nanos};
use crate::walk::{ReadCx, SkipReason};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The `openinference` profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenInference;

const NAME: &str = "openinference";
const KIND: &str = "openinference.span.kind";
const LLM: &str = "LLM";
const INPUT_MESSAGES: &str = "llm.input_messages.";
const OUTPUT_MESSAGES: &str = "llm.output_messages.";
/// OpenInference's `TraceConfig` placeholder for hidden content.
const REDACTED: &str = "__REDACTED__";

/// `(suffix, value)` pairs of one indexed entry, in attribute order.
type Fields = Vec<(String, Value)>;

fn kind<'s>(span: &'s Span<'_>) -> Option<&'s str> {
    Attrs(&span.attributes).str(KIND)
}

/// `span-<spanId>`; an empty span id is no id.
fn generation_id(span: &Span) -> Option<String> {
    (!span.span_id.is_empty()).then(|| format!("span-{}", span.span_id))
}

/// A string attribute; an empty string is absent.
fn string(attrs: &[KeyValue], key: &str) -> Option<String> {
    Attrs(attrs)
        .str(key)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Profile for OpenInference {
    fn name(&self) -> &'static str {
        NAME
    }

    fn claims(&self, _resource: &Resource, _scope: &Scope, span: &Span) -> bool {
        kind(span) == Some(LLM)
    }

    fn absorbs(&self, _resource: &Resource, _scope: &Scope, span: &Span) -> bool {
        kind(span).is_some_and(|k| k != LLM)
    }

    fn identify(&self, unit: &Unit<'_>) -> Ident {
        let Some(span) = unit.span else {
            return Ident::default();
        };
        Ident {
            generation_id: generation_id(span),
            session_id: string(&span.attributes, "session.id"),
        }
    }

    fn extract<'a>(
        &self,
        unit: &Unit<'a>,
        _trace: &TraceView<'a>,
        _cx: &mut ReadCx<'a>,
    ) -> Result<Generation, SkipReason> {
        let span = unit.span.ok_or(SkipReason::MissingPayload)?;
        let id = generation_id(span).ok_or(SkipReason::MissingPayload)?;
        Ok(extract_span(id, unit.resource, unit.scope, span))
    }
}

/// `<prefix><i>.<rest>` entries grouped by `i`, sorted numerically (`10`
/// after `9`). Keys whose index segment is not a number are ignored.
fn indexed(fields: &[(String, Value)], prefix: &str) -> BTreeMap<usize, Fields> {
    let mut out: BTreeMap<usize, Fields> = BTreeMap::new();
    for (k, v) in fields {
        let Some(rest) = k.strip_prefix(prefix) else {
            continue;
        };
        let Some((i, tail)) = rest.split_once('.') else {
            continue;
        };
        if let Ok(i) = i.parse::<usize>() {
            out.entry(i)
                .or_default()
                .push((tail.to_string(), v.clone()));
        }
    }
    out
}

fn get<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn text(fields: &[(String, Value)], name: &str) -> Option<String> {
    get(fields, name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tool_calls(fields: &[(String, Value)]) -> Vec<ToolCall> {
    indexed(fields, "message.tool_calls.")
        .into_values()
        .map(|c| ToolCall {
            id: text(&c, "tool_call.id").unwrap_or_default(),
            function: FunctionCall {
                name: text(&c, "tool_call.function.name").unwrap_or_default(),
                arguments: get(&c, "tool_call.function.arguments")
                    .cloned()
                    .unwrap_or(Value::Null),
            },
        })
        .collect()
}

fn message(fields: &[(String, Value)]) -> Message {
    Message {
        role: text(fields, "message.role").unwrap_or_default(),
        content: get(fields, "message.content")
            .cloned()
            .unwrap_or(Value::Null),
        tool_calls: tool_calls(fields),
        tool_call_id: text(fields, "message.tool_call_id"),
        ..Default::default()
    }
}

/// A missing family, or any value in it equal to the redaction
/// placeholder, makes that side absent (a skeleton side).
fn is_absent(family: &BTreeMap<usize, Fields>) -> bool {
    family.is_empty()
        || family
            .values()
            .flatten()
            .any(|(_, v)| v.as_str() == Some(REDACTED))
}

fn usage(span: &Span) -> Usage {
    let a = Attrs(&span.attributes);
    let output = a.u64("llm.token_count.completion");
    Usage {
        input_tokens: a.u64("llm.token_count.prompt"),
        output_tokens: output,
        total_tokens: a.u64("llm.token_count.total"),
        cached_input_tokens: a.u64("llm.token_count.prompt_details.cache_read"),
        cache_write_tokens: a.u64("llm.token_count.prompt_details.cache_write"),
        reasoning_tokens: a
            .u64("llm.token_count.completion_details.reasoning")
            .map(|r| output.map_or(r, |o| r.min(o))),
        ..Default::default()
    }
    // The prompt count includes cache reads and writes.
    .with_basis(CacheBasis::Inclusive)
}

fn extract_span(id: String, resource: &Resource, scope: &Scope, span: &Span) -> Generation {
    let all: Fields = span
        .attributes
        .iter()
        .map(|kv| (kv.key.to_string(), any_value_to_json(kv.value)))
        .collect();
    let inputs = indexed(&all, INPUT_MESSAGES);
    let outputs = indexed(&all, OUTPUT_MESSAGES);
    let absent = Absent {
        prompt: is_absent(&inputs),
        completion: is_absent(&outputs),
    };

    let messages: Vec<Message> = if absent.prompt {
        Vec::new()
    } else {
        inputs.values().map(Vec::as_slice).map(message).collect()
    };
    let history = if absent.prompt {
        History::Delta
    } else {
        History::Full
    };

    // Output index 0 is the completion; later indices are extra choices.
    let mut choices = outputs.values().filter(|_| !absent.completion);
    let completion = choices
        .next()
        .map(|f| {
            let m = message(f);
            Completion {
                text: m.content.as_str().unwrap_or_default().to_string(),
                tool_calls: m.tool_calls,
                ..Default::default()
            }
        })
        .unwrap_or_default();
    let extra: Vec<Value> = choices
        .map(|f| Value::Object(f.iter().cloned().collect()))
        .collect();

    let mut source_meta = Map::new();
    if !extra.is_empty() {
        source_meta.insert("choices".into(), Value::Array(extra));
    }
    source_meta.insert(
        "scope".into(),
        json!({"name": scope.name, "version": scope.version}),
    );
    if let Some(p) = string(&span.attributes, "llm.invocation_parameters") {
        let parsed = serde_json::from_str(&p).unwrap_or(Value::String(p));
        source_meta.insert("invocation_parameters".into(), parsed);
    }

    let model = string(&span.attributes, "llm.model_name");
    Generation {
        id,
        profile: NAME.to_string(),
        trace_id: span.trace_id.to_string(),
        start_ns: nanos(span.start_time_unix_nano).unwrap_or(0),
        end_ns: nanos(span.end_time_unix_nano).unwrap_or(0),
        session_id: string(&span.attributes, "session.id"),
        user_id: string(&span.attributes, "user.id"),
        client_key: string(&resource.attributes, "service.name"),
        messages,
        completion,
        usage: usage(span),
        request_model: string(&span.attributes, "llm.request.model_name").or_else(|| model.clone()),
        response_model: string(&span.attributes, "llm.response.model_name").or(model),
        provider: string(&span.attributes, "llm.system"),
        source_meta,
        history,
        absent,
        ..Default::default()
    }
}
