#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"
image="minio/minio@sha256:14cea493d9a34af32f524e538b8346cf79f3321eff8e708c1e2960462bd8936e"
container_name="axiusflow-minio-conformance"
access_key="axiusflowconformance"
secret_key="axiusflow-conformance-secret-2026"

if [[ ! -x "$repo_root/target/debug/axiusflow_ingest_conformance" ]]; then
    echo "build first: cargo build --package axiusflow_ingest_conformance --features redpanda" >&2
    exit 1
fi

mkdir -p "$evidence_dir"

if [[ -z "${DOCKER_CONFIG:-}" && ! -w "${HOME}/.docker" ]]; then
    DOCKER_CONFIG="$(mktemp -d)"
    export DOCKER_CONFIG
fi

cleanup() {
    docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker run --detach --name "$container_name" \
    --publish 127.0.0.1:19000:9000 \
    --env MINIO_ROOT_USER="$access_key" \
    --env MINIO_ROOT_PASSWORD="$secret_key" \
    "$image" \
    server /data --address :9000

ready=false
for _ in $(seq 1 30); do
    if docker logs "$container_name" 2>&1 | grep -q "API:"; then
        ready=true
        break
    fi
    sleep 1
done
if [[ "$ready" != true ]]; then
    echo "MinIO did not become ready" >&2
    docker logs "$container_name" 2>&1 | tail -20 >&2
    exit 1
fi

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance" \
    --s3-raw-capture \
    127.0.0.1 \
    19000 \
    "$access_key" \
    "$secret_key" \
    "$evidence_dir/stage_2_raw_s3_capture_evidence_Docker.json"
