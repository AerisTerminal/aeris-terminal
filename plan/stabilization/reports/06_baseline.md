# Verification baseline

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal`, branch `main` (ahead of `origin/main` by 4 commits).
Host: Windows 11, PowerShell 7, 24 logical cores, 32 GB RAM. Measured 2026-10-13.

Important context: the `./target` directory was already fully warm (71.6 GB of debug artifacts) when this run started, so every gate time below is a warm-cache time. A cold build was **not** measured. A full cold rebuild would need about 70 GB of disk in a separate target directory, and only 93 GB is free on C:.

Concurrent edits: when the run started, `git status --short` showed a clean tree. While the gates were running, another actor (not this audit) modified two files:
- `apps/desktop/src/native_ui/input.rs`
- `third_party/gpui_base/src/input/base/element.rs`

The first fmt, clippy and test runs therefore validated the clean committed tree. The later re-runs validated the tree with those edits applied. Zed's rust-analyzer (PID 27960) was also running against the repository; it holds the Cargo build-directory lock during flycheck, and one cargo invocation reported `Blocking waiting for file lock on build directory`.

## Toolchain

| Item | Value |
|---|---|
| rustc | 1.99.0 (b940084d7 2026-09-28) |
| cargo | 1.99.0 (5f94df478 2026-08-27) |
| rust-toolchain.toml | channel `1.99.0`, components `clippy`, `rustfmt`, profile `minimal` (honored; the active rustc matches) |
| Workspace members | 25 packages including `tools/naming_check` (`aeris_naming_check`) |

## Gate results

| Gate | Command | Result | Duration (warm) | Notes |
|---|---|---|---|---|
| Format (clean tree) | `cargo fmt --all -- --check` | PASS | 2.8 s | 0 files would change |
| Format (after concurrent edits) | same | **FAIL** | 2.8 s | 2 files would change: `apps/desktop/src/native_ui/input.rs:287` and `third_party/gpui_base/src/input/base/element.rs:1992`. Both are uncommitted edits made by another actor during this run. |
| Clippy (clean tree) | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS | 1.0 s (fully cached) | 0 warnings, 0 errors |
| Clippy (after concurrent edits) | same | PASS | 7.3 s | Re-checked `gpui-base` and `aeris_desktop` only |
| Build (CI step) | `cargo build --workspace --all-targets --all-features --locked` | PASS | 37.3 s, then 9.0 s | Recompiled rithmic_protocol, platform_runtime and desktop (first run) and desktop only (second run) because of the concurrent edits |
| Tests | `cargo test --workspace --all-features --locked` | PASS | 100.4 s (run 1), 108.8 s (run 2) | Run 1: 1878 passed, 0 failed, 14 ignored. Run 2: 1881 passed, 0 failed, 14 ignored (the concurrent edits added 3 tests). |
| naming_check binary | `cargo run -p aeris_naming_check --locked` (from repo root) | PASS | 17.3 s (includes compiling the bin) | Prints `snake_case naming check passed` |
| naming_check tests | `cargo test -p aeris_naming_check --locked` | PASS | 1.9 s | 66 passed (these are the architecture assertions) |
| Desktop dev build | `cargo build -p aeris_desktop --locked` | PASS | 46.5 s (first), 0.8 s (no-op), 9.9 s (after a desktop-only edit) | See the feature-unification note below |

## Clippy findings summary

| Lint | Crate | Count |
|---|---|---|
| (none) | all 25 workspace packages | 0 |

The full log is at `C:\Users\devraj\AppData\Local\Temp\aeris_audit\clippy.log` (clean tree) and `clippy2.log` (after the concurrent edits). The `-D warnings` gate is green in both states.

## Test results

Run 1 (clean tree). The full log is at `C:\Users\devraj\AppData\Local\Temp\aeris_audit\test.log`. The second run is at `test2.log`.

| Crate / binary | Tests | Passed | Failed | Ignored | Time (s) |
|---|---|---|---|---|---|
| aeris_account (lib) | 5 | 5 | 0 | 0 | 0.00 |
| aeris_account_runtime (lib) | 90 | 90 | 0 | 0 | 1.78 |
| aeris_application (lib) | 5 | 5 | 0 | 0 | 0.00 |
| aeris_chart_integration (lib) | 159 | 158 | 0 | 1 | 1.06 |
| aeris_context_runtime (lib) | 13 | 12 | 0 | 1 | 0.02 |
| aeris_contracts (lib) | 0 | 0 | 0 | 0 | 0.00 |
| aeris_contracts (tests/contracts.rs) | 6 | 6 | 0 | 0 | 0.00 |
| aeris_ctrader_open_api_adapter (lib) | 89 | 89 | 0 | 0 | **66.78** |
| aeris_design_system (lib) | 16 | 16 | 0 | 0 | 0.00 |
| aeris_desktop (lib) | 83 | 83 | 0 | 0 | 0.29 |
| aeris_desktop (bin main.rs) | 438 | 437 | 0 | 1 | 1.07 |
| aeris_hyperliquid_market_adapter (lib) | 61 | 57 | 0 | 4 | 0.05 |
| aeris_instruments (lib) | 0 | 0 | 0 | 0 | 0.00 |
| aeris_market_data (lib) | 25 | 25 | 0 | 0 | 0.00 |
| aeris_market_engine (lib) | 34 | 34 | 0 | 0 | 0.00 |
| aeris_market_runtime (lib) | 280 | 276 | 0 | 4 | 0.26 |
| aeris_naming_check (bin) | 66 | 66 | 0 | 0 | 1.31 |
| aeris_observability (lib) | 20 | 20 | 0 | 0 | 0.00 |
| aeris_platform_runtime (lib) | 116 | 116 | 0 | 0 | **10.05** |
| aeris_platform_runtime (bin aeris_launcher) | 20 | 20 | 0 | 0 | 0.38 |
| aeris_provider_history (lib) | 0 | 0 | 0 | 0 | 0.00 |
| aeris_provider_history (tests/handoff_conformance) | 3 | 3 | 0 | 0 | 0.00 |
| aeris_provider_history (tests/provider_history_conformance) | 11 | 11 | 0 | 0 | 0.00 |
| aeris_rithmic_protocol_adapter (lib) | 159 | 159 | 0 | 0 | 3.09 |
| aeris_rithmic_protocol_adapter (bin provision_rithmic_test) | 0 | 0 | 0 | 0 | 0.00 |
| aeris_rithmic_protocol_adapter (bin rithmic_test_smoke) | 11 | 11 | 0 | 0 | 1.43 |
| aeris_study_sdk (lib) | 22 | 21 | 0 | 1 | 0.00 |
| aeris_study_sdk (tests/native_sdk_surface) | 3 | 3 | 0 | 0 | 0.00 |
| aeris_tastytrade_market_adapter (lib) | 24 | 24 | 0 | 0 | 3.60 |
| aeris_terminal_ui (lib) | 37 | 36 | 0 | 1 | 0.06 |
| aeris_trading (lib) | 8 | 8 | 0 | 0 | 0.00 |
| aeris_trading_runtime (lib) | 88 | 87 | 0 | 1 | 3.49 |
| Doc-tests (25 crates) | 0 | 0 | 0 | 0 | 0.00 |
| **Total** | **1892** | **1878** | **0** | **14** | 94.7 s of test runtime in 100.4 s wall time |

Slowest test binaries:
1. `aeris_ctrader_open_api_adapter` lib, 66.8 s. This is dominated by `session_tests::general_and_historical_buckets_limit_200_requests_each` (`crates/adapters/ctrader_open_api/src/session_tests.rs:679`). The test drives the real-time rate-limit buckets over a local TLS socket, so it is inherently wall-clock bound; it triggered the "running for over 60 seconds" notice. It took 66.74 s in run 2, so its timing is stable.
2. `aeris_platform_runtime` lib, 10.1 s.
3. `aeris_tastytrade_market_adapter` lib (3.6 s), `aeris_trading_runtime` lib (3.5 s), `aeris_rithmic_protocol_adapter` lib (3.1 s).

Cargo runs test binaries one after another, so the ctrader binary alone accounts for about two thirds of the test gate's wall time.

Ignored tests (14). All are intentionally manual, live-network, or release-soak tests:
- Live public network: `sources::tests::official_public_sources_publish_current_validated_shapes` (BEA/Fed/NOAA/CFTC); `desktop::engine_market_worker::tests::native_market_runtime_hyperliquid_startup_resolves_stale_default_generation`; Hyperliquid `http::tests::live_catalog_includes_volume_ranked_markets`, `live_month_candle_snapshot_decodes`, `live_three_day_candle_snapshot_decodes`; `ws::tests::live_public_socket_connects_and_receives_aggregated_book`; `hyperliquid_realtime::tests::live_worker_cancellation_is_prompt`.
- Release-mode measurements and soaks: `view::tests::measured_order_flow_burst_frame_work_stays_inside_one_frame`; `order_book_view::tests::measured_ladder_render_cost_stays_inside_one_frame`; `tests::measured_store_workload_is_bounded_and_restartable`; `study::tests::concurrent_native_study_tail_soak_keeps_state_output_and_demand_bounded`; `study::tests::native_non_bar_burst_soak_keeps_output_and_demand_bounded`; `study::tests::native_study_reinitialize_and_historical_repair_soak_keeps_accounting_bounded`; `builtins::tests::ema_live_tail_release_soak_measures_recursive_work`.

## Failures and flaky tests

- No test failures in either full run (run 1: 1878/0/14; run 2: 1881/0/14), so no flaky-test rerun was needed.
- Timing-sensitive test: the ctrader rate-limit test above (about 66.8 s, stable across both runs). It is the only test that runs longer than 60 s. Under heavy CPU contention it is the most likely candidate for timing variance, but no failure was observed.
- One stderr line during the desktop tests, `Aeris chart older history is unavailable: provider history failed (retryable): ...`, is expected output from a test that injects a failure. It is not a failure.
- Format gate failure: the only failing gate is `cargo fmt --check` against the two files that another actor edited, uncommitted, during this run (`apps/desktop/src/native_ui/input.rs:287` and `third_party/gpui_base/src/input/base/element.rs:1992`). The committed tree is format-clean.

## naming_check result

- CI (`.github/workflows/ci.yml`, both Linux and Windows lanes) has no dedicated naming_check step. It runs naming_check implicitly through `cargo test --workspace --all-features --locked`, because `tools/naming_check` is a workspace member. Its 66 `#[test]` functions encode the architecture assertions (dependency direction, provider-kit isolation, chart-integration interaction routing, single market-engine owner, and similar).
- `main()` performs the snake_case path check over the current directory, so the binary must be run from the repository root: `cargo run -p aeris_naming_check --locked`.
- Results: binary PASS (`snake_case naming check passed`); `cargo test -p aeris_naming_check --locked` PASS, 66/66 in 1.48 s.
- CI on Windows also runs `tools/test_evidence_verifiers.ps1`, `tools/test_desktop_endurance_workflow.ps1` and the `market_data_performance` release example. These were outside this assignment and were **not** run.

