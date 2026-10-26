# test(quickstart): the toolpath.net quick start, step by step, offline

Status: planned · Base: the goldens stack once merged (needs path-cli at 0.30, the opencode schema fixture from pr/18)

## Why
The home page's "Start with the sessions already on your machine" is the first thing a user runs. Nothing tested it: install.sh,
`cargo install path-cli`, `path p cache sync`, the exact `path query` filter, `path share`, `path resume <pathbase URL> --harness codex`,
and the Claude Code plugin's /path:share and /path:query. A user is the first to find a broken step.

Complements pr/29-site-promises (query over the pinned documents and cache sync for five harnesses, in Rust) and
pr/31-cli-surface (which leaves the quick start to this piece): this one is the user's path end to end, from install.sh to resume.

## What changes
- `scripts/test-quickstart.sh`, run as a new `quickstart` quality gate. In a throwaway HOME, no network:
  - `curl … https://toolpath.net/install.sh | bash` verbatim, with only curl stubbed: it serves `scripts/install.sh` (what
    eleventy publishes at /install.sh) and a release-shaped tarball + .sha256 of this build. Asserts the install dir, version,
    the off-PATH note; the platform table through a uname stub (Linux x86_64/aarch64, macOS arm64; Intel Mac refused with the
    cargo hint); a tampered tarball refused.
  - `cargo install --path crates/path-cli` (crates.io stand-in).
  - one session per harness where it lives: fixture bytes for Claude Code, Codex, Pi, Copilot; `path p export` from the
    golden documents for Gemini CLI, opencode (into an empty 1.18.18-schema database) and Cursor (into an empty state.vscdb).
  - `path p cache sync`: all seven ingested; a second sync changes nothing.
  - the site's `path query` filter verbatim: a per-actor output-token table across harnesses.
  - `path share` against a stateful Pathbase stub on localhost (the anon upload + download shapes from the openapi and the
    recorded cassette); bare `path share` is interactive (picker needs a TTY), so the test drives `--harness/--session/--project`.
  - `path resume` with the site's URL verbatim (refused, see concern) and with the URL share printed: a `codex` stub checks
    the projection wrote a rollout under ~/.codex/sessions with the resume dir as cwd and that `codex resume <its id>` ran.
  - plugin: the marketplace offers path@toolpath; /path:share and /path:query call `path share` / `path query` through
    ensure-path.sh; the wrapper's query result equals the CLI's. Install-on-first-use stays covered by scripts/test-plugin.sh.
- `site/.qual`: concern 8f4d6db1 on site/index.md:238 — the example `https://pathbase.dev/alex/pathstash/path-pr-42` cannot
  resolve: graphs are addressed by UUID and the CLI refuses the slug before any request.

## Verification
- `bash scripts/test-quickstart.sh`: 12 checks pass on macOS arm64 (2026-10-19); one CANNOT-AS-WRITTEN (the resume URL).
- shellcheck clean.

## Paths
```
scripts/test-quickstart.sh
scripts/quality_gates.sh
site/.qual
```
