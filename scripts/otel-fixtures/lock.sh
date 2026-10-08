#!/usr/bin/env bash
# Regenerate requirements.txt and requirements-openai-v2.txt (every package pinned,
# with hashes) from their .in files.
# Needs network access to PyPI only. Maintainers only.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
py="${OTEL_FIXTURES_PYTHON:-python3.12}"
cd "$here"
"$py" -m venv .venv-lock
.venv-lock/bin/python -m pip install --quiet "pip-tools==7.6.1"
# Two locks: openai-v2 2.4b0 needs util-genai<1.2b0, everything else needs >=1.2b0 (README).
for lock in requirements requirements-openai-v2; do
    CUSTOM_COMPILE_COMMAND="./lock.sh" .venv-lock/bin/pip-compile --quiet --generate-hashes \
        --allow-unsafe --strip-extras --no-emit-index-url \
        --output-file "$lock.txt" "$lock.in"
done
