# path-cli

One binary that ties together everything Toolpath can do: derive provenance from git and Claude, query and filter documents, render DAG visualizations, track live editing sessions, and merge results into release graphs. If you want to use Toolpath without writing Rust, start here.

Unified CLI for deriving, querying, and visualizing Toolpath provenance documents.

## Installation

```bash
cargo install path-cli
```

This installs a binary called `path`.

> Previously published as `toolpath-cli`. The old crate name still works — `cargo install toolpath-cli` is now a thin shim that depends on this crate and installs the same `path` binary — but new installs should pin to `path-cli` directly.

Or run from source:

```bash
cargo run -p path-cli -- <command>
```

The CLI splits into porcelain (top-level: `show`, `share`, `resume`,
`query`, `auth`, `haiku`) and plumbing (`path p …`: `p list`,
`p import`, `p export`, `p cache`, `p render`, `p merge`, `p validate`,
`p derive`, `p project`, `p incept`, `p track`).

## Typical workflows

**Capture the provenance of a PR:**

```bash
path p import git --repo . --branch feature --no-cache --pretty > pr-provenance.json
```

**Visualize how a branch evolved, including dead ends:**

```bash
path p import git --repo . --branch main:HEAD~20 --no-cache | path p render dot | dot -Tpng -o history.png
```

**Review what an AI agent changed:**

```bash
path p import claude --project . --no-cache | path query --input - 'map(select(.step.actor | startswith("agent:")))'
```

**Record provenance for a live editing session:**

```bash
cat src/main.rs | path p track init --file src/main.rs --actor "human:alex"
# ... edit the file ...
cat src/main.rs | path p track step --session /tmp/session.json --seq 1 --parent-seq 0
path p track annotate --session /tmp/session.json --intent "Refactored auth"
path p track close --session /tmp/session.json --pretty > session-provenance.json
```

**Merge multiple sources into a release graph:**

```bash
path p merge git-provenance.json claude-provenance.json --title "v2.0 Release" --pretty
```

## Commands

### p list

Discover available sources before importing.

```bash
# List git branches with metadata
path p list git --repo .

# List Claude projects
path p list claude

# List sessions within a project
path p list claude --project /path/to/project

# Machine-readable output
path p list git --repo . --json
```

### p import

Generate Toolpath documents from source systems and write them to the
local cache (`~/.toolpath/documents/`). Use `--no-cache` to stream the
JSON to stdout for shell composition instead.

```bash
# From git history (single branch -> Path, multiple -> Graph)
path p import git --repo . --branch main --pretty
path p import git --repo . --branch main --branch feature --title "Release v2"
path p import git --repo . --branch main:HEAD~20 --pretty

# From Claude conversation logs
path p import claude --project /path/to/project --pretty
path p import claude --project /path/to/project --session abc123
path p import claude --project /path/to/project --all
```

### p export object / p import object / p list object

Object storage — an S3 bucket, an S3-compatible endpoint, or a folder —
as a backup, a hand-off between machines, or a team's shared record.

```bash
# One session, or every cached session (unchanged ones are skipped)
path p export object --input claude-abc --to s3://my-bucket/traces
path p export object --all --to ~/Dropbox/toolpath-traces

# See what is there without downloading anything
path p list object s3://my-bucket/traces --format tsv

# Bring one back, or everything under a prefix
path p import object s3://my-bucket/traces/2026-03-04-fix-the-parser--path-claude-code-abc.json
path p import object s3://my-bucket/traces

# Nightly, from cron: sync the cache, push what changed
path p cache sync && path p export object --all --to s3://team-bucket/traces
```

Objects are named `<date>-<topic>--<id>.json`. The ID is the document's
`graph.id`, and `--` is reserved: split on the last `--` to get it.
Objects hold the full document — every turn, verbatim diffs, and tool
output — so treat the bucket as you would the sessions themselves.
Identity is `graph.id`: two documents with different `graph.id`s never
share a key, and two that share one (git-derived documents from two
repos on the same branch, for instance) replace each other unless
`--no-overwrite` is set.

Credentials: a folder needs none. For `s3://`, your `~/.aws` profiles
(SSO included, via the AWS CLI), `AWS_PROFILE`, or environment keys are
used automatically; `path auth s3 login` stores settings for endpoints
the AWS tooling doesn't know (MinIO, R2). `path auth s3 status` shows
which credential source is in effect; `path auth s3 whoami` asks STS.

#### Object storage as a record store

By default a re-export replaces a session's own object. For a bucket
that must be a record:

- `path auth s3 login --no-overwrite` makes every put create-only
  (`--no-overwrite` on a single export does the same once).
