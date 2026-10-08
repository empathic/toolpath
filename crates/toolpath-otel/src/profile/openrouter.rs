//! OpenRouter Broadcast: one `LLM Generation` root span per API request
//! carries everything read; its children duplicate it.

use crate::generation::{
    Absent, CacheBasis, Completion, Cost, Generation, History, Message, ToolCall, Usage,
};
use crate::hash::{canonical_json, sha256_hex};
use crate::otlp::{Attrs, Resource, Scope, Span, nanos};
use crate::profile::{Ident, Profile, TraceView, Unit};
use crate::walk::memo::parse_at;
use crate::walk::scan;
use crate::walk::{ReadCx, SkipReason};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The root span of one request.
pub const ROOT_SPAN: &str = "LLM Generation";
/// OpenRouter's destination test.
pub const CONNECTION_TEST_SPAN: &str = "openrouter-connection-test";
/// The Broadcast settings' "Test" button: a canned conversation, not a real request.
pub const TEST_GENERATION_SPAN: &str = "Test Generation";

const SERVICE_NAME: &str = "service.name";
/// The profile's name.
pub(crate) const NAME: &str = "openrouter";
const SERVICE: &str = "openrouter";
const SCOPE: &str = "openrouter";
const METADATA_PREFIX: &str = "trace.metadata.openrouter.";
const GENERATION_CHILD: &str = "generation";
const ATTEMPT_CHILD_PREFIX: &str = "provider attempt ";

/// OpenRouter's resource or scope marker.
fn marked(resource: &Resource, scope: &Scope) -> bool {
    Attrs(&resource.attributes).str(SERVICE_NAME) == Some(SERVICE) || scope.name == SCOPE
}

/// A root span named exactly `Test Generation`.
fn is_test_generation(span: &Span) -> bool {
    span.name == TEST_GENERATION_SPAN && span.parent_span_id.is_empty()
}

/// The `openrouter` profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenRouter;

impl Profile for OpenRouter {
    fn name(&self) -> &'static str {
        NAME
    }

    /// `LLM Generation` or the connection test with OpenRouter provenance
    /// (`trace.metadata.openrouter.*` counts, for Collectors that rewrite
    /// resources). `Test Generation` needs the resource or scope marker: the
    /// name alone is too generic.
    fn claims(&self, resource: &Resource, scope: &Scope, span: &Span) -> bool {
        if is_test_generation(span) {
            return marked(resource, scope);
        }
        (span.name == ROOT_SPAN || span.name == CONNECTION_TEST_SPAN)
            && (matches!(
                Attrs(&resource.attributes).str(SERVICE_NAME),
                None | Some(SERVICE)
            ) || scope.name == SCOPE
                || span
                    .attributes
                    .iter()
                    .any(|kv| kv.key.starts_with(METADATA_PREFIX)))
    }

    /// Marked vendor children (`generation`, `provider attempt N: …`), which
    /// duplicate the root's `provider_responses`.
    fn absorbs(&self, resource: &Resource, scope: &Scope, span: &Span) -> bool {
        (span.name == GENERATION_CHILD || span.name.starts_with(ATTEMPT_CHILD_PREFIX))
            && marked(resource, scope)
    }

    /// The destination test and the Broadcast test generation.
    fn pre_skip(&self, unit: &Unit<'_>) -> Option<SkipReason> {
        unit.span
            .filter(|s| s.name == CONNECTION_TEST_SPAN || is_test_generation(s))
            .map(|_| SkipReason::ConnectionTest)
    }

    fn session_meta_keys(&self) -> &'static [&'static str] {
        &["creator_user_id", "entity_id"]
    }

    fn identify(&self, unit: &Unit<'_>) -> Ident {
        let Some(span) = unit.span else {
            return Ident::default();
        };
        let a = Attrs(&span.attributes);
        Ident {
            generation_id: response_id(&a).map(str::to_string),
            session_id: a.str("session.id").map(str::to_string),
        }
    }

    fn extract<'a>(
        &self,
        unit: &Unit<'a>,
        _trace: &TraceView<'a>,
        cx: &mut ReadCx<'a>,
    ) -> std::result::Result<Generation, SkipReason> {
        let span = unit.span.ok_or(SkipReason::MissingPayload)?;
        let a = Attrs(&span.attributes);
        let id = response_id(&a).ok_or(SkipReason::MissingPayload)?;
        parse_generation(span, &a, id, cx)
    }
}

/// The generation id; an empty string is no id.
fn response_id<'a>(a: &Attrs<'a>) -> Option<&'a str> {
    a.str("gen_ai.response.id").filter(|id| !id.is_empty())
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value.filter(|v| !v.is_null()) {
        map.insert(key.to_string(), v);
    }
}

/// `rawRequest` fields carried elsewhere (the history, the tool list, the
/// Responses-API input), dropped from `request_params`.
const RAW_REQUEST_PAYLOAD_KEYS: [&str; 3] = ["messages", "tools", "input"];

