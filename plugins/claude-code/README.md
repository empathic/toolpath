# Toolpath plugin for Claude Code

Slash commands for the [Toolpath](https://toolpath.net) `path` CLI, with no
manual install step: the plugin resolves the binary on first use — preferring
an existing install, otherwise downloading the latest release and installing
it globally to `~/.local/bin`.

## Install

Inside Claude Code:

```
/plugin marketplace add empathic/toolpath
/plugin install path@toolpath
```

## Commands

| Command | Description |
|---------|-------------|
| `/path:share` | Share an agent session to Pathbase and get a link. With no arguments it shares the current conversation; pass a hint to pick another session, `--harness <name>` for another harness, and `--anon` / `--public` / `--repo` / `--name` / `--url` to control the upload. |
| `/path:query` | Ask questions about your local agent-session history. Takes plain English (translated to a jaq filter) or a jaq filter verbatim, plus `--source` / `--project` scoping. |
| `/path:resume` | Bring a shared session (Pathbase URL, `owner/repo/slug`, file, or cache id) into this project and get the exact resume step — `/resume <id>` here, or `claude -r <id>` from a terminal. |
| `/path:link-pr` | Share the current conversation and append the Pathbase link to a PR description — the PR you name, or the current branch's. |

## Tags

Type a line of the form

```
tag: decision auth
```

on its own to label the message you just read. Tags are separated by spaces
or commas and are otherwise opaque (`bug:auth` is one tag). The plugin's
`UserPromptSubmit` hook (`hooks/hooks.json`, `scripts/tag-hook.sh`) blocks
the line before it reaches the model, so it costs no tokens and adds nothing
to the context; Claude Code records the block in the session log, and `path`
derives the tags onto the previous message's step (`meta.tags`) when the
session is imported, shared, or queried:

```
path query 'map(select(.meta.tags // [] | index("decision")))'
```

The same line typed into any other harness `path` reads (codex, gemini, pi,
…) is recognised too; there it reaches the model as an ordinary message.

## How the binary is bundled

The commands run the CLI through `scripts/ensure-path.sh`, which resolves in
order (the tag hook never resolves a binary):

1. `$TOOLPATH_BIN`, when set — an explicit override that wins over
   everything else. Point it at a working tree's `target/release/path` to
   run the plugin against a build you have not installed. Set but unusable
   is a warning, not a silent fall-through.
2. `path` already on `PATH`, if it identifies as the Toolpath CLI (an
   existing cargo / Homebrew / install.sh install is always preferred)
3. `~/.local/bin/path` (or `$TOOLPATH_INSTALL_DIR/path`)
4. `~/.toolpath/bin/path` (or `$TOOLPATH_CONFIG_DIR/bin/path`)
5. Otherwise it downloads the latest GitHub release for your platform,
   verifies the sha256 checksum, and installs to `~/.local/bin` — falling
   back to `~/.toolpath/bin` if an unrelated binary named `path` already
   claims the `~/.local/bin` name.

The install is global: after first use, `path` works from your own shell too
(the script prints a PATH hint if `~/.local/bin` isn't on it). Platforms
without prebuilt binaries (Intel Mac, native Windows) get a
`cargo install path-cli` recipe instead.

## Development

Run Claude Code with the local plugin:

```bash
claude --plugin-dir ./plugins/claude-code
```

Validate manifests and exercise the bootstrap logic offline:

```bash
scripts/test-plugin.sh   # from the repo root
```
