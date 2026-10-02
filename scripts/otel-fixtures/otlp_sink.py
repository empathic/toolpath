"""A local OTLP/HTTP receiver that keeps each request body verbatim.

Bound to 127.0.0.1 on an ephemeral port. The capture points the OTLP/HTTP
protobuf exporters at it (no compression) and writes what arrived as
traces.binpb / logs.binpb. Maintainer-only; CI never runs it.
"""
import http.server
import threading

SIGNALS = {"/v1/traces": "traces", "/v1/logs": "logs"}


class Sink:
    def __init__(self):
        self.bodies = {signal: [] for signal in SIGNALS.values()}
        sink = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802 (http.server naming)
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length)
                signal = SIGNALS.get(self.path)
                if signal is None or self.headers.get("Content-Encoding"):
                    self.send_response(415 if signal else 404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                sink.bodies[signal].append(body)
                self.send_response(200)
                self.send_header("Content-Type", "application/x-protobuf")
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def endpoint(self):
        host, port = self.server.server_address[:2]
        return f"http://{host}:{port}"

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()

    def one(self, signal):
        """The single request body for `signal` ("traces" or "logs")."""
        bodies = self.bodies[signal]
        if len(bodies) != 1:
            raise SystemExit(
                f"expected exactly one {signal} export request, got {len(bodies)}; "
                "flush once at the end with large batch limits"
            )
        return bodies[0]
