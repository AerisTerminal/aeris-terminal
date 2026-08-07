#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/Cargo.toml"

for package in \
    axiusflow_coinbase_market_adapter \
    axiusflow_desktop_history \
    axiusflow_desktop_provider_runtime \
    axiusflow_desktop
do
    cargo test \
        --manifest-path "$manifest_path" \
        --locked \
        --package "$package" \
        --all-targets \
        --all-features
done

echo "coinbase_desktop_component_conformance=passed explicit_states=true bounded_event_inbox=true bounded_history=true coalescing=true ordered_recovery=true btc_eth_continuity=true shipping_offline_startup=passed shipping_corrupt_cache_rejection=passed shipping_reconnect=not_proven shipping_corrupt_cache_refetch=not_proven shipping_redaction=not_proven shipping_shutdown=not_proven live_desktop_smoke=not_run"
