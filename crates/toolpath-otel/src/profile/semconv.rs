//! Profile for the OpenTelemetry GenAI semantic conventions
//! (`open-telemetry/semantic-conventions-genai`), current and v1.36 forms.

use super::{Ident, Profile, SpanRef, TraceView, Unit};
use crate::generation::{
    Absent, CacheBasis, Completion, FunctionCall, Generation, History, Message, ToolCall,
    ToolOutput, Usage,
};
use crate::hash::{canonical_json, sha256_hex};
use crate::normalize::content_text;
use crate::otlp::{
    Attrs, KeyValue, LogRecord, Resource, Scope, Span, SpanEvent, any_value_to_json, nanos,
};
use crate::walk::SkipReason;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The `semconv` profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct Semconv;

pub(crate) const OPERATION: &str = "gen_ai.operation.name";
const INFERENCE_OPS: [&str; 3] = ["chat", "text_completion", "generate_content"];
const DETAILS_EVENT: &str = "gen_ai.client.inference.operation.details";
/// v1.36 per-role input events, read only when neither
/// `gen_ai.input.messages` nor `gen_ai.system_instructions` is present.
const LEGACY_INPUT: [&str; 4] = [
    "gen_ai.system.message",
    "gen_ai.user.message",
    "gen_ai.assistant.message",
    "gen_ai.tool.message",
];
/// v1.36 output events, read only when `gen_ai.output.messages` is absent.
const LEGACY_CHOICE: [&str; 1] = ["gen_ai.choice"];
const EXECUTE_TOOL: &str = "execute_tool";

/// Event names `semconv` turns into generations when they arrive as log records.
fn is_semconv_event(name: &str) -> bool {
    name == DETAILS_EVENT || LEGACY_INPUT.contains(&name) || LEGACY_CHOICE.contains(&name)
}

/// Instrumentation scope names whose `gen_ai.usage.input_tokens` excludes
/// cache reads and writes (keyed on the emitter, not the model provider).
/// Empty: every known emitter, Anthropic's included, reports inclusive counts.
pub const EXCLUSIVE_INPUT_SCOPES: &[&str] = &[];

/// `(source_meta.request_params key, attribute)`.
const REQUEST_PARAMS: [(&str, &str); 12] = [
    ("temperature", "gen_ai.request.temperature"),
    ("top_p", "gen_ai.request.top_p"),
    ("top_k", "gen_ai.request.top_k"),
    ("max_tokens", "gen_ai.request.max_tokens"),
    ("seed", "gen_ai.request.seed"),
    ("stop_sequences", "gen_ai.request.stop_sequences"),
    ("frequency_penalty", "gen_ai.request.frequency_penalty"),
    ("presence_penalty", "gen_ai.request.presence_penalty"),
    ("reasoning_level", "gen_ai.request.reasoning.level"),
    ("choice_count", "gen_ai.request.choice.count"),
    ("stream", "gen_ai.request.stream"),
    ("output_type", "gen_ai.output.type"),
];

fn operation<'s>(span: &'s Span<'_>) -> Option<&'s str> {
    Attrs(&span.attributes).str(OPERATION)
}

impl Profile for Semconv {
    fn name(&self) -> &'static str {
        "semconv"
    }

    fn claims(&self, _resource: &Resource, _scope: &Scope, span: &Span) -> bool {
        operation(span).is_some_and(|op| INFERENCE_OPS.contains(&op))
    }

    /// Every other GenAI operation (`execute_tool`, `invoke_agent`, …): not a
    /// generation, visible to `extract` through `TraceView`.
    fn absorbs(&self, _resource: &Resource, _scope: &Scope, span: &Span) -> bool {
        operation(span).is_some_and(|op| !INFERENCE_OPS.contains(&op))
    }

    /// Details records and v1.36 per-role events with no span in the batch.
    fn claims_log(&self, _resource: &Resource, log: &LogRecord) -> bool {
        log.event().is_some_and(is_semconv_event)
    }

    fn identify(&self, unit: &Unit<'_>) -> Ident {
        let lk = Lookup::for_unit(unit);
        Ident {
            generation_id: generation_id(&lk, unit),
            session_id: session_id(&lk),
        }
    }

    fn extract(&self, unit: &Unit<'_>, trace: &TraceView<'_>) -> Result<Generation, SkipReason> {
        let mut g = extract_unit(unit, EXCLUSIVE_INPUT_SCOPES)?;
        // An orphan unit has no span to scope `execute_tool` results by.
        if let Some(span) = unit.span {
            g.tool_results = tool_results(&g.completion, span, trace);
        }
        Ok(g)
    }
}

