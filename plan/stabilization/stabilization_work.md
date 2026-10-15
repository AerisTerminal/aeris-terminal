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
| MD-02 | High | The stalled-live recovery watchdog only works for tastytrade | M2.6 (market), M4.8 (trading) | market part closed in this commit; trading gate M4.8 open |
| MD-03 | High (likely; needs fixture confirmation) | Session-day live bars are bucketed from the last bar, not from the exchange trading day | M2.7 | closed in this commit |
| MD-04 | Medium | Weekly and monthly live updates are silently missing for non-CME venues | M2.5 (demand), M2.7 (live) | closed in this commit |
| MD-05 | High (performance; unmeasured) | Hot paths copy the full canonical series on every live tick | M2.2 (measurement), M3.3 (copies), M3.4 (idle CPU) | closed in this commit |
| MD-06 | High | Duplicate lifecycle state machines per `LiveModel` | M3.1 (candle half), M3.2 (trade half) | closed in this commit |
| MD-07 | High | The history ledger is spread across eight parallel maps | M2.10 | closed in this commit |
| MD-08 | Medium | Descriptor flags keep growing while the coordinator still hard-codes providers | M3.5, M3.6 | M3.5 capability/presentation split and literal ban closed in this commit; provider-session registry remains M3.6 |
| MD-09 | Medium | The generic layer depends on provider-specific calendars, and there are three calendars | M2.5 | closed in this commit |
| MD-10 | Medium | The desktop is not provider-neutral and owns series identity | M3.10 (series identity), M6.7 | open |
| MD-11 | Medium | The desktop bridge polls every 2 ms and mixes mutex slots with a command channel | M3.8 (trading feed), M3.9, M4.7 (trading part) | open |
| MD-12 | Medium | Reconnect policy has no backoff, and the "retry limit" message is misleading | M2.4 | closed in this commit |
| MD-13 | Medium | The coordinator is a god struct with duplicated cleanup | M2.9 (fence/cleanup), M3.3 (study wrappers), M3.8 (order-book demand) | command fences/consumer cleanup and study wrappers closed in this commit; order-book demand M3.8 open |
| MD-14 | Low-Medium | `TradeLiveHandoff` is overloaded and allocates per event | M3.2 | closed in this commit; the bounded Indexed correction overlay still recomputes its retained bars |
| MD-15 | Medium | The desktop re-derives runtime state to work around fencing gaps | M2.8 (market), M3.1 (presentation state), M3.9, M6.3 (desktop) | open |
| MD-16 | Low | Smaller defects and inconsistencies | M2.10, M3.2, M3.5, M3.6, M6.6 | M2.10 history comments, M3.2 trade retention/demand-dirty/provider wording, and M3.5 Rithmic presentation and capability documentation closed; other items open |
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

Status: validated (2026-10-13), pushed (f2170311)

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
  - Verification: full local gate green with `AERIS_RITHMIC_KIT_DISABLED=1` (VAL-FND-006).
    No post-push CI run occurred for `f2170311`: on 2026-10-13 `gh workflow list --all`
    reported `CI` as `disabled_manually`. Only the maintainer can re-enable it; the local
    gate is not a substitute for a hosted run.
  - Closes PL-09.
  - Scrutiny follow-up: every workflow now rejects self-hosted runners combined with `push` or
    `pull_request`, including inline and quoted YAML trigger forms; live and macOS lanes remain manual-only.
  - User-testing follow-up: the live_market_gates header now distinguishes workflow_dispatch-only
    self-hosted live lanes from GitHub-hosted push/pull-request CI; no trigger or runner changed.
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
  - User-testing follow-up: naming_check's combined failure heading now names naming and source-policy
    violations rather than misidentifying secret-Debug failures as path naming errors.
- [x] M1.7 diagnostics-log-sink-panic-hook - Rotating log file, panic crash files and a fatal-startup dialog
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added bounded rotating logs, durable panic reports, native fatal dialogs and debug-only `--force-diagnostics-panic`.
  - Validation produced `logs/aeris.log` and one crash report containing the forced panic and backtrace.
  - Scrutiny follow-up: rotation uses `aeris.N.log`, drop notices use the contract text, child-panic
    evidence checks every field and the previous hook, and process exits explicitly flush within two seconds.
  - User-testing follow-up: the debug-only forced panic now occurs after trading, context and market
    setup; a flushed session line precedes the crash. One isolated restart produced one crash file
    with the panic, location, thread, backtrace and release identity.
- [x] M1.8 single-instance-and-validation-mode - Per-data-folder instance lock and validation mode with an isolated vault namespace
  - Commit `3c42981b` bundled M1.4-M1.9; this accepted history is documented rather than rewritten.
  - Added safe `--validation-root`, hashed validation vault namespaces, scoped cTrader borrowing and per-root instance locks.
  - Readiness schema 5 reports the lock, log path and validation context; validation disables release side effects.
  - Scrutiny follow-up: the namespace is exactly `com.aeris.terminal.validation.<sha12>`, borrowing
    logs exactly once at initialization, and launcher promotion, update checks and AUMID registration use one tested deny policy.
  - User-testing follow-up: scripted venue tests distinguish a configured relay from an opened demo
    session; the attachment line appears only after the session opens. Root-overlap and fake-vault tests
    cover contained production roots and exact-key read/write-through with other keys isolated.
    A non-opt-in isolated desktop showed no venue attachment line in 60 seconds.
  - Follow-up: an unconfigured cTrader catalog warm-up no longer announces a skipped provider;
    the diagnostic test isolates the process-wide log sink with a non-recursive, 15-second-bounded
    child. The full gate passed; a fresh 60-second isolated GUI session rendered Hyperliquid with
    no cTrader session, venue or warm-up lines, and a forced-panic run left earlier log lines and one crash file.
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

