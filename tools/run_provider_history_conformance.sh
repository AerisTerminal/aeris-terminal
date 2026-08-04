#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package axiusflow_provider_history \
    --test provider_history_conformance

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package axiusflow_provider_history \
    --test handoff_conformance

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package axiusflow_coinbase_market_adapter \
    history::tests::profile_rejects_every_history_lane_without_a_fetch_adapter

echo "provider_history=passed scheduler_cases=8 handoff_cases=3 coinbase_fail_closed_profile=true bounded=true deduplication=true cancellation=true bounded_fetch_failure=true visible_priority=true adjacent_prefetch=true pagination=true monotonic_rate_limits=true empty_snapshot_watermark=true contiguous_handoff=true"