/// One lookup layer: an attribute list (span attributes, a details span
/// event's or a details log record's attributes) or a details record's
/// kvlist body, already plain JSON.
enum Layer<'a> {
    Attrs(&'a [KeyValue<'a>]),
    Body(Map<String, Value>),
}

/// Layered attribute lookup: span attributes, details span events, then each
/// details log record's attributes and kvlist body. The first layer holding a key wins.
struct Lookup<'a> {
    layers: Vec<Layer<'a>>,
}

impl<'a> Lookup<'a> {
    fn for_unit(unit: &Unit<'a>) -> Self {
        let mut layers = Vec::new();
        if let Some(span) = unit.span {
            layers.push(Layer::Attrs(span.attributes.as_slice()));
            layers.extend(
                span.events
                    .iter()
                    .filter(|e| e.name == DETAILS_EVENT)
                    .map(|e| Layer::Attrs(e.attributes.as_slice())),
            );
        }
        for l in &unit.logs {
            if l.record.event() == Some(DETAILS_EVENT) {
                layers.push(Layer::Attrs(l.record.attributes.as_slice()));
                if let Value::Object(body) = any_value_to_json(l.record.body) {
                    layers.push(Layer::Body(body));
                }
            }
        }
        Lookup { layers }
    }

    fn json(&self, key: &str) -> Option<Value> {
        self.layers
            .iter()
            .find_map(|l| match l {
                Layer::Attrs(a) => Attrs(a).get(key).map(any_value_to_json),
                Layer::Body(m) => m.get(key).cloned(),
            })
            .filter(|v| !v.is_null())
    }

