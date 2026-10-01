"""The binary capture path: the sink, the protobuf -> JSON converter, the
body rerank, the fixture clock and the manifest facts about log records."""
import http.client
import unittest

from opentelemetry.exporter.otlp.proto.common.trace_encoder import encode_spans
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

from capture import describe_logs
from otlp_json import BASE_NS, STEP_NS, FixtureIds, normalize_times, to_otlp_json
from otlp_sink import Sink
from pb_to_json import body_to_otlp_json, rerank_body


def finished_spans(seed="t"):
    exporter = InMemorySpanExporter()
    provider = TracerProvider(resource=Resource({"service.name": "otel-fixture"}),
                              id_generator=FixtureIds(seed))
    provider.add_span_processor(SimpleSpanProcessor(exporter))
    tracer = provider.get_tracer("fixture.scope", "9.9")
    with tracer.start_as_current_span("parent", attributes={"n": 7}):
        with tracer.start_as_current_span("child") as child:
            child.add_event("evt", {"k": "v"})
    return exporter.get_finished_spans()


def post(endpoint, path, body, headers=None):
    host, port = endpoint.removeprefix("http://").split(":")
    conn = http.client.HTTPConnection(host, int(port), timeout=5)
    conn.request("POST", path, body=body, headers=headers or {})
    status = conn.getresponse().status
    conn.close()
    return status


class SinkTest(unittest.TestCase):
    def test_keeps_one_body_per_signal_verbatim(self):
        with Sink() as sink:
            self.assertEqual(post(sink.endpoint, "/v1/traces", b"\x0a\x00"), 200)
            self.assertEqual(post(sink.endpoint, "/v1/logs", b"\x0a\x01\x02"), 200)
        self.assertEqual(sink.one("traces"), b"\x0a\x00")
        self.assertEqual(sink.one("logs"), b"\x0a\x01\x02")

    def test_two_requests_for_a_signal_stop_the_capture(self):
        with Sink() as sink:
            post(sink.endpoint, "/v1/traces", b"a")
            post(sink.endpoint, "/v1/traces", b"b")
        with self.assertRaises(SystemExit):
            sink.one("traces")

    def test_compressed_or_unknown_requests_are_refused(self):
        with Sink() as sink:
            self.assertEqual(
                post(sink.endpoint, "/v1/traces", b"x", {"Content-Encoding": "gzip"}), 415)
            self.assertEqual(post(sink.endpoint, "/v1/metrics", b"x"), 404)
        self.assertEqual(sink.bodies, {"traces": [], "logs": []})


class PbToJsonTest(unittest.TestCase):
    def test_body_converts_to_the_sdk_writers_json(self):
        spans = finished_spans()
        body = encode_spans(spans).SerializeToString()
        self.assertEqual(body_to_otlp_json(body, "traces"), to_otlp_json(spans))

    def test_identity_rank_leaves_the_body_untouched(self):
        body = encode_spans(finished_spans()).SerializeToString()
        doc = body_to_otlp_json(body, "traces")
        times = {int(s["startTimeUnixNano"]) for rs in doc["resourceSpans"]
                 for ss in rs["scopeSpans"] for s in ss["spans"]}
        self.assertIs(rerank_body(body, "traces", {t: t for t in times}), body)

    def test_rerank_rewrites_the_body_like_normalize_times_rewrites_the_json(self):
        body = encode_spans(finished_spans()).SerializeToString()
        doc = body_to_otlp_json(body, "traces")
        rank = normalize_times(doc)
        self.assertNotEqual(rank, {t: t for t in rank})  # real clock: not yet ranked
        self.assertEqual(body_to_otlp_json(rerank_body(body, "traces", rank), "traces"), doc)


class FixtureClockTest(unittest.TestCase):
    def test_ranked_times_need_no_rewrite(self):
        # Simulates the fixture clock: times already BASE_NS + i * STEP_NS.
        doc = {"resourceSpans": [{"scopeSpans": [{"spans": [
            {"startTimeUnixNano": str(BASE_NS), "endTimeUnixNano": str(BASE_NS + 2 * STEP_NS)},
            {"startTimeUnixNano": str(BASE_NS + STEP_NS),
             "endTimeUnixNano": str(BASE_NS + 3 * STEP_NS)}]}]}]}
        rank = normalize_times(doc)
        self.assertEqual(rank, {t: t for t in rank})

    def test_observed_time_is_ranked_with_the_spans(self):
        spans = {"resourceSpans": [{"scopeSpans": [{"spans": [
            {"startTimeUnixNano": "10", "endTimeUnixNano": "30"}]}]}]}
        logs = {"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {"observedTimeUnixNano": "20"}]}]}]}
        normalize_times([spans, logs])
        observed = logs["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]
        self.assertEqual(observed["observedTimeUnixNano"], str(BASE_NS + STEP_NS))


class Scenario:
    NAME = "unit"
    EVENT_ENV = {}


def logs_doc(*records):
    return {"resourceLogs": [{"scopeLogs": [{"logRecords": list(records)}]}]}


def details(event_name_field=True, attribute=False, in_attributes=True, in_body=False):
    content = {"key": "gen_ai.input.messages", "value": {"stringValue": "[]"}}
    record = {"attributes": [content] if in_attributes else []}
    if event_name_field:
        record["eventName"] = "gen_ai.client.inference.operation.details"
    if attribute:
        record["attributes"].append({"key": "event.name", "value": {
            "stringValue": "gen_ai.client.inference.operation.details"}})
    if in_body:
        record["body"] = {"kvlistValue": {"values": [content]}}
    return record


class DescribeLogsTest(unittest.TestCase):
    def test_event_name_field_and_content_in_attributes(self):
        self.assertEqual(describe_logs(logs_doc(details()), Scenario), {
            "event_name_source": "eventName",
            "details_content_location": "attributes",
            "legacy_events": [],
        })

    def test_attribute_named_event_with_content_in_both_places(self):
        facts = describe_logs(logs_doc(details(event_name_field=False, attribute=True,
                                               in_body=True)), Scenario)
        self.assertEqual(facts["event_name_source"], "event.name attribute")
        self.assertEqual(facts["details_content_location"], "both")

    def test_legacy_events_are_listed_sorted(self):
        user = {"eventName": "gen_ai.user.message", "body": {"stringValue": "hi"}}
        choice = {"eventName": "gen_ai.choice", "body": {"stringValue": "ok"}}
        facts = describe_logs(logs_doc(user, choice), Scenario)
        self.assertEqual(facts["legacy_events"], ["gen_ai.choice", "gen_ai.user.message"])
        self.assertEqual(facts["details_content_location"], "none")

    def test_no_content_on_any_record_stops_the_capture(self):
        with self.assertRaises(SystemExit):
            describe_logs(logs_doc(details(in_attributes=False)), Scenario)


if __name__ == "__main__":
    unittest.main()
