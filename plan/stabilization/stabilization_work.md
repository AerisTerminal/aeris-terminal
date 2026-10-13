# Aeris Terminal stabilization: working document

This is the durable tracker for the Aeris Terminal platform stabilization mission. It lets a fresh
mission (with new agents) understand and resume the work. Keep the section headings of this file
stable; future workers and validators locate content by them.

## 1. Purpose and how to resume

The mission replaces "patch on top of patch" layers with one clean design at each ownership boundary.
It matures paper trading to the approved "solid core", hardens the platform (diagnostics, single
instance, durable writes, SQLite hardening, CI), and trims bloat, without deleting the update pipeline
or the dormant account runtime.

Layout of this directory:

```
plan/stabilization/
  stabilization_work.md     # this tracker
  architecture.md           # the binding target architecture (copy of the mission architecture)
  reports/                  # copies of the mission research: audits 01-05, baseline 06,
                            # designs 07, 09, 10, 11, and work_items.md; plus redacted
                            # probe/benchmark evidence committed by later features
```

Note on layout: the mission tracking rules sketched separate `audit/` and `design/` folders, while
the binding architecture (section 7, milestone M1 row) and the validation contract place the audit and
design report copies under `plan/stabilization/reports/` with the architecture at
`plan/stabilization/architecture.md`. The latter is what is implemented here. Evidence files committed
by later features also live in `reports/`; name them `<item-id>_<probe-or-benchmark>.<ext>` so the
twelve source copies stay distinguishable.

To resume in a fresh mission:

1. Read this file top to bottom.
2. Read `plan/stabilization/architecture.md` (binding target design; it wins over the research design
   documents where they conflict).
3. Read the repo `AGENTS.md` (root) and the "Active restrictions" section below for the rules that are
   already in force.
4. Find the first unchecked work item in "Milestones and work items" below. Its id (for example
   `M1.5`) indexes the full specification in `reports/work_items.md`; design references there point
   into the copies under `reports/`.
5. Work items in dependency order; commit each feature together with an update to this document
   (see the per-feature update rules in the repo `AGENTS.md`).