    fn string(&self, key: &str) -> Option<String> {
        match self.json(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    fn u64(&self, key: &str) -> Option<u64> {
        match self.json(key)? {
            Value::Number(n) => n.as_u64().or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                    .map(|f| f as u64)
            }),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn bool(&self, key: &str) -> Option<bool> {
        self.json(key)?.as_bool()
    }

    /// A content attribute. A JSON string is parsed, and one that is not
    /// valid JSON is a present-but-corrupt payload (`Truncated`, never a
    /// skeleton); a structured value is used as is.
    fn content(&self, key: &str) -> Result<Option<Value>, SkipReason> {
        // The common case, a string attribute, parsed in place, not copied.
        if let Some(Layer::Attrs(a)) = self.layers.iter().find(|l| match l {
            Layer::Attrs(a) => Attrs(a).get(key).is_some(),
            Layer::Body(m) => m.contains_key(key),
        }) && let Some(s) = Attrs(a).str(key)
        {
            return serde_json::from_str(s)
                .map(Some)
                .map_err(|_| SkipReason::Truncated);
        }
        match self.json(key) {
            None => Ok(None),
            Some(Value::String(s)) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|_| SkipReason::Truncated),
            Some(v) => Ok(Some(v)),
        }
    }

    /// Every `gen_ai.usage.*` key as received, modality-scoped ones included.
    fn usage_raw(&self) -> Map<String, Value> {
        let mut out = Map::new();
        for layer in &self.layers {
            match layer {
                Layer::Attrs(a) => {
                    for kv in a.iter().filter(|kv| kv.key.starts_with("gen_ai.usage.")) {
                        out.entry(kv.key)
                            .or_insert_with(|| any_value_to_json(kv.value));
                    }
                }
                Layer::Body(m) => {
                    for (k, v) in m.iter().filter(|(k, _)| k.starts_with("gen_ai.usage.")) {
                        out.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
            }
        }
        out
    }
}

/// `gen_ai.response.id`, else `span-<spanId>` for a span unit, else
/// `log-<traceId>-<spanId>` (lowercased) for an orphan unit. Empty is no id.
fn generation_id(lk: &Lookup<'_>, unit: &Unit<'_>) -> Option<String> {
    non_empty(lk.string("gen_ai.response.id")).or_else(|| match unit.span {
        Some(span) => (!span.span_id.is_empty()).then(|| format!("span-{}", span.span_id)),
        None => {
            let r = unit.logs.first()?.record;
            (!r.trace_id.is_empty() || !r.span_id.is_empty()).then(|| {
                format!(
                    "log-{}-{}",
                    r.trace_id.to_ascii_lowercase(),
                    r.span_id.to_ascii_lowercase()
                )
            })
        }
    })
}

fn session_id(lk: &Lookup<'_>) -> Option<String> {
    non_empty(lk.string("gen_ai.conversation.id")).or_else(|| non_empty(lk.string("session.id")))
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn part_type(p: &Value) -> &str {
    p.get("type").and_then(Value::as_str).unwrap_or("")
}

fn part_text(p: &Value) -> Option<&str> {
    p.get("content").and_then(Value::as_str)
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value.filter(|v| !v.is_null()) {
        map.insert(key.to_string(), v);
    }
}

/// Only text parts → one string joined with `\n`; anything else → a parts
/// list with text parts rewritten to `{type:"text", text}` and the rest
/// verbatim; nothing → `null` (a tool-call-only assistant message).
fn content_value(parts: &[Value]) -> Value {
    if parts.is_empty() {
        return Value::Null;
    }
    if parts.iter().all(|p| part_type(p) == "text") {
        return Value::String(
            parts
                .iter()
                .filter_map(part_text)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    Value::Array(
        parts
            .iter()
            .filter(|p| part_type(p) != "text" || part_text(p).is_some())
            .map(|p| match part_type(p) {
                "text" => json!({"type": "text", "text": part_text(p)}),
                _ => p.clone(),
            })
            .collect(),
    )
}

fn tool_call(p: &Value) -> ToolCall {
    ToolCall {
        id: p
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        function: FunctionCall {
            name: p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments: p.get("arguments").cloned().unwrap_or(Value::Null),
        },
    }
}

/// A tool result as text: a string as is, anything else as canonical JSON.
fn response_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => canonical_json(other),
    }
}

/// `ChatMessage[]` → OpenAI-chat messages; a non-array is `Truncated`.
fn convert_messages(v: &Value) -> Result<Vec<Message>, SkipReason> {
    let items = v.as_array().ok_or(SkipReason::Truncated)?;
    let mut out = Vec::new();
    for m in items {
        convert_message(m, &mut out);
    }
    Ok(out)
}

/// One `ChatMessage` → messages in source part order: one tool message per
/// `tool_call_response` part, one source-role message per run of other parts.
fn convert_message(m: &Value, out: &mut Vec<Message>) {
    let role = m
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let parts = m
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let first = out.len();
    let mut run = RoleRun::default();
    for p in parts {
        match part_type(&p) {
            "tool_call_response" => {
                run.flush(&role, out);
                out.push(Message {
                    role: "tool".to_string(),
                    content: Value::String(response_text(p.get("response"))),
                    tool_call_id: Some(
                        p.get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    ..Default::default()
                });
            }
            "reasoning" => run.reasoning.push(p),
            "tool_call" => run.calls.push(tool_call(&p)),
            _ => run.content.push(p),
        }
    }
    run.flush(&role, out);
    if out.len() == first {
        out.push(Message {
            role,
            content: Value::Null,
            ..Default::default()
        });
    }
}

/// The non-response parts of one run of a `ChatMessage`.
#[derive(Default)]
struct RoleRun {
    content: Vec<Value>,
    reasoning: Vec<Value>,
    calls: Vec<ToolCall>,
}

impl RoleRun {
    fn flush(&mut self, role: &str, out: &mut Vec<Message>) {
        if self.content.is_empty() && self.reasoning.is_empty() && self.calls.is_empty() {
            return;
        }
        let run = std::mem::take(self);
        out.push(Message {
            role: role.to_string(),
            content: content_value(&run.content),
            tool_calls: run.calls,
            reasoning_details: run.reasoning,
            ..Default::default()
        });
    }
}

/// The first output message → the completion. Other part types render as
/// `[<type>]`, matching `content_text` on the history echo, and are returned
/// for `output_parts`.
fn convert_completion(m: &Value) -> (Completion, Vec<Value>) {
    let mut c = Completion::default();
    let (mut text, mut reasoning, mut other) = (Vec::new(), Vec::new(), Vec::new());
    for p in m
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        match part_type(&p) {
            "text" => text.extend(part_text(&p).map(str::to_string)),
            "reasoning" => {
                reasoning.push(part_text(&p).unwrap_or("").to_string());
                c.reasoning_details.push(p);
            }
            "tool_call" => c.tool_calls.push(tool_call(&p)),
            t => {
                text.push(format!("[{t}]"));
                other.push(p);
            }
        }
    }
    c.text = text.join("\n");
    c.reasoning = (!reasoning.is_empty()).then(|| reasoning.join("\n"));
    (c, other)
}

fn system_message(v: &Value) -> Option<Message> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| part_type(p) == "text")
            .filter_map(part_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    Some(Message {
        role: "system".to_string(),
        content: Value::String(text),
        ..Default::default()
    })
}

fn has_compaction(messages: &Value) -> bool {
    messages.as_array().into_iter().flatten().any(|m| {
        m.get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|p| part_type(p) == "compaction")
    })
}

fn finish_reasons(lk: &Lookup<'_>) -> Vec<Value> {
    match lk.json("gen_ai.response.finish_reasons") {
        Some(Value::Array(a)) => a,
        Some(Value::String(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(Value::Array(a)) => a,
            _ => vec![Value::String(s)],
        },
        _ => Vec::new(),
    }
}

/// The emitter's basis, by instrumentation scope name.
pub(crate) fn source_basis(scope_name: &str, exclusive: &[&str]) -> CacheBasis {
    if exclusive.contains(&scope_name) {
        CacheBasis::Exclusive
    } else {
        CacheBasis::Inclusive
    }
}

fn usage(lk: &Lookup<'_>, scope_name: &str, exclusive: &[&str]) -> Usage {
    let first = |keys: &[&str]| keys.iter().find_map(|k| lk.u64(k));
    // Struct literal: clippy's field_reassign_with_default fires on non_exhaustive types.
    let mut u = Usage {
        input_tokens: first(&["gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens"]),
        output_tokens: first(&[
            "gen_ai.usage.output_tokens",
            "gen_ai.usage.completion_tokens",
        ]),
        cached_input_tokens: first(&[
            "gen_ai.usage.cache_read.input_tokens",
            "gen_ai.usage.cache_read_input_tokens",
        ]),
        cache_write_tokens: first(&[
            "gen_ai.usage.cache_write.input_tokens",
            "gen_ai.usage.cache_creation_input_tokens",
        ]),
        reasoning_tokens: first(&["gen_ai.usage.reasoning.output_tokens"]),
        ..Default::default()
    };
    if let (Some(r), Some(o)) = (u.reasoning_tokens, u.output_tokens) {
        u.reasoning_tokens = Some(r.min(o));
    }
    u.with_basis(source_basis(scope_name, exclusive))
}

fn source_meta(
    lk: &Lookup<'_>,
    scope: &Scope,
    reasons: &[Value],
    outputs: &[Value],
    output_parts: Vec<Value>,
) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "operation", lk.json(OPERATION));
    m.insert(
        "scope".into(),
        json!({"name": scope.name, "version": scope.version}),
    );
    let mut params = Map::new();
    for (name, key) in REQUEST_PARAMS {
        put(&mut params, name, lk.json(key));
    }
    if !params.is_empty() {
        m.insert("request_params".into(), Value::Object(params));
    }
    put(&mut m, "server_address", lk.json("server.address"));
    put(
        &mut m,
        "time_to_first_chunk_s",
        lk.json("gen_ai.response.time_to_first_chunk"),
    );
    if !reasons.is_empty() {
        m.insert("finish_reasons".into(), Value::Array(reasons.to_vec()));
    }
    if outputs.len() > 1 {
        m.insert("choices".into(), Value::Array(outputs[1..].to_vec()));
    }
    if let Ok(Some(defs)) = lk.content("gen_ai.tool.definitions") {
        let digest = sha256_hex(&[canonical_json(&defs).as_bytes()]);
        m.insert("tools_digest".into(), Value::String(digest));
    }
    let raw = lk.usage_raw();
    if !raw.is_empty() {
        m.insert("usage_raw".into(), Value::Object(raw));
    }
    if !output_parts.is_empty() {
        m.insert("output_parts".into(), Value::Array(output_parts));
    }
    m
}

/// A legacy event's body: a map-valued `body` attribute, else the attributes.
fn event_body(attrs: &[KeyValue]) -> Map<String, Value> {
    if let Some(Value::Object(m)) = Attrs(attrs).get("body").map(any_value_to_json) {
        return m;
    }
    attrs
        .iter()
        .map(|kv| (kv.key.to_string(), any_value_to_json(kv.value)))
        .collect()
}

/// The span's events with these names, ordered by (timeUnixNano, input order).
fn legacy_span_events<'a>(span: &'a Span, names: &[&str]) -> Vec<(&'a str, Map<String, Value>)> {
    let mut events: Vec<(u64, usize, &SpanEvent)> = span
        .events
        .iter()
        .enumerate()
        .filter(|(_, e)| names.contains(&e.name))
        .map(|(i, e)| (nanos(e.time_unix_nano).unwrap_or(0), i, e))
        .collect();
    events.sort_by_key(|(t, i, _)| (*t, *i));
    events
        .into_iter()
        .map(|(_, _, e)| (e.name, event_body(&e.attributes)))
        .collect()
}

