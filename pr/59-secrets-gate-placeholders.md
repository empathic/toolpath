# chore(gitleaks): allow the two webmock replay placeholders in run.rs

Status: draft · Base: pr/53-develop-webmock-run · Config only (.gitleaks.toml)

## Why
`scripts/quality_gates.sh secrets` (gitleaks over the whole tree) fails on wip itself. Two of the findings are the made-up
credentials webmock `run` hands an agent in replay mode so it starts (the server never checks them):
the `sk-ant-oat01-…` placeholder (CLAUDE_CODE_OAUTH_TOKEN) and the `gho_…` placeholder (COPILOT_GITHUB_TOKEN), both in
crates/toolpath-webmock/src/run.rs (pr/53). .gitleaks.toml spells them out; this record does not, since its path is outside
the allowlist.

## What changes
- .gitleaks.toml: one `[[allowlists]]` entry, `condition = "AND"`: only that file, only those two exact matches. Any other
  token shape, or the same shape in another file, still fails the gate.

## Not done
- The third finding, crates/toolpath-goldens/src/observed.rs:101, is NOT addressed: gitleaks is not available where this was
  prepared, so the rule that fires could not be reproduced. Run `gitleaks dir` and add a matching narrow entry (or change the
  line) once the rule id is known.
- gitleaks itself was not run; only the Rust subset in crates/toolpath-webmock/tests/no_secrets.rs (passes).
