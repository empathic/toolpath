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

Secret headers are never stored (`is_secret_header`: the list plus any `*-key`, `*-token`, `*-secret`). The first gemini recording predates the `x-goog-api-key` entry and was scrubbed by hand before being committed; the key it carried is to be rotated. The same recordings kept `x-gemini-api-privileged-user-id` (the Google account behind the key) and, in the pi response chunks, the recording machine's scratch path inside the base64; both scrubbed, the header added to the secret list, and `tests/no_secrets.rs` now reads the base64 too.

## Provenance

`<upstream>/provenance.json` says where each directory's cassettes came from: the golden set (its input file and sha256, the session id in it), the resume-matrix cell, the toolpath commit the projection was made at (`projection.toolpath_rev`, with how that was established in `basis`), the probe command and its prompt, and the file list. `tests/provenance.rs` checks it against the tree: the sha256 is the sha256 of `goldens/<set>/input.jsonl` now, the session id is in that file, the file list is the directory.

Every cassette is the golden session plus the headless probe turn, by construction: `path p derive` turned the golden input into the Toolpath document, `path resume --harness <agent>` wrote it where the agent keeps sessions, and the agent was run headlessly on that session with one more prompt (the probe). The request body the proxy recorded is therefore the golden's conversation followed by the probe; nothing else was sent. So cassette and golden are the same session, and a cassette is traceable to its golden through the input sha256.

| cassettes | golden set (input sha256) | cell | toolpath rev |
|---|---|---|---|
| claude | codex (`472c7633…`) | `v2-cell-codex-to-claude` | `6aeff36` |
| codex | claude-session (`a667edf0…`) | `v2-cell-claude-session-to-codex` | `6aeff36` |
| pi | codex | `v2-cell-codex-to-pi` | `6aeff36` |
| copilot | codex | `v3-cell-codex-to-copilot` | `6aeff36` |
| opencode | codex | `gate-cell-codex-to-opencode` | `d93750a` (= `5ff0d26` on wip) |
| gemini | codex | `v4-cell-codex-to-gemini` | `0655ae9` |
| pathbase | codex | `src-codex` (derive only; `path share`) | `0655ae9` (binary), see `basis` |

## Gates

`tests/no_secrets.rs` fails on any credential shape or machine identity in the committed fixture surfaces (rules and allowlist: `/.gitleaks.toml`, shared with the `secrets` gate of `scripts/quality_gates.sh`). It lists every `file:line` and rule, and reads base64 runs. Run it: `cargo test -p toolpath-webmock --test no_secrets`.

The agents' request bodies name the recording machine's temporary HOME; it is replaced by `<scratch>` here, which means the strict request key no longer matches these bodies byte for byte. Re-record with this crate once its normalizer masks that path (`HOME` and the cell directory), as the pathbase cassette already needed nothing of the kind. Until then the model cassettes are evidence (what each harness sends and receives), not replayable fixtures in `cargo test`.
