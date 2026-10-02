"""Scripted replay server for the capture mocks: loopback only, standard library only.

Each route serves its scripted JSON responses in order and records every request
body, so a scenario's expected.json can be written from what the client really sent.
"""
import json
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class ReplayServer:
    def __init__(self, port, routes):
        self.routes = [(re.compile(pattern), list(responses)) for pattern, responses in routes]
        self.requests = []
        self.base_url = f"http://127.0.0.1:{port}"
        server = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802 (http.server naming)
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length) if length else b"{}"
                path = self.path.split("?", 1)[0]
                for rx, queue in server.routes:
                    if rx.fullmatch(path):
                        if not queue:
                            return self._send(500, {"error": f"script exhausted for {path}"})
                        server.requests.append({"path": path, "body": json.loads(raw)})
                        return self._send(200, queue.pop(0))
                return self._send(404, {"error": f"no route for {path}"})

            def _send(self, code, obj):
                data = json.dumps(obj).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        self.httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)

    def __enter__(self):
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.httpd.shutdown()
        self.httpd.server_close()

    def unserved(self):
        return {rx.pattern: len(queue) for rx, queue in self.routes if queue}
