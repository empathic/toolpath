"""OpenAI Chat Completions via openinference-instrumentation-openai (flattened
llm.* span attributes), inside an OpenInference session."""
from mock_openai import serve as MOCK  # noqa: F401
from scenarios import openai_chat as chat
from scenarios.common import SUMMARY, SYSTEM, TURN_ROLES

NAME = "openinference-openai-chat"
FIXTURE_DIR = "openinference/openai-chat"
DESCRIPTION = __doc__
ENV = {
    "OPENINFERENCE_HIDE_INPUTS": "false",
    "OPENINFERENCE_HIDE_OUTPUTS": "false",
    "OPENINFERENCE_HIDE_INPUT_MESSAGES": "false",
    "OPENINFERENCE_HIDE_OUTPUT_MESSAGES": "false",
}
CONTENT_KEYS = ("llm.input_messages.0.message.role", "llm.output_messages.0.message.role")
SESSION = "fixture-oi-session"
SCRIPT = {"chat": [
    chat.SCRIPT["chat"][0],
    chat.SCRIPT["chat"][1],
    chat.completion(2, [chat.choice(0, {"role": "assistant", "content": SUMMARY}, "stop")]),
]}


def is_llm_span(attrs):
    return attrs.get("openinference.span.kind") == "LLM"


def instrumentor():
    from openinference.instrumentation.openai import OpenAIInstrumentor
    return OpenAIInstrumentor()


def drive(base_url):
    import openai
    from openinference.instrumentation import using_session
    client = openai.OpenAI(base_url=f"{base_url}/v1", api_key="sk-fixture-dummy", max_retries=0)
    with using_session(SESSION):
        chat.conversation(client, n_last=1)


def expected(requests):
    base = chat.expected(requests)
    base.update({
        "scenario": NAME,
        "generation_ids": None,  # OpenInference carries no response id: span-<spanId>
        "session_id": SESSION,
        "turn_roles": TURN_ROLES,
        "system_text": SYSTEM,
        "extra_choices": [[], [], []],
    })
    return base
