#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/Cargo.toml"

for package in \
    axiusflow_coinbase_market_adapter \
    axiusflow_desktop_history \
    axiusflow_desktop
do
    cargo test \
        --manifest-path "$manifest_path" \
        --locked \
        --package "$package" \
        --all-targets \
        --all-features
done

echo "coinbase_desktop_shipping_conformance=passed explicit_states=true bounded_event_inbox=true bounded_history=true coalescing=true ordered_recovery=true btc_eth_continuity=true shipping_bounded_launch=true shipping_offline_startup=true shipping_corrupt_cache_refetch=true shipping_reconnect=true shipping_stale_generation_fence=true shipping_redaction=true shipping_history_live_continuity=true shipping_shutdown=true live_desktop_smoke=not_run"
