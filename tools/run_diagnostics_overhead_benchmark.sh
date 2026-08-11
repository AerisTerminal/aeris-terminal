#!/usr/bin/env bash
set -euo pipefail

cargo run --locked --release --package axiusflow_diagnostics_overhead -- "$@"
