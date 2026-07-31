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

## How the binary is bundled

Both commands run the CLI through `scripts/ensure-path.sh`, which resolves in
order:

1. `path` already on `PATH`, if it identifies as the Toolpath CLI (an
   existing cargo / Homebrew / install.sh install is always preferred)
2. `~/.local/bin/path` (or `$TOOLPATH_INSTALL_DIR/path`)
3. `~/.toolpath/bin/path` (or `$TOOLPATH_CONFIG_DIR/bin/path`)
4. Otherwise it downloads the latest GitHub release for your platform,
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
