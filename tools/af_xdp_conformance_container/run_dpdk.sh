#!/usr/bin/env bash
set -euo pipefail

report_path="/evidence/stage_1_dpdk_vdev_lifecycle_evidence_Docker.json"

test -n "${GITHUB_SHA:-}"
test -x /workspace/target/debug/axiusflow_ingest_conformance

export AXIUSFLOW_DPDK_PMD_LIBRARY="${AXIUSFLOW_DPDK_PMD_LIBRARY:-/workspace/.cache/linux-dev-root/usr/lib/x86_64-linux-gnu/librte_net_ring.so.26}"

/workspace/target/debug/axiusflow_ingest_conformance \
    --dpdk-vdev-lifecycle \
    "$report_path"
