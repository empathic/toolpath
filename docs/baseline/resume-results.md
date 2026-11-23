# Resume results: what real agents did with a projected session

Summary updated 2026-10-21 from the evidence files below. It states only what those files show, and every count here is backed by a
file. The evidence lives outside this repository (the fleet ops directory `~/.lobby/ops/goldens-session-2026-10-19/`); the files are named
so a reader can check each line. Cells ran on toolpath `origin/wip/bdelanghe` at `f308ab5`, `c1e8cd9`, `a6dc10bd` or `133cf46b` as each file says,
with nix-store agent binaries and a temp home, one model call per probe.

## Method (same in every file)
The probe asks, in one line, for facts only the recording's own history contains (the first file written, the replaced word, a
sub-agent's count, or the facts the recording does have). Controls: a fresh session asked the same question answers `NONE`; a
nonexistent session id is refused by the agent before any model call.

## Count

| kind | backed by a file | loaded | not run |
|---|---:|---:|---|
| own session (7 agents) | 7 | 7 | none |
| cross-agent | 5 | 5 | every other pair: no file |

The earlier "28/28 cross-agent" count is not backed by a file and is withdrawn. Own session is now 7 of 7, each backed by a file.

## Own session: the agent's recording goes through toolpath and back into the same agent

| agent (version) | probe reply | verdict | file |
|---|---|---|---|
| gemini-cli 0.47.0 | `notes.md \| scratch -> fixture \| 11` | loaded | there-and-back-gemini-opencode.md |
| opencode 1.18.18 | `notes.md \| scratch -> fixture \| 10` | loaded | there-and-back-gemini-opencode.md |
| codex-cli 0.162.1 | `notes.md \| wc -w notes.md \| 11` | loaded | there-and-back-codex-pi.md |
| pi 0.84.3 | `notes.md \| scratch -> fixture \| 68` | loaded | there-and-back-codex-pi.md |
| cursor-agent (from a Cursor IDE recording) | `notes.md \| scratch -> fixture \| 11` | loaded | there-and-back-cursor.md |
| Claude Code 2.1.296 (recording made with 2.1.132) | `ls -la \| Write blocked \| note*` | loaded | there-and-back-claude-copilot.md |
| Copilot CLI 1.0.61 (recording made with 1.0.68), account bobby-emp (Copilot Free) | `notes.md \| scratch -> fixture \| 16` | loaded | there-and-back-claude-copilot.md |

Controls named in those files:
- Fresh session: answers `NONE` in every cell that ran.
- Bad id: refused before any model call, with no key in the environment. Gemini rc 42; opencode, codex, pi, claude and copilot rc 1.

The Claude cell answered from its history with the `Read`/`Write`/`Bash` tools disabled (`--tools ""`). It used claude-haiku-4-5, $0.0150
for the probe and $0.0020 for the control (as Claude Code reported).

The Copilot cell ran on the account bobby-emp (Copilot Free), with the account's default model mai-code-1.1-flash (claude-haiku-4.5
is not available on Free), and used 2 premium requests (control and probe). The session Copilot wrote has the same `session.start`
keys as the recording. An earlier attempt on another account was refused with `402 quota_exceeded` before any model call. The same
cell also loaded through Copilot CLI's bring-your-own-key mode (Anthropic provider, `COPILOT_OFFLINE=true`, no GitHub token), which
is the fallback when the account has no quota; that run is in the same file.

## Cross-agent: one agent's recording resumed by a different agent (cross-into-cursor-and-pi-opencode.md)

| cell | verdict |
|---|---|
| claude-session -> cursor-agent | loaded (`notes.txt,other.txt \| first line of the notes`) |
| codex -> cursor-agent | loaded (`notes.md \| wc -w notes.md \| 11`) |
| pi -> cursor-agent | loaded (`notes.md \| scratch -> fixture \| 68`) |
| copilot -> cursor-agent | loaded (`notes.md \| scratch -> fixture \| 16`) |
| pi -> opencode | loaded (`notes.md \| scratch -> fixture \| 68`) |

5 cells, 5 model calls, no error, no rate limit. The fresh-session controls were not repeated there; that file points to the two
there-and-back files above.

## Older runs not counted
`resume-matrix/` holds earlier runs: `v5.sh`, `headless-gemini.sh`, and seven `screens/v4-*` logs into gemini (claude-session, codex,
copilot and pi as sources). They predate the probe-and-`NONE` method, carry no verdict, and two of them end in a `503` from the model
API. They are not counted above.

## Not found
- **Driver and raw outputs:** `there-and-back/` (drivers `tab.sh`, `tab2.sh`, `tab3.sh`, `xc.sh`; `.cmd`/`.out`/`.rc`/`.err` files;
  projected files). Not copied into the repo.
- The cursor-agent cell cleanup ("clean /tmp/.cursor/<slug> after each cell") is an instruction from Bobby; I found no file showing it done.
