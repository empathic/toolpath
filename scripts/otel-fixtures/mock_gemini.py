"""Gemini mock: /v1beta/models/{model}:generateContent on 127.0.0.1:18433."""
from replay import ReplayServer

PORT = 18433


def serve(script):
    return ReplayServer(
        PORT, [(r"/v1beta/models/[^/:]+:generateContent", script.get("generate", []))]
    )