Status: in progress

- [x] M2.1 market-characterization-tests - Characterization tests at the coordinator command/event boundary
  - Four scripted command/event scenarios pin reseed continuity, overflow covering snapshots,
    retired history completion rejection and symbol/interval changes without session replacement.
    Tests inspect published events and replies, not coordinator maps.
  - `aeris_naming_check` requires each test name individually; the test suite verifies all
    four removal cases. Full local fmt, workspace clippy, workspace tests and naming gate passed.
    MD-17 is partly addressed; replay migration remains M3.11.
  - User-testing round 1: the four named scenarios now take exact history tickets from provider requests and
    assert only consumer publications, command replies and provider-session status; scripted fixture setup and
    coordinator turns live outside the characterization file. No private history/coordinator state is inspected.
  - User-testing round 2: the shared driver injects `ScriptedClock` at construction and sends bounded
    commands and provider events through production channels; the production coordinator turn drains both
    without benchmark-only helpers. All four scenarios, crate checks and the full gate pass.
- [x] M2.2 market-benchmark-baseline - Hot-path and coordinator benchmark baseline
  - Release engine example measures 50,000 completed bars, 10,000 forming-bar revisions
    per simulated second, 63 snapshots at a 16 ms cadence and 256 full study-input copies.
    Allocated bar payload is a capacity-derived lower bound (excludes `Arc` header and
    allocator overhead), not a process-wide allocation counter.
  - Offline coordinator example drives 64 canonical series through 63 idle and loaded
    coordinator turns; 1,025 distinct queued updates exceed the 1,024-event outbox
    capacity and produce a covering snapshot. CPU figures are active single-thread
    workload time per simulated second, not process CPU including worker threads.
    CI's benchmark job is gated by `workflow_dispatch`, never the push/PR path.
  - Baseline, Windows x86_64 release, 24 logical processors, source revision
    `20abd4cc6e8ab9c3b290e39a14e52a515b9b286a` plus M2.2 source diff
    (engine source tree SHA-256 `3c9ef3bedf6d62705c2fedebf3beca6817a6d84dd7a3c09ba55062155da9d93c`):
    engine snapshot 975,593 ns/publish, 6,389 ns/trade, 3,200,064
    bar-payload bytes/publish minimum; study input copy 440,159 ns/tick,
    3,200,000 bar-payload bytes/copy minimum.
    Coordinator first/second release runs (synthetic provider, no sockets):
    idle active CPU 82,000/74,200 ns per simulated second; loaded
    4,018,600/4,530,100 ns; idle and loaded wakeups 63/s; covering snapshot
    797,800/1,633,500 ns; 1,024 events dropped.
    The benchmark workload and source tree are committed by this item; rerun
    from the resulting commit for a clean-tree provenance hash.
- [x] M2.3 market-injectable-clock - Injected clock for the market coordinator
  - `Clock` supplies monotonic and wall time; production uses `SystemClock`, while
    tests advance `ScriptedClock` without sleeping. The coordinator receives a
    clock at construction and shares it with trade handoffs, history retries
    and the stall watchdog.
  - `history_retry_uses_scripted_clock_without_waiting` and
    `watchdog_uses_scripted_clock_without_waiting` pass in under one second.
    Existing public session-status requests use the caller-supplied wall timestamp;
    provider calendar resolution remains M2.5.
- [x] M2.4 reconnect-policy - One jittered, capped, budgeted reconnect policy
  - `Backoff` provides clock-seeded deterministic full/equal jitter, caps, healthy reset,
    server-directed rate-limit waits and typed budget exhaustion. Hyperliquid market
    and display-depth workers use full jitter from 500 ms to 30 s; cTrader has
    a five-attempt failure with the last cause, while tastytrade and the shared
    cTrader venue reuse bounded backoff.
  - Rithmic transport keeps only its adapter retry ticket; non-transport live
    replacement waits at least one second. A scripted adapter retry asserts
    exactly one concurrent ticker login. Tests cover policy bounds, capped
    retries, rate-limit headers and recovery, and cTrader failure publication.
    The blocked-network instance log is deferred to the single M2 milestone probe.