Validation: the milestone gate is `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
`cargo test --workspace --all-features --locked`, and `cargo run -p aeris_naming_check --locked`.

## 2. Decisions

Binding user decisions (recorded 2026-10-13, at mission start). These override the research design
documents wherever they disagree.

| # | Topic | Decision |
|---|---|---|
| D1 | Scope | All four tracks: paper trading maturity, market-data lifecycle rebuild, desktop decomposition + provider neutrality, platform foundations + bloat trim. |
| D2 | Dormant code | Launcher / updater / rollout / signed lifecycle and the dormant account runtime are KEPT ("update pipeline is necessary"). Review, fix, isolate and trim them; do not delete them for being dormant. |
| D3 | Paper realism | "Solid core": correct lifecycle, market/limit/stop/stop-limit/brackets/OCO, TIF DAY/GTC/IOC/FOK, bid/ask fills, tick checks, multiplier, fees, cash ledger, restart-safe. OUT: depth-aware partial fills, slippage model, margin/buying power (hooks only). |
| D4 | Closed market | Paper orders placed while the market is closed OR the session is unknown are REJECTED with a clear typed reason ("market closed" / "session unknown"). |
| D5 | DAY expiry | Session close for exchange instruments; 00:00 UTC for 24/7 crypto; DAY rejected when the session is unknown. |
| D6 | Stale quotes | Stale after 10 s (crypto) / 60 s (other). Stale -> reject new orders / hold fills with typed reason. |
| D7 | Fees | Configurable per instrument class; default zero. |
| D8 | Existing data | Users' trading.sqlite3, workspace state, chart chrome, vault entries are migrated, never wiped. Existing practice brackets migrate to the new order-group model. |
| D9 | Fill prices | Market and marketable limit fill at touch (buy at ask, sell at bid); resting limit fills at its limit price; stops trigger on the correct side and fill at touch including gaps. |
| D10 | Golden files | Trading golden test files may be replaced when behavior intentionally changes; list each change in the commit/handoff. |
| D11 | Fake CLI modes | `--rithmic` and `--multi-chart` fake-instrument startup modes are removed. |
| D12 | provider_history | Unused parts trimmed (see `architecture.md` for the exact keep/delete list). |
| D13 | Aeris Charts | Cross-repo changes allowed: edit/test/push aeris-charts per its AGENTS.md, then bump only the `aeris_charts_*` revisions here. |
| D14 | CI | GitHub-hosted `windows-latest` (fmt, clippy, test, naming_check) + `ubuntu-latest` (fmt, naming) on push and pull_request to main. Hosted runners lack the Rithmic kit; kit-dependent code skipped there. |
| D15 | plan/ | `plan/` stays in the repo. Tracking doc at `plan/stabilization/stabilization_work.md`, audit/design reports beside it. Repo AGENTS.md updated to allow `plan/stabilization/` and to record each new restriction as it lands. |
| D16 | Git | Workers commit on `main`; orchestrator pushes each validated milestone to `origin/main` (fetch first, never force). |
| D17 | Validation | Validators may launch the real desktop in an isolated data folder while the user keeps their own desktop closed; cTrader demo (stored connection) and Hyperliquid public data may be used live; Rithmic live excluded. |
| D18 | Gates | Milestone gate: full workspace fmt / clippy -D warnings / test / naming_check. Workers scope to the touched crate while iterating and run the full gate before cross-crate commits. |
| D19 | Delivery | One worker per milestone; crate-scoped checks while iterating; full gate and real-path verification once per milestone; then one commit per work item. |

Reconciled decisions recorded in `architecture.md` (binding there; noted here so they are not lost):

- **Per-pane account selection is kept.** Workspace-level trading account selection is out of scope
  (architecture.md section 3.7.3).
- **Risk session rollover default boundary: 00:00 UTC.** (architecture.md risk table, "keep today /
  00:00 UTC / per pane" row.)
- **V24 (dropping the superseded trading tables) is DEFERRED out of this mission.** The tables remain
  in place and unread until V22/V23 are verified in a released build (architecture.md sections 3.4
  and 5).

Design defaults (no explicit user decision; change only with orchestrator approval):

- Hyperliquid price grid: `Unknown` with a warning until verified (architecture.md).
- Scale-in: the first-triggering stop AND target both grow so each role equals the position size
  (architecture.md section 3.6.5 / invariant 13). An earlier note saying targets do not grow is wrong
  and must not be implemented.
- Bracket legs keep offsets from the entry fill.

## 3. Findings index

Finding IDs refer to the audit reports in `reports/` (MD = 01_market_data, TR = 02_trading,
UI = 03_desktop_ui, PL = 04_platform_infra, LC/AC = 07_platform_design). Status is `open` or
`closed in <commit>` ("closed in this commit" is backfilled with the hash from `git log` later).

| Finding | Severity | Title | Closed by | Status |
|---|---|---|---|---|
| MD-01 | High | A persisted 3-day chart pane cannot be restored because the workspace writer and reader disagree | M1.3 | closed in ab5ff341 |
| MD-02 | High | The stalled-live recovery watchdog only works for tastytrade | M2.6 (market), M4.8 (trading) | open |
| MD-03 | High (likely; needs fixture confirmation) | Session-day live bars are bucketed from the last bar, not from the exchange trading day | M2.7 | open |
| MD-04 | Medium | Weekly and monthly live updates are silently missing for non-CME venues | M2.5 (demand), M2.7 (live) | open |
| MD-05 | High (performance; unmeasured) | Hot paths copy the full canonical series on every live tick | M2.2 (measurement), M3.3 (copies), M3.4 (idle CPU) | open |
| MD-06 | High | Duplicate lifecycle state machines per `LiveModel` | M3.1 (candle half), M3.2 (trade half) | open |
| MD-07 | High | The history ledger is spread across eight parallel maps | M2.10 | open |
| MD-08 | Medium | Descriptor flags keep growing while the coordinator still hard-codes providers | M3.5, M3.6 | open |
| MD-09 | Medium | The generic layer depends on provider-specific calendars, and there are three calendars | M2.5 | open |
| MD-10 | Medium | The desktop is not provider-neutral and owns series identity | M3.10 (series identity), M6.7 | open |
| MD-11 | Medium | The desktop bridge polls every 2 ms and mixes mutex slots with a command channel | M3.8 (trading feed), M3.9, M4.7 (trading part) | open |
| MD-12 | Medium | Reconnect policy has no backoff, and the "retry limit" message is misleading | M2.4 | open |
| MD-13 | Medium | The coordinator is a god struct with duplicated cleanup | M2.9 (fence/cleanup), M3.3 (study wrappers), M3.8 (order-book demand) | open |
| MD-14 | Low-Medium | `TradeLiveHandoff` is overloaded and allocates per event | M3.2 | open |
| MD-15 | Medium | The desktop re-derives runtime state to work around fencing gaps | M2.8 (market), M3.1 (presentation state), M3.9, M6.3 (desktop) | open |
| MD-16 | Low | Smaller defects and inconsistencies | M2.10, M3.2, M3.5, M3.6, M6.6 | open |
| MD-17 | Medium | The test structure locks in the current implementation | M2.1 (part), M3.11 | open |
| TR-01 | Critical | The simulated venue gets prices only from chart panes | M3.8 (market side), M4.7 | open |
| TR-02 | High | Closed-market and stale-quote protection exists only for tastytrade | M4.8 (groundwork M3.7) | open |
| TR-03 | High | The kill switch strips protection and leaves positions open | M5.9 | open |
| TR-04 | High | Flatten, flatten-all and event flatten only cover the observed instrument | M5.9 | open |
| TR-05 | High | Snapshot polling cost and the 10,000-order snapshot failure | M4.4 (runtime lookups), M5.11 (runtime), M6.10 (desktop) | open |
| TR-06 | High | Per-quote durable writes and retention that deletes the ledger | M4.10 (per-quote writes), M5.8 (retention and repair) | open |
| TR-07 | High | Position protection is built from overlapping patch layers | M5.2 (validator), M5.5 (model), M5.6 (cleanup) | open |
| TR-08 | Medium | Fill model gaps | M5.1 (part), M5.2 (tick checks), M5.3, M5.4 | open |
| TR-09 | Medium | The risk session never rolls over and no profile is registered | M5.10 (runtime part; desktop UI out of scope) | open |
| TR-10 | Medium | Desktop keeps per-pane copies of trading state and resolves intents against them | M6.10 (part), M6.11 | open |
| TR-11 | Medium | Duplicate PnL trackers and no cash ledger | M4.9 (single PnL path), M5.7 | open |
| TR-12 | Low | Smaller defects and debt | M4.1, M4.2, M4.5, M4.6, M5.9, M8.10 | open |
| UI-01 | High | Practice venue market data is relayed through the chart UI | M3.8 (market side), M4.7 | open |
| UI-02 | High | Symbol/interval switching is a UI-owned state machine without generations | M6.3, M6.4 | open |
| UI-03 | High | Provider-specific identity, entitlement and wording in the desktop | M6.7, M6.8, M6.9 | open |
| UI-04 | Medium | Symbol selection takes two round trips and startup catalog resolution lives in the desktop | M3.10 (runtime part), M6.5 | open |
| UI-05 | Medium | Busy-polling worker loop and duplicated slot/channel command paths | M3.9 (runtime/bridge part), M6.6 | open |
| UI-06 | Medium | Parallel message model and bespoke mailbox in the desktop | M6.1 (fixture part), M6.6 | open |
| UI-07 | High | `WorkspaceSurface` and `TerminalApp` are god objects with no internal encapsulation | M7.3 (AerisChartView), M7.4 (WorkspaceSurface), M7.5 (TerminalApp) | open |
| UI-08 | Medium | Trading state duplicated per pane and polled from the render path | M6.10 | open |
| UI-09 | Medium | Persistence lacks versioning, duplicates atomic-write logic, and silently resets on corruption | M7.7 (duplication/backup), M7.8 | open |
| UI-10 | Medium | Control flow depends on matching display strings | M5.1 (runtime part), M6.8, M6.11 | open |
| UI-11 | Medium | Discarded results and fragmented error presentation | M1.4 (exit park part), M6.2 (most), M6.4 (restore setters), M6.12 | exit park closed in 3c42981b; remainder open |
| UI-12 | Medium (bloat; not a runtime risk) | Dormant account and placeholder code compiled into production | M7.2 | open |
| UI-13 | Low | Hardcoded colours outside the design system | M7.1 | open |
| UI-14 | Medium | Chart analytics and transforms in the integration glue | M7.3 (requests), M7.9 (hollow), M7.10 (sweeps), M7.11 (tape) | open |
| UI-15 | Medium | Tests are coupled to implementation details | M7.12 | open |
| UI-16 | Low | Frame loop does unrelated work on every frame | M7.6 | open |
| PL-01 | High | Dormant release, launcher, rollout, and updater stack | M8.1 (kept and isolated per D2) | open |
| PL-02 | High | `unsafe_code` forbid is not enforced in platform_runtime | M1.5 (start), M8.3 | platform_sys boundary started in 3c42981b; remaining FFI migration open |
| PL-03 | High | Diagnostics are lost in Windows release builds; no panic hook or log file | M1.7 | closed in 3c42981b |
| PL-04 | High | No single-instance lock | M1.8 | closed in 3c42981b |
| PL-05 | Medium-High | Trading SQLite store lacks busy timeout, integrity check, and corruption recovery | M1.9 | closed in 3c42981b |
| PL-06 | Medium | Five atomic-write implementations; trading `write_atomic` is not atomic | M1.5 (primitive), M1.6 | closed in 3c42981b |
| PL-07 | Medium | Dormant account_runtime with latent defects and a heavy dependency stack | M1.4 (part), M8.4 (rest) | token safety closed in 3c42981b; remainder open |
| PL-08 | Medium | `naming_check` pins source text and is co-edited with the code it checks | M8.12 | open |
| PL-09 | High | CI is manual-only | M1.2 | closed in 63a71463 |
| PL-10 | Medium | Stale physical-pacing tooling and display_timing module | M8.3 (display_timing), M8.11 | open |
| PL-11 | Low | `plan/` directory contradicts the AGENTS documentation rule | M1.1 (rule change; `plan/` kept per D15), M8.12 (Readme/.gitignore part) | closed in 853a3916 |
| PL-12 | Medium | Vendored `gpui_base` is unlinted and oversized | M8.11 | open |
| PL-13 | Medium | `study_sdk` depends on `market_runtime` | M8.9 (accepted facade; see architecture.md 2.2 rule 8) | open |
| PL-14 | Low-Medium | `contracts` mixes in-process DTOs with persisted schema | M8.9 | open |
| PL-15 | Low | Dependency hygiene | M8.10 | open |
| PL-16 | Low | Legacy brand migrations and tokens | M8.6 (tokens/installer); legacy migrations deferred with LC-17 | open |
| PL-17 | Low | Hardcoded user agent version and unbounded context request wait | M8.10 | open |
| PL-18 | Low | Chart chrome file is unversioned and read without bounds | M7.7 (bounds), M7.8 | open |
| PL-19 | Low | Unused or test-only exports in platform_runtime | M8.3 (part), M8.11 | open |
| LC-01 | High | Launcher loads the signing key before command dispatch; uninstall fails without a key | M8.2 | open |
| LC-02 | Critical | No producer for the release pipeline; the pipeline cannot be tested end to end | M8.7 | open |
| LC-03 | High | Manifest signature covers re-serialized JSON with no domain prefix | M8.6 | open |
| LC-04 | Medium | Release wire tokens still Axiusflow-branded, with transitional aliases | M8.6 | open |
| LC-05 | High | Readiness probe migrates the real trading database, bricking rollback | M8.5 (probe part), M8.8 | open |
| LC-06 | Medium | Verification can exceed the 20 s restart handshake budget | M8.8 | open |
| LC-07 | Medium | Authenticode checked via PowerShell text parsing; silently skipped without a thumbprint | M8.2 | open |
| LC-08 | Medium | Block-delivery errors swallowed without a record | M8.6 | open |
| LC-09 | Medium | Probation misses a hung or parked desktop | M8.8 | open |
| LC-10 | Medium | Some release publishes are not durable (plain renames) | M1.5 (primitive), M1.6 | closed in 3c42981b |
| LC-11 | Medium | Readiness report written non-atomically; schema declared twice | M1.6 (durable-write part), M8.5 | durable write closed in 3c42981b; shared schema remains open |
| LC-12 | High | Uninstall deletes user data without asking; Axiusflow installer lines remain | M8.6 | open |
| LC-13 | Low | `.bat` self-delete with unescaped paths | M8.2 | open |
| LC-14 | Low | Launcher shows errors by spawning PowerShell | M8.2 | open |
| LC-15 | Low | sysinfo full-process scans in 20 ms polling loops | M8.2 | open |
| LC-16 | Medium | Desktop spawns launcher promotion on every no-arg start, including dev builds | M8.2 | open |
| LC-17 | Low | Legacy data-root migration and legacy vault probes run on every start | deferred: needs maintainer confirmation that migration is complete (07 F19) | open |
| LC-18 | Medium | Recovery from `Activated` re-executes candidate code; stray directories block launch | M8.8 | open |
| LC-19 | Low | `minimum_version` mixes product semver with generation numbering | M8.6 | open |
| LC-20 | Low | Six build-time release variables read in five places; partial config compiles silently | M8.2 | open |
| LC-21 | Medium | No end-to-end download, readiness-schema contract, producer or durable-write crash tests | M8.7 | open |
| AC-01 | High | Tokens derive `Clone + Debug`; any `{:?}` leaks them | M1.4 | closed in 3c42981b |
| AC-02 | Medium | Profile vault limit (4096) exceeds the Windows 2560-byte cap; no size precheck | M8.4 | open |
| AC-03 | Medium | Vault I/O runs under the account state mutex | M8.4 | open |
| AC-04 | High (when enabled) | OIDC issuer comes from a runtime environment variable | M8.4 | open |
| AC-05 | High | Exit path can park the thread forever, leaving a zombie process | M1.4 | closed in 3c42981b |
| AC-06 | Medium | Poisoned credential-access mutex fails permanently | M8.4 | open |
| AC-07 | Medium | GPUI reqwest client installed only to fetch the avatar image | M8.10 | open |
| AC-08 | Info | Unverified that an empty issuer spawns no network work | M8.4 | open |

Findings intentionally not closed by any single item (from `reports/work_items.md`):

- LC-17: deferred until the maintainer confirms their machine is migrated.
- PL-16 (legacy migrations part): same deferral as LC-17; tokens and installer residue close in M8.6;
  the never-migrated Asceify brand is left as is.
- TR-12 (two position models): intentional; net positions for the simulated route and hedged broker
  positions stay, behind one ledger.
- PL-01 "delete or feature-gate" fix: superseded by D2; closed as isolation plus fixes in M8.1-M8.8.
- PL-07 / UI-12 "cargo feature" fix: superseded by D2; the account code is isolated behind
  `AccountGate` (M7.2) and fixed (M1.4, M8.4), not feature-gated.
- 05 root causes 7 and 8 (commit rhythm, two-repo chart cost): process findings, addressed by the
  one-commit-per-item discipline and this tracking document.

## 4. Milestones and work items

Item specifications live in `reports/work_items.md`. Check the box and add 1-3 status sub-bullets in
the same commit as the feature.

### M1 foundations

Status: in progress

- [x] M1.1 stabilization-tracking-doc - Create the stabilization tracking document and allow `plan/stabilization` in AGENTS.md
  - Created `plan/stabilization/stabilization_work.md` (this file) with all 93 finding IDs, decisions
    D1-D18 plus the reconciled decisions, all 89 items, progress log, restrictions and resume
    instructions; copied audits 01-05, baseline 06, designs 07/09/10/11 and `work_items.md` into
    `plan/stabilization/reports/` and the mission architecture into `plan/stabilization/architecture.md`
    (byte-identical copies; redaction scan clean); repo `AGENTS.md` sanctions `plan/stabilization/`
    and records the tracking discipline.
  - Deviation from `library/tracking.md` layout: report copies sit under `reports/` (not `audit/` +
    `design/`), following the binding `architecture.md` section 7 and validation contract.
  - Verification: `aeris_naming_check` and the full workspace gate green with the new files.
- [x] M1.2 hosted-ci-push-pr - CI on push and pull request with GitHub-hosted runners
  - Rewrote `.github/workflows/ci.yml`: triggers `push`/`pull_request` (main) and `workflow_dispatch`,
    top-level `permissions: contents: read`, concurrency with cancel-in-progress, no secrets and no
    self-hosted labels. `windows-latest` runs fmt, clippy `-D warnings`, the workspace tests
    (all-features, locked) and naming_check with job timeouts, the toolchain from
    `rust-toolchain.toml` and `AERIS_RITHMIC_KIT_DISABLED=1` / `CARGO_INCREMENTAL=0` /
    `CARGO_PROFILE_DEV_DEBUG=line-tables-only`; `ubuntu-latest` runs fmt plus the naming_check
    test and binary. The self-hosted-only header is replaced (kit-gated Rithmic code is documented
    as skipped on hosted runners) and the stale evidence-verifier and performance steps are off the
    push path (they leave with their tools in M8.11); actions stay pinned to full commit SHAs.
  - naming_check CI-shape rule 12 landed: `push_ci_shape_violations` with one positive and four
    negative tests (pull_request_target, self-hosted runner on a pull_request workflow, missing
    read-only permissions, missing concurrency) plus `hosted_push_ci_shape_remains_required` on the
    real file replace `workflows_never_run_fork_pull_requests` and the self-hosted shape pin;
    `workflows_never_use_pull_request_target` keeps the fork-safety guarantee for every workflow.
  - Verification: full local gate green with `AERIS_RITHMIC_KIT_DISABLED=1` (VAL-FND-006); the
    post-push GitHub Actions run URL is recorded here by the orchestrator after the milestone push.
  - Closes PL-09.
  - Scrutiny follow-up: every workflow now rejects self-hosted runners combined with `push` or
    `pull_request`, including inline and quoted YAML trigger forms; live and macOS lanes remain manual-only.
- [x] M1.3 unified-interval-mapping - Fix Day3 workspace restore with one runtime-owned interval/cadence mapping
  - `market_runtime::cadence` now owns interval resolution and the legacy inverse; workspace panes persist stable `ChartInterval` labels.
  - Both legacy Day3 encodings restore, unsupported panes are repaired or skipped with a notice, and desktop cadence construction is forbidden.
  - Hyperliquid's official candle-snapshot contract and a representative public `3d` response were checked first; opens were UTC-aligned and exactly 259,200,000 milliseconds apart.
  - Verified by runtime/desktop round-trip, legacy migration, repair-notice, naming, and real Hyperliquid 3D restart checks.
- [x] M1.4 bounded-exit-and-secret-tokens - Exit never parks forever; tokens are redacted and zeroized
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Exit and readiness perform one bounded account-refresh quiesce wait; failure is logged and exits nonzero without parking.
  - `VerifiedTokens` now uses non-cloneable, zeroizing `SecretToken` values with redacted debug output.
- [x] M1.5 platform-sys-durable-writer - New `platform_sys` crate with the single durable file writer and unsafe isolation rule
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added the leaf `platform_sys` crate, durable staged-file primitives, crash/drop tests, and an exact interim unsafe allowlist.
- [x] M1.6 adopt-durable-writer - Replace every hand-rolled file writer, including the non-atomic trading export
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Workspace, chart chrome, trading export, readiness, lifecycle, launcher and chart export writes use the durable writer.
  - Removed write-time `.bak` rotation and verified uncommitted trading exports preserve the previous file.
  - Scrutiny follow-up: lifecycle scans and audits share the exact `StagedFile` name parser; read-only
    workspace and chart-chrome `.bak` recovery remains available without restoring backup rotation.
  - Scrutiny R2-B1: out-of-line `#[cfg(test)] mod name;` no longer hides subsequent production from
    naming checks; both module forms have production-function regression fixtures. Debug and interim
    unsafe allowlists now have per-entry reasons and reject stale entries; no owner violations surfaced.
