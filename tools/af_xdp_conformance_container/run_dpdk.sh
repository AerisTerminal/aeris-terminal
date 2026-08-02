#!/usr/bin/env bash
set -euo pipefail

report_path="/evidence/stage_1_dpdk_vdev_lifecycle_evidence_Docker.json"

test -n "${GITHUB_SHA:-}"
test -x /workspace/target/debug/axiusflow_ingest_conformance

export AXIUSFLOW_DPDK_DRIVER_DIRECTORY="${AXIUSFLOW_DPDK_DRIVER_DIRECTORY:-/workspace/.cache/linux-dev-root/usr/lib/x86_64-linux-gnu}"

/workspace/target/debug/axiusflow_ingest_conformance \
    --dpdk-vdev-lifecycle \
    "$report_path"
