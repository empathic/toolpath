# Object storage: identity, credentials, automation, and record-store posture

**Status:** Design proposal
**Date:** 2026-09-14
**Branch:** `alex/object-storage` (PR #238), `path-cli` 0.21.0 unreleased

## Goal

Make the object-storage transport (`p export object`, `p import object`,
`path resume <destination>`, `path auth s3`) serve three jobs end to end
with no workaround and no surprise:

1. **A solo developer backing up sessions** to a folder or bucket, and
   getting one back on another machine.
2. **A security owner using a bucket as a record** of what agents did,
   who wrote each record, and under what protections.
3. **A platform lead feeding automation** from one bucket with a
   predictable layout, no interactive step, and a stable identity per
   session.

A three-persona usability audit of the branch (2026-09-11) found the
transport and the credential design sound and the product around them
unfinished: the object key came from the input filename, the import
cache ID was the whole URI flattened, folder destinations resolved AWS
credentials, nothing was scriptable, and the docs described flags and
commands that do not exist. This design closes every merge-blocking
finding and the automation and record-store gaps in one change set.

## What exists

- `crates/path-cli/src/store.rs` owns `Destination`, `ObjectUri`,
  `ObjectName`, `S3Settings`, and the `object_store` plumbing.
  `store_options()` resolves credentials for every scheme before the
  scheme is known. `ObjectUri::put` is an unconditional overwrite.
  `ObjectUri::cache_id()` flattens host and key into the ID.
- `crates/path-cli/src/aws_creds.rs` resolves credentials: stored keys,
  then a named profile, then `AWS_ACCESS_KEY_ID`, then `[default]`, then
  the instance chain. Static-key profiles are read from `~/.aws`;
  everything else is delegated to `aws configure export-credentials`. An
  expired SSO session is offered a login on a TTY. `resolved_credentials`
  drops the error with `.ok()`.
- `cmd_export::run_object` names the object after the input file's stem,
  uploads whatever bytes it read, and prints the URI.
- `cmd_resume` routes `s3://`, `s3a://`, `file://`, and scheme-less paths
  to object storage. A keyless destination opens a picker; without a
  TTY it prints the inventory to stderr and exits 1.
- `derive::object_fetch_to_doc` validates the body and caches it under
  `ObjectUri::cache_id()`.
- `toolpath_convo::derive_path` sets `path.id` to
  `path-<provider>-<8 chars of the session id>`. That is the only place
  a derived path ID is chosen; the provider crates' eight-character
  prefixes feed titles only. Every harness derive's cache ID is
  `<source>-<path.id>`.
- The sync manifest (`sync/engine.rs`) maps artifact type and ID to a
  `SyncRecord` with the cache ID and a stat fingerprint. `share` uses
  `fresh_cache_id` to upload a cached document instead of re-deriving.
- `share_config.rs` resolves a `[[project]]` rule to a `ConfiguredRemote`
  through `remote::parse_remote`, which accepts `owner/name` or an
  `https://` Pathbase repo URL and rejects every other scheme.
- `object_store` 0.14.1 provides `PutMode::Create` (also honored by the
  local backend), `Attribute::Metadata` (rejected by the local backend),
  and the config keys `aws_conditional_put`,
  `aws_server_side_encryption`, and `aws_sse_kms_key_id`.

## Design

### 1. Identity

**Object name.** `<date>-<topic>--<id>.json`. `--` is reserved: the
slugger collapses dash runs, so neither the date nor the topic can
contain it. Automation reads the ID by splitting on the last `--`. A
document with no date or topic is named `<id>.json`; the documented rule
is "the ID is everything after the last `--`, else the whole stem".

**The ID is `graph.id`**, read from the parsed document. It is never
derived from the input path. For every derived session `graph.id` equals
`path.id`. An ID longer than 64 characters is truncated to 48 and
suffixed with the first 8 hex characters of its SHA-256, so hand-written
documents cannot produce unbounded names. Exporting one session from a
cache ID and from a copied file lands on one key; two different
documents with the same basename land on two.

**Import cache ID.** `object-<id>`, where the ID comes from the object
name when the name carries `--`, otherwise from a slug of the stem capped
at 100 characters. It is a function of the URI alone, so the cache probe
before a fetch still costs no request. `ObjectUri::cache_id()` is the
single implementation; `cmd_resume` and `derive::object_fetch_to_doc`
keep calling it.

The round trip is `claude-path-claude-code-<id>` → cache →
`<date>-<topic>--path-claude-code-<id>.json` → `object-path-claude-code-<id>`.
Re-exporting an `object-` document names itself by the same `graph.id`,
so a mirror loop is a fixed point. `path query --source object` selects
exactly the imported documents.

**Path ID width.** `toolpath_convo::derive_path` takes 16 characters of
the session ID instead of 8. The provider crates' prefixes for titles
stay at 8. `toolpath-convo` bumps 0.11.1 → 0.11.2 with a changelog entry
stating that newly derived path IDs are wider and that existing cached
documents keep their IDs.

**Migration.** A Claude session's cache ID is `claude-<path.id>`, so the
first re-derive of a changed session after this change writes a new
cache file. The sync engine, when a record's `cache_id` changes on
re-derive, removes the superseded cache document after the new one is
written. Records whose sources never change keep their old IDs and old
object names; that is correct and needs no action.

### 2. Credentials

**Resolution is per scheme.** `open_with` passes the URL scheme to
`store_options`. For `file://` the option list is empty: no resolver
call, no `aws` spawn, no SSO offer. For `s3://` and `s3a://` the
resolver runs and its error propagates. The resolver already returns an
instance-chain result when nothing is configured, so the only errors
left are explicit failures (a named profile that does not exist, an
expired SSO session with no terminal, a malformed `export-credentials`
response), and those become the command's error verbatim.
`S3Settings::resolved_credentials` is removed; `resolve_real` is the
only entry point.

**Provenance survives the environment merge.** `merge_env` sets a
`#[serde(skip)]` flag on `S3Settings` when it fills the access key from
`AWS_ACCESS_KEY_ID`. `resolve_with` maps that flag to
`Source::Environment`, so `auth s3 status` no longer reports env keys as
stored.

**`auth s3 status`** prints the access key ID for every source
(including profile-resolved and CLI-resolved credentials), prints the
effective region with a `(default)` marker when nothing supplied one, and
runs the resolver before deciding whether to print the "run `path auth
s3 login`" advice; it prints that advice only when resolution ends at the
instance chain.

**`auth s3 whoami`** runs `aws sts get-caller-identity --output json`
with the resolved credentials injected as `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, and region, so it works
for every source. It prints account, ARN, and user ID, and errors with a
plain message when `aws` is not on PATH.

### 3. Validation and errors

**Validate before put.** `run_object` parses the body as a Graph and
runs `schema::validate` on it before any PUT. Failure is an error naming
the input and the reason; `--force` uploads anyway. The name is computed
from the parsed document, so `name_for_body` and its degrade path are
removed.

**Errors keep their cause.** `terse` walks the `std::error::Error`
source chain to the innermost message and strips the `Generic S3
error: ` and `Generic LocalFileSystem error: ` prefixes. When
resolution ended at the instance chain and the transport error mentions
`169.254.169.254`, `explain_location` replaces it with: "no credentials
found (tried ~/.aws, the environment, and the EC2/ECS/EKS chain). Run
`path auth s3 login` or set AWS_PROFILE."

**`--dry-run`** on `p export object` prints the resolved URI, endpoint,
region, credential source, and whether the put would be create-only,
then exits 0 without writing.

### 4. Automation surface

**`p list object <destination> [--format pretty|tsv|json]`** lists the
`.json` objects directly under a destination. Columns: ID, date, topic,
size, modified, uri. Format resolution follows `p list`: pretty on a
TTY, tsv when piped. Empty is exit 0 (pretty prints "no documents in
<dest>" to stderr; tsv and json print nothing and `[]`).

**`p import object <prefix>`.** A target whose key does not end in
`.json` is a destination: every object it lists is imported, per-object
failures are reported and skipped, and the exit code is 1 at the end if
any failed. `--force` and `--no-cache` apply per object.

**`path share --to <destination>`** routes the picked or explicit session
through the object exporter instead of Pathbase, skipping Pathbase
preflight auth. `--to` conflicts with `--repo`, `--anon`, `--name`, and
`--public`. The `[[project]] remote` grammar accepts `s3://`, `s3a://`,
`file://`, and absolute or `~/` paths in addition to the Pathbase forms;
`ConfiguredRemote` becomes an enum of Pathbase or object destination, and
`resolve_destination` dispatches on it.

**`p export object --all --to <destination>`** exports every cached
document except those with an `object-` or `pathbase-` prefix
(`--include-imported` includes them). Unchanged documents are skipped
using the export ledger. Per-document failures are tallied, not fatal;
exit 1 at the end if any failed. Output is one line per document on
stderr and the URIs on stdout.

**Export ledger.** `~/.toolpath/exports.json` (0600), a map of
destination → cache ID → `{uri, sha256, uploaded_at, uploader, bytes}`.
Every object export, single or bulk, writes it with the same
temp-and-rename discipline as the manifest. `--all` skips a document
whose sha256 matches the ledger entry for that destination. This file is
also the local egress record: it answers "what left this machine, to
where, when, as whom".

**Non-TTY picker.** The error reads: "picking needs an interactive
terminal; list with `path p list object <dest>` and pass a full
location instead." The inventory is no longer printed on the error path.

**`path query`.** An unknown `--source` with no matching cache files
warns on stderr rather than silently returning an empty result.

### 5. Record-store posture (opt in)

**Overwrite stays the default.** `--no-overwrite` on `p export object`
and `share --to` uses `PutMode::Create`; an existing key is an error:
"<uri> already exists; drop --no-overwrite to replace it". `s3.json`
gains `no_overwrite`, `server_side_encryption`, and `sse_kms_key_id`, set
by `auth s3 login --no-overwrite | --overwrite`, `--sse <AES256|aws:kms>`,
and `--kms-key-id <id>`. The SSE fields pass through to
`aws_server_side_encryption` and `aws_sse_kms_key_id`. A stored
`no_overwrite` applies to every S3 put; the flag overrides per call.

**Uploader identity** travels as S3 object metadata, not in the body:
`toolpath-graph-id`, `toolpath-sha256`, `toolpath-uploader`
(`<user>@<host>`), `toolpath-cli-version`, `toolpath-uploaded-at`.
Attributes are attached only for `s3`/`s3a`; the local backend rejects
them.

**Folder permissions.** After a `file://` put, the object is set to
0600, and any directory the export created (absent before the put) is
set to 0700. Pre-existing directories are left alone.

**Docs.** A "Object storage as a record store" section in the path-cli
README names what the bucket side must supply: Versioning, Object Lock,
SSE-KMS, bucket-owner-enforced ownership, server access logging, and
CloudTrail data events for attribution beyond the metadata stamp.

### 6. Documentation reconciliation

Fix every place the docs describe something the code does not do:

- Remove `path target` from `cmd_auth.rs` help and the `store.rs` module
  doc; the replacement wording is "`--to` on export, or a `[[project]]`
  remote".
- Remove the per-command `--profile` from CLAUDE.md, the CHANGELOG
  entry, and the `store.rs` comment; `--profile` exists only on
  `auth s3 login`.
- `cmd_resume.rs` input help lists `s3://`, `s3a://`, `file://`, and
  folder shapes; the `--no-cache` and `--force` help no longer says
  Pathbase-only.
- CLAUDE.md precedence order matches the resolver: named profile, then
  environment keys, then `[default]`, then the instance chain.
- The object-name shape is documented with the `--` rule and the
  optional date and topic.
- README CLI reference, path-cli README (`p export object`, `p import
  object`, `p list object`, `auth s3`, `share --to`, a worked nightly
  cron), `site/pages/cli.md`, `site/_data/crates.json` role text, and
  the export help's one sentence: objects carry the full transcript with
  verbatim diffs and tool output.
- CHANGELOG `path-cli 0.21.0` entry rewritten to describe the shipped
  behavior; `toolpath-convo 0.11.2` entry added.

### 7. Proof

Folder-backed integration tests in `tests/object_storage.rs`:

- two different documents with the same basename land on two keys;
- the same document from a cache ID and from a file lands on one key;
- the existing overwrite test asserts a filename-independent name;
- import of a `--` name yields `object-<id>`, and re-export of that
  document is a fixed point;
- prefix import, list in all three formats including empty, bulk export
  with a ledger skip on the second run, dry run writes nothing,
  `--no-overwrite` errors on the second put;
- a stub `aws` on PATH recording its invocations: zero for a folder
  export, and a propagated error (no fall-through, no IMDS URL) for an
  expired SSO profile on `s3://` without a TTY;
- `auth s3 status` reports environment keys as environment.

Sync engine unit test: a record whose cache ID changes on re-derive
loses its old cache document.

Live S3: a test gated on `TOOLPATH_S3_TEST_ENDPOINT` (plus bucket and
credential variables) that round-trips one document, and
`scripts/test-object-storage-live.sh` that runs it against a MinIO
container, mirroring `scripts/test-pathbase-live.sh`.

## Out of scope

- Redaction flags (`--no-thinking`, `--no-tool-output`).
- Deletion, retention, and erasure procedures.
- Per-destination S3 settings (one stored endpoint still applies to
  every `s3://` destination).
- `path query --input s3://…` reading directly from object storage.
- A `--latest` selector on `resume <destination>`.

## Decisions taken

- `toolpath-convo` takes a patch bump; the API is unchanged and the ID
  width is a behavior change consumers on `^0.11` should receive.
- Imported documents get an `object-` prefix rather than reusing the
  originating harness cache ID, so provenance is visible and local sync
  records are never overwritten by a remote copy.
- The export ledger is its own file, not part of the sync manifest, which
  describes sources, not egress.
- Record-store settings live in `s3.json` beside the connection settings
  they modify.
