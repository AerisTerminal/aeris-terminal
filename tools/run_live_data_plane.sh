#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$repo_root/.cache/evidence"

if [[ ! -x "$repo_root/target/debug/axiusflow_ingest_conformance" ]]; then
    echo "build first: cargo build --package axiusflow_ingest_conformance --features quic" >&2
    exit 1
fi

cargo build --locked --package axiusflow_market_data_plane

mkdir -p "$evidence_dir"

plane_address="127.0.0.1:21042"
plane_log="$(mktemp -t axiusflow-plane-XXXXXX.log)"
"$repo_root/target/debug/axiusflow_market_data_plane"     --listen "$plane_address"     --products BTC-USD     > "$plane_log" 2>&1 &
plane_pid=$!
cleanup() {
    kill "$plane_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 60); do
    if grep -q "listener_started=true" "$plane_log" 2>/dev/null; then
        break
    fi
    sleep 1
done
grep -q "listener_started=true" "$plane_log"

GITHUB_SHA="$(git -C "$repo_root" rev-parse HEAD)"
export GITHUB_SHA
"$repo_root/target/debug/axiusflow_ingest_conformance"     --live-data-plane     "$plane_address"     BTC-USD     90     "$evidence_dir/stage_2_live_data_plane_evidence_Linux.json"
