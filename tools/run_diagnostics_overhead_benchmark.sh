#!/usr/bin/env bash
set -euo pipefail

cargo run --locked --release --package aeris_diagnostics_overhead -- "$@"
