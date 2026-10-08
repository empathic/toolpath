#!/usr/bin/env python3
"""Compare a fresh capture tree (argv[1]) with the committed fixtures, byte for byte
(the JSON files and the exporter request bodies, *.binpb)."""
import sys
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent.parent / "test-fixtures" / "otel"


def main():
    fresh = Path(sys.argv[1])
    bad = []
    files = sorted([*fresh.rglob("*.json"), *fresh.rglob("*.binpb")])
    if not files:
        sys.exit("verify: the fresh capture tree is empty")
    for new in files:
        rel = new.relative_to(fresh)
        old = FIXTURES / rel
        if not old.exists() or old.read_bytes() != new.read_bytes():
            bad.append(str(rel))
    if bad:
        sys.exit("verify: re-capture differs from the committed fixtures:\n  " + "\n  ".join(bad))
    print(f"verify: {len(files)} files identical")


if __name__ == "__main__":
    main()
