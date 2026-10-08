#!/usr/bin/env python3
"""Run one capture scenario offline and write its fixture directory.

Usage: python capture.py <scenario> [--mode span|event] [--out DIR]

The scenario's instrumentation exports into an in-memory exporter; the spans
are written as OTLP/JSON (traces.json) next to manifest.json (versions, env,
observed scopes) and expected.json (the scenario's oracle, built from what the
mock served and what the client sent). A capture with no message content, or
one containing a host path, host name or key, fails instead of being written.

Semconv scenarios (FIXTURE_DIR ending in /span) also export through the
OTLP/HTTP protobuf exporters into a local sink (otlp_sink.py) and keep the
request bodies: span mode adds traces.binpb; event mode (content on log
records, written to .../event/) writes traces.binpb and logs.binpb, and
traces.json and logs.json converted from those bodies by pb_to_json.py.
"""
import argparse
import contextlib
import copy
import importlib
import json
import os
import platform
import socket
import sys
from importlib import metadata
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent.parent / "test-fixtures" / "otel"
COMMON_ENV = {
    "OTEL_RESOURCE_ATTRIBUTES": "service.name=otel-fixture",
    "OTEL_EXPERIMENTAL_RESOURCE_DETECTORS": "",
    "OTEL_TRACES_EXPORTER": "none",
    "OTEL_METRICS_EXPORTER": "none",
    "OTEL_LOGS_EXPORTER": "none",
}
# The lock this interpreter's venv was installed from (capture.sh picks the venv per scenario).
REQUIREMENTS = HERE / ("requirements-openai-v2.in" if Path(sys.prefix).name == ".venv-openai-v2"
                       else "requirements.in")
PACKAGES = [line.split("==")[0] for line in REQUIREMENTS.read_text().splitlines()
            if line and not line.startswith("#")]
DUMMY_KEYS = ("sk-fixture-dummy", "sk-ant-fixture-dummy", "fixture-dummy-key")


def guard_network():
    """Refuse every connection that is not to loopback: no real endpoint is ever reached."""
    real_connect = socket.socket.connect

    def connect(sock, address):
        host = address[0] if isinstance(address, tuple) else address
        if host not in ("127.0.0.1", "::1", "localhost"):
            raise RuntimeError(f"capture tried to reach {address!r}; only 127.0.0.1 is allowed")
        return real_connect(sock, address)

    socket.socket.connect = connect


def attr_value(any_value):
    for key in ("stringValue", "boolValue", "doubleValue"):
        if key in any_value:
            return any_value[key]
    if "intValue" in any_value:
        return int(any_value["intValue"])
    return any_value


def spans_of(doc):
    for rs in doc.get("resourceSpans", []):
        for ss in rs.get("scopeSpans", []):
            for span in ss.get("spans", []):
                yield ss.get("scope", {}), span


def check_content(doc, scenario):
    claimed = 0
    for _, span in spans_of(doc):
        attrs = {kv["key"]: attr_value(kv.get("value", {})) for kv in span.get("attributes", [])}
        if not scenario.is_llm_span(attrs):
            continue
        claimed += 1
        missing = [k for k in scenario.CONTENT_KEYS if k not in attrs]
        if missing:
            sys.exit(f"capture {scenario.NAME}: span {span.get('name')!r} has no {missing}; "
                     f"the content-capture settings {scenario.ENV} did not take effect")
    if claimed == 0:
        sys.exit(f"capture {scenario.NAME}: no LLM span was exported")


def check_no_span_content(doc, scenario):
    """Event mode: content travels on log records only."""
    for _, span in spans_of(doc):
        keys = {kv["key"] for kv in span.get("attributes", [])}
        if keys & set(scenario.CONTENT_KEYS):
            sys.exit(f"capture {scenario.NAME}: event mode, but span {span.get('name')!r} "
                     f"carries {sorted(keys & set(scenario.CONTENT_KEYS))}")


DETAILS = "gen_ai.client.inference.operation.details"
LEGACY = {"gen_ai.system.message", "gen_ai.user.message", "gen_ai.assistant.message",
          "gen_ai.tool.message", "gen_ai.choice"}
CONTENT_KEYS = {"gen_ai.input.messages", "gen_ai.output.messages", "gen_ai.system_instructions"}


def _records(logs_json):
    for rl in logs_json.get("resourceLogs", []):
        for sl in rl.get("scopeLogs", []):
            yield from sl.get("logRecords", [])


def _event_name(record):
    attr = next((kv["value"].get("stringValue") for kv in record.get("attributes", [])
                 if kv.get("key") == "event.name"), None)
    return record.get("eventName") or attr, bool(record.get("eventName")), attr is not None