/// v1.36 per-role events for a unit, in time order. If any arrives as a log
/// record, log records are the only source (span copies are the same data).
fn legacy_events<'a>(unit: &Unit<'a>, names: &[&str]) -> Vec<(&'a str, Map<String, Value>)> {
    let is_legacy = |n: &str| LEGACY_INPUT.contains(&n) || LEGACY_CHOICE.contains(&n);
    let from_logs = unit
        .logs
        .iter()
        .any(|l| l.record.event().is_some_and(is_legacy));
    let events: Vec<(&'a str, Map<String, Value>)> = if from_logs {
        unit.logs
            .iter()
            .filter_map(|l| {
                let name = l.record.event().filter(|n| names.contains(n))?;
                let body = match any_value_to_json(l.record.body) {
                    Value::Object(m) => m,
                    _ => event_body(&l.record.attributes),
                };
                Some((name, body))
            })
            .collect()
    } else if let Some(span) = unit.span {
        legacy_span_events(span, names)
    } else {
        Vec::new()
    };
    events
        .into_iter()
        .map(|(n, b)| (n, normalize_legacy_body(b)))
        .collect()
}

/// Rewrite openai-v2's flat legacy tool calls (`{id, type, name, arguments}`)
/// to the v1.36 nested form (`{id, type, function: {name, arguments}}`).
fn normalize_legacy_body(mut body: Map<String, Value>) -> Map<String, Value> {
    fn nest(calls: Option<&mut Value>) {
        let Some(Value::Array(calls)) = calls else {
            return;
        };
        for call in calls {
            let Value::Object(o) = call else { continue };
            if o.contains_key("function") {
                continue;
            }
            let mut function = Map::new();
            for k in ["name", "arguments"] {
                if let Some(v) = o.remove(k) {
                    function.insert(k.to_string(), v);
                }
            }
            if !function.is_empty() {
                o.insert("function".to_string(), Value::Object(function));
            }
        }
    }
    nest(body.get_mut("tool_calls"));
    nest(
        body.get_mut("message")
            .and_then(|m| m.get_mut("tool_calls")),
    );
    body
}

