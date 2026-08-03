#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"
workdir="$(mktemp -d)"
plane_address="127.0.0.1:21042"

if [[ ! -x "$repo_root/target/debug/axiusflow_ingest_conformance" ]]; then
    echo "build first: cargo build --package axiusflow_ingest_conformance --features quic" >&2
    exit 1
fi

cargo build --locked --package axiusflow_market_data_plane
cargo build --locked --package axiusflow_ingest_conformance --features quic

mkdir -p "$evidence_dir"
fuser -k 21042/tcp >/dev/null 2>&1 || true

plane_log="$(mktemp -t axiusflow-entitlement-plane-XXXXXX.log)"
plane_pid=""
cleanup() {
    if [[ -n "$plane_pid" ]]; then
        kill "$plane_pid" >/dev/null 2>&1 || true
    fi
    rm -rf "$workdir"
    rm -f "$plane_log"
}
trap cleanup EXIT

"$repo_root/target/debug/axiusflow_ingest_conformance" --mint-jwks "$workdir"

"$repo_root/target/debug/axiusflow_market_data_plane" \
    --listen "$plane_address" \
    --products BTC-USD \
    --jwks "$workdir/lane_jwks.json" \
    --policy "$workdir/lane_policy.json" \
    --resnapshot-seconds 2 \
    > "$plane_log" 2>&1 &
plane_pid=$!

ready=false
for _ in $(seq 1 60); do
    if grep -q "listener_started=true" "$plane_log" 2>/dev/null; then
        ready=true
        break
    fi
    sleep 1
done
if [[ "$ready" != true ]]; then
    echo "market data plane did not become ready" >&2
    cat "$plane_log" >&2
    exit 1
fi

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance" \
    --entitlement-enforcement \
    "$plane_address" \
    "$workdir" \
    2 \
    "$evidence_dir/stage_2_entitlement_enforcement_evidence_Linux.json"
