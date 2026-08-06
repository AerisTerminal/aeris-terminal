#!/usr/bin/env bash
set -euo pipefail

cargo run --locked --package axiusflow_diagnostics_overhead -- "$@"
