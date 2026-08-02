#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"
image="postgres@sha256:a426e44bac0b759c95894d68e1a0ac03ecc20b619f498a91aae373bf06d8508d"
container_name="axiusflow-postgres-conformance"
user="axiusflow"
password="axiusflow-conformance-2026"
database="axiusflow_conformance"

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
    --publish 127.0.0.1:15432:5432 \
    --env POSTGRES_USER="$user" \
    --env POSTGRES_PASSWORD="$password" \
    --env POSTGRES_DB="$database" \
    "$image"

ready=false
for _ in $(seq 1 60); do
    if docker exec "$container_name" pg_isready -U "$user" -d "$database" >/dev/null 2>&1; then
        ready=true
        break
    fi
    sleep 1
done
if [[ "$ready" != true ]]; then
    echo "PostgreSQL did not become ready" >&2
    docker logs "$container_name" 2>&1 | tail -20 >&2
    exit 1
fi

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance" \
    --postgres-persistence \
    127.0.0.1 \
    15432 \
    "$user" \
    "$password" \
    "$database" \
    "$evidence_dir/stage_2_postgres_persistence_evidence_Docker.json"