/// The unit's `gen_ai.choice` bodies by `index` (equal or missing indexes
/// keep time order).
fn legacy_choices(unit: &Unit<'_>) -> Vec<Map<String, Value>> {
    let mut choices: Vec<Map<String, Value>> = legacy_events(unit, &LEGACY_CHOICE)
        .into_iter()
        .map(|(_, body)| body)
        .collect();
    choices.sort_by_key(|b| b.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX));
    choices
}

fn legacy_calls(v: Option<&Value>) -> Vec<ToolCall> {
    v.and_then(|c| Vec::<ToolCall>::deserialize(c).ok())
        .unwrap_or_default()
}

/// One legacy input event → a message.
fn legacy_message(name: &str, body: &Map<String, Value>) -> Message {
    match name {
        "gen_ai.tool.message" => Message {
            role: "tool".to_string(),
            content: Value::String(response_text(body.get("content"))),
            tool_call_id: Some(
                body.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            ..Default::default()
        },
        _ => Message {
            role: body
                .get("role")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    name.trim_start_matches("gen_ai.")
                        .trim_end_matches(".message")
                        .to_string()
                }),
            content: body.get("content").cloned().unwrap_or(Value::Null),
            tool_calls: legacy_calls(body.get("tool_calls")),
            ..Default::default()
        },
    }
}

