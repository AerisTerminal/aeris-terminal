#!/usr/bin/env bash
set -euo pipefail

cargo run --locked --release --package tradingplot_diagnostics_overhead -- "$@"
