import json
import re
import unittest

from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

from otlp_json import BASE_NS, STEP_NS, FixtureIds, dumps, normalize_times, to_otlp_json


def capture(seed):
    exporter = InMemorySpanExporter()
    provider = TracerProvider(resource=Resource({"service.name": "otel-fixture"}),
                              id_generator=FixtureIds(seed))
    provider.add_span_processor(SimpleSpanProcessor(exporter))
    tracer = provider.get_tracer("fixture.scope", "9.9")
    with tracer.start_as_current_span("parent", attributes={"n": 7, "s": "x", "l": ["a", "b"]}):
        with tracer.start_as_current_span("child") as child:
            child.add_event("evt", {"k": "v"})
    doc = to_otlp_json(exporter.get_finished_spans())
    normalize_times(doc)
    return doc


def spans(doc):
    return [s for rs in doc["resourceSpans"] for ss in rs["scopeSpans"] for s in ss["spans"]]


class OtlpJsonTest(unittest.TestCase):
    def test_ids_are_hex_and_parent_links_hold(self):
        by_name = {s["name"]: s for s in spans(capture("t"))}
        for s in by_name.values():
            self.assertRegex(s["traceId"], r"^[0-9a-f]{32}$")
            self.assertRegex(s["spanId"], r"^[0-9a-f]{16}$")
        self.assertEqual(by_name["child"]["parentSpanId"], by_name["parent"]["spanId"])
        self.assertEqual(by_name["child"]["traceId"], by_name["parent"]["traceId"])

    def test_otlp_json_scalars(self):
        doc = capture("t")
        parent = next(s for s in spans(doc) if s["name"] == "parent")
        attrs = {kv["key"]: kv["value"] for kv in parent["attributes"]}
        self.assertEqual(attrs["n"], {"intValue": "7"})
        self.assertEqual(attrs["s"], {"stringValue": "x"})
        self.assertIn("arrayValue", attrs["l"])
        self.assertIsInstance(parent["kind"], int)
        self.assertEqual(doc["resourceSpans"][0]["resource"]["attributes"],
                         [{"key": "service.name", "value": {"stringValue": "otel-fixture"}}])

    def test_times_are_ranked_and_ordered(self):
        doc = capture("t")
        text = json.dumps(doc)
        times = sorted({int(t) for t in re.findall(r'UnixNano": "(\d+)"', text)})
        self.assertEqual(times, [BASE_NS + i * STEP_NS for i in range(len(times))])
        parent = next(s for s in spans(doc) if s["name"] == "parent")
        child = next(s for s in spans(doc) if s["name"] == "child")
        self.assertLessEqual(int(parent["startTimeUnixNano"]), int(child["startTimeUnixNano"]))
        self.assertLessEqual(int(child["endTimeUnixNano"]), int(parent["endTimeUnixNano"]))

    def test_capture_is_deterministic(self):
        self.assertEqual(dumps(capture("same")), dumps(capture("same")))
        self.assertNotEqual(dumps(capture("a")), dumps(capture("b")))


if __name__ == "__main__":
    unittest.main()