def describe_logs(logs_json, scenario):
    """Manifest facts that settle the spec's open questions, and a check
    that content really travelled on log records."""
    field = attribute = False
    places, legacy, content = set(), set(), False
    for r in _records(logs_json):
        name, by_field, by_attr = _event_name(r)
        field |= by_field
        attribute |= by_attr
        if name in LEGACY:
            legacy.add(name)
            content |= bool(r.get("body"))
        if name == DETAILS:
            if any(kv.get("key") in CONTENT_KEYS for kv in r.get("attributes", [])):
                places.add("attributes")
                content = True
            body_keys = {kv.get("key") for kv in
                         (r.get("body", {}).get("kvlistValue", {}).get("values", []))}
            if body_keys & CONTENT_KEYS:
                places.add("body")
                content = True
    if not content:
        sys.exit(f"capture {scenario.NAME}: event capture carries no message content on any "
                 f"log record; the content-capture settings {scenario.EVENT_ENV} did not "
                 "take effect")
    if not (field or attribute):
        sys.exit(f"capture {scenario.NAME}: no log record names its event")
    return {
        "event_name_source": "both" if field and attribute else
                             "eventName" if field else "event.name attribute",
        "details_content_location": "both" if len(places) == 2 else
                                    (places.pop() if places else "none"),
        "legacy_events": sorted(legacy),
    }


def add_otlp_exporters(tracer_provider, resource, endpoint, mode):
    """Send spans (and, in event mode, logs) to the sink as OTLP/HTTP
    protobuf, uncompressed, in one request per signal at the final flush.
    Returns the LoggerProvider in event mode, else None."""
    from opentelemetry.exporter.otlp.proto.http import Compression
    from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
    from opentelemetry.sdk.trace.export import BatchSpanProcessor

    # The sink is on 127.0.0.1: never route the export through a proxy. Not
    # part of the recorded env (it changes nothing the instrumentation sees).
    os.environ["NO_PROXY"] = os.environ["no_proxy"] = "127.0.0.1,localhost"
    batch = dict(max_queue_size=100_000, max_export_batch_size=100_000,
                 schedule_delay_millis=3_600_000)
    tracer_provider.add_span_processor(BatchSpanProcessor(
        OTLPSpanExporter(endpoint=f"{endpoint}/v1/traces", compression=Compression.NoCompression),
        **batch))
    if mode != "event":
        return None
    from opentelemetry import _logs
    from opentelemetry.exporter.otlp.proto.http._log_exporter import OTLPLogExporter
    from opentelemetry.sdk._logs import LoggerProvider
    from opentelemetry.sdk._logs.export import BatchLogRecordProcessor

    logger_provider = LoggerProvider(resource=resource)
    logger_provider.add_log_record_processor(BatchLogRecordProcessor(
        OTLPLogExporter(endpoint=f"{endpoint}/v1/logs", compression=Compression.NoCompression),
        **batch))
    _logs.set_logger_provider(logger_provider)
    return logger_provider


def exporter_manifest(verbatim):
    return {
        "package": "opentelemetry-exporter-otlp-proto-http",
        "version": metadata.version("opentelemetry-exporter-otlp-proto-http"),
        "binpb": ("request bodies as exported" if verbatim else
                  "request bodies as exported, times rewritten to the JSON's ranks"),
    }


