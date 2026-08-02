#!/usr/bin/env bash
set -euo pipefail

test -n "${GITHUB_SHA:-}"

export CARGO_TARGET_DIR=/tmp/axiusflow-target

cargo build --locked --package axiusflow_ingest_conformance --features dpdk-native

"$CARGO_TARGET_DIR/debug/axiusflow_ingest_conformance" \
    --dpdk-vdev-lifecycle \
    /evidence/stage_1_dpdk_vdev_lifecycle_evidence_Docker.json
