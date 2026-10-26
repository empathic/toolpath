# design: run the Actions workflows locally with `act` (DESIGN ONLY, nothing installed or started)

Status: design for review, docs only · Base: pr/110 · Source: Bobby 2026-10-21 "run the Actions workflows locally with act"

## What was found on this machine (read-only checks, 2026-10-22)
- `act`: NOT installed. `docker` (CLI), `colima`, `podman`, `limactl` are in the nix profile; `colima status` says "colima is not running" and there is no `~/.colima`: the VM was never created. No OrbStack, no Docker Desktop.
- `.github/workflows/` holds only `deploy-site.yml` and `release.yml`. The workflows this design is for (capture, live checks, add-harness-version, replay tests; pr/92 pinning.md section "Isolation in Actions") do not exist yet, so there is nothing to run today; this is the contract for when they do.
- Creating the colima VM downloads and runs a Linux VM image: that needs Bobby's OK first (his brief), and tell scratch-main-82 before. Not done.

## Design
1. **act from nixpkgs, pinned**: a flake input (same lock as the rest), `nix run .#act` / on PATH through the dev shell. Not `brew`, not a downloaded binary.
2. **Runtime: colima (nix-pinned) with the docker socket**, one profile `toolpath-act`: `colima start --profile toolpath-act --cpu 4 --memory 8 --arch aarch64 --vm-type vz` and `DOCKER_HOST` pointed at that profile's socket only for act. Chosen over OrbStack/Docker Desktop because it is already in the nix profile and needs no GUI app or licence. First start = the VM install = Bobby's OK.
3. **Runner image pinned by digest**: `-P ubuntu-24.04=ghcr.io/catthehacker/ubuntu:act-24.04@sha256:<digest>` (the digest recorded in `agents.lock`-style file `ci/act.lock`, bumped on purpose). Workflows may use only what that image has; steps that need a GitHub-hosted-only service (OIDC token, `actions/cache` backend, required reviewers, environment protection) are not emulated: act ignores `environment:` protection rules, so the one-agent-per-environment rule is enforced by the wrapper below, and a workflow that needs hosted-only features says so in a comment `# act: skip` and the wrapper skips that job.
4. **Isolation (the point)**: third-party agent binaries run only inside the container, never on macOS. The container gets: the repo checkout read-only plus a tmpfs work dir, no host HOME, no host sockets, `--network` limited to the model API host of the one agent (a small egress proxy container on a user-defined docker network; act itself has no egress filter, so this is the wrapper's job).
5. **Secrets: one agent per invocation**, mirroring `environment: live-<agent>`: the wrapper reads that agent's key from the read-only 1Password service account (`op read`), writes a `0600` temp file, passes `--secret-file`, and deletes the file in a `trap` on exit. Never on the command line, never in the image, never logged (act's `--secret-file` output masking on). The key is the capped/free one; codex and pi (uncapped) stay behind Bobby's click as in pinning.md.
6. **Entry point**: `just act <workflow> <agent>` (a `justfile` recipe calling one script, logic in the script not the recipe): resolves the workflow file, refuses an agent with no `live-<agent>` entry in `ci/live-envs.toml`, starts the egress proxy, runs `act -W .github/workflows/<workflow>.yml --job <job> --secret-file <tmp> --container-architecture linux/arm64 ...`, tears down. `just act replay claude` runs the replay tests for one agent without any secret.
7. **Same workflows, not copies**: act reads the real workflow files; the only local differences are the runner image mapping, the secret file, and the egress network.

## Limits said up front
- Linux containers only: macOS-specific steps (the cursor GUI check, anything needing the keychain) cannot run; arm64 Linux builds of each agent CLI must exist (claude, codex, gemini, copilot, opencode, pi, cursor-agent: to be checked per CLI; a missing one is reported, not worked around).
- act's behaviour differs from Actions in details (contexts, `services:`, reusable workflows); the first local run of a new workflow is a check of the workflow as much as of the code.
- The replay tests need a bound port: inside the container that is allowed, which also makes `just act replay <agent>` the way to run them from a seat whose own sandbox forbids bind.

## Not done / needs a decision
Nothing installed, no VM created, no workflow written, no `ci/` files. Needs: Bobby's OK to start colima (first VM), the egress-proxy choice (squid vs a 30-line allowlist proxy), and the pinned image digest (recorded when first pulled).
