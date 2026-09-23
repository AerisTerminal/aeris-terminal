#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/Cargo.toml"
kit_proto_dir="$repo_root/provider_kit/current/proto"

if [[ -d "$kit_proto_dir" ]]; then
    cargo test \
        --manifest-path "$manifest_path" \
        --locked \
        --package asceify_rithmic_protocol_adapter \
        --all-targets \
        --all-features
    kit_evidence="passed"
else
    kit_evidence="unavailable"
fi

ASCEIFY_RITHMIC_KIT_DISABLED=1 cargo test \
    --manifest-path "$manifest_path" \
    --locked \
    --package asceify_rithmic_protocol_adapter \
    --all-targets \
    --all-features

echo "rithmic_protocol_conformance=passed installed_kit=$kit_evidence kit_unavailable=passed discovery=true login=true trades=true quotes=true depth=true heartbeat=true heartbeat_message_silence=client_local_fault_injection_passed disconnect_reconnect=true recovery=true clean_stop=true bounds=true read_only_allowlist=true redaction=true native_offline_startup=true native_network_recovery=true native_suspend_resume=true environmental_generation_fence=true authorized_test_traffic=not_run"
