# claims: seven "partly" claims become pass (wording, rust-version, quick-start checks)

Status: draft · Base: wip f308ab5 (independent of pr/63-70) · README.md, site/index.md, site/pages/faq.md, Cargo.toml + 20 member manifests, scripts/test-quickstart.sh, docs/model/claims.json, qualifier records

## Why
These were the cheapest entries in the claims-partly-to-yes plan (2026-10-20): a concern on each where the text overstated, the
example could not run, or the evidence existed but the record did not cite it. Each is fixed in its own commit.

## What changes
- **readme-breakdowns-never-added** (7f2d869): README.md:188 now says "never added into the token counters". total_usage.breakdowns
  sums them class by class (pr/27), so "any total" was false; the counters never include them (test_total_usage_breakdowns_round_trip).
- **readme-rust-185** (c62fc01): README.md:441 now says "Rust 1.94+". `[workspace.package] rust-version = "1.94"`, and all 20
  members inherit it (`rust-version.workspace = true`). toolpath-cli is excluded from the workspace and unchanged.
- **faq-open-evolution** (c7eae30): "How does the format evolve?" moves from Open design questions to Format design, answered with
  versioned kind URIs (links /kinds/ and the v1.1.0 spec). The three open-question records below it moved 4 lines and are
  re-anchored by superseding records with the same content.
- **quickstart-flow** (8fb6ba8): site/index.md:238 uses `https://pathbase.dev/u/alex/repos/pathstash/graphs/<uuid>` (a concrete
  example UUID) in place of the `path-pr-42` slug. test-quickstart.sh no longer expects the page's line to be refused: it reads the
  line from site/index.md, rewrites only the host to the Pathbase stub, and asserts it resolves and execs `codex resume`. Concern
  8f4d6db1 is resolved (fixed).
- **readme-quickstart-dead-end** (6ef546e) and **readme-quickstart-source** (a96f9a1): test-quickstart.sh runs both README
  quick-start queries verbatim after the seven-harness sync. The dead-end count must be an integer (0 on the fixtures). The
  `--source claude` line must return only claude rows. Its filter selects nothing on the fixtures, so a second line checks that
  `--source claude` scopes the cache to claude-* ids.
- **pathbase-stores-shares-resumes** (008508d): record only. test-quickstart.sh already shares to a stateful stub and resumes from the
  shared URL into Codex (`--no-cache`, so it downloads). The pass record cites it.

## Checked (nix develop, rustc 1.97.1; CARGO_TARGET_DIR private)
- `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --features path-cli/develop,toolpath/verify -- -D warnings`: clean.
- `cargo test --workspace --features path-cli/develop,toolpath/verify` (RUSTFLAGS -D warnings): 2220 passed, 0 failed.
- `cargo doc --workspace … --no-deps` (RUSTDOCFLAGS -D warnings): clean.
- `cargo check --offline --workspace --all-targets` (same features) on rustc 1.94.1 (rustup): passes. That supports the 1.94 floor.
- `scripts/quality_gates.sh secrets shellcheck`: 2/2 pass. Site prettier check: clean.
- `qualifier review`: 239 checked, 239 fresh, 0 drifted, 0 missing.
- `just baseline`: ok. `bash scripts/test-quickstart.sh` (offline, stubs): 15 checks passed.

## Open
- README.md:111 still shows the slug URL `https://pathbase.dev/alex/pathstash/path-pr-42` in its own quick start. It has the
  same defect but is not one of these claims, so it is left for a follow-up.
- FAQ.md (repo root) still lists "How does the format evolve?" as open, at line 211. It is not in claims.json.
- docs/model/model.json is not regenerated here. It still shows the old quotes and statuses until the next `just model`.