- [x] M2.5 market-calendar-service - Provider-neutral market calendar and session service
  - Added the session source resolver (provider calendar, contract hours, exchange rule, continuous, unknown), domain phase/window/source kinds, and shared exchange-code normalization. CME bucket and DST rules moved from the Rithmic adapter to the shared instruments domain; the runtime calendar owns dated phase/transition projection and weekly contract hours.
  - Runtime session status delegates to the calendar; interval resolution takes calendar source kind. Unsupported weekly/monthly trade-built demands produce a permanent `MarketDemandError` event and an error reply, including tastytrade equity 1W, rather than a series without live handoff. Existing cTrader daily cadence remains accepted without published hours.
  - Calendar matrix, four-provider source resolution, CME DST, unknown 1W/1M and tastytrade equity 1W tests pass; naming_check rejects provider literals in generic calendar and adapter-owned calendar imports. Full local fmt, workspace clippy/tests and naming gate passed. MD-04's non-CME live aggregation remains M2.7.
  - User-testing round 1: every CME/DST, CFE, equity regular/extended, crypto, FX weekend/open and Unknown
    matrix row asserts phase, complete session window and next transition. The public
    `MarketService::market_session_status` is compared with calendar phase and source for all four providers;
    the three moved CME rule tests pass in `aeris_instruments`.
- [x] M2.6 provider-neutral-watchdog - Stall watchdog and practice session answer for every provider
  - Candle and trade handoffs use the injected clock for last progress. The watchdog evaluates both models for every provider against the calendar's open/extended/always-open phase, never retries while closed, and uses a 300-second threshold for unknown sessions (30 seconds while open). It no longer calls the tastytrade broker API from the coordinator.
  - Practice session admission retrieves the installed instrument from the coordinator and asks the same calendar for all providers; unknown and closed phases produce distinct errors. Removed the superseded tastytrade-only practice gate.
  - Scripted cTrader open/closed and Rithmic CME no-trades recovery tests assert one current history request while open and none while closed; pure tests cover all four providers, unknown grace and practice crypto/CME/unknown cases. Full local fmt, workspace clippy/tests and naming gate passed. MD-02 market half closed; the trading-side gate remains M4.8.
  - User-testing round 1: scripted coordinator tests count exactly one current history request for each
    open-market cTrader, tastytrade, Rithmic CME and AlwaysOpen Hyperliquid series; Unknown requests none
    at 30 seconds and exactly one after 300 seconds. The injected clock advances without sleeps.
- [x] M2.7 calendar-session-live-bars - Session-day, weekly and monthly live bars follow the calendar
  - Trade handoffs use calendar session/day, week and month buckets rather than elapsed daily seconds or CME-only buckets. Coordinator owns the installed calendar; tape dates, watchdog and trade-built bars read its current source. Contiguous contract hours still roll daily at the local opening time.
  - Dated provider projections and holidays arrive through generation-fenced provider calendar events, take precedence while valid, and then fall back to contract/exchange rules or Unknown. The tastytrade catalog worker publishes bounded updates when its cached calendar changes and on instrument selection.
  - Tests cover CME daily rolls in normal and DST weeks, Day3 session rolls, all six non-CME weekly/monthly cases, tape trading dates, provider holidays and validity expiry. Scoped tests/Clippy and the full local fmt, workspace Clippy/tests/naming gate pass. The single M2 GUI pass remains scheduled after M2.10.
- [x] M2.8 typed-epoch-fencing - `Epoch<Tag>` and `Fenced<T>` replace raw generations at the provider boundary
  - Added non-arithmetic `Epoch<Tag>`, exhaustion-safe `EpochSequence`, `Fenced` and equality-only `Fence` in market_engine. The coordinator registry alone stamps adapter wire generations; provider events and history completions carry typed provider epochs, including dated calendar updates.
  - Stale provider events and history completions each increment their own bounded diagnostic count and cannot mutate current series or calendar state. Existing engine `ProviderGeneration` and `GenerationId` wrappers remain until their broader consumers migrate; raw adapter counters remain inside the wire boundary.
  - Scoped runtime/engine/naming tests and full workspace fmt/Clippy/tests/naming gate passed; naming_check rejects raw generations in the typed ProviderEvent.
- [x] M2.9 command-fences-consumer-registry - Command fences and one consumer registry; remove the startup sentinel
  - Replaced the five catalog pending maps with typed command-kind fences; desktop startup assigns a unique command generation and explicitly marks startup requests. Tastytrade retains independent startup and picker search lanes without a numeric sentinel.
  - `ConsumerRegistry::remove_consumer` is shared by detach and explicit remove; tests assert no command/outbox/client/demand/order-book residue on either path, and a superseded startup callback is dropped.
  - Market-runtime, desktop and naming scoped checks plus workspace fmt/Clippy/tests/naming passed. The single milestone GUI restore/search check remains after M2.10.
- [x] M2.10 history-tracker - `HistoryTracker` with current and backfill lanes; trim unused `provider_history`
  - One tracker replaces all eight coordinator ledgers and the retry tuple. Exact provider/demand/lane/sequence tickets travel through worker and provider completions; deferred, in-flight, retry, coverage, detached and cancellation state are pruned together.
  - The worker blocks on a full bounded completion channel and serves the oldest backfill every fourth slot. Removed the unused provider-history scheduler modules while preserving model/handoff/error and Rithmic conformance; fixed misplaced history comments.
  - Tracker API, boundary characterization, full fmt/workspace Clippy/workspace tests/naming passed. In an isolated desktop, BTC-USDC 1m history grew from 1,599 to 4,003 bars after panning, with zero interior gaps; ETH search, 1D chart and restart restore succeeded.
  - Scrutiny round 1: empty current-history completions now dispatch the deferred viewport backfill using the completed ticket's provider epoch; current failure/retry also drains it. Scripted-clock tests cover both empty branches, non-empty, retry and retired-epoch completion. Scoped and full gates passed.

