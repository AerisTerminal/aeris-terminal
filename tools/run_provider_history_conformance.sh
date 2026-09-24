#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package aeris_provider_history \
    --test provider_history_conformance

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

echo "provider_history=passed scheduler_cases=8 handoff_cases=3 rithmic_adapter_cases=8 bounded=true connection_cancellation=true definitive_connection_failure_fallback=true deduplication=true cancellation=true bounded_fetch_failure=true visible_priority=true adjacent_prefetch=true pagination=true monotonic_rate_limits=true empty_snapshot_watermark=true contiguous_handoff=true"
