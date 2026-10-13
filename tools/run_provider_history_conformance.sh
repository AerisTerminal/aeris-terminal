#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package aeris_provider_history \
    --test handoff_conformance

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package aeris_rithmic_protocol_adapter \
    history::tests

echo "provider_history=passed handoff_cases=3 rithmic_adapter_cases=8 contiguous_handoff=true"
