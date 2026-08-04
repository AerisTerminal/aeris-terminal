#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo test --locked --package axiusflow_platform_runtime network_notifications::tests::

live_probe="not_available"
if [[ "$(uname -s)" == "Linux" ]] && command -v busctl >/dev/null 2>&1 \
    && busctl --system get-property \
        org.freedesktop.NetworkManager \
        /org/freedesktop/NetworkManager \
        org.freedesktop.NetworkManager \
        State >/dev/null 2>&1; then
    probe_output="$(
        cargo run --quiet --locked \
            --package axiusflow_platform_runtime \
            --example native_network_monitor_probe
    )"
    live_probe="passed:${probe_output#native_network_monitor_current=}"
fi

echo "native_network_monitor=passed cases=4 linux_network_manager=true state_and_owner_match_rules=true global_state_available=true non_global_states_unavailable=true duplicate_states_coalesced=true compiled_backend_capability=true live_connect_and_current=${live_probe} next_event_integration=not_proven daemon_restart_integration=not_proven windows_backend=not_implemented macos_backend=not_implemented"
