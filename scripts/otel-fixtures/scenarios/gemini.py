"""Gemini generate_content via opentelemetry-instrumentation-google-genai: two
parallel function calls without ids, their responses, an answer, a follow-up.
The response usage carries a thoughts count and no thought text."""
from mock_gemini import serve as MOCK  # noqa: F401
from scenarios.common import (FOLLOW_UP, SEMCONV_CONTENT_KEYS, SUMMARY, SYSTEM, TURN_ROLES,
                              UTIL_GENAI_EVENT_ONLY, UTIL_GENAI_SPAN_ONLY, canonical,
                              is_semconv_llm_span)

NAME = "gemini"
FIXTURE_DIR = "semconv/gemini/span"
DESCRIPTION = __doc__
ENV = dict(UTIL_GENAI_SPAN_ONLY)
EVENT_ENV = dict(UTIL_GENAI_EVENT_ONLY)
CONTENT_KEYS = SEMCONV_CONTENT_KEYS
is_llm_span = is_semconv_llm_span

MODEL = "gemini-2.5-flash"
ASK_AB = "What do src/a.rs and src/b.rs export?"
A_TEXT = "pub fn a() {}"
B_TEXT = "pub fn b() {}"
ANSWER_AB = "They export a and b."
USAGE = [(30, 12, 4), (55, 9, 0), (70, 6, 0)]  # prompt, candidates, thoughts


def resp(i, parts):
    prompt, cand, thoughts = USAGE[i]
    usage = {"promptTokenCount": prompt, "candidatesTokenCount": cand,
             "totalTokenCount": prompt + cand + thoughts}
    if thoughts:
        usage["thoughtsTokenCount"] = thoughts
    return {"candidates": [{"content": {"role": "model", "parts": parts},
                            "finishReason": "STOP", "index": 0}],
            "usageMetadata": usage, "modelVersion": MODEL, "responseId": f"gemini-fixture-{i + 1}"}


SCRIPT = {"generate": [
    resp(0, [{"functionCall": {"name": "read_file", "args": {"path": "src/a.rs"}}},
             {"functionCall": {"name": "read_file", "args": {"path": "src/b.rs"}}}]),
    resp(1, [{"text": ANSWER_AB}]),
    resp(2, [{"text": SUMMARY}]),
]}


def instrumentor():
    from opentelemetry.instrumentation.google_genai import GoogleGenAiSdkInstrumentor
    return GoogleGenAiSdkInstrumentor()


def drive(base_url):
    from google import genai
    from google.genai import types
    client = genai.Client(api_key="fixture-dummy-key",
                          http_options=types.HttpOptions(base_url=base_url, api_version="v1beta"))
    decl = types.FunctionDeclaration(
        name="read_file", description="Read a file",
        parameters=types.Schema(type="OBJECT", properties={"path": types.Schema(type="STRING")},
                                required=["path"]))
    config = types.GenerateContentConfig(
        system_instruction=SYSTEM, tools=[types.Tool(function_declarations=[decl])],
        automatic_function_calling=types.AutomaticFunctionCallingConfig(disable=True))
    contents = [types.Content(role="user", parts=[types.Part(text=ASK_AB)])]
    r1 = client.models.generate_content(model=MODEL, contents=contents, config=config)
    contents += [r1.candidates[0].content, types.Content(role="user", parts=[
        types.Part(function_response=types.FunctionResponse(name="read_file", response={"output": A_TEXT})),
        types.Part(function_response=types.FunctionResponse(name="read_file", response={"output": B_TEXT})),
    ])]
    r2 = client.models.generate_content(model=MODEL, contents=contents, config=config)
    contents += [r2.candidates[0].content,
                 types.Content(role="user", parts=[types.Part(text=FOLLOW_UP)])]
    client.models.generate_content(model=MODEL, contents=contents, config=config)


def expected(requests):
    return {
        "scenario": NAME,
        "requests": len(requests),
        "generation_ids": None,  # gen_ai.response.id or span-<spanId>: instrumentation's choice
        "response_ids": ["gemini-fixture-1", "gemini-fixture-2", "gemini-fixture-3"],
        "sessions": 1,
        "turn_roles": TURN_ROLES,
        "system_text": SYSTEM,
        # google-genai 1.2b0 synthesizes "<name>_<part index>" for id-less calls
        # and responses (message.py `_to_part`); the mock sent no ids.
        "tool_calls": [
            {"id": "read_file_0", "name": "read_file", "input": {"path": "src/a.rs"},
             "result": canonical({"output": A_TEXT})},
            {"id": "read_file_1", "name": "read_file", "input": {"path": "src/b.rs"},
             "result": canonical({"output": B_TEXT})},
        ],
        "ids_sent_by_mock": False,
        "completion_texts": ["", ANSWER_AB, SUMMARY],
        "extra_choices": [[], [], []],
        "usage_served": [{"prompt": p, "candidates": c, "thoughts": t} for p, c, t in USAGE],
    }