## Resource notes

- Target directory: 74.2 GB total (`target/debug` 71.6 GB, `target/release` 2.6 GB). C: has 93.4 GB free.
- Peak memory, sampled every 3 s across all runs: about 7.5 GB combined working set for cargo, rustc and the linker, with up to 7 toolchain processes at once. Peak system memory in use was 21.2 GB of 32 GB, including other desktop apps. These peaks came from the incremental rebuilds of `gpui-base` and `aeris_desktop` (desktop link plus test binaries). A cold build at full parallelism (24 jobs) was not measured and would likely peak higher.
- Rebuild times (warm cache):
  - No-op `cargo build -p aeris_desktop`: 0.8 s. No-op clippy, test `--no-run` and build: 0.7–1.0 s.
  - Desktop-only source edit: about 9–10 s for `cargo build -p aeris_desktop` or the workspace build, and about 7 s for clippy.
  - Edit in `third_party/gpui_base` (plus chart_integration and desktop): 37–46 s.
- Feature-unification rebuild: switching from `--workspace --all-features` to `cargo build -p aeris_desktop` without `--all-features` recompiled `aeris_chart_integration`, `gpui-base` and `aeris_desktop` (46.5 s). Alternating the two invocation styles causes repeated rebuilds of these crates. Use one style consistently, preferably the gate style with `--workspace --all-features`.
- Cold build time: not measured (see the context note at the top).

