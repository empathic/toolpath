# Redaction: known gaps

Found by adversarial review of `path p redact` (branch `evan/redact`,
2026-07-30). **None of these are fixed.** They are recorded here rather
than closed, because closing them means changing behavior that already
ships. Read this before assuming a redacted document stays redacted.

The invariant all of these violate is the one Task 10 exists to protect:
`sync::engine::is_unchanged` decides re-derivation from source mtime and
size and never inspects the document, so anything that re-derives and
writes without replaying the stored `RedactionPolicy` publishes the
cleartext it was hiding.

## 1. `p import --force` and `share` overwrite a redacted cache entry

`cmd_import.rs` and `cmd_share.rs` call `write_cached` with the raw
derivation and no replay, then call `sync::record_artifact`, which
deliberately preserves the stored policy while restamping `modified` and
`size` from the fresh provenance. `is_unchanged` therefore returns true
on every later sync and the cleartext is never repaired. The manifest
ends up asserting a policy that is not in force.

Reproduce: `path p redact -i claude-sess-1`, then
`path p import claude --project P --session sess-1 --force`.

## 2. `p cache rm` un-redacts on the next sync

`evict_cache_id` clears the record's policy and `run_rm` deletes the key.
The record keeps its stamps but loses `cache_id`, so the next in-scope
sync re-derives with no policy and recreates the document in cleartext.
`path query` triggers that sync implicitly, so no explicit sync is
needed.

Keeping the key and the policy would be the fix. A policy whose key is
gone can only fail forever, but the key is 32 bytes and its whole purpose
is fingerprint stability across re-redactions of that one document.

Related: `run_rm` removes the key named by the *cache id*, not by
`policy.key_id`. Nothing enforces that those are equal, so a policy with
a different `key_id` means `rm` deletes another document's key.

## 3. A policy recorded mid-sync is clobbered

`sync_bundle` loads the manifest once for the whole run and
`flush_writes` merges with `BTreeMap::extend`, which replaces whole
records. Redacting in one shell while `path query` syncs in another loses
both the document and the policy. `record_artifact` already handles the
same aliasing correctly, so the two writers disagree.

## 4. An empty `detectors` list replays as a silent no-op

`build_detectors` returns an empty `DetectorSet` for `&[]`, so
`generate_checked` finds nothing, `apply` early-returns byte-identically,
and the run reports the document as re-redacted. The manifest is
user-editable JSON, so this is reachable without a code bug.

## 5. An unparseable `redaction` value takes down the whole manifest

`SyncRecord.redaction` deserializes strictly. `#[serde(default)]` covers a
missing key, not a malformed value, so one bad policy makes the entire
manifest fail to load for every artifact type. The error hint then tells
the user to delete the manifest, which discards every stored policy and
un-redacts everything on the next sync.

Version skew is enough to trigger it: `Transform` is a plain string enum
with no unknown-variant fallback.

## 6. Idempotence is unproven for `hash` and `partial`

`internal::mask_existing_markers` blanks `[REDACTED:…]` and runs of `█`
before any rule scans, which is what makes redaction reach a fixed point.
It does **not** cover `Transform::Hash` output (bare 6 hex characters) or
`Transform::Partial` output (`head…tail`). The idempotence test passes
because its scanner uses prefixed self-delimiting formats that no
transform output can reconstitute, so the gap is never exercised.

## 7. `share` uploads the un-redacted derivation

Redacting a document does not affect what `share` sends to Pathbase: the
cached copy is redacted and the uploaded copy is not. This one is
arguably as specified, since the plan's non-goals say share gains no scan
and no automatic redaction. Replaying an already-approved policy for that
exact document is not obviously either of those, so it is recorded here
as a decision rather than a defect.
