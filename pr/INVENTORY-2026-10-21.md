# Flatten inventory, 2026-10-21

wip/bdelanghe was flattened on 2026-10-21 from `d27eef07` (the 2026-10-15 squash) plus 137 signed commits (tip `ae1faded`) into one commit over main `77dc16a5`. The pre-flatten history is kept locally only, as the ref `refs/backup/wip-pre-flatten-2026-10-21` in Bobby's toolpath checkout plus a verified bundle (`toolpath-wip-pre-flatten-2026-10-21.bundle` in his ops backups); it is not in the mirror, because the pre-push hook refuses branches other than wip/bdelanghe. Every one of those 137 commits is listed here with the record it belongs to; each record's commits are exported, in order, as an mbox in `pr/patches/<record>.patch` (`git am` replays them). PROCESS commits only touched `pr/` itself and have no patch.

| commit | record | subject |
|---|---|---|
| `78a8cef8` | 15-no-secrets | fix(webmock): x-gemini-api-privileged-user-id is a secret header; scrub what the |
| `1326db8d` | 15-no-secrets | test(webmock): no_secrets gate over the committed fixture surfaces; gitleaks gat |
| `ffac9f66` | 13-webmock | docs(webmock): cassette provenance — golden set, input sha256, cell, toolpath re |
| `843c01d2` | 15-no-secrets | fix(quality-gates): the secrets gate scans . from the repo root so the .gitleaks |
| `ba79ebb8` | 15-no-secrets | fix(webmock): needless_borrow at up_headers_for_client (clippy -D warnings under |
| `e80b23c8` | 07-insta-goldens | fix(goldens): allow rustdoc::invalid_html_tags for the clap help placeholders (d |
| `cab1d3dd` | PROCESS | chore(pr): shared files are assigned per hunk; cut.sh gates every piece on its o |
| `bb841bf8` | 12-goldens-all-harnesses | fix(goldens,webmock): every PR piece builds on its own tree — producer test skip |
| `b5aa80ee` | PROCESS | chore(pr): era-restore and gate-piece ride with the pr/ process |
| `303d19c9` | PROCESS | chore(pr): open.sh opens the drafts from these records; every record carries its |
| `fd7161cf` | 14-claims-audit | docs(site): kinds/index.md formatted the way the prettier gate wants it |
| `d4969562` | PROCESS | chore(pr): gate-piece runs the prettier half of the format gate too (CI caught s |
| `490f2aca` | 13-webmock | test(webmock): the committed-cassette test picks the cassette, never provenance. |
| `151e379a` | PROCESS | chore(pr): 16-bless-lock — the blessing follow-up, decided 2026-10-16 (gate stay |
| `08c6a6b7` | 17-goldens-feature | feat(path-cli): `path goldens` behind a `goldens` feature, off by default |
| `45cb679b` | 18-pin-unpinned-edges | test(goldens): pin the opencode database read and codex's absolute rollout_path; |
| `e0415cd6` | 20-cursor-3-24-heads | fix(cursor): read Cursor 3.24's composerHeaders table; write a row there too |
| `9fc0cd8d` | PROCESS | chore(pr): 20-cursor-3-24-heads — the cursor 3.24 header fix as its own piece |
| `635f8dc8` | 19-gemini-jsonl | fix(gemini): read gemini-cli 0.47's session-*.jsonl record stream |
| `83042e2f` | 21-webmock-jsonrpc-keys | fix(webmock): replay JSON-RPC answers with the live id; key JSON-RPC calls by me |
| `d32e275d` | 22-codex-resume-cwd | test(goldens): retire claude-to-codex-caller-cwd; the resume directory is the co |
| `79003bcd` | 23-object-model | feat(model): toolpath-model, the goldens work as one generated object model |
| `e0b08b64` | 23-object-model | chore(model): generate docs/model/model.json at 79003bc; schema, design and pr/2 |
| `8f6837ee` | 24-mock-harness | test(harness-mock): what Claude Code would send when it resumes a projected sess |
| `3f75b6b3` | 25-ci-qualifier-fail-closed | ci(goldens): install the pinned qualifier in CI; the qualifier review fails clos |
| `86c325bd` | 26-writer-isolation | test(goldens): each writer on its own — one generated document, a writer@<agent> |
| `76377538` | 27-accounting-totals | fix(gemini): the session total adds the turns' breakdowns |
| `80e8a169` | 27-accounting-totals | fix(opencode): the session total adds the turns' breakdowns |
| `9eec499c` | 27-accounting-totals | fix(copilot): session.shutdown's totals settle on the last assistant turn |
| `04afa536` | PROCESS | chore(pr): 27-accounting-totals — the three accounting known-defects fixed as on |
| `af15dabd` | 28-claude-writer-entry-types | fix(claude): write only Claude Code entry types; write message.stop_reason |
| `88131405` | 29-site-promises | test(goldens): path query over every session, the core types, both file formats, |
| `26ad6b4a` | 30-git-import-rfc-vcs | test(git): RFC.md's VCS table row by row through the git import |
| `242861e7` | 31-cli-surface | test(path-cli): pin the CLI surface — help tree, the CLI page against the binary |
| `7edb3772` | PROCESS | chore(pr): 31-cli-surface — renumbered from 29 (taken by site-promises); gates o |
| `810a58b6` | 33-signatures | feat(toolpath): the RFC's signature algorithm, proven — JCS inputs, SHA-256 dige |
| `04640200` | 32-quickstart-e2e | test(quickstart): the toolpath.net quick start, step by step, offline |
| `1474580f` | PROCESS | chore(pr): 33-signatures — renumbered from 29 then 32 (taken by site-promises, q |
| `ba8a9cd1` | 34-head-keeps-the-conversation | fix(convo): path.head keeps the conversation in its ancestry |
| `7b2d82e8` | 37-claims-as-records | claims: every statement on the home page, the format page, the v1.1.0 kind pages |
| `860e3e3f` | 27-accounting-totals | claims: accounting holds for all nine sets after pr/27 (session-totals now pass) |
| `893cae71` | 27-accounting-totals | toolpath-model: match a claim's quote whitespace-collapsed; pr/27 names pr/01 in |
| `2de1ca16` | 37-claims-as-records | claims: the FAQ (site/pages/faq.md, FAQ.md) as records; the model knows writer@  |
| `6207e3d7` | 28-claude-writer-entry-types | model: regenerated at the claims records (113 claims: 64 pinned, 48 partly, 1 fa |
| `a5b609c6` | 37-claims-as-records | claims: the crate READMEs docs.rs renders — one doctest record per README (run,  |
| `49f4f8f8` | 37-claims-as-records | claims: the repo README (supported-harness table, quick start, token accounting, |
| `064ce6d1` | 37-claims-as-records | claims: re-point records at the tests that landed (query over every harness, cor |
| `902d0fed` | 31-cli-surface | pr/31: the claims piece (renumbered; pr/28 is the claude writer); pr/28's note q |
| `dd53a90e` | 37-claims-as-records | model: regenerated (172 claims: 98 pinned, 68 partly, 6 false) |
| `09b4e596` | 31-cli-surface | pr/31: the tests it cites have landed |
| `277cfb51` | 23-object-model | toolpath-model: formats and validate_transforms snapshot kinds |
| `49dc274e` | 37-claims-as-records | model: regenerated with the new snapshot kinds |
| `05e3781c` | 23-object-model | toolpath-model: fmt |
| `c2e6f2c3` | 32-quickstart-e2e | claims: signatures (ssh, feature verify) and the CLI surface landed — records re |
| `ebc66ca6` | 32-quickstart-e2e | model: regenerated (172 claims: 100 pinned, 66 partly, 6 false); pr/32 counts |
| `d82523ad` | 37-claims-as-records | qualifier: note that the path-02 jsonl concern was fixed by 8813140 (it drifts a |
| `1302016c` | 37-claims-as-records | qualifier: drop the path-02 reply; 33-signatures resolved that record itself |
| `e299e60a` | 34-head-keeps-the-conversation | claims: the quickstart gate landed — install, sync, query, share and resume reco |
| `a1146643` | 37-claims-as-records | model: regenerated (172 claims: 101 pinned, 65 partly, 6 false) |
| `a350fb8d` | 36-loader-validation | docs(formats): how each harness validates the session it resumes — extra/missing |
| `a5a32feb` | 37-claims-as-records | claims: the dead-end definition passes now that ba8a9cd keeps the conversation i |
| `199b19f2` | 37-claims-as-records | model: regenerated |
| `9548571e` | 37-claims-as-records | pr/37: the claims piece (renumbered; 34 and 36 taken); model regenerated (102 pi |
| `5815197f` | 35-regression-corpus | test(goldens): contracts and a regression corpus from CHANGELOG's shipped bugs |
| `dfd4f3db` | 40-pi-provider | feat(convo,pi): who served the model rides the RFC's actor slot; pi keeps its pr |
| `76d1da2c` | 35-regression-corpus | chore(goldens): repin after the actor-provider change; pr/35-pi-provider |
| `7a93457c` | 38-cursor-resume-history | fix(cursor): write the model history Cursor 3.24 reads, so a resumed chat carrie |
| `82068460` | 39-baseline-first-development | docs: development starts from the baseline; the unreleased changelog entry |
| `d46c080c` | PROCESS | chore(pr): 39-baseline-first-development — renumbered from 38 (taken by cursor-r |
| `e7d928ee` | PROCESS | chore(pr): 38-pi-provider — renumbered from 35 (taken by regression-corpus) |
| `8a43b834` | PROCESS | chore(pr): 40-pi-provider — renumbered (38 taken by cursor-resume-history) |
| `f82f03e4` | 41-baseline-schemas | test(baseline): a JSON Schema per snapshot kind; every committed snapshot checke |
| `16f20cbb` | 42-era-restore-no-machine-identity | chore(pr): era-restore.sh derives the machine identity instead of carrying it; p |
| `884d04e5` | PROCESS | chore(pr): 42-era-restore-no-machine-identity — renumbered from 39 (taken by bas |
| `b59ac9fe` | 50-one-test-per-recording | test(goldens): one test per recording and per writer — a break names its recordi |
| `4b2c8167` | 52-shipped-fidelity-bugs | docs: which published releases carry this week's fidelity bugs — all in path-cli |
| `b30de37a` | 51-webmock-prior-art | docs(webmock): prior art — ten record/replay tools against the resume cells' nee |
| `616868ed` | 45-claims-blockers-true | docs(claims): the six wip blockers are true now (copilot in meta.source; README  |
| `8fc03180` | 37-claims-as-records | model: regenerated (172 claims: 108 pinned, 64 partly, 0 false) |
| `e8b4ea70` | 53-develop-webmock-run | feat(path-cli,webmock): `path d` (baseline, webmock, model), just recipes; `webm |
| `7031c525` | 43-observed-format-facts | docs(formats): the format notes' facts generated from the recordings, and checke |
| `5b95c2fa` | 55-development-uses-path-d | docs: DEVELOPMENT.md and the changelog use path d and its just recipes |
| `12eb87b6` | 54-examples-generated | test(examples): the examples a producer can make are made by one |
| `01b41a4e` | 56-writer-contracts | test(baseline): each writer's transform declared up front as a typed IR (schemar |
| `dc5a6dc7` | 54-examples-generated | docs(pr): renumber the duplicate pr/54 records (development-uses-path-d -> 55, w |
| `add3ffee` | 38-cursor-resume-history | test(goldens): re-record the Cursor snapshots 7a93457 changed; drop the committe |
| `31b7c104` | PROCESS | chore(pr): 38-cursor-resume-history — snapshots re-recorded in 3e8f7dfceb67; the |
| `a5c46d38` | 57-codex-keeps-turn-settings | fix(codex): a Codex session projected back to Codex keeps its turn settings (app |
| `7f479829` | 58-cursor-agent-resume | feat(cursor): `path resume --harness cursor-agent` writes the Cursor CLI's chat  |
| `3ded03e8` | 59-secrets-gate-placeholders | chore(gitleaks): allow the two webmock replay placeholders in run.rs (pr/59) |
| `7f8a7034` | 60-rfc-jcs-key-order | docs(rfc): JCS key ordering is by UTF-16 code units, not UTF-8 bytes (pr/60) |
| `cfcc637b` | 61-secrets-gate-clean | chore(secrets): reword observed.rs:101 so the secrets gate passes (pr/61) |
| `f308ab58` | 62-rfc-jcs-annotations-anchored | docs(rfc): keep pr/60's rule 1 on one line so RFC.md's annotations stay anchored |
| `60eecafe` | 63-opencode-reads-are-not-writes | fix(opencode): read, list, glob and grep no longer become file changes (pr/63) |
| `c7d56661` | 64-delegations-from-tool-calls | fix(convo): every source reader builds delegations from delegation-type tool cal |
| `27f46183` | 65-stop-reason-defaults | fix(opencode, pi): a turn that ended in tool calls is not written as stop; defau |
| `54a84dc3` | 66-codex-model-provider-from-the-view | fix(codex): the written session's model_provider is the view's, else openai (pr/ |
| `b1c5ec5f` | 67-file-change-paths-relative | fix(convo): file-change keys are relative to the working directory, one key per  |
| `d7be95d7` | 68-twice-around | test(goldens): the twice-around round trip, drift declared (pr/68) |
| `42301d77` | 69-codex-empty-turn-context | fix(codex): the writer opens a turn_context only for a turn it writes lines for  |
| `e8a84f4d` | 70-copilot-reads-system-message | fix(copilot): the reader keeps system.message (pr/70) |
| `d27b9b54` | 71-convo-cwdless-no-macos-alias | fix(convo): drop the macOS alias from cwd-less paths; capture the goldens; re-an |
| `42cdebf0` | 72-agent-api-proxy-design | docs(webmock): design record for the agent API proxy (pr/72) |
| `0f9d3338` | 74-claude-multi-config-dir | feat(sync): support ':'-separated CLAUDE_CONFIG_DIR list for multi-account cache |
| `1695c8c2` | 74-claude-multi-config-dir | docs(sync): record for the multi-config-dir piece (pr/74); rustfmt its test |
| `9c8a6835` | 73-baseline-resumes-in-the-recordings-cwd | fix(goldens): the baseline resumes each projection in the recording's own direct |
| `3dfc94d5` | 80-claims-cheap-wins | docs(readme): breakdowns are never added into the token counters (pr/80) |
| `553a2136` | 80-claims-cheap-wins | build: declare rust-version 1.94 for the workspace; README says Rust 1.94+ (pr/8 |
| `2e129820` | 80-claims-cheap-wins | docs(site): the FAQ answers 'how does the format evolve?' with versioned kind UR |
| `378f9de3` | 80-claims-cheap-wins | docs(site): the quick start's resume URL is a canonical graph URL; test-quicksta |
| `07a257ff` | 80-claims-cheap-wins | test(quickstart): run the README's dead-end query verbatim on the synced cache ( |
| `673a16c9` | 80-claims-cheap-wins | test(quickstart): run the README's --source claude query verbatim and check the  |
| `f8686ba8` | 80-claims-cheap-wins | claims: Pathbase resume is tested end to end; pathbase-stores-shares-resumes is  |
| `9e8b3083` | 80-claims-cheap-wins | chore(pr): 80-claims-cheap-wins, the record for the seven claims made pass (pr/8 |
| `c1e8cd91` | 75-doc-link-caller-cwd | fix(goldens): CALLER_CWD's doc no longer links a private item (pr/75) |
| `03a9b1b0` | 86-wording-picks-16-17-18 | docs(site): cut the unsourced 'majority of the decisions' claim (consultant pick |
| `3d30b4d7` | 76-cursor-agent-reads-the-resume-dir | fix(goldens): a cursor-agent store is read with the directory it was resumed in  |
| `b8c7ac0c` | 72-agent-api-proxy-design | docs(webmock): caps are mandatory for codex and pi, which have no provider-side  |
| `497d2318` | 77-system-text-and-tool-calls-survive | fix(opencode, cursor): system-role text survives; every tool call gets a Cursor  |
| `7e2f4a3b` | 78-contract-classification-design | docs(goldens): design record for classifying every lost and gained path on the r |
| `ad00e464` | 91-baseline-live-evidence | docs(baseline): resume-results summary of the live evidence files, with what cou |
| `7c9dc488` | 81-cursor-reads-system-text | fix(cursor): the IDE reader reads system-role text back from the model history ( |
| `6a1c6b09` | 82-no-invented-resume-dir | fix(goldens): a cursor-agent read-back gets no working directory the document ne |
| `fdc3da4e` | 83-cursor-writes-no-invented-workspace | fix(cursor): the IDE writer names no workspace the document never named (pr/83) |
| `09fadd0c` | 84-classification-report | feat(goldens): the coverage map in report mode, lost and gained paths classified |
| `a6dc10bd` | 84-classification-report | feat(goldens): source bookkeeping declared per source agent, a class in the cove |
| `133cf46b` | 91-baseline-live-evidence | docs(baseline): resume results file-backed; claude own-session live, copilot blo |
| `73b6f105` | 84-classification-report | feat(goldens): renamed and synthesized classes, the unexplained paths listed; fi |
| `fd0d1098` | 85-codex-unexplained-triage | docs(goldens): triage of the codex unexplained lost paths (pr/85) |
| `2c39dc58` | 86-wording-picks-16-17-18 | docs: FAQ answers what the code decided, CI dropped from the signatures claim, p |
| `d6166d64` | 87-copilot-session-id | fix(copilot): the session id is derived from the document (UUIDv5), never clobbe |
| `605a9f5b` | 88-compaction-baseline | test(goldens): the compactions forced in claude, codex, pi and opencode become b |
| `4d7b7e1d` | 89-share-cells-findings | docs(goldens): where the four lossy share cells die (pr/89) |
| `405bf5f4` | 90-document-serializes-in-key-order | fix(toolpath): documents serialize their maps in key order; the upload body is s |
| `3cea437d` | 90-document-serializes-in-key-order | docs(toolpath): who reads the document bytes, pr/90 |
| `b9f3f78a` | 91-baseline-live-evidence | docs(baseline): copilot own-session cell loaded on bobby-emp (Copilot Free); own |
| `ae1faded` | 91-baseline-live-evidence | docs(baseline): deletion sensitivity of the 36 paths claude→claude drops, measur |

## Landed after the flatten (amended into the one wip commit)

Author commits on the pre-flatten base `ae1faded`, applied as a tree diff onto `ba795761` and folded into the single commit. Their own labels said pr/91 for the signature fix; renumbered to pr/95 because pr/91 is baseline-live-evidence.

| commit | record | subject |
|---|---|---|
| `50679d6e` | 95-claude-thinking-signature | fix(claude): a claude -> claude round trip returns every thinking signature, agent-tagged in the IR (author label pr/91) |
| `1c7afcd6` | 92-pinning-model-design | docs(baseline): one pinning model, flake-shaped agents.lock, and CI isolation (design) |
| `8bdf5553` | 93-extended-capture-prompt-design | docs(agents): extended capture prompt and its design record (draft for review) |
| `736bcfb8` | 92-pinning-model-design | docs(baseline): pinning.md names where the live keys live in CI and the click as the spend cap |
| `29fa6512` | 94-jsonl-batching-tests | docs(pr): record and patch for the #305 batching tests (Base: #305 at a84386a2; the patch applies there with `git am`) |
| `(amended)` | 97-baseline-compacted-resume | docs(baseline): a compacted session resumed live (claude loses history through toolpath; opencode re-sends it) |
| `01e76f2b` | 100-codex-permission-settings | fix(codex): a codex -> codex round trip keeps permission_profile and file_system_sandbox_policy (author label pr/95) |
| `d1e7d13e` | 96-copilot-sub-agent-messages | fix(copilot): a copilot -> copilot round trip keeps the sub-agent's assistant.message events |
| `d0e1fa84` | 101-cursor-thinking-text | fix(cursor): a Cursor IDE round trip keeps the thinking text (author label pr/97) |
| `abf172e5` | 98-shed-gained-paths | feat(goldens): the shed reports list the gained paths, not only the lost ones |
| `da0930ab` | 99-codex-forced-unexplained-triage | docs(pr): triage of the codex-forced unexplained lost paths |
| `2660ec19` | 102-codex-tool-search-items | fix(codex): a codex -> codex round trip keeps the tool-search items (author label pr/100) |
| `9bde99b0` | 103-replay-in-cargo-test | test(webmock): replay per agent in cargo test, design + tests not yet run (skip unless WEBMOCK_CELLS is set) (author label pr/101) |
| `803a015b` | 104-codex-compaction-and-base-instructions | fix(codex): a codex -> codex round trip keeps the compaction and base instructions (author label pr/102) |
| `ff34bc5e` | 105-codex-context-fields | fix(codex): a codex -> codex round trip keeps current_date, timezone, workspace_roots, thread_source (author label pr/103) |
| `acb928e3` | 106-codex-task-lifecycle | fix(codex): a codex -> codex round trip keeps the task lifecycle (author label pr/104) |
| `ac4cdd1a` | 107-replay-agents-green | test(webmock): every agent's cassette replays green through its real CLI (`just replay-agents`) |
| `1ea78730` | 113-keypaths-file-path-keys | fix(goldens): file-path keys under changes collapse to <path> (author label pr/105) |
| `60d8b504` | 114-same-format-in-adapter-shape | fix(goldens): compare the same-format round trip in the adapter's own shape (author label pr/106) |
| `054eb77d` | 115-gemini-tokens-tool-total | fix(gemini): the writer states tokens.tool and tokens.total (author label pr/107) |
| `692efb24` | 108-agent-extras-proposal | docs(pr): proposal for an agent-tagged extras bag (proposal, not built) |
| `10e054ec` | 109-kind-v1-2-0-readiness | test(goldens): baseline against a combined agent-coding-session v1.2.0 fixture |
| `3ea94f16` | 110-claude-resume-keeps-attachments-on-chain | fix(claude): a projected session keeps its attachments on the resume chain |
| `00c98c5e` | 111-act-local-workflows-design | docs(pr): design for running the Actions workflows locally with act (design, not built) |
| `e99ec948` | 112-wip-cut-design | docs(pr): design for one cut file on wip with derived slices (design, not built) |
