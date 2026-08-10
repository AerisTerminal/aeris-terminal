#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
history_root="${AXIUSFLOW_COINBASE_SMOKE_HISTORY_ROOT:-$repo_root/.cache/coinbase-desktop-live-smoke}"

for product in BTC-USD ETH-USD
do
    mkdir -p "$history_root/$product"
    cargo run \
        --quiet \
        --locked \
        --manifest-path "$repo_root/Cargo.toml" \
        --package axiusflow_desktop \
        -- \
        --coinbase-live-smoke "$product" "$history_root/$product"
done

echo "coinbase_shipping_live_smoke=passed products=BTC-USD,ETH-USD"
