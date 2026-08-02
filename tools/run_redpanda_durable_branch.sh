#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"
image="docker.redpanda.com/redpandadata/redpanda@sha256:8f7e9e4c1422baaa1a5e2a6c6c668cfe05442cb3cb476542c7dff61725e6fe31"
container_name="axiusflow-redpanda-conformance"

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
    --publish 127.0.0.1:19092:9092 \
    "$image" \
    redpanda start \
    --smp 1 \
    --memory 768M \
    --reserve-memory 0M \
    --overprovisioned \
    --node-id 0 \
    --check=false \
    --kafka-addr 0.0.0.0:9092 \
    --advertise-kafka-addr 127.0.0.1:19092

ready=false
for _ in $(seq 1 60); do
    if docker logs "$container_name" 2>&1 | grep -q "Successfully started Redpanda"; then
        ready=true
        break
    fi
    sleep 1
done
if [[ "$ready" != true ]]; then
    echo "Redpanda did not become ready" >&2
    docker logs "$container_name" 2>&1 | tail -20 >&2
    exit 1
fi

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance" \
    --redpanda-durable-branch \
    127.0.0.1:19092 \
    "$evidence_dir/stage_2_redpanda_durable_branch_evidence_Docker.json"
