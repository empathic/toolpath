# Cassettes

One directory per upstream, recorded 2026-10-15 by the Bun prototype of this crate (the spec it was ported from), one real call each, from the toolpath resume matrix's headless cells.

| upstream | what was recorded | replay |
|---|---|---|
| pathbase | `path share --anon` of a golden session against the local pathbase | exact |
| claude | one `claude -p -r` resume cell (Messages API, SSE) | exact in the prototype |
| codex | one `codex exec` resume cell (Responses API, SSE, https through a loopback TLS listener) | exact in the prototype |
| pi | one resume cell | exact in the prototype |
| opencode | one `opencode run` resume cell on the free model | exact in the prototype |
| gemini | one `gemini --resume -p` cell; the request body carries the restored history, the stream was cut by a 429 retry loop | request pinned; no complete response |
| copilot | one `copilot -p` resume cell | records; replay misses (the remote-MCP tool list is nondeterministic) |

Secret headers are never stored (`is_secret_header`: the list plus any `*-key`, `*-token`, `*-secret`). The first gemini recording predates the `x-goog-api-key` entry and was scrubbed by hand before being committed; the key it carried is to be rotated.

The agents' request bodies name the recording machine's temporary HOME; it is replaced by `<scratch>` here, which means the strict request key no longer matches these bodies byte for byte. Re-record with this crate once its normalizer masks that path (`HOME` and the cell directory), as the pathbase cassette already needed nothing of the kind. Until then the model cassettes are evidence (what each harness sends and receives), not replayable fixtures in `cargo test`.
