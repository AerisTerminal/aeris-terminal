#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo test --locked --package axiusflow_desktop_provider_runtime

echo "desktop_provider_runtime=passed cases=11 vault_credentials=true credential_bytes_bounded=true credential_bytes_zeroized=true session_generation_fencing=true bounded_semantic_queue=true immutable_publication=true overflow_recovery=true unconfirmed_stop_blocks_replacement=true environmental_events_retained_during_cleanup=true interleaved_power_network_ordering=true idle_environmental_cycles_do_not_connect=true public_connect_respects_environment=true drop_attempts_session_stop=true publication_event_debug_redacted=true redacted_diagnostics=true suspend_resume_recovery=true network_change_recovery=true worker_thread_ownership=true provider_adapter=not_implemented native_network_monitor=not_implemented history_composition=not_integrated gpui_integration=not_implemented cloud_absence_proof=not_claimed"