/// Latency attributes, stored flat under the attribute's last segment.
const LATENCY_ATTRS: [&str; 7] = [
    "first_token_ms",
    "router_latency_ms",
    "provider_request_ms",
    "provider_headers_ms",
    "provider_body_end_ms",
    "provider_time_to_first_token_ms",
    "inter_token_latency_ms",
];

/// sha256 hex of the canonical JSON of `completion.tools`, else
/// `rawRequest.tools` (where OpenRouter actually puts them).
fn tools_digest(completion: &Value, raw_request: &Value) -> Option<String> {
    let tools = [completion.get("tools"), raw_request.get("tools")]
        .into_iter()
        .flatten()
        .find(|t| !t.is_null())?;
    Some(sha256_hex(&[canonical_json(tools).as_bytes()]))
}

/// What a generation reads from the completion text.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct CompletionParts {
    /// The members other than `rawRequest` (and, when read member by
    /// member, `tools`); the last copy of a key wins.
    fields: Map<String, Value>,
    /// `rawRequest` less its payload keys.
    request_params: Value,
    request_session_id: Option<String>,
    tools_digest: Option<String>,
}

/// The completion text parsed whole; `None` when it does not parse as a
/// JSON object.
pub(crate) fn completion_whole(raw: &str) -> Option<CompletionParts> {
    let c: Value = serde_json::from_str(raw).ok()?;
    let raw_request = c.get("rawRequest").cloned().unwrap_or(Value::Null);
    let tools_digest = tools_digest(&c, &raw_request);
    let request_session_id = raw_request
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let request_params = match raw_request {
        Value::Object(mut params) => {
            for k in RAW_REQUEST_PAYLOAD_KEYS {
                params.remove(k);
            }
            Value::Object(params)
        }
        other => other,
    };
    let Value::Object(mut fields) = c else {
        return None;
    };
    fields.remove("rawRequest");
    fields.remove("tools");
    Some(CompletionParts {
        fields,
        request_params,
        request_session_id,
        tools_digest,
    })
}

/// The completion text member by member: a tool list seen before in the
/// read is checked and digested once. Gives what [`completion_whole`]
/// gives, or `None` when the text is not a plain object or a member does
/// not parse (then the whole-text parse decides).
pub(crate) fn completion_members<'a>(raw: &'a str, cx: &mut ReadCx<'a>) -> Option<CompletionParts> {
    let mut fields = Map::new();
    let mut tools = None;
    let mut raw_request = (None, Value::Null);
    for (k, v) in scan::members(raw, |k, rest| cx.known(k, rest))? {
        match k {
            "tools" => {
                cx.check(v, 1).then_some(())?;
                tools = Some(v);
            }
            "rawRequest" => raw_request = raw_request_members(v, cx)?,
            _ => {
                fields.insert(k.to_string(), parse_at(v, 1)?);
            }
        }
        cx.remember(k, v);
    }
    let (request_tools, request_params) = raw_request;
    let tools_digest = match (tools.filter(|t| *t != "null"), request_tools) {
        (Some(t), _) => cx.value_digest(t, 1),
        (None, Some(t)) => cx.value_digest(t, 2),
        (None, None) => None,
    };
    let request_session_id = request_params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(CompletionParts {
        fields,
        request_params,
        request_session_id,
        tools_digest,
    })
}

/// `rawRequest`'s non-null `tools` text and the request less its payload
/// keys. Payload keys are inserted and removed as the whole-text path
/// does, so the map's key order matches with `preserve_order` too.
fn raw_request_members<'a>(raw: &'a str, cx: &mut ReadCx<'a>) -> Option<(Option<&'a str>, Value)> {
    if !raw.starts_with('{') {
        return Some((None, parse_at(raw, 1)?));
    }
    let mut params = Map::new();
    let mut tools = None;
    for (k, v) in scan::members(raw, |k, rest| cx.known(k, rest))? {
        if RAW_REQUEST_PAYLOAD_KEYS.contains(&k) {
            cx.check(v, 2).then_some(())?;
            params.insert(k.to_string(), Value::Null);
            if k == "tools" {
                tools = Some(v);
            }
        } else {
            params.insert(k.to_string(), parse_at(v, 2)?);
        }
        cx.remember(k, v);
    }
    for k in RAW_REQUEST_PAYLOAD_KEYS {
        params.remove(k);
    }
    Some((tools.filter(|t| *t != "null"), Value::Object(params)))
}

/// The prompt text parsed whole; `None` when it does not parse.
fn prompt_whole(raw: &str) -> Option<Vec<Message>> {
    #[derive(Deserialize)]
    struct Prompt {
        messages: Vec<Message>,
    }
    Some(serde_json::from_str::<Prompt>(raw).ok()?.messages)
}

/// The prompt's messages: through the read's memos, else
/// [`prompt_whole`]. `None` when the prompt does not parse.
#[cfg(test)]
pub(crate) fn prompt_messages<'a>(raw: &'a str, cx: &mut ReadCx<'a>) -> Option<Vec<Message>> {
    match cx.prompt(raw) {
        Some(tail) => Some(cx.messages(tail)),
        None => prompt_whole(raw),
    }
}

