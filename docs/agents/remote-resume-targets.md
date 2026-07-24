# Remote resume targets: exe.dev and Sprite

Runbook for standing up an SSH-reachable box that `path resume --remote` can
target. `path resume --remote <ssh-url-or-alias>` shells out to the real `ssh`
binary for the interactive launch, and uses libssh2 (raw TCP dial) for the
preflight/ship half — so a target must be reachable as a plain `ssh` host with
`path` **and** `claude` installed.

See also: `crates/path-cli/src/cmd_resume.rs` module docs (the remote protocol)
and the design spec `docs/superpowers/specs/2026-07-23-remote-persistence-picker-design.md`.

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

**Provision + prepare a VM**
```bash
ssh exe.dev new --name=pathremote --json      # -> pathremote.exe.xyz (Ubuntu 24.04 x86_64, user exedev)
# claude ships pre-installed at /usr/local/bin/claude

# install path on the box:
ssh pathremote.exe.xyz 'curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal'
ssh pathremote.exe.xyz 'sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config libssl-dev cmake'
tar czf - --exclude=target --exclude=.git . | ssh pathremote.exe.xyz 'rm -rf ~/toolpath && mkdir -p ~/toolpath && tar xzf - -C ~/toolpath'
ssh pathremote.exe.xyz 'cd ~/toolpath && source ~/.cargo/env && cargo build --release -p path-cli'   # ~3m on 2 CPUs
ssh pathremote.exe.xyz 'sudo cp ~/toolpath/target/release/path /usr/local/bin/path'
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
`ssh exe.dev ls --json` to list. Rebuild + recopy `path` if the branch's
remote-resume code changes materially.

---

## Sprite (Fly.io) — 📋 documented, not yet stood up

Sprite is authed (org `robert-delanghe`, card on file, token minted) but **does
not fit the SSH contract out of the box**: it exposes `sprite console` /
`sprite exec` / `sprite proxy`, not a raw `ssh` endpoint. The lift is to give it
one.

### The endpoint decision (do option A)

- **A — sshd inside + `sprite proxy` → `localhost:<port>`** *(recommended)*.
  Dialing `localhost:<port>` is a **raw TCP socket**, which libssh2 (the
  preflight/ship half) handles — so both ship and launch work.
- **B — `ProxyCommand` shim over `sprite exec`**. Hits a known limitation:
  **libssh2 does not honor `ProxyCommand`**, so the interactive launch would work
  but the file-ship step fails. Do not use until the ssh-CLI ship fallback exists
  (tracked as an open question in the design spec).

### Steps (option A)

1. **Create a sprite**
   ```bash
   sprite create -o robert-delanghe pathremote
   ```
2. **Install + start sshd inside it** (via `sprite console` / `sprite exec`)
   ```bash
   sprite exec -- sudo apt-get update -qq
   sprite exec -- sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssh-server
   sprite exec -- sudo sed -i 's/#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
   sprite exec -- 'mkdir -p ~/.ssh && printf "%s\n" "$(cat ~/.ssh/id_ed25519_signing.pub)" >> ~/.ssh/authorized_keys'  # push your pubkey
   sprite exec -- sudo service ssh start
   ```
3. **Forward the SSH port to localhost**
   ```bash
   sprite proxy 2222   # maps a local port to the sprite's :22 (confirm exact mapping syntax with `sprite proxy --help`)
   ```
4. **Install `path` + `claude`** — same recipe as exe.dev, but over the tunnel
   (`ssh -p 2222 user@localhost …`). Then OAuth `claude` in a real terminal:
   ```bash
   ssh -t -p 2222 user@localhost claude
   ```
5. **Verify**
   ```bash
   path resume <cache-id> --remote ssh://user@localhost:2222 --harness claude
   ```

### Open items for Sprite
- Confirm `sprite proxy` direction/port-mapping syntax (`sprite proxy --help`).
- The proxy tunnel must stay up for the duration of the resume (ship + launch).
- Alternative to sshd: if a future `path` gains a `sprite exec`-based transport
  (a `--via sprite` seam), Sprite could skip sshd entirely — not planned.

---

## Quick reference

| Target | SSH fit | `path` | `claude` | Status |
|---|---|---|---|---|
| exe.dev `pathremote.exe.xyz` | native | ✅ built (this branch) | ✅ OAuth'd | verified working |
| Sprite | needs sshd + `sprite proxy` | ⬜ | ⬜ | documented, not stood up |