## Recommendations for mission gate commands

Per edit or iteration, scoped to the touched crate (seconds on a warm cache):
```
cargo fmt --all -- --check
cargo clippy -p <crate> --all-targets --all-features --locked -- -D warnings
cargo test -p <crate> --all-features --locked
```
Use `--all-features` on scoped commands too. This keeps feature unification identical to the workspace gates and avoids the gpui-base and chart_integration rebuild described above.

Per milestone, the full documented gates, which are feasible every milestone (about 2–3 min warm, plus incremental compile of the edited crates):
```
cargo fmt --all -- --check                                                         # ~3 s
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings      # ~1-60 s depending on edits
cargo test --workspace --all-features --locked                                     # ~100-110 s warm; ~67 s is one ctrader test
cargo run -p aeris_naming_check --locked                                           # run from repo root; tests already covered by cargo test
```

Parallelism:
- `--jobs`: keep cargo's default (24). The measured peak of about 7.5 GB toolchain memory leaves headroom on 32 GB. If several agents or editors share the machine, or a cold build is expected, use `--jobs 16` to cap linker and rustc memory. Do not run two cargo invocations against the same `target/` at once; they serialize on the build-directory lock anyway, and rust-analyzer (Zed) also takes that lock.
- `--test-threads`: keep the default (number of cores). Each test binary already runs its tests in parallel, and the gate's wall time is bound by one sequential rate-limit test, so more threads would not help. To shorten milestone loops while working outside the ctrader adapter, run `cargo test --workspace --all-features --locked --exclude aeris_ctrader_open_api_adapter` (about 35 s), then run the full gate once before delivery.
- Do not lower test threads to 1. That would serialize roughly 1800 fast tests without any observed flakiness to justify it.
- Doc-tests: all 25 crates have 0 doc-tests. Their overhead is small (about 5 s total), so no `--lib --bins --tests` restriction is needed.

Before delivery on Windows, CI additionally runs `tools/test_evidence_verifiers.ps1`, `tools/test_desktop_endurance_workflow.ps1` and `cargo run -p aeris_market_engine --example market_data_performance --release --locked -- <out.json>`. Their duration was not measured here.
