# Remote resume target: exe.dev

Runbook for standing up an SSH-reachable box that `path resume --remote` can
target. `path resume --remote <ssh-url-or-alias>` shells out to the real `ssh`
binary for the interactive launch and uses libssh2 (raw TCP dial, with an
`ssh`-CLI fallback for proxied/bastion hosts) for the preflight/ship half. The
host does all the toolpath work and ships the finished session file, so the
target needs only `sshd` + `claude` — **no `path` on the remote**.

See `crates/path-cli/src/cmd_resume.rs` module docs for the full remote protocol.

---

## exe.dev — ✅ verified working

exe.dev is SSH-native (VMs at `<vm>.exe.xyz` support full SSH), so it drops
straight into the remote-resume contract. This is the reference setup.

**Account / key**
- Account `me@robertdelanghe.dev`. Auth is by SSH key; registration is a one-time
  interactive `ssh exe.dev` in a **real terminal** (the CLI `!` shell has no TTY
  and can't complete it).
- `~/.ssh/config` pins the key:
  ```
  Host exe.dev *.exe.xyz
    IdentitiesOnly yes
    IdentityFile ~/.ssh/id_ed25519_signing
    StrictHostKeyChecking accept-new
  ```

**Provision a VM** (no `path` install needed — the host ships the session):
```bash
ssh exe.dev new --name=pathremote --json      # -> pathremote.exe.xyz (Ubuntu 24.04 x86_64, user exedev)
# claude ships pre-installed at /usr/local/bin/claude
```
Optionally install a persistence backend if you want `--persist`:
```bash
ssh pathremote.exe.xyz 'sudo apt-get update -qq && sudo apt-get install -y -qq tmux'   # or dtach
```

**Authenticate claude** (real terminal — OAuth device flow):
```bash
ssh -t pathremote.exe.xyz claude   # complete the login URL in your browser
```

**Verify**
```bash
path resume <cache-id> --remote ssh://pathremote.exe.xyz --harness claude
# also works via a config alias: --remote pathbox
```

**Housekeeping:** persistent VM = billing. `ssh exe.dev rm pathremote` to delete;
`ssh exe.dev ls --json` to list.

**Proxied/bastion hosts:** a target reachable only through a `ProxyJump`/
`ProxyCommand` works too — the ship falls back from libssh2 to the `ssh` CLI
(which honors `~/.ssh/config`) automatically.
