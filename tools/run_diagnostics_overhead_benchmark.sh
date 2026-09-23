#!/usr/bin/env bash
set -euo pipefail

cargo run --locked --release --package asceify_diagnostics_overhead -- "$@"