### M3 market lifecycle

Status: in progress

- [x] M3.1 series-lifecycle-candle-path - One per-series state machine, ported for candle providers
  - Moved cTrader and Hyperliquid candle state, the single forming bar, seed/recovery transitions, bounded publications and presentation status into `lifecycle::SeriesLifecycle`; removed the old candle handlers and publisher. History uses its exact tracker ticket and the engine remains the canonical completed-bar owner.
  - Scripted lifecycle transitions, published status order, four coordinator characterization scenarios, crate-scoped tests and the full fmt/workspace Clippy/workspace test/naming gate passed. Isolated Hyperliquid 1m had 1,599 bars, zero gaps and forming updates; EURUSD demo had 1,600 bars and live updates.
  - EURUSD's 10 interior gaps are provider omissions, not Aeris-created: an opt-in diagnostic of the **raw decoded cTrader trendbar page** showed exactly 10 gaps (44 missing minutes; starts in UTC seconds: 1791923340, 1791925200, 1791925320, 1791925500, 1791927060, 1791927180, 1791927480, 1791927660, 1791933900, 1791934320). The pre-M3 M2 binary on a separate isolated root showed the same 10 gaps over the overlapping interval; no history-page seam falls in these 10 gaps. The temporary raw-page logger was removed after capture. No credentials or account identifiers were recorded.
- [x] M3.2 series-lifecycle-trade-path - Port trade-built providers and delete the duplicated lifecycle
  - CandleOverlay, Indexed tastytrade and Sequence Rithmic series share `SeriesLifecycle.state` and one bounded publication turn; trade aggregation lives in `lifecycle/trade_model.rs`. Removed the split trade registry, state enum, named Rithmic handlers/publisher and redundant history handoffs.
  - Hyperliquid candle events compare the cached demand coin/interval without formatting a wire symbol per event. Indexed recent trades prune by time, provider errors are neutral, and demand reconciliation no longer doubles `demand_dirty`. The bounded Indexed correction overlay still recomputes retained bars.
  - One table-driven suite exercises all three models, exact ticket rejection, recovery, contiguous completed bars and one forming bar; a scripted adapter-confirmed Rithmic SequenceGap recovers through covering history to Live. The naming gate rejects `SeriesLiveMap` in production with positive/negative scanner fixtures. Scoped and full fmt/workspace Clippy/workspace test/naming gates passed; Rithmic live qualification remains excluded.
- [x] M3.3 engine-series-tail - Series-tail accessor; remove full-series copies from hot and existence paths
  - Added canonical `series_tail`, timestamp existence and borrowed/owned segmented views. Study calculations and output timelines share covering history and sealed 64-bar chunks, copying at most the open chunk; live diagnostics, study bounds and history/publication existence avoid full snapshots. One `execute_study_change` dispatch replaces the five coordinator wrappers. Full covering snapshots remain for recovery, repair and explicit resync.
  - Release benchmark (Windows x86_64, 24 logical processors, 10,000 forming revisions, 63 publishes per size, bar-payload capacity lower bound, excludes allocator/header overhead): M2.2 baseline at 50,000 bars **3,200,064 bytes/publish**; M3.3 tail at 1,000 / 16,384 / 50,000 completed bars **128 / 128 / 128 bytes/publish** (50k:1k ratio **1.000**, limit 1.100). The 50k segmented study-view workload copied zero open-bar payload bytes per forming revision (293 ns/view for 256 ticks); the legacy full-copy reference was 3,200,000 bytes/tick (M2.2: 440,159 ns/tick; distinct workload, not a speedup claim). Evidence command: `cargo run -p aeris_market_engine --release --example market_data_performance --locked -- <out.json>`.
  - Segmented bounds and chained study outputs match full canonical snapshots; chunk rollover and historical correction preserve immutable older views. Focused engine/runtime tests and Clippy plus workspace fmt/Clippy/tests/naming passed. Rithmic live was not used.
- [x] M3.4 coordinator-dirty-publication - Dirty-set publication and deadline timers in the coordinator loop
  - Coordinator blocks with no work and uses injected monotonic deadlines for watchdog, history retry, session transitions, tape publication and bounded control retries; only dirty series publish. Shutdown sends a bounded wake. Scripted idle/deadline and one-series publication tests, four characterization scenarios, workspace fmt/Clippy/tests and naming gate passed.
  - M2.2 release coordinator baseline (`20abd4cc` plus M2.2 diff, Windows x86_64, 24 logical processors, synthetic 64-series workload): idle active coordinator CPU 82,000/74,200 ns per simulated second, loaded 4,018,600/4,530,100 ns, idle/loaded 63 wakeups/s. M3.4 pre-commit release workload (same host, 64 series, 63 loaded batches; `cargo run -p aeris_market_runtime --release --example coordinator_performance --locked`): idle zero active turns and 0 ns/s, loaded 4,613,700/4,401,500 ns/s, idle 0 wakeups/s, loaded 63 wakeups/s. This is synthetic active single-thread time, not desktop process CPU; idle zero reflects no scheduled turns, and loaded CPU did not consistently improve.
  - Release desktop CPU, same host, fresh isolated validation roots, default live Hyperliquid BTC-USDC 1m, 30 s settle then one 60 s `Get-AerisProcessMetrics -CpuSampleSec 60` sample (% of one core): archived M2.2-era source at exact `20abd4cc`, kit disabled, **13.9%** (PID 35720); M3.4 pre-commit working tree, kit disabled, **12.8%** (PID 33228). Improvement 1.1 percentage points (~7.9% relative); one sample per revision, not a sustained-load guarantee. Each desktop and its host exited gracefully before the next launch; no production data root or stored cTrader connection was used.
