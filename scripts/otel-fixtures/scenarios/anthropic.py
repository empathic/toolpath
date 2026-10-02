"""Anthropic Messages via the official opentelemetry-instrumentation-genai-anthropic:
system parameter, extended thinking with a signature, cache read/write usage."""
from mock_anthropic import serve as MOCK  # noqa: F401
from scenarios.common import (ANSWER, ASK, FILE_TEXT, FOLLOW_UP, READ_FILE_SCHEMA,
                              SEMCONV_CONTENT_KEYS, SUMMARY, SYSTEM, TURN_ROLES,
                              UTIL_GENAI_EVENT_ONLY, UTIL_GENAI_SPAN_ONLY, is_semconv_llm_span)

NAME = "anthropic"
FIXTURE_DIR = "semconv/anthropic/span"
DESCRIPTION = __doc__
ENV = dict(UTIL_GENAI_SPAN_ONLY)
EVENT_ENV = dict(UTIL_GENAI_EVENT_ONLY)
CONTENT_KEYS = SEMCONV_CONTENT_KEYS
is_llm_span = is_semconv_llm_span

MODEL = "claude-sonnet-4-5"
SERVED_MODEL = "claude-sonnet-4-5-20250929"
THINKING_TEXT = "I should read the file first."
SIGNATURE = "sig-fixture-1"
USAGE = [(11, 7, 5, 20), (13, 12, 0, 9), (9, 25, 0, 6)]  # input, cache_read, cache_write, output
TOOLS = [{"name": "read_file", "description": "Read a file", "input_schema": READ_FILE_SCHEMA}]


def message(i, content, stop):
    inp, read, write, out = USAGE[i]
    return {"id": f"msg_fixture_{i + 1}", "type": "message", "role": "assistant",
            "model": SERVED_MODEL, "content": content, "stop_reason": stop,
            "stop_sequence": None,
            "usage": {"input_tokens": inp, "cache_read_input_tokens": read,
                      "cache_creation_input_tokens": write, "output_tokens": out}}


SCRIPT = {"messages": [
    message(0, [{"type": "thinking", "thinking": THINKING_TEXT, "signature": SIGNATURE},
                {"type": "tool_use", "id": "toolu_fixture_1", "name": "read_file",
                 "input": {"path": "src/lib.rs"}}], "tool_use"),
    message(1, [{"type": "text", "text": ANSWER}], "end_turn"),
    message(2, [{"type": "text", "text": SUMMARY}], "end_turn"),
]}


def instrumentor():
    from opentelemetry.instrumentation.genai.anthropic import AnthropicInstrumentor
    return AnthropicInstrumentor()


def drive(base_url):
    import anthropic
    client = anthropic.Anthropic(base_url=base_url, api_key="sk-ant-fixture-dummy", max_retries=0)
    thinking = {"type": "enabled", "budget_tokens": 1024}
    msgs = [{"role": "user", "content": ASK}]
    r1 = client.messages.create(model=MODEL, max_tokens=2048, system=SYSTEM, messages=msgs,
                                tools=TOOLS, thinking=thinking)
    msgs += [{"role": "assistant", "content": [b.model_dump(exclude_none=True) for b in r1.content]},
             {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_fixture_1",
                                           "content": FILE_TEXT}]}]
    r2 = client.messages.create(model=MODEL, max_tokens=2048, system=SYSTEM, messages=msgs,
                                tools=TOOLS, thinking=thinking)
    msgs += [{"role": "assistant", "content": r2.content[0].text},
             {"role": "user", "content": FOLLOW_UP}]
    client.messages.create(model=MODEL, max_tokens=2048, system=SYSTEM, messages=msgs,
                           tools=TOOLS, thinking=thinking)


def expected(requests):
    return {
        "scenario": NAME,
        "requests": len(requests),
        "generation_ids": ["msg_fixture_1", "msg_fixture_2", "msg_fixture_3"],
        "response_ids": ["msg_fixture_1", "msg_fixture_2", "msg_fixture_3"],
        "sessions": 1,
        "turn_roles": TURN_ROLES,
        "system_text": SYSTEM,
        "tool_calls": [{"id": "toolu_fixture_1", "name": "read_file",
                        "input": {"path": "src/lib.rs"}, "result": FILE_TEXT}],
        "completion_texts": ["", ANSWER, SUMMARY],
        "extra_choices": [[], [], []],
        "thinking": [THINKING_TEXT, None, None],
        "signature_sent_in_history": any(
            SIGNATURE in str(r["body"].get("messages")) for r in requests),
        "usage_served": [{"input": i, "cache_read": r, "cache_write": w, "output": o}
                         for i, r, w, o in USAGE],
    }
