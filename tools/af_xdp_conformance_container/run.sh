#!/usr/bin/env bash
set -euo pipefail

receive_interface="axrx0"
transmit_interface="axtx0"
report_path="/evidence/stage_1_af_xdp_copy_evidence_Docker.json"

test -n "${GITHUB_SHA:-}"
test -x /workspace/target/debug/axiusflow_ingest_conformance

mkdir -p "${LIBXDP_BPFFS:?}"

ip link add "$receive_interface" \
    numrxqueues 1 numtxqueues 1 \
    type veth peer name "$transmit_interface" \
    numrxqueues 1 numtxqueues 1
ip link set dev "$receive_interface" address 02:00:00:00:00:02
ip link set dev "$transmit_interface" address 02:00:00:00:00:01
ip link set "$receive_interface" up
ip link set "$transmit_interface" up

/workspace/target/debug/axiusflow_ingest_conformance \
    --af-xdp-copy-conformance \
    "$receive_interface" \
    "$transmit_interface" \
    "$report_path"