def check_hygiene(text, scenario):
    forbidden = ["/Users/", str(Path.home()), *DUMMY_KEYS]
    host = socket.gethostname()
    if host:
        forbidden.append(host)
    for needle in forbidden:
        if needle and needle in text:
            sys.exit(f"capture {scenario.NAME}: output contains {needle!r}")
    start = 0
    while (i := text.find("/home/", start)) >= 0:
        if not text.startswith("/home/user", i):
            sys.exit(f"capture {scenario.NAME}: output contains a /home/ path")
        start = i + 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario")
    ap.add_argument("--mode", choices=["span", "event"], default="span")
    ap.add_argument("--out", type=Path, default=FIXTURES)
    args = ap.parse_args()

    sys.path.insert(0, str(HERE))
    scenario = importlib.import_module(f"scenarios.{args.scenario}")
    # Semconv scenarios keep the exporter's request bodies and have an event mode.
    binary = scenario.FIXTURE_DIR.endswith("/span")
    event = args.mode == "event"
    if event and not binary:
        sys.exit(f"capture {scenario.NAME}: no event mode (FIXTURE_DIR "
                 f"{scenario.FIXTURE_DIR!r} does not end in /span)")
    env = {**COMMON_ENV, **(scenario.EVENT_ENV if event else scenario.ENV)}
    os.environ.update(env)
    guard_network()

    from opentelemetry.sdk.resources import Resource
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import SimpleSpanProcessor
    from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

    from otlp_json import FixtureIds, dumps, install_fixture_clock, normalize_times, to_otlp_json
    from otlp_sink import Sink
    from pb_to_json import body_to_otlp_json, rerank_body

    install_fixture_clock(logs=event)
    exporter = InMemorySpanExporter()
    resource = Resource({"service.name": "otel-fixture"})
    provider = TracerProvider(resource=resource, id_generator=FixtureIds(scenario.NAME))
    provider.add_span_processor(SimpleSpanProcessor(exporter))

    bodies = {}
    with Sink() if binary else contextlib.nullcontext() as sink:
        logger_provider = (add_otlp_exporters(provider, resource, sink.endpoint, args.mode)
                           if binary else None)
        instrumentor = scenario.instrumentor()
        with scenario.MOCK(scenario.SCRIPT) as server:
            kwargs = {"tracer_provider": provider}
            if logger_provider is not None:
                kwargs["logger_provider"] = logger_provider
            instrumentor.instrument(**kwargs)
            try:
                scenario.drive(server.base_url)
            finally:
                instrumentor.uninstrument()
            if server.unserved():
                sys.exit(f"capture {scenario.NAME}: scripted responses left unserved: "
                         f"{server.unserved()}")
            requests = list(server.requests)
        spans = exporter.get_finished_spans()
        if binary:
            provider.force_flush()
            if logger_provider is not None:
                logger_provider.force_flush()
            bodies["traces"] = sink.one("traces")
            if event:
                bodies["logs"] = sink.one("logs")
            # Nothing is left queued, so the SDK's exit-time shutdown sends nothing.

    # PR 2's writer: OTLP/JSON from the SDK's span objects.
    doc = to_otlp_json(spans)
    logs = body_to_otlp_json(bodies["logs"], "logs") if event else None
    # Span and log times are ranked together, so they stay comparable.
    rank = normalize_times([doc, logs] if event else doc)
    if event:
        check_no_span_content(doc, scenario)
        facts = describe_logs(logs, scenario)
    else:
        check_content(doc, scenario)

    # The request bodies, times rewritten only if the fixture clock's times
    # were not already the ranks; each must say exactly what its JSON says.
    binpb = {signal: rerank_body(body, signal, rank) for signal, body in bodies.items()}
    for signal, body in binpb.items():
        if body_to_otlp_json(body, signal) != (doc if signal == "traces" else logs):
            sys.exit(f"capture {scenario.NAME}: the exported {signal} request body disagrees "
                     f"with the {'SDK spans' if signal == 'traces' else 'logs JSON'}")
        check_hygiene(body.decode("utf-8", "replace"), scenario)

    # Event mode writes the JSON converted from the bodies (equal to the SDK
    # doc, checked above), so both files trace back to what was exported.
    traces = dumps(body_to_otlp_json(binpb["traces"], "traces") if event else doc)
    check_hygiene(traces, scenario)
    logs_text = dumps(logs) if event else None
    if event:
        check_hygiene(logs_text, scenario)

    scopes = sorted({(s.get("name", ""), s.get("version", "")) for s, _ in spans_of(doc)})
    manifest = {
        "scenario": scenario.NAME,
        "description": scenario.DESCRIPTION,
        "python": ".".join(platform.python_version_tuple()[:2]),
        "packages": {name: metadata.version(name) for name in PACKAGES},
        "env": env,
        "scopes": [{"name": n, "version": v} for n, v in scopes],
        "mock": {"module": scenario.MOCK.__module__, "requests": len(requests)},
        "mode": args.mode,
    }
    if event:
        manifest.update(facts)
    if binary:
        verbatim = all(binpb[s] == bodies[s] for s in bodies)
        manifest["otlp_exporter"] = exporter_manifest(verbatim)
    expected = scenario.expected(requests)

    fixture_dir = (scenario.FIXTURE_DIR.removesuffix("/span") + "/event" if event
                   else scenario.FIXTURE_DIR)
    out = args.out / fixture_dir
    out.mkdir(parents=True, exist_ok=True)
    (out / "traces.json").write_text(traces)
    for signal, body in binpb.items():
        (out / f"{signal}.binpb").write_bytes(body)
    if event:
        (out / "logs.json").write_text(logs_text)
    (out / "manifest.json").write_text(dumps(manifest))
    (out / "expected.json").write_text(dumps(expected))
    print(f"wrote {out}")
    if event:
        print(f"  {json.dumps(facts)}")
        return

    # A scenario may define SYNTHETIC, which
    # returns a COPY of the normalized real capture carrying spec-defined
    # attributes the pinned instrumentation does not emit. The copy lands in
    # its own directory; its manifest says SYNTHETIC. The real capture above
    # is never edited.
    synthetic = getattr(scenario, "SYNTHETIC", None)
    if synthetic is not None:
        sub, sdoc, note = synthetic(copy.deepcopy(doc), requests)
        straces = dumps(sdoc)
        check_hygiene(straces, scenario)
        sout = args.out / sub
        sout.mkdir(parents=True, exist_ok=True)
        (sout / "traces.json").write_text(straces)
        # The copy has no exporter body of its own.
        smanifest = {k: v for k, v in manifest.items() if k != "otlp_exporter"}
        (sout / "manifest.json").write_text(dumps({**smanifest, "synthetic": note}))
        (sout / "expected.json").write_text(dumps(expected))
        print(f"wrote {sout} (SYNTHETIC)")


if __name__ == "__main__":
    main()
