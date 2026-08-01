#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/fuzz/Cargo.toml"
seed_corpus="$repo_root/fuzz/corpus/af_xdp_safe_boundary"
runs="${AXIUSFLOW_AF_XDP_FUZZ_RUNS:-1024}"

case "$runs" in
    ''|*[!0-9]*|0)
        echo "AXIUSFLOW_AF_XDP_FUZZ_RUNS must be a positive integer" >&2
        exit 2
        ;;
esac

corpus="$(mktemp -d)"
trap 'rm -rf "$corpus"' EXIT
cp "$seed_corpus"/* "$corpus"/

cargo rustc --locked --manifest-path "$manifest" --bin af_xdp_safe_boundary -- \
    --cfg fuzzing \
    -C passes=sancov-module \
    -C llvm-args=-sanitizer-coverage-level=3 \
    -C llvm-args=-sanitizer-coverage-inline-8bit-counters \
    -C llvm-args=-sanitizer-coverage-pc-table \
    -C llvm-args=-sanitizer-coverage-trace-compares

"$repo_root/fuzz/target/debug/af_xdp_safe_boundary" \
    "-runs=$runs" \
    -print_final_stats=1 \
    "$corpus"
