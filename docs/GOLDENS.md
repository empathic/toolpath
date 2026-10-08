# SPEC: hermetic goldens for conversation transformations

Written 2026-10-08 (bobby-scratch, from Bobby's rulings between 13:00 and 16:30 EDT; each rule cites the time it was said). Applies to empathic/toolpath and empathic/pathbase; the same text is committed in each repo as docs/GOLDENS.md with a repo appendix. `goldens check` cites this file by sha.

## 1. What a golden is
A golden is a captured input (a transcript a real harness produced) plus the exact bytes the transformation produced from it at a pinned revision, plus a manifest entry that pins everything used to produce it. It asserts "this is what the code did"; a human review (the bless, section 9) makes it "this is right".

## 2. Capture is the tool (15:3x, "capturing the goldens is the demo")
Capture, check, diff, list and evidence are first-class verbs of the repo's tool (`path goldens …`, `pathbase-goldens …`), never an environment variable on a test and never a shell script. Tests call the tool's library, so test and tool cannot drift.

## 3. Hermetic (13:0x onward)
Every capture and every check runs under a cleared environment: temp HOME, TMPDIR, XDG_*, and each harness's own config dir (CLAUDE_CONFIG_DIR, CODEX_HOME, …), cwd pinned, a throwaway database for pathbase. The leak check refuses any output that names the real home directory, the local username, a credential, or a non-allowlisted email; account identifiers are redacted to stable placeholders and the redaction is recorded. Credential files are only ever copied into the temp home and die with it.

## 4. Everything pinned (14:14, "the harness and everything they use should be sha pinned")
The manifest records, per set: input and output sha256, the exact command, the repo rev, the tool version, the nix store path and narHash of any pinned binary, the toolchain channel, the lock files, and the sha256 of the goldens code itself. `check` fails on any moved pin and names it; `capture` rewrites all of it; nothing is hand-edited.

## 5. Harness provenance (15:5x, 16:0x)
Each set records the harness that produced its input: name, version, binary path and sha256, code-signing authority (Developer ID, adhoc, or none) and the upstream match (release manifest, GitHub release digest, npm integrity) with matched true or false. Unknown-origin fixtures carry version "unknown" and the commit that first added them. A changed local harness is information, not failure; the input bytes are what is pinned.

## 6. Coverage (16:2x, "goldens for every harness in our codebase")
Every adapter in the workspace has at least one set whose input came from that harness, in every direction the adapter supports, or a written reason in DEMO.md saying what closes the gap. `check` fails on an adapter with none. Pathbase is an adapter: IR -> stored document -> IR.

## 7. The property is per adapter (16:2x, "really it's just to and from toolpath")
For every adapter X, IR -> X -> IR must preserve the IR's own content; longer chains are compositions and get no test of their own. Generated documents (from the IR types, seeded, bounded) exercise the edge for every adapter; the report lists, per adapter, the fields that never survive, each marked declared or defect; a failing seed is minimised into a committed golden.

## 8. Shed is reported, not hidden (16:1x)
`shed` shows, per stage, event types and key paths in and out, bytes and text in and out, and the dropped fields by name. DEMO.md states what the IR keeps on purpose and what it sheds by design.

## 9. Lock by blessing (15:5x)
After a set is on main at commit S, a follow-up PR adds or bumps LOCK.json with S and each golden's git blob sha, and nothing else. `check` verifies blob shas against it; `capture` never writes it; a golden change without a lock bump fails.

## 10. Tests come from goldens (15:5x, "never pure fabrication")
Test inputs are a golden or a factory that slices one (`golden(set).input()`, `.events()`, `.slice()`, `.with_field()`); a test that cannot be derived from a capture is tagged FABRICATED with a reason so the count stays visible.

## 11. Findings are recorded where the code is (16:3x, qualifier)
Every shed defect, declared limit, fabricated test, uncovered adapter and known defect is a qualifier record anchored on the owning code, issued by the session or person that found it, committed with the goldens. `check` runs `qualifier review` and reports drift.

## 12. Evidence and the demo (16:0x)
`evidence` writes check output, provenance and shed reports under goldens/evidence/<date>/, each file sha-listed in the manifest. The Friday demo is a stepped understudy run of the tools with a screenshot per stop; the demo board shows the latest evidence.

## 13. Installs roll from wip (15:2x)
Every tool here is installed from its repo's wip/bdelanghe pin by the nightly bump and autoswitch, so a demo can always run off wip; side-by-side variants are store paths.

---

# Appendix: empathic/toolpath

## Verbs (`path goldens …`, `scripts/goldens.sh` is a one-line wrapper)
`list`, `init`, `capture <harness> <fixture> [--name N] [--project P]`, `capture --all`, `check [set]`, `diff <set>`, `roundtrip [set]`,
`capture-live <claude|codex|pi|copilot>`, `provenance <agent>`. Not yet built: `shed`, `evidence`, `check --json/--report`, `roundtrip --generated`.
The goldens test (`crates/path-cli/tests/goldens.rs`) calls `path_cli::goldens::check`, the same library.

## Adapter table (rule 6)
| adapter | sets (input origin) | projection directions | gap, and what closes it |
|---|---|---|---|
| claude | `claude` (fixture, declares 2.1.132), `claude-compacted` (synthetic), `claude-session` (live capture, 2.1.285) | codex, pi, copilot | none |
| codex | `codex` (fixture, declares 0.128.0) | claude, pi, copilot | live capture: `capture-live codex` (Bobby's terminal) |
| copilot | `copilot` (fixture, declares 1.0.68) | claude, codex, pi | live capture: `capture-live copilot`; copilot -> claude cannot be re-derived (pinned) |
| pi | `pi` (fixture, version unknown) | claude, codex, copilot | live capture: `capture-live pi` |
| cursor | none | none yet | the fixture `test-fixtures/cursor/convo.json` is an export that only test code loads, and cursor-agent keeps a protobuf store the adapter does not parse; closed by a library loader in the goldens module or `p derive --from-file` |
| gemini | none | none yet | gemini is not installed here; `test-fixtures/gemini/convo.jsonl` exists but its `~/.gemini/tmp/<slot>/chats` placement is not yet worked out |
| opencode | none | none yet | opencode keeps its session in SQLite (no single-file input); the fixture is an export parsed by test code only; closed by a library loader or `p derive --from-file` |
`check` does not yet fail on an adapter with no set (cursor, gemini, opencode): that is the next change after the loader.

## FABRICATED tests (rule 10)
Audit not yet done; zero tests are tagged `// FABRICATED:` so far. The count will be listed here.

## Known defects pinned (rule 11)
`goldens/known-defect/codex-to-claude-illegal-types.tsv` (codex events leak into claude output as foreign `type`s) and
`goldens/known-defect/claude-to-codex-caller-cwd.tsv` (`p export codex -o` writes the caller's cwd). Round-trip losses are pinned in `goldens/roundtrip/`.
