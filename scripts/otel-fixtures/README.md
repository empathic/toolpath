# OTel GenAI fixture capture

## What this is

A maintainer-only, fully offline harness that captures the OpenTelemetry GenAI
telemetry real instrumentation packages emit, by driving the official client
SDKs against local mock servers. It is never run by CI. The committed fixtures
under `test-fixtures/otel/semconv/` and `test-fixtures/otel/openinference/` are
the test inputs; this directory only regenerates and checks them.

## Requirements

- `python3.12`, or another interpreter named by `OTEL_FIXTURES_PYTHON`.
- Network access to PyPI for `pip install` only. Packages install into two
  git-ignored venvs, never globally, each with `--require-hashes`:
  `.venv` from `requirements.txt` and `.venv-openai-v2` from
  `requirements-openai-v2.txt` (see "Two locks" below).

## Commands

- `./capture.sh` — capture every scenario into `test-fixtures/otel/`: each
  scenario in span mode, then the four semconv scenarios again in event mode.
- `./capture.sh verify` — re-capture into a temporary directory and require
  output byte-identical to the committed fixtures.
- `./capture.sh selftest` — import smoke check of both venvs (`smoke.py`:
  every instrumentation a venv serves imports, instruments and
  uninstruments), then the harness unit tests (`tests/`) in each venv.
- `./lock.sh` — regenerate `requirements.txt` and `requirements-openai-v2.txt`
  (every transitive package pinned, with hashes) from their `.in` files.

`python capture.py <scenario> [--mode span|event] [--out DIR]` runs one
scenario (in the venv `capture.sh` picks for it) and writes
`<DIR or test-fixtures/otel>/<FIXTURE_DIR>/{traces,manifest,expected}.json`;
a semconv scenario's span mode adds `traces.binpb`. `--mode event` writes
`<FIXTURE_DIR with its final span replaced by event>/` with
`traces.json`, `logs.json`, `traces.binpb`, `logs.binpb`, `manifest.json` and
`expected.json`, and refuses a scenario whose `FIXTURE_DIR` does not end in
`/span` (`openinference_openai_chat`).

## Pinned packages

| Package | Pin |
|---|---|
| `opentelemetry-api` | 1.45.0 |
| `opentelemetry-sdk` | 1.45.0 |
| `opentelemetry-proto` | 1.45.0 |
| `opentelemetry-exporter-otlp-proto-common` | 1.45.0 |
| `opentelemetry-exporter-otlp-proto-http` | 1.45.0 (both locks) |
| `opentelemetry-instrumentation` | 0.66b0 |
| `opentelemetry-semantic-conventions` | 0.66b0 |
| `opentelemetry-util-genai` | 1.2b0 |
| `opentelemetry-instrumentation-openai-v2` | 2.4b0 |
| `opentelemetry-instrumentation-genai-openai` | 1.2b0 |
| `opentelemetry-instrumentation-genai-anthropic` | 1.2b0 |
| `opentelemetry-instrumentation-google-genai` | 1.2b0 |
| `openinference-instrumentation` | 0.1.66 |
| `openinference-instrumentation-openai` | 0.1.61 |
| `openinference-semantic-conventions` | 0.1.39 |
| `openai` | 3.20.0 |
| `anthropic` | 1.9.0 |
| `google-genai` | 2.25.0 |

`requirements-openai-v2.in` pins `opentelemetry-util-genai` 1.1b0,
`opentelemetry-instrumentation-openai-v2` 2.4b0 and `httpx` 0.28.1, with the
same OpenTelemetry core packages and `openai` as above.

### Two locks

`opentelemetry-instrumentation-openai-v2` 2.4b0 (the newest release) imports
`opentelemetry.util.genai.instruments`, which `opentelemetry-util-genai` 1.2b0
removed (it ships `_instruments.py`). openai-v2 declares
`opentelemetry-util-genai>=0.4b0.dev`, so pip happily installs the broken pair.
Meanwhile genai-openai, genai-anthropic and google-genai 1.2b0 all require
`opentelemetry-util-genai>=1.2b0`. No single util-genai version serves every
instrumentation, so `openai_chat` captures in its own venv (`.venv-openai-v2`,
util-genai 1.1b0) and every other scenario uses the main lock. `capture.sh`
picks the venv per scenario, and `capture.py` records the packages of the lock
its venv came from in `manifest.json`. The main lock still lists openai-v2
2.4b0, but nothing imports it there.

openai-v2 2.4b0 also imports `httpx` without declaring it. `openai` 3.x
depends on `httpx2` instead, so `httpx` is pinned explicitly in the openai-v2
lock (in the main lock, google-genai happens to pull it in).

Collapse back to one lock once an openai-v2 release works with
`opentelemetry-util-genai>=1.2b0`: pin it in `requirements.in`, delete
`requirements-openai-v2.*`, the `.venv-openai-v2` venv selection in
`capture.sh`, `capture.py`'s lock choice and the `openai-v2` entry in
`smoke.py` (moving its instrumentor to `main`), then run `./lock.sh` and
`./capture.sh verify`.

Scenario to instrumentation:

| Scenario | Instrumentation |
|---|---|
| `openai_chat` | `opentelemetry-instrumentation-openai-v2` (own venv, `.venv-openai-v2`) |
| `openai_responses` | `opentelemetry-instrumentation-genai-openai` (openai-v2 2.4b0 does not wrap `responses.create`) |
| `anthropic` | `opentelemetry-instrumentation-genai-anthropic` (the official package; `opentelemetry-instrumentation-anthropic` is OpenLLMetry's) |
| `gemini` | `opentelemetry-instrumentation-google-genai` |
| `openinference_openai_chat` | `openinference-instrumentation-openai` |

## Content capture

- util-genai-based packages (openai-v2, genai-openai, genai-anthropic,
  google-genai): `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY`
  and `OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT=false` (a scenario's `ENV`).
- Event mode (a scenario's `EVENT_ENV`, the same four packages):
  `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=EVENT_ONLY` and
  `OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT=true`. An event capture fails if a
  span carries a content key, or if no log record carries message content.
- OpenInference captures content by default; the scenarios still set
  `OPENINFERENCE_HIDE_*=false` explicitly.
- A capture whose LLM spans lack the scenario's content keys fails instead of
  being written.

## Determinism

- Fixed mock ports: 18431 (OpenAI), 18432 (Anthropic), 18433 (Gemini).
- Trace and span ids come from `FixtureIds` (sha256 of the scenario name and a
  counter).
- Every `*UnixNano` timestamp is replaced by its rank, preserving order.
- A fixed resource `service.name=otel-fixture`; resource detectors are off.
- The fixture clock (`otlp_json.install_fixture_clock`) makes the SDK stamp
  spans (and, in event mode, log records) from a counter that starts at the
  first rank, so the times are already ranked when the exporter sends them.
- `./capture.sh verify` must report the JSON files identical. The `*.binpb`
  request bodies are written but not committed yet, so `verify` does not
  compare them, and a capture into `test-fixtures/otel/` leaves them
  untracked.

## Binary bodies and event mode

The semconv scenarios also export through the OTLP/HTTP protobuf exporters
(`opentelemetry-exporter-otlp-proto-http`, uncompressed, one request per
signal at the final flush) into `otlp_sink.py`, a receiver on `127.0.0.1`
that keeps each request body verbatim. The bodies are written as
`traces.binpb` and, in event mode, `logs.binpb`.

The JSON beside them never comes from the Rust decoder under test:

- span mode: `traces.json` is PR 2's writer over the SDK's span objects
  (`otlp_json.to_otlp_json`);
- event mode: `traces.json` and `logs.json` are the request bodies converted
  by Google's protobuf library (`pb_to_json.py`).

Either way the capture converts each body with `pb_to_json.py` and fails
unless it says exactly what its JSON says. Times are ranked across spans and
log records together. When the fixture clock already produced the ranks, a
body is written as exported; otherwise its times are rewritten to the same
ranks (`pb_to_json.rerank_body`), and the manifest says which.

Manifest keys added by these captures:

| Key | Value |
|---|---|
| `mode` | `"span"` or `"event"` (every capture) |
| `otlp_exporter` | `package`, `version`, and `binpb` (as exported, or times rewritten) |
| `event_name_source` | event mode: `"eventName"`, `"event.name attribute"` or `"both"` |
| `details_content_location` | event mode: where the `gen_ai.client.inference.operation.details` record carries content: `"attributes"`, `"body"`, `"both"` or `"none"` |
| `legacy_events` | event mode: the sorted v1.36 per-role event names seen |

## Safety

- Dummy keys only; no real API key and no real provider endpoint is ever used.
  The mocks bind `127.0.0.1`.
- A socket guard refuses every non-loopback connection during a capture.
- A capture fails if its output contains a host path, the host name, or a
  dummy key.

## Known instrumentation behavior at these pins

- `opentelemetry-instrumentation-genai-openai` 1.2b0 never sets
  `gen_ai.request.previous_response.id` (the util-genai field exists but the
  Responses wrapper does not fill it); fixture normalization writes a
  SYNTHETIC continuation copy of the real capture. It also emits no
  `gen_ai.usage.reasoning.output_tokens` on Responses spans, although the mock
  serves a reasoning count (6 on the first request), so the Responses capture
  has no reasoning breakdown.
- `opentelemetry-instrumentation-openai-v2` 2.4b0 on util-genai 1.1b0 (the
  `openai-chat` capture) reports scope `opentelemetry.util.genai.handler`
  1.1b0; emits no cache-read usage key, although the mock serves cached tokens
  (32 on the second request); sends the system prompt as a `system` message in
  `gen_ai.input.messages`, not `gen_ai.system_instructions`; and emits no
  `gen_ai.tool.definitions`, `server.address` or `server.port`.
- `opentelemetry-instrumentation-genai-anthropic` 1.2b0 (scope
  `opentelemetry.instrumentation.genai.anthropic`) adds
  `cache_creation_input_tokens` and `cache_read_input_tokens` into
  `input_tokens`, so its counts are inclusive; it emits thinking blocks as
  `reasoning` parts without the signature.
- `opentelemetry-instrumentation-google-genai` 1.2b0 synthesizes
  `"{name}_{part index}"` ids for function calls and responses that carry
  none, and emits Gemini thought parts as ordinary `text` parts.
- In event mode, all four packages name the details record by the
  `eventName` field (no `event.name` attribute), put its content in
  attributes (`gen_ai.input.messages`, `gen_ai.output.messages`,
  `gen_ai.system_instructions` where the span mode has it), and emit no
  per-role legacy events. The event manifests record this per capture.
- `opentelemetry-instrumentation-google-genai` 1.2b0 emits a details record
  without content even with `OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT=false`;
  span mode installs no log exporter, so it is not captured there.
- Content capture for the util-genai-based packages is
  `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` in {`NO_CONTENT`,
  `SPAN_ONLY`, `EVENT_ONLY`, `SPAN_AND_EVENT`} (case-insensitive; anything else
  falls back to `NO_CONTENT` with a warning); `OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT`
  (default `false`) gates the details event. OpenInference captures content by
  default and `OPENINFERENCE_HIDE_*` hide it; its tool-result key is
  `message.tool_call_id`.

## Re-encoder

`reencode_openrouter.py` (standard library only): `python3 reencode_openrouter.py [--check]`.