- `path auth s3 login --sse aws:kms --kms-key-id <key>` sets server-side
  encryption; a bucket policy that denies unencrypted puts then works.
- S3 objects carry `x-amz-meta-toolpath-graph-id`, `-sha256`,
  `-uploader`, `-cli-version`, and `-uploaded-at`. `-uploader` is
  `$USER@$HOSTNAME` as reported by the exporting process — attribution,
  not authentication.
- `~/.toolpath/exports.json` records every upload from this machine.

The bucket supplies the rest: Versioning and Object Lock for
immutability, bucket-owner-enforced ownership, server access logging,
and CloudTrail data events for the authoritative principal behind each
write — the CloudTrail data events or server access logs, not the
`-uploader` metadata above, are the authoritative record of who wrote
an object.

No `path` command removes an object. Use bucket lifecycle rules or the
AWS CLI for retention and erasure. Object Lock, recommended above,
makes erasure impossible by design for as long as its retention period
runs, so choose that period deliberately.

### query

Load every step in the local cache into one JSON array and transform it with
an in-process jaq (jq) filter. Each element wraps a Toolpath step with
`cache_id`, `path` (the parent path's `id`/`base`/`meta`), and `dead_end`.
Scope flags choose which documents load; the filter does the rest.

```bash
# Find abandoned branches (the former `dead-ends` subcommand)
path query 'map(select(.dead_end))'

# Steps by an agent actor (the former `filter --actor`)
path query --input doc.json 'map(select(.step.actor | startswith("agent:")))'

# Turns over 50k input tokens, in Claude sessions only
path query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'

# Top 10 steps by total tokens
path query --kind agent-coding-session \
  'map({step: .step.id, t: ([.change[].structural.token_usage//empty | (.input_tokens//0)+(.output_tokens//0)] | add//0)}) | sort_by(-.t) | .[:10]'

# Raw output (-r): a column of ids straight into another command
path query -r '.[].cache_id' | sort -u
```

Scope flags: `--source <name>` / `--id <cache-id>` / `--input <file>` (file
selection), `--project <path>` / `--kind <selector>` (content scoping). Output
mirrors jq: pretty on a TTY, compact when piped (`-c` forces compact); `-r`
prints string results unquoted (for piping ids/paths onward, or reading
text/diff content unescaped).

### kind

List the document kinds the binary bundles a spec for, or print a kind's
bundled `schema.json` — the per-field type and semantics reference for writing
`path query` filters.

```bash
path kind                                # list bundled kinds
path kind agent-coding-session           # newest version's schema
path kind agent-coding-session/v1.0.0    # pin a version
```

### p query

Low-level graph traversal on a single document.

```bash
# Walk ancestry from a step
path p query ancestors --input doc.json --step-id step-003
```

### p render

Render documents to other formats.

```bash
# Graphviz DOT output
path p render dot --input doc.json --output graph.dot
path p render dot --input doc.json --show-files --show-timestamps

# Pipe through Graphviz
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png
```

### p merge

Combine multiple documents into a single Graph.

```bash
path p merge doc1.json doc2.json --title "Release v2" --pretty
path p merge *.json --pretty
```

### p track

Incrementally build a Path document step by step, useful for editor integrations and live sessions.

```bash
# Start a session (pipe initial content via stdin)
echo "hello" | path p track init --file src/main.rs --actor "human:alex" --title "Editing session"

# Record a step (pipe current content via stdin)
echo "world" | path p track step --session /tmp/session.json --seq 1 --parent-seq 0

# Record a step with VCS source metadata
echo "world" | path p track step --session /tmp/session.json --seq 2 --parent-seq 1 \
  --source '{"type":"git","revision":"abc123"}'

# Add a note to the current step
path p track note --session /tmp/session.json --intent "Refactored for clarity"

# Annotate any step with metadata (intent, source, refs)
path p track annotate --session /tmp/session.json --step step-001 \
  --intent "Extract helper" \
  --source '{"type":"git","revision":"abc123"}' \
  --ref '{"rel":"issue","href":"https://github.com/org/repo/issues/42"}'

# Export the session as a Toolpath Path document
path p track export --session /tmp/session.json --pretty

# Export and clean up
path p track close --session /tmp/session.json --pretty

# List active sessions
path p track list
```

### p validate

Check that a JSON file is a valid Toolpath document.

```bash
path p validate --input examples/step-01-minimal.json
# Valid: Step (id: step-001)
```

### haiku

```bash
path haiku
```

## Global flags

| Flag | Description |
|---|---|
| `--pretty` | Pretty-print JSON output |

## Part of Toolpath

This is the CLI for the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