- [x] M3.5 provider-capabilities - `ProviderCapabilities` and `ProviderPresentation` replace descriptor flags and literals
  - Split internal provider registration into behavioral capabilities (engine streams, live model, calendar, history policy, concurrency, reconnect, depth and quote availability) and presentation metadata; corrected Rithmic selection scope and ready label, and moved interval-to-Rithmic replay aggregation to the adapter with exact interval tests.
  - Generic lifecycle/history/calendar/coordinator/realtime/runtime/publication files reject provider-id literals in production, including after test modules; fixture tests cover each path (quotes and outbox when introduced). Provider-specific invalidation wording moved to provider-event translation. The remaining provider dispatch enums are deliberately M3.6.
  - Workspace fmt, Clippy `-D warnings`, tests and naming_check passed. Characterization (4), lifecycle (12), aggregation and descriptor matrix suites passed; no Rithmic live session or GUI run (internal-only item; M3.4 GUI validation completed).
- [x] M3.6 provider-session-registry - `ProviderSession` trait and `ProviderRegistry`
  - Part 1/3: tastytrade HTTP transport is borrowed outside the catalog-cache mutex and returned on all paths; authorization revocation retires an in-flight transport. Two regression tests and the full workspace gate passed. Registry and per-provider idle work remain before this item can be checked.
  - Part 2/3: idle release and stop-overlap check only the stopping provider's own series, book and alert demand; a scripted two-provider book test guards the overlap. Full workspace gate passed with the CI debug profile and one Cargo job.
  - Part 3/5: catalog command/response dispatch now has one bounded, provider-keyed channel boundary. Deleted `ProviderCatalogChannelSet` and `ProviderCatalogDispatch`; channel tests cover typed round trip, capacity and screen entitlement. The registry-minted pre-connect epoch, realtime channel/dispatch removal, `ProviderSession` and desktop session check remain; item is deliberately unchecked. Catalog migration preceded the epoch move without adding a second epoch or retry owner.
  - Part 4/8: Hyperliquid realtime now uses a registry-reserved epoch before connect, an epoch-bound event sink, a single coordinator backoff and diff-based `ProviderSession` demand. The worker no longer schedules socket retries; its arm of the realtime dispatch enums is gone. Scripted registry tests cover demand changes without reconnect and epoch reservation on a due retry. cTrader, tastytrade and Rithmic retain their previous paths until their respective migration parts; M3.6 remains unchecked.
  - Part 5/8: cTrader market/catalog traffic now uses the registry-reserved, epoch-bound session with registry-owned market reconnect and diff-based demand; its dedicated bounded channels replace the cTrader realtime dispatch arm. The shared cTrader trading relay and its trading-only retry remain intact. Scripted tests cover catalog-only socket failure, demand changes without reconnect, retry budget/server wait, and shared catalog/market epoch; 38 cTrader tests, four characterization cases, and the full workspace gate passed. An isolated demo desktop loaded AUDCAD 1m, then AUDCAD 1h and AUDCHF 1h with live updates and a single market-session-open log entry; provider-omitted history gaps were observed, not filled. tastytrade and Rithmic still retain their previous paths until parts 6 and 7; M3.6 remains unchecked.
  - Part 6/8: tastytrade uses a registry-reserved pre-connect epoch and bound realtime/history events; the worker no longer increments a transport generation or retries sockets. Registry backoff owns transient and server-rate-limit retry; local subscription-budget pacing stays in the worker. Its realtime dispatch enum arm is removed. Scripted demand, epoch and rate-limit tests and four characterization cases pass. Rithmic remains on its original sole transport retry path until part 7; M3.6 remains unchecked.
  - Part 7/8: Rithmic now binds adapter events and history to a registry-reserved epoch; adapter transport replacement emits `TransportReplaced`, which reserves a new epoch and rebinds without opening another socket. The adapter remains the sole Rithmic transport retry owner. Deleted the remaining realtime channel/dispatch enums, session slot and production stamp-after-event path; scripted registry and coordinator tests cover replacement, stale wire retirement and demand changes without reconnection. Four characterization cases and the full workspace gate passed. An isolated Hyperliquid desktop switched BTC to ETH to SOL and SOL 1m to 1h to 1m without a logged reconnect; each captured snapshot had zero interior gaps. On fresh startup BTC initially displayed Loading despite completed history, then rendered after selection changes; this separate startup-resolution issue remains under investigation. Rithmic live validation is deferred until kit and credentials are available, and tastytrade live validation requires fresh OAuth.
  - Part 8/8: `ProviderRegistry` scripted checks record one connection and five named demand changes (symbol, interval, viewport, tab, layout) for each of the four providers. Diagnostics now record each actual Hyperliquid socket open and do not mislabel intentional shutdown cancellation as a reconnect. In the isolated diagnostics desktop at `m3-6-registry-qualified`, UIA identified BTC-USDC, ETH-USDC, SOL-USDC, 1h, then 1m; the stderr and rotating logs each record exactly one Hyperliquid session open (`epoch=2`), no reconnect or connection failure, and snapshots for ETH, SOL 1m, SOL 1h and SOL 1m with `interior_gaps: 0` (1,599/1,599/1,599/5,222 bars). The desktop and host exited gracefully. The fresh-default BTC Loading state persists before a manual symbol change, despite completed history, and is not represented as resolved.