/// A `gen_ai.choice` body's `message` → the completion.
fn legacy_completion(choice: &Map<String, Value>) -> Completion {
    let message = choice.get("message");
    Completion {
        text: content_text(
            message
                .and_then(|m| m.get("content"))
                .unwrap_or(&Value::Null),
        ),
        tool_calls: legacy_calls(message.and_then(|m| m.get("tool_calls"))),
        ..Default::default()
    }
}

/// Call ids an inference span's completion produced, read as `extract_unit`
/// reads it, including from log records so event-mode generations count.
fn produced_call_ids(r: SpanRef<'_>, trace: &TraceView<'_>) -> Vec<String> {
    let logs = trace
        .all_logs()
        .filter(|l| {
            // An id-less span owns no record (an id-less record would match it).
            !r.span.span_id.is_empty()
                && l.record.span_id.eq_ignore_ascii_case(r.span.span_id)
                && l.record.event().is_some_and(is_semconv_event)
        })
        .collect();
    let unit = Unit {
        resource: r.resource,
        scope: r.scope,
        span: Some(r.span),
        logs,
    };
    let calls = match Lookup::for_unit(&unit).content("gen_ai.output.messages") {
        Ok(Some(Value::Array(items))) => items
            .first()
            .map(|m| convert_completion(m).0.tool_calls)
            .unwrap_or_default(),
        Ok(Some(_)) | Err(_) => Vec::new(),
        Ok(None) => legacy_choices(&unit)
            .first()
            .map(|c| legacy_completion(c).tool_calls)
            .unwrap_or_default(),
    };
    calls.into_iter().map(|c| c.id).collect()
}

fn start_of(span: &Span) -> u64 {
    nanos(span.start_time_unix_nano).unwrap_or(0)
}

