# Remote session persistence & transport: the landscape

Reference for `path resume --remote`. Captures the tool landscape behind the
`--persist` (session-survival) and `--via` (connection-survival) axes, and why
reachability is delegated to `~/.ssh/config`. Companion to the design spec
(`docs/superpowers/specs/2026-07-23-remote-persistence-picker-design.md`) and the
targets runbook (`docs/agents/remote-resume-targets.md`).

## The four independent layers

Remote resume touches four *separate* concerns that **stack rather than compete**.
The classic mistake is asking one tool to cover two layers (expecting tmux to
solve roaming, or mosh to solve NAT). Keep them separate:

| Layer | Concern | In `path` |
|---|---|---|
| **Reachability** | Can I connect at all? (NAT, mesh, broker) | `~/.ssh/config` — not a flag |
| **Connection-survival** | Does the link survive roaming/sleep/IP change? | `--via ssh\|mosh\|et` |
| **Session-survival** | Does work survive a client death / reattach elsewhere? | `--persist <backend>` |
| **Workspace** | Panes / tabs / layout | a `--persist` backend (zellij) |

---

## Layer 2 — Session survival (`--persist`)

Anchors the session **server-side** so the harness keeps running under a daemon
even if the client dies, the laptop reboots, or you reattach from another machine.

### Command-wrapping (clean fit — hand them the command)
- **tmux** — the incumbent. `tmux new-session -A -s <name> '<cmd>'`; `-A`
  attaches-or-creates so re-running reattaches. Full multiplexer (panes, tabs,
  status bar, copy-mode) that happens to persist.
- **abduco** — minimal detach/attach only (from the dvtm author; splitting is
  left to dvtm). `abduco -A <name> <cmd>`. Mature, tiny; no output-replay on
  reattach, rougher resize handling.
- **dtach** — the ~20-year-old ancestor; does even less (no session listing, you
  manage socket paths). `dtach -A <sock> <cmd>`. Works, but little reason over
  abduco/shpool today.

### Layout-wrapping
- **zellij** — the modern Rust "tmux designed this decade": discoverable
  keybindings (status bar shows modes), floating/stacked panes, **resurrection**
  (serializes sessions to disk → survives a reboot, re-runs pane commands on
  attach after confirmation), WASM plugins with a capability/permission model,
  KDL layouts as declarative workspace-as-data. No "new named session running
  command X" one-liner — the supported path is a **KDL layout** with the command
  in a pane, launched `zellij --session <name> --layout <file>`.

### Attach-only (persistence, no multiplexer)
- **shpool** (Google) — "shell pool": a per-user daemon holds PTYs; `shpool
  attach <name>` reconnects or creates. Persistence *only* — no panes/tabs/status
  bar/prefix; your terminal emulator's native scrollback keeps working (a real
  ergonomic win). One client per session (reattaching steals from a dead
  connection). **No command arg** — attach always starts the configured shell,
  so it can't be handed a one-shot command; needs `loginctl enable-linger` to
  survive full logout. Best paired with a terminal/WM that owns splitting.

### Restore-after-reboot
Only **zellij** reconstructs layout + pane commands declaratively. tmux needs
`resurrect`/`continuum` bolted on. Nothing truly restores process state — it's
all "re-run the commands" reconstruction.

---

## Layer 1.5 — Connection survival (`--via`)

Keeps one client↔host link alive; does **not** anchor the session server-side.

- **mosh** — state-sync protocol (SSP) over UDP: client and server sync a
  terminal-screen state object, so it survives IP changes / sleep / roaming.
  No server-side session daemon beyond its own process, no attach-from-elsewhere,
  **no scrollback** (its biggest wart). SSH for the initial handshake, then its
  own UDP port. `mosh host -- <cmd>` (mosh owns the TTY, so no `ssh -t`).
- **Eternal Terminal (et)** — mosh's idea over **TCP** with native scrollback
  intact and transparent auto-reconnect. SSH handshake, then its own port.
  Knocks: needs its own daemon + open port; development has slowed.
- **SSH3** (misnomer, not an official successor) — SSH semantics over HTTP/3/QUIC;
  QUIC gives connection migration natively (mosh's property, standardized at the
  transport). Researchy; not dependable yet.

In `path`: `--via` swaps the launch client binary; the persistence wrapping is
identical. v1 implements only `ssh`; `mosh`/`et` are reserved (seam in place).

---

## Layer 1 — Reachability (delegated to `~/.ssh/config`)

These dissolve the *reachability* problem, not the reconnect problem. **None are
a `path` flag** — they're how the host resolves/connects, expressed as a `Host`
block (`ProxyCommand`, `ProxyJump`, or a mesh hostname). Because `--remote`
accepts a `~/.ssh/config` alias, resume rides all of them for free.

**Mesh / overlay (no public inbound ports):**
- **Tailscale SSH** — WireGuard mesh where `tailscaled` terminates SSH; auth via
  your IdP, no keys to manage, no open ports. WireGuard endpoint migration keeps
  the *tunnel* alive across IP changes (a plain TCP session inside can still
  notice long outages — run mosh/shpool *over* tailscale, not instead).
- **WireGuard / Nebula / NetBird** — same idea, less product around it.

**Rendezvous / brokered (outbound-only from the host):**
- **Cloudflare Tunnel + `cloudflared access ssh`** — outbound tunnel, SSH brokered
  through Cloudflare's edge, optional access policies.
- **upterm / tmate** — reverse-tunnel a session through a relay for sharing /
  reaching a NATed box (tmate is literally a tmux fork with a rendezvous server).
- **Teleport / Boundary** — enterprise: certificate-based, audited,
  session-recorded access brokers. Heavy, but the only family where the transport
  itself has an attribution story (every session tied to a signed identity cert,
  recorded, policy-checked before the PTY is granted).

**Browser-as-client:**
- **ttyd / gotty / sshx** — expose a terminal over WebSocket/HTTP (sshx adds
  multiplayer + E2E encryption). "Any device, zero client install"; run behind one
  of the mesh/broker layers, not raw.

### Known limitation
libssh2 (the probe/ship half of `--remote`) **does not honor `ProxyCommand`/
`ProxyJump`** — it dials a raw TCP socket. A host reachable *only* through a
broker/jump (Cloudflare Tunnel, bastion-only) will launch fine (the `ssh` CLI
honors the config) but **fail at the ship step**. Candidate mitigation: fall back
to shipping over the `ssh` CLI (`ssh host 'cat > dest'` / scp) when the libssh2
dial fails and a `ProxyCommand` is configured. Tracked as an open question.

---

## Composition guidance

The layers stack. Pick per concern:

- **Reachability:** ssh-config alias over Tailscale (identity-attributed) or a
  broker; plain host if directly routable.
- **Connection survival:** mosh/et if you roam; skip if the link is stable.
- **Session survival:** tmux (ubiquitous), zellij (workspace + resurrection),
  shpool (minimal, terminal-native scrollback), abduco/dtach (tiny).

For agent runs in a claude-box-style setup, a strong pairing is **Tailscale SSH +
shpool**: identity-attributed transport (auth logged against a signed identity,
not a static `authorized_keys`) with server-side session persistence — versus
mosh, which adds a second auth path and a long-lived UDP daemon to the attack
surface. When you want the workspace layer too, that's **zellij** instead of
shpool.

The niche bifurcates cleanly and there's no middle tool: transport persistence
(mosh, et), session persistence (shpool, abduco), workspace persistence (zellij,
tmux). Reachability is its own axis on top.
