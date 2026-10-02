#!/usr/bin/env python3
"""Re-encode the OpenRouter M0 fixtures as current GenAI semconv (spec: Re-encoder).

Standard library only. Writes test-fixtures/otel/equivalence/<name>.ndjson.
  python3 reencode_openrouter.py          write the files
  python3 reencode_openrouter.py --check  exit 1 when a committed file differs
"""
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SRC = REPO / "test-fixtures" / "otel" / "openrouter"
DST = REPO / "test-fixtures" / "otel" / "equivalence"
INPUTS = ["claude-code.ndjson", "codex.ndjson", "opencode.ndjson", "pi.ndjson",
          "synthetic-fork.ndjson", "codex-error-span.json"]
ROOT = "LLM Generation"
SCOPE = "toolpath-reencode"
COPY = ["gen_ai.response.id", "user.id", "gen_ai.request.model", "gen_ai.response.model",
        "gen_ai.provider.name"]
USAGE = {
    "gen_ai.usage.input_tokens": "gen_ai.usage.input_tokens",
    "gen_ai.usage.output_tokens": "gen_ai.usage.output_tokens",
    "gen_ai.usage.input_tokens.cached": "gen_ai.usage.cache_read.input_tokens",
    "gen_ai.usage.input_tokens.cache_write": "gen_ai.usage.cache_write.input_tokens",
    "gen_ai.usage.output_tokens.reasoning": "gen_ai.usage.reasoning.output_tokens",
}
THINKING = ("thinking", "redacted_thinking", "reasoning")


def compact(v):
    return json.dumps(v, ensure_ascii=False, separators=(",", ":"))


def s(value):
    return {"stringValue": value}


def content_text(content):
    """The Rust crate's normalize::content_text, reimplemented."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        out = []
        for p in content:
            t = p.get("type") if isinstance(p, dict) else None
            if t in (None, "text"):
                if isinstance(p, dict) and isinstance(p.get("text"), str):
                    out.append(p["text"])
            elif t in THINKING:
                continue
            else:
                out.append(f"[{t}]")
        return "\n".join(out)
    return ""


def content_parts(content):
    if isinstance(content, str):
        return [{"type": "text", "content": content}]
    if not isinstance(content, list):
        return []
    parts = []
    for p in content:
        if not isinstance(p, dict):
            continue
        t = p.get("type")
        if t in (None, "text"):
            if isinstance(p.get("text"), str):
                parts.append({"type": "text", "content": p["text"]})
        else:
            parts.append(p)  # verbatim generic part, type kept
    return parts


def tool_call_part(call):
    fn = call.get("function") or {}
    part = {"type": "tool_call", "name": fn.get("name"), "arguments": fn.get("arguments")}
    if call.get("id") is not None:
        part["id"] = call["id"]
    return part


def message(m):
    role = m.get("role")
    if role == "tool":
        part = {"type": "tool_call_response", "response": content_text(m.get("content"))}
        if m.get("tool_call_id") is not None:
            part["id"] = m["tool_call_id"]
        return {"role": "tool", "parts": [part]}
    parts = [{"type": "reasoning", "content": d.get("text") or ""}
             for d in (m.get("reasoning_details") or []) if isinstance(d, dict)]
    parts += content_parts(m.get("content"))
    parts += [tool_call_part(c) for c in (m.get("tool_calls") or []) if isinstance(c, dict)]
    return {"role": role, "parts": parts}


def output(completion, finish_reason):
    parts = []
    if completion.get("reasoning"):
        parts.append({"type": "reasoning", "content": completion["reasoning"]})
    if completion.get("completion"):
        parts.append({"type": "text", "content": completion["completion"]})
    parts += [tool_call_part(c) for c in (completion.get("toolCalls") or [])]
    out = {"role": "assistant", "parts": parts}
    if finish_reason is not None:
        out["finish_reason"] = finish_reason
    return [out]


def span(sp):
    attrs = {kv["key"]: kv.get("value") for kv in sp.get("attributes") or []}
    get = lambda k: (attrs.get(k) or {}).get("stringValue")  # noqa: E731
    model = get("gen_ai.request.model") or ""
    new = [{"key": "gen_ai.operation.name", "value": s("chat")}]
    new += [{"key": k, "value": attrs[k]} for k in COPY if k in attrs]
    if "session.id" in attrs:
        new.append({"key": "gen_ai.conversation.id", "value": attrs["session.id"]})
    finish = get("gen_ai.response.finish_reason")
    if finish is not None:
        new.append({"key": "gen_ai.response.finish_reasons",
                    "value": {"arrayValue": {"values": [s(finish)]}}})
    new += [{"key": USAGE[k], "value": attrs[k]} for k in USAGE if k in attrs]
    prompt = get("gen_ai.prompt")
    if prompt is not None:
        msgs = json.loads(prompt).get("messages") or []
        new.append({"key": "gen_ai.input.messages", "value": s(compact([message(m) for m in msgs]))})
    completion = get("gen_ai.completion")
    if completion is not None:
        new.append({"key": "gen_ai.output.messages",
                    "value": s(compact(output(json.loads(completion), finish)))})
    out = {k: sp[k] for k in ("traceId", "spanId", "parentSpanId", "kind",
                              "startTimeUnixNano", "endTimeUnixNano", "status") if k in sp}
    out["name"] = f"chat {model}".strip()
    out["attributes"] = new
    return out, get("trace.metadata.openrouter.api_key_name")


def delivery(d):
    resource_spans = []
    for rs in d.get("resourceSpans") or []:
        for ss in rs.get("scopeSpans") or []:
            for sp in ss.get("spans") or []:
                if sp.get("name") != ROOT:
                    continue  # generation / provider attempt children, connection test
                new, key_name = span(sp)
                resource = {"attributes": [{"key": "service.name", "value": s(key_name)}]} \
                    if key_name is not None else {"attributes": []}
                resource_spans.append({"resource": resource,
                                       "scopeSpans": [{"scope": {"name": SCOPE}, "spans": [new]}]})
    return {"resourceSpans": resource_spans} if resource_spans else None


def reencode(name):
    text = (SRC / name).read_text()
    values = [json.loads(line) for line in text.splitlines() if line.strip()] \
        if name.endswith(".ndjson") else [json.loads(text)]
    lines = [compact(out) for out in map(delivery, values) if out is not None]
    return "".join(line + "\n" for line in lines)


def main():
    check = "--check" in sys.argv[1:]
    DST.mkdir(parents=True, exist_ok=True)
    bad = []
    for name in INPUTS:
        target = DST / (name.rsplit(".", 1)[0] + ".ndjson")
        body = reencode(name)
        if check:
            if not target.exists() or target.read_text() != body:
                bad.append(target.name)
        else:
            target.write_text(body)
    if bad:
        sys.exit("re-encoded fixtures are stale: " + ", ".join(bad))
    print("ok" if check else f"wrote {len(INPUTS)} files to {DST.relative_to(REPO)}")


if __name__ == "__main__":
    main()
