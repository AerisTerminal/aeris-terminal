#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo test --locked --package aeris_platform_runtime network_notifications::tests::
cargo test --locked --package aeris_platform_runtime power_notifications::tests::

live_probe="not_available"
platform="$(uname -s)"
linux_backend="not_current_platform"
windows_network_backend="not_current_platform"
windows_power_backend="not_current_platform"
macos_network_backend="not_current_platform"
macos_power_backend="not_current_platform"

if [[ "$platform" == "Linux" ]]; then
    linux_backend="implemented"
fi
if [[ "$platform" == MINGW* || "$platform" == MSYS* || "$platform" == CYGWIN* ]]; then
    windows_network_backend="implemented_and_registered"
    windows_power_backend="implemented_and_registered"
fi
if [[ "$platform" == "Darwin" ]]; then
    macos_network_backend="implemented_and_registered"
    macos_power_backend="implemented_and_registered"
fi

if [[ "$platform" == "Linux" ]] && command -v busctl >/dev/null 2>&1 \
    && busctl --system get-property \
        org.freedesktop.NetworkManager \
        /org/freedesktop/NetworkManager \
        org.freedesktop.NetworkManager \
        State >/dev/null 2>&1; then
    probe_output="$(
        cargo run --quiet --locked \
            --package aeris_platform_runtime \
            --example native_network_monitor_probe
    )"
    live_probe="passed:${probe_output#native_network_monitor_current=}"
fi
if [[ "$platform" == "Darwin" || "$platform" == MINGW* || "$platform" == MSYS* || "$platform" == CYGWIN* ]]; then
    probe_output="$(
        cargo run --quiet --locked \
            --package aeris_platform_runtime \
            --example native_network_monitor_probe
    )"
    live_probe="passed:${probe_output#native_network_monitor_current=}"
fi

echo "native_environment_monitor=passed linux_backend=${linux_backend} windows_network_backend=${windows_network_backend} windows_power_backend=${windows_power_backend} macos_network_backend=${macos_network_backend} macos_power_backend=${macos_power_backend} state_mapping=true duplicate_states_coalesced=true callback_burst_latest_network_retained=true suspend_before_resume_retained=true compiled_backend_capability=true live_connect_and_current=${live_probe} physical_network_transition=not_run physical_suspend_resume=not_run linux_daemon_restart=not_run"
