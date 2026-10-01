"""Anthropic Messages mock: /v1/messages on 127.0.0.1:18432."""
from replay import ReplayServer

PORT = 18432


def serve(script):
    return ReplayServer(PORT, [(r"/v1/messages", script.get("messages", []))])