fn parse_generation<'a>(
    span: &Span,
    a: &Attrs<'a>,
    id: &str,
    cx: &mut ReadCx<'a>,
) -> std::result::Result<Generation, SkipReason> {
    // Privacy Mode: either side may be absent (a skeleton on that side).
    let prompt_raw = payload(a, "gen_ai.prompt")?;
    let completion_raw = payload(a, "gen_ai.completion")?;
    // A memo-read prompt is handed out by the walk once the read is done
    // (`cx.tail`), so the read's prompts share their messages.
    let mut messages = Vec::new();
    if let Some(raw) = prompt_raw {
        match cx.prompt(raw) {
            Some(tail) => cx.tail = Some(tail),
            None => messages = prompt_whole(raw).ok_or(SkipReason::Truncated)?,
        }
    }
    // An absent completion reads as `null`, so every field below defaults.
    let parts = match completion_raw {
        Some(raw) => completion_members(raw, cx)
            .or_else(|| completion_whole(raw))
            .ok_or(SkipReason::Truncated)?,
        None => CompletionParts::default(),
    };
    let c = &parts.fields;
    let absent = Absent {
        prompt: prompt_raw.is_none(),
        completion: completion_raw.is_none(),
    };
    let tool_calls = match c.get("toolCalls") {
        Some(v) if !v.is_null() => {
            Vec::<ToolCall>::deserialize(v).map_err(|_| SkipReason::Truncated)?
        }
        _ => Vec::new(),
    };
    let completion = Completion {
        text: c
            .get("completion")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        reasoning: c
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::to_string),
        tool_calls,
        reasoning_details: Vec::new(),
    };
    let s = |k: &str| a.str(k).map(|v| Value::String(v.to_string()));
    let f = |k: &str| a.f64(k).map(|v| json!(v));

    let mut meta = Map::new();
    put(
        &mut meta,
        "api_key_name",
        s("trace.metadata.openrouter.api_key_name"),
    );
    put(
        &mut meta,
        "creator_user_id",
        s("trace.metadata.openrouter.creator_user_id"),
    );
    put(
        &mut meta,
        "entity_id",
        s("trace.metadata.openrouter.entity_id"),
    );
    put(
        &mut meta,
        "openrouter_user_id",
        s("trace.metadata.openrouter.user_id"),
    );
    put(
        &mut meta,
        "provider_name",
        s("trace.metadata.openrouter.provider_name"),
    );
    put(
        &mut meta,
        "provider_slug",
        s("trace.metadata.openrouter.provider_slug"),
    );
    put(
        &mut meta,
        "upstream_finish_reason",
        s("trace.metadata.openrouter.finish_reason"),
    );
    for name in LATENCY_ATTRS {
        put(
            &mut meta,
            name,
            f(&format!("trace.metadata.openrouter.{name}")),
        );
    }
    let mut unit_price = Map::new();
    put(
        &mut unit_price,
        "input",
        f("trace.metadata.openrouter.input_unit_price"),
    );
    put(
        &mut unit_price,
        "output",
        f("trace.metadata.openrouter.output_unit_price"),
    );
    if !unit_price.is_empty() {
        meta.insert("unit_price".into(), Value::Object(unit_price));
    }
    put(
        &mut meta,
        "provider_responses",
        a.str("trace.metadata.provider_responses")
            .map(|r| serde_json::from_str(r).unwrap_or_else(|_| Value::String(r.to_string()))),
    );
    put(&mut meta, "request_params", Some(parts.request_params));
    put(
        &mut meta,
        "tools_digest",
        parts.tools_digest.map(Value::String),
    );

    Ok(Generation {
        id: id.to_string(),
        trace_id: span.trace_id.to_string(),
        start_ns: nanos(span.start_time_unix_nano).unwrap_or(0),
        end_ns: nanos(span.end_time_unix_nano).unwrap_or(0),
        session_id: a.str("session.id").map(str::to_string),
        request_session_id: parts.request_session_id,
        user_id: a.str("user.id").map(str::to_string),
        client_key: a
            .str("trace.metadata.openrouter.api_key_name")
            .map(str::to_string),
        messages: messages.into(),
        completion,
        usage: Usage {
            input_tokens: a.u64("gen_ai.usage.input_tokens"),
            output_tokens: a.u64("gen_ai.usage.output_tokens"),
            cached_input_tokens: a.u64("gen_ai.usage.input_tokens.cached"),
            cache_write_tokens: a.u64("gen_ai.usage.input_tokens.cache_write"),
            cache_write_5m_tokens: a.u64("gen_ai.usage.input_tokens.cache_write_5m"),
            cache_write_1h_tokens: a.u64("gen_ai.usage.input_tokens.cache_write_1h"),
            reasoning_tokens: a.u64("gen_ai.usage.output_tokens.reasoning"),
            total_tokens: a.u64("gen_ai.usage.total_tokens"),
            cache_basis: None,
        }
        .with_basis(CacheBasis::Inclusive),
        cost: Cost {
            input: a.f64("gen_ai.usage.input_cost"),
            output: a.f64("gen_ai.usage.output_cost"),
            total: a.f64("gen_ai.usage.total_cost"),
        },
        request_model: a.str("gen_ai.request.model").map(str::to_string),
        response_model: a.str("gen_ai.response.model").map(str::to_string),
        provider: a.str("gen_ai.provider.name").map(str::to_string),
        finish_reason: a.str("gen_ai.response.finish_reason").map(str::to_string),
        source_meta: meta,
        history: if absent.prompt {
            History::Delta
        } else {
            History::Full
        },
        absent,
        ..Default::default()
    })
}

