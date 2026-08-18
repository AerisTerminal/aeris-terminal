#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --package axiusflow_local_storage \
    --test history_store_lifecycle

echo "local_history_store=passed atomic_publication=true encrypted_segments=true scope_isolation=true bounded_catalog=true targeted_quarantine=true quarantine_expiry=true refetch_replacement=true revision_invalidation=true retention=true key_revocation_required=true shared_key_rejected=true recovery=provider_refetch_or_live_only"
