"""OpenAI Responses with server-side state (previous_response_id) via
opentelemetry-instrumentation-genai-openai (openai-v2 2.4b0 does not wrap
responses.create). Each request after the first carries only the new input."""
from mock_openai import serve as MOCK  # noqa: F401
from scenarios.common import (ANSWER, ASK, FILE_TEXT, FOLLOW_UP, READ_FILE_SCHEMA,
                              SEMCONV_CONTENT_KEYS, SUMMARY, SYSTEM, TURN_ROLES,
                              UTIL_GENAI_EVENT_ONLY, UTIL_GENAI_SPAN_ONLY, is_semconv_llm_span)

NAME = "openai-responses"
FIXTURE_DIR = "semconv/openai-responses/span"
DESCRIPTION = __doc__
ENV = dict(UTIL_GENAI_SPAN_ONLY)
EVENT_ENV = dict(UTIL_GENAI_EVENT_ONLY)
CONTENT_KEYS = SEMCONV_CONTENT_KEYS
is_llm_span = is_semconv_llm_span

MODEL = "gpt-4.1-mini"
SERVED_MODEL = "gpt-4.1-mini-2025-04-14"
TOOLS = [{"type": "function", "name": "read_file", "description": "Read a file",
          "parameters": READ_FILE_SCHEMA}]
USAGE = [(40, 15, 0, 6), (64, 9, 32, 0), (80, 7, 48, 0)]  # input, output, cached, reasoning


def response(i, output):
    inp, out, cached, reasoning = USAGE[i]
    return {"id": f"resp_fixture_{i + 1}", "object": "response", "created_at": 1800000000 + i,
            "status": "completed", "model": SERVED_MODEL, "output": output,
            "parallel_tool_calls": True, "tool_choice": "auto", "tools": [],
            "previous_response_id": None if i == 0 else f"resp_fixture_{i}",
            "usage": {"input_tokens": inp, "input_tokens_details": {"cached_tokens": cached},
                      "output_tokens": out,
                      "output_tokens_details": {"reasoning_tokens": reasoning},
                      "total_tokens": inp + out}}


def text(i, body):
    return {"type": "message", "id": f"msg_fixture_{i}", "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": body, "annotations": []}]}


SCRIPT = {"responses": [
    response(0, [{"type": "function_call", "id": "fc_fixture_1", "call_id": "call_fixture_r1",
                  "name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}",
                  "status": "completed"}]),
    response(1, [text(2, ANSWER)]),
    response(2, [text(3, SUMMARY)]),
]}


def instrumentor():
    from opentelemetry.instrumentation.genai.openai import OpenAIInstrumentor
    return OpenAIInstrumentor()


def drive(base_url):
    import openai
    client = openai.OpenAI(base_url=f"{base_url}/v1", api_key="sk-fixture-dummy", max_retries=0)
    r1 = client.responses.create(model=MODEL, instructions=SYSTEM,
                                 input=[{"role": "user", "content": ASK}], tools=TOOLS)
    call = next(o for o in r1.output if o.type == "function_call")
    r2 = client.responses.create(model=MODEL, instructions=SYSTEM, previous_response_id=r1.id,
                                 input=[{"type": "function_call_output", "call_id": call.call_id,
                                         "output": FILE_TEXT}], tools=TOOLS)
    client.responses.create(model=MODEL, instructions=SYSTEM, previous_response_id=r2.id,
                            input=[{"role": "user", "content": FOLLOW_UP}], tools=TOOLS)


def expected(requests):
    return {
        "scenario": NAME,
        "requests": len(requests),
        "generation_ids": ["resp_fixture_1", "resp_fixture_2", "resp_fixture_3"],
        "response_ids": ["resp_fixture_1", "resp_fixture_2", "resp_fixture_3"],
        "previous_response_ids": [r["body"].get("previous_response_id") for r in requests],
        "sessions": 1,
        # genai-openai 1.2b0 emits no gen_ai.request.previous_response.id, so
        # without it each request stands alone.
        "sessions_without_continuation": 3,
        "turn_roles": TURN_ROLES,
        "system_text": SYSTEM,
        "tool_calls": [{"id": "call_fixture_r1", "name": "read_file",
                        "input": {"path": "src/lib.rs"}, "result": FILE_TEXT}],
        "completion_texts": ["", ANSWER, SUMMARY],
        "extra_choices": [[], [], []],
        "usage_served": [{"input": i, "output": o, "cache_read": c, "reasoning": r}
                         for i, o, c, r in USAGE],
    }


SYNTHETIC_DIR = "semconv/openai-responses/span-continuation"
SYNTHETIC_NOTE = {
    "label": "SYNTHETIC",
    "derived_from": "semconv/openai-responses/span/traces.json",
    "added": ["gen_ai.request.previous_response.id"],
    "reason": ("opentelemetry-instrumentation-genai-openai 1.2b0 never sets "
               "gen_ai.request.previous_response.id. This copy of the real capture sets the "
               "spec-defined attribute to the previous_response_id the client really sent "
               "(recorded by the mock). Everything else is the real capture, byte for byte."),
    "open_point": "replace with a real capture once a pinned instrumentation emits the attribute",
}


def SYNTHETIC(doc, requests):
    """Return (dir, doc, note) for a COPY of the real capture with the
    continuation attribute set. `doc` is already a deep copy."""
    prev = {f"resp_fixture_{i + 1}": r["body"].get("previous_response_id")
            for i, r in enumerate(requests)}
    spans = [sp for rs in doc.get("resourceSpans", []) for ss in rs.get("scopeSpans", [])
             for sp in ss.get("spans", [])]
    added = 0
    for span in spans:
        attrs = span.setdefault("attributes", [])
        rid = next((kv["value"].get("stringValue") for kv in attrs
                    if kv["key"] == "gen_ai.response.id"), None)
        if prev.get(rid):
            attrs.append({"key": "gen_ai.request.previous_response.id",
                          "value": {"stringValue": prev[rid]}})
            added += 1
    wanted = sum(1 for p in prev.values() if p)
    if added != wanted:
        raise SystemExit(f"capture {NAME}: SYNTHETIC set the continuation attribute on "
                         f"{added} spans, expected {wanted} (no gen_ai.response.id?)")
    return SYNTHETIC_DIR, doc, SYNTHETIC_NOTE