/// A payload attribute: `None` when missing or valueless (proto3 unset), the
/// `stringValue`, else `Truncated` (present-but-corrupt is never a skeleton).
fn payload<'a>(a: &Attrs<'a>, key: &str) -> std::result::Result<Option<&'a str>, SkipReason> {
    let Some(v) = a.get(key) else {
        return Ok(None);
    };
    match v {
        Value::Null => Ok(None),
        Value::Object(o) if o.values().all(Value::is_null) => Ok(None),
        Value::Object(o) => match o.get("stringValue") {
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            _ => Err(SkipReason::Truncated),
        },
        _ => Err(SkipReason::Truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::OtelError;
    use crate::profile::ProfileSelection;
    use crate::walk::ReadOutcome;

    fn read_deliveries<'a>(
        d: impl IntoIterator<Item = &'a serde_json::Value>,
    ) -> crate::Result<ReadOutcome> {
        crate::walk::read_deliveries(d, ProfileSelection::Auto)
    }
    use serde_json::json;

    fn delivery(attrs: serde_json::Value, status: serde_json::Value) -> serde_json::Value {
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
            "traceId": "t1", "name": "LLM Generation",
            "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
            "attributes": attrs, "status": status
        }]}]}]})
    }

    fn attr(k: &str, v: &str) -> serde_json::Value {
        json!({"key": k, "value": {"stringValue": v}})
    }

    #[test]
    fn non_otlp_value_is_an_error() {
        assert!(matches!(
            read_deliveries(&[json!({"sessions": {}})]),
            Err(OtelError::NotOtlp)
        ));
    }

    #[test]
    fn missing_prompt_and_completion_read_as_a_skeleton() {
        let d = delivery(
            json!([attr("gen_ai.response.id", "gen-1")]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
        let g = &out.generations[0];
        assert_eq!(g.id, "gen-1");
        assert!(g.absent.prompt && g.absent.completion);
        assert_eq!(g.history, History::Delta);
        assert!(g.messages.is_empty());
        assert_eq!(g.completion, Completion::default());
    }

    #[test]
    fn unparseable_prompt_is_skipped_as_truncated() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-2"),
                attr("session.id", "s1"),
                attr(
                    "gen_ai.prompt",
                    "{\"messages\":[{\"role\":\"user\",\"content\":\"hi"
                ),
                attr("gen_ai.completion", "{\"completion\":\"ok\"}")
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated);
        assert_eq!(out.skipped[0].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn parts_content_and_missing_span_input_are_fine() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-3"),
                attr(
                    "gen_ai.prompt",
                    r#"{"messages":[{"role":"user","content":[{"type":"text","text":"a","cache_control":{"type":"ephemeral"}}]}]}"#
                ),
                attr(
                    "gen_ai.completion",
                    r#"{"completion":null,"reasoning":null,"toolCalls":null}"#
                )
            ]),
            json!({"code": "STATUS_CODE_OK"}),
        );
        let out = read_deliveries(&[d]).unwrap();
        let g = &out.generations[0];
        assert_eq!(g.completion.text, "");
        assert!(g.completion.tool_calls.is_empty());
        assert!(g.messages[0].content.is_array());
    }

    fn ok_generation(id: &str, extra: Vec<serde_json::Value>) -> serde_json::Value {
        let mut attrs = vec![
            attr("gen_ai.response.id", id),
            attr(
                "gen_ai.prompt",
                r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            ),
            attr("gen_ai.completion", r#"{"completion":"ok"}"#),
        ];
        attrs.extend(extra);
        delivery(json!(attrs), json!({"code": 1}))
    }

    #[test]
    fn empty_response_id_is_treated_as_absent() {
        let missing = delivery(
            json!([
                attr(
                    "gen_ai.prompt",
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                ),
                attr("gen_ai.completion", r#"{"completion":"ok"}"#),
            ]),
            json!({"code": 1}),
        );
        let empty = ok_generation("", Vec::new());
        let a = read_deliveries(&[missing]).unwrap();
        let b = read_deliveries(&[empty]).unwrap();
        assert!(a.generations.is_empty() && b.generations.is_empty());
        assert_eq!(a.skipped.len(), 1);
        assert_eq!(b.skipped.len(), 1);
        assert_eq!(b.skipped[0].reason, a.skipped[0].reason);
        assert_eq!(b.skipped[0].generation_id, None);
    }

    #[test]
    fn int_value_as_string_or_number_both_read() {
        let as_string = ok_generation(
            "gen-s",
            vec![json!({"key": "gen_ai.usage.input_tokens", "value": {"intValue": "123"}})],
        );
        let as_number = ok_generation(
            "gen-n",
            vec![json!({"key": "gen_ai.usage.input_tokens", "value": {"intValue": 456}})],
        );
        let garbage = ok_generation(
            "gen-g",
            vec![json!({"key": "gen_ai.usage.input_tokens", "value": {"intValue": "12x"}})],
        );
        let out = read_deliveries(&[as_string, as_number, garbage]).unwrap();
        let tokens: Vec<Option<u64>> = out
            .generations
            .iter()
            .map(|g| g.usage.input_tokens)
            .collect();
        assert_eq!(tokens, vec![Some(123), Some(456), None]);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn status_code_error_enum_name_is_an_error_status_skip() {
        let bad = delivery(
            json!([
                attr("gen_ai.response.id", "gen-e"),
                attr("session.id", "s9")
            ]),
            json!({"code": "STATUS_CODE_ERROR", "message": "client disconnected"}),
        );
        let good = ok_generation("gen-ok", vec![]);
        let out = read_deliveries(&[bad, good]).unwrap();
        assert_eq!(out.generations.len(), 1);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].reason, SkipReason::ErrorStatus);
        assert_eq!(out.skipped[0].generation_id.as_deref(), Some("gen-e"));
        assert_eq!(out.skipped[0].session_id.as_deref(), Some("s9"));
    }

    #[test]
    fn privacy_mode_root_without_prompt_is_skipped_not_fatal() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-2"),
                attr(
                    "gen_ai.completion",
                    "{\"completion\":\"ok\",\"toolCalls\":[{\"id\":\"c1\",\"function\":{\"name\":\"Bash\",\"arguments\":\"{}\"}}]}"
                ),
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "3"}},
                attr("gen_ai.request.model", "m-req"),
                attr("gen_ai.response.model", "m-resp")
            ]),
            json!({"code": 1}),
        );
        let g = read_deliveries(&[d]).unwrap().generations.remove(0);
        assert!(g.absent.prompt && !g.absent.completion);
        assert_eq!(g.completion.text, "ok");
        assert_eq!(g.completion.tool_calls[0].id, "c1");
        // A skeleton keeps its usage and models.
        assert_eq!(g.usage.input_tokens, Some(10));
        assert_eq!(g.usage.output_tokens, Some(3));
        assert_eq!(g.request_model.as_deref(), Some("m-req"));
        assert_eq!(g.response_model.as_deref(), Some("m-resp"));
    }

    #[test]
    fn a_completion_skeleton_keeps_its_full_prompt() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-3"),
                attr(
                    "gen_ai.prompt",
                    "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
                )
            ]),
            json!({"code": 1}),
        );
        let g = read_deliveries(&[d]).unwrap().generations.remove(0);
        assert!(!g.absent.prompt && g.absent.completion);
        assert_eq!(g.history, History::Full);
        assert_eq!(g.messages.len(), 1);
    }

    #[test]
    fn an_unparseable_present_side_is_still_truncated() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-4"),
                attr("gen_ai.completion", "{\"completion\":\"o")
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated);
    }

    #[test]
    fn a_non_string_prompt_is_truncated_not_a_skeleton() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-5"),
                {"key": "gen_ai.prompt", "value": {"intValue": "7"}},
                attr("gen_ai.completion", r#"{"completion":"ok"}"#)
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert!(out.generations.is_empty());
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated);
    }

    #[test]
    fn a_completion_that_is_not_an_object_is_truncated_not_empty() {
        for raw in ["[1]", "\"text\"", "5", "null"] {
            let d = delivery(
                json!([
                    attr("gen_ai.response.id", "gen-7"),
                    attr(
                        "gen_ai.prompt",
                        "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
                    ),
                    attr("gen_ai.completion", raw)
                ]),
                json!({"code": 1}),
            );
            let out = read_deliveries(&[d]).unwrap();
            assert!(out.generations.is_empty(), "{raw}: {:?}", out.generations);
            assert_eq!(out.skipped[0].reason, SkipReason::Truncated, "{raw}");
        }
    }

    #[test]
    fn a_non_string_completion_is_truncated_not_a_skeleton() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-6"),
                attr(
                    "gen_ai.prompt",
                    "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
                ),
                {"key": "gen_ai.completion", "value": {"intValue": "7"}}
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert!(out.generations.is_empty());
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated);
    }

    #[test]
    fn null_or_empty_payload_values_are_absent() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-7"),
                {"key": "gen_ai.prompt", "value": null},
                {"key": "gen_ai.completion", "value": {}}
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
        let g = &out.generations[0];
        assert!(g.absent.prompt && g.absent.completion);
        assert_eq!(g.history, History::Delta);
    }

    #[test]
    fn no_response_id_is_still_missing_payload() {
        let d = delivery(json!([attr("session.id", "s")]), json!({"code": 1}));
        assert_eq!(
            read_deliveries(&[d]).unwrap().skipped[0].reason,
            SkipReason::MissingPayload
        );
    }

    #[test]
    fn request_params_strip_messages_tools_and_input() {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-r"),
                attr(
                    "gen_ai.prompt",
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#
                ),
                attr(
                    "gen_ai.completion",
                    r#"{"completion":"ok","rawRequest":{"model":"m","temperature":0.2,"messages":[1],"tools":[2],"input":[3]}}"#
                )
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        let params = out.generations[0].source_meta["request_params"]
            .as_object()
            .unwrap();
        assert_eq!(params.len(), 2, "{params:?}");
        assert_eq!(params["model"], "m");
        assert_eq!(params["temperature"], 0.2);
        for k in ["messages", "tools", "input"] {
            assert!(!params.contains_key(k));
        }
    }

    #[test]
    fn latencies_are_flat_keys_named_by_attribute() {
        let d = ok_generation(
            "gen-l",
            vec![
                json!({"key": "trace.metadata.openrouter.first_token_ms", "value": {"doubleValue": 12.5}}),
                json!({"key": "trace.metadata.openrouter.router_latency_ms", "value": {"intValue": "3"}}),
            ],
        );
        let out = read_deliveries(&[d]).unwrap();
        let meta = &out.generations[0].source_meta;
        assert_eq!(meta["first_token_ms"], 12.5);
        assert_eq!(meta["router_latency_ms"], 3.0);
        assert!(!meta.contains_key("latency_ms"));
        assert!(!meta.contains_key("provider_request_ms"));
    }

    fn digest_of(completion: &str) -> Option<serde_json::Value> {
        let d = delivery(
            json!([
                attr("gen_ai.response.id", "gen-t"),
                attr(
                    "gen_ai.prompt",
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#
                ),
                attr("gen_ai.completion", completion)
            ]),
            json!({"code": 1}),
        );
        let out = read_deliveries(&[d]).unwrap();
        out.generations[0].source_meta.get("tools_digest").cloned()
    }

    #[test]
    fn tools_digest_prefers_completion_tools() {
        let both = digest_of(
            r#"{"completion":"ok","tools":[{"name":"A"}],"rawRequest":{"tools":[{"name":"B"}]}}"#,
        );
        let only_completion = digest_of(r#"{"completion":"ok","tools":[{"name":"A"}]}"#);
        let only_raw = digest_of(r#"{"completion":"ok","rawRequest":{"tools":[{"name":"B"}]}}"#);
        assert!(both.is_some());
        assert_eq!(both, only_completion);
        assert_ne!(both, only_raw);
    }

    #[test]
    fn tools_digest_falls_back_to_raw_request_tools() {
        let d = digest_of(
            r#"{"completion":"ok","tools":null,"rawRequest":{"model":"m","tools":[{"b":1,"a":2}]}}"#,
        )
        .expect("digest from rawRequest.tools");
        let hex = d.as_str().unwrap();
        assert_eq!(hex.len(), 64);
        let reordered = digest_of(r#"{"completion":"ok","rawRequest":{"tools":[{"a":2,"b":1}]}}"#);
        assert_eq!(Some(d.clone()), reordered);
        assert_eq!(hex, crate::hash::sha256_hex(&[br#"[{"a":2,"b":1}]"#]));
    }

    #[test]
    fn proto3_nulls_and_malformed_spans_never_abort_a_batch() {
        let nulls = json!({"resourceSpans": [
            {"scopeSpans": null},
            {"scopeSpans": [
                {"spans": null},
                {"spans": [{
                    "traceId": null, "name": "LLM Generation",
                    "startTimeUnixNano": null, "endTimeUnixNano": null,
                    "attributes": null, "status": null
                }]}
            ]}
        ]});
        let missing_key = {
            let mut d = ok_generation("gen-k", vec![]);
            d["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"value": {"stringValue": "orphan"}}));
            d
        };
        let batch = [
            ok_generation("gen-a", vec![]),
            nulls,
            missing_key,
            ok_generation("gen-b", vec![]),
        ];
        let out = read_deliveries(&batch).expect("never Err inside an OTLP object");
        let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["gen-a", "gen-k", "gen-b"]);
        assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
        assert_eq!(out.skipped[0].reason, SkipReason::MissingPayload);
        assert_eq!(out.skipped[0].generation_id, None);
    }

    #[test]
    fn wrongly_typed_fields_degrade_per_span() {
        let mut d = ok_generation("gen-w", vec![]);
        let span = &mut d["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        span["traceId"] = json!(42);
        span["attributes"]
            .as_array_mut()
            .unwrap()
            .extend([json!("not a kv"), json!({"key": 7, "value": null})]);
        let odd = json!({"resourceSpans": [
            "not a resource",
            {"scopeSpans": {"spans": []}},
            {"scopeSpans": ["not a scope", {"spans": [
                "not a span",
                {"name": "LLM Generation", "attributes": {"not": "a list"}}
            ]}]}
        ]});
        let mut weird_status = ok_generation("gen-s", vec![]);
        weird_status["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["status"] = json!("weird");
        let out = read_deliveries(&[d, odd, weird_status, ok_generation("gen-z", vec![])]).unwrap();
        let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["gen-w", "gen-z"]);
        assert_eq!(out.generations[0].trace_id, "");
        let reasons: Vec<_> = out.skipped.iter().map(|s| s.reason).collect();
        assert_eq!(
            reasons,
            vec![SkipReason::MissingPayload, SkipReason::ErrorStatus],
            "{:?}",
            out.skipped
        );
        assert_eq!(out.skipped[1].generation_id.as_deref(), Some("gen-s"));
    }

    fn with_status(id: &str, status: Option<serde_json::Value>) -> serde_json::Value {
        let mut d = ok_generation(id, vec![]);
        let span = d["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
            .as_object_mut()
            .unwrap();
        match status {
            Some(v) => span.insert("status".into(), v),
            None => span.remove("status"),
        };
        d
    }

    fn status_outcome(status: Option<serde_json::Value>) -> ReadOutcome {
        read_deliveries(&[with_status("gen-st", status)]).unwrap()
    }

    #[test]
    fn unreadable_status_fails_closed() {
        for bad in [
            json!(2),
            json!(1),
            json!("STATUS_CODE_ERROR"),
            json!("weird"),
            json!([]),
            json!(true),
            json!({"code": "ERROR"}),
            json!({"code": "STATUS_CODE_ERROR "}),
            json!({"code": 7}),
            json!({"code": 1.5}),
            json!({"code": {}}),
        ] {
            let out = status_outcome(Some(bad.clone()));
            assert!(out.generations.is_empty(), "{bad}");
            assert_eq!(out.skipped.len(), 1, "{bad}");
            assert_eq!(out.skipped[0].reason, SkipReason::ErrorStatus, "{bad}");
            assert_eq!(out.skipped[0].generation_id.as_deref(), Some("gen-st"));
        }
    }

    #[test]
    fn absent_null_and_recognized_ok_statuses_are_read() {
        for ok in [
            None,
            Some(json!(null)),
            Some(json!({})),
            Some(json!({"code": null})),
            Some(json!({"code": 0})),
            Some(json!({"code": 1})),
            Some(json!({"code": "STATUS_CODE_UNSET"})),
            Some(json!({"code": "STATUS_CODE_OK"})),
            Some(json!({"code": "status_code_ok"})),
            Some(json!({"code": "1"})),
            Some(json!({"code": "0"})),
        ] {
            let out = status_outcome(ok.clone());
            assert_eq!(out.generations.len(), 1, "{ok:?}");
            assert!(out.skipped.is_empty(), "{ok:?}");
        }
        let err = status_outcome(Some(json!({"code": 2})));
        assert_eq!(err.skipped[0].reason, SkipReason::ErrorStatus);
    }

    #[test]
    fn resource_spans_must_be_an_array_or_null() {
        for bad in [
            json!({"resourceSpans": {}}),
            json!({"resourceSpans": "x"}),
            json!([]),
            json!("resourceSpans"),
            json!(null),
        ] {
            assert!(
                matches!(
                    read_deliveries(std::slice::from_ref(&bad)),
                    Err(OtelError::NotOtlp)
                ),
                "{bad}"
            );
        }
        let empty = read_deliveries(&[json!({"resourceSpans": null})]).unwrap();
        assert!(empty.generations.is_empty() && empty.skipped.is_empty());
    }

    #[test]
    fn tools_digest_absent_when_no_tools() {
        assert_eq!(digest_of(r#"{"completion":"ok"}"#), None);
        assert_eq!(
            digest_of(r#"{"completion":"ok","rawRequest":{"model":"m"}}"#),
            None
        );
    }

    fn resource(service: &str) -> serde_json::Value {
        json!({"attributes": [{"key": "service.name", "value": {"stringValue": service}}]})
    }

    fn scoped(
        resource: serde_json::Value,
        scope: &str,
        spans: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        json!({"resourceSpans": [{"resource": resource, "scopeSpans": [{"scope": {"name": scope}, "spans": spans}]}]})
    }

    fn root_span(id: &str, extra: Vec<serde_json::Value>) -> serde_json::Value {
        let mut attrs = vec![
            attr("gen_ai.response.id", id),
            attr(
                "gen_ai.prompt",
                r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            ),
            attr("gen_ai.completion", r#"{"completion":"ok"}"#),
        ];
        attrs.extend(extra);
        json!({"traceId": id, "spanId": "r", "name": "LLM Generation", "attributes": attrs})
    }

    fn child(name: &str) -> serde_json::Value {
        json!({"traceId": "gen-c", "spanId": name, "parentSpanId": "r", "name": name,
               "attributes": [attr("gen_ai.operation.name", "chat")]})
    }

    #[test]
    fn roots_are_claimed_only_with_openrouter_provenance() {
        let meta = attr("trace.metadata.openrouter.api_key_name", "k");
        for (d, claimed) in [
            (
                scoped(resource("openrouter"), "x", vec![root_span("g1", vec![])]),
                true,
            ),
            (
                scoped(
                    resource("collector"),
                    "openrouter",
                    vec![root_span("g2", vec![])],
                ),
                true,
            ),
            (
                scoped(
                    resource("collector"),
                    "x",
                    vec![root_span("g3", vec![meta.clone()])],
                ),
                true,
            ),
            (scoped(json!({}), "x", vec![root_span("g4", vec![])]), true),
            (
                scoped(resource("collector"), "x", vec![root_span("g5", vec![])]),
                false,
            ),
        ] {
            let out = read_deliveries(&[d]).unwrap();
            assert_eq!(out.generations.len(), usize::from(claimed), "{claimed}");
            assert_eq!(out.unclaimed, usize::from(!claimed));
        }
    }

    #[test]
    fn children_are_absorbed_by_marker_never_unclaimed() {
        let spans = vec![
            root_span("gen-c", vec![]),
            child("generation"),
            child("provider attempt 1: Anthropic"),
        ];
        let by_resource = scoped(resource("openrouter"), "x", spans.clone());
        let by_scope = scoped(resource("collector"), "openrouter", spans);
        let out = read_deliveries(&[by_resource, by_scope]).unwrap();
        assert_eq!(out.generations.len(), 1);
        assert_eq!(out.skipped[0].reason, SkipReason::Duplicate);
        assert_eq!(out.unclaimed, 0);
    }

    #[test]
    fn a_markerless_child_is_unclaimed() {
        let meta = attr("trace.metadata.openrouter.api_key_name", "k");
        let d = scoped(
            resource("collector"),
            "x",
            vec![root_span("gen-c", vec![meta]), child("generation")],
        );
        // Named profile: under Auto, semconv claims the child and the
        // ancestor rule absorbs it.
        let only = crate::profile::ProfileSelection::OpenRouter;
        let out = crate::walk::read_deliveries(&[d], only).unwrap();
        assert_eq!((out.generations.len(), out.unclaimed), (1, 1));
    }

    #[test]
    fn a_nested_root_is_absorbed_by_the_ancestor_rule() {
        let outer = root_span("gen-o", vec![]);
        let mut inner = root_span("gen-i", vec![]);
        inner["traceId"] = json!("gen-o");
        inner["spanId"] = json!("inner");
        inner["parentSpanId"] = json!("r");
        let d = scoped(resource("openrouter"), "openrouter", vec![outer, inner]);
        let out = read_deliveries(&[d]).unwrap();
        let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["gen-o"]);
        assert_eq!(out.unclaimed, 0);
    }
    /// The Broadcast test button's span shape, with dummy values.
    fn test_generation() -> serde_json::Value {
        let span = json!({
            "traceId": "t-test", "spanId": "s-test", "name": "Test Generation",
            "kind": 3, "status": {"code": 1},
            "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
            "attributes": [
                attr("gen_ai.operation.name", "chat"),
                attr("gen_ai.request.model", "openai/gpt-4-turbo"),
                attr("gen_ai.response.id", "gen-test-0"),
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "10"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "5"}},
                attr("gen_ai.prompt", r#"{"messages":[{"role":"user","content":"hello"}]}"#),
                attr("gen_ai.completion", r#"{"completion":"hi there"}"#),
            ]
        });
        scoped(resource("openrouter"), "openrouter", vec![span])
    }

    #[test]
    fn broadcast_test_generation_is_a_connection_test() {
        for sel in [ProfileSelection::Auto, ProfileSelection::OpenRouter] {
            let out = crate::walk::read_deliveries(&[test_generation()], sel).unwrap();
            assert!(out.generations.is_empty(), "{sel:?}");
            assert_eq!(out.unclaimed, 0, "{sel:?}");
            let reasons: Vec<SkipReason> = out.skipped.iter().map(|s| s.reason).collect();
            assert_eq!(reasons, vec![SkipReason::ConnectionTest], "{sel:?}");
        }
    }

    #[test]
    fn test_generation_needs_the_openrouter_marker_and_a_root() {
        let mut d = test_generation();
        d["resourceSpans"][0]["resource"] = resource("collector");
        d["resourceSpans"][0]["scopeSpans"][0]["scope"]["name"] = json!("x");
        let only = ProfileSelection::OpenRouter;
        let out = crate::walk::read_deliveries(&[d.clone()], only).unwrap();
        assert!(out.skipped.is_empty());
        assert_eq!((out.generations.len(), out.unclaimed), (0, 1));
        let out = read_deliveries(&[d]).unwrap();
        assert!(out.skipped.is_empty());
        let profiles: Vec<&str> = out.generations.iter().map(|g| g.profile.as_str()).collect();
        assert_eq!(profiles, vec!["semconv"]);

        let mut nested = test_generation();
        nested["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["parentSpanId"] = json!("p");
        let out = read_deliveries(&[nested]).unwrap();
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn llm_generation_is_not_a_test_generation() {
        let d = scoped(
            resource("openrouter"),
            "openrouter",
            vec![root_span("gen-r", vec![])],
        );
        let out = read_deliveries(&[d]).unwrap();
        assert_eq!(out.generations.len(), 1);
        assert!(out.skipped.is_empty());
    }
}
