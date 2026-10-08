"""OpenAI mock: /v1/chat/completions and /v1/responses on 127.0.0.1:18431."""
from replay import ReplayServer

PORT = 18431


def serve(script):
    return ReplayServer(
        PORT,
        [
            (r"/v1/chat/completions", script.get("chat", [])),
            (r"/v1/responses", script.get("responses", [])),
        ],
    )