- [x] M1.7 diagnostics-log-sink-panic-hook - Rotating log file, panic crash files and a fatal-startup dialog
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added bounded rotating logs, durable panic reports, native fatal dialogs and debug-only `--force-diagnostics-panic`.
  - Validation produced `logs/aeris.log` and one crash report containing the forced panic and backtrace.
  - Scrutiny follow-up: rotation uses `aeris.N.log`, drop notices use the contract text, child-panic
    evidence checks every field and the previous hook, and process exits explicitly flush within two seconds.
- [x] M1.8 single-instance-and-validation-mode - Per-data-folder instance lock and validation mode with an isolated vault namespace
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added safe `--validation-root`, hashed validation vault namespaces, scoped cTrader borrowing and per-root instance locks.
  - Readiness schema 5 reports the lock, log path and validation context; validation disables release side effects.
  - Scrutiny follow-up: the namespace is exactly `com.aeris.terminal.validation.<sha12>`, borrowing
    logs exactly once at initialization, and launcher promotion, update checks and AUMID registration use one tested deny policy.
- [x] M1.9 sqlite-hardening-migrations - SQLite busy timeout, integrity check, quarantine, table-driven migrations and pre-migration backup
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added quick-check corruption errors, confirmed-only quarantine, contiguous migrations, pre-migration backups and clean checkpoints.
  - Scrutiny follow-up: damaged/headerless files and newer schemas return typed non-mutating errors;
    migrations preserve rows for every historical version, and fresh/current databases create no backup.
  - Deterministic v18 generator: set `AERIS_V18_FIXTURE_OUT` and run
    `cargo test -p aeris_trading_runtime --locked generate_sanitized_v18_fixture_for_validation -- --ignored --nocapture`.
    The generated validation fixture SHA-256 was `10e47e62016a7eabc3aba0072a2fea5809c6ad7329e40e5180cb167451164506`.
  - On Windows 11 Pro 10.0.26300, Intel Core i7-13700K, 31.7 GiB RAM, a 50,000-order,
    7,032,832-byte fixture measured `PRAGMA quick_check` at 32.7642 ms in the debug test profile and
    18.1913 ms in release. Both are below 250 ms, so `quick_check` remains inline during store open.
  - Scrutiny recheck follow-up: preflight of an existing database now inspects a scratch copy of the
    database plus its `-wal`/`-shm` sidecars, so declining quarantine leaves every file under
    `trading\` byte-identical (VAL-FND-032).

### M2 market foundations

Status: pending

- [ ] M2.1 market-characterization-tests - Characterization tests at the coordinator command/event boundary
- [ ] M2.2 market-benchmark-baseline - Hot-path and coordinator benchmark baseline
- [ ] M2.3 market-injectable-clock - Injected clock for the market coordinator
- [ ] M2.4 reconnect-policy - One jittered, capped, budgeted reconnect policy
- [ ] M2.5 market-calendar-service - Provider-neutral market calendar and session service
- [ ] M2.6 provider-neutral-watchdog - Stall watchdog and practice session answer for every provider
- [ ] M2.7 calendar-session-live-bars - Session-day, weekly and monthly live bars follow the calendar
- [ ] M2.8 typed-epoch-fencing - `Epoch<Tag>` and `Fenced<T>` replace raw generations at the provider boundary
- [ ] M2.9 command-fences-consumer-registry - Command fences and one consumer registry; remove the startup sentinel
- [ ] M2.10 history-tracker - `HistoryTracker` with current and backfill lanes; trim unused `provider_history`

### M3 market lifecycle

Status: pending

- [ ] M3.1 series-lifecycle-candle-path - One per-series state machine, ported for candle providers
- [ ] M3.2 series-lifecycle-trade-path - Port trade-built providers and delete the duplicated lifecycle
- [ ] M3.3 engine-series-tail - Series-tail accessor; remove full-series copies from hot and existence paths
- [ ] M3.4 coordinator-dirty-publication - Dirty-set publication and deadline timers in the coordinator loop
- [ ] M3.5 provider-capabilities - `ProviderCapabilities` and `ProviderPresentation` replace descriptor flags and literals
- [ ] M3.6 provider-session-registry - `ProviderSession` trait and `ProviderRegistry`
- [ ] M3.7 instrument-leases-feed-contract - Trading feed domain contract and MarketEngine instrument leases
- [ ] M3.8 trading-quote-feed - Market-side trading quote feed driven by instrument demand
- [ ] M3.9 fenced-events-waitable-outbox - Every consumer event is fenced; the bridge blocks on a waitable outbox
- [ ] M3.10 select-and-demand-descriptors - `select_and_demand` command and runtime-published instrument and provider descriptors
- [ ] M3.11 market-replay-harness - Deterministic market replay harness with permanent scenarios
- [ ] M3.12 hyperliquid-lifecycle-probe - Live Hyperliquid market lifecycle probe with JSON evidence

### M4 trading core

Status: pending

- [ ] M4.1 trading-clock-command-time - Injected trading clock; cancel events at command time
- [ ] M4.2 trading-core-owner-shell - Thin owner thread around a synchronous `TradingCore`
- [ ] M4.3 trading-replay-harness - Deterministic trading replay harness
- [ ] M4.4 indexed-order-book - Indexed order book; remove linear scans
- [ ] M4.5 trading-core-module-split - Split `lib.rs` and the Coordinator into focused modules
- [ ] M4.6 trading-store-module-split - Split `store.rs` by table family
- [ ] M4.7 runtime-owned-quotes - Trading runtime owns its quotes; the desktop relay is removed
- [ ] M4.8 provider-neutral-gating - Session and freshness gating for every provider
- [ ] M4.9 fill-accounting-consolidation - One fill-accounting implementation (V20)
- [ ] M4.10 facts-only-persistence - Commit-then-apply steps; no durable writes per quote

### M5 trading engine

Status: pending

- [ ] M5.1 order-lifecycle-typed-events - Complete order state machine, revisions and typed reasons (V21)
- [ ] M5.2 admission-validator - One admission validator for simulated and broker routes
- [ ] M5.3 execution-fill-rules - Bid/ask fill rules, stop and stop-limit triggering, multipliers
- [ ] M5.4 time-in-force-expiry - DAY expiry at session close or 00:00 UTC; GTC stays working
- [ ] M5.5 order-groups-and-bracket-migration - One order-group model with migration of existing brackets (V22)
- [ ] M5.6 remove-protection-layers - Delete the superseded protection layers
- [ ] M5.7 cash-ledger-and-fees - Fill-driven cash ledger with configurable fees, default zero (V23)
- [ ] M5.8 checkpoints-retention-equity - Checkpoints before retention, repair from checkpoints, bounded equity history
- [ ] M5.9 emergency-operations - Kill switch, flatten scopes and atomic reverse across all instruments
- [ ] M5.10 risk-session-rollover - Persisted risk session with rollover; bracket legs count as attached stops
- [ ] M5.11 trading-publication-api - Change-driven, bounded publication with paged history
- [ ] M5.12 paper-trading-live-probes - Live paper-trading probe on Hyperliquid with restart, plus cTrader demo regression

### M6 desktop bridge

Status: pending

- [ ] M6.1 desktop-services-test-support - Inject `DesktopServices`; move `FixtureMarketWorker` to test support
- [ ] M6.2 notification-center - One `UserNotice` model and `NotificationCenter`
- [ ] M6.3 chart-binding-generation-fence - Per-chart `ChartBinding` drops stale publications in one place
- [ ] M6.4 pane-presentation - `PanePresentation` applies every user chart property in one place
- [ ] M6.5 select-and-demand-panes - One `select_and_demand` per selection; startup resolution leaves the desktop
- [ ] M6.6 bridge-message-collapse - Collapse the parallel desktop market message model
- [ ] M6.7 descriptor-driven-identity - Render runtime descriptors; remove fake CLI modes; provider-neutrality rule
- [ ] M6.8 typed-provider-and-risk-events - Typed provider and risk events; default workspace without an instrument
- [ ] M6.9 descriptor-login-and-symbol-browser - Descriptor-driven provider connect, login form and symbol browser
- [ ] M6.10 trading-store - One workspace `TradingStore` fed by the change stream
- [ ] M6.11 trading-intents-and-snapshot-removal - Revisioned chart intents and scoped emergency buttons; remove `TradingSnapshot`
- [ ] M6.12 discard-policy-enforcement - Zero unhandled discards in desktop production code

### M7 desktop structure

Status: pending

- [ ] M7.1 design-tokens-for-chart-colors - Remaining chart and picker colors become design tokens
- [ ] M7.2 account-gate-isolation - Dormant account integration isolated behind `AccountGate`
- [ ] M7.3 chart-host-events - `AerisChartView` emits host events; split its host state
- [ ] M7.4 pane-controllers-split - `WorkspaceSurface` becomes composition and render over pane controllers
- [ ] M7.5 app-controllers-split - `TerminalApp` split into app controllers
- [ ] M7.6 frame-scheduler - Dirty-source `FrameScheduler`; delete the 100 ms timer
- [ ] M7.7 workspace-durable-container - Versioned, checksummed workspace container on top of `platform_sys`
- [ ] M7.8 workspace-document-migrations - Writer-owned workspace document, migration chain and visible restore outcomes
- [ ] M7.9 aeris-charts-hollow-candles - Hollow candles as an Aeris Charts series style (cross-repo) and pin bump
- [ ] M7.10 runtime-tape-sweeps - Sweep classification and incremental tape in `market_runtime`
- [ ] M7.11 desktop-incremental-tape - Desktop consumes runtime sweeps and incremental tape
- [ ] M7.12 desktop-test-rewrite - Desktop tests against controller boundaries

### M8 lifecycle and trim

Status: pending

- [ ] M8.1 release-lifecycle-crate-split - Move release code into a `release_lifecycle` crate
- [ ] M8.2 launcher-hygiene-and-trust - Launcher command dispatch, process handling, release config and Authenticode
- [ ] M8.3 ffi-consolidation - Remaining FFI moves to `platform_sys`; `platform_runtime` forbids unsafe
- [ ] M8.4 account-runtime-safety - Account runtime vault, lock and issuer fixes
- [ ] M8.5 readiness-contract - Shared readiness report; readiness never touches the real database
- [ ] M8.6 release-wire-protocol-and-residue - Release wire protocol v1, uninstall safety and Axiusflow residue removal
- [ ] M8.7 release-packager-e2e - Release producer tool and end-to-end lifecycle harness (no publishing)
- [ ] M8.8 rollback-safety - Schema-gated rollback, startup-complete probation, deterministic recovery
- [ ] M8.9 dependency-direction-and-contracts - Fix dependency direction and reduce `contracts` to the persisted schema
- [ ] M8.10 dependency-hygiene - Remove reqwest stack, unused crates and duplicate pins; small context_runtime fixes
- [ ] M8.11 vendor-and-tooling-trim - Trim `gpui_base`, stale tooling and test-only exports
- [ ] M8.12 naming-check-structural-and-docs - Structural naming_check and final README/AGENTS update

## 5. Active restrictions

Rules added during the mission. Each entry names the commit that introduced it and mirrors the rule in
the repo root `AGENTS.md`.

- `plan/stabilization/` is the one sanctioned planning location in the repo; the "no planning
  documents" rule applies everywhere else (M1.1, 853a3916).
- Every feature commit updates `plan/stabilization/stabilization_work.md` in the same commit: check the
  item, note the findings it closes, append a progress-log line, and record any new restriction here
  (M1.1, 853a3916).
- Every commit that introduces or changes an architecture restriction (a `tools/naming_check` rule, a
  crate boundary, a writer or CI rule) also updates the repo `AGENTS.md` in the same commit (M1.1,
  853a3916).
- CI runs on GitHub-hosted runners on `push`/`pull_request` to `main` and `workflow_dispatch`:
  `windows-latest` runs the full local gate and `ubuntu-latest` runs fmt plus naming_check, with
  read-only `permissions`, concurrency cancellation and `AERIS_RITHMIC_KIT_DISABLED=1`.
  `pull_request_target`, `secrets.` references and self-hosted runners are forbidden on the
  pull-request path; `tools/naming_check` enforces the shape (M1.2, 63a71463).
- Desktop production code stores `ChartInterval` and never constructs `SeriesCadence`; the sole mapping belongs
  to `market_runtime::cadence` and is enforced by `desktop_never_constructs_series_cadence` (M1.3, ab5ff341).
- `platform_sys` is the only new unsafe boundary. Only its platform-specific source files may add unsafe code;
  the remaining pre-existing `platform_runtime` FFI files stay on an exact interim allowlist until M8.3
  (M1.5, 3c42981b).
- Durable file publication goes through `platform_sys::fs`; production raw `fs::write` and `fs::rename` calls are
  rejected outside the exact lifecycle/build allowlist (M1.6, 3c42981b).
- Secret-bearing types use reviewed redacting wrappers and cannot derive or use debug formatting in production
  (M1.7, 3c42981b).
- Each data root has one process owner. Validation roots and vault namespaces are isolated unless the explicit
  stored-cTrader borrowing flag is present (M1.8, 3c42981b).

## 6. Progress log

Append-only, newest last: `YYYY-MM-DD <item id> <commit subject> - <one-line note>`.

- 2026-10-13 M1.1 docs(stabilization): add the stabilization tracking document and plan/stabilization report copies - created the tracker with all 93 findings, decisions D1-D18 plus reconciled decisions and all 89 items; copied the 11 research reports into `reports/` and the architecture beside this file (redaction scan clean); `AGENTS.md` sanctions `plan/stabilization/` and records the tracking and restriction discipline; PL-11 closed by the deliberate rule change (D15).
- 2026-10-13 M1.2 ci(workflows): run the full gate on hosted runners for pushes and pull requests - `.github/workflows/ci.yml` now triggers on push/pull_request (main) and workflow_dispatch on `windows-latest` (fmt, clippy, workspace tests, naming_check) and `ubuntu-latest` (fmt, naming_check) with read-only permissions, concurrency cancellation, no secrets and `AERIS_RITHMIC_KIT_DISABLED=1`; the naming_check CI-shape rule gained positive and negative tests and replaced the self-hosted pins; PL-09 closed.
- 2026-10-13 M1.3 fix(market): unify chart interval cadence mapping - moved interval resolution and legacy inversion to `market_runtime`, persisted `ChartInterval` labels, repaired unsupported panes with notices, and restored both legacy Day3 encodings.
- 2026-10-13 M1.4 feat(foundations): harden lifecycle persistence and diagnostics - bounded every account-refresh exit wait and introduced redacted, zeroizing token storage.
- 2026-10-13 M1.5 feat(foundations): harden lifecycle persistence and diagnostics - added `platform_sys`, the durable staged writer, unsafe isolation and crash-boundary tests.
- 2026-10-13 M1.6 feat(foundations): harden lifecycle persistence and diagnostics - adopted the durable writer across persistence, exports, readiness and release paths.
- 2026-10-13 M1.7 feat(foundations): harden lifecycle persistence and diagnostics - added rotating logs, durable panic reports, fatal dialogs and `--force-diagnostics-panic`.
- 2026-10-13 M1.8 feat(foundations): harden lifecycle persistence and diagnostics - added validation-root isolation, vault namespaces, readiness schema 5 and per-root instance locking.
- 2026-10-13 M1.9 feat(foundations): harden lifecycle persistence and diagnostics - hardened SQLite open/shutdown, migrations, backups and confirmed-only corruption quarantine.
- 2026-10-13 M1.2 fix(ci): enforce safe triggers across every workflow - closed scrutiny B1 with cross-workflow self-hosted trigger checks and repaired M1 tracking metadata.
- 2026-10-13 M1.6 fix(persistence): recover staged and legacy state safely - closed scrutiny B2-B4 with shared staged-file recognition, read-only backup fallback and complete production-source scanning.
- 2026-10-13 M1.7 fix(observability): flush exact diagnostics before exit - closed scrutiny B5-B7 with contract log names/text, complete panic evidence and bounded explicit exit flushing.
- 2026-10-13 M1.8 fix(validation): isolate identity and vault side effects - closed scrutiny B8-B9 with one side-effect policy, exact vault namespace derivation and exact once-only borrowing diagnostics.
- 2026-10-13 M1.9 fix(trading): reject unsafe journals and preserve migrations - closed scrutiny B10-B14 with typed newer-schema errors, non-mutating corruption checks, correct backup eligibility, historical-row fixtures and measured integrity-check placement.
- 2026-10-13 M1.9 fix(trading): reject unsafe journals and preserve migrations - closed the VAL-FND-032 recheck: WAL databases are preflighted on a scratch copy so a declined quarantine keeps every trading file byte-identical.
- 2026-10-13 M1.6 fix(naming): scan production after cfg(test) declarations - closed scrutiny R2-B1 with direct scanner fixtures for inline/out-of-line modules and reasoned, stale-checked allowlists.

## 7. Known gaps / deferred

From `reports/work_items.md` ("Deferred / out of scope") plus mission-accepted limitations:

- Depth-aware partial fills, the slippage model, margin and buying power, currency conversion, and the
  Funding/MarginReserve ledger kinds. Only hooks are left (D3 "solid core").
- V24: dropping the legacy trading tables is deferred out of this mission; it needs V22/V23 verified
  in a released build first (09 section 5.2). The superseded tables stay, unread.
- Removing the legacy data-root migration and the vault legacy-name probes (F19, LC-17) waits until
  the maintainer confirms their machine is migrated.
- Risk profile registration UI (TR-09 desktop part); only runtime rollover is in scope.
- Automatic route resolution for migrated instruments without a stored route (09 Q10); these show
  "feed unavailable" until the instrument is selected again.
- Holiday data for exchange-rule calendars (10 Q3). Exchange rules report Open on holidays; trading is
  still protected by the staleness rule.
- Focusing the existing window on a second launch; it needs IPC.
- An Aeris Charts append operation for order flow (only if measured as the bottleneck in M7.11).
- An Aeris Charts in-place data reset, if the M6.4 check shows the engine lacks it.
- Workspace-level trading account selection; the per-pane selection is kept (architecture.md 3.7.3).
- Rithmic live qualification, fresh cTrader OAuth (hosted broker 404), production release publication,
  code-signing certificate and key custody, the AWS release endpoint, and the macOS workflows.
- Manual checks only the maintainer can run: logoff flush (M8.3), VM install and uninstall (M8.6),
  Rithmic login through the native vault (M6.9).