/// Results for this generation's calls from absorbed `execute_tool` spans.
/// Time-scoped because synthesized ids repeat across turns (`read_file_0`):
/// the earliest span at or after this generation and before a later
/// generation that produced the same id.
fn tool_results(
    completion: &Completion,
    own: &Span,
    trace: &TraceView<'_>,
) -> BTreeMap<String, ToolOutput> {
    let mut out = BTreeMap::new();
    let wanted: HashSet<&str> = completion
        .tool_calls
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    if wanted.is_empty() {
        return out;
    }
    let start = start_of(own);
    // (start, input order, call id, span) of every candidate result.
    let mut runs: Vec<(u64, usize, &str, &Span)> = Vec::new();
    // Later generations' produced ids are parsed only when a candidate lies after one.
    let mut later: Vec<(u64, SpanRef<'_>)> = Vec::new();
    for (i, r) in trace.spans().enumerate() {
        let (span, t) = (r.span, start_of(r.span));
        match operation(span) {
            Some(EXECUTE_TOOL) if t >= start => {
                if let Some(id) = Attrs(&span.attributes)
                    .str("gen_ai.tool.call.id")
                    .and_then(|id| wanted.get(id).copied())
                {
                    runs.push((t, i, id, span));
                }
            }
            Some(op) if INFERENCE_OPS.contains(&op) && t > start && span.span_id != own.span_id => {
                later.push((t, r));
            }
            _ => {}
        }
    }
    runs.sort_by_key(|(t, i, _, _)| (*t, *i));
    later.sort_by_key(|(t, _)| *t);
    let mut produced: HashMap<usize, Vec<String>> = HashMap::new();
    let mut closed: HashSet<&str> = HashSet::new();
    for (t, _, id, span) in runs {
        if closed.contains(id) {
            continue;
        }
        let reused = later
            .iter()
            .enumerate()
            .take_while(|(_, (lt, _))| *lt <= t)
            .any(|(k, (_, l))| {
                produced
                    .entry(k)
                    .or_insert_with(|| produced_call_ids(*l, trace))
                    .iter()
                    .any(|p| p == id)
            });
        closed.insert(id);
        if reused {
            continue;
        }
        let a = Attrs(&span.attributes);
        let content = response_text(
            a.get("gen_ai.tool.call.result")
                .map(any_value_to_json)
                .as_ref(),
        );
        let is_error =
            span.status.as_ref().is_some_and(|s| s.is_error()) || a.get("error.type").is_some();
        out.insert(id.to_string(), ToolOutput { content, is_error });
    }
    out
}

/// Trace id and time bounds: the span's, or for an orphan unit the first
/// record's trace id (lowercased) and the min/max record time.
fn unit_bounds(unit: &Unit<'_>) -> (String, u64, u64) {
    if let Some(span) = unit.span {
        return (
            span.trace_id.to_string(),
            nanos(span.start_time_unix_nano).unwrap_or(0),
            nanos(span.end_time_unix_nano).unwrap_or(0),
        );
    }
    let times: Vec<u64> = unit
        .logs
        .iter()
        .filter_map(|l| {
            nanos(l.record.time_unix_nano)
                .filter(|t| *t > 0)
                .or_else(|| nanos(l.record.observed_time_unix_nano))
        })
        .collect();
    let trace_id = unit
        .logs
        .first()
        .map(|l| l.record.trace_id.to_ascii_lowercase())
        .unwrap_or_default();
    (
        trace_id,
        times.iter().copied().min().unwrap_or(0),
        times.iter().copied().max().unwrap_or(0),
    )
}

fn extract_unit(unit: &Unit<'_>, exclusive: &[&str]) -> Result<Generation, SkipReason> {
    let (resource, scope) = (unit.resource, unit.scope);
    let lk = Lookup::for_unit(unit);
    let id = generation_id(&lk, unit).ok_or(SkipReason::MissingPayload)?;
    let input = lk.content("gen_ai.input.messages")?;
    let system = lk.content("gen_ai.system_instructions")?;
    let output = lk.content("gen_ai.output.messages")?;
    let outputs = match &output {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(SkipReason::Truncated),
    };

    let legacy_in = if input.is_none() && system.is_none() {
        legacy_events(unit, &LEGACY_INPUT)
    } else {
        Vec::new()
    };
    let legacy_choices = if output.is_none() {
        legacy_choices(unit)
    } else {
        Vec::new()
    };

    let mut messages: Vec<Message> = system
        .as_ref()
        .and_then(system_message)
        .into_iter()
        .collect();
    if let Some(input) = &input {
        messages.extend(convert_messages(input)?);
    } else {
        messages.extend(
            legacy_in
                .iter()
                .map(|(name, body)| legacy_message(name, body)),
        );
    }
    let absent = Absent {
        prompt: input.is_none() && legacy_in.is_empty(),
        completion: output.is_none() && legacy_choices.is_empty(),
    };
    let (completion, output_parts) = match (outputs.first(), legacy_choices.first()) {
        (Some(first), _) => convert_completion(first),
        (None, Some(choice)) => (legacy_completion(choice), Vec::new()),
        (None, None) => Default::default(),
    };
    let continues = non_empty(lk.string("gen_ai.request.previous_response.id"));
    let history = if continues.is_some() || absent.prompt {
        History::Delta
    } else {
        History::Full
    };
    let compacted = lk.bool("gen_ai.conversation.compacted") == Some(true)
        || input.as_ref().is_some_and(has_compaction);
    let reasons = finish_reasons(&lk);
    let finish_reason = reasons
        .first()
        .and_then(Value::as_str)
        .or_else(|| {
            outputs
                .first()
                .and_then(|m| m.get("finish_reason"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            legacy_choices
                .first()
                .and_then(|c| c.get("finish_reason"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let mut source_meta = source_meta(&lk, scope, &reasons, &outputs, output_parts);
    // `outputs` is empty whenever legacy choices are read.
    if legacy_choices.len() > 1 {
        source_meta.insert(
            "choices".into(),
            Value::Array(
                legacy_choices[1..]
                    .iter()
                    .cloned()
                    .map(Value::Object)
                    .collect(),
            ),
        );
    }

    let (trace_id, start_ns, end_ns) = unit_bounds(unit);

    // Struct literal (see `usage`).
    Ok(Generation {
        id,
        profile: "semconv".to_string(),
        trace_id,
        start_ns,
        end_ns,
        session_id: session_id(&lk),
        user_id: non_empty(lk.string("user.id")),
        client_key: Attrs(&resource.attributes)
            .str("service.name")
            .map(str::to_string),
        messages: messages.into(),
        completion,
        usage: usage(&lk, scope.name, exclusive),
        request_model: lk.string("gen_ai.request.model"),
        response_model: lk.string("gen_ai.response.model"),
        provider: non_empty(lk.string("gen_ai.provider.name"))
            .or_else(|| non_empty(lk.string("gen_ai.system"))),
        finish_reason,
        source_meta,
        continues,
        history,
        absent,
        compacted,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_of(input: u64, read: u64, write: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            cached_input_tokens: Some(read),
            cache_write_tokens: Some(write),
            ..Default::default()
        }
    }

    #[test]
    fn exclusive_scope_passes_input_through() {
        let basis = source_basis("x.anthropic", &["x.anthropic"]);
        assert_eq!(basis, CacheBasis::Exclusive);
        let u = usage_of(11, 7, 5).with_basis(basis);
        assert_eq!(u.input_tokens, Some(11));
        assert_eq!(u.cache_basis, Some(CacheBasis::Exclusive));
    }

    #[test]
    fn other_scopes_are_inclusive_and_subtract_cache() {
        let basis = source_basis("other", &["x.anthropic"]);
        assert_eq!(basis, CacheBasis::Inclusive);
        let u = usage_of(23, 7, 5).with_basis(basis);
        assert_eq!(u.input_tokens, Some(11));
        assert_eq!(u.cache_basis, Some(CacheBasis::Inclusive));
    }

    #[test]
    fn inclusive_subtraction_keeps_absent_input_absent_and_saturates() {
        let cache_only = Usage {
            cached_input_tokens: Some(4),
            ..Default::default()
        }
        .with_basis(CacheBasis::Inclusive);
        assert_eq!(cache_only.input_tokens, None, "no input count stays absent");
        let short = usage_of(3, 7, 5).with_basis(CacheBasis::Inclusive);
        assert_eq!(short.input_tokens, Some(0));
        let huge = usage_of(5, u64::MAX, u64::MAX).with_basis(CacheBasis::Inclusive);
        assert_eq!(huge.input_tokens, Some(0));
    }

    #[test]
    fn the_shipped_table_is_empty() {
        assert!(EXCLUSIVE_INPUT_SCOPES.is_empty());
    }
}