- Startup regression repair (follow-up `fix(market): reserve provider generation before startup history`): detached fresh-root `f00d4a99` and `2f3fd729` builds both rendered BTC-USDC 1m without input; the M3.6 head loaded history but displayed Loading. The first history request captured catalog generation 1 while registry connection reserved epoch 2, so its completion was discarded; synchronous lifecycle activation at reservation now assigns generation 2 before demand/history. The deterministic pre-event ticket test failed before the change and passes after it. A fresh-root fixed desktop rendered 1,599 BTC candles with zero interior gaps, forming updates and one epoch-2 socket without manual selection. Repeat this fresh-default check at M3.10 and M3.12.
- [x] M3.7 instrument-leases-feed-contract - Trading feed domain contract and MarketEngine instrument leases
  - Added validated provider-neutral `aeris_trading::market` routes, exact-scale non-crossed quotes, bounded quote interest, typed feed states/session/terms and events; added `InstrumentClass` and `PriceGrid` on durable contract metadata with legacy Unknown/Other defaults and round-trip coverage. Unverified Hyperliquid grids remain Unknown; M3.8 owns provider terms conversion and verification.
  - Re-keyed the single `DataLeaseRegistry` with `MarketDemandKey::{Series, Instrument}` and one explicit 320-lease runtime bound (64 previous series leases plus 256 quote interests). Owner replacement preflights and is atomic; entitlement keys stay distinct; consumer retirement releases owned leases. `instrument_subscriptions` merges shared demand, and the market-service `QuoteSubscription` guard enqueues a bounded nonblocking drop release processed by the coordinator. Provider quotes-only subscriptions, event delivery, `latest()` and the trading/alert feed integration follow in M3.8, not as parallel M3.7 paths.
  - The complete workspace fmt, strict Clippy, tests and naming check pass, including the four characterization tests, quote precision/crossed-touch, owner sharing/capacity, guard drop/release, durable field round trip and legacy metadata fallback. No GUI behavior changed in M3.7; the probe pass follows the final visible M3 item.
- [x] M3.8 trading-quote-feed - Market-side trading quote feed driven by instrument demand
  - Part 1/3: quote-only instrument leases now reach the shared provider demand and canonical book without creating chart series; Hyperliquid subscribes BBO without candles, cTrader/tastytrade/Rithmic retain their session-owned quote mapping. `QuoteSubscription::latest` reads the bounded coordinator-updated quote projection, validated against the route; release removes the book and quote and changes provider demand. Scripted no-chart quote/release and invalid-spread tests, 381 market-runtime tests (4 ignored), and full workspace gate passed. Trading interest sink, feed events, alert lease migration, terms/grid conversion and live BBO example remain in parts 2-3; item stays unchecked.
  - Part 2/3: revisioned trading quote interest installs and replaces leases in the engine owner. Active price alerts now hold the same owner-scoped instrument leases, including after one-shot triggers and consumer removal; the retired alert-only provider demand merge and its superseded flags are removed. Scripted tests cover transactional interest changes and no-chart alert trade subscriptions. Feed events, runtime terms conversion and live BBO example remain; item stays unchecked.
  - Part 3/3: `TradingFeedLink` delivers bounded latest-wins Quote/Feed/Session/Terms events on its own worker; current catalog and leased sessions replay on attachment, chart quote streams also emit without a separate instrument lease, session transitions use the coordinator's calendar deadline, and retired provider quotes are invalidated. Runtime converts validated provider terms to `InstrumentTerms` and the desktop uses that projection instead of maintaining a separate price-grid/class conversion. Hyperliquid's documented five significant figures and `6 - szDecimals` perp (`8 - szDecimals` spot) grid are sourced from catalog metadata rather than normalized quantity scale; invalid size precision rejects catalog data. An isolated-root, lock-held `hyperliquid_bbo` example subscribed BTC perpetual quotes without any series and printed 20 distinct positive uncrossed price changes at scale 8 within 30 s, exited 0. The scripted no-chart/chart-active, reconnect/feed, catalog terms, session transition, latest-wins, grid and precision tests passed; full workspace gate passed. Live Rithmic qualification still requires gated kit/credentials. No GUI changed, so the GUI pass follows the final desktop-visible M3 item.
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
- The four market coordinator characterization cases and exact test names remain in the boundary
  test file, checked by `aeris_naming_check` (M2.1, this commit).
