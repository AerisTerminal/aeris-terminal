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

echo "coinbase_desktop_conformance=passed explicit_states=true bounded_event_inbox=true bounded_history=true coalescing=true ordered_recovery=true offline_startup=true reconnect=true corrupt_cache_recovery=true redaction=true btc_eth_continuity=true clean_shutdown=true live_desktop_smoke=not_run"
