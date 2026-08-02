#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"

if [[ ! -x "$repo_root/target/debug/axiusflow_ingest_conformance" ]]; then
    echo "build first: cargo build --package axiusflow_ingest_conformance --features quic" >&2
    exit 1
fi

cargo build --locked --package axiusflow_authorization_service

mkdir -p "$evidence_dir"

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance" \
    --authorization-boundary \
    "$repo_root/target/debug/axiusflow_authorization_service" \
    "$evidence_dir/stage_2_authorization_boundary_evidence_Linux.json"
