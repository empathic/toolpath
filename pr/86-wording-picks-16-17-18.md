# docs: consultant picks 16, 17, 18 (wording)

Status: draft · Base: pr/85 · Files: FAQ.md, RFC.md, site/index.md, site/pages/format.md, docs/model/claims.json, the .qual records they move

- **16 (projection code location):** site/index.md says "a provider crate per harness (it reads sessions and writes them back; the CLI's `projection` modules in
  `path_cli` wire the writers in)". The consultant's suggested "a reader crate plus a projector in path_cli" is not what the tree shows: the projectors (`project.rs`) are
  in the provider crates; `path_cli/src/projection/*` is the CLI glue. No code moved.
- **17 (CI signatures):** "CI" is dropped from the format page table, the FAQ and RFC beyond-VCS item 5 (now "author and reviewer signatures"), and the RFC's
  "CI attestation signatures" bullet became "Several signers on one step or path" (same line count, so no RFC annotation drifts). The `ci` scope stays in the scope role
  list, and `scopes_without_a_canonical_form_are_refused` is untouched. Two qualifier records went from concern to pass, because the unproven part is gone.
- **18 (FAQ open questions):** step ids now answered from the code (git import `step-<short oid>`, conversation derive uses the turn id or `step-NNNN`); structural types
  answered (`conversation.*` and `file.*` are the core namespaces shipped; no registry); evolution answered by versioned kind URIs; privacy reworded as open and honest (no
  redaction marker exists today); the `toolpath:` base sentence now says it "names" a step and notes resolution is unspecified (RFC: no cross-path step references).
  NOT done from the pick: making step ids "the RFC rule" with the id-uniqueness validator fix; that edits Alex's RFC and the validator, so it is a separate piece for him.

Four FAQ/index records re-anchored (qualifier record --supersedes). Not run: pinned 1.94, gitleaks, nix-develop recapture.
