# Persistent VM/Sandbox Platforms for AI Agents — 2026 Landscape

> Research note, 2026-07-24. Compiled from a fan-out web-research pass (25 sources
> fetched, 119 claims extracted, 23 confirmed / 2 refuted after 3-vote adversarial
> verification). Findings rest on vendor/author self-descriptions and blog roundups,
> **not** independent efficacy benchmarks — see caveats. Relevant to the effect-mediation
> thesis behind `clash` and possession-based brokering.

## The two-axis frame

The field splits along two independent axes:

1. **Isolation + persistence** — how the sandbox walls off untrusted agent code, and
   whether it keeps state between runs.
2. **Effect mediation** — how secrets and side-effects are governed. This is where the
   *possession-based vs. deny-at-boundary* distinction turns out to be a real, named
   schism in the 2026 literature.

## Axis 1 — Isolation & persistence

| Product | Isolation | Persistence model | GPU | Notes |
|---|---|---|---|---|
| **E2B** | Firecracker microVM, dedicated per-sandbox kernel (~80–200ms boot) | **Pet** — pause/resume *same* sandbox, ~1s resume, full mem+FS restore | ❌ (Firecracker limitation) | Persistence is public beta with a known repeated-resume data-loss bug (GitHub #884) |
| **Fly.io Sprites** | Firecracker persistent VM | **Pet** — disk state (files, packages, repos) survives days; checkpoint/restore ~1s (~300ms COW checkpoints) | — | Launched Jan 2026. Caveat: only *disk* persists idle — RAM/running processes don't |
| **Modal** | gVisor (user-space kernel over containers) | **Cattle** — snapshot restores as a *new* sandbox | ✅ incl. full-VRAM memory snapshots (weights + CUDA kernels/graphs) | H100 $0.001097/s, A100-80GB $0.000694/s, B200 $0.001736/s (1.5–1.75× region multipliers) |
| **Daytona** | Docker/OCI default, optional **Kata** for VM-grade | ~27ms warm/fork (sub-90ms cold headline) | — | SOC 2 Type I, HIPAA, GDPR; customer-VPC/on-prem. *Medium* confidence — metrics partly marketing |

**Coverage gap, stated honestly:** the verify pass produced no surviving claims for
**exe.dev, shellbox, Runloop, Vercel Sandbox, Cloudflare Containers, Blaxel,
Northflank**, nor the self-hosted options (Lima, Multipass, cloud-hypervisor, bwrap).
They appeared in sources but nothing about them cleared 3-vote verification. Treat them
as uncovered here, not absent.

## Axis 2 — Effect mediation (the analytical core)

Three genuinely distinct patterns, now with academic backing:

### (1) Deny-at-kernel-boundary syscall interception — agentsh, Sandlock

- **agentsh**: operates *below the model layer*, intercepting real syscalls via
  **seccomp user-notify, Landlock, FUSE, eBPF, ptrace**. Ships as a Go binary replacing
  `/bin/bash`. Self-scores "100% enforcement" on Linux (its own number). **Handles
  secrets by *filtering*** — minimal env (PATH/LANG/TERM/HOME), stripping secret keys,
  denying `~/.ssh/**` and `~/.aws/**` paths, DLP redaction, blocking metadata endpoints.
  Allowlist/denylist, not withholding.
- **Sandlock** (arXiv 2605.26298, ~May 2026): static policy compiled to kernel-enforced
  Landlock/seccomp-bpf + a narrow runtime supervisor. Pitches ~5ms startup vs Docker
  ~300ms for agents running many short-lived commands, and does *reversible* filesystem
  effects via copy-on-write (`--dry-run`, commit-or-discard).

### (2) Possession-based effect mediation / broker-or-nothing — CapSeal, Wardgate, Warden, Shuru

- **CapSeal** (arXiv 2604.16762, April 2026) is the canonical academic statement of the
  possession-based model. The agent **never obtains the secret** — it requests
  session-bound capability handles that are "cryptographically useless outside the
  specific channel and session," and a local trusted broker executes all
  credential-bearing actions. Secrets are "strictly confined to the broker-side
  execution path."
- Its rationale: **"Bearer credentials are unsafe when handed to a component that is both
  semantically steerable and externally connected."** Because a prompted model can
  transform, paraphrase, or exfiltrate a credential through tool params/logs/follow-on
  instructions, intercepting misuse *after* exposure is insufficient — the only robust
  guarantee is that the agent physically cannot reach the secret.
- **Host-boundary injection brokers** (Wardgate, Warden, Shuru) implement the same
  principle operationally: agent authenticates with its own key/JWT/SPIFFE SVID, real
  credentials injected server-side per-request; Shuru proxies placeholder tokens into the
  VM and substitutes real values only on outbound HTTPS — "secrets never enter the VM."

### (3) MicroVM kernel isolation

Firecracker/gVisor/Kata (Axis 1) — orthogonal; walls off the *process*, says nothing
about secrets.

## The distinction worth writing up

agentsh and a possession-based broker look identical at skim speed — both "policy-enforced
effect mediation distinct from isolation" — but the trust root differs:

- **agentsh = deny-at-boundary.** The secret is *present* in the environment; policy tries
  to catch every path it could leak through (env, files, network, subprocess).
  Completeness of the denylist is the trust assumption.
- **CapSeal / broker-or-nothing = possession-based.** The secret is *never present*.
  There's no leak path to enumerate because there's nothing to leak.

**The adversarial verifier drew blood on exactly the right claim:** the assertion that
*agentsh deterministically blocks injection-triggered exfiltration at the kernel level*
was **REFUTED 0-3**. That is the empirical crack under the deny-at-boundary approach — its
efficacy against a *motivated, steerable* agent is unverified, which is precisely the gap
CapSeal's argument exploits and a possession model closes by construction.

## Caveats

- All governance findings rest on vendor/author self-descriptions, **not independent
  efficacy benchmarks**. CapSeal, Wardgate, Warden, Shuru appear to be research/early-stage,
  not proven at scale.
- Fast-moving field: Sprites (Jan 2026), papers (Apr–May 2026); pricing/boot-times/beta
  features will drift.
- Daytona's headline metrics (27ms, "out of the box" SOC 2 Type I) are partly marketing
  framing.

## Open questions

1. Does possession-based mediation actually beat interception in practice, or does it just
   relocate the trust boundary to the broker's *intent-evaluation policy* — which is itself
   agent-influenced? No efficacy benchmark survived verification for either side.
2. Can microVM isolation + in-VM syscall governance + host-boundary broker compose as
   complementary layers, and at what latency cost for many short-lived commands?
3. Where do the uncovered products (exe.dev, shellbox, Runloop, Vercel, Cloudflare, Blaxel,
   Northflank) sit — and does any already combine microVM isolation *with* a
   possession-based broker?

## Key sources

- CapSeal: Capability-Sealed Secret Mediation — <https://arxiv.org/html/2604.16762v1> (primary)
- Sandlock — <https://arxiv.org/html/2605.26298v1> (primary), <https://github.com/multikernel/sandlock>
- agentsh — <https://www.agentsh.org/docs/secure-sandbox/>, <https://github.com/canyonroad/agentsh>
- awesome-agent-runtime-security — <https://github.com/bureado/awesome-agent-runtime-security>
- E2B persistence — <https://e2b.dev/docs/sandbox/persistence>
- Modal GPU memory snapshots — <https://modal.com/blog/gpu-mem-snapshots>
- Fly.io Sprites — <https://www.sdxcentral.com/news/flyio-debuts-sprites-persistent-vms-that-let-ai-agents-keep-their-state/>, <https://sprites.dev>
- Sandbox infra roundup — <https://agentmarketcap.ai/blog/2026/04/07/ai-agent-sandbox-infrastructure-e2b-modal-daytona-fly-machines-secure-code-execution>
