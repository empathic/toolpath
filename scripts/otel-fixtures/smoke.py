#!/usr/bin/env python3
"""Import smoke check: every instrumentation a venv serves imports, instruments and uninstruments,
and the OTLP/HTTP exporters and log SDK the binary captures use import.

Usage: python smoke.py main|openai-v2

pip accepts pairs that cannot import (openai-v2 2.4b0 declares util-genai>=0.4b0.dev
but needs a module util-genai 1.2b0 removed), so resolution alone proves nothing.
Run by `./capture.sh selftest` in each venv against the lock it was installed from.
"""
import importlib
import sys

from opentelemetry.sdk.trace import TracerProvider

INSTRUMENTORS = {
    # requirements.txt -> .venv
    "main": [
        ("opentelemetry.instrumentation.genai.openai", "OpenAIInstrumentor"),
        ("opentelemetry.instrumentation.genai.anthropic", "AnthropicInstrumentor"),
        ("opentelemetry.instrumentation.google_genai", "GoogleGenAiSdkInstrumentor"),
        ("openinference.instrumentation.openai", "OpenAIInstrumentor"),
    ],
    # requirements-openai-v2.txt -> .venv-openai-v2
    "openai-v2": [
        ("opentelemetry.instrumentation.openai_v2", "OpenAIInstrumentor"),
    ],
}

EXPORT_PATH = [
    ("opentelemetry.exporter.otlp.proto.http", "Compression"),
    ("opentelemetry.exporter.otlp.proto.http.trace_exporter", "OTLPSpanExporter"),
    ("opentelemetry.exporter.otlp.proto.http._log_exporter", "OTLPLogExporter"),
    ("opentelemetry.sdk._logs", "LoggerProvider"),
    ("opentelemetry.sdk._logs.export", "BatchLogRecordProcessor"),
    ("opentelemetry.proto.collector.logs.v1.logs_service_pb2", "ExportLogsServiceRequest"),
]


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in INSTRUMENTORS:
        sys.exit(f"usage: smoke.py {'|'.join(INSTRUMENTORS)}")
    # capture.py's binary path (both venvs): exporters, log SDK, the sink's converter.
    for module, name in EXPORT_PATH:
        getattr(importlib.import_module(module), name)
    print(f"smoke {sys.argv[1]}: OTLP/HTTP exporters and log SDK ok")
    provider = TracerProvider()
    for module, cls in INSTRUMENTORS[sys.argv[1]]:
        instrumentor = getattr(importlib.import_module(module), cls)()
        instrumentor.instrument(tracer_provider=provider)
        instrumentor.uninstrument()
        print(f"smoke {sys.argv[1]}: {module}.{cls} ok")


if __name__ == "__main__":
    main()