- Coordinator-owned watchdog, history retry and trade-handoff timestamps come from
  the injected clock; only `SystemClock` reads system time (M2.3, this commit).
- Provider reconnect attempts use capped policy backoff; Rithmic transport has
  only the adapter scheduler and a separate bounded non-transport delay
  (M2.4, this commit).
- Session/cadence resolution uses one provider-neutral calendar and one exchange-code table; generic calendar code cannot contain provider literals or import adapter calendars, and unsupported weekly/monthly live demand reports a typed error (M2.5, this commit).
- Every live model's stall policy and every provider's practice-session check use calendar phases. Unknown sessions use a conservative 300-second stall threshold; closed sessions do not recover from silence (M2.6, this commit).
- Dated provider calendar events are fenced at the coordinator and expire into the instrument's recurring calendar or Unknown; live session, week and month bars and tape dates use the same calendar (M2.7, this commit).
- Typed provider events and history completions use registry-minted epochs and an equality fence; naming_check rejects a raw `ProviderEvent.generation` (M2.8, this commit).
- Catalog callbacks use coordinator-owned command fences by consumer/provider/kind; startup resolution has an explicit lane and consumer retirement uses one registry cleanup path (M2.9, this commit).
- `HistoryTracker` owns current/backfill retry, deferral, empty coverage, detached windows and cancellation behind exact provider/demand/sequence tickets; no parallel coordinator history maps (M2.10, this commit).
- Candle series control and forming state belong to `lifecycle::SeriesLifecycles`; candle handlers and direct clocks in `lifecycle/` are forbidden (M3.1, this commit).
- Trade-built series share that lifecycle owner; `SeriesLiveMap` cannot return to market-service production source (M3.2, this commit).
- Generic market-service lifecycle/history/calendar/quote/outbox/coordinator/realtime/runtime/publication source contains no provider-id literals, including after test modules; provider replay aggregation does not live on `ChartInterval` (M3.5, this commit).
- Provider catalog command/event traffic crosses one bounded `ProviderCatalogChannel`; provider-specific wire commands are translated within its registered channel (M3.6, this commit).

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
- 2026-10-13 M1.6 fix(naming): label source-policy failures accurately - corrected the shared naming_check diagnostic heading for secret-Debug failures.
- 2026-10-13 M1.7 fix(desktop): trigger forced panic after store startup - moved the debug-only panic after service and market setup so the crashing session leaves earlier diagnostics.
- 2026-10-13 M1.8 fix(validation): report only real cTrader venue attachments - corrected attachment diagnostics and covered root overlap and exact-key vault read/write isolation.
- 2026-10-13 M1.2 fix(ci): correct live gate runner guidance - removed the obsolete hosted-runner ban from the live gate comment without changing workflow behavior.
- 2026-10-13 M1.8 fix(validation): keep unconfigured cTrader startup diagnostics silent - suppressed the disconnected catalog warm-up line and bounded the isolated diagnostic test; full gate and isolated desktop/panic checks passed.
- 2026-10-13 M2.1 test(market): characterize coordinator market handoffs - pinned four command/event scenarios and registered their exact names; backfilled the M1 validated/pushed state and recorded the disabled hosted CI workflow.
- 2026-10-13 M2.2 perf(market): record engine and coordinator baselines - measured release workloads and added a manual-only CI performance job.
- 2026-10-13 M2.3 feat(market): inject coordinator clock into recovery timing - shared a monotonic/wall clock with handoffs, watchdog and history retries; scripted tests advance without sleeps.
- 2026-10-13 M2.4 feat(market): bound provider reconnect policies - replaced fixed Hyperliquid retry, budgeted cTrader with real-cause failure, jittered tastytrade and bounded Rithmic non-transport recovery.
- 2026-10-13 M2.5 feat(market): centralize calendar sources and exchange rules - shared CME calendar and venue table, resolved dated/weekly/continuous sessions and rejected unsupported weekly/monthly live demand explicitly.
- 2026-10-13 M2.6 feat(market): recover stalled live series by calendar phase - applied the same injected-clock watchdog to candle and trade models and routed practice-session checks through the calendar.
- 2026-10-13 M2.7 feat(market): roll live bars at calendar sessions - replaced fixed session durations and CME-only live buckets, integrated fenced dated provider holidays and calendar tape dates, and tested six non-CME week/month paths.
- 2026-10-14 M2.8 feat(market): fence provider events and history with typed epochs - registry-stamped adapter generations, fenced dated calendar and history completions, counted retired inputs and pinned typed event fields.
- 2026-10-14 M2.9 feat(market): unify catalog command fences and consumer retirement - removed the startup sentinel and five pending maps, separated startup/picker searches, and centralized consumer cleanup.
- 2026-10-14 M2.10 feat(market): centralize history lanes and exact completion tickets - replaced eight maps, bounded retry/deferral and provider-history scheduler with a single tracker; verified older Hyperliquid chart pages without gaps.
- 2026-10-14 M2.10 fix(market): drain deferred backfill after empty current history - closed the M2 scrutiny blocker with ticket-fenced dispatch and scripted completion-path regressions.
- 2026-10-14 M2.1/M2.5/M2.6 test(market): close user-testing boundary and calendar coverage gaps - moved characterization observations to the command/event boundary; completed the calendar/status matrix and five scripted recovery request checks; full gate green.
- 2026-10-14 M2.1 test(market): drive characterization through coordinator channels - removed direct clock assignment and benchmark turn calls; verified four boundary scenarios and the full gate.
- 2026-10-14 M3.1 feat(market): centralize candle series lifecycle - migrated both candle providers to the lifecycle owner, retained four coordinator scenarios, and verified live Hyperliquid and raw cTrader provider omissions against the pre-M3 binary.
- 2026-10-14 M3.2 feat(market): centralize trade series lifecycle - unified candle/trade control and publication, cached candle wire identity, fenced scripted SequenceGap reseed and one-forming continuity; full workspace gate green.
- 2026-10-14 M3.3 perf(market): share segmented series for study and live publication - replaced full-series study/live/existence copies, unified study dispatch and measured 128-byte tail payload at all three history sizes; full gate green.
- 2026-10-14 M3.4 perf(market): sleep between dirty market publications and deadlines - removed idle coordinator sweeps, preserved deadline recovery and measured lower isolated one-chart desktop CPU against archived M2.2 source; full gate green.
- 2026-10-14 M3.5 refactor(market): separate provider capabilities from presentation - removed descriptor behavior flags and generic provider-id literals, corrected Rithmic presentation, relocated replay aggregation to its adapter, and passed the full workspace gate.
- 2026-10-15 M3.6 refactor(market): release tastytrade state lock during HTTP (M3.6 part 1/3) - isolated the exclusive transport from its bounded catalog cache and fenced authorization revocation; full gate green, registry work follows.
- 2026-10-15 M3.6 refactor(market): bind cTrader market to the provider registry (M3.6 part 5/8) - removed worker-owned market epoch/retry, preserved the shared trading relay and verified demo symbol/interval switches on one session; full gate green, remaining providers follow.
- 2026-10-15 M3.6 fix(market): scope idle-stop overlap to its provider (M3.6 part 2/3) - per-provider demand prevents an unrelated order book from reopening a stopped session; full gate green.
- 2026-10-15 M3.6 refactor(market): unify bounded catalog dispatch (M3.6 part 3/5) - deleted four-arm catalog channel/dispatch enums, preserved provider-specific screen and overflow behavior; full gate green.
- 2026-10-15 M3.6 refactor(market): migrate Hyperliquid to epoch-bound provider session (M3.6 part 4/8) - reserved connection epochs in the registry, transferred realtime retry ownership to the coordinator and kept the three other providers on their existing paths.
- 2026-10-15 M3.6 refactor(market): bind tastytrade session to provider registry (M3.6 part 6/8) - moved epoch and retry ownership from the DXLink worker to the registry with bounded event channels; Rithmic migration follows.
- 2026-10-15 M3.6 refactor(market): bind Rithmic transport replacements to registry epochs (M3.6 part 7/8) - removed the last legacy dispatch and kept adapter transport retry ownership; scripted rebind and isolated Hyperliquid switch checks passed, with startup Loading under investigation.
- 2026-10-15 M3.6 test(market): qualify provider registry desktop switching (M3.6 part 8/8) - observed one Hyperliquid socket and zero reconnects through four zero-gap switch snapshots; recorded separate fresh-start Loading issue and gated-provider live deferrals.
- 2026-10-15 M3 startup fix(market): reserve provider generation before startup history - pre-M3 and pre-registry detached builds rendered the default; fixed the generation race at the coordinator and verified fresh-default rendering without input.
- 2026-10-15 M3.7 feat(market): add bounded instrument leases and trading feed contracts - validated quote/route/terms contracts and durable grid/class, re-keyed the single shared lease owner, and added a nonblocking quote guard release; provider subscriptions and feed follow in M3.8.
- 2026-10-15 M3.8 feat(market): connect quote-only leases to provider demand (M3.8 part 1/3) - no-chart demand drives upstream BBO, quote validation and guard latest state; complete feed and probe follow.
- 2026-10-15 M3.8 refactor(market): lease trading and alert quote demand in engine (M3.8 part 2/3) - replaces the alert-only upstream merge and validates revisioned trading interest; feed events and live probe follow.
- 2026-10-15 M3.8 feat(market): publish bounded trading quote feed (M3.8 part 3/3) - verified no-chart Hyperliquid BBO, calendar/feed/terms events, and the exchange-sourced significant-figure grid under an isolated validation root.

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
- As checked with `gh workflow list --all` on 2026-10-13, the GitHub `CI` workflow is
  `disabled_manually`; consequently there was no post-push run for M1 head `f2170311`.
  Re-enabling it requires the maintainer; no Actions setting was changed.
- The M2.4 blocked-network reconnect interval log was not collected: routing an isolated
  validation process through an unreachable HTTP proxy also blocks the required startup catalog,
  so no WebSocket session begins. Deterministic backoff tests pass; a controlled WebSocket-only
  failure injection or maintainer-approved per-process network restriction is needed for the live log.
