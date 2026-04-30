#!/usr/bin/env python3
"""Pre-publish check: verify that already-published workspace crates'
on-registry manifests still declare requirements compatible with the
sibling versions we're about to publish (or have already published).

Catches the failure mode where bumping a foundation crate (e.g. toolpath
0.2 -> 0.4) silently breaks a satellite (toolpath-convo 0.7.0) whose
published manifest still pins the old major. Without this check the
publish goes green at dry-run time (already-published satellites are
skipped from dry-run, and crates that depend on something also being
published in the same run are deferred), and only blows up at real
publish — when the live registry view is mixed back in and drags two
majors of the foundation crate into the same graph.

Usage:
    python3 scripts/check-registry-compat.py \\
        <name>=<version> [<name>=<version> ...] \\
        --skip <name> [<name> ...]

Each `name=version` lists a workspace crate and its current local
version. `--skip <name>` flags crates that are already on the registry
at that version (i.e. would be skipped by cargo publish). For each
skipped crate we fetch its registry manifest's dependencies and verify
that any workspace-sibling requirements are satisfied by the workspace
versions we're about to ship.

Exits 0 if all checks pass, 1 on any incompatibility. Stdout is the
human-readable per-crate report; failures print remediation guidance.
"""
import json
import sys
import urllib.error
import urllib.request


def parse_caret_or_compat(req: str):
    """Parse a cargo dep req. Returns (lo, hi) tuple of (M, m, p) bounds,
    or None when the requirement is in a shape we can't safely interpret
    (in which case the caller should not flag a failure).

    Cargo defaults a bare `X.Y.Z` to `^X.Y.Z`. We support that and
    explicit caret. Anything else (`>=`, `~`, `=`, multi-constraint) we
    skip — false negatives here are preferable to false positives.
    """
    s = req.strip()
    if not s or s == "*":
        return None
    if s.startswith("^"):
        s = s[1:].strip()
    elif s.startswith(("=", ">", "<", "~")):
        return None
    if "," in s:
        return None
    parts = s.split(".")
    try:
        major = int(parts[0])
        minor = int(parts[1]) if len(parts) > 1 else 0
        patch = int(parts[2]) if len(parts) > 2 else 0
    except (ValueError, IndexError):
        return None
    if major > 0:
        return ((major, minor, patch), (major + 1, 0, 0))
    if minor > 0:
        return ((0, minor, patch), (0, minor + 1, 0))
    return ((0, 0, patch), (0, 0, patch + 1))


def parse_version(v: str):
    parts = v.split(".")
    try:
        return (
            int(parts[0]),
            int(parts[1]) if len(parts) > 1 else 0,
            int(parts[2].split("-")[0]) if len(parts) > 2 else 0,
        )
    except (ValueError, IndexError):
        return None


def satisfies(req: str, ver: str) -> bool:
    rng = parse_caret_or_compat(req)
    if rng is None:
        return True  # unparseable / unsupported; don't fail spuriously
    v = parse_version(ver)
    if v is None:
        return True
    lo, hi = rng
    return lo <= v < hi


def fetch_registry_deps(crate: str, version: str):
    url = f"https://crates.io/api/v1/crates/{crate}/{version}/dependencies"
    req = urllib.request.Request(
        url, headers={"User-Agent": "toolpath-release-check"}
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    return data.get("dependencies", [])


def main() -> int:
    args = sys.argv[1:]
    workspace: dict[str, str] = {}
    skip: list[str] = []
    mode = "ws"
    for a in args:
        if a == "--skip":
            mode = "skip"
            continue
        if mode == "ws":
            name, _, ver = a.partition("=")
            workspace[name] = ver
        else:
            skip.append(a)

    failed: list[tuple[str, str, list[str]]] = []
    for crate in skip:
        version = workspace.get(crate)
        if not version:
            print(f"    {crate}: warn, --skip but no workspace version supplied")
            continue
        deps = fetch_registry_deps(crate, version)
        if deps is None:
            print(f"    {crate} {version}: warn, registry returned 404")
            continue
        issues = []
        for d in deps:
            if d.get("kind") != "normal":
                continue
            dep_name = d["crate_id"]
            if dep_name not in workspace:
                continue
            req = d["req"]
            target_ver = workspace[dep_name]
            if not satisfies(req, target_ver):
                issues.append(
                    f"      {dep_name} {req} (registry) vs {target_ver} (workspace)"
                )
        if issues:
            print(f"    {crate} {version}: INCOMPATIBLE")
            for line in issues:
                print(line)
            failed.append((crate, version, issues))
        else:
            print(f"    {crate} {version}: ok")

    if failed:
        print()
        names = ", ".join(c for c, _, _ in failed)
        print(f"registry compatibility check failed for: {names}")
        print()
        print("the on-registry manifest of these crates declares sibling-dep")
        print("requirements that do not match the workspace's current versions.")
        print("publishing as-is would leave consumers with two majors of the")
        print("workspace dep in the same graph (E0308 type mismatch at compile")
        print("time). bump each failing crate to a new version so a fresh,")
        print("aligned manifest reaches the registry, then re-run.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
