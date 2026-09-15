#!/usr/bin/env bash
# Live round trip for `p export/list/import object` against a real S3
# endpoint. With no arguments it starts a MinIO container, creates a
# bucket, runs the ignored live test, and tears the container down.
# Point it at an existing endpoint instead with the environment:
#
#   TOOLPATH_S3_TEST_BUCKET=my-bucket AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… \
#     [TOOLPATH_S3_TEST_ENDPOINT=https://…] scripts/test-object-storage-live.sh --existing
#
# Preconditions for the MinIO path: `docker` and `aws` on PATH.

set -euo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "${_root}"

if [[ "${1:-}" == "--existing" ]]; then
    : "${TOOLPATH_S3_TEST_BUCKET:?set TOOLPATH_S3_TEST_BUCKET}"
    : "${AWS_ACCESS_KEY_ID:?set AWS_ACCESS_KEY_ID}"
    : "${AWS_SECRET_ACCESS_KEY:?set AWS_SECRET_ACCESS_KEY}"
    cargo test -p path-cli --test object_storage live_s3_round_trip -- --ignored --nocapture
    exit 0
fi

command -v docker >/dev/null || { echo "docker not on PATH" >&2; exit 64; }
command -v aws >/dev/null || { echo "aws CLI not on PATH (needed to create the bucket)" >&2; exit 64; }

_container="toolpath-minio-$$"
_port=9000
# Docker Hub's minio/minio no longer allows anonymous pulls; quay.io/minio/minio does.
docker run -d --rm --name "${_container}" -p "${_port}:9000" \
    -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
    quay.io/minio/minio server /data >/dev/null
trap 'docker stop "${_container}" >/dev/null 2>&1 || true' EXIT

export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export AWS_REGION=us-east-1
export TOOLPATH_S3_TEST_ENDPOINT="http://127.0.0.1:${_port}"
export TOOLPATH_S3_TEST_BUCKET="toolpath-live"

for _attempt in $(seq 1 30); do
    if curl -sf "${TOOLPATH_S3_TEST_ENDPOINT}/minio/health/live" >/dev/null; then
        break
    fi
    sleep 1
done
aws --endpoint-url "${TOOLPATH_S3_TEST_ENDPOINT}" s3 mb "s3://${TOOLPATH_S3_TEST_BUCKET}" >/dev/null

cargo test -p path-cli --test object_storage live_s3_round_trip -- --ignored --nocapture
