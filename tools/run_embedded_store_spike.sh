#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$repo_root/target/release/axiusflow_ingest_conformance"
evidence_dir="$repo_root/.cache/evidence"

mkdir -p "$evidence_dir"
source_revision="$(git -C "$repo_root" rev-parse HEAD)"
workspace_fingerprint="$({
    while IFS= read -r -d '' source_file; do
        if [[ -f "$repo_root/$source_file" ]]; then
            printf '%q %s\n' \
                "$source_file" \
                "$(git -C "$repo_root" hash-object "$source_file")"
        else
            printf '%q missing\n' "$source_file"
        fi
    done < <(git -C "$repo_root" ls-files --cached --others --exclude-standard -z)
} | git -C "$repo_root" hash-object --stdin)"
GITHUB_SHA="$source_revision+workspace.$workspace_fingerprint"
export GITHUB_SHA

cargo build \
    --manifest-path "$repo_root/Cargo.toml" \
    --release \
    --package axiusflow_ingest_conformance

"$binary" \
    --embedded-store-spike \
    "$evidence_dir/stage_2_embedded_store_spike_$(uname -s).json"
