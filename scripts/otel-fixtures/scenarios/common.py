"""The conversation every capture scenario runs, and shared settings.

user asks -> model calls read_file -> tool result -> answer -> follow-up -> answer.
"""
import json

SYSTEM = "You are a coding assistant."
ASK = "What does src/lib.rs export?"
FILE_TEXT = "pub fn add(a: i32, b: i32) -> i32 { a + b }"
ANSWER = "It exports one function, add."
FOLLOW_UP = "Summarize that in four words."
SUMMARY = "One public add function."
ALT_SUMMARY = "Exports a single adder."
READ_FILE_SCHEMA = {
    "type": "object",
    "properties": {"path": {"type": "string"}},
    "required": ["path"],
}
TURN_ROLES = ["system", "user", "assistant", "assistant", "user", "assistant"]

# util-genai based packages (openai-v2 2.4b0, genai-openai, genai-anthropic,
# google-genai 1.2b0): content on span attributes only, no details event.
UTIL_GENAI_SPAN_ONLY = {
    "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT": "SPAN_ONLY",
    "OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT": "false",
    "OTEL_SEMCONV_STABILITY_OPT_IN": "gen_ai_latest_experimental",
}
# The same packages in event mode (capture.py --mode event): content only on
# the gen_ai.client.inference.operation.details log record, none on spans.
# util-genai 1.1b0 (openai-v2's venv) and 1.2b0 both accept EVENT_ONLY.
UTIL_GENAI_EVENT_ONLY = {
    "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT": "EVENT_ONLY",
    "OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT": "true",
    "OTEL_SEMCONV_STABILITY_OPT_IN": "gen_ai_latest_experimental",
}
SEMCONV_CONTENT_KEYS = ("gen_ai.input.messages", "gen_ai.output.messages")


def is_semconv_llm_span(attrs):
    return attrs.get("gen_ai.operation.name") in ("chat", "text_completion", "generate_content")


def canonical(value):
    """The Rust crate's canonical_json: sorted keys, compact."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
