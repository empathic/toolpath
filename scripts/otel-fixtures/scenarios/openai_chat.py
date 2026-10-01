"""OpenAI Chat Completions via opentelemetry-instrumentation-openai-v2: a tool call,
its result, an answer, and a follow-up requested with n=2."""
from mock_openai import serve as MOCK  # noqa: F401 (scenario contract)
from scenarios.common import (ALT_SUMMARY, ANSWER, ASK, FILE_TEXT, FOLLOW_UP, READ_FILE_SCHEMA,
                              SEMCONV_CONTENT_KEYS, SUMMARY, SYSTEM, TURN_ROLES,
                              UTIL_GENAI_EVENT_ONLY, UTIL_GENAI_SPAN_ONLY, is_semconv_llm_span)

NAME = "openai-chat"
FIXTURE_DIR = "semconv/openai-chat/span"
DESCRIPTION = __doc__
ENV = dict(UTIL_GENAI_SPAN_ONLY)
EVENT_ENV = dict(UTIL_GENAI_EVENT_ONLY)
CONTENT_KEYS = SEMCONV_CONTENT_KEYS
is_llm_span = is_semconv_llm_span

MODEL = "gpt-4o-mini"
SERVED_MODEL = "gpt-4o-mini-2024-07-18"
TOOLS = [{"type": "function", "function": {"name": "read_file", "description": "Read a file",
                                           "parameters": READ_FILE_SCHEMA}}]
USAGE = [(42, 9, 0), (70, 8, 32), (90, 12, 0)]  # prompt, completion, cached
CALL = {"id": "call_fixture_1", "type": "function",
        "function": {"name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}"}}


def completion(i, choices):
    prompt, out, cached = USAGE[i]
    return {"id": f"chatcmpl-fixture-{i + 1}", "object": "chat.completion",
            "created": 1800000000 + i, "model": SERVED_MODEL, "choices": choices,
            "usage": {"prompt_tokens": prompt, "completion_tokens": out,
                      "total_tokens": prompt + out,
                      "prompt_tokens_details": {"cached_tokens": cached}}}


def choice(index, message, finish):
    return {"index": index, "message": message, "finish_reason": finish, "logprobs": None}


SCRIPT = {"chat": [
    completion(0, [choice(0, {"role": "assistant", "content": None, "tool_calls": [CALL]}, "tool_calls")]),
    completion(1, [choice(0, {"role": "assistant", "content": ANSWER}, "stop")]),
    completion(2, [choice(0, {"role": "assistant", "content": SUMMARY}, "stop"),
                   choice(1, {"role": "assistant", "content": ALT_SUMMARY}, "stop")]),
]}


def instrumentor():
    from opentelemetry.instrumentation.openai_v2 import OpenAIInstrumentor
    return OpenAIInstrumentor()


def conversation(client, n_last):
    msgs = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": ASK}]
    r1 = client.chat.completions.create(model=MODEL, messages=msgs, tools=TOOLS)
    call = r1.choices[0].message.tool_calls[0]
    msgs += [
        {"role": "assistant", "content": None, "tool_calls": [
            {"id": call.id, "type": "function",
             "function": {"name": call.function.name, "arguments": call.function.arguments}}]},
        {"role": "tool", "tool_call_id": call.id, "content": FILE_TEXT},
    ]
    r2 = client.chat.completions.create(model=MODEL, messages=msgs, tools=TOOLS)
    msgs += [{"role": "assistant", "content": r2.choices[0].message.content},
             {"role": "user", "content": FOLLOW_UP}]
    extra = {"n": n_last} if n_last > 1 else {}
    client.chat.completions.create(model=MODEL, messages=msgs, tools=TOOLS, **extra)


def drive(base_url):
    import openai
    client = openai.OpenAI(base_url=f"{base_url}/v1", api_key="sk-fixture-dummy", max_retries=0)
    conversation(client, n_last=2)


def expected(requests):
    return {
        "scenario": NAME,
        "requests": len(requests),
        "generation_ids": ["chatcmpl-fixture-1", "chatcmpl-fixture-2", "chatcmpl-fixture-3"],
        "response_ids": ["chatcmpl-fixture-1", "chatcmpl-fixture-2", "chatcmpl-fixture-3"],
        "sessions": 1,
        "turn_roles": TURN_ROLES,
        "system_text": SYSTEM,
        "tool_calls": [{"id": "call_fixture_1", "name": "read_file",
                        "input": {"path": "src/lib.rs"}, "result": FILE_TEXT}],
        "completion_texts": ["", ANSWER, SUMMARY],
        "extra_choices": [[], [], [ALT_SUMMARY]],
        "usage_served": [{"input": p, "output": o, "cache_read": c} for p, o, c in USAGE],
        "n_requested": [r["body"].get("n", 1) for r in requests],
    }
