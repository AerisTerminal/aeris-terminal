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
eleven source copies stay distinguishable.

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
| D14 | CI | GitHub-hosted `windows-latest` (fmt, clippy, test, naming_check) + `ubuntu-latest` (fmt, naming) on push and pull_request to main. Hosted runners lack the Rithmic kit; kit-dependent code skipped there. Later amended: the `workflow_dispatch`-only `macos.yml` and `macos_preview.yml` workflows exist, and `macos_preview.yml` is the one hosted lane that may receive the Rithmic kit (repo `AGENTS.md`). |
| D15 | plan/ | `plan/` stays in the repo. Tracking doc at `plan/stabilization/stabilization_work.md`, audit/design reports beside it. Repo AGENTS.md updated to allow `plan/stabilization/` and to record each new restriction as it lands. |
| D16 | Git | Workers commit on `main`; orchestrator pushes each validated milestone to `origin/main` (fetch first, never force). |
| D17 | Validation | Validators may launch the real desktop in an isolated data folder while the user keeps their own desktop closed; Hyperliquid public data may be used live; Rithmic live excluded. |
| D18 | Gates | Milestone gate: full workspace fmt / clippy -D warnings / test / naming_check. Workers scope to the touched crate while iterating and run the full gate before cross-crate commits. |
| D19 | Delivery | One worker per milestone; crate-scoped checks while iterating; full gate and real-path verification once per milestone; then one commit per work item. |

Reconciled decisions recorded in `architecture.md` (binding there; noted here so they are not lost):

- **Per-pane account selection is kept.** Workspace-level trading account selection is out of scope
  (architecture.md section 3.7.3).
- **Risk session rollover default boundary: 00:00 UTC.** (architecture.md risk table, "keep today /
  00:00 UTC / per pane" row.) The runtime also offers `RiskSessionBoundary::ChicagoClose`
  (17:00 America/Chicago) for futures; no desktop code selects it yet (section 7).
- **V24 was first deferred; superseded.** V24 drops `managed_brackets` and `protective_orders` and is
  applied by the gated `finalize_group_migration` step (after the group backfill and the runtime
  conversion of awaiting brackets), never by the migration chain. Only the removal of the old P&L
  tables (`trade_pnl_cycles`, `risk_trade_cycles`) and the per-fill P&L columns stays deferred; they
  remain unread (architecture.md sections 3.4 and 5).

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
| MD-02 | High | The stalled-live recovery watchdog only works for one broker provider | M2.6 (market), M4.8 (trading) | market watchdog and trading practice gate closed; M4.8 GUI resting-order check outstanding |
| MD-03 | High (likely; needs fixture confirmation) | Session-day live bars are bucketed from the last bar, not from the exchange trading day | M2.7 | closed in ecc749ab |
| MD-04 | Medium | Weekly and monthly live updates are silently missing for non-CME venues | M2.5 (demand), M2.7 (live) | closed in ecc749ab |
| MD-05 | High (performance; unmeasured) | Hot paths copy the full canonical series on every live tick | M2.2 (measurement), M3.3 (copies), M3.4 (idle CPU) | closed in 30a2d5d1 |
| MD-06 | High | Duplicate lifecycle state machines per `LiveModel` | M3.1 (candle half), M3.2 (trade half) | closed in 8c61798d |
| MD-07 | High | The history ledger is spread across eight parallel maps | M2.10 | closed in 1303adbd |
| MD-08 | Medium | Descriptor flags keep growing while the coordinator still hard-codes providers | M3.5, M3.6 | closed: M3.5 capability/presentation split and literal ban in 2f3fd729; M3.6 provider-session registry in 3d474290 |
| MD-09 | Medium | The generic layer depends on provider-specific calendars, and there are three calendars | M2.5 | closed in 0eae9355 |
| MD-10 | Medium | The desktop is not provider-neutral and owns series identity | M3.10 (series identity), M6.7 | open |
| MD-11 | Medium | The desktop bridge polls every 2 ms and mixes mutex slots with a command channel | M3.8 (trading feed), M3.9, M4.7 (trading part) | closed: trading feed in db7a572c; waitable outbox and single command queue in 866032c4 and 9f568443; desktop quote relay removed in cd54c087 |
| MD-12 | Medium | Reconnect policy has no backoff, and the "retry limit" message is misleading | M2.4 | closed in a00725b0 |
| MD-13 | Medium | The coordinator is a god struct with duplicated cleanup | M2.9 (fence/cleanup), M3.3 (study wrappers), M3.8 (order-book demand) | closed: command fences/consumer cleanup and study wrappers in e52e72f8; order-book demand with M3.8 in db7a572c |
| MD-14 | Low-Medium | `TradeLiveHandoff` is overloaded and allocates per event | M3.2 | closed in 8c61798d; the bounded Indexed correction overlay still recomputes its retained bars |
| MD-15 | Medium | The desktop re-derives runtime state to work around fencing gaps | M2.8 (market), M3.1 (presentation state), M3.9, M6.3 (desktop) | open |
| MD-16 | Low | Smaller defects and inconsistencies | M2.10, M3.2, M3.5, M3.6, M6.6 | M2.10 history comments, M3.2 trade retention/demand-dirty/provider wording, and M3.5 Rithmic presentation and capability documentation closed; other items open |
| MD-17 | Medium | The test structure locks in the current implementation | M2.1 (part), M3.11 | closed in 0e77fa3c |
| TR-01 | Critical | The simulated venue gets prices only from chart panes | M3.8 (market side), M4.7 | closed in cd54c087; GUI pane-free bracket fill recorded in 6ed03a11 |
| TR-02 | High | Closed-market and stale-quote protection exists only for one broker provider | M4.8 (groundwork M3.7) | provider-neutral session, feed-generation and per-class age gates in trading owner; GUI resting-order check outstanding |
| TR-03 | High | The kill switch strips protection and leaves positions open | M5.9 | closed: one atomic step locks, cancels non-protective orders with the kill-switch cause and flattens priced positions; unpriced positions keep protection and are reported |
| TR-04 | High | Flatten, flatten-all and event flatten only cover the observed instrument | M5.9 | closed: FlattenScope covers every account/instrument from owner quotes; event flatten needs no observation; broker closes cover every instrument |
| TR-05 | High | Snapshot polling cost and the 10,000-order snapshot failure | M4.4 (runtime lookups), M5.11 (runtime), M6.10 (desktop) | runtime closed: bounded change-driven publication, paged history and the snapshot guard fix landed in M5.11; desktop snapshot consumers remain with M6.10 |
| TR-06 | High | Per-quote durable writes and retention that deletes the ledger | M4.10 (per-quote writes), M5.8 (retention and repair) | closed: checkpoint-fold retention, checkpoint repair and the seeded property identity run |
| TR-07 | High | Position protection is built from overlapping patch layers | M5.2 (validator), M5.5 (model), M5.6 (cleanup) | closed: one group authority, legacy layers deleted in M5.6 |
| TR-08 | Medium | Fill model gaps | M5.1 (part), M5.2 (tick checks), M5.3, M5.4 | closed: resting and placement limits fill at their stated prices, stops/stop-limits emit durable triggers before touch fills, nonmarketable IOC/FOK and five-second no-quote markets cancel with typed causes, candidates are price-prioritized through a side-aware trigger index, fill quantity/price use the single touch-only policy pair, and DAY orders expire at their acceptance session close with verified-route calendar recovery |
| TR-09 | Medium | The risk session never rolls over and no profile is registered | M5.10 (runtime part; desktop UI out of scope) | runtime closed: V27 `risk_sessions` roll at the account boundary on the owner timer with durable baseline, meter reset and daily-lock release; desktop registration UI stays deferred |
| TR-10 | Medium | Desktop keeps per-pane copies of trading state and resolves intents against them | M6.10 (part), M6.11 | open |
| TR-11 | Medium | Duplicate PnL trackers and no cash ledger | M4.9 (single PnL path), M5.7 | closed: exact cost basis plus one durable cash ledger with fees |
| TR-12 | Low | Smaller defects and debt | M4.1, M4.2, M4.5, M4.6, M5.9, M8.10 | open; reverse is one atomic admitted operation since M5.9, and broker and practice positions share one netted ledger since the 2026-10-22 futures venue contract (V28); the order plant is no longer unused since the Rithmic venue relay, and the rest stays with M8.10 |
| UI-01 | High | Practice venue market data is relayed through the chart UI | M3.8 (market side), M4.7 | closed in cd54c087 |
| UI-02 | High | Symbol/interval switching is a UI-owned state machine without generations | M6.3, M6.4 | open |
| UI-03 | High | Provider-specific identity, entitlement and wording in the desktop | M6.7, M6.8, M6.9 | open |
| UI-04 | Medium | Symbol selection takes two round trips and startup catalog resolution lives in the desktop | M3.10 (runtime part), M6.5 | M3.10 closes the two-step chart pick and desktop startup catalog ownership; M6.5 retains the broader picker cleanup |
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
- TR-12 (two position models): was intentional; superseded on 2026-10-22 by the maintainer's decision
  to replace the hedged broker model. Broker futures fills now share the practice net-position
  ledger (V28), so one position model remains.
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
  - Added safe `--validation-root`, hashed validation vault namespaces, scoped borrowing of the former FX/CFD broker's connection and per-root instance locks.
  - Readiness schema 5 reports the lock, log path and validation context; validation disables release side effects.
  - Scrutiny follow-up: the namespace is exactly `com.aeris.terminal.validation.<sha12>`, borrowing
    logs exactly once at initialization, and launcher promotion, update checks and AUMID registration use one tested deny policy.
  - User-testing follow-up: scripted venue tests distinguish a configured relay from an opened demo
    session; the attachment line appears only after the session opens. Root-overlap and fake-vault tests
    cover contained production roots and exact-key read/write-through with other keys isolated.
    A non-opt-in isolated desktop showed no venue attachment line in 60 seconds.
  - Follow-up: an unconfigured FX/CFD broker catalog warm-up no longer announces a skipped provider;
    the diagnostic test isolates the process-wide log sink with a non-recursive, 15-second-bounded
    child. The full gate passed; a fresh 60-second isolated GUI session rendered Hyperliquid with
    no FX/CFD broker session, venue or warm-up lines, and a forced-panic run left earlier log lines and one crash file.
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

Status: validated (2026-10-14) at `f00d4a99`; scrutiny and user-testing pass, VAL-MKT-001..030 all pass

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
    and display-depth workers use full jitter from 500 ms to 30 s; the former FX/CFD broker had
    a five-attempt failure with the last cause, while the former Level 1 broker and the shared
    FX/CFD broker venue reused bounded backoff.
  - Rithmic transport keeps only its adapter retry ticket; non-transport live
    replacement waits at least one second. A scripted adapter retry asserts
    exactly one concurrent ticker login. Tests cover policy bounds, capped
    retries, rate-limit headers and recovery, and FX/CFD broker failure publication.
    The blocked-network instance log is deferred to the single M2 milestone probe.
- [x] M2.5 market-calendar-service - Provider-neutral market calendar and session service
  - Added the session source resolver (provider calendar, contract hours, exchange rule, continuous, unknown), domain phase/window/source kinds, and shared exchange-code normalization. CME bucket and DST rules moved from the Rithmic adapter to the shared instruments domain; the runtime calendar owns dated phase/transition projection and weekly contract hours.
  - Runtime session status delegates to the calendar; interval resolution takes calendar source kind. Unsupported weekly/monthly trade-built demands produce a permanent `MarketDemandError` event and an error reply, including a non-CME equity 1W, rather than a series without live handoff. Existing FX/CFD broker daily cadence remained accepted without published hours.
  - Calendar matrix, four-provider source resolution, CME DST, unknown 1W/1M and non-CME equity 1W tests pass; naming_check rejects provider literals in generic calendar and adapter-owned calendar imports. Full local fmt, workspace clippy/tests and naming gate passed. MD-04's non-CME live aggregation remains M2.7.
  - User-testing round 1: every CME/DST, CFE, equity regular/extended, crypto, FX weekend/open and Unknown
    matrix row asserts phase, complete session window and next transition. The public
    `MarketService::market_session_status` is compared with calendar phase and source for all four providers;
    the three moved CME rule tests pass in `aeris_instruments`.
- [x] M2.6 provider-neutral-watchdog - Stall watchdog and practice session answer for every provider
  - Candle and trade handoffs use the injected clock for last progress. The watchdog evaluates both models for every provider against the calendar's open/extended/always-open phase, never retries while closed, and uses a 300-second threshold for unknown sessions (30 seconds while open). It no longer calls the former Level 1 broker's API from the coordinator.
  - Practice session admission retrieves the installed instrument from the coordinator and asks the same calendar for all providers; unknown and closed phases produce distinct errors. Removed the superseded single-provider practice gate.
  - Scripted FX/CFD broker open/closed and Rithmic CME no-trades recovery tests assert one current history request while open and none while closed; pure tests cover all four providers, unknown grace and practice crypto/CME/unknown cases. Full local fmt, workspace clippy/tests and naming gate passed. MD-02 market half closed; the trading-side gate remains M4.8.
  - User-testing round 1: scripted coordinator tests count exactly one current history request for each
    open-market broker, Rithmic CME and AlwaysOpen Hyperliquid series; Unknown requests none
    at 30 seconds and exactly one after 300 seconds. The injected clock advances without sleeps.
- [x] M2.7 calendar-session-live-bars - Session-day, weekly and monthly live bars follow the calendar
  - Trade handoffs use calendar session/day, week and month buckets rather than elapsed daily seconds or CME-only buckets. Coordinator owns the installed calendar; tape dates, watchdog and trade-built bars read its current source. Contiguous contract hours still roll daily at the local opening time.
  - Dated provider projections and holidays arrive through generation-fenced provider calendar events, take precedence while valid, and then fall back to contract/exchange rules or Unknown. The Level 1 broker catalog worker published bounded updates when its cached calendar changes and on instrument selection.
  - Tests cover CME daily rolls in normal and DST weeks, Day3 session rolls, all six non-CME weekly/monthly cases, tape trading dates, provider holidays and validity expiry. Scoped tests/Clippy and the full local fmt, workspace Clippy/tests/naming gate pass. The single M2 GUI pass remains scheduled after M2.10.
- [x] M2.8 typed-epoch-fencing - `Epoch<Tag>` and `Fenced<T>` replace raw generations at the provider boundary
  - Added non-arithmetic `Epoch<Tag>`, exhaustion-safe `EpochSequence`, `Fenced` and equality-only `Fence` in market_engine. The coordinator registry alone stamps adapter wire generations; provider events and history completions carry typed provider epochs, including dated calendar updates.
  - Stale provider events and history completions each increment their own bounded diagnostic count and cannot mutate current series or calendar state. Existing engine `ProviderGeneration` and `GenerationId` wrappers remain until their broader consumers migrate; raw adapter counters remain inside the wire boundary.
  - Scoped runtime/engine/naming tests and full workspace fmt/Clippy/tests/naming gate passed; naming_check rejects raw generations in the typed ProviderEvent.
- [x] M2.9 command-fences-consumer-registry - Command fences and one consumer registry; remove the startup sentinel
  - Replaced the five catalog pending maps with typed command-kind fences; desktop startup assigns a unique command generation and explicitly marks startup requests. The Level 1 broker retained independent startup and picker search lanes without a numeric sentinel.
  - `ConsumerRegistry::remove_consumer` is shared by detach and explicit remove; tests assert no command/outbox/client/demand/order-book residue on either path, and a superseded startup callback is dropped.
  - Market-runtime, desktop and naming scoped checks plus workspace fmt/Clippy/tests/naming passed. The single milestone GUI restore/search check remains after M2.10.
- [x] M2.10 history-tracker - `HistoryTracker` with current and backfill lanes; trim unused `provider_history`
  - One tracker replaces all eight coordinator ledgers and the retry tuple. Exact provider/demand/lane/sequence tickets travel through worker and provider completions; deferred, in-flight, retry, coverage, detached and cancellation state are pruned together.
  - The worker blocks on a full bounded completion channel and serves the oldest backfill every fourth slot. Removed the unused provider-history scheduler modules while preserving model/handoff/error and Rithmic conformance; fixed misplaced history comments.
  - Tracker API, boundary characterization, full fmt/workspace Clippy/workspace tests/naming passed. In an isolated desktop, BTC-USDC 1m history grew from 1,599 to 4,003 bars after panning, with zero interior gaps; ETH search, 1D chart and restart restore succeeded.
  - Scrutiny round 1: empty current-history completions now dispatch the deferred viewport backfill using the completed ticket's provider epoch; current failure/retry also drains it. Scripted-clock tests cover both empty branches, non-empty, retry and retired-epoch completion. Scoped and full gates passed.

### M3 market lifecycle

Status: validated (2026-10-15) at `530a7c8d`; scrutiny and user-testing pass, VAL-MKT-031..061 all pass

- [x] M3.1 series-lifecycle-candle-path - One per-series state machine, ported for candle providers
  - Round-2 evidence repair (VAL-MKT-034): the existing `AERIS_LIVE_SNAPSHOT` diagnostic now includes at most 32 `{start_utc_ns, missing_intervals}` records and `gaps_truncated` while retaining its existing interior-gap metric and diagnostic gate. The test checks a two-minute gap and 33-gap truncation. Two isolated borrowed-demo EURUSD 1m sessions, current and detached pre-M3 `f00d4a99`, each showed the same seven gaps (37 minutes) in their snapshot and their 1,601-bar raw decoded page, with both adjacent bars present and no page seam. Sanitized UTC-only primary pages, snapshots and a per-gap table are in the mission `validation/market-lifecycle/evidence/` (EURUSD folder); temporary instrumentation was removed, no orders placed.
  - User-testing coverage (VAL-MKT-031): thirteen separately named `lifecycle_` tests exercise each section 2.4 transition row, including bounded candle buffering, Dormant reopening and trade, and stale input rejection. Rolled-Dormant candle and trade regressions failed before the lifecycle-owner fix and now request reseeding instead of resuming stale live data.
  - Moved FX/CFD broker and Hyperliquid candle state, the single forming bar, seed/recovery transitions, bounded publications and presentation status into `lifecycle::SeriesLifecycle`; removed the old candle handlers and publisher. History uses its exact tracker ticket and the engine remains the canonical completed-bar owner.
  - Scripted lifecycle transitions, published status order, four coordinator characterization scenarios, crate-scoped tests and the full fmt/workspace Clippy/workspace test/naming gate passed. Isolated Hyperliquid 1m had 1,599 bars, zero gaps and forming updates; EURUSD demo had 1,600 bars and live updates.
  - EURUSD's 10 interior gaps are provider omissions, not Aeris-created: an opt-in diagnostic of the **raw decoded FX/CFD broker trendbar page** showed exactly 10 gaps (44 missing minutes; starts in UTC seconds: 1791923340, 1791925200, 1791925320, 1791925500, 1791927060, 1791927180, 1791927480, 1791927660, 1791933900, 1791934320). The pre-M3 M2 binary on a separate isolated root showed the same 10 gaps over the overlapping interval; no history-page seam falls in these 10 gaps. The temporary raw-page logger was removed after capture. No credentials or account identifiers were recorded.
- [x] M3.2 series-lifecycle-trade-path - Port trade-built providers and delete the duplicated lifecycle
  - CandleOverlay, Indexed (Level 1 broker) and Sequence Rithmic series share `SeriesLifecycle.state` and one bounded publication turn; trade aggregation lives in `lifecycle/trade_model.rs`. Removed the split trade registry, state enum, named Rithmic handlers/publisher and redundant history handoffs.
  - Hyperliquid candle events compare the cached demand coin/interval without formatting a wire symbol per event. Indexed recent trades prune by time, provider errors are neutral, and demand reconciliation no longer doubles `demand_dirty`. The bounded Indexed correction overlay still recomputes retained bars.
  - One table-driven suite exercises all three models, exact ticket rejection, recovery, contiguous completed bars and one forming bar; a scripted adapter-confirmed Rithmic SequenceGap recovers through covering history to Live. The naming gate rejects `SeriesLiveMap` in production with positive/negative scanner fixtures. Scoped and full fmt/workspace Clippy/workspace test/naming gates passed; Rithmic live qualification remains excluded.
- [x] M3.3 engine-series-tail - Series-tail accessor; remove full-series copies from hot and existence paths
  - Added canonical `series_tail`, timestamp existence and borrowed/owned segmented views. Study calculations and output timelines share covering history and sealed 64-bar chunks, copying at most the open chunk; live diagnostics, study bounds and history/publication existence avoid full snapshots. One `execute_study_change` dispatch replaces the five coordinator wrappers. Full covering snapshots remain for recovery, repair and explicit resync.
  - Release benchmark (Windows x86_64, 24 logical processors, 10,000 forming revisions, 63 publishes per size, bar-payload capacity lower bound, excludes allocator/header overhead): M2.2 baseline at 50,000 bars **3,200,064 bytes/publish**; M3.3 tail at 1,000 / 16,384 / 50,000 completed bars **128 / 128 / 128 bytes/publish** (50k:1k ratio **1.000**, limit 1.100). The 50k segmented study-view workload copied zero open-bar payload bytes per forming revision (293 ns/view for 256 ticks); the legacy full-copy reference was 3,200,000 bytes/tick (M2.2: 440,159 ns/tick; distinct workload, not a speedup claim). Evidence command: `cargo run -p aeris_market_engine --release --example market_data_performance --locked -- <out.json>`.
  - Segmented bounds and chained study outputs match full canonical snapshots; chunk rollover and historical correction preserve immutable older views. Focused engine/runtime tests and Clippy plus workspace fmt/Clippy/tests/naming passed. Rithmic live was not used.
- [x] M3.4 coordinator-dirty-publication - Dirty-set publication and deadline timers in the coordinator loop
  - Coordinator blocks with no work and uses injected monotonic deadlines for watchdog, history retry, session transitions, tape publication and bounded control retries; only dirty series publish. Shutdown sends a bounded wake. Scripted idle/deadline and one-series publication tests, four characterization scenarios, workspace fmt/Clippy/tests and naming gate passed.
  - M2.2 release coordinator baseline (`20abd4cc` plus M2.2 diff, Windows x86_64, 24 logical processors, synthetic 64-series workload): idle active coordinator CPU 82,000/74,200 ns per simulated second, loaded 4,018,600/4,530,100 ns, idle/loaded 63 wakeups/s. M3.4 pre-commit release workload (same host, 64 series, 63 loaded batches; `cargo run -p aeris_market_runtime --release --example coordinator_performance --locked`): idle zero active turns and 0 ns/s, loaded 4,613,700/4,401,500 ns/s, idle 0 wakeups/s, loaded 63 wakeups/s. This is synthetic active single-thread time, not desktop process CPU; idle zero reflects no scheduled turns, and loaded CPU did not consistently improve.
  - Release desktop CPU, same host, fresh isolated validation roots, default live Hyperliquid BTC-USDC 1m, 30 s settle then one 60 s `Get-AerisProcessMetrics -CpuSampleSec 60` sample (% of one core): archived M2.2-era source at exact `20abd4cc`, kit disabled, **13.9%** (PID 35720); M3.4 pre-commit working tree, kit disabled, **12.8%** (PID 33228). Improvement 1.1 percentage points (~7.9% relative); one sample per revision, not a sustained-load guarantee. Each desktop and its host exited gracefully before the next launch; no production data root or stored broker connection was used.
- [x] M3.5 provider-capabilities - `ProviderCapabilities` and `ProviderPresentation` replace descriptor flags and literals
  - Split internal provider registration into behavioral capabilities (engine streams, live model, calendar, history policy, concurrency, reconnect, depth and quote availability) and presentation metadata; corrected Rithmic selection scope and ready label, and moved interval-to-Rithmic replay aggregation to the adapter with exact interval tests.
  - Generic lifecycle/history/calendar/coordinator/realtime/runtime/publication files reject provider-id literals in production, including after test modules; fixture tests cover each path (quotes and outbox when introduced). Provider-specific invalidation wording moved to provider-event translation. The remaining provider dispatch enums are deliberately M3.6.
  - Workspace fmt, Clippy `-D warnings`, tests and naming_check passed. Characterization (4), lifecycle (12), aggregation and descriptor matrix suites passed; no Rithmic live session or GUI run (internal-only item; M3.4 GUI validation completed).
- [x] M3.6 provider-session-registry - `ProviderSession` trait and `ProviderRegistry`
  - Round-2 SOL interval investigation (VAL-MKT-043): a fresh isolated BTC-USDC 1m pane selected SOL-USDC once and remained at 1m. The picker passes `WorkspaceSurface::interval` unchanged into its catalog selection, so a 1W SOL after an earlier timeframe action is the pane's carried interval, not a SOL-specific or fresh-root default. The round-2 GUI report lacks a complete ordered input trace to identify the earlier 1W action; this explanation and the fresh-root check are recorded in the mission library.
  - User-testing coverage (VAL-MKT-042): scripted two-provider demand verifies selective idle stop while another provider retains book demand; concrete interval, viewport-visibility, tab and layout changes diff without extra connects.
  - Part 1/3: the Level 1 broker HTTP transport is borrowed outside the catalog-cache mutex and returned on all paths; authorization revocation retires an in-flight transport. Two regression tests and the full workspace gate passed. Registry and per-provider idle work remain before this item can be checked.
  - Part 2/3: idle release and stop-overlap check only the stopping provider's own series, book and alert demand; a scripted two-provider book test guards the overlap. Full workspace gate passed with the CI debug profile and one Cargo job.
  - Part 3/5: catalog command/response dispatch now has one bounded, provider-keyed channel boundary. Deleted `ProviderCatalogChannelSet` and `ProviderCatalogDispatch`; channel tests cover typed round trip, capacity and screen entitlement. The registry-minted pre-connect epoch, realtime channel/dispatch removal, `ProviderSession` and desktop session check remain; item is deliberately unchecked. Catalog migration preceded the epoch move without adding a second epoch or retry owner.
  - Part 4/8: Hyperliquid realtime now uses a registry-reserved epoch before connect, an epoch-bound event sink, a single coordinator backoff and diff-based `ProviderSession` demand. The worker no longer schedules socket retries; its arm of the realtime dispatch enums is gone. Scripted registry tests cover demand changes without reconnect and epoch reservation on a due retry. The two former broker providers and Rithmic retain their previous paths until their respective migration parts; M3.6 remains unchecked.
  - Part 5/8: FX/CFD broker market/catalog traffic now uses the registry-reserved, epoch-bound session with registry-owned market reconnect and diff-based demand; its dedicated bounded channels replace its realtime dispatch arm. The shared broker trading relay and its trading-only retry remain intact. Scripted tests cover catalog-only socket failure, demand changes without reconnect, retry budget/server wait, and shared catalog/market epoch; 38 FX/CFD broker tests, four characterization cases, and the full workspace gate passed. An isolated demo desktop loaded AUDCAD 1m, then AUDCAD 1h and AUDCHF 1h with live updates and a single market-session-open log entry; provider-omitted history gaps were observed, not filled. The Level 1 broker and Rithmic still retain their previous paths until parts 6 and 7; M3.6 remains unchecked.
  - Part 6/8: the Level 1 broker uses a registry-reserved pre-connect epoch and bound realtime/history events; the worker no longer increments a transport generation or retries sockets. Registry backoff owns transient and server-rate-limit retry; local subscription-budget pacing stays in the worker. Its realtime dispatch enum arm is removed. Scripted demand, epoch and rate-limit tests and four characterization cases pass. Rithmic remains on its original sole transport retry path until part 7; M3.6 remains unchecked.
  - Part 7/8: Rithmic now binds adapter events and history to a registry-reserved epoch; adapter transport replacement emits `TransportReplaced`, which reserves a new epoch and rebinds without opening another socket. The adapter remains the sole Rithmic transport retry owner. Deleted the remaining realtime channel/dispatch enums, session slot and production stamp-after-event path; scripted registry and coordinator tests cover replacement, stale wire retirement and demand changes without reconnection. Four characterization cases and the full workspace gate passed. An isolated Hyperliquid desktop switched BTC to ETH to SOL and SOL 1m to 1h to 1m without a logged reconnect; each captured snapshot had zero interior gaps. On fresh startup BTC initially displayed Loading despite completed history, then rendered after selection changes; this separate startup-resolution issue remains under investigation. Rithmic live validation is deferred until kit and credentials are available, and Level 1 broker live validation required fresh OAuth.
  - Part 8/8: `ProviderRegistry` scripted checks record one connection and five named demand changes (symbol, interval, viewport, tab, layout) for each of the four providers. Diagnostics now record each actual Hyperliquid socket open and do not mislabel intentional shutdown cancellation as a reconnect. In the isolated diagnostics desktop at `m3-6-registry-qualified`, UIA identified BTC-USDC, ETH-USDC, SOL-USDC, 1h, then 1m; the stderr and rotating logs each record exactly one Hyperliquid session open (`epoch=2`), no reconnect or connection failure, and snapshots for ETH, SOL 1m, SOL 1h and SOL 1m with `interior_gaps: 0` (1,599/1,599/1,599/5,222 bars). The desktop and host exited gracefully. The fresh-default BTC Loading state persists before a manual symbol change, despite completed history, and is not represented as resolved.
  - Scrutiny round 1 repair: a full control queue consumes the registry Backoff budget and installs only future deadlines (including zero-jitter draws); a disconnected worker becomes terminal Failed with a cause published once through the coordinator. ScriptedClock registry/coordinator tests bound turns and check both failure paths; demand/history/idle/rebind dispatches are reviewed, and history-only runtimes no longer register a nonexistent realtime worker. The full workspace gate passes.
- Startup regression repair (follow-up `fix(market): reserve provider generation before startup history`): detached fresh-root `f00d4a99` and `2f3fd729` builds both rendered BTC-USDC 1m without input; the M3.6 head loaded history but displayed Loading. The first history request captured catalog generation 1 while registry connection reserved epoch 2, so its completion was discarded; synchronous lifecycle activation at reservation now assigns generation 2 before demand/history. The deterministic pre-event ticket test failed before the change and passes after it. A fresh-root fixed desktop rendered 1,599 BTC candles with zero interior gaps, forming updates and one epoch-2 socket without manual selection. Repeat this fresh-default check at M3.10 and M3.12.
- [x] M3.7 instrument-leases-feed-contract - Trading feed domain contract and MarketEngine instrument leases
  - Added validated provider-neutral `aeris_trading::market` routes, exact-scale non-crossed quotes, bounded quote interest, typed feed states/session/terms and events; added `InstrumentClass` and `PriceGrid` on durable contract metadata with legacy Unknown/Other defaults and round-trip coverage. Unverified Hyperliquid grids remain Unknown; M3.8 owns provider terms conversion and verification.
  - Re-keyed the single `DataLeaseRegistry` with `MarketDemandKey::{Series, Instrument}` and one explicit 320-lease runtime bound (64 previous series leases plus 256 quote interests). Owner replacement preflights and is atomic; entitlement keys stay distinct; consumer retirement releases owned leases. `instrument_subscriptions` merges shared demand, and the market-service `QuoteSubscription` guard enqueues a bounded nonblocking drop release processed by the coordinator. Provider quotes-only subscriptions, event delivery, `latest()` and the trading/alert feed integration follow in M3.8, not as parallel M3.7 paths.
  - The complete workspace fmt, strict Clippy, tests and naming check pass, including the four characterization tests, quote precision/crossed-touch, owner sharing/capacity, guard drop/release, durable field round trip and legacy metadata fallback. No GUI behavior changed in M3.7; the probe pass follows the final visible M3 item.
- [x] M3.8 trading-quote-feed - Market-side trading quote feed driven by instrument demand
  - User-testing coverage (VAL-MKT-058): coordinator-driven chart and price-alert quote interests share upstream demand through the first release and unsubscribe only after the last release.
  - Part 1/3: quote-only instrument leases now reach the shared provider demand and canonical book without creating chart series; Hyperliquid subscribes BBO without candles, the broker providers and Rithmic retain their session-owned quote mapping. `QuoteSubscription::latest` reads the bounded coordinator-updated quote projection, validated against the route; release removes the book and quote and changes provider demand. Scripted no-chart quote/release and invalid-spread tests, 381 market-runtime tests (4 ignored), and full workspace gate passed. Trading interest sink, feed events, alert lease migration, terms/grid conversion and live BBO example remain in parts 2-3; item stays unchecked.
  - Part 2/3: revisioned trading quote interest installs and replaces leases in the engine owner. Active price alerts now hold the same owner-scoped instrument leases, including after one-shot triggers and consumer removal; the retired alert-only provider demand merge and its superseded flags are removed. Scripted tests cover transactional interest changes and no-chart alert trade subscriptions. Feed events, runtime terms conversion and live BBO example remain; item stays unchecked.
  - Part 3/3: `TradingFeedLink` delivers bounded latest-wins Quote/Feed/Session/Terms events on its own worker; current catalog and leased sessions replay on attachment, chart quote streams also emit without a separate instrument lease, session transitions use the coordinator's calendar deadline, and retired provider quotes are invalidated. Runtime converts validated provider terms to `InstrumentTerms` and the desktop uses that projection instead of maintaining a separate price-grid/class conversion. Hyperliquid's documented five significant figures and `6 - szDecimals` perp (`8 - szDecimals` spot) grid are sourced from catalog metadata rather than normalized quantity scale; invalid size precision rejects catalog data. An isolated-root, lock-held `hyperliquid_bbo` example subscribed BTC perpetual quotes without any series and printed 20 distinct positive uncrossed price changes at scale 8 within 30 s, exited 0. The scripted no-chart/chart-active, reconnect/feed, catalog terms, session transition, latest-wins, grid and precision tests passed; full workspace gate passed. Live Rithmic qualification still requires gated kit/credentials. No GUI changed, so the GUI pass follows the final desktop-visible M3 item.
- [x] M3.9 fenced-events-waitable-outbox - Every consumer event is fenced; the bridge blocks on a waitable outbox
  - Part 1/3: The coordinator owns the existing bounded consumer queues and signals an attached client's `ConsumerOutbox` through `Mutex`/`Condvar` after publication; a retiring client loses its pending wake. The desktop background bridge waits until a publication, UI command, or its 60-second calendar deadline, rather than polling every 2 ms. The bounded drain command replaces the old `poll_events` round trip without changing the 1,024-update covering-snapshot overflow. UI command send/slot and message-drain edges wake the bridge without blocking GPUI. Scripted publication/wake, client retirement, four characterization cases, desktop bridge tests and the full gate passed.
  - Offline `coordinator_performance` measured 256 coordinator-to-bridge publication wakes: p50 1,000 ns, p99 2,900 ns; one attached consumer idle for 30 s received 0 notifications (0/s), versus approximately 500 bridge polls/s before this part. This is a debug-build local benchmark, not a process-wide CPU claim.
  - Part 2/3: All foreground selection, viewport, resource-class, depth and alert commands now use the endpoint's one bounded FIFO command sender; the parallel mutex slots and priority-before-queue handlers are deleted. Only adjacent latest-value updates coalesce while draining the same queue; a click is an ordering barrier. Saturation returns an explicit error without advancing the selection sequence or suppressing chart publications. Test-first click-then-viewport initially failed because the viewport overtook the selection, then passed after the move; the full workspace gate passed.
  - Part 3/3: Every runtime consumer event exits in a `Fenced<ConsumerTag, MarketRuntimeEvent>` envelope with a fresh epoch per registration; retired consumers lose their epochs. The coordinator rejects selection-bound snapshots, deltas, state, depth, tape and study events at drain when their exact demand generation is no longer installed. Study output/removal/invalidation carry that demand generation; catalog callbacks retain their existing command fences, and alerts are consumer-lifetime interests rather than chart selections. The naming check rejects unfenced public delivery even after test modules; stale-study and retirement tests, four characterization cases and the complete workspace gate pass. `EVENT_WAIT|poll_events` is absent under desktop and market-runtime sources; the Rithmic adapter still has its independent 2 ms transport/control wait, now explicitly named `ADAPTER_CONTROL_WAIT`, not a desktop consumer poll. An isolated desktop selection pass at 07:23:47, 07:23:57 and 07:24:08 UTC changed the UIA symbol button in 141/59/159 ms and printed the new symbol's covering-history completion in 348/270/369 ms, respectively (BTC→ETH→BTC→ETH); all three met the 2 s/3 s limits. The desktop exited gracefully; no live Rithmic session was attempted.
- [x] M3.10 select-and-demand-descriptors - `select_and_demand` command and runtime-published instrument and provider descriptors
  - User-testing follow-up (VAL-MKT-054): the native symbol search now clears its query on close and ignores delayed edits while closed; a GPUI test verifies Ctrl+A replacement and empty query after reopening. A posted-window-input storm still needs isolated GUI qualification.
  - Part 1/3: the runtime publishes provider descriptors directly from its four registered providers, with typed connect methods, logos, browser/autoload affordances and supported intervals. It also publishes instrument metadata with explicit unknown terms and resolves selected series through the existing calendar-aware cadence owner. Tests assert complete descriptors for every registered provider and fixed UTC versus CME session daily cadences; the full workspace gate passed. Transactional selection, startup catalog ownership and typed readiness follow; this item stays unchecked.
  - Part 2/3: `MarketService::select_and_demand` submits one coordinator command carrying the consumer, generation, catalog reference or ungated public default, interval, streams and viewport; a typed `Selected` or `Failed` result returns to the caller. Calendar/identity/viewport errors leave the previous demand untouched, while a failure after provisional installation restores the previous engine demand, subscription, viewport and queued events under the same coordinator turn. Tests cover selected/stale generations, an unsupported interval preserving live deltas, an ungated registered default, and exact rollback; the full workspace gate passed. The desktop still uses the old bridge path until part 3/3 moves its startup catalog and selections together; M3.10 remains unchecked.
  - Part 3/3: The coordinator fences one pending asynchronous catalog selection per consumer, emits one outbox outcome per epoch before the selected snapshot, rejects late superseded callbacks, and resolves catalog symbols from picker entries without a desktop-side chart selection round trip. Desktop startup sends only runtime selection; the picker no longer waits for a removed startup search; readiness uses `CatalogReady` rather than display-text matching. One bounded endpoint command queue/outbox remains. Scripted supersession and catalog callback tests, the complete fmt/Clippy/workspace-tests/naming gate, and a fresh-root default BTC chart without input passed.
  - Isolated live evidence at `m3-10-final`: one `ETH-USDC` invoke closed search and published a 1,599-bar zero-gap snapshot within 5 seconds. Ten alternating BTC/ETH search-result invokes ended on ETH at the ETH price axis; the last zero-gap snapshot was followed by live updates, and the chart still showed only ETH prices 30 seconds later. One Hyperliquid socket opened at epoch 2; the validation desktop and host exited.
  - User-testing repair (VAL-MKT-034): the desktop now forwards the completed search generation separately from its selection generation. The coordinator retains the completed picker search identity in `CommandFences` until replacement or retirement and dispatches the broker selection with that identity, without another result cache. The scripted broker channel regression failed before the fix and passed after it; 419 market-runtime tests and scoped Clippy passed. In an isolated, borrowed-demo desktop, search selected EURUSD and rendered candles; an immediate restart emitted a 1,600-bar EURUSD snapshot, but its 7 interior gaps require the raw-provider/baseline comparison from the VAL-MKT-034 ruling before the full assertion can be called passed. No orders were placed.
  - User-testing coverage (VAL-MKT-052): a scripted current-epoch broker `Connected` publishes typed `MarketRuntimeEvent::CatalogReady`, while an otherwise identical retired-epoch `Connected` publishes nothing and increments the stale-event count. No duplicate provider event variant was added.
- [x] M3.11 market-replay-harness - Deterministic market replay harness with permanent scenarios
  - User-testing coverage (VAL-MKT-056/057/058): 50-selection replay injects retired history and live events and checks every final bar's identity; reconnect/gap recoveries assert Live, zero interior gaps and one forming bar, while stale history drops and exhausted backfill stay fenced. Coordinator tests verify broker open/closed stall detection, CME Day1 published rollover across DST, calendar variants and Hyperliquid retry schedule; Rithmic transport retry stays adapter-owned, with its nontransport delay verified at the adapter boundary.
  - `ReplayHarness` drives coordinator commands and scripted registry history/demand through the existing bounded event boundary. Fifteen named scenarios cover 50 alternating selections with late completions, epoch rebind, typed sequence gap, one forming bar, exhausted backfill, overflow, stalls, calendars, backoff, leases and Day3; the private-map sequence-gap test moved to the harness.
  - The sanitized Hyperliquid public candle-shape fixture contains no private fields. Two consecutive replay runs, the four characterization tests and the full workspace gate passed. Existing private-map tests for unrelated behavior remain as complementary coverage, not a second replay implementation.
- [x] M3.12 hyperliquid-lifecycle-probe - Live Hyperliquid market lifecycle probe with JSON evidence
  - Final-HEAD qualification (VAL-MKT-059, user testing round 3): on clean `main` at `088899c4e0c70cfceb43e4fefc21e1871bfe34e5`, `market_lifecycle_probe --instrument BTC --duration-secs 120` on a fresh isolated root exited 0 in 36 s with `aeris.probe.v1`, `outcome: passed`, 28/28 checks passed (none failed or skipped), `git_revision` equal to HEAD, `source_clean: true`, and zero secret-like JSON matches. The negative run with `LOCALAPPDATA` overridden to a fresh temp folder and `--validation-root <tmp>\Aeris` exited 1 with `the validation data folder must not equal or contain the production data folder`; `<tmp>\Aeris` and the output stayed absent, and the real production folder was not accessed. This entry supersedes the requalification note below; only this tracking line changed after that revision.
  - Round-2 positive run (VAL-MKT-059): after the probe selection fix, the fresh isolated root `m3-round2-probe-5e9bfb32` produced `aeris.probe.v1`, `outcome: passed`, 28/28 passed checks, `git_revision: 5e9bfb32b51176945197c2c83cecfc56f2baf20b` equal to HEAD at execution, `source_clean: true`, and no secret-like JSON fields. Exit 0; the negative overridden-production-root refusal had already passed in round 2. Requalify the final HEAD after all three commits because subsequent documentation and diagnostics commits change the revision.
  - Round-2 regression (VAL-MKT-059): the probe alone supplied `CatalogSymbol.search_generation = 0`, which the catalog-selection validator rejects. Symbol switches now use stable Hyperliquid perp identity via the coordinator's `InstrumentRef` path; a focused test checks ETH identity and BTC default. A clean-revision 120-second live run follows this commit.
  - User-testing repair (VAL-MKT-059): validate the root before collecting Git evidence, so an overridden-`LOCALAPPDATA` production overlap reports the refusal instead of masking it with a missing-path error. The built probe child exited 1 with `the validation data folder must not equal or contain the production data folder`; its fresh `<tmp>\Aeris` stayed absent. The child-process regression passed; no real production folder was used.
  - Part 1/2: added the workspace `aeris_probes` headless binary, strict isolated-root and locked-run CLI, `aeris.probe.v1` evidence envelope, bounded consumer observation, continuity/quote/paging/session/shutdown checks, and a feature-gated WebSocket-only failure control. The registry records at most 16 exact retry schedules and attempted times; HTTP catalog/history remain available during injected failures. Probe-only socket faults must not use process-wide proxies (also recorded in `AGENTS.md`).
  - Four tool tests and the WebSocket isolation unit test pass, as do fmt, strict workspace Clippy, workspace tests and naming. A dirty-source live preflight reached every market check with 0 gaps for 1m/1h/1d, one session through switches, one-epoch forced recovery, a live quote, older contiguous pages and a closed 1m bar; as designed its `source_clean` check failed.
  - Part 2/2: the clean-source `--instrument BTC --duration-secs 120` run on isolated root `m3-12-qualified` exited 0. Its `aeris.probe.v1` JSON reports `passed` for every check, `source_clean: true`, revision `6d559cd7e73e028babd595a714b542c8d5662189`, 0 interior gaps in 1m/1h/1d and 2,399 older contiguous bars, forming seen after 1,335 ms, one contiguous 1m close, UTC-midnight 1d bars, and a live quote of age 0 ms. BTC to ETH to SOL and 1m to 1h to 1d to 1m retained epoch 2 and one session; forced recovery advanced epoch 2 to 3, returned Live with one forming bar and zero gaps. The WebSocket-only retry records show scheduled delays of 494/407/973 ms and observed delays of 497/419/988 ms against full jitter over 500 ms × 2^n capped at 30 s. Fenced stale counts were 0 observed (not an injected stale event), outbox high-water was 2/1,024, and shutdown took 41 ms. The JSON secret-like scan had no matches; a production-root invocation exited nonzero before writing there.
  - On separate fresh isolated root `m3-12-desktop`, an untouched startup rendered the BTC-USDC Hyperliquid 1m candle chart without manual selection. The window screenshot shows candles and a live price marker; stderr records one epoch-2 session, a 1,599-bar snapshot with zero interior gaps, and forming updates. The desktop and host exited gracefully.

### M4 trading core

Status: validated (2026-10-16) at `3529116c`; scrutiny and user-testing pass, VAL-TRD-001..029 and VAL-CROSS-009 all pass (GUI assertions use retained isolated-run evidence)

- [x] M4.1 trading-clock-command-time - Injected trading clock; cancel events at command time
  - Injected `Clock` via `TradingServiceConfig`; owner command stamps clamp backward wall steps. The cancel event uses the owner command time rather than the order's old observation. `current_unix_nanos` is removed; direct production time reads remain in `clock.rs` only.
  - `cancellation_uses_command_time_even_when_wall_clock_moves_backwards` and both golden tests pass. The sole golden change is `simulated_stop_golden.txt` cancel event `2000 -> 3000`; the other golden is unchanged. The workspace fmt, Clippy and test gates passed; naming_check passed after adding the clock trait to the documented boundary list.
  - GUI cancel-time verification is blocked at this stage: an isolated practice-account order-book bid click returned "trading instrument is not registered" before any order could be placed. M4.7's runtime Terms registration must land before repeating VAL-TRD-003. The current Order model has no terminal-time field; the last Cancelled event supplies that time until M5's lifecycle schema adds an updated time.
  - Pre-M4.2 regression triage: the detached pre-M3 `f00d4a99` build accepted a live Hyperliquid practice ladder order on a fresh isolated root; `6b248b2f` rejected the same flow as "trading instrument is not registered". M3 default selection installed provisional metadata without requesting verified catalog terms. The coordinator now requests those terms asynchronously behind the startup command fence while retaining immediate chart selection. A second M3 regression removed the fixed tick for Hyperliquid's significant-figure grid; bracket offsets now use the exact local increment at the entry price, and quote handling with no active bracket never demands a tick. A fresh isolated desktop after both fixes showed a working Buy Limit line and "Practice order ... is working"; no golden files changed. VAL-TRD-003's 60-second cancel-timing GUI assertion remains to be run.
- [x] M4.2 trading-core-owner-shell - Thin owner thread around a synchronous `TradingCore`
  - Moved bounded `Command` to `service/mod.rs` and the single owner wait loop to `service/owner.rs`; `TradingCore` serializes `handle(command, now)`, `on_market(batch, now)` and due `on_timer(now)` without changing the current durable order path or goldens.
  - Eight concurrent submitters place 1,000 distinct orders with contiguous event sequences and revisions preserved across restart. An idle `ManualClock` owner retains zero timer calls across 1,000 status commands; its named thread is asserted. TR-12 god-object split has begun, but the broader finding remains open for M4.5/M4.6/M5.9/M8.10.
- [x] M4.3 trading-replay-harness - Deterministic trading replay harness
  - Moved the two frozen simulated scenarios into `src/replay/` without changing either golden; both now drive `TradingCore` directly against a temporary SQLite file and compare byte-identical transcripts across independent runs.
  - Shared core startup with the production owner; replay restart drops/reopens its SQLite connection, and a test-only SQLite commit hook aborts the selected commit. Seeded, bounded xorshift runs check order/event consistency and restart recovery at each step, and report reproducible seed/step failures. The hook dependency is dev-only; no new production boundary or restriction.
- [x] M4.4 indexed-order-book - Indexed order book; remove linear scans
  - `book::OrderBook` owns canonical orders and client/broker, open `(account, instrument)` and executable instrument indexes; insert, replacement, account purge and retention removal maintain all views. Replaced client, broker reverse, scoped cancellation and market candidate scans without changing order or golden behavior.
  - Seeded 4,000-step insert/transition/broker-bind/remove property test recomputes every index after each operation; trading crate tests and scripted broker/golden scenarios pass. On this Windows machine, release-mode 50,000-order/1,000-client-lookup comparison: former full scan 409.646 ms; indexed lookup 0.281 ms. Command: `cargo test -p aeris_trading_runtime --release --locked -- book::tests::benchmark_50k_order_lookup --ignored --nocapture` (same book and machine, timed lookup loops only).
  - TR-05 remains open for M5.11 publication and M6.10 desktop snapshot work; no persistence schema or golden changes.
- [x] M4.5 trading-core-module-split - Split `lib.rs` and the Coordinator into focused modules
  - Moved the existing `TradingCore` method bodies into admission, execution, ledger, risk, groups, emergency and market modules; moved protection/planned-leg and broker implementations without changing the single owner, store schema, golden transcripts or scripted venue tests. `lib.rs` now declares modules and re-exports the public surface.
  - Pre/post trading characterization: 108 passed, 4 ignored; full workspace fmt, Clippy, tests and naming_check passed. TR-12's remaining store split and later cleanup stay assigned to M4.6/M5.9/M8.10.
- [x] M4.6 trading-store-module-split - Split `store.rs` by table family
  - Moved the V1–V19 migration table, schema preflight and backup into `store/schema.rs`; split the existing SQL methods into orders, fills, ledger, groups, risk, user records, retention and export modules. `StepTx` wraps a rollback-on-drop SQLite transaction and is first used for order insertion; M4.10 will apply it to whole owner steps. Schema stays V19; no golden or existing assertion changed.
  - Store open sweeps at most 32 exact-name stale preflight directories, deleting only known copied SQLite files; unknown contents are preserved and cleanup failures are logged through observability. Deterministic tests cover stale cleanup and a refused unsafe directory.
  - Before the split, 108 trading tests passed (4 ignored). After the split, 110 passed (4 ignored), including all historical migration, persistence/restart, scripted venue and golden tests. Existing store-test suffix is byte-identical; full workspace fmt, strict Clippy, tests and naming_check passed. TR-12 remains open for M5.9 and M8.10 cleanup.
- [x] M4.7 runtime-owned-quotes - Trading runtime owns its quotes; the desktop relay is removed
  - M4.7 part 1/3: The market owner projects only provider-verified expiry, first-notice, last-trade, weekly hours and session generation into `InstrumentTerms`. On restored quote leases without catalog entries it searches and selects the exact saved route through a bounded, generation-fenced provider catalog request; no terms are synthesized. Feed attachment snapshots include only quotes from the current provider generation.
  - Market/domain crate checks and the full fmt, workspace strict Clippy, workspace tests and naming gate passed. Focused tests cover invalid dates, restored route resolution with no chart, rejected early/retired callbacks and current-generation snapshot quotes. Execution still uses desktop observations alone until the atomic part 3 cutover; M4.7 remains unchecked.
  - M4.7 part 2/3: Trading owner registers verified Terms and persists their routes in V20; migration leaves old instruments without invented routes. Owner-fenced QuoteBook, bounded latest-wins feed inbox, 64-ticket and 256-total interest follow working orders, positions and managed groups, with restart republishing. Session events now carry provider generation. The feed is deliberately **not** an execution input; existing desktop observations alone still drive fills until part 3 wires composition and removes the relay. V20 is occupied by route persistence, so the planned M4.9 fill-accounting migration must use V21 or later; no M4.7 GUI/no-chart stop claim is made yet.
  - M4.7 part 3/3: The desktop composition root attaches both market/feed directions before accepting trading commands. The trading owner executes feed quotes from its QuoteBook, stamps desktop entry/modify instructions with owner quote provenance and prices chart bracket entry type from its current touch; removed the desktop book relay, direct instrument registration, every desktop observation construction and the old pending-observation mailbox. A production naming rule rejects any desktop `SimulatedMarketObservation` reference, including code after a test module. A no-chart fixture covers fill, marks, stop and restart with recovered route demand. TR-01, UI-01 and the trading part of MD-11 are addressed in code; GUI pane-free stop/bracket remains outstanding, so M4.7 stays unchecked.
  - Part 3 verification: crate-scoped checks and tests, full workspace fmt, strict Clippy, all-feature tests and naming-check tests/scanner passed. On an isolated Hyperliquid desktop, a practice buy filled; a resting bid placed at 20:15:52 UTC was canceled at 20:17:12 UTC (SQLite event times). Switching the only BTC chart to ETH removed BTC series demand while the BTC position mark changed from USD 18 to USD 13 in 30 seconds; after restart on ETH, BTC marks resumed (USD 84 to USD 70). A GUI stop or bracket fill after closing BTC demand was not established; the deterministic no-chart test covers it. M4.9's fill-accounting migration must start at V21 or later because route persistence already occupies V20.
  - GUI qualification follow-up: `6ed03a11` records a clean-account BTC bracket within five verified USD 1 grid steps of touch; the BTC pane was removed before the parent and exactly one stop leg filled, the target was canceled, and the position was flat within 14 seconds. The earlier pane-free t0/+30/+60/+120 mark sequence passed VAL-TRD-014. Evidence and the M4 working-order evidence ruling are in mission `library/user-testing.md`; the M4.7 item is now complete.
  - M4 scrutiny round 1 (VAL-TRD-011): Restored snapshot controls now precede the cached quote both when the outbox is queued and when its worker drains between coordinator publishes. The keyed outbox still coalesces one event per instrument/kind within 1,024 entries; overflow retains Terms, Session and Feed ahead of Quotes. A deterministic separate-drain regression failed before the fix and passes now; the other EventKey kinds and overflow priorities are covered.
  - M4 user-testing round 1 (VAL-TRD-012): separate QuoteBook tests cover non-positive bid/ask, equal/crossed spread, wrong bid/ask scale, older generation and lower same-generation sequence without retiring the valid quote. A mixed 1,025-control inbox test proves oldest-Terms eviction, FIFO retention, newest quote, resync and an owner-driven full refresh through the recorded interest sink. An undrained owner command queue and a producer thread prove bounded push completion and one drain wake. Trading crate check, 145 tests and strict Clippy passed.
  - M4 user-testing round 1 (VAL-TRD-009): A replay-only scripted market provider composes through the production market owner and the desktop's two feed attachments, with no runtime-to-runtime production edge. The desktop integration test proves an active trading lease with zero chart subscriptions, successive published mark changes, stop fill at bid and flat position; it also caught and fixed a slow feed-worker shutdown. The full workspace gate passed.
  - M4 user-testing round 1 (VAL-TRD-010): One deterministic test checks each published interest set on order acceptance, planned bracket legs, filled-order/nonflat-position retention, flat release and the 64-ticket/256-sink limits (including rejection without a send); unchanged ticket and mark steps emit nothing. A test-only owner projection isolates the group-only branch without an overlapping open order or position. M4 has no real `Inactive` orders; their transition is deferred to M5 by the amended contract. The full workspace gate passed.
- [x] M4.8 provider-neutral-gating - Session and freshness gating for every provider
  - Simulated-order admission now requires a tradable verified session, Live feed, current provider generation and a fresh quote (crypto 10 s; other classes 60 s; per-class policy overrides). Typed `RejectReason` names closed/unknown/unavailable/stale states. Resting orders may mark but cannot fill on an ineligible quote; owner-local `eligible_after` replaces the restored-order fence. Removed the single-provider practice-session callbacks on both sides.
  - Manual-clock tests cover inclusive quote-age thresholds, 11 s/61 s rejection, Open/Extended/AlwaysOpen versus Closed/PreOpen/Break/Unknown for market/limit/stop/stop-limit/bracket entry, unchanged rejected revisions/rows, four provider identities, non-Live feeds, superseded generation, closed marks and post-restart first eligible quote. Trading goldens remain unchanged.
  - Fresh isolated GUI: BTC-USDC practice Buy produced a 1 BTC position and Join Bid was accepted and filled another BTC while quotes were live; no reject appeared. In a two-pane BTC/ETH workspace, closing the BTC pane kept its 2 BTC position visible and P/L changed USD 37 to USD 19 while ETH remained the only chart. The same root restarted on ETH; SQLite retained the 2 BTC position. **Not yet proven:** the contract's limit 5% below bid as a *working* order (the Join Bid filled immediately and the current compact ticket has no arbitrary-price control); neither a pane-free stop nor bracket fill was placed. These GUI clauses remain pending despite the checked implementation item.
- [x] M4.9 fill-accounting-consolidation - One fill-accounting implementation (V21)
  - V21 migrates V20 route-bearing instruments, backfills one accounting row per historical fill and cycle baselines, retains the legacy columns/tables unread for deferred removal, and adds open-order, fill and event indexes. V19→V21 fixtures prove migration backup and row preservation.
  - A single `ledger/position.rs::apply_fill` computes the position, per-fill realized P&L, cycle state and risk updates. Reversal-through-zero and scale-out tests compare risk, journal, round trips and account view before and after restart; existing golden transcripts remain unchanged. Full workspace fmt, strict Clippy, tests and naming check passed. GUI journal parity remains for the final milestone pass.
- [x] M4.10 facts-only-persistence - Commit-then-apply steps; no durable writes per quote
  - M4.10 part 1/4: Revision persistence now joins each existing revisioned SQLite write transaction rather than making a second commit after its order/account/group/risk write. The owner reads the committed revision into memory; write-only projections and store-open housekeeping keep their previous revision behavior. Memory application still precedes some later effects, and multi-effect commands/quotes still use multiple old write paths pending parts 2 and 3.
  - The characterized `crash_during_commit(2)` replay proves cancel and amend no longer expose a second revision commit; a first-commit failure rolls back the order and revision, survives restart and replays. Existing simulated goldens remain semantically identical; replay comparison normalizes Windows checkout CRLF. The ignored open-position release quote benchmark is restored unchanged in workload (1,000 alternating mark quotes); baseline at `20be05fd`: 1,244.4 quotes/s, command `$env:CARGO_BUILD_JOBS='2'; cargo test -p aeris_trading_runtime --release --locked benchmark_quotes_with_one_open_position -- --ignored --nocapture`. Crate checks and full fmt, strict workspace Clippy, tests and naming check passed. M4.10 remains unchecked.
  - M4.10 part 2/4: Simulated place, copier, protective/bracket placement, modify, cancel, bracket cascade and CancelAll stage their order facts for one `StepTx`, then apply memory and dispatch diagnostics only after commit. Failed-commit replay covers acceptance/cancel/amend, copied orders and multi-order bracket cancellation; the two original simulated goldens remain unchanged.
  - Broker place/amend/cancel reserve the bounded outbound slot across commit; full/disconnected queues reject before SQLite, and a failed commit drops the reservation without publishing a request or touching memory. The superseded broker compensating transactions are removed. Venue relay and reservation tests cover bounded capacity, disconnect and rollback. Quote-effect migration remains part 3, mark-only/retention/benchmark remains part 4.
  - M4.10 part 3/4: Each simulated quote computes fills, accounting, position and risk changes, bracket arming/completion, protection resizing and fill-caused cancellations on a private owner projection. A single `StepTx` commits the resulting facts and revision before replacing canonical memory. A failed commit preserves orders, fills, bracket state, revision and the observation cursor for replay; it reports an error without publishing a changed snapshot. Superseded bracket child/rejection store write methods were removed.
  - Replay injects a failed commit into a two-fill quote that arms a bracket and into a stop fill that cancels its target; restart retains the last committed snapshot, then one replay succeeds and recovers. Both original simulated goldens, crate strict Clippy/tests and full fmt, workspace strict Clippy/tests/naming gate passed. Part 4 still owns mark-only zero-write optimization, bounded hourly retention and the after-benchmark.
  - M4.10 part 4/4: Validated noncrossing quotes now update unrealized P&L only in owner memory, bypass the full state clone and SQLite entirely, and do not advance the durable revision. A crossing quote still commits fill/accounting/order/position/revision once; private planning remains for brackets and active risk. The 1,000-mark replay asserts zero SQLite row writes, unchanged second-connection `data_version`, unchanged fills/revision and changed P&L; failed-commit replay still passes. A process restart keeps durable facts and refreshes ephemeral marks on the next quote.
  - Retention runs from the hourly owner timer, not store open, commands or quotes; deletion is capped at 5,000 rows per table per run, including combined per-kind user-record pruning. A 12,000-excess-row test requires three runs (5,000/5,000/2,000). Intentional golden differences: `simulated_stop_golden.txt` changes stop-touch revision 8→9 because that quote now bumps for a durable fill; `simulated_golden.txt` changes only flatten/bracket/reverse/kill revisions (10→8, 13→11, 17→13, 23→17, 26→20), removing retention/mark-driven revisions. No order, event, fill or P&L value changed. Full fmt, workspace strict Clippy, workspace tests and naming check passed.
  - At baseline `20be05fd`: **1,244.4 quotes/s** (1,000 quotes in 803.5697 ms); after M4.10 part 4/4 at committed `658a73bd`: **1,203,224.6 quotes/s** (1,000 quotes in 831.1 µs); fresh HEAD `58fbb06b`: **1,176,609.0 quotes/s** (1,000 quotes in 849.9 µs). Same Windows maintainer machine, open position, release profile; exact command `$env:CARGO_BUILD_JOBS='2'; cargo test -p aeris_trading_runtime --release --locked benchmark_quotes_with_one_open_position -- --ignored --nocapture`. This measured throughput comparison is not a latency guarantee.
  - VAL-TRD-029 follow-up: `replay_crypto_stop_restart_accounting_golden` and its frozen transcript cover a crypto market buy at ask, working sell stop with no chart demand, restart and replay of the old quote without a fill, a fresh bid stop fill, exact fill-accounting/risk/account agreement, and a second facts-and-revision recovery. The two original golden files remain unchanged.
  - GUI follow-up: The bottom panel formerly skipped every quote-only snapshot once its durable revision matched, freezing open-position and account unrealized P&L even though the owner held current marks. The panel now refreshes account summaries and open positions on changed ephemeral P&L without copying committed trade history; an unchanged mark still avoids a redraw. A controller regression covers same-revision marks.
  - Combined isolated GUI evidence (details and screenshots in mission `library/user-testing.md`): VAL-TRD-014 passed, with pane-free BTC unrealized P&L changing USD 61→45→29→44 at t0/+30/+60/+120 s; ETH was the only chart symbol. A hard kill recovered the ETH workspace and BTC 1 @ 82,195 position, but VAL-CROSS-009 is **not complete** without its far working limit, protective stop, sub-5-second relaunch and independent candle-range check. VAL-TRD-021's standalone limit 5% below bid cannot be placed: the compact ticket lacks a price input and the pinned chart intent enum has no standalone PlaceOrder; legend market buy worked. The drawing toolbar exposes `Create order` for a bracket, but this attempt produced no confirmation or accepted bracket; VAL-TRD-015 remains unverified. VAL-TRD-024 remains unverified because GUI quantity selection raced and the exact reversal was not obtained.
  - Order-entry follow-up: the compact ticket now has an exact decimal price field and Buy/Sell Limit and Buy/Sell Stop actions. Empty, malformed, over-precise and overflowing text never dispatches; runtime admission remains authoritative. This closes the VAL-TRD-021 / VAL-CROSS-009 **GUI order-entry gap** identified above, but those assertions still need their isolated real-desktop checks.
  - Chart bracket follow-up: `chart_bracket_from_intent` previously required `product.price_increment`, which is absent for the verified significant-figure grid and rejected every Hyperliquid chart bracket as InvalidBracket. It now sends exact chart prices to the trading owner; that owner derives relative offsets from its registered verified `PriceGrid` at the entry, with no fabricated desktop tick. Regression tests cover significant-figure distances and real owner feed admission; VAL-TRD-015 still needs GUI evidence.
  - Isolated desktop follow-up on a fresh guarded root: VAL-TRD-021 legend market Buy 1 filled and ticket Buy Limit 1 @ 78,000 (>5% below bid) remained working. The M4 bottom panel lacks the contract's working-order row (M6 owns that view), so ticket status plus SQLite, not a bottom-panel row, prove the far limit. VAL-TRD-024 confirmed Buy 1 / Sell 2 / Buy 1, flat with account/journal realized USD 13.00 matching the three `fill_accounting` entries (+14.00 and -1.00) after graceful close. VAL-CROSS-009 recovered a separate Buy 1 @ 82,287, Buy Limits @ 78,000 and 73,000, and Sell Stop @ 73,000 unchanged after a forced kill and guarded relaunch in 388 ms; BTC workspace state, SQLite, file listing and Hyperliquid 1m lows (82,278 minimum) showed no corruption or phantom fill. Far 73,000 chart lines were outside the screenshot's price range, so exact prices are evidenced by SQLite, not visible lines. VAL-TRD-015: on a flat second account, a chart bracket with stop at the 82,250 bid and target one USD 1 grid step below the 82,260 ask was placed with entry 82,251; BTC pane removed 763 ms after submission; parent and exactly one stop leg filled after pane removal at 01:33:04 UTC, target canceled and position returned flat in 14 seconds. Earlier attempts had moved too far from touch or filled before pane removal, and are not used for this passing result. Exact screenshots/timestamps/recipe: mission `library/user-testing.md`.

### M5 trading engine

Coverage follow-up (2026-10-23): The user-testing rerun found assertion gaps despite the earlier M5 closeout. Owner regressions now distinguish maker/taker via persisted tiered commissions on placement, stop and stop-limit fills; prevent same-quote newly armed bracket fills; check typed active-stop and expiry cascades; combine a maker-entry bracket with fees and two restarts; retain marked migration P&L; roll checkpoints and deletes back together on an injected SQLite delete failure; and verify ten unchanged equity ticks. Identical Terms replay no longer advances the durable revision. Route-less feed-state publication, open-order-referenced retention and the live probe follow-up are not yet proven; the old closeout is not replacement evidence.

Status: M5.1-M5.12 code and tests are complete; the in-scope M5 milestone GUI/probe pass was closed (2026-10-18). The 2026-10-23 user-testing rerun found a later desktop regression in VAL-TRD-064: the account shortcut targeted the active chart instrument. The M5.9 follow-up now sends the selected account with no instrument from the desktop command handle; the explicitly instrument-scoped Flat ticket action remains unchanged. A new owner regression closes two positions when desktop ticket interest covers only one instrument, then verifies flat state after restart. The isolated BTC/ETH GUI rerun at this fix showed both positions flat within 2.3 s, two closing trade-history rows, and zero positions after relaunch (mission library `evidence/m5-flatten-account-fix/`). Earlier release build e48c8fa3 covered paper probes (VAL-TRD-069..073), V19 upgrade and relaunch (VAL-TRD-050/051, VAL-CROSS-010), off-grid ticket refusal (VAL-TRD-036), GUI ledger identity (VAL-TRD-058) and two-instrument flatten-all with relaunch (VAL-TRD-064); that historical result does not replace the new rerun. The corrected VAL-TRD-068 fixture now has 10,001 terminal orders, one open BTC-USDC position and one working BTC-USDC limit at live scale; an isolated release desktop showed balance and the position 0.86 s after the workspace window appeared and 300 trade-history rows. The literal bottom-panel working-order row of VAL-TRD-068 stays deferred to M6's orders view (TR-10), and VAL-TRD-058's minimum-quantity GUI clause stays deferred to M6's fractional ticket input; neither is claimed passed. VAL-TRD-074 was a separate approval-gated broker demo probe, not run during this closeout; it was retired with that broker. Redacted earlier evidence: mission library `evidence/m5-milestone/trd068-btc-*`.

- [x] M5.1 order-lifecycle-typed-events - Complete order state machine, revisions and typed reasons (V22)
  - Part 1: Client order id retries use one instruction comparison for simulated, broker, and standalone protective routes. A matching retry returns the existing order without a fact or revision; changed price, side, size or TIF returns `ClientIdReused`, including after restart. No order lifecycle or schema changes yet; V22 and transition ownership are next.
  - Regression `client_id_resubmission_is_idempotent_only_for_matching_instructions_after_restart` failed before the fix and passes after; all trading runtime and domain tests pass. Existing goldens and scripted broker tests are unchanged.
  - Part 2: V22 adds lifecycle columns to orders and typed-reason storage columns to events. V21 facts backfill revision 1, accepted/updated/eligibility from submitted, terminal time from a terminal event where known, and codes for four recognized legacy details; unmatched detail remains untouched with no invented reason. A V21 fixture verifies counts, values, unchanged detail and a pre-migration V21 backup. Runtime model, new-event writes, transitions, warnings and v2 goldens remain for subsequent parts; the item and VAL-TRD-030..032 remain incomplete.
  - Part 3: Canonical `Order.terminal_unix_nanos` is persisted for simulated cancellation through `book::lifecycle::cancel` in both command and quote transactions. A ManualClock regression checks returned and restarted orders at the clamped command time after backwards wall movement; a pure transition test rejects cancellation from Filled. Broker terminal timestamps, other canonical lifecycle fields, transition ownership for every status, typed events/warnings and v2 goldens remain; M5.1 stays unchecked.
  - Part 4: `book::lifecycle::accept` initializes canonical revision 1 and owner-clock acceptance time for simulated and broker orders, including flatten and broker-mirrored orders; SQLite persists them and restores both routes across restart. Simulated cancellation advances revision to 2 in the same atomic command/quote step as its terminal time, including backwards wall movement. Early V22 orders with null accepted time restore their submitted time (no data loss). Other status transitions still need revision/timestamp ownership; VAL-TRD-030..032 remain incomplete.
  - Part 5: Canonical `Order.updated_unix_nanos` and revision now advance through pure lifecycle functions for simulated modify/cancel and broker pending modify/cancel requests, stamped at clamped owner command time and persisted in both command/quote and broker transactions. An early V22 null update time restores from accepted/submitted. Scripted broker pending modify survives restart; queue saturation leaves snapshot facts unchanged; command transition table rejects non-executable statuses. Venue reports, fills and reconcile still need transition ownership and revisioned timestamps; expected-revision commands, typed reasons/warnings, inactive/expired/triggered semantics and v2 goldens remain. VAL-TRD-030..032 are not complete.
  - Part 6: Simulated fills, including flatten fills, now advance per-order revision, owner-clock updated time and terminal time through `book::lifecycle::fill_simulated`. The quote writer and standalone fill transaction persist those fields atomically with fill/event facts; memory no longer fabricates Filled independently. Restart regressions assert exact order equality and a pure transition test rejects a repeat fill. Broker fills/reports/reconcile, trigger/expiry timestamps, expected-revision commands, typed events/warnings and v2 goldens remain; VAL-TRD-030..032 stay incomplete.
  - Part 7: `book::lifecycle` now owns broker report, refusal, missing-reconcile and deduplicated-deal status/revision transitions. Changed facts advance per-order revision and clamped owner updated time; terminal transitions stamp terminal time, while an unchanged report stays a no-op and a late deal preserving a terminal status keeps its original terminal time. Both broker order and broker-deal SQLite transactions persist revision/updated/terminal with their event/fill facts. Scripted ack/modify/cancel/refusal/rejection/expiry/deal/mirror/reconcile regressions and restart equality cover both writers. Trigger/expiry status and timestamps, expected-revision commands, complete edge table, typed events/warnings, and v2 goldens remain; VAL-TRD-030..032 stay incomplete.
  - Part 8: A real SQLite commit-hook rollback regression now faults broker acknowledgement and broker deal commits independently; each rejected transaction preserves the complete owner snapshot and restored snapshot, then reattachment/replay commits once. It verifies the final Filled revision and terminal timestamp and deduplicates a repeated deal without new facts. No production behavior or goldens changed. Inactive/Expired and triggered/expiry timestamps, expected-revision commands, typed reasons/warnings, full transition table and v2 goldens remain; VAL-TRD-030..032 stay incomplete.
  - Part 9: Provider-neutral `Inactive` (open, not executable) and `Expired` (terminal) statuses and `Triggered`/`Expired`/`Activated`/`Resized` event wire values have exact SQLite parsing. The broker Expired report now commits an Expired status/event with terminal time and restores without reopening on late reports; pure acceptance admits an Inactive held child but never fills it. No simulated expiry scheduler or real Inactive child producer exists yet (M5.4/M5.5). The pinned chart engine lacks an Inactive order status; the desktop deliberately does not mislabel a held leg as actionable and must gain engine support before real Inactive legs ship. Triggered/activation/resize event producers, expected revisions, typed reasons/warnings, full transition table and v2 goldens remain; VAL-TRD-030..032 stay incomplete.
  - Part 10: Revisioned modify/cancel commands now check optional `expected_revision` inside the serialized trading owner before either simulated or broker routing. Mismatch returns typed `OrderCommandError::StaleRevision` with the exact current order, without a row, event, or broker request; matching revisions use the same existing transition and persistence path. Pure, service and scripted broker tests check stale/matching commands, snapshot and restart equality, and outbound queue emptiness. Legacy `modify_order`/`cancel_order` and chart-intent callers have not migrated to the new checked methods; chart drags must supply their revisions in the later desktop integration. Move-leg revision fencing, typed unified command error categories, owner-clock trigger/eligibility/expiry times, full transition table, typed reasons/warnings and v2 goldens remain. M5.1 and VAL-TRD-030..032 remain unchecked.
  - Part 11: A table over every current order status and broker report/reconcile/deal edge revealed that `is_open` incorrectly admitted a held `Inactive` child to broker report, missing-order reconciliation and deal transitions. All three now reject Inactive; late terminal deal accounting remains allowed. A full status-by-request-target matrix also asserts acceptance, simulated fills and pending requests, preventing implicit status edges. These tests characterize current transitions only: group activation/cascade/resize, trigger and simulated DAY expiry still need real producers and their allowed/disallowed edges. Typed reasons/warnings and v2 goldens remain; VAL-TRD-030..032 stay incomplete.
  - Part 12: `OrderEvent.reason` persists the V22 codes `touch_fill`, `user_cancel`, `risk_warning`, and `local_managed`, including managed protective acceptance, and restores recognized historical codes without interpreting display detail. Risk evaluation returns typed `RiskWarning`; new acceptance detail omits the old `RISK WARNING` prefix, while the desktop chart annotation selects by typed reason. Migration, warning-restart, and replay tests cover these facts. Three v1 goldens become v2: each ledger event line gains a reason column before provenance (`-` where absent); all other lines and accounting remain byte-for-byte unchanged. The crypto acceptance with no attached stop now shows `risk_warning` beside the same unprefixed detail; touch fills, local cancels, and managed protective acceptances show their reason codes. Historical V21 warning detail retains its old prefix for faithful migration, never as runtime control flow. The item remains unchecked: expected revisions on legacy/chart and move-leg commands, owner-clock trigger/eligibility/expiry transitions and full group/trigger/expiry edges still require work; actual simulated DAY scheduling is M5.4 and actual Inactive bracket legs are M5.5. VAL-TRD-030..032 are not complete.
  - Part 13: Chart modify/cancel and ladder drag now carry the displayed order revision into the serialized trading owner; the chart modify route fences before reading the owner's verified quote. A stale drag returns the current order without a write; a current drag stamps the owner quote and survives restart. The new test failed before the method existed, then passed; runtime (167 passing), desktop (452 passing), full fmt/workspace Clippy/workspace tests/naming gate passed. The milestone GUI pass remains outstanding. Legacy public unguarded command forms, planned-leg moves, owner-clock trigger/eligibility/expiry and complete transition edges remain; M5.1 and VAL-TRD-030..032 stay unchecked.
  - Part 14: Each order now owns its durable `eligible_after_unix_nanos` from acceptance and simulated modification; cancellation preserves the last eligibility time. All insert, modify and quote writers persist it, and the V22 loader restores the migration value (falling back to submitted time for early nullable rows). The former per-order `TradingCore` eligibility map is removed; one process-start floor still rejects pre-restart quotes without altering the persisted order. ManualClock and lifecycle tests check owner time, restart equality and non-reset on cancellation. The test first failed because the canonical field did not exist; runtime 168 passing/5 ignored, full fmt/strict workspace Clippy/workspace tests/naming gate green after updating two desktop fixture constructors. Trigger/expiry transitions and the complete transition table remain; M5.1 and VAL-TRD-030..032 stay unchecked.
  - Part 15: A late full broker deal no longer changes an already terminal Expired, Cancelled or Rejected order to Filled. Accounting and the order revision still advance, but the original terminal time survives; a table checks partial and full deals against every status, including held Inactive rejection. The Expired regression failed first (became Filled), then passed; runtime 170 passed/5 ignored and the full fmt, strict workspace Clippy, workspace tests and naming gate passed. M5.1 remains unchecked: owner-clock trigger/expiry producers, inactive activation/resize/cascade edges and complete simulated transition table remain for M5.3-M5.5 integration; no golden changed.
  - Part 16: Canonical `Order.triggered_unix_nanos` and `expires_at_unix_nanos` now load and persist through all V22 insert, simulated command/quote/fill update and broker report/deal update paths. Early V22 nullable values restore as `None`, and an insert round trip verifies both non-null values. No timestamp is fabricated for old data. The test first failed to compile without the fields; runtime 170 passed/5 ignored, domain 14 passed, desktop checked, full fmt/strict workspace Clippy/workspace tests/naming gate passed. Trigger production and expiry deadline scheduling remain with M5.3/M5.4; M5.1 remains unchecked, no goldens changed.
  - Part 17 / M5.1 scope ruling: M5.1 owns pure transitions in `book/lifecycle.rs` and their exhaustive state-by-edge table, not runtime producers. Acceptance to Inactive/Working, activation, five typed modification causes, shrink/grow resize, Triggered event with owner trigger time and unchanged status, five typed cancellation causes, due DAY expiry with distinct deadline and terminal time, fill and pending rejection each advance revision/owner update time; disallowed terminal-to-open and Inactive-to-fill edges reject. The existing simulated user-cancel path remains executable-only; M5.5's held-leg cascade uses the typed cause. Precise `cfg_attr(not(test), expect(dead_code, reason = ...))` annotations mark functions awaiting their named producers, never a second owner. M5.3 owns trigger and NotMarketable production, M5.4 DAY scheduling, M5.5 activation/resize/cascade and move-leg revisions (planned-leg moves disappear in M5.6), M5.9 kill-switch/flatten causes. No speculative runtime producer is wired here.
  - VAL-TRD-030: `simulated_lifecycle_transition_table`, `simulated_lifecycle_causes_and_deadlines`, existing broker/status matrices and three v2 goldens; `RISK WARNING` appears only in historical V21 migration SQL and test fixtures, not runtime production warnings. VAL-TRD-031: `stale_revision_after_fill_returns_the_terminal_order_without_a_new_fact`, existing `stale_modify_and_cancel_return_current_order_without_committing_facts`, `client_id_resubmission_is_idempotent_only_for_matching_instructions_after_restart`. VAL-TRD-032: `v21_lifecycle_fixture_migrates_to_v22_without_losing_events_or_orders` checks revision/accepted/updated backfill, typed historical reasons, row counts, and backup. No golden transcript changed in part 17. V21/V22 migration numbering follows the mission contract override, not older work-item prose.
- [x] M5.2 admission-validator - One admission validator for simulated and broker routes
  - Part 1: shared `admission::validate_price_grid` checks exact fixed ticks and significant figures (integers permitted, decimal precision bounded), with `Unknown` accepted and no implicit rounding. `OffGrid` is typed and names the specific grid reason; simulated and broker standalone order placement call the same validator for limit/stop prices before writing or dispatching. The former off-grid manual-stop test fixture moved from 0.40 to the on-grid 0.50 without changing its fill or bracket-rejection intent. `fixed_and_significant_figure_grids_reject_off_grid_prices` failed first on missing validator/reason and now passes; the full runtime suite and scripted broker tests remain green. M5.2 is not complete: unify the remaining route checks in `admit`, VenueCapabilities/TIF, quantity, trigger-side and RestingAt rules, capacity, group legs and GUI qualification in subsequent parts; `Unknown` warning remains to wire. No goldens changed.
  - Part 2: `admission::validate_trigger_side` rejects buy stop/stop-limit triggers at or below the ask and sell triggers at or above the bid with typed `WrongSideTrigger`, and accepts strict opposite-side triggers. Both simulated and broker placement use the owner's available Live QuoteBook observation; no second quote owner is introduced. The pure touch-side test first failed to compile for the missing validator and now passes. If no owner Live quote exists, this check has no price reference and does not fabricate one; simulated real-feed gating separately rejects missing quotes. Broker parity with attached feed and legacy bracket-leg side checks remain for the complete M5.2 validator.
  - Part 3: `admission::validate_time_in_force` rejects IOC and FOK on Stop/StopLimit with typed `TifNotSupported` on both simulated and broker paths before writes/venue requests. DAY remains valid on either route; the broker route's existing DAY-to-GTC mapping is unchanged. The pure TIF regression first failed on missing function and reason, then passed, as did the existing scripted broker and replay suites. M5.2 remains open for the unified `admit` call, capabilities, capacity, RestingAt, bracket leg checks and GUI verification.
  - Part 4: both placement routes now call one side-effect-free common admission check for client-id length, scales, positive quantity/provider increment, TIF, grid and open-order capacity (including staged copy orders). `ClientIdTooLong`, `InvalidQuantity`, and `CapacityExceeded` are typed; the original user-facing quantity/client-id messages remain compatible. The old duplicate quantity check and broker validation branches are removed. A shared-route test failed first on missing reasons/check and now passes; existing scripted broker tests and runtime suite pass. Quote-interest capacity, risk locks, venue capabilities, RestingAt, the complete `admit()` authority and GUI pass still belong to later M5.2 parts. No schema or goldens changed.
  - Part 5: `TradingCore::admit` now gathers the existing shared checks, owner stop-side check and bounded quote-interest check before persistence on simulated and broker orders, including staged copy and legacy managed child orders. The 257th distinct routed interest returns typed `CapacityExceeded` rather than a market-feed string, without publishing demand or inserting the order. The test first failed with the former string; runtime suite (177 passed, 5 ignored) and full fmt/strict workspace Clippy/workspace tests/naming gate passed. The session/risk checks still live outside this boundary; VenueCapabilities, typed lock, RestingAt, full route parity/group-leg checks and GUI remain to finish M5.2. No schema or golden changes.
  - Part 6: Route-specific `VenueCapabilities` owns the broker route's client-id length, its DAY-to-GTC and market-to-IOC effective TIF, native SL/TP support and unsupported managed trailing; simulated route retains native DAY and managed trailing. Broker placement and modification use one effective-TIF rule instead of a broker-local conversion, and bracket routing consults the capabilities. The test first failed because the type was absent; runtime suite (178 passed, 5 ignored) and full fmt/strict workspace Clippy/workspace tests/naming gate passed. Typed account lock, RestingAt, full route parity and GUI remain in M5.2; no schema/golden changes.
  - Part 7: `admit` refuses already locked risk-increasing simulated and broker orders with `RejectReason::AccountLocked` before persistence; `admission_detail` preserves the stored lock's user-facing reason, and direct risk evaluation uses the same typed display. Reduce-only standalone managed protection bypasses the lock but still validates that it reduces the position. The service regression first failed on the missing typed code, then passed; runtime/domain suites and full fmt/strict workspace Clippy/workspace tests/naming gate passed. Dynamically triggered risk-rule details remain unchanged; RestingAt, bracket-leg relations, route parity and GUI remain. No schema or golden changes.
  - Part 8: `RestingAt` is an admission-only price-relative instruction. The trading owner resolves buy/sell Limit or Stop from its Live bid/ask before placing either simulated or broker orders, and persisted rows contain only executable kinds. Chart bracket placement sends `RestingAt` rather than classifying against a desktop quote; the old bracket classifier was removed. Missing owner quotes reject without writes; a ManualClock feed test checks all four side/price combinations and exact restart restoration. Pure test failed first on the missing variant/resolver, then 179 runtime tests and full fmt/strict workspace Clippy/workspace tests/naming passed. The parity suite, group-leg checks, buying-power hook, Unknown grid warning and GUI pass remain; M5.2 stays unchecked. No schema or golden changed.
  - Part 9: `admission::BuyingPower` is an explicit no-op step reached by both routes inside `TradingCore::admit`. Margin and buying-power enforcement remain out of scope; no redundant policy trait or alternate state owner was introduced. A failing test first required the hook, then scoped runtime tests (180 passed, 5 ignored) and the full fmt/strict workspace Clippy/workspace tests/naming gate passed. The validator still needs typed session/currency/risk integration, group-leg checks, the Unknown-grid warning, route parity and GUI confirmation; M5.2 remains unchecked.
  - Part 10: unknown instrument grids remain admissible without fabricated rounding and now attach `RiskWarning::UnverifiedPriceGrid` to both simulated and broker acceptance events, persisted with the order. A test that initially failed on the absent warning exercised both routes and exact event restoration after restart; the scoped runtime tests and full fmt/strict workspace Clippy/workspace tests/naming gate passed. Existing scripted broker tests were not edited. M5.2 still requires group-leg price/side checks, typed common risk/session/currency admission, the parity suite and isolated GUI verification; no schema or golden changed.
  - Part 11: simulated verified-session/fresh-quote checks and account-currency matching now run inside `TradingCore::admit`, using typed `CurrencyMismatch` and `AccountNotRegistered` while retaining the old user-facing currency text. Broker-route admission remains exempt from practice currency and quote gates; risk-increasing orders still use the existing post-admission mutable risk evaluator. A pure currency test failed first, then a service refusal proved no SQLite/event change. The first scoped runtime suite exposed quote-interest capacity being masked by an unknown session; capacity now rejects first, restoring the existing bounded-interest test. Scoped runtime 184 passed/5 ignored, full fmt/strict workspace Clippy/workspace tests/naming passed. Group-leg prevalidation, mutable risk integration, route parity and GUI remain; M5.2 stays unchecked.
  - Part 12: priced bracket templates now preflight the stop and each target against the instrument's exact price grid and their relative entry sides before either simulated entry commits or a broker request is reserved. An entry tick offset alone no longer implies a valid resulting price. A failing-test-first pure grid case and a simulated SQLite restart/no-write regression cover the refusal. Market entries have no known fill price at admission and are still checked when their children are created; group-leg GTC, mutable risk consolidation, broker parity and GUI qualification remain. M5.2 stays unchecked.
  - Part 13: one service-boundary parity suite submits the same stop IOC/FOK, off-grid limit and wrong-increment quantity to both simulated and attached demo-broker routes. Both return the same typed reason and keep snapshots and the broker request queue unchanged. The same suite checks the broker route's 50-character client-id limit and durable locks on both routes. It initially failed on a test fixture violating the increment before the TIF check; the corrected fixture passes. Live-quote wrong-side parity, group GTC and risk unification, quote-interest capacity parity, and isolated GUI qualification remain, so M5.2 stays unchecked.
  - Part 14: `admission::admit()` now owns the shared preflight and the existing dynamic risk evaluator for both simulated and broker submissions; protective children use the same boundary with the reduce-only exception. It returns warnings to the respective acceptance event instead of a duplicate post-admission risk call in each route. Existing lock/session-plan/discipline behavior, the scripted broker suite and all three trading replay goldens remain green. The parity suite verifies unchanged no-write refusals. Group-leg GTC and quote/interest parity, full contract and GUI qualification remain; M5.2 stays unchecked.
  - Part 15: the service-boundary parity suite now drives a Live owner quote through the real bounded market inbox and confirms that both routes reject buy stops at the ask and sell stops at the bid, for Stop and StopLimit alike, with the same `WrongSideTrigger`. Each of the eight refusals leaves order facts and the broker request queue unchanged. This is characterization of the unified validator; the test passed on first run. Group-leg GTC, 257th-interest broker parity and the isolated GUI pass remain; M5.2 stays unchecked.
  - Part 16 / closure: activated simulated bracket stop and target orders use GTC despite a DAY stop entry; broker DAY is normalized to GTC in the canonical pending order and venue request. The real-feed 257th *routed* practice order returns typed `CapacityExceeded` without changing the snapshot or quote-lease publication. Broker orders do not consume practice-market quote leases, so a broker-side 257th lease case would assert the wrong ownership boundary. The existing `admission::admit` preflights each currently created managed child; real Inactive group members and OCO remain M5.5's authority and must call the same validator. The broker DAY test first failed on an incorrect expectation that the canonical pending order retained DAY; the assertion now checks its actual GTC normalization. Scoped runtime tests: 189 passed, 5 ignored. GUI pending milestone pass: BTC-USDC off-grid ticket refusal and visible specific reason (VAL-TRD-036). No scripted broker tests changed.
- [x] M5.3 execution-fill-rules - Bid/ask fill rules, stop and stop-limit triggering, multipliers
  - Part 1: a resting buy or sell Limit fills at its stated limit when a later quote crosses at a better touch, rather than taking that later touch. A deterministic two-sided regression failed first at buy 98.00 versus limit 99.00, then passed with both buy and sell fills at their respective limits; runtime 190 passed/5 ignored. This changes only the resting-candidate evaluator; placement-time marketable fills, IOC/FOK, market timeout, stop trigger persistence, ordering and liquidity classification remain for subsequent M5.3 parts. No golden transcript changed.
  - Part 2: an exact-value simulated round-trip characterizes `point_value` 50 and 1 on two contracts: two bought at 5000.00, marked bid 5000.50, then sold bid 5001.25 yield unrealized 50.00/1.00 and realized 125.00/2.50 respectively. This passed on the existing one-owner accounting path without a behavior change, completing VAL-TRD-041's arithmetic evidence. Remaining fill/trigger/IOC mechanics stay in M5.3.
  - Part 3: nonmarketable buy and sell Limit IOC/FOK instructions on an owner Live quote now commit Accepted then `Cancelled{NotMarketable}` in the same SQLite order step; the returned order is terminal, with no fill or residual working order, including after restart. A single placement-touch classifier returns bid/ask for future taker execution, distinct from the resting-limit maker price. The test first failed with Working, then passed for all four cases; runtime 192 passed/5 ignored. Marketable placement fills, no-quote timeout, stop triggering, ordering and liquidity flags remain in subsequent M5.3 parts. No golden changed.
  - Part 4: market and marketable Limit orders on an owner Live quote now take the bid/ask immediately, including IOC/FOK. Acceptance, fill, accounting, position and copied orders are projected privately and committed in one SQLite transaction; the shared quote fill rules drive the economic effects. A ManualClock test first failed with Working, then covered both sides, GTC/IOC/FOK, exact touch prices, event ordering and restart equality. The chartless desktop integration expected the old next-quote entry price; its two mark assertions now reflect the placement ask. Existing interest test expects an immediately flattened position to release its quote lease without an extra quote. Scoped runtime 193 passed/5 ignored, chartless harness passed, full fmt/strict workspace Clippy/workspace tests/naming gate passed. No golden changed. Market-no-quote timeout, stops, deterministic group ordering and liquidity flags remain in M5.3.
  - Part 5: an injected SQLite commit-hook failure during a marketable placement leaves acceptance, events, fill, position, sequence and revision absent in owner memory; retry succeeds and the filled order, fill and position survive restart. The test uses a Live owner quote, existing single-step writer and ManualClock, with no production-path changes; it passed first as characterization of part 4's atomicity. Scoped runtime 194 passed/5 ignored and strict Clippy passed. The next part handles the five-second no-quote market wait.
  - Part 6: a Market order on a verified open session and current Live feed with no quote now waits; a fresh quote before five seconds fills at the touch, otherwise the owner timer commits typed `Cancelled{NoQuote}` at acceptance + 5 s. Stale quotes, unknown/closed sessions and non-Live feeds still refuse admission. The deadline derives from durable acceptance time, survives restart, and is processed before a queued snapshot/quote; a failed timeout commit retries at a bounded cadence. A ManualClock service test failed first on the absent reason and then on the missing timer; timeout, pre-deadline fill and restart recovery pass. Runtime 196 passed/5 ignored; fmt, strict workspace Clippy, workspace tests and naming passed. No golden changed. Stop/stop-limit trigger, deterministic candidate/group ordering and liquidity hooks remain in M5.3.
  - Part 7: on an eligible crossing quote, a buy Stop tests the ask and a sell Stop tests the bid, emits `Triggered` before `Filled` at the quote receive time, then fills at that exact or gapped touch. The canonical trigger timestamp and per-order revision survive restart. Four-sided exact/gap service cases failed first on missing trigger time, then passed; runtime 197 passed/5 ignored. Two v2 goldens changed only in the stop step: each adds one `Triggered` event before `Filled`, and subsequent event/fill identifiers advance by one; stop price, PnL, positions and global revision remain unchanged. Full fmt/strict workspace Clippy/workspace tests/naming passed after cleaning the Cargo target. Stop-limit gap-through, deterministic candidate/group ordering and liquidity hooks remain in M5.3.
  - Part 8: an untriggered StopLimit crossing its stop but gapping beyond its limit now emits one durable `Triggered` event and remains Working; the later eligible quote fills at its limit, even after restart, without a second trigger. A stop/limit crossing on one quote fills at the touch with the trigger before the fill. Buy/sell gap and same-quote tests use the owner feed and ManualClock; the gap test failed first with no trigger, then passed. Runtime 199 passed/5 ignored; full fmt/strict workspace Clippy/workspace tests/naming passed. No golden changed. Deterministic candidate/group reaction ordering, trigger index and liquidity hooks remain in M5.3.
  - Part 9: quote planning now processes initially eligible candidates in stable order, rechecks current executable status and revision, and applies bracket reaction/protection after each fill before the next candidate. An unchanged trigger retains its same-quote taker price; changed orders recompute their touch. The bracket activation fence, stop cascade and restart characterization passed before this change; the first full runtime suite exposed a same-quote stop-limit taker-price regression, fixed before the full gate. Runtime 200 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed. Explicit OCO sibling cancellation and the 100-run transcript property await M5.5 real group authority; trigger index and liquidity hooks remain M5.3. No golden changed.
  - Part 10: the bounded quote candidate list now orders Market first, untriggered Stops/StopLimits by side-aware trigger price, then resting Limits/triggered StopLimits by side-aware limit price; accepted time, numeric local order sequence and ID break ties. A service test placing a lower-priority buy limit first failed at fill order `sim-order-1,2,3` before the sort and passed `2,3,1` afterwards; another checks stops before a crossing limit in low-to-high buy trigger order. Runtime 201 passed/5 ignored; full fmt, strict workspace Clippy, tests and naming passed. This is sorting a bounded candidate list, not yet the per-side trigger index; touch-only execution hooks and real OCO remain. No golden changed.
  - Part 11: `execution::Liquidity`/`FillPrice` have exactly one touch-only implementation each (`TouchLiquidity`, `TouchPrice`), and the shared simulated fill path uses both for full quantity and exact observed price. The hook unit test failed first on missing types, then passed; an initial scoped Clippy found test-module ordering and the first full gate found the justified-traits inventory, both corrected before a clean full fmt/strict workspace Clippy/tests/naming gate. The explicit traits have documented naming-check exceptions and an AGENTS.md ownership rule. No economics or golden changed; trigger index and real OCO group property remain.
  - Part 12: `OrderBook` owns `book/trigger_index.rs`, a bounded per-instrument and per-side price-keyed membership for executable Market, Stop, Limit and triggered StopLimit orders. Insert, replacement, removal and recovered-book construction maintain it; quote candidates and stop-limit triggering query crossing price ranges before the stable priority sort. The 4,000-step seeded mutation/recovery test compares every indexed crossing set with an independent executable full scan and first failed on the absent query. Runtime suite 204 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed. No golden or economic change. Real OCO sibling cancellation and 100-run transcript proof remain M5.5, the first owner of explicit OCO groups.
  - Closeout (2026-10-18): all twelve parts above are complete with their regressions and gates. The items parts 9-12 deferred to later owners landed there: explicit OCO sibling cancellation and the 100-run wide-quote transcript proof under M5.5's real group authority. M5.3 closes; the remaining real-path fill-state checks are tracked with the M5 milestone GUI pass.
- [x] M5.4 time-in-force-expiry - DAY expiry at session close or 00:00 UTC; GTC stays working
  - Part 1: a routed simulated DAY order now fixes `expires_at_unix_nanos` at acceptance from the owner's verified session end, or the next UTC midnight in AlwaysOpen, and persists it through restart; GTC remains null. The real-feed service regression failed first with null expiry; a pure boundary test checks one second before midnight, noon and exactly midnight. Legacy test-only unrouted replay fixtures use a synthetic UTC day so historical characterization stays runnable; production always needs the actual owner session. Runtime 206 passed/5 ignored, full fmt/strict workspace Clippy/tests/naming passed. The owner expiry timer, startup recovery and migrated null-DAY assignment remain subsequent M5.4 parts; milestone GUI Expired status remains pending.
  - Part 2: the owner waits for the nearest simulated order deadline and expires due DAY orders before a queued quote or snapshot, preserving the acceptance deadline as event and terminal time. Expiry and any managed bracket cascade commit in one order step; failed commits retry on a bounded cadence. A ManualClock service regression failed with Working before the timer, then passed at the exact boundary and after restart; GTC remains open. Runtime 207 passed/5 ignored and full fmt/strict workspace Clippy/workspace tests/naming passed. Migrated null-deadline DAY rows and proactive startup recovery remain M5.4 part 3; GUI Expired status pending milestone pass.
  - Part 3: store-open recovery expires persisted overdue DAY deadlines and commits their event/revision before startup acknowledges ready. A test first failed by reading Working directly from SQLite after `TradingCore::open` (without running the owner loop), then passed with Expired at the original session close; the service restart agrees. Runtime 208 passed/5 ignored and full fmt/strict workspace Clippy/workspace tests/naming passed. Migrated open DAY rows without a deadline still require a verified historical calendar path; this part does not guess one. GUI pending milestone pass.
  - Part 4: an open migrated DAY order with no deadline is fenced at the single quote eligibility boundary, before either stop-limit trigger or fill, including after two restarts and a crossing Live quote. The regression first filled the legacy row, then passed; runtime 209 passed/5 ignored, strict scoped Clippy and full fmt/strict workspace Clippy/workspace tests/naming gate passed. This preserves the row without inventing a calendar deadline. A verified historical session projection and durable deadline assignment are still required to close M5.4 and VAL-TRD-044; GUI pending milestone pass.
  - Part 5: startup recovery recognizes a verified persisted continuous-market route with no contract hours, reconstructs a null DAY deadline as the next UTC midnight after acceptance, persists all recoverable deadlines atomically, then expires overdue orders before the first command. It never treats the instrument class alone as proof of an AlwaysOpen calendar; unresolved calendared/route-less rows remain fenced by part 4. The real-feed migration regression first failed with null, then passed with exact terminal time; runtime 210 passed/5 ignored, strict scoped Clippy and full fmt/strict workspace Clippy/workspace tests/naming gate passed. Calendared historical acceptance-session recovery and the full three-boundary/two-restart fixture remain before M5.4 closure; GUI pending milestone pass.
  - Part 6: `aeris_instruments::calendar::weekly_hours` is the single bounded recurring-session projection for both market status and trading startup recovery. It handles IANA DST, split days and overnight segments. For verified persisted routes with contract hours, migrated null-deadline DAY orders derive the acceptance-time session end; startup commits deadlines, then expires overdue orders before ready. A real-feed ManualClock test failed with null, then passed across two restarts and multiple boundaries with the original Friday close and GTC still Working. Domain overnight and DST regressions and existing market weekly status tests pass; scoped runtime 211 passed/5 ignored. An unverified/route-less or acceptance-closed legacy row stays fenced, never receives a guessed date. A failed-backfill transaction and multi-DAY fixture are still to be tested; GUI pending milestone pass.
  - Part 7: when verified Terms arrive for a previously route-less instrument, the trading owner now backfills fenced legacy DAY orders from their acceptance-time calendar and expires overdue ones before processing another quote. An isolated Live-feed test cleared both the persisted route and DAY deadline, observed Working/None after startup, failed first when Terms left the order Working, then passed with Expired and the original UTC midnight after Terms. The week-schedule test now spans Friday closes on Nov 6, 13 and 20 with GTC Working across two restarts. No route is invented at startup; absent Terms leaves the row fenced. The multi-DAY failed-transaction fixture is still to be tested; GUI pending milestone pass.
  - Part 8: a two-DAY fixture installs an SQLite trigger that aborts the second deadline UPDATE during startup. The service refuses startup, both deadlines remain null and both rows Working; removing the trigger allows the next startup to backfill and expire both at their original UTC midnight. The characterization passed before any further production change. M5.4's verified-route acceptance, timer, recovery, GTC, DST and atomicity paths are complete; GUI Expired status pending milestone pass. Safety deviation from literal VAL-TRD-044: an older migrated row with no persisted verified route cannot receive an acceptance-time calendar at first startup without guessing. It remains non-executable until verified Terms arrive, which then assigns its deadline and expires it before a later quote. Never infer an AlwaysOpen session from instrument class.
- [x] M5.5 order-groups-and-bracket-migration - One order-group model with migration of existing brackets (V22)
  - Prerequisite (2026-10-17): Aeris Charts `36d5b92` adds a provider-neutral Inactive line with muted/dashed "Inactive" chrome and no modify/cancel affordance or request by default; an explicit engine host policy can enable actions. Terminal pins that revision and projects real trading `Inactive` orders without suppressing or mislabeling them. The existing legacy `planned_bracket_legs` projection remains Working and draggable until M5.5 replaces that producer with real held group orders; changing it now would remove the current planned-leg editing behavior. No new engine capabilities beyond this held-order status/policy were introduced between the old and new pins. GUI confirmation of seeded held legs remains the M5 milestone pass after M5.5.
  - Part 1: `groups/model.rs` defines the typed Bracket/OCO/Protection group and leg roles, status, management and revision. Structural validation bounds membership to a stop plus four targets, rejects duplicate order ids and malformed bracket allocations/roles, and leaves order/exposure validation with the trading owner. Two new model tests and the existing runtime suite (215 passed, 5 ignored) pass; full fmt, strict workspace Clippy/tests and naming_check passed. No schema or live group producer changed, so legacy managed brackets remain the sole runtime authority. Next: durable V23 groups and historical migration before any group activation.
  - Part 2: V23 introduces constrained `order_groups` and `order_group_legs` tables, bounded lookup index and foreign keys to real orders without deleting legacy data. A V22 fixture checks previous order/bracket facts survive and an orphan leg is refused; every historical schema migrates to V23. A legacy unpriceable terminal bracket may have zero legs in the typed model; the new test failed first, then passed. The schema fixture failed first at version 22, then passed. Runtime 217 passed/5 ignored; full fmt, strict workspace Clippy/tests/naming passed. The legacy writer remains the only behavioral authority until M5.5's idempotent historical backfill and real-leg producer; V23 tables are empty at this boundary. GUI pending milestone pass.
  - Part 3: Startup converts legacy standalone `protective_orders` into idempotent, real-order-linked Protection group rows inside a single transaction without changing their order or legacy authority. A V19 fixture and reopen check its backup, original Working status and exactly one group leg. A conflicting prior leg membership fails and rolls back every projected row; the regression failed first on a silently orphaned group. Pending/active/terminal managed brackets and new Inactive legs remain for subsequent M5.5 parts. GUI pending milestone pass.
  - Part 4: Startup projects terminal completed/cancelled managed brackets to V23 group rows, links retained stop/targets only when the full child set exists, and leaves old order facts unchanged. A missing retained target yields a terminal group with zero legs instead of false partial protection; idempotent replay and template offsets/allocation are tested. The new test first failed to compile on the absent backfill. The legacy bracket writer remains authoritative; awaiting and active group conversion and typed recovery notices follow. GUI pending milestone pass.
  - Part 5: Startup projects complete active legacy brackets to V23 groups linked to their original stop and target OrderIds; no order price, quantity, status or legacy bracket changes. A sanitized file-backed V19 active 4-unit bracket with 50/50 targets checks exact IDs, offsets, allocation, management, order facts, V19 backup and idempotent reopen. A legacy active-to-completed change is reconciled on the next open, retaining its linked child history. The tests first failed on missing active projection and then on the terminal status conflict. Runtime 222 passed/5 ignored and full fmt, strict workspace Clippy/tests/naming passed. The legacy writer remains the behavioral authority; awaiting Inactive legs, atomic whole-history conversion, recovery notices and live group ownership remain. GUI pending milestone pass.
  - Part 6: Protection, terminal-bracket and active-bracket historical projections now share a single retryable SQLite transaction without bumping the owner revision. A V23 two-source fixture injects an abort on the last active target leg: no group or leg survives, retry after removing the trigger creates both groups and three legs, and a second retry changes nothing. The test failed first because the atomic entry point was absent. Runtime 223 passed/5 ignored and full fmt, strict workspace Clippy/tests/naming passed. The legacy writer remains authoritative; awaiting Inactive legs and the complete V19 fixture follow. GUI pending milestone pass.
  - Part 7: Canonical validation permits unpriced Inactive Stop/Limit/StopLimit legs awaiting a market entry fill, while the same unpriced orders remain invalid when Working. The domain test failed first on InvalidOrderPrices and passed after the status-scoped shape rule. No leg is created yet; V23 awaiting-entry migration, activation and live group authority follow. GUI pending milestone pass.
  - Part 8: Typed group validation requires at least two members for OCO and exactly one for standalone Protection. The group-model test failed first because both invalid shapes were accepted, then passed with the existing 224 runtime tests. This is structural preparation, not the behavioral authority switch: legacy bracket arming remains the sole writer until Inactive children, activation and pending migration switch together. GUI pending milestone pass.
  - Part 9: `OrderStep` now has a single-transaction insertion seam for typed groups after their real orders, sharing the command's commit and revision. Group validation and account/instrument membership reject foreign child orders even when SQLite foreign keys alone permit them. A last-leg injected failure rolls back both group and member rows and leaves revision unchanged; retry commits both. The cross-account test failed first when an unrelated child was accepted. Runtime 226 passed/5 ignored; full fmt, strict workspace Clippy/tests and naming_check passed. No live producer uses this seam yet, so legacy managed brackets remain the sole behavioral authority; the approved placement/activation authority switch and V19 awaiting migration still follow. GUI pending milestone pass.
- [x] M5.6 remove-protection-layers - Delete the superseded protection layers
  - The legacy protection authority is gone in one cut: `groups/protection.rs` and `groups/planned_leg.rs` are deleted, the legacy recover/align/reconcile writers and the planned-leg store fallback are removed, and every read (snapshot, stop-fill classification, risk scenario filter, quote interest) derives from `order_groups`. The desktop-facing view types survive under their view names (`BracketView`/`BracketViewStatus`, `ProtectionView`/`ProtectionRole`, `PlannedLeg`/`MovePlannedLeg`, `PlacementOutcome`) until M6.11, exactly as the work item specifies; `rg` shows zero matches for every deleted file, type and function name across crates/apps/tools. The startup sequence is repair, group verification with coverage repair, awaiting conversion, then a gated V24 finalize: converted legacy rows leave `managed_brackets` per bracket and the store drops both legacy tables only once no awaiting row remains (V24 is a finalize step, not a chain entry; a failed conversion keeps its row and retries on the next start). Admission's immediate placement fill now reconciles groups (a marketable entry activates its bracket at once), practice-account deletion purges `order_groups`, and the one process-start quote floor stays as the durable restart fence (its deletion attempt regressed the crossed pre-restart quote test and was reverted; the work item's "restored-order fence" was the legacy pending-bracket validation on entry modify, now removed). The desktop already sends `RestingAt` plus `AttachedProtection`. Three goldens changed only in the view type name inside `managed_brackets=[...]` lines; runtime 240 passed/5 ignored, full fmt, strict workspace Clippy, workspace tests and naming passed. GUI placement evidence (VAL-TRD-053) remains with the milestone GUI pass.
- [x] M5.7 cash-ledger-and-fees - Fill-driven cash ledger with configurable fees, default zero (V25)
  - Part 1/2 (V25): schema adds `ledger_entries`, per-account `fee_schedules`, exact position cost-basis columns and
    denormalized instrument class/price-grid columns; `migrate_ledger` backfills legacy accounts (deposit entry),
    fills (realized P&L) and cost bases once per store after group finalize, idempotent via a scratch marker. Exact
    `CostBasis` i128 math now threads every fill path, including the quote-step commit and repair loop.
  - Part 3/4: `Command::SetFeeSchedule` validates and persists deterministic fee schedules; maker/taker
    classification (quote-side resting Limit or triggered StopLimit = Maker; placement, stop triggers, market and
    flatten fills = Taker) drives per-contract/per-unit (min + notional cap) and notional-bps models with a single
    exact rounding. Each fill commits RealizedPnl and negative Commission ledger entries in the same transaction;
    simulated registration (or the V25 backfill) writes the funding Deposit. A simulated account's balance is the
    exact sum of its ledger entries; only a gated pre-group store awaiting conversion keeps legacy math. Replay
    traces print the ledger chain; five goldens including a new fee round-trip lock restart determinism.
  - Regressions: fee maker/taker, per-instrument override across restart, per-unit clamp/cap unit test, rejection of
    unknown accounts/instruments and invalid models, V19-legacy backfill idempotency and scale-in cost basis.
  - VAL-TRD-054/055/056/057 code-verified (workspace clippy + tests green); VAL-TRD-058 deferred to the milestone GUI pass.
  - 2026-10-23 round-2 follow-up (VAL-TRD-056/057): route-less persisted Hyperliquid BTC orders now carry an owner-derived per-order `FeedState::Unavailable { RouteUnknown }` and "feed unavailable (route unknown)" message in the bounded working-set publication; a verified BTC Terms event installs the symbol route, publishes Live readiness and permits the resting order to fill on a later quote. The new `replay_maker_bracket_fee_chain_golden_v2.txt` is an addition, not a change to the existing market-entry/no-fee bracket golden: it records a 2-contract maker fill at 9,900, stop at 9,675, target `Cancelled{Cascade}`, both restarts, and exact Deposit 5,000,000 / entry Commission -10 / RealizedPnl -22,500 / exit Commission -10 (currency scale 2).
  - 2026-10-23 route-bypass migration, OCO harness: the shared `routed_fixture` uses the deterministic owner clock and sends verified Terms, open Session, Live Feed and a current Quote through the trading service's bounded market inbox. Both OCO tests retain their existing status, error and group assertions; they pass with all three test-only route bypasses temporarily removed and with the bypasses restored. No golden or production path changed. Service fixtures, replay, venue scripts, group recovery and remaining direct-observation tests still need conversion before the three bypasses can be removed; the final route-less admission/resting/resolution regressions remain pending.
  - 2026-10-23 route-bypass migration, placement-atomicity harness: the SQLite failure/retry test now installs verified Terms and a Live owner quote through `TradingCore::apply_market_batch`, reusing the shared feed test builders instead of inserting a route directly into `state.market_routes` or bypassing the owner with `quote_book.accept`. Its original rollback, retry, fill and restart assertions pass both with and without the three bypasses. No golden changed. Service fixtures, replay, venue scripts, group recovery and other direct-observation cases remain.
  - 2026-10-23 route-bypass migration, scripted-venue harness: all 27 scripted tests pass with and without the bypasses. The common demo setup now installs verified Terms, session, Live feed and a quote through the owner inbox. The separate offline global-command test uses `routed_fixture` for its simulated resting order; it exposed the sole bypass-free failure and now passes without altering any assertions. The broker-only tests retain their request/reconciliation checks and fixture clock. No golden changed. Service fixtures, replay, group recovery and other direct-observation cases remain.
  - 2026-10-23 route-bypass migration, service foundation harness: all 10 foundation tests pass with and without the bypasses. Three simulated-order cases now register verified Terms and session through the owner inbox: the provenance/modify case uses a Live quote; the data-only live-account and broker-bracket cases intentionally use a Live feed without a quote so their original market orders remain Working with inactive bracket legs, preserving every status and count assertion. No golden changed. The larger service fixture, replay, group recovery and other direct-observation cases still need migration.
  - 2026-10-23 route-bypass migration, group recovery harness: all five recovery tests pass with and without the bypasses. Both entry fixtures now use verified Terms, open session and a nonmarketable Live owner quote; the entry is a resting 10,000 limit rather than a market order so its original direct-observation assertion still sees exactly one fill at the 10,000 touch, instead of filling at placement. The migrated-protection fixture restores the persisted route with a fresh owner session/quote after restart, then verifies the stop on a later eligible observation. Every recovery, leg, count and stable-reopen assertion is retained; no golden changed. The bypasses remain until the larger service fixture, replay and other direct-observation harnesses are migrated, then route-less regressions are added.
  - 2026-10-23 route-bypass migration, service fixture batch: the eight-submitter/1,000-order restart test and whole-unit quantity/restart test now use the shared routed fixture. Verified Terms preserve the quantity scale and increment, while the owner quote keeps their 9,000 limits resting. Both tests pass with and without all three bypasses; all original assertions and goldens remain unchanged. Other direct-observation service fixtures and replay remain before bypass removal and route-less regressions.
  - 2026-10-23 route-bypass migration, warning/revision service fixtures: the unknown-grid simulated/broker persisted-warning test and stale modify/cancel restart test now use verified Terms and a Live owner quote. Both tests pass with and without the bypasses, preserving the warning, no-extra-fact, revision and restart assertions. No golden changed. Remaining service fixtures and replay must be converted before the bypasses and route-less regressions land.
  - 2026-10-23 route-bypass migration, service fill/bracket batch (21 tests: currency mismatch x2, command-time cancel, stale-after-fill, client-id reuse, refreshed Terms, future/equity and point-value round trips, maker/taker and override fees, equity samples, irreparable legacy position, practice delete, held-leg and pending-entry cascades, managed and stop-entry brackets, pending held legs, resting-limit price, crossed limit and stop priority): new shared `awaiting_quote_fixture` (verified Terms, open session, Live, no quote), `quoted_fixture`, `reopen_awaiting_quote` and `deliver_quote[_at]` helpers deliver every fill-producing quote through the owner inbox on the deterministic clock; direct observations are no longer used by these tests. Production rules now visible in the fixtures: a market order placed while the owner holds a Live quote fills at placement, so the exit legs of the round-trip tests deliver their exit quote first and assert the placement fill; quotes must be received after `eligible_after`; an instrument with no open order or position leaves quote interest, so the command-time and two-instrument tests hold a ticket interest; a restart re-attaches the feed on the same clock. `crossed_stops_precede_limits_in_trigger_price_order` now rests a sell limit at 10,250 instead of a buy limit at 10,500: no Live quote can make buy stops at 10,100/10,200 valid while keeping a 10,500 buy limit nonmarketable, so the original fixture was reachable only through the bypass; the stop-before-limit ordering assertion is unchanged. The fills, prices, P&L, fees, statuses, events and restart assertions are otherwise retained, and the irreparable-position error is read from the owner's `market_observation_error`. All 21 pass with and without the three bypasses; no golden changed. About 39 further service tests and the 21 replay tests still depend on the bypasses.
  - 2026-10-23 route-bypass migration, large service fixture batch (31 further tests in tests.rs plus the route-less Hyperliquid and chart-modify feed cases): bracket, protection, discipline, risk-meter, retention, session-plan, tilt, copier, news-restriction, startup-repair, significant-figure and flatten fixtures now route every fill-producing quote through the owner inbox on the deterministic clock; the feed cases keep their RouteUnknown-to-Live and stale-revision-first assertions (the V20-migrated legacy store is modeled by nulling the persisted route after a routed placement). New shared helpers: `bootstrap_always_open_feed`/`always_open_session` for far-future clocks, `resume_live_without_quote` (a provider resume drops the retained quote), `winning_round_trips`, `hyperliquid_quote`. Production rules now encoded in the fixtures: ticket interest keeps quote interest while an account is flat so the owner book's observation revision stays monotone (a pruned then recreated entry restarts its revision and the observation cursor fences the first post-recreate quotes as stale); a resting market order waits only while a Live feed holds no quote, so the pinned-bracket entry follows a provider resume; stops must not already be crossed against the retained quote (the arming test ticks the market up before placing the manual stop); DAY orders on a verified route carry a real acceptance-session deadline and a system-clock restart rolls risk sessions across every boundary since the fixture epoch, so the working-exposure test rests GTC orders and the drawdown/v21 tests restart on the same deterministic clock. All original assertions are retained (the dead `current_wall_unix_nanos` helper is removed). All 31 pass with and without the three bypasses; runtime 298 passed/7 ignored and full fmt, strict workspace Clippy/tests/naming passed. The 21 replay tests remain the last bypass-dependent harness before the bypasses and route-less regressions land.
  - 2026-10-23 route-bypass migration, replay harness: `ReplayHarness` now owns the replay instrument and delivers verified `replay-feed` Terms (built from the instrument's own contract and provenance, so the later registration is an idempotent no-op), an open session and Live feed through `apply_market_batch`. It re-primes that owner-memory feed after every restart. Golden setup restarts once at a rebased 1,000 ns owner clock so every fixture observation clears `eligible_after` and the recovery quote floor; the broker test and the post-restart cascade stamps of the bracket and maker goldens set the clock back to 5,000 to keep their recorded command stamps. Resting-limit admissions take an inert owner quote received at 1 ns: it passes the freshness check but predates the recovery floor, so it never executes anything and every fill still comes from its recorded fixture observation. Market entries that must wait for a fill observation are placed before the first quote or after a provider resume drops the retained quote. The flatten/reverse/kill commit-failure tests replay the same verified quote after the restart before the durable comparison, because quote marks are owner memory. All 25 replay tests pass with and without the three bypasses; runtime 298 passed/7 ignored bypass-free. Intentional golden change: `simulated_golden_v2.txt` stage revisions drop by one from the flatten stage on (10/13/16/21/25 to 9/12/15/20/24), because the verified route now arrives with the instrument registration instead of as a separate Terms revision at the first flatten quote. Flatten/reverse/kill provenance book sequences move from 1/2/3 to 2/4/5 because the owner book also counts the admission quote and the post-resume quote that production requires before the touch limit and the bracket entry. No order, event kind, fill price, quantity, timestamp or P&L changed; the other replay goldens are byte-identical. Next: remove the three bypasses and add the route-less admission, no-fill and post-Terms regressions.
  - [x] 2026-10-23 route-bypass removal: the three `#[cfg(test)]` route bypasses in `check_practice_session`, `check_practice_market` and `quote_order_eligible` are deleted, so test builds now run the same unknown-route admission and fill gating as production. New `market/route_gating_tests.rs` regressions: `route_less_instrument_rejects_admission_with_quote_unavailable` (a store instrument without verified Terms ignores session/Live/quote events, keeps `FeedState::Unavailable { RouteUnknown }`, and rejects both a market and a resting limit with `RejectReason::QuoteUnavailable`, with no order, event or revision written) and `route_less_resting_order_never_fills_until_verified_terms_resolve_the_route` (a resting GTC limit whose persisted route is nulled to model a V20 legacy row stays Working with no fill through a route-less owner quote and an eligible crossing direct observation, then fills at its 9,900 limit as soon as verified Terms, session, Live and a crossing quote arrive through the owner feed). By inspection of the removed branches, both tests would fail with the bypasses present, because admission and the direct fill would succeed. No test-only route branch remains in production code; runtime 300 passed/7 ignored, and full fmt, workspace check, strict workspace Clippy, workspace tests (2,378 passed) and naming check passed.
- [x] M5.8 checkpoints-retention-equity - Checkpoints before retention, repair from checkpoints, bounded equity history
  - Part 1/2 (V26): `position_checkpoints`, `ledger_checkpoints` and `equity_samples` land in the chain after V25,
    still unreachable for a gated pre-group store. Retention now runs at startup and hourly, folds each pruned fill
    prefix through the owner position math into its position checkpoint and the pruned/orphaned ledger entries into
    the ledger checkpoint, all inside the deletion transaction, and never deletes rows referenced by an open order
    or a live group. Fill replay order is execution time with the rowid commit tie-break everywhere. Recovery
    rebuilds a checkpointed position as checkpoint plus the retained suffix with the rebuild authoritative;
    `rebuild_position_from_retained_fills` is deleted. A ledgered balance is the checkpoint plus the exact sum of
    the remaining entries. Regressions: exact projection/balance recovery after pruning (fill_count 9, entry chain
    13+7, balance identity), live-group fill pinning, migration chain to V26. Runtime 250 passed/5 ignored.
  - Part 3 (bounded equity): every simulated fill carries one equity sample inside its commit transaction (the
    emergency close flushes its sample in a follow-up write); the 60 s owner timer samples only when equity changed
    plus once at each UTC risk-session boundary for profiled accounts; a 1,440-entry ring per account is rebuilt
    from the newest durable minute samples at open, and the first tick waits one interval so a restart replays the
    history unchanged. Retention downsamples minute rows older than seven days into last-of-hour rows and drops
    hourly rows older than a year in the same transaction, with both windows trailing the newest sample so a
    backward clock cannot mass-expire history. Regressions: per-fill/timer/session/restart sampling, ring bound,
    downsample collapse and year drop. Runtime 253 passed/5 ignored.
  - Part 4 (property run, VAL-TRD-060): `tests/property.rs` drives 200 seeds x 500 steps through the service
    (submits, cancels, modifies, quotes, partial closes, scale-ins, inline brackets, restarts, kill + unlock and
    retention runs) and asserts after every step: pooled coverage (executable legs never oppose or exceed the
    position, none rest on a flat account, and each role totals zero or exactly |net| unless a user action
    disturbed the pool), per-instruction fill quote eligibility, no DAY order open past its deadline, the balance
    identity and the 1,440-sample equity ring bound. Each of the ~3,000 restarts proves no orphan legs, position
    = checkpoint + retained suffix, balance = ledger checkpoint + remaining entries, and exact reopen equality
    whenever startup recovery reported no repair. The run caught a real defect: a pending bracket terminated
    through a held leg left its working entry naked (`stage_bracket_termination` cascaded only the legs); the
    entry now cancels with the group, locked by `cancelling_a_held_leg_cancels_the_pending_entry_too`.
    Runtime 255 passed/5 ignored.
  - 2026-10-23 round-2 follow-up (VAL-TRD-046): cancelling the pending 2-contract bracket entry asserts all three held child order events carry `Cancelled{Cascade}` and the group is Cancelled; this closes the last cascade test gap without changing the group writer.
- [x] M5.9 emergency-operations - Kill switch, flatten scopes and atomic reverse across all instruments
  - Status (2026-10-23 follow-up): VAL-TRD-064 failed on the current desktop because the account shortcut passed the active chart's instrument to `FlattenScope`. The shortcut now targets every instrument of the selected account using owner quotes, without requiring an order-book frame. The Flat ticket and position-close actions remain instrument-scoped. A deterministic two-instrument owner regression removes desktop interest from the second instrument before flatten, asserts exactly one closing fill per instrument at each owner's bid and flat persisted state; the GUI rerun on an isolated practice account showed two positions become zero within 2.3 s and stay zero after relaunch.
  - Input finding from the M5.8 property run: the kill switch cancels open group legs without in-session group
    reconciliation, leaving brackets Active with dead legs until startup recovery retires them (BrokenGroup
    notice); M5.9 owns the in-session reconciliation.
  - Part 1/3: Flatten and reverse are atomic owner steps priced only from the owner's verified quotes.
    `FlattenScope { account, instrument }` scopes any combination (None = wildcard); one `plan_atomic`
    computes close fills, scope cancellations and group reconciliation/alignment on a clone and commits it
    in a single `commit_quote_step`, so a failed commit leaves memory and disk at the last commit (restart
    retries it). Positions without a current-generation Live quote are reported as typed
    `FlattenOutcome.unpriced` and keep their protection and orders instead of a fabricated price. Reverse
    closes then stages one `sim-reverse-*` IOC market order with enforce_risk inside the same commit; a risk
    rejection leaves the position untouched. The economic-event risk action now flattens/cancels by the
    profile's scope through the same path, without a desktop quote. Broker flatten closes every position on
    the flattened instruments, scoped or global. Per-instrument verified observation joins
    `check_practice_market` and the Live-gated QuoteBook; the route check precedes it so a live account
    still gets the typed refusal. The obsolete per-step fill/insertion writers
    (`fill_policy_persistence`, `insert_fill`, `insert_order`, `FillPolicyPersistence`,
    `flush_pending_equity_samples`) are removed; `commit_quote_step` is the only fill/commit path. Desktop
    flatten-all and event triggers no longer need an order-book frame; flatten feedback reports unpriced
    counts.
  - Part 1 tests: flatten/reverse regressions converted from the test-only observation injection to the
    production feed path (Terms/session/Live/Quote batches through `apply_market_batch`); new regressions
    cover account/instrument scope isolation, unpriced-position reporting with protection retained,
    risk-rejected reverse no-op, and replay crash-during-flatten/reverse-commit atomicity. The v2 simulated
    golden regenerated intentionally: quote provenance is the replay feed with owner ingress sequences,
    per-instrument revisions number independently (no failed-commit rewrite attempts), the reverse leg is
    the `sim-reverse-*` IOC fill at bid, entry fills at touch time, the reversal leaves unrealized PnL 0
    and the bracket completes. Runtime 260 passed/5 ignored; desktop 449+80; full fmt, strict workspace
    Clippy, workspace tests and naming passed. Kill-switch outcome/reporting (TR-03), scripted-broker
    two-symbol parity and TR-04/TR-12 closure remain; M5.9 stays unchecked.
  - Part 2/3: The kill switch now performs the full emergency action in one durable step per
    invocation: it locks every in-scope tradable account, cancels their non-protective working
    orders with the typed `KillSwitch` cause (previously everything, including protection, was
    cancelled with `user_cancel`), and flattens every priced position at the owner's verified
    quotes with group reconciliation in the same `plan_atomic` transaction. Protection legs are
    never stripped: they retire with the flatten fill, and a position without a current verified
    quote keeps its stop and target and is reported in `EmergencyOutcome::unpriced`. Broker
    accounts are locked in the same step; their cancels and position closes remain venue requests
    reported through `pending_close_requests`/`incomplete` (the uncancellable-order report moved
    from an error string to `incomplete` while the lock stands). The result is the new
    `EmergencyOutcome { accounts_locked, cancelled, fills, pending_close_requests, unpriced,
    incomplete }`; the desktop kill feedback renders it. The economic-event Lock rule now shares
    `lock_and_disarm` (lock + non-protective cancel, no flatten) instead of calling the kill
    switch. The stale `expect(dead_code)` on `CancellationCause` is removed: every cause now has
    a production producer.
  - Part 2 tests: the four-instrument VAL-TRD-061 regression (bracketed long, protected short,
    non-protective limit, stale-quoted protected position) asserts the lock, the
    `Cancelled{KillSwitch}` limit, both flatten fills at bid/ask, retired protection on the
    flattened instruments, the intact stop and target on the stale instrument, the unpriced
    report, lock enforcement and exact restart replay; a replay crash test proves a failed kill
    commit leaves memory and disk unlocked and unflattened and retries cleanly after restart.
    The property run caught that it recorded bracket-leg instructions before activation assigned
    their exact limit prices (previously invisible because the old kill cancelled every leg);
    the harness now follows the activation assignment, with richer fill forensics. The v2
    simulated golden regenerated intentionally: the kill cancels the pending limit with the
    `kill_switch` reason and flattens the reversed short at the ask (new fill/order/ledger
    lines, position flat, realized 6250). Runtime 262 passed/5 ignored; desktop 449+81; full
    fmt, strict workspace Clippy, workspace tests and naming passed. Scripted-broker parity and
    TR-03/TR-04/TR-12 closure remain; M5.9 stays unchecked.
  - Part 3/3: `a_broker_flatten_requests_closes_for_every_instrument` proves the scripted
    broker parity of VAL-TRD-062: positions on two symbols both receive close requests from
    one account-wide flatten, and an instrument scope closes only its own.
    `reverse_on_a_locked_account_returns_account_locked_without_a_fill` completes VAL-TRD-063
    (atomic reverse: crash-replay and risk-reject were covered in parts 1-2). TR-03 and TR-04
    close; TR-12's reverse part closes here, the rest stays with M8.10. The item's GUI
    flatten-all pass (VAL-TRD-064) moves to the milestone GUI/probe pass; M5.9 is otherwise
    complete.
- [x] M5.10 risk-session-rollover - Persisted risk session with rollover; bracket legs count as attached stops
  - V27 adds `risk_sessions` (seeded per profile from the registration baseline) and `risk_locks.kind`; a manual lock
    survives rollover while daily-loss and trailing-drawdown locks release. The owner timer rolls the session at the
    account boundary (UTC midnight by default; 17:00 America/Chicago via `set_risk_session_boundary`), resets the
    daily meter, persists the rolled row with the realized baseline and writes a boundary equity sample in the same
    transaction. Bracket entry admission counts the template stop as attached, so the "no attached stop" warning
    fires only for genuinely stop-less entries. Session-aware realized P&L falls back to the registration baseline
    for gated pre-group stores. GUI/probe: none beyond the milestone pass; TR-09's desktop registration UI stays deferred.
- [x] M5.11 trading-publication-api - Change-driven, bounded publication with paged history
  - `TradingService::subscribe` returns a bounded `TradingSubscription` (at most 8 live subscribers, pruned on drop) whose
    per-subscriber slot holds at most 1,024 typed `TradingEvent`s; each event carries the exact bounded `WorkingSet`
    (open orders including inactive held legs, active groups, open positions, the newest 500 fills, position/account P&L,
    risk locks and recovery notices). The owner publishes after every revision-changing step and coalesces quote-only mark
    changes to at most one publication per 100 ms with a trailing flush; an idle runtime publishes nothing. A subscriber
    that overflows its queue resynchronizes with a fresh `lagged` working set and drops the stale queued events.
  - Paged order history (`HistoryQuery`, at most 500 rows per page, keyset pagination returning every order exactly once
    in insertion order) is served by a dedicated `aeris-trading-history` reader thread holding a `SQLITE_OPEN_READ_ONLY`
    connection opened after the owner has created the store; writes on it fail by construction. The compatibility
    snapshot no longer hard-fails past 10,000 stored orders (retention already bounds the live set). Measured on this
    machine: working-set build at 50,100 stored orders (50,000 terminal + 100 open + 601 fills) 3.44 ms; the cold-load
    snapshot path succeeds. VAL-TRD-066/067 covered by the seven `tests/publication.rs` tests; the >10,000-order GUI
    pass (VAL-TRD-068) stays with the milestone GUI pass; TR-05's runtime part closes here.
- [x] M5.12 paper-trading-live-probes - Live paper-trading probe on Hyperliquid with restart, plus broker demo regression
  - 2026-10-23 coverage follow-up: the isolated probe now has a bounded no-quote negative mode that withholds only SOL quotes from the trading owner while live Terms/Feed/Session still arrive, asserting the typed QuoteUnavailable rejection before returning non-zero. The positive run compares flatten sells to the recorded bid, records the first post-restart quote arrival and checks the exact durable revision alongside each state family, and verifies export replacement/parity against the snapshot fills and CSV with no staged files. The DAY and export check names match the active contract. Retired broker-borrow envelope fields are not restored. Clean-HEAD positive, no-fees and both negative runs still require final archived evidence.
  - `tools/aeris_probes` gains `paper_trading_probe` and a broker demo probe bin on a shared envelope lib. The paper
    probe wires the real market-runtime-to-trading feed on a validation root, registers a practice account (with a fee
    schedule or `--no-fees`), and exercises a market buy, resting limit, stop-limit, inline bracket, IOC, DAY expiry and
    a stale-quote rejection, then restarts the trading owner on the same database, checks field-identical durable state
    and quote resume without UI, flattens all and proves the ledger identity. Both live runs passed all 20 checks
    (fees run: fill at recorded ask with 0 ms quote age, restart digest differences [], ledger 9,999,778 =
    10,000,000 - 220 - 2; no-fees run: fees 0, balance == deposit). Roots kept for the post-run database assertions:
    `%LOCALAPPDATA%\Temp\aeris_validation\m512-paper` and `m512-paper-nofees`.
  - The probe exposed and fixed three production defects: catalog installs now re-arm the demand flush for waiting
    trading leases (install_provider_instrument), the quote-interest revision is durable in the store metadata so an
    owner restart resumes above the coordinator fence, and a provider reconnect republishes trading sessions before
    Feed::Live. Each fix carries a regression test.
  - The broker demo probe (since removed with its broker) was implemented (session/venue/reconcile checks, live-account refusal, account masking to
    the last two digits, far-limit place-and-cancel) but its run stays blocked until the maintainer approves it with
    the desktop closed; it was never executed here.

### M6 desktop bridge

Status: pending

- [x] M6.1 desktop-services-test-support - Inject `DesktopServices`; move `FixtureMarketWorker` to test support
  - Part 1: move the disconnected replay worker to cfg-gated `desktop/test_support/`, preserve its
    diagnostics-only conformance command and test clients, and enforce the boundary in naming_check.
  - Part 2: construct `DesktopServices` with the trading handle at startup; pass it through the
    lifecycle, workspace panes, UI actions and market client's ticket-interest worker. Delete the
    process-global trading service and its `handle()`/`install()` lookup, guarded by naming_check
    positive and negative examples. Independent in-memory desktop compositions retain separate
    trading owners. The feedback mailbox, account gate,
    market factory, persistence, clock and PaneHarness remain for subsequent M6.1 parts.
  - Part 3: the composition root constructs one account runtime per desktop instance and
    supplies it through `DesktopServices` to the account client, title bar, menu, lifecycle
    quiesce and workspace actions. The process-global `DesktopAccount::shared()` and
    account-service singleton are deleted together; a naming rule rejects their return,
    including production code after test modules. Two injected account sessions are
    independently allocated in the deterministic account test. The full workspace gate
    passed. Market, feedback, persistence, clock and PaneHarness remain for later parts;
    M6.1 stays unchecked.
  - Part 4: the composition root constructs the market owner once and injects it through
    `DesktopServices` into workspace market workers, their bounded factory and wake callbacks,
    provider login/actions, trading feed attachment and lifecycle shutdown. Removed the market
    `OnceLock` and lazy global lookups; a naming rule guards the instance boundary with positive
    and negative tests. An independent-consumer test attaches the same ID to two separate
    desktop market owners. Feedback, persistence, clock and PaneHarness remain; M6.1 stays unchecked.
    The full gate exposed the existing burst-conformance process-RSS race for a third time;
    its unchanged memory and delivery assertions now execute in an isolated test child, so
    concurrently allocating GPUI tests cannot be charged to the fixture's working set.
  - Part 5: trading command feedback is a per-instance handle created by `DesktopServices`.
    Command dispatch, chart intents, hotkeys, ticket actions, account controls and the pane
    status reader share that handle; the process-global mailbox and latest-feedback lookup
    are deleted, along with an unused repricing helper. A two-desktop test proves independent
    revisions and shared pane visibility; naming_check accepts injected/test-only examples
    but rejects production globals. Full workspace fmt/check/strict Clippy/tests/naming_check
    passed (the first full test run hit a known short-endurance parallel timing failure;
    its isolated retry and the complete rerun passed). Persistence, clock and PaneHarness
    remain; M6.1 stays unchecked.
  - Part 6: `DesktopServices` supplies a per-instance persistence root and coalesced
    chart-chrome save owner. Startup restore, workspace layout background saves, pane
    preference writes, update restart and exit durability checks use the injected
    path/receipt. Removed the process-global chart-chrome mutexes and implicit workspace
    path lookup; kept existing file format, backup fallback, generation checks and
    bounded off-thread saves. File-backed two-owner tests and a naming guard cover
    isolation; full workspace fmt/check/strict Clippy/tests/naming_check passed.
    Clock injection and PaneHarness remain;
    M6.1 stays unchecked.
  - Part 7: run the short-endurance regression's unchanged frame, continuity,
    mailbox, memory and clean-stop assertions in an isolated test child, so
    unrelated parallel GPUI allocations cannot be charged to its process RSS.
    Production endurance and its wall-clock bound are unchanged. Clock
    injection and PaneHarness remain; M6.1 stays unchecked.
  - Part 8: the composition root supplies a per-instance clock through
    `DesktopServices` and the shared trading feedback handle. Chart intents,
    simulated ticket/ladder orders, modifications, kill-switch commands,
    economic-event dispatch, alert creation and pane time-zone labels now
    read that injected clock instead of `trading::now()` or local wall-clock
    lookups. The old trading timestamp helper is deleted and naming_check
    guards against its return; independent ManualClock instances prove pane
    feedback sees its own clock. Market worker publication/session timestamps
    and remaining desktop wall-clock presentation paths require the next
    clock-routing part; FixtureMarket/PaneHarness and VAL-DSK-001/002
    closeout remain. M6.1 stays unchecked.
  - Part 9: market-worker session status, selection outcomes, chart snapshots,
    live tails and recovery snapshots now use the clock supplied by the same
    `DesktopServices` instance as their trading commands. The worker no longer
    reads the process wall clock. A ManualClock session regression covers two
    different session dates; conversion regressions check the publication
    provenance timestamp. Other desktop wall-clock presentation helpers and
    FixtureMarket/PaneHarness remain for later parts. M6.1 stays unchecked.
  - Part 10: header session countdowns, fill toasts, economic-calendar rows,
    chart time-zone badges and provider copyright years receive the desktop
    clock through their owning app/pane state rather than reading process wall
    time in the rendering components. A ManualClock calendar regression checks
    visibility across the release window. Startup quarantine file naming,
    dormant account/update internals and diagnostics remain outside this
    presentation change; FixtureMarket/PaneHarness and VAL-DSK-001/002 closeout
    remain. M6.1 stays unchecked.
  - Part 11: test-only `PaneHarness` drives an actual GPUI workspace pane from
    the gated disconnected `FixtureMarketWorker` through the bounded market
    mailbox, including snapshot, readiness and chart rendering. No provider
    session is opened by the fixture. The default-features desktop build and
    full workspace gate passed; VAL-DSK-001/002 are closed. Startup corruption
    quarantine filenames and dormant account/update lifecycle timestamps are
    outside M6.1's injected presentation/command clock boundary; dormant
    lifecycle hardening remains M8 work.
- [x] M6.2 notification-center - One `UserNotice` model and `NotificationCenter`
  - Part 1: added the bounded 32-key notice owner and wired the existing toast cards to its
    keyed projections. Repeated update and chart-preference save failures replace their visible
    notice rather than stacking; successful asynchronous preference saves clear the persistent
    failure, and timed notices leave the owner when their toast expires. Logging includes only
    static titles and severity, not raw errors or detail. Six deterministic notification/toaster
    tests cover replacement, eviction priority, persistent-key bound, targeted clearing,
    secret-bearing error exclusion, and rendered-card deduplication; an additional test covers
    successful-save clearing. The full fmt/check/strict Clippy/tests/naming_check gate passed
    with serialized Cargo compilation. M6.2 stays unchecked: pane strings, four PaneHarness
    error injections, complete source/action variants and remaining discard/unwrap cleanup follow.
  - Part 2: removed two dead desktop presentation/selection parameters and replaced
    the Rithmic shell startup panic, missing workspace factory panic and GPUI window
    opening panics with explicit diagnostic/error paths. The full workspace
    fmt/check/strict Clippy/tests/naming_check gate passed with serialized Cargo.
    The bounded notice migration
    and remaining production registry/startup invariants remain open.
  - Part 3 (test stability): the seeded 200-world/500-step trading property run keeps
    every invariant and restart check but allows up to 10 seconds for each owner
    shutdown under parallel scheduler contention (previously 2 seconds). Its
    focused regression passed all 200 seeds; full workspace fmt/check/strict
    Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1; M6.2 notice
    consolidation remains open.
  - Part 4: pane order-book demand failures now emit a keyed, sanitized notice into the
    TerminalApp-owned toaster, with successful requests clearing that pane's condition.
    Restored and newly created panes share the same subscription; retired pane cards
    release their notice keys. A disconnected PaneHarness command receiver exercises
    repeated failure through the actual pane and rendered toast. M6.2 remains unchecked:
    restore setters, price-alert processing, symbol-search notices, the other injected
    failures, complete source/action variants and remaining discard/unwrap cleanup follow.
  - Part 5: market-runtime price-alert processing reports sanitized failure/success
    controls to the pane. Repeated failures post one pane-keyed TerminalApp notice and
    show a safe dialog message; a later successful runtime replacement clears both.
    The worker no longer transfers raw processing errors to the UI. PaneHarness drives
    the bounded publication mailbox, checks one rendered card, then success clearing.
    M6.2 remains unchecked: restore setters, symbol search, remaining injected failures,
    source/action variants and discarded-result/unwrap cleanup follow.
  - Part 6: snapshot chart restoration now collects actual fallible setter outcomes
    at the pane boundary, posts one sanitized pane-keyed notice to TerminalApp on
    failure and clears it on a successful replacement. An invalid persisted IANA
    zone exercises the real chart setter on initial and replacement snapshots in
    PaneHarness; one rendered card remains after both failures. No second chart
    presentation state was added. M6.2 stays unchecked: startup chart setter
    failures, symbol search, remaining injected failures, source/action variants
    and discarded-result/unwrap cleanup follow; M6.4 still owns PanePresentation.
  - Part 7: failed symbol-search dispatch and current-generation catalog search
    rejection post the same pane-keyed sanitized notice to TerminalApp; an accepted
    search completion clears it. PaneHarness fills the real bounded command queue,
    repeats a credential-shaped query without leaking it to notice detail, renders
    exactly one card, then drains the queue and confirms completion clears the card.
    M6.2 stays unchecked: startup chart setters, broader pane-string consolidation,
    source/action variants and discarded-result/unwrap cleanup follow.
  - Part 8: removed the pane's price-alert message and duplicate dialog error
    rendering. Restored/default alert queue failure is retained only until pane
    notice subscription is wired; action validation and command failure, plus
    runtime synchronization failure, post sanitized keyed notices to TerminalApp.
    Action retry or correction and runtime success clear their respective keys.
    PaneHarness covers failed restore, repeated bounded-queue deletion failure,
    rendered-card deduplication and successful clearing. M6.2 remains unchecked:
    startup chart setters, other pane strings, source/action variants and
    discarded-result/unwrap cleanup remain.
  - Part 9: deleted the pane's `symbol_message`. The instrument menu derives
    normal searching, queued, result-count, no-match, selection and ready labels
    from its fenced symbol browser and current chart selection, without storing
    another status string. Invalid search, unavailable dispatch, catalog rejection
    and failed selection post sanitized pane-keyed notices; accepted results and
    committed selections clear their respective failures. PaneHarness verifies
    repeated search/selection failure deduplication, redaction, rendered cards,
    derived statuses and subsequent clearing. No chart-integration identifier
    needed removal. M6.2 stays unchecked: startup chart setters, indicator and
    chart-state strings, source/action variants and discarded-result cleanup follow.
  - Part 10: removed the per-pane indicator error string and its header/menu
    projection; the indicator menu keeps its domain-derived count hint. Study
    registration, settings/removal, drawing, order-flow and chart-setting failures
    post only static sanitized pane-keyed notices to TerminalApp; successful
    operations clear the relevant category. Study-settings dialog feedback remains
    dialog-local but runtime rejection text is sanitized. PaneHarness exercises
    repeated study registration failure, removed-volume settings and invalid zone,
    each with one rendered card and a recovery clear. M6.2 stays unchecked:
    startup chart setters, chart-state presentation, source/action variants and
    discarded-result/unwrap cleanup remain.
  - Part 11: removed the pane's chart-state text and rendered it from typed
    chart/connection state, pending interval and a typed startup/runtime failure
    cause. Unsupported restored providers and worker-stop states retain their
    distinct, sanitized chart status; repeated runtime failures use the single
    pane-keyed notice and successful coverage clears it. PaneHarness verifies
    a redacted rendered card, one-card deduplication and snapshot recovery.
    M6.2 stays unchecked: startup chart setters, remaining connection/series
    feedback, source/action variants and discarded-result/unwrap cleanup remain.
  - Part 12: moved empty-chart startup time-zone and order-line setters to the
    post-subscription pane initialization; their actual `Result` errors join
    the existing sanitized `chart_restore` notice, without retaining a second
    failure flag. A covering snapshot with successful restored settings clears
    that key. PaneHarness creates the real Rithmic startup chart with an invalid
    persisted zone, checks initial rendered notice, repeated failure deduplication
    and a corrected-zone replacement snapshot clearing it. A setter's `Ok(false)`
    is unchanged, not a failure; the startup price-axis action's `false` means
    no target series yet, so it is not reported as an error. M6.2 stays unchecked:
    connection/series feedback, source/action variants and discarded-result/unwrap
    cleanup remain.
  - Part 13: existing pane-owned notices now carry their pane source, provider
    selection failures carry the selected browser provider's registered ID,
    and workspace/persistence notices retain their existing source. A typed
    `Retry(ChartChromePreferences)` action is attached only to the durable
    preference-save failure; the toaster renders a real Retry button and the
    TerminalApp owner requeues its existing asynchronous save. Button activation
    rechecks the live keyed notice, does not dismiss it prematurely, and success
    still clears the key. Rendered click, bounded keyed replacement, pane-source
    and selection regressions cover this increment. M6.2 stays unchecked:
    Trading(RequestId) awaits a real request-scoped notice, OpenFolder needs a
    safe platform opener, and ConnectProvider requires M6.9 descriptor-driven
    login rather than another provider switch; connection/series feedback and
    discarded-result/unwrap cleanup also remain.
  - Part 14: audited the bounded market/workspace discards. Failed pane
    resource-class demand now posts one sanitized, keyed notice through the
    existing owner; a successfully queued class clears it. Detached shutdown
    treats a closed receiver as expected, reports queue saturation only to
    diagnostics and clears stale demand notices. A full worker retirement queue
    explicitly wakes and drains before sender closure; implicit Drop abandons
    only the acknowledgement, never blocks the UI. Failed automatic study
    removal and an invalid previously accepted search retained for retry are
    diagnosed without duplicating user notices. Default-listing searches already
    post a keyed notice on failure, while a pending search retains the replacement;
    empty-chart precision `false` is not an error. The existing order-book
    request and pending price-alert handlers already post keyed notices, so
    neither needed another path. PaneHarness and worker tests cover saturated
    demand, deduplication, recovery, detached shutdown and full-queue retirement.
    M6.2 remains unchecked: GPUI owner-update discards, account profile refresh,
    market bridge feedback sends, other shutdown/diagnostic discards, connection
    and series feedback, deferred source/actions and unwrap cleanup remain.
  - Part 15: pane and workspace async callbacks now inspect GPUI weak-owner
    updates: a released owner is an expected cancellation, while a still-live
    owner whose update fails is diagnosed. This covers chrome/flyout/copy
    timers, broker and provider connection results, context key saves, trading
    refresh, toaster wake, drawing input/template submission and the fullscreen
    hint without blocking the UI or adding state. The remaining `chart.update`
    discards in trading projection consume chart-setter change booleans, not
    weak-owner update errors. Audited `terminal_view` and `command_registry`:
    their only production `expect`s assert constant startup layout and
    compile-time registry completeness (the registry test checks all IDs);
    neither has an optional failure to propagate without weakening its
    invariant. M6.2 remains unchecked for profile refresh, market bridge and
    shutdown discards, connection/series feedback, deferred source/actions and
    other unwrap cleanup.
  - Part 16: the market bridge now delivers connection/chart errors through its
    existing bounded pane mailbox and chart-failure notice projection, diagnoses
    saturated control sends instead of silently discarding them, and treats a
    released presentation receiver as benign teardown. Catalog control
    saturation returns an explicit error; endpoint removal failures reach
    diagnostics, while released or already-acknowledged retirement waiters are
    handled explicitly. UI diagnostics feedback is optional after its collector
    closes. Partial-startup trading/context shutdown errors and failed startup
    dialogs reach diagnostics; failed final diagnostic flush emits a static
    stderr-safe diagnostic without retrying the flush. A focused mailbox test
    covers delivery, control reserve
    saturation and receiver retirement. M6.2 remains unchecked: the three
    account profile-refresh requests in workspace tabs, connection/series
    feedback, deferred Trading/OpenFolder/ConnectProvider sources/actions, and
    remaining unwrap/discard cleanup remain. Update-process cleanup, optional
    auth cancellation, and chart change booleans are separate follow-ups; no
    M6.6 bridge collapse was attempted.
  - Part 17: account profile refresh after aborted update restart and window
    activation now skips signed-out teardown and diagnoses bounded-queue
    failures without exposing account details. Live UI failures post one
    workspace-keyed sanitized notice; a released UI owner uses diagnostics.
    Browser-open login cancellation failure is diagnostic only; disabled
    production authentication remains disabled, with no new backend traffic
    or desktop-owned account state. Account tests cover signed-out, live
    success, and full-queue failure. M6.2 remains unchecked for
    connection/series feedback, deferred Trading/OpenFolder/ConnectProvider
    sources/actions, and remaining unwrap/discard cleanup.
  - Part 18: stopped market connections use the existing typed chart-failure
    projection and pane-keyed `chart_state` notice; streaming recovery clears
    only a worker-stop failure. Timeframe and market-history dispatch failures
    use one sanitized `series_selection` pane notice, cleared on a successful
    dispatch or ready/awaiting-data state. Normal connection and series phases
    remain typed status, not extra notices. Removed the unused stored
    connection string, provider-specific startup text builder, Rithmic shell
    message and the unused series-header text pass-through; quick-timeframe
    failure explanation stays local to its input. PaneHarness exercises
    repeated connection/series errors, one rendered card, redaction, and
    successful clearing. M6.2 remains unchecked for deferred
    Trading/OpenFolder/ConnectProvider sources/actions and other
    unwrap/discard cleanup. No M6.3 generation-binding work was attempted.
  - Part 19: the chart trading projection now handles all three fallible
    Aeris Charts setters (trading snapshot, host overlay, session-plan levels)
    at the pane boundary, notifies the chart on success, and posts one
    sanitized pane-keyed `trading_projection` notice on failure. A successful
    projection clears it. PaneHarness injects invalid session-plan data,
    repeats the error, verifies one rendered redacted card and recovery.
    Removed the redundant workspace context discard. Audited the specifically
    named `components/terminal_view.rs`, `command_registry.rs` and `desktop.rs`:
    they have no production `let _ =` discards; the first two retain only the
    tested constant-root and exhaustive command-registry `expect` invariants,
    and desktop.rs has no production `expect`/`unwrap` (its remaining ones
    are test-only). M6.2 stays unchecked. `Trading(RequestId)` cannot be
    honestly sourced from the current revision-and-string-only desktop
    `TradingFeedback`; the trading owner must expose a stable request identity
    in command feedback first. `OpenFolder` needs a safe local-path opener
    owned by `platform_runtime` (only URL browser opening exists).
    `ConnectProvider(ProviderId)` needs the M6.9 descriptor-driven connect
    dispatch; production account authentication remains disabled. Do not add
    no-op buttons or a parallel trading/account store to satisfy variants.
    Other update-process kill/wait/spawn result discards remain in `update.rs`
    and need owner-level cleanup; the chart-setter discards are closed here.
    VAL-DSK-003..007 identifiers were not found in this checkout's
    stabilization reports, so their external acceptance evidence was not
    asserted.
  - Part 20: classified dormant update launcher/helper termination and
    reaping results on the update worker, including already-exited children,
    OS failures with sanitized diagnostics, and a bounded detached cleanup
    worker for teardown without process waiting on GPUI. A failed cleanup
    worker spawn retains the process and single-flight slot rather than
    permitting concurrent restarts; a termination failure also retains the
    slot. The retired acknowledgement receiver is diagnosed without printing
    wire data. A subprocess test covers both exited and running helpers.
    Automatic updates remain disabled; no release was installed/published.
    Re-audited against the active mission's `validation-contract.md`:
    VAL-DSK-003 has four passing notification tests and a 32-notice bound;
    VAL-DSK-004 has zero named legacy message fields and a toaster rendering
    test; VAL-DSK-005 has PaneHarness cases for restore, order-book, alert
    processing and search with deduplication, but not every case explicitly
    asserts both source and severity; VAL-DSK-006 logs only the static title
    and severity and tests that secret input is absent from retained notices,
    but the test does not capture/assert the diagnostic line. VAL-DSK-007:
    `let _ = provider;` has zero matches. The listed production `expect`
    calls are the commented, tested constant startup-root and exhaustive
    command-registry invariants; `desktop.rs` matches are test-only.
    The contract's literal zero-match `selection_pending` check cannot pass:
    actively used selection state/parameters remain in desktop and symbol
    menu. Removing them solely for the textual assertion would change
    selection behavior; reconcile that contract with the specific dead
    parameter intended before claiming closure. M6.2 remains unchecked for
    these evidence gaps and the blocked Trading/OpenFolder/ConnectProvider
    sources/actions described in Part 19.
  - Part 21: closed the literal VAL-DSK-005 evidence gap for the four
    PaneHarness injections. Restore, order-book dispatch and symbol-search
    failures now use `Error` severity and loss tone, matching the existing
    price-alert processing failure. Each regression asserts exactly one
    rendered notice after repeated failure, pane source and error severity.
    VAL-DSK-006 now has an isolated subprocess PaneHarness regression that
    injects a secret-bearing invalid restored chart zone through the real
    setter, observes its sanitized rendered detail, captures the real
    bounded observability sink, drains it and asserts exactly one diagnostic
    notice line containing the public title and no secret. Its temporary
    fixture is removed on success; no production logging hook or raw-error
    diagnostic was added. M6.2 remains unchecked: VAL-DSK-007's literal
    `selection_pending` zero-match still contradicts active selection state,
    and Trading(RequestId), OpenFolder and ConnectProvider still lack the
    prerequisites recorded in Part 19.
  - Closeout: VAL-DSK-003..006 pass the bounded-center, toaster and four
    PaneHarness injected-failure tests, including captured diagnostic redaction.
    VAL-DSK-007's active `selection_pending` is an in-flight selection guard,
    not a dead parameter; the obsolete `let _ = provider;` is gone. Production
    `expect`s in the listed files are limited to documented, tested startup
    layout and exhaustive registry invariants; `desktop.rs` matches are test-only.
    The final notice contract is dependency-aware: M6.9 wires descriptor
    `ConnectProvider`, M6.10 wires stable-ID `Trading` feedback, and M7.8
    wires trusted-location `OpenFolder`. M6.12 enforces remaining desktop-wide
    discards. No placeholder action or parallel owner is introduced.
- [ ] M6.3 chart-binding-generation-fence - Per-chart `ChartBinding` drops stale publications in one place (code complete in `ChartPaneController`, which took the planned `ChartBinding` role; Part 28b guarded VAL-DSK-013/014/015 GUI storm not yet validated)
  - Part 1: fixed a replacement tape rewrite invalidating the still-displayed
    chart's order-flow resume cursor before its covering snapshot. The tape
    remains retained for projection into the replacement chart. The existing
    interval-switch GPUI regression now delivers a rewritten prefix, asserts
    that the old chart retains its ordinal, and verifies the replacement
    receives the latest tape. This is not the full binding migration:
    per-message exact fencing, switch-guard deletion, interleavings,
    failure restoration and switch-storm GUI evidence remain outstanding.
  - Part 2: a runtime rejection before the switch marker now carries its exact
    selection sequence to the pane instead of a generic readiness state that
    left the pending switch stuck under a loading cover. Only the matching
    pending request rolls back; the original chart, instrument and interval
    remain visible, with one sanitized keyed notice. GPUI fixture tests cover
    failed interval retries, failed instrument selection and a retired failure
    interleaved with a newer request. This fixes the pre-marker failure path
    only. The unified binding, post-marker failure restoration, one acceptance
    point, switch-guard removal, full interleavings and GUI storm remain pending.
  - Part 3: interval-only selection keeps the previous instrument's chart
    visible without the instrument-change cover in both pending and committed
    phases; a different instrument remains covered through its handoff. A
    failing-first GPUI pane regression and instrument countercase exercise the
    actual selection commands and mailbox marker. The focused binary
    `PaneHarness` tests require
    `cargo test -p aeris_desktop --all-features --locked <test-name>`:
    Cargo builds the binary test against the desktop library without
    `cfg(test)`, so its fixture export is unavailable without the explicit
    `diagnostics` feature. Default production builds intentionally cannot
    construct `FixtureMarketWorker`. The prior default-feature focused
    invocation failed; it was not a passing check. Exact-generation binding,
    post-marker failure restoration, switch-guard deletion, interleavings and
    GUI storm remain pending.
  - Part 4: a rejected replacement after its switch marker now restores the
    previous chart's Ready presentation rather than misreporting that still
    valid chart as Error. Interval and instrument failures post one sanitized,
    pane-keyed selection notice; repeated instrument failures deduplicate.
    Failing-first GPUI PaneHarness coverage checks both identities, intervals,
    chart entity and notice contents. Exact-generation ChartBinding acceptance,
    late-error fencing, switch-guard removal, permutation/seeded interleavings,
    cover-through-ready and guarded GUI switch storm remain pending.
  - Part 5: moved the pending selection sequence out of WorkspaceSurface into
    ChartPaneController. Both commit markers and pre-marker failures now consume
    only the exact pending generation at its selection-outcome acceptance point;
    a retired or repeated outcome cannot clear a newer request. Pure outcome
    interleaving and the existing GPUI retired-failure regression cover this
    boundary. M6.3 remains unchecked: runtime publications, chart/tape state
    and switch guards still need the unified binding acceptance,
    permutation/seeded publication tests and guarded GUI switch storm.
  - Part 6: initial instrument descriptor replies now carry their runtime
    selection generation through the pane mailbox. ChartPaneController retains
    the latest requested sequence and rejects queued initial descriptors during
    a pending selection or after a newer request was committed or cleared.
    A GPUI regression delivers a retired same-instrument descriptor before and
    after the marker and checks that it cannot rewrite the selected terms;
    a pure controller test also covers cleared requests. M6.3 stays unchecked:
    state, snapshot, update, tape and other pane messages still require exact
    binding acceptance, legacy switch guards still remain, and permutation,
    seeded interleaving, cover-through-ready and GUI storm checks are pending.
  - Part 7: market-session statuses now carry the runtime selection generation
    through the pane mailbox and are admitted only for the committed chart
    binding. Initial descriptor adoption establishes that binding; selection
    markers advance it, and instrument changes clear the previous session
    status. A pre-commit catalog status for a different instrument is ignored.
    A pure controller regression covers pending, committed and failed
    selections; a GPUI PaneHarness regression delivers a retired closed
    session after the marker and verifies only the new status is shown.
    M6.3 stays unchecked: state, snapshots, updates, tape, other pane messages,
    switch-guard removal, full interleavings and guarded GUI storms remain.
  - Part 8: trade-tape snapshots now pass the pane controller's exact committed
    selection-generation fence before replacing retained tape or projecting
    trades into the chart. The interval-switch GPUI regression uses real
    selection generations, sends a higher-revision retired tape after a second
    marker, and verifies the replacement renders only the current selection's
    tape while the prior chart's cursor stays intact. The tape revision check
    still orders publications within one generation; it no longer treats a
    larger numeric generation as sufficient to cross the pane boundary.
    Other pane state, snapshot/update and auxiliary message acceptance,
    legacy switch guards, complete interleavings, cover-through-ready and
    guarded GUI switch storms remain; M6.3 stays unchecked.
  - Part 9: order-book frames now pass the same exact committed-selection
    acceptance as session statuses and tape before reaching the ladder. A
    failing-first GPUI PaneHarness regression delivers an early replacement
    frame while the old chart is displayed, then a retired higher-revision
    frame after the marker, followed by a current frame. The ladder retains
    only its current selection; its own revision ordering still applies
    within a selection. State, bar snapshots/updates, other pane messages,
    legacy switch guards, interleavings, cover-through-ready and guarded GUI
    storms remain; M6.3 stays unchecked.
  - Part 10: retained trade-tape ordering now resets at a committed selection
    boundary instead of comparing unrelated provider-session generations.
    The strengthened GPUI interval-switch test first failed because the
    replacement chart received no tape after its predecessor's higher provider
    generation; it now verifies the replacement's lower-generation tape renders.
    Within a selection, provider-generation and revision ordering remain
    unchanged. State, bar snapshots/updates, auxiliary messages, legacy switch
    guards, complete interleavings, cover-through-ready and guarded GUI storms
    remain; M6.3 stays unchecked.
  - Part 11: runtime bar snapshots and updates carry their selection generation
    into the desktop publication envelope and pass exact committed-generation
    acceptance before touching the chart, its metadata or live evidence. A GPUI
    regression rejects early replacement and retired snapshots, accepts the
    committed replacement and drops a retired incremental update. The existing
    order-flow interval-switch fixture now stamps its replacement selection.
    A cold 31.7 GiB target was cleaned and CARGO_BUILD_JOBS=1 used for all Cargo
    gates. ChartState and auxiliary messages, legacy switch guards, exhaustive
    interleavings, cover-through-ready and guarded GUI storms remain; M6.3
    stays unchecked.
  - Part 12: runtime series-readiness and retryable demand-error states now
    carry their selection generation through the pane mailbox. The controller
    rejects states for a pending replacement and a retired selection before
    presentation mutation; a GPUI regression checks both and a committed
    state. The legacy unfenced worker-error/startup state, connection and
    auxiliary messages, switch guards, exhaustive interleavings,
    cover-through-ready and guarded GUI storms remain. M6.3 stays unchecked.
  - Part 13: provider connection states and startup connection announcements
    now carry their pane selection generation. The controller rejects pending
    and retired connection messages before they can change chart failure,
    connection, ladder or latency presentation; a GPUI PaneHarness regression
    checks both drops and a committed streaming update. Worker-error/startup
    chart states, auxiliary messages, legacy switch guards, permutation and
    seeded interleavings, cover-through-ready and guarded GUI storms remain;
    M6.3 stays unchecked.
  - Part 14: market-series worker failures (including attach, command, runtime
    fault, pushed-event and group teardown) and startup/loading states now
    carry the endpoint's selection generation into `SeriesState`, using the
    existing committed-selection pane fence. Non-series catalog and screen
    failures keep their app-scoped `State` channel. A failing-first mailbox
    regression verifies the generation stamp and bounded reserve; an app
    regression verifies catalog failure routing, and the existing GPUI
    interleaving rejects retired series states. The pre-selection catalog
    pane, legacy fixture State route, auxiliary pane messages, switch-guard
    deletion, complete interleavings and guarded GUI storms remain; M6.3
    stays unchecked.
  - Part 15: demand-bound study output, output invalidation and removal now
    pass the pane controller's exact committed-selection acceptance before
    mutating chart or study presentation. Study lifecycle publications without
    a demand generation remain consumer-scoped for pre-selection registration
    and cleanup. A pure controller test covers pending, retired and committed
    generations; a GPUI PaneHarness regression verifies that a retired removal
    cannot delete a current study, while its current-generation removal works.
    Full serial workspace fmt/check/strict Clippy/tests/naming_check passed.
    M6.3 remains unchecked: pre-selection catalog pane and legacy State cleanup,
    other auxiliary messages, switch-guard deletion, permutations, at least 100
    seeded interleavings, cover-through-ready and guarded GUI storms remain.
  - Part 16: catalog and screen endpoints now report app-scoped failures as
    `CatalogFailure`, never as a chart state. A pane posts one sanitized,
    deduplicated catalog notice without changing its chart; the screener and
    market summary retain their failure presentation. GPUI tests cover failure
    before a series selection and after a chart is ready, and a mailbox test
    verifies the non-series route. Full serial workspace fmt/check/strict
    Clippy/tests/naming_check passed. M6.3 stays unchecked: retire the legacy
    fixture `State` route, fence remaining auxiliary pane publications, delete
    switch guards, run permutation and 100+ seeded interleavings, verify
    cover-through-ready and perform guarded GUI switch storms.
  - Part 17: removed the unfenced `MarketWorkerMessage::State` variant and all
    pane, screener and market-summary handlers for it. PaneHarness chart
    readiness and failure fixtures now use exact selection-stamped `SeriesState`;
    mailbox characterization tests do likewise. The committed interval failure
    regression rejects a retired error before accepting the replacement error.
    M6.3 stays unchecked: auxiliary pane publications, switch-guard removal,
    exhaustive interleavings, cover-through-ready and guarded GUI storms remain.
  - Part 18: chart recovery replies now carry their source pane selection
    generation through the worker mailbox and pass exact committed-selection
    acceptance before the chart recovery bridge or notices can change. A GPUI
    PaneHarness regression issues recovery on the old chart, commits a replacement,
    and rejects the retired failure; runtime recovery tests assert the stamped
    generation for success and superseded replies. The focused regression and
    serial workspace fmt/check/strict Clippy/tests/naming gates passed. The
    30.3 GiB target was cleaned before the cold build; CARGO_BUILD_JOBS=1 was used.
    M6.3 stays unchecked: remaining auxiliary publications, switch guards,
    permutations, 100+ seeded interleavings, cover-through-ready and guarded GUI
    storms remain. UI-02 and MD-15 are not yet closed.
  - Part 19: removed the never-produced `ChartViewport` worker reply and its
    unfenced pane handler. Restored viewports still apply from pane initialization;
    the generation-stamped `ChartViewportUpdate` command remains the live path.
    After cleaning 26.4 GiB of generated artifacts, serial workspace
    fmt/check/strict Clippy/tests/naming gates passed with CARGO_BUILD_JOBS=1.
    M6.3 stays unchecked: audit the other auxiliary pane publications, remove
    switch guards, run permutations and 100+ seeded interleavings, verify
    cover-through-ready and complete guarded GUI storms.
  - Part 20: a failed pending selection now retires only its request generation,
    leaving the previous displayed binding eligible for later publications.
    A pure controller regression covers rejection, repeated/stale outcomes and
    later commit; a GPUI interval-failure regression accepts the previous
    generation's market-session status after rejection and retains one notice.
    The two focused regressions and all five serial workspace gates passed after
    cleaning 26.2 GiB generated artifacts with CARGO_BUILD_JOBS=1. UI-02 and
    MD-15 remain open; M6.3 stays unchecked pending auxiliary publication audit,
    switch-guard deletion, fixed permutations, 100+ seeded interleavings,
    cover-through-ready and guarded GUI storms.
  - Part 21: a prior series Error delivered while a replacement request is
    pending no longer clears the request or prevents its marker from committing.
    The pane controller rejects lifecycle states during pending selection but
    continues to admit the displayed series' data; after a failed request the
    previous binding's lifecycle states are accepted again. A failing-first
    GPUI regression and a pure controller transition test pass. All five serial
    workspace gates passed after cleaning 28.1 GiB of generated artifacts, with
    CARGO_BUILD_JOBS=1 for the cold build. UI-02 and MD-15 stay open; M6.3
    remains unchecked pending auxiliary publication audit, switch-guard removal,
    fixed permutations, 100+ seeded interleavings, cover-through-ready and
    guarded GUI switch storms.
  - Part 22: catalog-driven chart selection now records its exact pending
    generation when dispatch succeeds, before catalog resolution. Old-series
    lifecycle failures cannot cancel that request during the catalog wait;
    catalog success and failure require its exact sequence, and rejection restores
    the displayed series' lifecycle acceptance. A GPUI regression covers the
    pending error and failed catalog response. M6.3 remains unchecked for the
    remaining auxiliary audit, switch guards, interleavings, cover-through-ready
    and guarded GUI storms.
  - Part 23: a recovery result for the displayed old series is a lifecycle
    outcome, not a bar publication; it cannot fail or settle a pending replacement
    selection. The failing-first GPUI regression observed the old recovery failure
    clear the request, then verified the pending marker commits without a failure
    notice after the controller fence. All five serial workspace gates passed after
    cleaning 29.0 GiB of generated output, with CARGO_BUILD_JOBS=1. UI-02 and MD-15
    remain open; M6.3 stays unchecked pending the rest of the auxiliary audit,
    switch-guard removal, fixed permutations, 100+ seeded interleavings,
    cover-through-ready and guarded GUI storms.
  - Part 24 mailbox audit (`market_worker.rs::MarketWorkerMessage` ->
    `workspace_surface.rs::apply_market_worker_message`, including delegated
    handlers; all current variants enumerated):
    - `Update` (snapshot and delta), `MarketSessionStatus`, `OrderBook`,
      `TradeTape`, and `Connection`: exact displayed selection generation via
      `ChartPaneController::accept_publication`, before chart, ladder, tape,
      session or connection mutation.
    - `SeriesState` and `Recovery`: exact displayed generation via
      `accept_series_state`, which also rejects lifecycle outcomes while a
      replacement request is pending. `Recovery` additionally matches its
      chart request ID. These are not ordinary old-series data publications.
    - `StudyOutput`, `StudyOutputsInvalidated`, `StudyRemoved`: demand-bound
      output uses `accept_study_publication(Some(generation))`; generation-free
      pre-selection consumer study lifecycle uses its `None` arm. The separate
      `StudyRegistered` and `StudyRegistrationFailed` replies consume the
      outstanding registration request sequence; `StudyReinitialized`,
      `StudyReinitializationFailed`, `StudyRemovalFailed` use the registered
      study ID and the consumer's pending reinitialization/removal state.
      These are consumer-scoped operations, not series publications.
    - `EngineSelectionCommitted` and `EngineSelectionFailed`: exact pending selection
      sequence via `accept` and `reject` respectively (legacy pending-switch
      guards still also apply). `SelectedInstrument`: initial descriptor
      adoption only when there is no pending request and its generation is not
      older than the latest requested selection. `CatalogSwitchCommitted`:
      exact pending sequence plus symbol-browser selection generation before
      committing; `CatalogSwitchFailed`: exact pending sequence plus browser
      rejection generation before pane failure presentation. These are
      selection outcomes, not committed-series publications.
    - `ProviderCatalog`: runtime consumer/provider/command callbacks are
      fenced before delivery; desktop matches provider and browser
      search/selection generations for results, previews and rejection.
      `CatalogReady` is provider-scoped startup; screen replies are ignored
      by a chart pane. `CatalogFailure` is an app-scoped sanitized notice and
      never changes chart state. `Diagnostics` only logs when enabled.
    - `PriceAlertTriggered`: alert-ID match against the pane's owned alert
      list before changing alert status/line or adding a bounded announcement;
      alerts survive symbol switches and are not chart-selection publications.
      `PriceAlertSyncFailed` and `PriceAlertSyncSucceeded` only post/clear a
      keyed pane notice for the consumer's alert subscription.
    - No remaining mailbox variant feeds selection-bound chart data without
      its exact displayed-generation or pending-selection acceptance. The
      consumer-scoped study/alert and catalog routes deliberately do not use
      a chart selection generation. This audit does not claim that the legacy
      switch guards are gone or that all interleavings have passed; those are
      Parts 25 and 26 respectively.
  - Part 25a: removed `stale_settled_state_during_engine_switch` and
    `ready_state_can_complete_switch`, which duplicated the controller's
    exact-generation lifecycle fence for Ready and AwaitingData. The GPUI
    pending-switch regression now delivers both old-series settled states in
    addition to Error, and checks that the exact pending selection survives.
    This is a verified guard-removal slice, not Part 25 closure: remove
    `RithmicSwitchState`, the marker wrapper and `rithmic_pending_*` fields
    next, then proceed to Part 26 permutations and seeded interleavings.
  - Part 25b1: selection commit and pre-marker rejection now rely on
    `ChartPaneController`'s exact pending generation, without a second legacy
    switch-flag acceptance check. The commit method is named for its selection
    outcome rather than a provider-specific marker. PaneHarness verifies that
    an exact pending outcome commits even if the obsolete flag disagrees.
    This does not close Part 25: remove `RithmicSwitchState`, the mailbox
    marker wrapper and `rithmic_pending_*` fields next.
  - Part 25b2a: removed both `rithmic_pending_*` fields. The pane controller
    now holds the pending interval and optional instrument alongside the exact
    request sequence; stale target registration is ignored, and rejection,
    replacement and restoration clear or consume the target with that sequence.
    Watchlist, status and chart selection read that one pending target. A pure
    regression checks retired target registration, replacement, acceptance and
    rejection. Part 25 stays open: remove `RithmicSwitchState` and
    `EngineSwitchMarker` next, then run the Part 26 interleavings.
  - Part 25b2b: removed the `RithmicSwitchState` phase and its
    `EngineSwitchMarker` mailbox variant. `ChartPaneController` alone tracks
    the committed generation, the generation displayed by the installed chart,
    and the exact pending request; the runtime now sends the explicit
    `EngineSelectionCommitted` outcome. A replacement chart only accepts
    incremental updates and retained tape after its covering snapshot is
    installed, while instrument changes remain covered and interval-only
    changes do not. The pane and controller tests verify these transitions,
    including a superseded chart through consecutive commits and failure
    restoration. This completes Part 25; M6.3 stays unchecked pending Part 26
    fixed permutations and seeded interleavings, Part 27 cover-through-ready,
    and Part 28 guarded isolated GUI storms.
  - Part 25c: repaired the published Part 25b2b commit's accidental dependency
    on uncommitted chart-chrome wiring. The committed terminal view again uses
    its existing drawing-toolbar field; the maintainer's alternate expression
    is restored only in the uncommitted worktree. The 17-file maintainer diff
    was rechecked after the repair; an isolated HEAD worktree passed the
    desktop all-targets/all-features check, separate from worktree gates.
  - Part 26: all 120 fixed orders of commit, old/new data and old/new lifecycle
    exercise the controller fence; six GPUI orders of commit and old/new
    snapshots assert the actual installed chart and replacement state after
    every event. Another 128 reproducible seeds run 256 mixed requests,
    commits, rejections, publications, lifecycle and study steps each, with
    seed and step printed on failure. No edge-case fix was required.
    M6.3 stays unchecked for Part 27 cover-through-ready and Part 28 guarded
    GUI symbol/interval/order-book storms on an isolated root.
  - Part 27: a failing-first GPUI instrument-switch regression covered both
    Ready-before-snapshot and snapshot-before-Ready ordering. The pane now
    retains the prior selection for the neutral instrument cover until the
    exact committed selection has both a covering snapshot and readiness.
    Ready alone cannot expose the superseded chart, and a snapshot alone
    cannot uncover history that the runtime has not declared ready. The prior
    selection is released when the second prerequisite arrives. M6.3 stays
    unchecked for Part 28's guarded isolated GUI switch storm.
  - Part 28a: a deterministic GPUI pane test issues 1m, 1h, 1D, 3D,
    5m, 3D before any reply, then delivers every superseded selection
    reply and snapshot late, followed by the final early snapshot, reply
    and current snapshot. The committed interval, displayed chart binding
    and interval label end on 3D; no controller defect reproduced from the
    first GUI attempt's blind fixed-coordinate clicks. Part 28b still
    requires literal guarded VAL-DSK-013/014/015 GUI evidence; M6.3 remains
    unchecked.
- [x] M6.4 pane-presentation - `PanePresentation` applies every user chart property in one place (code complete; literal VAL-DSK-017 GUI switch/restart not yet validated)
  - Part 1: saved appearance and order-flow settings that fail to parse are no longer
    dropped silently. The pane records the rejected fields in its retained presentation and
    posts the one keyed "Chart settings could not be restored" notice. That notice survives
    chart rebuilds until the trader edits each rejected setting. M6.4 stays unchecked:
    `PanePresentation` itself and the remaining restore setters (UI-11) follow.
  - Part 2: startup and covering-snapshot charts apply surface-owned time zone and
    order-line extension through one fallible settings operation, attempting both
    even if the first fails. The invalid-zone pane regression checks that the saved
    order-line choice survives both charts while one keyed notice is retained and
    clears on correction. M6.4 remains unchecked for the full presentation round trip.
  - Part 3: a GPUI pane regression changes chart style, swaps the existing
    bullish/bearish colors and selects logarithmic price scale, then switches
    instruments, serializes the resulting pane state, decodes it and reopens
    a chart. All three properties survive both boundaries. The complete
    `PanePresentation` ownership/field round trip and GUI clause remain open.
  - Part 4a: `PanePersistence` now writes the pane-owned time zone, three
    trading-visibility settings, context-panel state and chart-link state in
    one place for both loaded and pending charts. A failing-first GPUI test
    saved during loading, encoded/decoded protobuf, reopened and compared the
    loaded presentation; a separate GPUI test combines invalid time zone,
    appearance and order-flow fields into one sanitized keyed notice, which
    clears only after all three are corrected. Full `PanePresentation` apply
    ownership and every-field round trip remain before M6.4 can be checked.
  - Part 4b: `PanePresentation` now merges the saved chart document with the
    pane's latest settings, then applies startup and replacement chart settings
    at one boundary. The GPUI protobuf round trip covers style, appearance,
    scale, crosshair, legend, order-flow, indicator and study visibility,
    session time zone, trading visibility, context panel and linking; the
    switch regression and combined rejected-setting notice remain in place.
    M6.4 stays unchecked for the literal VAL-DSK-017 GUI switch/restart
    evidence: the maintainer owns a running release desktop, which must not
    be automated or interrupted. The current chart integration constructs
    a new `AerisChartView` on a covering snapshot; a drawing-preserving
    in-place cross-instrument reset remains an Aeris Charts operation for M7.9.
- [x] M6.5 select-and-demand-panes - One `select_and_demand` per selection; startup resolution leaves the desktop (code complete; VAL-DSK-020 guarded GUI restart not yet validated)
  - Parallel test isolation: the workspace persistence fixture used a PID
    and wall-clock timestamp as its temporary root, allowing concurrent
    tests in the same process to alias when the clock repeats. An atomic
    per-process sequence now distinguishes every root. A 16-thread,
    2,048-path regression and 20 concurrent default-parallel desktop test
    binary runs of the persistence module pass, including the previously
    intermittent template round trip. This is test-fixture isolation, not
    evidence for the remaining M6.5 GUI or restored-pane count clauses.
  - Part 1: the desktop bridge accepts only the exact outstanding initial or
    replacement selection reply. It consumes that reply once and rejects
    repeats and retired generations, so a repeated Selected cannot install
    the same pane identity twice. A GPUI pane test asserts one command per
    user interval selection, one applied reply and no follow-up selection.
    The restored-pane InstrumentRef command count, legacy selection API
    removal and literal four-pane GUI restart remain; M6.5 stays unchecked.
  - Part 2: startup now distinguishes a readable persisted workspace from
    a fresh default, even if both display BTC-USDC. Each saved pane carries
    its exact provider, instrument and entitlement as an `InstrumentRef`
    through the existing single initial selection call. Newly created
    explicit panes also use refs; only a genuinely fresh workspace asks for
    the runtime's ungated default. Protobuf reopen and four-pane identity
    regressions pass. An instrumented one-command/one-applied-reply test
    for all restored panes, legacy `try_select_engine` removal and the
    guarded four-pane GUI restart remain; M6.5 stays unchecked.
  - Part 3: renamed the remaining `try_select_engine` desktop API and
    callers to `try_select_instrument`, reflecting the runtime-owned
    select-and-demand boundary without changing dispatch or introducing
    a second selection command. The VAL-DSK-019 source search and runtime
    naming rule now pass. Instrumented restored-pane command/reply counts
    and literal VAL-DSK-020 four-pane GUI restart remain unverified;
    M6.5 stays unchecked.
  - Part 4: the four-pane restored-workspace endpoint regression captures
    the initial dispatch from each saved `InstrumentRef` through the same
    request construction used by production, checks four requests with
    exact consumer/generation/interval/identity, and feeds duplicate
    `Selected` outcomes to verify one reply per pane. The single-dispatch
    helper leaves the runtime as demand owner. This is endpoint evidence,
    not the literal four-pane GPUI PaneHarness or guarded GUI restart
    evidence required by VAL-DSK-018/020; M6.5 remains unchecked.
  - Part 5: a four-pane GPUI `PaneHarness` regression captures exactly four
    production endpoint startup dispatches with each saved `InstrumentRef`,
    consumer, generation and interval, opens distinct BTC/ETH/SOL/HYPE panes,
    accepts each initial
    `SelectedInstrument` reply exactly once and rejects altered duplicates
    without dispatching follow-up selections. The complementary endpoint
    regression in Part 4 counts four exact `InstrumentRef` requests and one
    applied worker reply per pane; the user-selection GPUI test counts one
    command with no follow-up. The new GPUI test exposed a pane-side
    duplicate-reply overwrite, now fenced as a one-shot initial descriptor.
    VAL-DSK-018/019 are covered by those tests and the source/naming rule
    checks. M6.5 code is complete; VAL-DSK-020 remains explicitly unvalidated
    because the maintainer owns the running release desktop (PID 40076).
- [ ] M6.6 bridge-message-collapse - Collapse the parallel desktop market message model
  - Part 5 route map (Part 24 fence audit; runtime events are delivered in a
    `Fenced<ConsumerTag, MarketRuntimeEvent>` envelope before the desktop
    translates them; names below describe the eventual pane projection, not
    permission to accept an unfenced worker message):
    - `Update` snapshot/tail <- `SeriesSnapshot`/`SeriesUpdate`: bar
      conversion is a GPUI projection, not a second canonical bar generation;
      accept the exact displayed selection generation.
    - `SeriesState` <- `SeriesState` (`Live`, `Ready`, `Resolving`,
      `Partial`, `Empty`, `Failed`), `DemandError` and endpoint failure:
      pane readiness projection; accept the displayed generation and reject
      old lifecycle outcomes during a pending replacement.
    - `OrderBook` <- `OrderBookSnapshot`; `TradeTape` <-
      `TradeTapeSnapshot`: convert depth for display or retain tape; accept
      the exact displayed generation and maintain per-selection revisions.
    - `StudyOutput` <- `StudyOutputSnapshot`; `StudyOutputsInvalidated` <-
      `StudyOutputsInvalidated`; `StudyRemoved` <- `StudyRemoved`: accept
      demand-bound generation, or consumer-scoped pre-selection study
      lifecycle when no generation exists.
    - `MarketSessionStatus` <- consumer instrument/session status;
      `Connection` <- `ProviderState`: accept exact displayed generation.
      `Recovery` <- covering `SeriesSnapshot`/`DemandError` for the pending
      recovery request: accept lifecycle generation plus exact request ID.
    - `ProviderCatalog` <- `CatalogReady`, instrument search result/preview,
      selection/rejection and market screen result/rejection: runtime fences
      consumer/provider/callback; browser checks its search/selection
      generation. `CatalogFailure` is an app-scoped sanitized failure notice.
    - `Diagnostics` is diagnostic-only (bounded FIFO, no chart mutation).
      `PriceAlertTriggered` <- `PriceAlertTriggered`, matched by owned
      alert ID; `PriceAlertSyncFailed`/`PriceAlertSyncSucceeded` are
      consumer-scoped keyed-notice outcomes, not series publications.
    - `StudyRegistered`/`StudyRegistrationFailed` are consumer-scoped
      registration replies matched by outstanding request sequence;
      `StudyReinitialized`/`StudyReinitializationFailed`/
      `StudyRemovalFailed` are consumer-scoped replies matched by study ID
      and pending operation.
    - `EngineSelectionCommitted`/`EngineSelectionFailed` <- the accepted
      select-and-demand result, matched to the exact pending sequence;
      `SelectedInstrument` is the one-shot initial descriptor gated by its
      selection generation. `CatalogSwitchCommitted`/
      `CatalogSwitchFailed` <- catalog selection result/rejection, matched
      by exact pending pane sequence and browser generation.
  - Part 5a: recovery now carries only a bounded presentation evidence
    record derived directly from the runtime-converted snapshot; the duplicate
    `MarketGeneration<ProvenancedMarketBar>` copy and its constructor are
    removed from desktop production code. The fixture uses its own
    test-only model to generate evidence. Empty covering snapshots still
    reject recovery. Chart data, readiness, and the remaining routes have
    not yet been moved to direct fenced pane projection; `ChartState` and
    `MarketWorkerMessage` remain, so VAL-DSK-021/022 are not claimed complete.
  - Part 5b: runtime `SeriesState` now retains its coordinator-issued
    `Fenced<ConsumerTag, MarketRuntimeEvent>` envelope in the bounded pane
    queue; the GPUI pane projects readiness only after exact lifecycle
    acceptance. The worker still handles failed series as a generation-stamped
    error; pending and retired readiness cannot change the chart. The pane
    regression exercises early, retired, awaiting-data and live outcomes.
    Legacy synthetic `SeriesState` messages, bar conversions, `ChartState`,
    and the remaining routes still need the ordered Part 5-8 collapse;
    VAL-DSK-021/022 remain unclaimed. The naming-check test fixture reader
    normalizes Windows CRLF for its LF-only source-literal assertions; this
    repaired two baseline test failures after the concurrent UI commits.
  - Part 5c: runtime bar snapshots and tail updates now keep their coordinator
    consumer envelope and emission order in the bounded pane queue. The GPUI
    pane rejects pending/retired selections before converting canonical bars
    into replay presentation; the bridge rejects wrong consumers and future
    generations. A bridge regression checks fenced snapshot/tail FIFO and
    retired rejection; a GPUI regression checks early, retired and covering
    snapshots plus the accepted tail. Fixture `Update`, the older worker
    conversion tests, synthetic `SeriesState`, `ChartState` and the other
    routes remain until Parts 5d-8; VAL-DSK-021/022 remain open.
  - Part 5d1: runtime `SeriesState::Failed` and `DemandError` now retain their
    coordinator consumer envelope through the bounded pane queue. The GPUI
    pane accepts only the current lifecycle generation before presenting
    failures, retryable recovery or an older-history diagnostic. Retired
    generations are dropped and future generations rejected at the bridge.
    Worker-created endpoint faults, legacy worker bar conversions and their
    tests, `ChartState`, and the synthetic fixture route remain for Part 5d2;
    M6.6 and VAL-DSK-021/022 remain open.
  - Part 5d2a1: the disconnected fixture now constructs canonical, valid
    series snapshots in a coordinator-fenced consumer event. PaneHarness's
    initial snapshot and readiness use the same runtime publication/status
    projection as production, and its fenced snapshot, tail, readiness,
    failure and demand-error examples share the test-support event builder.
    Migrated tests: fixture_market_publishes_disconnected_bounded_snapshots,
    pane_harness_smoke_disconnected_fixture_renders_pane,
    pane_harness_fenced_runtime_bars_project_only_after_selection_commit,
    pane_harness_fenced_runtime_readiness_projects_only_committed_selection,
    pane_harness_fenced_runtime_failure_restores_previous_binding and
    pane_harness_runtime_demand_error_projects_only_current_series.
    The remaining pane, mailbox and diagnostic fixtures are Parts 5d2a2-4;
    the old production route is removed only in 5d2b. M6.6 stays unchecked.
  - Part 5d2a2: migrated the remaining pane fixture's old bar `Update` and
    synthetic `SeriesState` messages to canonical coordinator-fenced runtime
    snapshots, tails, readiness, failures and demand errors. Migrated tests:
    pane_harness_retired_recovery_cannot_fail_the_replacement_chart,
    pane_harness_failed_committed_interval_restores_previous_chart_with_one_notice,
    pane_harness_failed_committed_instrument_keeps_original_chart_and_deduplicates_notice,
    pane_harness_bar_publications_reject_retired_selection,
    pane_harness_snapshot_order_commit_old_new,
    pane_harness_snapshot_order_commit_new_old,
    pane_harness_snapshot_order_old_commit_new,
    pane_harness_snapshot_order_old_new_commit,
    pane_harness_snapshot_order_new_commit_old,
    pane_harness_snapshot_order_new_old_commit,
    pane_harness_rapid_interval_storm_keeps_final_day3_binding,
    pane_harness_series_state_rejects_early_and_retired_selection,
    pane_harness_old_recovery_failure_during_pending_switch_keeps_request,
    pane_harness_old_series_error_during_pending_switch_does_not_cancel_selection,
    pane_harness_catalog_request_fences_old_series_before_catalog_reply,
    pane_harness_instrument_cover_waits_for_snapshot_after_ready,
    pane_harness_instrument_cover_waits_for_ready_after_snapshot,
    pane_harness_symbol_switch_retains_style_colors_and_scale_mode and
    pane_harness_chart_failure_renders_one_redacted_notice_then_snapshot_clears.
    The direct chart-reinstall setter fixture is unchanged; mailbox and
    diagnostic migrations remain Parts 5d2a3-4, before production deletion.
  - Part 5d2a3: migrated mailbox fixture bar snapshots, append/revise tails
    and synthetic readiness/failure states to canonical coordinator-fenced
    runtime events. Migrated tests:
    engine_background_transition_discards_stale_ui_market_publications,
    idempotent_foreground_assignment_preserves_initial_covering_snapshot,
    control_states_preserve_fifo_order_in_the_bounded_mailbox,
    bounded_bridge_drains_ten_thousand_outbox_events_in_emission_order,
    mailbox_wake_fires_once_per_edge_until_drained,
    live_tail_updates_share_one_frame_wake_until_the_mailbox_is_drained,
    queued_append_is_delivered_before_its_same_candle_revision,
    live_tails_for_distinct_bars_are_all_delivered,
    full_market_mailbox_returns_backpressure_without_mutating_queued_bars,
    mailbox_wake_fires_for_queued_messages_on_registration,
    mailbox_wake_stops_after_receiver_drops,
    diagnostics_keep_fifo_order_with_market_publications,
    terminal_control_preserves_a_queued_covering_snapshot,
    market_publication_waits_behind_a_full_control_queue,
    queued_covering_snapshot_is_unchanged_when_next_update_is_backpressured,
    control_messages_remain_fifo_while_market_publication_is_backpressured,
    terminal_and_failed_recovery_messages_survive_lower_priority_states,
    successful_recovery_and_newer_stream_update_preserve_fifo_order,
    series_state_survives_catalog_rejection_in_the_bounded_mailbox and
    mailbox_does_not_filter_publication_generations. The existing wake,
    recovery acknowledgement and legacy ChartState characterization tests
    remain; canonical bars now share market-publication background suppression
    and the bounded market capacity rather than the control reserve.
    Part 5d2a4 diagnostic fixture migration and 5d2b legacy route deletion
    remain; M6.6 and VAL-DSK-021/022 stay open.
  - Part 5d2a4: diagnostic burst and endurance mailbox producers now send
    canonical fenced coordinator snapshots built from validated fixture bars;
    their observers read publication generations from that envelope instead
    of the legacy replay `Update`. Migrated tests:
    burst_delivery_is_bounded_and_backpressured,
    short_endurance_preserves_continuity_and_bounds and
    interrupted_endurance_retains_an_atomic_incomplete_checkpoint. The
    separate replay-model ordering and history-gap characterization cases
    remain unchanged because they exercise the model, not the mailbox.
    Part 5d2a is complete. Part 5d2b must delete the old production chart
    route before M6.6 and VAL-DSK-021/022 can close.
  - Part 5d2b: deleted the duplicate `apply_pushed_event` bar/readiness/demand
    conversion and the old `Update`/synthetic `SeriesState` message and pane
    consumers. The market summary now converts coordinator-fenced canonical
    snapshots/tails under its exact initial selection; local failures carry
    `EndpointFailure` with an exact selection generation, rather than claiming
    to be coordinator readiness. `complete_pending_recovery` retains its
    covering snapshot, request-ID and selection fences. Migrated
    engine_market_worker characterization tests:
    native_market_runtime_hyperliquid_startup_resolves_stale_default_generation
    (still ignored without an optimized live runtime),
    retained_partial_history_and_live_repair_keep_fenced_readiness_order,
    empty_provider_history_keeps_its_fenced_readiness_reason,
    covering_snapshots_are_forwarded_to_the_chart_validator,
    non_contiguous_engine_tail_is_left_for_the_chart_validator,
    a_burst_larger_than_one_frame_reaches_the_chart_bridge_without_recovery,
    retained_snapshot_from_older_provider_session_reaches_the_chart_baseline,
    calendar_series_accepts_the_engine_live_handoff,
    replacement_engine_snapshots_are_forwarded_without_a_worker_generation_fence,
    retryable_and_permanent_demand_errors_retain_their_fenced_classification,
    older_history_demand_error_keeps_the_live_chart_and_its_pending_recovery,
    and hyperliquid_publication_uses_hyperliquid_worker_identity. New summary
    regression: market_summary_accepts_only_current_fenced_canonical_bars.
    pane_harness_fenced_runtime_readiness_projects_only_committed_selection
    additionally asserts that a post-Live partial backfill cannot re-cover a
    ready chart; pane_harness_endpoint_failure_is_selection_fenced verifies
    that only the current endpoint failure reaches chart presentation.
    The book/tape/study cases remain on their current route for Part 6.
    Part 5d2c still moves ChartState to pane presentation; M6.6 and
    VAL-DSK-021/022 remain open.
  - Part 5d2c: moved the chart's loading, readiness, connection and recovery
    presentation state out of the market-worker mailbox declaration and into
    `ChartPaneController`. The pane keeps the same selection-fenced projection
    and its labels and readiness-only transition gate have direct controller
    coverage. M6.6 and VAL-DSK-021/022 remain open for Parts 6-8.
  - Part 6a: the order-book snapshot keeps its coordinator consumer envelope
    through the bounded mailbox and is projected only after the pane's exact
    selection fence. Removed the old worker depth conversion, `OrderBook`
    message and its consumer in the same change. The projection stamps the
    accepted snapshot generation, not a potentially stale descriptor's
    selection generation. Migrated tests:
    rithmic_order_book_snapshot_uses_the_consumers_selection_identity,
    pane_harness_order_book_rejects_retired_selection_even_with_higher_revision,
    order_book_mailbox_preserves_regressing_runtime_order,
    order_book_mailbox_preserves_newer_frames_after_earlier_frames and
    order_book_mailbox_does_not_reinterpret_session_generations. Tape and
    study publications remain for Part 6b; M6.6 and VAL-DSK-021/022 stay open.
  - Part 6b: trade tape and study output/invalidation/removal now keep the
    coordinator consumer envelope through the bounded mailbox to the pane.
    The worker checks exact consumer and selection generation (while allowing
    generation-free consumer-scoped study lifecycle publications), and the
    pane retains its committed-selection and revision guards. Deleted the
    old tape/study conversion arms and mailbox variants in the same change.
    Tape and study-output publications retain the market capacity; study
    invalidations/removals retain the control reserve and are not discarded
    by background-market suppression. Migrated
    trade_tape_publication_is_consumer_and_generation_fenced,
    trade_tape_mailbox_preserves_emission_order_per_runtime_generation,
    pane_harness_retired_study_removal_cannot_delete_current_study and the
    order-flow tape switch regression; added
    study_publications_keep_the_consumer_envelope_and_selection_fence.
    Parts 7-8 and VAL-DSK-021/022 remain open.
  - Part 7a: price-alert triggers now retain the coordinator consumer envelope
    through the bounded pane queue. The worker rejects a mismatched consumer
    before native notification or mailbox delivery; the pane matches its owned
    alert ID before changing presentation. Deleted the old alert-trigger
    conversion and mailbox variant in the same change. The deterministic
    alert_publication_keeps_consumer_fence_and_rejects_wrong_identity test
    checks both rejection and exact-envelope delivery. Connection, session,
    recovery, catalog, screener, diagnostics and local alert-sync outcomes
    remain for Part 7; Part 8 and VAL-DSK-021/022 remain open.
  - Part 7b: provider connection states from the runtime retain their
    coordinator consumer epoch through the pane queue. The worker checks the
    selected provider and stamps its accepted pane generation, including a
    state held across pending selection adoption; the pane checks both
    provider identity and committed generation before projecting connection,
    RTT or failure. Deleted the superseded runtime-to-Connection conversion
    and pushed-event route. Locally generated startup connection controls
    remain separate. Bridge and GPUI regressions cover envelope preservation,
    provider mismatch, retired generation, RTT and pending adoption. Session,
    recovery, catalog, screener, diagnostics and alert-sync remain for Part 7;
    Part 8 and VAL-DSK-021/022 remain open.
  - Part 7c: catalog-installed verified terms no longer send an immediate
    session status ahead of their catalog reply. They invalidate the prior
    acknowledgement and use the existing clock-driven session-status publisher,
    which retains the exact selection generation and retries after bounded
    mailbox backpressure. The regression fills the reserve, verifies catalog
    ordering, checks a failed send does not acknowledge the status, then
    drains and receives exactly the new terms' status. The existing pane
    projection checks the exact committed generation and instrument identity;
    its legacy message variant is removed with the other local projections
    in Part 8. Recovery, catalog/screener, diagnostics and alert-sync routes
    remain in Part 7; M6.6 is unchecked.
  - Part 7d: a covering recovery snapshot now retains its coordinator consumer
    epoch through bounded delivery. The worker validates the exact pending
    request, consumer, active selection, provider session and nonempty covering
    bars before acknowledging it; the pane projects the canonical snapshot only
    after lifecycle-generation and provider acceptance, then installs it for the
    exact chart request. Local supersession and command failures stay explicit
    generation-stamped outcomes, not forged coordinator events. The superseded
    worker-built success bootstrap route is removed. Worker backpressure and
    request-fencing tests and a GPUI stale/current request regression cover the
    change. Catalog/screener, diagnostics and alert-sync still need Part 7;
    Part 8, VAL-DSK-021/022 and guarded GUI remain open.
  - Part 7e: removed the diagnostic snapshot message and pane log handler:
    no production code produced that message. Retained the bounded control
    reserve's FIFO, mixed-publication ordering and explicit overflow tests
    using the actual app-scoped catalog failure route. Catalog/screener and
    alert-sync still need Part 7; Part 8 and VAL-DSK-021/022 remain open.
  - Part 7f: local alert replacement acknowledgements now carry the sequence
    allocated only after bounded command acceptance. The pane applies only
    the latest accepted sequence, so delayed failures cannot post and delayed
    successes cannot clear a newer failure notice. Adjacent command coalescing
    retains the newest sequence and complete snapshot. The GPUI regression
    covers both stale directions. Full fmt, check, strict Clippy, workspace
    tests and naming gates pass. The first default-parallel workspace test
    run transiently failed trading_runtime's
    every_revision_changing_step_is_published at its 5-second placement
    subscription wait; the same test alone and the repeated full parallel
    workspace run passed without code changes. This intermittent,
    unrelated timing dependency needs owner investigation; it is not
    proof of deterministic test behavior. Catalog/screener and Part 8
    remain open; M6.6 and VAL-DSK-021/022 are not closed.
  - Part 7g: catalog search, selection and market-screen callbacks now remain
    coordinator-fenced through the bounded endpoint mailbox and are projected
    only by the pane or screener. The old worker-built ProviderCatalog variant
    and conversion route are deleted. The presentation-only catalog event
    shape moved from the worker into the desktop projection owner. Existing
    pane search, verified-terms and rejection regressions use canonical
    consumer events; the worker regression checks the exact retained consumer
    epoch and request sequence, and screen callbacks check request identity.
    App-scoped catalog failures remain explicit local controls; Part 8 must
    collapse the remaining message/mailbox model and prove VAL-DSK-021/022.
  - Parallel-suite publication timing follow-up: the trading owner's command
    reply precedes the end-of-turn publication. The revision-step fixture
    now uses the subsequent owner status command as a publication barrier,
    then reads the queued event without a wall-clock wait. Initial subscribe
    queues its snapshot synchronously. The production subscriber timeout and
    its dedicated deadline behavior are unchanged. The focused regression and
    three consecutive default-parallel workspace suites passed (trading:
    301 passed, seven ignored each); fmt, workspace check, strict Clippy and
    naming check passed.
  - Before Part 7b, parallel-suite timing follow-up: the study calculation
    worker now publishes its completion before replying, so an immediately
    following calculation cannot mistake an already-finished worker for a
    timed-out one. The ranged-history and independent-failure regressions
    assert the completed worker is reusable; the dedicated slow-calculation
    test still enables the production 50 ms deadline. Three trading fixtures
    (working exposure, news restriction and OCO cancellation) now join their
    owner on its explicit shutdown completion rather than racing a 2-second
    wall-clock deadline under parallel scheduling. A separate blocked-owner
    test proves the real production shutdown deadline still expires. Focused
    study (40 passed, three ignored) and four trading tests passed; fmt,
    workspace check, strict Clippy and three consecutive default-parallel
    full workspace test runs passed (trading: 301 passed, seven ignored in
    each). M6.6 and VAL-DSK-021/022 remain open.
  - Before Part 6b, the two parallel-suite timing flakes from the M5 probe
    follow-up were repaired at their test owners: ordinary study fixture
    calculations await the isolated worker's reply without a wall-clock
    deadline or shared turn budget; the dedicated deadline test explicitly
    enables the unchanged production 50 ms deadline. A parallel gate also
    exposed this scheduling race in the subtree checkpoint test, now covered
    by the same fixture policy. The offline-lease
    test installs the cached view before the startup fetch instead of racing
    that fetch with an insufficient idle-poll clock rewind. Evidence: both
    focused tests passed six successive runs each, the default-parallel
    market-runtime library suite passed three successive runs (374 passed,
    four ignored each), and the default-parallel full workspace suite passed
    twice after the fix; fmt, workspace check, strict Clippy and naming check
    also passed. The first pre-fix gate on this session exposed the same race
    in `subtree_checkpoint_restores_state_outputs_without_rewinding_generation`,
    which the shared test fixture policy resolved. M6.6 remains open.
  - Part 1: the bounded presentation mailbox no longer scans and replaces
    already-queued study, order-book or trade-tape publications. Those
    generation-bearing events remain in emission order across control messages;
    a 10,000-publication fake-outbox burst asserts the per-frame drain bound,
    exact order, and explicit `Full` backpressure without silent loss. The
    remaining diagnostic coalescing, parallel message/ChartState model,
    recovery panic and bridge collapse belong to later M6.6 parts; VAL-DSK-021
    and 022 are not yet claimed complete.
  - Part 2: diagnostic snapshots now enter the same bounded FIFO mailbox,
    using its control reserve. Enqueue no longer scans or replaces queued
    diagnostics, and a full reserve reports `Full` rather than silently
    discarding a snapshot. Tests cover diagnostic generation order, the
    explicit saturation result, and interleaving with chart publications.
    The parallel message/ChartState model, recovery panic and bridge discards
    remain; VAL-DSK-021/022 are not yet complete.
  - Part 3: recovery completion, cancellation and replacement retain the
    pending request until their acknowledgement enters the bounded mailbox;
    demand failures reply using the exact request without a production
    `expect`. A saturation regression checks both cancellation and
    runtime-failure replies under explicit backpressure and successful retry.
    Parallel market messages, ChartState/MarketGeneration, and the remaining
    bridge discards still require collapse; VAL-DSK-021/022 remain open.
  - Part 4: market session status revisions are recorded only after the
    bounded pane mailbox accepts the status. On backpressure the next worker
    iteration retries without waiting for the minute tick; a saturation
    regression verifies the retry and prevents a duplicate after delivery.
    Parallel market messages, ChartState/MarketGeneration and other bridge
    discards remain; VAL-DSK-021/022 remain open.
  - Status (2026-10-23 audit): series bars and status reach panes as fenced
    `MarketConsumerEvent`s; `MarketWorkerMessage` now carries
    `RuntimeSeriesStatus`, `RuntimeSeriesPublication` and `EndpointFailure`
    where the Part 5 route map names `Update` and `SeriesState`. Done: the
    enqueue scan (MD-16), the `publications.rs` expect and the duplicate
    `ChartState` (now in `chart_pane_controller`). Remaining: the order book,
    tape, study, connection and session routes still use the parallel desktop
    model, and `MarketWorkerMessage` is not deleted.
  - Part 8a (2026-10-24): the worker's bespoke FIFO/wake implementation moves
    to one typed, bounded per-pane projection queue. Runtime envelopes and
    local presentation outcomes still share FIFO order, a bounded control
    reserve, explicit `Full` backpressure, suppression policy and frame-budget
    wake continuation. The compatibility `MarketWorkerMessage` payload and
    `MarketWorkerSender`/`Receiver` names remain for the next Part 8 step;
    this is not a rename-based claim of VAL-DSK-021. The new queue has its own
    10,000-event budget/order/overflow regression; existing bridge and pane
    coverage still runs. M6.6 and VAL-DSK-021/022 remain open until the
    parallel message model is removed and the literal checks pass.
  - Part 8b1 (2026-10-24): six duplicate payload variants for coordinator
    series status, depth, tape, study, catalog and alerts are replaced by one
    `Runtime(MarketConsumerEvent)` queue entry. The pane dispatches by the
    coordinator event type, retaining its consumer envelope and the existing
    selection/operation fences; the screener projects only catalog events.
    The bounded queue classifies publication capacity by canonical event
    type instead of desktop variant names, with a regression for tape
    backpressure, readiness/catalog control reserve and background
    suppression. Bars, provider state and recovery still carry their
    additional presentation metadata; local outcomes and transitional
    `MarketWorkerMessage` names remain for the next Part 8b step.
    M6.6 and VAL-DSK-021/022 remain open.
  - Part 8b2a (2026-10-24): local select-and-demand success and failure
    acknowledgements share one sequence-fenced outcome instead of two
    independently routed message variants. The pane commits or rejects only
    the matching pending sequence; existing duplicate-reply, stale-reply,
    failed-switch, queue-order and rapid-switch regressions exercise both
    branches. This is a partial local-outcome collapse, not the final bridge
    migration: metadata-bearing fenced runtime entries, other local outcomes,
    `MarketWorkerMessage`, transitional channel aliases and `fn enqueue`
    remain. M6.6 and VAL-DSK-021/022 stay open.
  - Part 8b2b (2026-10-24): catalog-switch success and failure now share
    one local outcome, retaining the exact pane sequence and browser generation.
    The pane projects successful descriptors or a sanitized failure only after
    the same existing pending-request checks. The catalog pending-switch and
    redacted-notice GPUI regressions cover both branches; the remaining
    metadata-bearing runtime variants, other local outcomes and transitional
    message names still prevent VAL-DSK-021/022 closure.
  - Part 8b2c (2026-10-24): study registration success and failure share one
    sequence-fenced local outcome. The pane projects either branch only when
    its pending study request still exists; retired success and failure cannot
    consume the current pending study or add an active study. The redacted
    failure/success GPUI regression exercises both branches and late outcomes.
    Metadata-bearing runtime variants, remaining local outcomes, transitional
    message/channel names and `fn enqueue` still prevent VAL-DSK-021/022 closure.
  - Part 8b2d (2026-10-24): study reinitialization success and failure share
    one local result route. The pane retains the previous durable settings on
    failure, clears pending state, and applies a successful retried replacement
    through its existing study owner; runtime error detail never enters a pane
    notice. The GPUI fixture exercises both results in order. Provider/bar/
    recovery metadata-bearing runtime routes, other local outcomes, the
    transitional message/channel names and `fn enqueue` still remain;
    M6.6 and VAL-DSK-021/022 stay open.
  - Part 8b3a (2026-10-24): recovery covering snapshots and ordinary series
    publications share the canonical fenced snapshot/update queue entry. The
    recovery projection carries its exact pending request ID alongside received
    time, while ordinary bars carry no recovery ID; the pane keeps separate
    request/lifecycle acceptance and ordinary publication acceptance. The
    recovery request and publication FIFO/backpressure regressions cover both
    projections. Provider-state metadata, the remaining local outcomes and
    transitional message/channel names still block M6.6 and VAL-DSK-021/022.
  - Part 8b3b (2026-10-24): local connection and market-session statuses share
    one generation-stamped session projection instead of separate mailbox
    variants. The unused worker connection detail string is no longer carried
    to the pane; exact committed-selection acceptance, transport RTT, calendar
    retry on backpressure and notice behavior stay at their existing owners.
    A mixed FIFO/selection-stamp queue regression and the existing pane and
    worker tests cover both outcomes. Provider-state metadata, other local
    outcomes and transitional names remain; M6.6 and VAL-DSK-021/022 stay open.
  - Part 8b3c (2026-10-24): provider-state delivery uses the selection-bound
    fenced runtime projection, rather than a provider-specific mailbox arm.
    The worker captures its endpoint generation; the pane accepts only that
    stamp and the current provider, and rejects an unstamped provider event.
    Provider state remains control-reserved and FIFO with canonical bars.
    Other local outcomes, transitional names, and the literal VAL-DSK-021/022
    checks remain; M6.6 stays unchecked.
  - Part 8b3d (2026-10-24): locally detected catalog and endpoint failures
    share one typed failure projection. App-scoped catalog failures remain
    independent of chart selection, while endpoint failures retain the exact
    selection generation and cannot fail a retired or pending chart. The
    bounded queue preserves both scopes in FIFO order, and pane tests retain
    sanitized catalog notices and selection fencing. Recovery, alert and
    study-removal outcomes, transitional names, and literal VAL-DSK-021/022
    checks remain; M6.6 stays unchecked.
  - Part 8b3e (2026-10-24): study registration, reinitialization, removal
    failure and price-alert sync now share one bounded local command outcome
    entry, rather than four parallel mailbox variants. Each still reaches its
    pane owner with the original request sequence or study identity; failed
    removal no longer queues unused provider error text. A mixed-command FIFO
    test and pane regressions cover retired alert replies, study retries,
    registration and sanitized removal notices. Recovery and selection
    outcomes, transitional names and literal VAL-DSK-021/022 checks remain;
    M6.6 stays unchecked.
- Part 8b3g (2026-10-24): initial selection terms now use the same
  bounded local command outcome projection as recovery and other local
  replies, rather than a separate mailbox arm. The pane still accepts only
  the exact pending initial generation; duplicate and retired descriptors
  remain rejected. A mixed FIFO/control-reserve test and the restored
  four-pane/retired-descriptor GPUI regressions cover the route. Engine and
  catalog selection outcomes, metadata-bearing runtime entries and the
  transitional message/channel names remain; M6.6 and VAL-DSK-021/022 stay open.
- Part 8b3h (2026-10-24): catalog switch success and failure now use the
  bounded local command outcome projection, not a separate mailbox arm.
  Pane acceptance still requires the exact pending request sequence and
  browser generation; failed selection notices remain sanitized. The mixed
  FIFO/control-reserve test includes a catalog reply, and the pending
  lifecycle and retry GPUI regressions cover rejection and success.
  Engine selection, metadata-bearing runtime entries and transitional
  message/channel names remain; M6.6 and VAL-DSK-021/022 stay open.
- Part 8b3i (2026-10-24): engine select-and-demand acknowledgements now use
  the bounded local command outcome projection rather than a separate
  mailbox arm. The pane still commits or rejects only the exact pending
  sequence; duplicate and retired replies cannot change the displayed
  binding. The mixed-outcome FIFO/control-reserve fixture includes a failed
  engine reply; existing GPUI interval storm, selection failure and
  duplicate-reply regressions cover acceptance. Metadata-bearing fenced
  runtime entries and transitional message/channel names remain; M6.6
  and VAL-DSK-021/022 stay open.
- [ ] M6.7 descriptor-driven-identity - Render runtime descriptors; remove fake CLI modes; provider-neutrality rule
  - Status (2026-10-23 audit): `default_rithmic_contract_index` is gone. Still present:
    `--rithmic` and `--multi-chart` (`desktop.rs`, also used by
    `tools/test_window_controls.ps1`), `replay_conversion`, `worker_identity` and
    `default_product`; the `desktop_source_is_provider_neutral` rule does not exist yet.
- [ ] M6.8 typed-provider-and-risk-events - Typed provider and risk events; default workspace without an instrument
  - Status (2026-10-23 audit): done: the Rithmic catalog autoload check (the desktop handles
    `CatalogReady`), the display-text risk check (the desktop reads the typed
    `OrderEventReason::RiskWarning`) and the default workspace (`InstrumentRequest::Default`).
    `TradingEvent` is now a struct carrying the working set, so a risk warning stays an order
    event reason rather than a `TradingEvent` variant; it renders only a chart annotation, not
    a Warning notice. Remaining: `TerminalProvider`/`FALLBACK_TERMINAL_PROVIDER`, the
    provider-mapping naming_check rule and `desktop_control_flow_does_not_match_display_text`.
- [ ] M6.9 descriptor-login-and-symbol-browser - Descriptor-driven provider connect, login form and symbol browser
  - Status (2026-10-23 audit): `ConnectRithmic` is still the connect action; the login form is
    `components/rithmic_login.rs` and the browser lives in `rithmic_shell.rs`.
- [ ] M6.10 trading-store - One workspace `TradingStore` fed by the change stream
  - Status (2026-10-23 audit): `latest_feedback` is replaced by the `DesktopServices` feedback
    handle and `announce_fills` by snapshot diffing in `components/toaster.rs`
    (`announce_trading`). Remaining: `TradingPnlState`, `keep_trading_pnl_fresh` and
    `schedule_trading_refresh` (250 ms) and `service.snapshot()`; the desktop does not call
    `TradingService::subscribe` yet.
- [ ] M6.11 trading-intents-and-snapshot-removal - Revisioned chart intents and scoped emergency buttons; remove `TradingSnapshot`
  - Status (2026-10-23 audit): chart intents already carry `expected_revision`. Remaining: the
    desktop does not handle `StaleRevision`, flatten still needs a current bid and ask from the
    pane, and `TradingSnapshot`, `place_order_from_market`/`place_bracket_from_market` and
    `MAXIMUM_SNAPSHOT_ITEMS` remain.
- [ ] M6.12 discard-policy-enforcement - Zero unhandled discards in desktop production code
  - Status (2026-10-23 audit): every remaining `let _ =` in `apps/desktop/src` is test code or a
    test-fixture `Drop`, and `update.rs` has none. Remaining: the `let_underscore_must_use`
    and `let_underscore_future` deny lints.

### M7 desktop structure

Status: pending

- [ ] M7.1 design-tokens-for-chart-colors - Remaining chart and picker colors become design tokens
- [ ] M7.2 account-gate-isolation - Dormant account integration isolated behind `AccountGate`
- [ ] M7.3 chart-host-events - `AerisChartView` emits host events; split its host state
- [ ] M7.4 pane-controllers-split - `WorkspaceSurface` becomes composition and render over pane controllers
  - Status (2026-10-23 audit): `PanePersistence` and `PanePresentation` already exist (M6.4).
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
  - Status (2026-10-23 audit): the trading store already takes a preflight copy at open
    (`store/schema.rs`), but readiness still opens the real store through `start_trading_service()`.
- [ ] M8.6 release-wire-protocol-and-residue - Release wire protocol v1, uninstall safety and Axiusflow residue removal
- [ ] M8.7 release-packager-e2e - Release producer tool and end-to-end lifecycle harness (no publishing)
- [ ] M8.8 rollback-safety - Schema-gated rollback, startup-complete probation, deterministic recovery
- [ ] M8.9 dependency-direction-and-contracts - Fix dependency direction and reduce `contracts` to the persisted schema
- [ ] M8.10 dependency-hygiene - Remove reqwest stack, unused crates and duplicate pins; small context_runtime fixes
  - The spec's "keep the unused Rithmic `order_plant` out of the default build" part no longer
    applies: the order plant carries broker orders through `market_runtime::rithmic_venue`.
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
  pull-request path; `tools/naming_check` enforces the shape (M1.2, 63a71463). A workflow that uses a
  self-hosted runner stays `workflow_dispatch`-only in every trigger form, and live, credentialed and
  performance lanes stay `workflow_dispatch`-only. The manual `macos_preview.yml` tester build is the one
  hosted lane that may receive the Rithmic kit, from its only secret, and only when its `rithmic_kit`
  input is set.
- Desktop production code stores `ChartInterval` and never constructs `SeriesCadence`; the sole mapping belongs
  to `market_runtime::cadence` and is enforced by `desktop_never_constructs_series_cadence` (M1.3, ab5ff341).
- `platform_sys` is the only new unsafe boundary. Only its platform-specific source files
  (`src/windows/`, `src/macos/`) may add unsafe code, each block with a single-operation safety
  justification; the remaining pre-existing `platform_runtime` FFI files stay on an exact interim
  allowlist until M8.3 (M1.5, 3c42981b). `platform_sys` is a leaf crate: only `platform_runtime`,
  `trading_runtime` and `observability` may depend on it.
- Durable file publication goes through `platform_sys::fs` (a unique temporary in the destination directory,
  content sync before replacement, parent-directory sync on Unix); production raw `fs::write` and `fs::rename`
  calls are rejected outside the exact lifecycle/build allowlist (M1.6, 3c42981b).
- Secret-bearing types use reviewed redacting wrappers and cannot derive or use debug formatting in production
  (M1.7, 3c42981b).
- Each data root has one process owner. Validation roots and vault namespaces are isolated
  (M1.8, 3c42981b).
- The four market coordinator characterization cases and exact test names remain in the boundary
  test file, checked by `aeris_naming_check` (M2.1, 20abd4cc).
- Coordinator-owned watchdog, history retry and trade-handoff timestamps come from
  the injected clock; only `SystemClock` reads system time (M2.3, b2b66d8d).
- Provider reconnect attempts use capped policy backoff; Rithmic transport has
  only the adapter scheduler and a separate bounded non-transport delay
  (M2.4, a00725b0).
- Session/cadence resolution uses one provider-neutral calendar and one exchange-code table; generic calendar code cannot contain provider literals or import adapter calendars, and unsupported weekly/monthly live demand reports a typed error (M2.5, 0eae9355).
- Every live model's stall policy and every provider's practice-session check use calendar phases. Unknown sessions use a conservative 300-second stall threshold; closed sessions do not recover from silence (M2.6, 78085525). A trade-built series counts a newer quote on its instrument as progress, and its open-market threshold doubles after each stall recovery no trade followed, at most to eight minutes (2026-10-22 Rithmic stall-storm fix).
- Dated provider calendar events are fenced at the coordinator and expire into the instrument's recurring calendar or Unknown; live session, week and month bars and tape dates use the same calendar (M2.7, ecc749ab).
- Typed provider events and history completions use registry-minted epochs and an equality fence; naming_check rejects a raw `ProviderEvent.generation` (M2.8, 09e43c35).
- Catalog callbacks use coordinator-owned command fences by consumer/provider/kind; startup resolution has an explicit lane and consumer retirement uses one registry cleanup path (M2.9, 2fd43740).
- `HistoryTracker` owns current/backfill retry, deferral, empty coverage, detached windows and cancellation behind exact provider/demand/sequence tickets; no parallel coordinator history maps (M2.10, 1303adbd).
- Candle series control and forming state belong to `lifecycle::SeriesLifecycles`; candle handlers and direct clocks in `lifecycle/` are forbidden (M3.1, 0c4166cb).
- Trade-built series share that lifecycle owner; `SeriesLiveMap` cannot return to market-service production source (M3.2, 8c61798d).
- Generic market-service lifecycle/history/calendar/quote/outbox/coordinator/realtime/runtime/publication source contains no provider-id literals, including after test modules; provider replay aggregation does not live on `ChartInterval` (M3.5, 2f3fd729).
- Provider catalog command/event traffic crosses one bounded `ProviderCatalogChannel`; provider-specific wire commands are translated within its registered channel (M3.6, 13837b86).
- The market replay scenario names remain in `market_service/replay/mod.rs` and are checked by `aeris_naming_check` (M3.11, 0e77fa3c).
- Market probe fault injection is compiled only under the `probe` feature and interrupts the Hyperliquid WebSocket without affecting catalog HTTP; the probe CLI requires an isolated locked validation root before invoking it (M3.12, 6d559cd7).
- Trading owner-produced timestamps use the injected `trading_runtime::clock::Clock`, with wall time clamped monotonically per process; only its `SystemClock` reads system time. The trait is an explicit naming_check boundary (M4.1, 6b248b2f).
- One named trading owner thread runs the loop in `service/owner.rs` and drives `TradingCore` command, market and deadline entry points; the bounded command enum resides in `service/mod.rs` (M4.2, 98262ee5).
- `book::OrderBook` alone mutates canonical orders and maintains client, broker, open-scope and executable indexes (M4.4, f34adbc0).
- Store preflight scratch sweeping removes only exact-name directories containing expected database and sidecar copies; unknown contents remain and cleanup failures are logged (M4.6, 836c9573).
- V20 persists only verified trading market routes; migrated legacy instruments remain route-less until the market owner publishes verified Terms. The owner bounds ticket interest at 64 and combined quote interest at 256 (M4.7 part 2/3, 5c38b21e).
- V21 reads and writes only `fill_accounting` and `trade_cycles` for P&L; legacy cycle tables and fill columns are preserved, unread, for deferred removal (M4.9).
- The trading schema runs V1–V28 (full text in the repo `AGENTS.md`): V22 lifecycle columns; V23 `order_groups` and `order_group_legs`; V24 drops `managed_brackets` and `protective_orders` only through the gated `finalize_group_migration` step, so a failed conversion keeps its rows and retries; V25 cash ledger, fee schedules, exact cost basis and instrument class/grid columns, backfilled once by `complete_post_group_migrations` (a simulated balance is the exact sum of its ledger entries); V26 position and ledger checkpoints and `equity_samples` (one sample per simulated fill, timer samples only on change and at each risk boundary, a 1,440-entry ring per account, minute samples older than seven days folded to hourly and hourly rows older than a year dropped, both windows trailing the newest sample); V27 `risk_sessions` and `risk_locks.kind` (the owner timer rolls each session at its boundary in one transaction, releasing only daily-meter locks); V28 netted broker accounts (broker fills share the `positions` ledger, `broker_positions` and `broker_deals.broker_position_id` dropped, `broker_bracket_levels` added, no cash-ledger entries for broker accounts, a venue declares `VenueCapabilities` when attached).
- Desktop source may not reference `SimulatedMarketObservation`; quote execution and current quote provenance are owned by the trading QuoteBook fed from the market runtime (M4.7 part 3/3, cd54c087).
- Simulated order admission and fills require the trading owner's verified session, Live current-generation quote and class-specific freshness; a restart-time owner-local `eligible_after` fences restored orders without comparing provider generations across processes (M4.8).
- Broker order commands reserve bounded venue request capacity before their SQLite commit; the request becomes visible to the relay only after commit, and queue rejection causes no durable change or compensating undo (M4.10 part 2/4, ce69e2bb).
- Simulated mark-only quotes update unrealized P&L only in memory without cloning full trading state or writing SQLite; retention runs only from the owner timer at startup and hourly, writes position and ledger checkpoints in the same transaction before pruning, never deletes rows referenced by an open order or a live order group, and deletes at most 5,000 rows per table per run (M4.10 part 4/4, 658a73bd; checkpoints M5.8).
- Simulated fill quantity and price policies live only in `trading_runtime::execution`; the approved `TouchLiquidity` and `TouchPrice` traits/implementations retain full order quantity and exact touch (M5.3 part 11, a0ade75f).
- Broker orders reach Rithmic only through `market_runtime::rithmic_venue`, attached once by `MarketService::attach_broker_venue`; it routes only Rithmic Test and Rithmic Paper Trading logins, refuses a live login before any order-plant login, and attaches a new venue session generation with Rithmic's capabilities per connection (2026-10-22 Rithmic venue relay).

Rules in force in the repo `AGENTS.md` that earlier entries above do not state (recorded during the
2026-10-23 audit; `AGENTS.md` is authoritative):

- Quote and series interests share `MarketEngine`'s one bounded `DataLeaseRegistry`;
  `aeris_trading::market` depends only on `aeris_instruments`, and `market_engine` never depends on
  `aeris_trading` or a provider adapter (M3.7).
- `trading_runtime` is the single owner of broker accounts, orders, fills, positions, P&L, simulated
  execution and the user-record store; the desktop holds command/view handles only.
- Recurring contract-hour projection lives in `aeris_instruments::calendar::weekly_hours`; a historical
  DAY deadline comes only from a verified route and the acceptance-time open session, and unresolved
  legacy rows stay non-executable.
- Desktop trading handles, trading feedback, instruction and alert clocks, the account owner, the
  market owner, and workspace and chart-chrome persistence paths and save receipts all come from the
  instance's `DesktopServices`; none of their process-global lookups may return (M6.1).
- `FixtureMarketWorker` lives only under `apps/desktop/src/desktop/test_support/`, compiled only for
  tests or the `diagnostics` feature; the diagnostic conformance command is gated likewise (M6.1).
- Trading consumers subscribe through `TradingService::subscribe`: a typed `TradingEvent` with the
  bounded `WorkingSet` after every revision-changing step, quote-only marks coalesced to one
  publication per 100 ms with a trailing flush, at most eight subscriptions with a 1,024-event slot that
  overflows into a `lagged` resync, and paged history on the `aeris-trading-history` thread with
  500-row keyset pages (M5.11).
- `tools/naming_check` strips only balanced `#[cfg(test)]` module bodies, and every allowlist entry
  needs a written reason; stale entries fail.
- The trading owner's quote-interest revision is durable store metadata, and a provider reconnect
  republishes trading sessions before its `Feed::Live` (M5.12).
- `paper_trading_probe` runs the practice trading lifecycle on the public Hyperliquid feed from
  `tools/aeris_probes` (M5.12).
- Every Hyperliquid info request spends from the adapter's one process-wide weight budget
  (`hyperliquid_market/src/budget.rs`, 1,000 of 1,200 per minute) (2026-10-22 Hyperliquid 429 fix).
- Provider-specific capabilities and presentation metadata are declared with provider registration,
  and provider wire aggregation belongs to its adapter (M3.5).
- Every runtime-to-consumer event leaves the coordinator in a `Fenced<ConsumerTag, MarketRuntimeEvent>`
  envelope, and selection-bound events are rejected against the exact current demand (M3.9, 8bbfac6a).
- Every catalog search and selection carries a 45-second response deadline, and a catalog rejection
  names its search or selection lane explicitly.
- Chart selections, including startup and restored ones, resolve through the coordinator's
  `select_and_demand`; the desktop never owns startup catalog resolution (M3.10).
- When a trade-built series' covering history exhausts its retries, the runtime opens a live-only
  series with the `HistoryUnavailable` presentation and keeps one slow probe
  (`HISTORY_UNAVAILABLE_RETRY_DELAY`); an exhausted backfill lane keeps its cooldown until fresh demand
  (2026-10-22 live-only history fix).
- The popover layer (`--surface-popover`, `--border-popover`, `--border-popover-highlight`) is
  Aeris-only, declared solely in `:root:not([data-theme])` / `.dark:not([data-theme])`; the
  design-system build rejects it elsewhere and gives presets its CSS fallbacks (2026-10-23 theme update).
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
- 2026-10-13 M1.8 fix(validation): report only real broker venue attachments - corrected attachment diagnostics and covered root overlap and exact-key vault read/write isolation.
- 2026-10-13 M1.2 fix(ci): correct live gate runner guidance - removed the obsolete hosted-runner ban from the live gate comment without changing workflow behavior.
- 2026-10-13 M1.8 fix(validation): keep unconfigured broker startup diagnostics silent - suppressed the disconnected catalog warm-up line and bounded the isolated diagnostic test; full gate and isolated desktop/panic checks passed.
- 2026-10-13 M2.1 test(market): characterize coordinator market handoffs - pinned four command/event scenarios and registered their exact names; backfilled the M1 validated/pushed state and recorded the disabled hosted CI workflow.
- 2026-10-13 M2.2 perf(market): record engine and coordinator baselines - measured release workloads and added a manual-only CI performance job.
- 2026-10-13 M2.3 feat(market): inject coordinator clock into recovery timing - shared a monotonic/wall clock with handoffs, watchdog and history retries; scripted tests advance without sleeps.
- 2026-10-13 M2.4 feat(market): bound provider reconnect policies - replaced fixed Hyperliquid retry, budgeted the former FX/CFD broker with real-cause failure, jittered the former Level 1 broker and bounded Rithmic non-transport recovery.
- 2026-10-13 M2.5 feat(market): centralize calendar sources and exchange rules - shared CME calendar and venue table, resolved dated/weekly/continuous sessions and rejected unsupported weekly/monthly live demand explicitly.
- 2026-10-13 M2.6 feat(market): recover stalled live series by calendar phase - applied the same injected-clock watchdog to candle and trade models and routed practice-session checks through the calendar.
- 2026-10-13 M2.7 feat(market): roll live bars at calendar sessions - replaced fixed session durations and CME-only live buckets, integrated fenced dated provider holidays and calendar tape dates, and tested six non-CME week/month paths.
- 2026-10-14 M2.8 feat(market): fence provider events and history with typed epochs - registry-stamped adapter generations, fenced dated calendar and history completions, counted retired inputs and pinned typed event fields.
- 2026-10-14 M2.9 feat(market): unify catalog command fences and consumer retirement - removed the startup sentinel and five pending maps, separated startup/picker searches, and centralized consumer cleanup.
- 2026-10-14 M2.10 feat(market): centralize history lanes and exact completion tickets - replaced eight maps, bounded retry/deferral and provider-history scheduler with a single tracker; verified older Hyperliquid chart pages without gaps.
- 2026-10-14 M2.10 fix(market): drain deferred backfill after empty current history - closed the M2 scrutiny blocker with ticket-fenced dispatch and scripted completion-path regressions.
- 2026-10-14 M2.1/M2.5/M2.6 test(market): close user-testing boundary and calendar coverage gaps - moved characterization observations to the command/event boundary; completed the calendar/status matrix and five scripted recovery request checks; full gate green.
- 2026-10-14 M2.1 test(market): drive characterization through coordinator channels - removed direct clock assignment and benchmark turn calls; verified four boundary scenarios and the full gate.
- 2026-10-14 M3.1 feat(market): centralize candle series lifecycle - migrated both candle providers to the lifecycle owner, retained four coordinator scenarios, and verified live Hyperliquid and raw FX/CFD broker provider omissions against the pre-M3 binary.
- 2026-10-14 M3.2 feat(market): centralize trade series lifecycle - unified candle/trade control and publication, cached candle wire identity, fenced scripted SequenceGap reseed and one-forming continuity; full workspace gate green.
- 2026-10-14 M3.3 perf(market): share segmented series for study and live publication - replaced full-series study/live/existence copies, unified study dispatch and measured 128-byte tail payload at all three history sizes; full gate green.
- 2026-10-14 M3.4 perf(market): sleep between dirty market publications and deadlines - removed idle coordinator sweeps, preserved deadline recovery and measured lower isolated one-chart desktop CPU against archived M2.2 source; full gate green.
- 2026-10-14 M3.5 refactor(market): separate provider capabilities from presentation - removed descriptor behavior flags and generic provider-id literals, corrected Rithmic presentation, relocated replay aggregation to its adapter, and passed the full workspace gate.
- 2026-10-15 M3.6 refactor(market): release Level 1 broker state lock during HTTP (M3.6 part 1/3) - isolated the exclusive transport from its bounded catalog cache and fenced authorization revocation; full gate green, registry work follows.
- 2026-10-15 M3.6 refactor(market): bind FX/CFD broker market to the provider registry (M3.6 part 5/8) - removed worker-owned market epoch/retry, preserved the shared trading relay and verified demo symbol/interval switches on one session; full gate green, remaining providers follow.
- 2026-10-15 M3.6 fix(market): scope idle-stop overlap to its provider (M3.6 part 2/3) - per-provider demand prevents an unrelated order book from reopening a stopped session; full gate green.
- 2026-10-15 M3.6 refactor(market): unify bounded catalog dispatch (M3.6 part 3/5) - deleted four-arm catalog channel/dispatch enums, preserved provider-specific screen and overflow behavior; full gate green.
- 2026-10-15 M3.6 refactor(market): migrate Hyperliquid to epoch-bound provider session (M3.6 part 4/8) - reserved connection epochs in the registry, transferred realtime retry ownership to the coordinator and kept the three other providers on their existing paths.
- 2026-10-15 M3.6 refactor(market): bind Level 1 broker session to provider registry (M3.6 part 6/8) - moved epoch and retry ownership from its streaming worker to the registry with bounded event channels; Rithmic migration follows.
- 2026-10-15 M3.6 refactor(market): bind Rithmic transport replacements to registry epochs (M3.6 part 7/8) - removed the last legacy dispatch and kept adapter transport retry ownership; scripted rebind and isolated Hyperliquid switch checks passed, with startup Loading under investigation.
- 2026-10-15 M3.6 test(market): qualify provider registry desktop switching (M3.6 part 8/8) - observed one Hyperliquid socket and zero reconnects through four zero-gap switch snapshots; recorded separate fresh-start Loading issue and gated-provider live deferrals.
- 2026-10-15 M3 startup fix(market): reserve provider generation before startup history - pre-M3 and pre-registry detached builds rendered the default; fixed the generation race at the coordinator and verified fresh-default rendering without input.
- 2026-10-15 M3.7 feat(market): add bounded instrument leases and trading feed contracts - validated quote/route/terms contracts and durable grid/class, re-keyed the single shared lease owner, and added a nonblocking quote guard release; provider subscriptions and feed follow in M3.8.
- 2026-10-15 M3.8 feat(market): connect quote-only leases to provider demand (M3.8 part 1/3) - no-chart demand drives upstream BBO, quote validation and guard latest state; complete feed and probe follow.
- 2026-10-15 M3.8 refactor(market): lease trading and alert quote demand in engine (M3.8 part 2/3) - replaces the alert-only upstream merge and validates revisioned trading interest; feed events and live probe follow.
- 2026-10-15 M3.8 feat(market): publish bounded trading quote feed (M3.8 part 3/3) - verified no-chart Hyperliquid BBO, calendar/feed/terms events, and the exchange-sourced significant-figure grid under an isolated validation root.
- 2026-10-15 M3.9 feat(market): wait for consumer publications instead of polling (M3.9 part 1/3) - measured p50/p99 wake latency and zero notifications over a 30-second attached idle window; fenced events and ordered commands follow.
- 2026-10-15 M3.9 refactor(desktop): order market commands through one bounded queue (M3.9 part 2/3) - deleted priority slots, retained adjacent latest-wins within FIFO, and tested click-before-viewport ordering and explicit saturation.
- 2026-10-15 M3.9 feat(market): fence every consumer delivery (M3.9 part 3/3) - exact selection rejection, registration epochs and naming gate; three isolated desktop symbol clicks met UI and covering-history timing limits.
- 2026-10-15 M3.10 feat(market): publish registry-backed selection descriptors (M3.10 part 1/3) - complete four-provider metadata and calendar-resolved series are published without a second provider table; transactional selection follows.
- 2026-10-15 M3.10 feat(market): add transactional runtime chart selection (M3.10 part 2/3) - direct typed selection and rollback keep old live demand on failure; desktop migration follows.
- 2026-10-15 M3.10 feat(market): resolve chart selections inside the coordinator (M3.10 part 3/3) - fenced catalog callbacks, typed outbox outcomes and one-invoke desktop picks; full gate and isolated fresh-default/ten-switch checks passed.
- 2026-10-15 M3.11 test(market): replay market lifecycle through scripted registry - 50-switch late-completion replay, recovery, exhaustion and calendar scenarios passed twice; naming and full workspace gate green.
- 2026-10-15 M3.12 feat(market): add isolated lifecycle probe and WebSocket fault control (M3.12 part 1/2) - complete headless probe and bounded retry diagnostics passed focused and full gates; clean-source live evidence and GUI check follow.
- 2026-10-15 M3.12 test(market): qualify live lifecycle probe and fresh startup (M3.12 part 2/2) - clean-revision probe passed every check, production-root rejection and secret scan passed, and the untouched isolated desktop rendered a live zero-gap default chart.
- 2026-10-15 M3.6 fix(market): bound provider control retries and fail dead workers - scrutiny round 1: future retry deadlines, terminal worker failures and bounded scripted coordinator turns; full gate green.
- 2026-10-15 M3.10 fix(market): preserve broker search identity through chart selection - broker scripted search-result selection passes, and isolated demo EURUSD renders; provider-gap attribution remains unverified.
- 2026-10-15 M3.10 test(market): reject retired catalog readiness events - current connection publishes CatalogReady, retired connection is dropped and counted.
- 2026-10-15 M3.11 test(market): qualify lifecycle transition and recovery contracts - added table-row, demand-diff, late-event replay, stall, calendar, retry and shared-lease assertions; fixed rolled-Dormant live bypass at the lifecycle owner.
- 2026-10-15 M3.10 fix(market): clear closed symbol search input - fixed retained query on reopen with GPUI regression; window-targeted storm timing still requires isolated desktop verification.
- 2026-10-15 M3.12 fix(market): refuse overridden production root before probe IO - child process rejected the fresh overridden root without writing and retained the policy error.
- 2026-10-15 M3.12 fix(market): select probe symbols by stable instrument identity - removed the zero search-generation probe request and added focused coverage; clean-source live qualification follows.
- 2026-10-15 M3.1 fix(desktop): retain bounded per-gap live snapshot evidence - seven EURUSD 1m omissions matched raw decoded provider minutes and the pre-M3 chart; 32-entry truncation test and full workspace gate passed.
- 2026-10-15 M3.6 fix(market): document carried pane interval on symbol switch - fresh-root SOL stayed 1m; catalog selection carries the pane interval rather than a symbol-specific weekly default.
- 2026-10-15 M3.12 docs(plan): record final-HEAD lifecycle probe qualification - clean `088899c4` probe passed 28/28 and the overridden-root refusal wrote nothing.
- 2026-10-15 M4.1 fix(trading): stamp cancels at monotonic command time - injected clock, regression and single-line stop golden update; GUI cancel flow blocked by pre-M4.7 instrument registration.
- 2026-10-15 M4 triage fix(trading): restore verified default contract terms and dynamic bracket increments - pre-M3/HEAD isolated GUI comparison found two regressions; verified live practice ladder order after owner-side fixes, full gate green.
- 2026-10-15 M4.2 refactor(trading): isolate the single trading owner shell - moved command and timed wait boundaries, retained golden behavior, and verified concurrent revision/restart plus idle ManualClock coverage.
- 2026-10-15 M4.3 test(trading): replay core against SQLite with deterministic transcripts - ported both unchanged goldens, tested restart/commit abort and seeded reproduction.
- 2026-10-15 M4.4 perf(trading): index owner order lookups and executable demand - removed order and reverse-broker scans with index-consistency coverage; measured 409.646 ms scan vs 0.281 ms indexed for 1,000 lookups at 50,000 orders.
- 2026-10-15 M4.5 refactor(trading): split core behavior into focused modules - moved owner methods and broker/group implementations mechanically with unchanged goldens and scripted scenarios; full gates passed.
- 2026-10-15 M4.6 refactor(trading): split SQLite store by table family - preserved V1–V19 and restart assertions, added StepTx and bounded safe preflight scratch cleanup with failure diagnostics.
- 2026-10-15 M4.7 part 1/3 feat(market): restore trading lease routes with verified terms - preserved provider contract fields, resolved chart-free routes through the catalog and fenced the current quote snapshot; full gate passed.
- 2026-10-15 M4.7 part 2/3 feat(trading): prepare bounded owner market feed and persistent routes - tested validation/fencing, overflow, interest lifecycle/bounds, V19 migration and restart without changing simulated execution; desktop cutover remains part 3.
- 2026-10-16 M4.7 part 3/3 fix(trading): execute simulated orders from the owner quote book - removed the desktop quote relay and registration, wired feed and ticket demand, and tested pane-free stop/mark/restart; GUI assertions remain to be qualified.
- 2026-10-16 M4.8 feat(trading): gate simulated orders on provider-neutral session and fresh Live quote - four provider fixture families, all entry shapes, clock boundaries and restart fence tested; isolated BTC market/Join Bid accepted, but GUI far-resting limit and pane-free stop/bracket remain unverified.
- 2026-10-16 M4.9 feat(trading): consolidate fill accounting under V21 - migrated V20 routes and legacy fills/cycles, unified owner position and P&L path, and checked reversal/scale-out agreement with unchanged goldens.
- 2026-10-16 M4.10 part 1/4 feat(trading): persist revisions with existing write transactions - removed the standalone revision commit, characterized cancel/amend rollback and restored the ignored open-position quote benchmark.
- 2026-10-16 M4.10 part 2/4 feat(trading): M4.10 part 2/4 - committed staged command effects once before memory/publication, and reserved broker queue capacity through commit without compensating undo.
- 2026-10-16 M4.10 part 3/4 feat(trading): M4.10 part 3/4 - staged multi-effect quote facts on a private projection and committed one SQLite transaction before memory/publication; failed-commit replay covers bracket arming and OCO cancellation.
- 2026-10-16 M4.10 part 4/4 feat(trading): M4.10 part 4/4 - zero SQLite writes and no full clone for mark-only quotes; hourly retention caps each table at 5,000 deletions; release quote throughput measured before 1,244.4 vs after 1,203,224.6 quotes/s; combined GUI still outstanding.
- 2026-10-16 M4.10 test(trading): M4.10 VAL-TRD-029 replay golden - froze the chart-free crypto command/stop/restart/accounting recovery transcript without changing earlier goldens.
- 2026-10-16 M4.10 fix(desktop): refresh mark-only bottom panel P&L - follow-up to GUI pane-free validation; same-revision owner marks now refresh open rows and account summaries without recopying trade history.
- 2026-10-16 M4.10 docs(trading): record combined isolated GUI outcomes - pane-free marks passed; chart standalone limit gap and incomplete bracket, reversal and full hard-kill contract recorded without claiming passes.
- 2026-10-16 M4 follow-up feat(desktop): add priced order ticket entry - exact scale conversion and all four priced side/type actions close the VAL-TRD-021 / VAL-CROSS-009 GUI entry gap; real-path assertions pending.
- 2026-10-16 M4 follow-up fix(trading): price chart brackets from verified owner grid - removes the desktop fixed-tick precondition for Hyperliquid and tests owner bracket admission; GUI leg-fill evidence pending.
- 2026-10-16 M4 follow-up docs(trading): record isolated priced-ticket, reversal/accounting and sub-second hard-kill recovery checks; near-touch chart bracket stop filled after BTC pane closure, target canceled, flat in 14 seconds.
- 2026-10-16 M4 closeout docs(plan): close M4 trading-core tracker after isolated GUI qualification - checked M4.7; follow-up commits `7014b40e` (replay golden), `c7258bd3` (mark P&L), `2b87e65f` (initial GUI evidence), `d249e148` (priced ticket), `d298a526` (verified-grid brackets), `6ed03a11` (final GUI evidence) complete M4.1–M4.10; formal milestone validation is pending.
- 2026-10-16 M4.7 fix(market): deliver cached trading quotes after feed controls - restored chart-free snapshots deliver Terms, Session and Feed before a current-generation Quote across individual drains and bounded overflow; VAL-TRD-011 scrutiny blocker addressed.
- 2026-10-16 M4.10 docs(plan): record committed before/after and fresh HEAD quote throughput - 1,244.4 at `20be05fd`, 1,203,224.6 at `658a73bd`, 1,176,609.0 quotes/s at `58fbb06b` using the same release benchmark on the same Windows machine.
- 2026-10-16 M4.7 test(trading): cover quote rejection and mixed inbox overflow - VAL-TRD-012 exact bounded feed and sink-refresh cases pass.
- 2026-10-16 M4.7 test(trading): exercise chartless stop through market lease - VAL-TRD-009 scripted market feed, P&L marks and stop fill pass with zero chart subscriptions.
- 2026-10-16 M4.7 test(trading): cover every quote interest transition - VAL-TRD-010 adds one lifecycle test with planned bracket legs, group-only projection, ticket and sink bounds, flat release and no-send checks.
- 2026-10-16 UI (outside milestones) feat(ui): adopt the Theme System popover theme - new light/dark tokens, opaque popover layers with edge highlight, normal-weight buttons, chart settings on `surface`, and source-over `ThemeColor::over` fixing near-white title-bar hovers on translucent dark surfaces; design-system/terminal-UI/desktop tests, clippy, naming and website build pass; visual check by maintainer.
- 2026-10-16 UI (outside milestones) feat(ui): refine panels, settings controls and theme picker - Primary/Secondary button roles, field and panel surfaces, borderless settings and accounts cards, chart settings choices as fixed-width dropdowns matching color swatches, tabular clock numerals, and a preview-tile theme picker in the platform menu; desktop tests, clippy and naming pass; visual check by maintainer.
- 2026-10-16 M5.1 part 1 feat(trading): reject conflicting client identity on retry - unified simulated and broker order retry comparison with restart and terminal-state regression; V22 lifecycle migration and transitions remain for the next part.
- 2026-10-16 M5.1 part 2 feat(trading): migrate V21 lifecycle facts to V22 - add preserved lifecycle columns and recognized historical reason codes with fixture/backup regression; model and transition ownership remain.
- 2026-10-16 M5.1 part 3 feat(trading): persist simulated cancellation terminal time - one pure cancellation transition stamps owner command time, durable across command and quote paths and restart; remaining transitions follow.
- 2026-10-16 M5.1 part 4 feat(trading): initialize durable order acceptance revision - both routes share lifecycle admission fields, simulated cancellation increments revision, and early V22 rows restore.
- 2026-10-16 M5.1 part 5 feat(trading): revision command transitions on both routes - owner-clock update times and revisions survive simulated and broker command paths, rollback and restart; lifecycle remainder pending.
- 2026-10-16 M5.1 part 6 feat(trading): revision simulated fill transitions - fill and flatten persist owner-clock revision and terminal time across restart; broker transitions and typed events remain.
- 2026-10-16 M5.1 part 7 feat(trading): own broker lifecycle report transitions - broker reports, refusals, missing reconcile and deals advance durable order revisions and owner-clock times, with duplicate no-op and terminal fencing.
- 2026-10-16 M5.1 part 8 test(trading): preserve broker lifecycle on failed commits - fault-injected report and deal transactions leave memory and disk unchanged, then replay once.
- 2026-10-16 M5.1 part 9 feat(trading): distinguish inactive and expired lifecycle facts - typed status/event storage, broker expiry transition and held-child admission semantics; chart Inactive needs coordinated engine support before group migration.
- 2026-10-16 M5.1 part 10 feat(trading): fence modify and cancel by order revision - owner-side optional revision guard returns current order on stale instructions, with no persisted facts or venue dispatch; simulated and broker restart tests pass. Existing unguarded callers and move-leg fencing remain.
- 2026-10-16 M5.1 part 11 fix(trading): reject inactive broker lifecycle edges - exhaustive current-status request/admission/filled matrices and broker report/missing/deal edge tests; future group/trigger/expiry edges remain.
- 2026-10-16 M5.1 part 12 feat(trading): persist typed lifecycle reasons and risk warnings - typed SQLite round trips and historical migration, chart annotation keyed by reason, and v2 replay ledgers with reason column; remaining lifecycle edges deferred.
- 2026-10-16 M2-M5 docs(plan): record milestone validation results after the mission stopped - M2 (`f00d4a99`), M3 (`530a7c8d`) and M4 (`3529116c`) passed scrutiny and user-testing validation; M5 remains in progress with M5.1 parts 1-12 committed, M5.1 unchecked and VAL-TRD-030..074 unvalidated.
- 2026-10-16 M5.1 part 12 test(trading): give the V21 lifecycle fixture valid event provenance - the typed reason restore check loads events through the production decoder, which rejects the fixture's empty provenance; one fixture update keeps the test under the Clippy length limit.
- 2026-10-16 UI (outside milestones) test(account): expect the popover theme dark surface on the callback page - the sign-in callback embeds `platform.css`, whose dark `--surface` became `#171717` in the popover theme; the test still asserted `#1f1f1f`.
- 2026-10-16 UI (outside milestones) feat(charts): pin Aeris Charts 58e23d1 and use its order-flow footprint colors - the footprint keeps the engine's theme-shared buy `#006edd`, sell `#d9306a` and warning POC instead of Terminal's `bullish`/`bearish` override, so the theme-change footprint resync is removed; row merging (`adaptive_rows`) stays off by default, so dense rows rely on the engine's 8 px text floor; the naming_check pin moved with the bump; fmt, workspace clippy, workspace tests and naming_check pass, and `burst_delivery_is_bounded_and_backpressured` failed once under full-suite load but passed when rerun alone and in the next full run.
- 2026-10-16 UI (outside milestones) feat(ui): use dropdowns for indicator setting choices - study choices and line thickness, Volume placement and average type, and Big Trades filter and bubble size share one dropdown whose open state lives in the dialog's draft state; unused wrapped and disabled tab variants removed; desktop tests, clippy and naming pass; visual check by maintainer.
- 2026-10-16 UI (outside milestones) feat(charts): follow the engine crosshair default and add per-line study colors - saved chart appearance records whether the crosshair style was the user's pick (`crosshair_style_custom`), so layouts that stored the former dashed default now follow the engine's solid default; studies persist per-output line colors (`output_colors`, empty means the engine default) applied through the engine's `set_external_study_output_style`, edited as "Line colors" in study settings (EMA Ribbon's five lines included), and a malformed list is cleared on load instead of dropping the study; the indicator menu renders, highlights, arrows, Enter and scrolls from one entry list covering Order flow and native rows, highlighting only after typing or an arrow key; picker labels read Simple/Exponential Moving Average to match the settings dialog; fmt, workspace clippy, workspace tests and naming_check pass; visual check by maintainer.
- 2026-10-16 UI (outside milestones) fix(ui): keep chart templates to styling and give indicator dialogs the panel surface - chart settings templates no longer store or apply the chart type, so saving, applying or "apply to all" never switches a pane's series; tag 2 of `WorkspaceChartSettingsTemplateState` is retired and older templates load their styling only; the study, Volume and Big Trades dialogs fill with `surface` through `Dialog::panel_surface` and group their rows on titled, borderless `surface-secondary` cards like the chart settings panel (CSS.md rules updated); the Info Line card and the brushable-area tooltip are both painted by Aeris Charts, the card from `--surface`/`--border` chrome tokens and the tooltip from the popover tokens, so aligning them is an engine change; fmt, workspace clippy, workspace tests and naming_check pass; visual check by maintainer.
- 2026-10-16 UI (outside milestones) feat(charts): pin Aeris Charts e66b9a4 for the popover Info Line card - the engine paints the Info Line card with the same popover surface, border, 6 px compact radius (`radius.compact`, already Terminal's `--radius-compact`) and text colors as the brushable-area tooltip, closing the mismatch noted in the previous entry without Terminal wiring; the pin also brings 9e49960, whose engine themes carry three neutral text roles and an AA light muted gray `#717171`, which Terminal adopts: neutral text is `text-default` (now full strength `#0a0a0a` / `#fafafa`, taking the retired `text-primary` value), `text-secondary` and `text-muted`, and every `text_primary` caller moved to `text_default`; light `text-secondary`, `text-interactive` and `icon` darken from `#0a0a0a8c` to `#0a0a0a94`, so secondary text measures 4.68-4.88:1 on light surfaces and 4.66-5.91:1 on dark surfaces and popover tints (WCAG AA 4.5:1); CSS.md names the three text roles and points at the Theme System's current location (`design_system`); only the six Aeris Charts lockfile sources changed and the naming_check pin moved with the bump; the website mirror still carries `text-primary` (148 uses in 28 files) and waits for the maintainer's go-ahead because 11 of those files hold other uncommitted work; fmt, workspace clippy, workspace tests and naming_check pass; visual check by maintainer.
- 2026-10-17 M5.5 prerequisite feat(charts): project held orders with the engine's inactive status - pushed charts `36d5b92`, pinned only chart sources and tested chart_integration mapping; M5.5 must replace legacy planned-leg projections with real Inactive orders before GUI validation.
- 2026-10-17 M5.1 part 13 feat(trading): fence chart and ladder order edits by displayed revision - chart cancellation and repricing carry the displayed order revision, with owner-side stale rejection before quote resolution; full workspace gate passed, GUI qualification remains at milestone end.
- 2026-10-17 M5.1 part 14 feat(trading): persist each order's quote eligibility fence - replace the parallel eligibility map with durable lifecycle state and a process-start recovery floor; full workspace gate passed, milestone GUI qualification remains outstanding.
- 2026-10-17 M5.1 part 15 fix(trading): preserve terminal status on late broker fills - status-by-deal transition matrix and late Expired regression; full workspace gate passed, milestone GUI qualification remains outstanding.
- 2026-10-17 M5.1 part 16 feat(trading): persist trigger and expiry lifecycle timestamps - canonical V22 optional fields round-trip through all order writers without inferring legacy times; full workspace gate passed.
- 2026-10-17 M5.1 part 17 feat(trading): complete pure lifecycle transition table - typed causes, held activation, trigger event/time, due expiry and every simulated status edge; M5.1 closed under the pure-function scope ruling, producers remain with M5.3-M5.5/M5.9.
- 2026-10-17 M5.2 part 1 feat(trading): enforce shared price grids on order placement - exact fixed/significant-figure grid checks on both routes and a typed OffGrid rejection; full validator remains for later parts.
- 2026-10-17 M5.2 part 2 feat(trading): reject wrong-side stop triggers - shared strict bid/ask stop-side rule on owner Live quotes with typed rejection; unified admission remains open.
- 2026-10-17 M5.2 part 3 feat(trading): reject stop IOC and FOK on both routes - shared TIF rule and typed refusal before any order or broker request; full admission remains open.
- 2026-10-17 M5.2 part 3 docs(plan): correct the M5 milestone status line after the gated admission slices - M5.2 parts 1-3 are committed, later items and real-path verification remain.
- 2026-10-17 M5.2 part 4 feat(trading): share quantity and capacity admission across routes - one common pre-persistence validator replaces duplicated checks; full admission remains open.
- 2026-10-17 M5.2 part 5 feat(trading): preflight quote-interest capacity through shared admission - staged and committed interest count before an order fact, typed capacity refusal; full admission remains open.
- 2026-10-17 M5.2 part 6 feat(trading): centralize route trading capabilities - one broker TIF conversion and route protection policy; full admission remains open.
- 2026-10-17 M5.2 part 7 feat(trading): type locked-account admission refusals - both routes reject early without losing the stored lock reason; protective exits remain available.
- 2026-10-17 M5.2 part 8 feat(trading): resolve resting prices against the owner quote - chart and general RestingAt instructions become durable Limit or Stop orders on the owner, with missing-quote refusal and restart coverage; M5.2 remains open.
- 2026-10-17 M5.2 part 9 feat(trading): reserve a no-op buying-power admission step - both placement routes explicitly pass through the owner hook without imposing out-of-scope margin constraints; M5.2 remains open.
- 2026-10-17 M5.2 part 10 feat(trading): warn on unverified instrument price grids - Unknown stays tradable and records a typed warning on both routes through restart; M5.2 remains open.
- 2026-10-17 M5.2 part 11 feat(trading): unify practice market and currency admission - session/quote and typed currency checks join shared admission without masking quote-interest capacity; M5.2 remains open.
- 2026-10-17 M5.2 part 12 feat(trading): preflight priced bracket legs against the grid - stop and target prices are validated before admitting a priced entry on either route; M5.2 remains open.
- 2026-10-17 M5.2 part 13 test(trading): characterize simulated and broker admission parity - shared route cases assert typed refusals, no writes and no venue request; M5.2 remains open.
- 2026-10-17 M5.2 part 14 feat(trading): centralize dynamic risk in admission - the free admission boundary executes rules once for both routes and supplies acceptance warnings; M5.2 remains open.
- 2026-10-17 M5.2 part 15 test(trading): verify wrong-side stop parity on live owner quotes - Stop and StopLimit touch-side refusals match on practice and demo broker with no facts or outbound request; M5.2 remains open.
- 2026-10-17 M5.2 part 16 test(trading): verify group-leg GTC and routed interest capacity - close code-level admission checks, defer off-grid ticket GUI to the single milestone pass.
- 2026-10-17 M5.3 part 1 feat(trading): price resting limits at their stated limit - two-sided crossed-quote regression; placement-time behavior follows in the next part.
- 2026-10-17 M5.3 part 2 test(trading): pin point-value PnL at exact marks and exits - future and crypto multiplier arithmetic characterized in a two-sided round trip.
- 2026-10-17 M5.3 part 3 feat(trading): cancel nonmarketable immediate orders at acceptance - two-sided IOC/FOK terminal fact and restart regression.
- 2026-10-17 M5.3 part 4 feat(trading): fill marketable orders at placement touch - atomic accepted/fill projection, two-sided price/restart test and updated chartless mark expectations.
- 2026-10-17 M5.3 part 5 test(trading): prove placement fill rollback on failed commit - injected SQLite failure leaves no partial owner facts and retry restores exactly.
- 2026-10-17 M5.3 part 6 feat(trading): expire unquoted market orders after five seconds - Live missing-quote admission, owner deadline, typed cancellation and restart recovery.
- 2026-10-17 M5.3 part 7 feat(trading): emit a stop trigger before its touch fill - exact/gapped bid-ask cases and two explained v2 golden changes.
- 2026-10-17 M5.3 part 8 feat(trading): retain triggered stop-limits across quote gaps - buy/sell same-quote taker and post-restart resting maker tests.
- 2026-10-17 M5.3 part 9 feat(trading): react to each quote fill before its next candidate - current-order recheck, bracket activation fence and same-quote stop-limit price preservation.
- 2026-10-17 M5.3 part 10 feat(trading): prioritize triggered and resting fill candidates by price - stop-first, side-aware price and local acceptance order.
- 2026-10-17 M5.3 part 11 feat(trading): isolate full-size touch execution hooks - single venue policy for quantity and fill price, named-trait exception documented.
- 2026-10-17 M5.3 part 12 feat(trading): index crossing order candidates by side and price - bounded owner-maintained trigger membership and 4,000-step scan/restart comparison.
- 2026-10-17 M5.4 part 1 feat(trading): fix DAY deadline at simulated acceptance - owner session close or next UTC midnight persisted with the order.

- 2026-10-17 M5.4 part 2 feat(trading): expire due DAY orders on the owner timer - exact ManualClock deadline, atomic event and bracket cascade, bounded retry and restart equality.

- 2026-10-17 M5.4 part 3 feat(trading): recover overdue DAY orders before service startup - store-open expiry persists the original deadline before any command.
- 2026-10-17 M5.4 part 4 fix(trading): fence legacy DAY orders pending calendar recovery - null-deadline migrated rows cannot fill on crossing Live quotes, including after restart.
- 2026-10-17 M5.4 part 5 feat(trading): recover continuous DAY deadlines from verified routes - persisted route establishes AlwaysOpen, atomic midnight backfill then overdue expiry before startup.
- 2026-10-17 M5.4 part 6 feat(trading): recover weekly DAY deadlines from shared calendar - historical acceptance-session close reused by market and trading, including DST and overnight hours.
- 2026-10-17 M5.4 part 7 feat(trading): recover legacy DAY after verified Terms arrive - route-less row stays fenced until Terms backfill and overdue expiry.
- 2026-10-17 M5.4 part 8 test(trading): prove multi-DAY backfill rollback and retry - abort the second UPDATE, preserve both rows, then expire on clean restart; milestone GUI pending.
- 2026-10-17 M5.5 part 1 feat(trading): define bounded typed order-group model - structural invariants and roles without changing the legacy runtime producer.
- 2026-10-17 M5.5 part 2 feat(trading): add V23 order-group tables without replacing legacy facts - constrained durable group schema; historical row conversion and real legs follow.
- 2026-10-17 M5.5 part 3 feat(trading): backfill standalone protection group membership - idempotent V19/V23 startup projection with collision rollback; managed bracket conversion remains next.
- 2026-10-17 M5.5 part 4 feat(trading): retain terminal managed brackets as groups - completed/cancelled projection links only complete historical child sets without changing legacy orders.
- 2026-10-17 M5.5 part 5 feat(trading): link existing active bracket orders in group migration - V19 file fixture preserves active child facts and reconciles later terminal legacy status.
- 2026-10-17 M5.5 part 6 fix(trading): make historical group projection atomic - injected last-leg abort rolls back every group and retry is idempotent.
- 2026-10-17 M5.5 part 7 feat(trading): accept unpriced held bracket children - allow only Inactive legs to await fill-based pricing; migration producer follows.
- 2026-10-17 M5.5 part 8 fix(trading): enforce order-group member cardinality - require two OCO members and one standalone protection member before the group owner switch; no live group writer changed.
- 2026-10-17 M5.5 part 9 feat(trading): commit typed group members with order steps - one validated transactional insert seam with cross-scope and last-leg rollback coverage, before the live authority switch.
- 2026-10-17 M5.5 part 10 feat(trading): switch simulated brackets to group-owned inactive legs - entry, held stop/target orders and the group commit atomically, the entry fill reprices and activates the same order ids, and the legacy snapshot views keep the desktop unchanged; milestone GUI pending.
- 2026-10-17 M5.5 part 11 feat(trading): convert awaiting brackets to group authority at startup - legacy awaiting-entry rows become pending groups with real inactive legs at their exact planned prices, unpriceable rows become cancelled groups with a typed recovery notice, and a failed conversion rolls back and retries on the next start.
- 2026-10-17 M5.5 part 12 feat(trading): place explicit OCO pairs under group authority - both members rest as one active group in a single order step, a member's full fill cancels its siblings inside the same quote step, a manual member cancel leaves the sibling working, and the wide-quote transcript is identical across 100 runs and a restart replay (VAL-TRD-042).
- 2026-10-17 M5.5 part 13 test(trading): prove the group coverage invariant - the contract walk (4 -> 3 -> 6 -> reversal) and six seeded bracket/manual/quote/restart runs assert per-role coverage equals |net| or zero, correct protective side, no dangling or orphaned legs after every step (VAL-TRD-047); a rejected placement is asserted a state no-op.
- 2026-10-17 M5.5 part 14 feat(trading): verify recovered groups and retire protection at runtime - startup classifies every recovered pending/active group, cancelling a broken group (dropping its torn legs) or settling an interrupted member fill in one committed order step each with typed BrokenGroup/InterruptedGroupFill notices, and reports a CoverageRepair notice before the loaded-state alignment resizes; migrated Protection groups now retire with a fill (Completed) or terminal leg (Cancelled). The group store enforces member presence only for live groups so a retired historical group stays writable, and terminal groups with no legs are valid historical facts for every kind. Five recovery tests cover a missing leg, an interrupted stop fill, a coverage mismatch, protection fill/cancel retirement and notice-free idempotent reopen; runtime 238 passed/5 ignored.
- 2026-10-17 M5.6 feat(trading)!: delete the superseded protection layers - legacy managed-bracket/protective-order runtime, store writers and `protection.rs`/`planned_leg.rs` removed; views renamed group-derived (`BracketView`, `ProtectionView`, `PlannedLeg`, `PlacementOutcome`) serve the desktop until M6.11; gated V24 finalize drops both legacy tables after the last awaiting conversion; rg zero-match evidence for every deleted name, runtime 240 passed/5 ignored, full gate green; GUI pass pending (VAL-TRD-053).
- 2026-10-17 M5.5 part 15 feat(trading): gate stop management on one-tick moves and lock the restart golden - managed trailing now reprices the stop only when the candidate tightens it by at least one tick (a 0.05 sub-tick move was caught writing a real event and transaction, then fixed); the new bracket_stop_after_restart golden records an active bracket surviving restart, its stop filling on the first post-restart quote and cascading both targets in one step; the V19 fixture test now proves the pre-migration backup keeps user_version 19, no group tables and the original rows; a broker bracket is asserted to create no order-group rows. Runtime 240 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed. M5.5 is complete except the milestone GUI pass (VAL-TRD-051).
- 2026-10-17 M5.7 feat(trading): add the cash ledger, exact cost basis and simulated fee schedules - V25 adds `ledger_entries`, per-account `fee_schedules`, exact cost-basis columns and denormalized class/price-grid columns; `migrate_ledger` backfills deposits, realized P&L and cost bases once per store after group finalize; simulated balances become the exact sum of ledger entries (only a gated pre-group store keeps legacy math); maker/taker fee engine with per-contract/per-unit-clamped/notional-bps models and deterministic schedule codec; ledger chain printed in replay traces with 5 goldens including a fee round-trip; registration deposit, purge and delete paths covered. Runtime 248 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed. GUI pass pending (VAL-TRD-058).
- 2026-10-17 M5.8 part 1/2 feat(trading): checkpoint retention and checkpoint-based recovery - V26 position/ledger checkpoint and equity tables; startup-plus-hourly retention folds pruned fill prefixes and ledger entries into checkpoints in the deletion transaction, pins open-order and live-group rows, and repair rebuilds checkpointed positions as checkpoint plus retained suffix; `rebuild_position_from_retained_fills` deleted; balances stay checkpoint plus exact entry sums. Runtime 250 passed/5 ignored.
- 2026-10-17 M5.8 part 3 feat(trading): bound the equity history - one equity sample per simulated fill committed inside the fill's transaction (quote and placement steps pass the planned samples to commit_quote_step; the emergency close flushes a follow-up write), a 60 s owner timer samples only on change plus once per UTC risk-session boundary for profiled accounts, and a 1,440-entry ring per account reloads the newest durable minute samples at open with the first tick delayed one interval so restarts replay unchanged. Retention downsamples minute rows past seven days into last-of-hour rows and drops hourly rows past a year in the same transaction, windows trailing the newest sample so a backward clock cannot mass-expire history. Regressions: per-fill/timer/session/restart sampling, ring bound, downsample collapse and year drop; mark-only/failed-commit atomicity and group-recovery reopen goldens stay green. Runtime 253 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed.
- 2026-10-17 M5.8 part 4 feat(trading): property-test the trading invariants - 200 seeds x 500 service-level steps assert pooled coverage, fill eligibility, DAY expiry, balance identity and the equity ring bound after every step; each restart adds the durable identity (no orphan legs, checkpoint+suffix positions, checkpoint+entries balance, no stale DAY rows) and notice-free reopen equality. The run caught a pending-bracket cascade defect (held-leg cancel left the entry working); fixed in stage_bracket_termination with a focused regression. TR-06 closes. Runtime 255 passed/5 ignored; full fmt, strict workspace Clippy, workspace tests and naming passed.
- 2026-10-17 M5.9 part 1/3 feat(trading): price emergency operations from verified runtime quotes - atomic scoped flatten/reverse through one commit_quote_step per action, typed unpriced reporting that keeps protection, sim-reverse IOC staging with enforce_risk, observation-free event risk and scoped/global broker multi-instrument close; legacy per-step fill writers deleted; regressions converted to the real feed path plus scope/unpriced/risk-reject/crash-atomicity coverage; v2 simulated golden regenerated for the new feed provenance, per-instrument revision numbering, sim-reverse leg and completed bracket. Runtime 260 passed/5 ignored; full gate green.
- 2026-10-17 M5.9 part 2/3 feat(trading): make the kill switch the full atomic emergency action - one durable step locks the in-scope accounts, cancels non-protective orders with the typed KillSwitch cause and flattens every priced position with group reconciliation; protection is never stripped, unpriced positions keep their stop/target and land in EmergencyOutcome::unpriced; broker cancels/closes stay reported venue requests; event Lock shares lock_and_disarm without flattening; stale CancellationCause dead-code expectation removed. New VAL-TRD-061 four-instrument regression and replay crash-atomicity test; property harness now follows activation-assigned leg prices; v2 golden regenerated for the kill_switch cancel reason and the kill flatten fill. Runtime 262 passed/5 ignored; desktop 449+81; full gate green.
- 2026-10-17 M5.9 part 3/3 test(trading): close out emergency operations - scripted broker two-symbol flatten parity (VAL-TRD-062: close requests for both positions, scoped variant closes one) and the locked-account reverse refusal (VAL-TRD-063). TR-03 and TR-04 closed; TR-12's reverse part closed. M5.9 complete except the milestone GUI flatten-all pass (VAL-TRD-064). Runtime 264 passed/5 ignored; full gate green.
- 2026-10-18 M5.10 feat(trading): persist risk sessions and roll them at the account boundary - V27 adds `risk_sessions` (seeded per registered profile) and `risk_locks.kind`; the owner timer rolls the session in one transaction (durable row with realized baseline, daily meter reset, daily-loss/trailing-drawdown lock release, boundary equity sample); bracket admission counts the template stop as attached; `set_risk_session_boundary` supports 17:00 America/Chicago via aeris_instruments DST math; session P&L falls back to the registration baseline for gated pre-group stores and pre-V27 locks read as manual. TR-09 runtime part closed. VAL-TRD-065 integration tests cover UTC-midnight and Chicago-close rollover, restart survival and the bracket no-warning path; v26->v27 migration test. Runtime 272 passed/5 ignored; workspace 2317 passed; fmt, strict Clippy and naming_check green.
- 2026-10-18 M5.11 feat(trading): publish bounded working-set changes with paged history - subscribe/TradingSubscription with an 8-subscriber cap, a 1,024-event per-subscriber slot and lagged resync; mark coalescing at one publication per 100 ms with trailing flush; idle runtimes publish nothing; paged order history (500-row pages, exact-once keyset order) on a read-only reader thread; snapshot 10,000-order hard-fail removed. Working-set build measured 3.44 ms at 50,100 stored orders. VAL-TRD-066/067 integration tests green; TR-05 runtime part closed. Runtime 280 passed/5 ignored; workspace 2324 passed; fmt, strict Clippy and naming_check green.
- 2026-10-18 M5.12 feat(probes): add live paper-trading and broker demo probes - paper_trading_probe runs the full practice lifecycle on the public Hyperliquid feed with restart and ledger identity (both fee and no-fee runs pass all 20 checks); the run surfaced three production defects now fixed with regression tests (catalog install re-arms trading demand, durable quote-interest revision, reconnect session republish). the broker demo probe implemented but not run (maintainer approval gate). Workspace 2332 passed; fmt, strict Clippy and naming_check green.
- 2026-10-18 M5 milestone pass fix(market): keep Hyperliquid workers alive across idle stops and bounded queue pressure - the milestone GUI pass found a restored three-chart workspace stuck on "connection interrupted" with one core busy: the display-depth worker exited on an idle Stop, the next idle Stop then hit its closed channel and latched the provider terminal (`provider control worker disconnected`), and the demand flush re-ran every turn against the failed session. An idle Stop now parks the display worker; the realtime worker's Connecting/Disconnected epoch events retry for at most 2 s against a full event queue instead of being dropped (which had stranded the reserved epoch); a flush against a terminal session clears its dirty demand. Four regressions (idle-stop park, lifecycle backpressure delivery, closed-channel exit, failed-flush retirement); market_runtime 446 passed/4 ignored with all features; full fmt, strict workspace Clippy, workspace tests and naming_check passed. Release desktop on the restored BTC 1m / ETH 5m / SOL 3D workspace: history completed for all three (1599/1599/1599 bars), no stall indicators, about 5.5 CPU-seconds per minute versus one full core before. M5.3 closeout and TR-08 closure recorded.
- 2026-10-18 M5 milestone GUI pass docs(plan): record the isolated release-desktop results - on e48c8fa3: the V19 seed shows the active bracket (SL 80,341 / TP 88,710) and awaiting bracket (entry 71,136, planned SL 65,278 / TP 79,505) lines with no recovery notice, and a relaunch writes no second backup; a BTC-USDC Buy Limit at 80740.5 is refused with "OffGrid: price is off the instrument price grid" and no order row; a fresh 50,000 practice account's buy/sell of 1 BTC shows balance 49,984.00 and realized -16.00, equal to its `ledger_entries` sum and `realized_pnl` sum with no fee rows (the ticket quantity accepts whole contracts only, so the minimum 0.00001 BTC could not be entered); Flatten All with only a BTC chart open closed BTC and ETH in about 3 s with a closing fill each and stayed flat after relaunch; the 10,001-terminal-order fixture showed balance, position and 300 history rows within about 2 s with no snapshot error, while the working limit has no bottom-panel view until M6. VAL-TRD-074 remains blocked on maintainer approval.
- 2026-10-18 M5 milestone closeout test(trading): correct the VAL-TRD-068 generated desktop load fixture to use the real `hyperliquid:perp:BTC` instrument and scale-8 BTC quantity/prices; retain the existing ESZ6 load-test shape for 50,100-order tests. A new file-backed restart test checks its BTC position and working limit. Fresh-root SQLite inspection confirmed schema 27, 10,001 terminal rows, one working BTC limit and one open BTC position; isolated release GPUI capture showed balance USD 100,000 and long 0.10 BTC at 81,500 within 0.86 s of the workspace window, and history showed 300 trades. Full workspace check, fmt, strict Clippy, naming and tests passed; first two full test attempts encountered unrelated intermittent market idle-stop and desktop burst timing failures, each passed alone and the final full run passed. Working-order panel row and fractional minimum ticket input are explicitly M6 follow-ups, not M5 passes; the broker demo probe remained approval-gated separately.
- 2026-10-18 M6 preflight test(desktop): stabilize the two parallel-run timing regressions observed at M5 close. The display-depth idle-stop test uses a zero-capacity control-channel rendezvous instead of fixed sleeps to prove repeated stops leave the worker alive. The short desktop endurance test always performs its two asserted frames even if descheduled for its 320 ms sampling window; production endurance still stops at its requested wall-clock duration. Both focused regressions and the default-parallel workspace test suite passed; full workspace fmt, check, strict Clippy and naming gates passed. No M6.1 checkbox is claimed by this prerequisite repair.
- 2026-10-18 M6.1 Part 1 test(desktop): extracted `FixtureMarketWorker` from the default market mailbox into cfg-gated desktop test support; deterministic fixture publication and naming_check positive/negative tests protect the boundary. M6.1 remains open for services injection and PaneHarness.
- 2026-10-18 M6.1 Part 2 refactor(desktop): injected the trading owner through `DesktopServices` into the lifecycle, panes, UI command paths and market ticket-interest client; removed `TRADING_SERVICE` and its process-wide install/lookup. A two-composition regression proves trading account changes do not cross between owners; naming_check rejects reintroduced globals with positive/negative source examples, and AGENTS.md records the restriction. Full workspace fmt/check/strict Clippy/tests/naming_check green (the first test run exposed a stale naming_check source pin; its replacement and full rerun passed). M6.1 remains open for account/market/feedback/persistence/clock injection and PaneHarness.
- 2026-10-18 M6.1 Part 5 refactor(desktop): replaced the global trading feedback mailbox with one cloneable handle per DesktopServices composition; all command and pane status paths use it. Independent-desktop feedback and naming guard regressions added; removed the unused repricing helper. Full workspace fmt/check/strict Clippy/tests/naming passed (one short-endurance parallel timing failure on the first test run; isolated retry and complete rerun passed). Persistence, clock and PaneHarness remain.
- 2026-10-18 M6.1 Part 6 refactor(desktop): routed chart-chrome and workspace persistence through per-desktop paths and save receipts; removed the global chart-chrome locks and workspace path lookup. Two-composition file-backed isolation and naming-check guard tests added; full workspace fmt/check/strict Clippy/tests/naming_check passed. Clock, short-endurance child-process isolation and PaneHarness remain; M6.1 is open.
- 2026-10-18 M6.1 Part 7 test(desktop): isolated the short-endurance RSS assertions in a child process without changing any assertions or production behavior. Focused regression and full workspace fmt/check/strict Clippy/tests/naming_check passed; clock and PaneHarness remain.
- 2026-10-18 M6.1 Part 9 refactor(desktop): routed session status, selected-instrument status and market snapshot/tail/recovery provenance timestamps through the injected desktop clock; removed the market-worker direct system-time helper. ManualClock session and provenance regressions pass; full workspace fmt/check/strict Clippy/tests/naming_check green. Other desktop presentation wall-clock helpers and PaneHarness remain; M6.1 is open.
- 2026-10-18 M6.1 Part 10 refactor(desktop): routed UI header/countdown, fill toast, context calendar, time-zone badge and provider notice timestamps through the injected desktop clock, removing their direct wall-time helpers. ManualClock calendar regression and full workspace fmt/check/strict Clippy/tests/naming_check pass. PaneHarness and VAL-DSK-001/002 closeout remain; M6.1 is open.
- 2026-10-18 M6.1 Part 11 test(desktop): gated disconnected fixture now drives a GPUI pane through the bounded mailbox; the smoke checks snapshot-to-chart, explicit readiness and render. Default desktop build, full fmt/check/strict Clippy/tests/naming_check and fixture naming positive/negative regressions pass; VAL-DSK-001/002 close and M6.1 is checked. Startup quarantine file naming and dormant account/update lifecycle wall-time internals are not desktop command/presentation clock consumers; M8 owns dormant lifecycle hardening.
- 2026-10-18 M6.2 Part 1 feat(desktop): added the bounded keyed notice owner and toast projection, safe static-title diagnostics, async chart-preference failure/clear behavior and update failure notices. Notification/toaster regression tests and the full workspace fmt/check/strict Clippy/tests/naming_check gate pass with serialized Cargo compilation; M6.2 remains open for pane notices, source/action expansion and discarded-result cleanup.
- 2026-10-18 M6.2 Part 2 fix(desktop): removed dead provider and selector-pending parameters, made shell initialization, workspace factory absence and window opening explicit failures rather than production panics. An initial cold parallel check hit MSVC LNK1114 in aws-lc-sys; CARGO_BUILD_JOBS=1 rebuilt successfully and the full workspace fmt/check/strict Clippy/tests/naming_check gate passed. M6.2 remains unchecked pending notice consolidation and the four injected-failure tests.
- 2026-10-18 M6.2 Part 3 test(trading): extended only the seeded-property test's bounded shutdown wait from 2 to 10 seconds to tolerate concurrent workspace scheduling; 200 seeds and 500 steps per seed, all invariant/restart assertions unchanged. The focused property run and the full workspace fmt/check/strict Clippy/tests/naming_check gate passed with CARGO_BUILD_JOBS=1; M6.2 remains unchecked pending pane notices and the four injected-failure tests.
- 2026-10-18 M6.2 Part 4 fix(desktop): routed order-book demand failure/clear events from each pane to the single TerminalApp toaster with pane-scoped deduplication and retired-pane cleanup; injected disconnected command delivery in PaneHarness and asserted the rendered notice. Full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1 after a cold aws-lc-sys MSVC LNK1114 retry. M6.2 stays open for the remaining pane error sources, injected failures and discarded-result cleanup.
- 2026-10-18 M6.2 Part 5 fix(desktop): sent sanitized price-alert processing failure/success controls through the bounded market mailbox to the pane notice bridge; repeated failures render one card and successful processing clears it in PaneHarness. Focused regression and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for restore setters, symbol search, source/action variants, remaining injected failures and discarded-result cleanup.
- 2026-10-18 M6.2 Part 6 fix(desktop): routed fallible snapshot chart restore setters and the surface time-zone apply to a single sanitized pane notice, with a repeated invalid persisted-zone setter regression rendered by PaneHarness. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. Startup chart setters and other M6.2 notice/discard work remain open.
- 2026-10-18 M6.2 Part 7 fix(desktop): routed bounded-queue and catalog-rejected symbol searches to the one pane notice bridge with sanitized detail; an accepted search completion clears the notice. PaneHarness exercises full command queue, repeat deduplication, rendered card and completion clearing. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for startup setters, pane strings, source/action expansion and discarded-result cleanup.
- 2026-10-18 M6.2 Part 8 fix(desktop): removed per-pane price-alert message and inline dialog errors; restore, user action and processing failures use sanitized pane notices with scoped success clearing. PaneHarness covers queued restore failure, repeated delete failure and rendered-card clearing. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 stays open for startup setters, remaining pane strings, source/action variants and discarded-result cleanup.
- 2026-10-18 M6.2 Part 9 fix(desktop): replaced per-pane symbol status string with fenced browser/chart projection and routed search validation/dispatch and selection failures through sanitized keyed pane notices. PaneHarness verifies repeated failures, no credential-bearing detail, rendered cards and accepted completion/commit clearing; focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1 after a cold aws-lc-sys MSVC transient retry. M6.2 remains open for startup setters, indicator/chart-state strings, source/action variants and discarded-result cleanup.
- 2026-10-18 M6.2 Part 10 fix(desktop): removed per-pane indicator error string and its header/menu rendering; study, settings, order-flow, drawing and unsupported-zone failures now post static sanitized pane notices, with category-scoped success clearing and sanitized dialog-local runtime feedback. PaneHarness verifies repeated study/volume/zone failure rendering and recovery. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for startup setters, chart-state presentation, source/action variants and discarded-result/unwrap cleanup.
- 2026-10-18 M6.2 Part 11 fix(desktop): removed per-pane chart-state message storage; chart status derives from typed chart/connection state, pending timeframe and sanitized failure cause, retaining distinct unsupported-restore and worker-stop status. Runtime failures post one pane-keyed notice and successful coverage clears it; PaneHarness exercises redaction, rendered one-card deduplication and recovery. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for startup chart setters, other pane feedback, source/action variants and discarded-result/unwrap cleanup.
- 2026-10-18 M6.2 Part 12 fix(desktop): deferred fallible empty-chart startup setters until after pane notice subscription; their real errors post the same sanitized chart-restore key as snapshot chart restore, with no duplicate state. PaneHarness exercises actual Rithmic startup creation, initial failure, repeated keyed one-card rendering and corrected-zone replacement snapshot clearing. Focused and full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for connection/series feedback, source/action variants and discarded-result/unwrap cleanup.
- 2026-10-18 M6.2 Part 13 fix(desktop): classified existing pane notices by pane and provider selection by the registered provider ID; added one real Retry action for chart-preference save failure through the existing toaster to TerminalApp's async save. Rendered click, keyed replacement, pane source and provider-selection tests passed; full workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1. M6.2 remains open for Trading(RequestId), OpenFolder/ConnectProvider prerequisites, connection/series feedback and discarded-result/unwrap cleanup.
- 2026-10-18 M6.2 Part 14 fix(desktop): handled saturated pane resource-class demand with one keyed notice and successful clear, distinguished detached shutdown and market worker full-queue retirement, and documented/diagnosed pending search, automatic study removal and empty-chart precision outcomes. PaneHarness and worker retirement regressions plus full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains open for GPUI owner-update, profile refresh, market bridge and shutdown discards, other source/actions, connection/series feedback and unwrap cleanup.
- 2026-10-18 M6.2 Part 15 fix(desktop): handled weak-owner update outcomes for pane/workspace callbacks without duplicate state: released owners cancel quietly, live update failures reach diagnostics. Preserved the tested constant-layout and complete-command-registry startup invariants instead of arbitrary fallbacks. Focused timer-fence tests and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains open for profile refresh, market bridge and shutdown discards, connection/series feedback, deferred source/actions and other unwrap cleanup.
- 2026-10-19 M6.2 Part 16 fix(desktop): classified bounded bridge send failures and retired receivers, surfaced startup/shutdown cleanup failures through stderr-safe diagnostics, and preserved chart-failure notice ownership. Focused mailbox and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains open for account profile refresh, connection/series feedback, deferred notice variants, and remaining unwrap/discard cleanup.
- 2026-10-19 M6.2 Part 17 fix(desktop): classified account profile-refresh rollback/activation results as signed-out teardown or live bounded-queue failure, with sanitized keyed UI notice or detached-owner diagnostic; diagnosed optional login cancellation failure without enabling authentication. Deterministic account queue tests and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains open for connection/series feedback, deferred notice variants, and remaining unwrap/discard cleanup.
- 2026-10-19 M6.2 Part 18 fix(desktop): routed stopped-connection and series-dispatch failures into the existing sanitized keyed pane notice bridge, preserved typed loading/status projection and removed unused connection/header display strings. PaneHarness repeated-error/redaction/recovery tests and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains open for deferred notice sources/actions and remaining unwrap/discard cleanup.
- 2026-10-19 M6.2 Part 19 fix(desktop): handled chart trading/overlay/plan setter errors through one pane-keyed redacted notice and successful clear, removed a redundant context discard, and documented exact request-ID, safe-folder-opener and descriptor-connect blockers rather than fake actions. Focused PaneHarness and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains unchecked for blocked source/action variants and update-process result cleanup.
- 2026-10-19 M6.2 Part 20 fix(desktop): classified update child cleanup, reaping and detached worker failures without GPUI waiting, and verified exited/running helper paths. Audited VAL-DSK-003..007 against the active mission contract: 005/006 have missing exact evidence, and 007's literal selection_pending zero-match conflicts with active selection state. Focused and full serial workspace fmt/check/strict Clippy/tests/naming_check passed; M6.2 remains unchecked for those gaps and deferred real source/actions.
- 2026-10-19 M6.2 Part 21 fix(desktop): aligned four injected pane failure notices to Error severity and asserted source, single rendered card and repeated-key behavior; captured a real diagnostic sink in an isolated child test to verify secret-bearing restore errors never reach notice detail or the logged public title line. Focused and full serial workspace fmt/check/strict Clippy/tests/naming_check passed. M6.2 remains unchecked for active VAL-DSK-007 selection-state wording and blocked Trading/OpenFolder/ConnectProvider sources/actions.
- 2026-10-19 M6.2 closeout docs(plan): reconciled VAL-DSK-007 against live selection guarding, closed the bounded notice/toaster and injected-failure item, and recorded real owners for deferred ConnectProvider (M6.9), Trading request feedback (M6.10), guarded OpenFolder (M7.8), and the final discard policy (M6.12) in tracker, work items and architecture. No no-op action was added. Full serial workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1 on a cold target.
- 2026-10-19 M6.3 Part 1 fix(desktop): fenced tape prefix invalidation from the superseded chart during an interval selection; the strengthened GPUI switch test first failed with a cleared ordinal and passed after the fix. Full serial workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1 after cleaning the 31.2 GiB generated target. M6.3 stays unchecked pending the unified ChartBinding and exact-generation contract.
- 2026-10-19 M6.3 Part 2 fix(desktop): delivered exact-sequence pre-marker selection failures to the pane, restoring the previous chart instead of leaving a pending loading cover; tested interval retry deduplication, instrument rollback and retired failure interleaving in GPUI. Full serial workspace fmt/check/strict Clippy/tests/naming_check passed with CARGO_BUILD_JOBS=1 after cleaning a 30.5 GiB generated target. M6.3 remains unchecked pending the unified ChartBinding and exact-generation contract.
- 2026-10-19 M6.3 Part 3 fix(desktop): kept interval-only chart selection uncovered while retaining the instrument-change cover; failing-first GPUI interval regression and instrument countercase passed under --all-features. Documented why the earlier default-feature PaneHarness invocation fails and preserved fixture exclusion from production. Full gate evidence is recorded in the commit verification; M6.3 remains unchecked pending exact-generation binding and GUI storm.
- 2026-10-19 M6.3 Part 4 fix(desktop): post-marker replacement failure leaves the prior chart Ready with one sanitized selection notice; interval and repeated instrument cases pass in GPUI PaneHarness. M6.3 stays unchecked pending exact-generation acceptance migration, switch-guard deletion, interleavings and GUI storm.
- 2026-10-19 M6.3 Part 5 refactor(desktop): centralized exact pending-generation acceptance of selection commits and failures in ChartPaneController, removing the pane's raw pending sequence. Pure and GPUI retired-outcome checks pass. M6.3 stays unchecked pending remaining publication binding and GUI storm.
- 2026-10-19 M6.3 Part 6 fix(desktop): fenced queued initial instrument descriptor replies against later pane selection requests; pure and GPUI same-instrument retired-descriptor regressions pass. Full gate results are recorded with the commit. M6.3 remains unchecked pending all other pane publications, switch-guard removal, seeded interleavings and guarded GUI storm.
- 2026-10-19 M6.3 Part 7 fix(desktop): fence market-session statuses at the pane's committed selection; runtime and GPUI regressions cover pending and retired statuses. Full gate results recorded with this commit; M6.3 remains unchecked pending other pane publications and guarded GUI evidence.
- 2026-10-19 M6.3 Part 9 fix(desktop): the pane now fences order-book frames before ladder mutation; a failing-first GPUI test proved an early replacement frame previously overwrote the displayed book, and covers a higher-revision retired frame after selection commit. Full serial gate results accompany this commit; M6.3 remains unchecked pending state, bars, auxiliary messages, switch-guard removal, interleavings and guarded GUI storms. CARGO_BUILD_JOBS=1 was used for the cold rebuild.
- 2026-10-19 M6.3 Part 10 fix(desktop): selection changes no longer reject a current tape just because the previous provider session had a higher epoch; a failing-first GPUI interval-switch test covers the lower-generation replacement. Full gate results accompany the commit. M6.3 remains unchecked pending state and bar fencing, auxiliary messages, switch-guard removal, interleavings and GUI storm.
- 2026-10-19 M6.3 Part 11 fix(desktop): stamped runtime bar snapshots and updates with the exact pane selection and rejected retired data before any presentation mutation; the new GPUI regression initially failed to compile without selection stamping and now passes with all 29 pane tests. One existing order-flow fixture needed the committed selection stamp; the first full suite caught it, and the corrected full workspace test suite, fmt, check, strict Clippy and naming check passed. A 31.7 GiB target was cleaned, and CARGO_BUILD_JOBS=1 avoided the documented cold MSVC linker issue. M6.3 remains unchecked pending ChartState and auxiliary fences, switch-guard removal, interleavings and GUI storms.
- 2026-10-19 M6.3 Part 12 fix(desktop): exact-selection runtime series-readiness and retryable demand-error states pass the pane fence; the new GPUI regression covers early replacement, retired and accepted committed state. The first full suite exposed three existing state tests filtering only the old message variant; their drain helper now asserts the runtime selection is stamped. Cold target above 25 GiB was cleaned (twice during the gate); CARGO_BUILD_JOBS=1 was used. M6.3 remains unchecked pending the remaining state sources, auxiliary messages, legacy switch-guard deletion, interleavings and GUI storms.
- 2026-10-19 M6.3 Part 14 fix(desktop): stamped series worker startup/loading and failure states with the active pane selection generation; non-series catalog/screen failures remain app-scoped. The failing-first mailbox regression, app-scoped countercase and GPUI retired-state test passed. Full serial workspace fmt/check/strict Clippy/tests/naming_check passed after fixing a Clippy function-length finding; the 34.5 GiB generated target was cleaned and CARGO_BUILD_JOBS=1 used for the cold rebuild. M6.3 remains unchecked pending pre-selection catalog pane and legacy State cleanup, auxiliary messages, switch guards, exhaustive interleavings and GUI storms.
- 2026-10-19 M6.3 Part 15 fix(desktop): fenced demand-bound study output, invalidation and removal to the committed pane selection; pure and GPUI retired-study-removal tests passed. Full serial workspace fmt/check/strict Clippy/tests/naming_check passed without CARGO_BUILD_JOBS override. M6.3 stays unchecked pending catalog/legacy State cleanup, other auxiliary messages, switch guards, exhaustive interleavings and guarded GUI storms.
- 2026-10-19 M6.3 Part 16 fix(desktop): non-series catalog and screen failures no longer enter the chart-state handler; they show a sanitized, keyed catalog notice on panes and remain visible in the screener and market summary. Pre-selection and late-failure GPUI regressions, the runtime mailbox case and all five serial workspace gates passed without a jobs override. M6.3 remains unchecked pending legacy fixture State cleanup, auxiliary fencing, switch-guard removal, interleavings and GUI storms.
- 2026-10-19 M6.3 Part 17 fix(desktop): removed the legacy unfenced chart-state message and migrated pane and mailbox fixtures to selection-stamped states; the interval failure regression rejects a retired state before restoring on the committed failure. All five serial workspace gates passed with CARGO_BUILD_JOBS=1 after cleaning 29.0 GiB generated artifacts. M6.3 remains unchecked pending auxiliary fences, guard deletion, interleavings and GUI storms.
- 2026-10-19 M6.3 Part 18 fix(desktop): recovery replies carry the originating pane selection generation and cannot fail or install into a replacement chart; the GPUI retired-recovery test and all five serial workspace gates passed with CARGO_BUILD_JOBS=1 after cleaning 30.3 GiB generated artifacts. M6.3 remains unchecked pending other auxiliary fences, switch guards, interleavings and guarded GUI storms.
- 2026-10-19 M6.3 Part 19 fix(desktop): remove the never-produced, unfenced viewport reply and pane mutation; keep generation-stamped viewport commands and initialization restore. All five workspace gates passed with CARGO_BUILD_JOBS=1 after cleaning 26.4 GiB. M6.3 remains unchecked pending auxiliary audit, switch guards, interleavings and guarded GUI storms.
- 2026-10-19 M6.3 Part 20 fix(desktop): reject failed pending selection without advancing the displayed generation; the prior chart accepts later session publications and one deduplicated failure notice remains. Pure and GPUI regressions plus all five serial workspace gates passed after cleaning 26.2 GiB with CARGO_BUILD_JOBS=1. M6.3 remains unchecked pending auxiliary audit, switch-guard removal, exhaustive interleavings and guarded GUI storms.
- 2026-10-19 M6.3 Part 21 fix(desktop): protect pending selections from old-series errors; failing-first GPUI and pure controller tests plus all five serial workspace gates passed after cleaning 28.1 GiB with CARGO_BUILD_JOBS=1. M6.3 remains unchecked pending auxiliary audit, switch guards, interleavings, cover-through-ready and guarded GUI storms.
- 2026-10-19 M6.3 Part 22 fix(desktop): record catalog chart selection at dispatch and reject stale lifecycle during catalog resolution; GPUI regression and serial full gates recorded with this commit. M6.3 remains unchecked pending auxiliary audit, switch guards, fixed permutations, 100+ interleavings, cover-through-ready and guarded GUI storms.
- 2026-10-19 M6.3 Part 23 fix(desktop): old-series recovery results now use the lifecycle acceptance fence while a replacement is pending; the failing-first GPUI regression and all five serial workspace gates passed after cleaning 29.0 GiB with CARGO_BUILD_JOBS=1. M6.3 remains unchecked pending the rest of the auxiliary audit, switch guards, exhaustive interleavings, cover-through-ready and guarded GUI storms.
- 2026-10-19 M6.3 Part 24 docs(desktop): audited every market-worker mailbox variant delivered to the chart pane and recorded its exact generation, lifecycle, pending-selection, consumer-scoped or app-scoped acceptance above. No new unfenced selection-bound route was found. M6.3 remains unchecked; Part 25 removes legacy switch guards, then Part 26 exercises fixed permutations and seeded interleavings, Part 27 verifies cover-through-ready and Part 28 runs isolated GUI storms.
- 2026-10-20 M6.3 Part 25b1 refactor(desktop): removed duplicate legacy-flag acceptance from both selection commit and rejection, with a GPUI controller-authority regression; the remaining legacy switch state, marker variant and pending targets still need removal before Part 26.
- 2026-10-20 M6.3 Part 25b2a refactor(desktop): consolidated pending chart target identity in ChartPaneController, removed duplicate pending interval/product fields and covered exact-sequence target ownership; legacy switch phase and mailbox marker remain for Part 25b2b before Part 26.
- 2026-10-20 M6.3 Part 25b2b refactor(desktop): removed legacy switch phase and marker variant; chart presentation gates now follow the controller's installed-versus-committed selection generation. Focused GPUI pane regressions passed (39 tests); full workspace gate and hunk-isolated commit evidence recorded with this part. M6.3 remains unchecked pending Parts 26-28.
- 2026-10-20 M6.3 Part 25c fix(desktop): restored the self-contained committed terminal-view drawing-toolbar expression in 260b9415 while retaining the maintainer's expression uncommitted; five workspace gates and an isolated HEAD desktop check passed with CARGO_BUILD_JOBS=1. The 17-file maintainer baseline remained intact after the verified push.
- 2026-10-20 M6.3 Part 26 test(desktop): all 120 controller event permutations, all six GPUI snapshot/marker orders and 128 seeded 256-step interleavings passed. M6.3 stays unchecked for Parts 27-28.
- 2026-10-20 M6.3 Part 27 fix(desktop): failing-first GPUI tests proved either readiness or covering history could uncover a changed instrument prematurely; the pane retains the neutral cover until both arrive. M6.3 stays unchecked for Part 28 guarded GUI storms.
- 2026-10-20 M6.3 Part 28a test(desktop): six rapid interval requests ending on 3D retain the exact last pending generation under late superseded replies and publications. The GPUI regression passes; the initial 5m GUI outcome remains unexplained until corrected, dynamically located UIA actions are verified. M6.3 remains unchecked for Part 28b's literal three GUI clauses.
- 2026-10-20 UI feat(desktop): typography and chrome polish with no stabilization item. Inter 4.1 Regular/Medium/Bold replace the single synthesized Inter face; Geist Mono (`platform_mono_font_family()`) renders numbers in the order book, time and sales, bottom-panel and screener cells; market panels move from the header to a right icon sidebar; the bottom panel's tabs move into its open body; tooltips become trigger-anchored secondary-surface pills with top/bottom/left/right placement and a spring fade, replacing GPUI's cursor-positioned tooltip. Backfilled the 35 "closed in this commit" placeholders in sections 3 and 5 with the commits that introduced them. On an isolated worktree holding only this change, fmt, Clippy and naming_check passed and every test passed except three naming_check self-tests that read CRLF-checked-out files there; they pass on the LF main tree (120/120). M6.3 and M6.4 stay unchecked.
- 2026-10-20 M2.7 follow-up fix(market): trade-built session, week and month bars now reproduce their provider's bar timestamp. Each provider declares a `CalendarBarStamp` with its registration (Hyperliquid and the former Level 1 broker stamp 00:00 UTC of the trading date; Rithmic and the former FX/CFD broker stamp the session open), and the trade model buckets the seeded bar by that stamp, so a US equity's midnight-stamped daily or weekly bar takes its own trading date's trades instead of opening a duplicate bar. Generic calendar code stays provider-neutral. The `midnight_utc_equity_bars_take_their_trading_date_trades` regression and the full workspace gates passed.
- 2026-10-20 UI feat(desktop): chrome visibility and watchlist precision with no stabilization item. The drawing tools sidebar is a saved "Drawing toolbar" switch in the platform menu (the collapse and expander controls are removed), and the "Quick trade buttons" chart setting shows or hides the Buy / quantity / Sell bar on every chart; both persist in the chart chrome preferences and default to shown. Watchlist prices use the instrument tick precision, color only the changed digits, and ignore sub-grid prints for tick direction. Preference round-trip and watchlist precision tests passed with the full workspace gates.
- 2026-10-20 M6.4 Part 1 fix(desktop): rejected saved appearance and order-flow fields post one restore notice that persists across chart rebuilds until the trader edits them. The failing-first `pane_harness_invalid_saved_presentation_reports_one_notice_for_multiple_fields` GPUI regression now passes and covers partial and full recovery; fmt, Clippy, workspace tests and naming_check passed. M6.4 stays unchecked.
- 2026-10-20 UI style(design-system): no stabilization item. `--font-weight-normal` is now 400, so normal platform text (the chart legend included) renders Inter Regular as it did before the Medium face was bundled; emphasis and strong text stay Bold 700. The website mirror changed in the same task.
- 2026-10-20 UI chore(deps): no stabilization item. Pinned Aeris Charts to 584edc22 (footprint style options and per-cluster summary, larger trading markers with positions above coincident orders, general-chart legend and histograms). That revision moved to Rust 1.99.1 and Zed GPUI 56934ec0, so the workspace toolchain, the GPUI pins and `third_party/gpui_base` follow to keep one GPUI in the lockfile, and naming_check approves the new revision. The Terminal ignores the general-chart `GeneralLegendChanged` event like `GeneralBrushChanged`. The new footprint style options are not yet exposed in Terminal settings. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-20 UI revert(market): no stabilization item. Reverted the Level 1 broker Screener (641605f6): every screen request returned HTTP 403 on an unfunded account, and that broker documents `/market-data/by-type` as available only to funded account holders. The REST screen, its adapter client, the `ProviderMarketKind`/day-volume contract fields and the desktop provider tabs are removed. Only the table rounding stays: the header and last row round themselves because GPUI clips children to a rectangle, so square cells no longer overflow the table's rounded corners. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-20 M6.4 Part 2 fix(desktop): after the maintainer reverted the unavailable broker screen at 6a067c7c, reapplied the previously interrupted unified chart-surface setters on a clean main tree. Kept M6.3 GUI Part 28b deferred until the guarded milestone-end pass, and kept M6.4 unchecked until its presentation/persistence contract is complete.
- 2026-10-20 M6.4 Part 3 test(desktop): verified actual chart style, two nondefault series colors and logarithmic scale across a committed instrument selection and proto round-trip/reopen; no behavior change was needed for those properties. Full presentation ownership remains M6.4 work, so do not check the item or claim VAL-DSK-016 complete.
- 2026-10-20 M6.4 Part 4a fix(desktop): corrected the pending-chart save dropping pane-owned visibility, context and link edits; shared its capture with loaded-chart persistence and extended the combined restore-failure notice test. The pending-chart regression failed first with the old visibility and now covers protobuf reopen. M6.4 remains unchecked until full presentation ownership and every-field round trip, followed by the guarded GUI pass.
- 2026-10-20 M6.4 Part 4b refactor(desktop): moved chart-setting application into a transient PanePresentation snapshot of the saved document plus pane-owned settings; the new GPUI regression checks nondefault chart preferences and study visibility through loading, protobuf decoding and a loaded chart. M6.4 remains unchecked pending guarded VAL-DSK-017 GUI evidence; a maintainer-owned release desktop is running. Cleaned only Cargo's dev profile after target reached 31.63 GiB, never the release profile.
- 2026-10-20 M6.4 code closeout / M6.5 Part 1 fix(desktop): marked M6.4 code-complete with literal VAL-DSK-017 GUI evidence explicitly unvalidated pending the guarded final pass; fenced initial and replacement selection replies at the bridge and covered duplicate/retired outcomes plus one-command/no-follow-up pane dispatch. M6.5 remains unchecked for restored-pane identity and legacy API removal. Cleaned only Cargo's dev profile after target reached 28.14 GiB.
- 2026-10-20 M6.5 Part 2 fix(desktop): separated saved workspace provenance from the provisional fresh default so a restored BTC pane no longer becomes `Default`; four saved panes carry exact `InstrumentRef` identity into the existing initial runtime selection. Fresh and explicit newly added pane paths remain distinct. The literal four-pane GUI assertion and instrumented restored-pane command/reply count are not yet validated. Removed only Cargo's dev profile after target reached 31.64 GiB; never touched release.
- 2026-10-20 UI fix(desktop): no stabilization item. The panel sidebar's watchlist, order book, time and sales and context toggles now use the header's text-toggle style (`text-interactive` while off, `text-active` while on, no selected fill), so their icons no longer look active while the panel is closed. Verified in an isolated worktree at 821f8f76: fmt, desktop Clippy, desktop tests and naming_check passed; the workspace-wide gates were not rerun because the mission's uncommitted work shares the checkout.
- 2026-10-20 M6.5 Part 3 refactor(desktop): removed legacy `try_select_engine` references from desktop production and tests, keeping the single runtime-owned instrument select-and-demand operation and its existing exact-reply fence. The first full suite had one isolated-pass persistence test failure; after a dev-only clean the second full suite and all other gates passed. Restored-pane command/reply instrumentation and guarded GUI restart remain before M6.5 closure.
- 2026-10-20 M6.5 parallel-test isolation test(desktop): added a per-process atomic sequence to the timestamped temporary workspace root and covered concurrent path generation. Twenty simultaneous default-parallel desktop persistence test runs passed. The earlier one-off empty-template failure is consistent with a shared fixture root; M6.5 remains unchecked pending four-pane restored selection counts and GUI restart.
- 2026-10-20 M6.5 Part 4 test(desktop): instrumented the production initial select-and-demand request builder in the restored four-pane endpoint test; four exact saved refs yield four requests and four applied replies despite duplicate outcomes. Focused regression and full workspace gates pass. PaneHarness and guarded four-pane GUI restart remain outstanding while a maintainer-owned release desktop runs; M6.5 remains unchecked.
- 2026-10-20 M6.5 Part 5 fix(desktop): four restored GPUI panes capture four exact endpoint select-and-demand dispatches, reject altered duplicate initial replies and retain distinct saved identities and intervals; the user-selection GPUI regression counts one command/no follow-up. VAL-DSK-020 GUI restart remains unvalidated while the maintainer's release desktop runs; only M6.5 code is checked.
- 2026-10-21 M6.6 Part 1 refactor(desktop): stopped in-place study, order-book and tape publication coalescing in the bounded GPUI mailbox; 10,000 ordered fixture publications survive frame-budgeted drains with explicit backpressure. All five workspace gates passed. M6.6 remains unchecked for full message collapse and VAL-DSK-021/022 completion.
- 2026-10-21 M6.6 Part 2 refactor(desktop): removed the diagnostic replacement/eviction scans from market mailbox enqueue; diagnostic snapshots preserve FIFO order with publication events, use the bounded control reserve, and report full backpressure instead of silent loss. The message/ChartState collapse, recovery expect and bridge discards remain for later M6.6 parts.
- 2026-10-21 M6.6 Part 3 fix(desktop): removed the recovery-demand `expect` and made recovery acknowledgement consumption follow successful bounded-mailbox delivery; a saturation regression verifies the pending request survives failed cancellation and failure-reply sends, then completes once capacity returns. The parallel message/ChartState/MarketGeneration collapse and bridge discards remain.
- 2026-10-21 M6.6 Part 4 fix(desktop): market session status no longer advances its delivered revision on mailbox backpressure, and retries on the next worker iteration after capacity returns. A bounded-mailbox regression covers failure, retry and nonduplicate delivery. M6.6 remains unchecked pending message collapse and VAL-DSK-021/022.
- 2026-10-21 UI feat(desktop): no stabilization item. Pinned Aeris Charts to 9bd919db (right-edge order-book depth bars and CVD candles). Order entry moved out of the order book into its own sidebar panel; saved workspaces that showed the ticket inside the order book open the new panel (local-state schema revision 2). The chart order-flow menu adds "Order book on chart (DOM)" and a CVD Line/Candles style, both saved with the pane. The chart's book shares the pane's single order-book demand with the docked panel, is fed only frames the pane accepted, and clears on product switch, recovery, disconnect and stop. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-21 UI fix(desktop): no stabilization item. Pinned Aeris Charts to 9f819dcb (depth profile ticks merge into rows at least one label tall at any zoom). A restored chart that opened the order book before its saved instrument resolved now keeps that intent and applies the depth stream once the selection is adopted, so the chart book receives full depth instead of staying on the top of book. A regression covers depth opened during restored-selection resolution. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-21 UI feat(desktop): no stabilization item. Pinned Aeris Charts to 7dbd7ad9 (CVD reset and colors, delta colors, in-place depth profile options). CVD, Delta and Order Book on chart each open their own settings window from the legend gear (CVD style, reset and colors; delta colors; order-book levels, width, opacity, sizes and colors); the chart Settings menu no longer carries those controls. The options persist with the pane in new optional workspace fields, and older workspaces restore the defaults. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-21 UI fix(market): no stabilization item. Restored charts no longer stay on "Connecting": a provider whose catalog is not bound to a session (Hyperliquid, declared by the new `catalog_session_binding` capability) has its resolved instrument joined to the current session, so several restored panes resolving at once no longer fail as stale after the first opens the session. The symbol browser accepts the empty query again, so each provider's default listing loads instead of "Symbol search unavailable". A study that misses its execution deadline no longer fails the executor permanently: the executor recovers once the abandoned job finishes, and the desktop retries reinitialization after 5 seconds instead of looping and flooding history. Order-book frames now carry the consumer's demand generation, the identity panes accept, instead of the catalog's own selection counter, so the order book fills instead of staying on "Loading order book". Regressions cover each. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-21 refactor(market): no stabilization item. Two broker integrations and the hosted-broker OAuth client were removed entirely: their adapter crates, vendored protocol files, market-service sessions, catalog channels, venue relay, examples, the demo probe, validation vault borrowing, assets, naming_check rules and AGENTS.md rules. The provider-neutral broker venue boundary in `trading_runtime` remains for a future broker route. Stored accounts and contract terms of a removed venue still load from existing trading stores, cannot trade, and are not offered in the Accounts panel. The AWS broker stacks, their retained table, encryption key and parameters were deleted. fmt, Clippy, workspace tests and naming_check passed.
- 2026-10-21 M6.6 Part 5b refactor(desktop): runtime series readiness retains the fenced coordinator envelope through the bounded pane queue and projects at the GPUI consumer after the exact selection/lifecycle fence. The GPUI regression exercises early, retired and current readiness. Bars, synthetic failures and remaining routes still use the old message shape; M6.6 and VAL-DSK-021/022 remain open.
- 2026-10-21 M6.6 Part 5c refactor(desktop): runtime snapshots/tail updates retain the fenced coordinator event through the bounded pane queue; GPUI projects canonical bars after exact selection acceptance. Bridge and pane regressions cover FIFO, pending/retired rejection, covering snapshot and tail. Legacy fixture/worker conversions, ChartState and other routes still require Parts 5d-8; M6.6 remains unchecked.
- 2026-10-21 M6.6 Part 5d1 refactor(desktop): coordinator-fenced series failures and demand errors reach the GPUI pane unchanged; the pane rejects pending/retired lifecycle generations, distinguishes older-history diagnostics and retryable recovery, and retains failed-replacement restoration. Focused bridge and GPUI regressions pass. Part 5d2 must remove synthetic worker readiness and old bar conversions, then ChartState; M6.6 stays unchecked.
- 2026-10-21 M6.6 Part 5d2a1 test(desktop): shared fixture coordinator-event builder and canonical disconnected snapshot migrate the pane's initial snapshot/readiness and the existing fenced examples. The pane's displayed worker identity now matches production's runtime projection; the fixture's disconnected label remains asserted at the source. Remaining 5d2a2-4 fixtures and 5d2b production deletion are outstanding.
- 2026-10-21 M6.6 Part 5d2a2 test(desktop): pane switch permutations, retired/early bar and status fences, replacement cover, recovery, presentation and redacted failure cases use canonical fenced coordinator events through the production GPUI projection; direct setter-reinstall fixture remains deliberate. M6.6 stays unchecked pending mailbox/diagnostic migration, old-route deletion and later parts.
- 2026-10-21 M6.6 Part 5d2a3 fix(desktop): mailbox order, frame-budget, explicit overflow, foreground/background and wake tests now exercise canonical fenced bars and readiness. Fixed the discovered mailbox classification gap so fenced bars share the market-publication capacity and suppression policy. Diagnostic fixtures and the production legacy-route deletion remain; M6.6 stays unchecked.
- 2026-10-21 M6.6 Part 5d2a4 test(desktop): the diagnostic 10,000-event burst and bounded endurance fixture now send and inspect canonical fenced coordinator snapshots while retaining their generation continuity, high-water, memory and checkpoint assertions. Part 5d2a is complete; production legacy deletion is next.
- 2026-10-21 M6.6 Part 5d2b refactor(desktop): removed superseded production bar and readiness conversion and its message consumers in one change; market-summary bars use the fenced coordinator event, local endpoint failures have their own generation-stamped message, and engine characterization runs against the fenced path. Recovery request fencing and replay converters remain. M6.6 stays unchecked pending ChartState relocation and Parts 6-8.
- 2026-10-21 M6.6 Part 5d2c refactor(desktop): ChartState and its labels/readiness gate now belong to ChartPaneController, not the market-worker mailbox; the existing pane projection and its exact selection fence stay intact. M6.6 remains unchecked pending Parts 6-8 and VAL-DSK-021/022.
- 2026-10-21 M6.3 fix(desktop): reverted the Part 3 interval-only exception; a timeframe switch again covers the previous series with the loading surface from request through covering snapshot and readiness, like an instrument switch, instead of leaving the old interval visible and popping the replacement in. The interval GPUI regression and both snapshot/readiness orderings now cover interval switches too.
- 2026-10-21 docs(plan): no stabilization item. Removed the last planned work for the two dropped brokers: the target `ConnectMethod` is `VaultCredentials { form } | Public` with no hosted-broker OAuth variant, M8.9's `hosted_broker` move (G2) is closed because the module was deleted, and the M6.9 provider-login risk no longer waits on a hosted broker. Neither broker returns without a new maintainer decision.
- 2026-10-21 docs(agents): no stabilization item. The outreach CRM gained a `Rejected by us` status; AGENTS.md now says a firm we drop gets that status, role not a target, fit not a fit and no next action. The two dropped brokers' deals were moved there, and the dropped broker's demo account, its orders, fills and contract terms were deleted from the maintainer's local trading store after a backup.
- 2026-10-21 fix(rithmic): no stabilization item. A made-up user ID and password got Rithmic `rp_code` 13 ("permission denied") on the production and test gateways, and Aeris reported that as "Rithmic accepted the user ID and password but denied access for Aeris". Code 13 says nothing about the credentials, so the separate permission-denied error and invalidation reason are removed; every refused login is one rejection whose message asks to check the user ID, password and system, then market-data access. A refused login was never saved. Restriction: do not infer credential validity from a Rithmic login refusal code.
- 2026-10-21 ci(macos): no stabilization item. The manual macOS preview build gained a `rithmic_kit` input so a Mac tester can get a build that speaks Rithmic. Only with that input does the hosted runner unpack the kit's `proto` folder from the `AERIS_RITHMIC_KIT_TAR_XZ_B64` repository secret (a base64 tar.xz); it deletes the kit right after the build, fails if the generated Rithmic schema is missing, and names the disk image `-rithmic`. Without the input the build stays kitless. Restriction (AGENTS.md, naming_check): this is the only hosted workflow that may read a secret or receive the kit, and only on manual dispatch.
- 2026-10-21 style(design): no stabilization item. Applied the Theme System's new order-book colors to `platform.css` and `CSS.md`: depth fills mix the primary (bids) and #ff2f5e (asks) into `surface`, price text beside the fills keeps `book-bid-text` / `book-ask-text`, and the new `book-fill-text` (black in light, white in dark) colors the quantity text drawn over a fill. The website mirror is deliberately left unchanged on the maintainer's instruction.
- 2026-10-21 style(design): no stabilization item. The platform order book now uses the website's order-book tokens: ask fills mix 60% #ff2f5e into `surface`, ask text is 75% #ff2f5e with black (light) and 85% with white (dark), and the quantity text over a depth fill uses its side's `book-bid-text` / `book-ask-text` again instead of `book-fill-text`. `platform.css`, `CSS.md` and the website mirror now carry the same order-book values.
- 2026-10-21 style(design): no stabilization item. Lighter order-book depth fills from the maintainer's updated theme: bid and ask fills mix 40% into `surface` in light and 35% in dark (were 60%). Text tokens are unchanged. Only the platform (`platform.css`, `CSS.md`) changes; the website mirror and the Theme System are deliberately left as they are.
- 2026-10-22 fix(rithmic): no stabilization item. A Mac tester logged in to Rithmic but got an empty symbol search and no order book. Causes fixed: a broad search that matched more instruments than its bound, or repeated a symbol with different details, failed the whole Rithmic session; the bounded collector now keeps the lowest exchange/symbol keys and the first copy of a repeated key. Rithmic searches now ask the provider for futures only (Rithmic answers `Future`, so the collector no longer echo-checks the type). Present-but-empty optional catalog fields decode as absent instead of failing the frame. A search rejected without a search-specific reason (for example an unavailable dispatch) was shown to the desktop as a selection rejection, so the search box waited forever; `MarketRuntimeEvent::ProviderCatalogRejected` now carries the command lane, and the adapter's superseded-search rejection is a selection rejection. The Rithmic worker rejects its pending searches and selections on every live-session exit and rejects catalog commands while it waits for a replacement session. The coordinator gives every search and selection a 45-second response deadline and answers an overdue one with `SearchTimedOut` / `SelectionTimedOut`. A full or closed pane mailbox no longer stops the workspace market worker when a catalog result or selection session status cannot be delivered. Catalog session failures and invalid search responses are now logged. Not verified live: the futures filter matches the provider client's documented Rithmic Test usage, but no live search ran in this change. Restriction: catalog rejections carry their command lane explicitly; consumers must not infer search versus selection from the rejection reason.
- 2026-10-22 fix(market): no stabilization item. Rithmic Test reports from the maintainer and a Mac tester: empty symbol menu, "Symbol search unavailable", "Market selection failed", a chart stuck on Connecting and an order book stuck awaiting its snapshot; Hyperliquid BTC also kept showing "Footprint needs a published price increment". Causes fixed: (1) Rithmic Test sends order-level snapshots with no sequence on any level or on the completion; the assembler now installs that image, replays updates that arrived after its first level in arrival order, and checks continuity from the first live sequence instead of requesting resnapshots until depth was disabled. (2) Rithmic labels its sessions with its own wire generations, but the coordinator began a lifecycle session at the registry epoch number, which another provider had already advanced; history then asked for the epoch number and every completion was fenced as retired ("provider instrument session is stale"). For an adapter that labels its sessions, the epoch now binds to the current wire label. (3) Rithmic catalog callbacks were routed by the consumer's own generation, so two consumers (or a search and a selection) using the same number crossed results; `CatalogCommands` in `rithmic_realtime` now mints one unique wire generation per command, routes each callback and rejection lane by that wire id, and keeps the 8 most recent completed searches (`MAXIMUM_RITHMIC_RETAINED_SEARCHES`, shared with the adapter) so a selection may name a recent search; queries are upper-cased for Rithmic's exact match, results are ordered front month first, and an undispatched command is logged with its cause. (4) An empty desktop search box sent an empty query, which Rithmic cannot search; it now searches the provider's default listing. A leftover startup auto-select that could switch the chart to MNQ after a provider change is removed. (5) The fresh-workspace Hyperliquid default is installed as a placeholder numbered session 1, selection 1, and the provider's first verified resolution carries the same numbers, so it was rejected as a conflicting selection and BTC never received its price increment or contract terms; the coordinator now lets the provider's resolution replace only that provisional default, and the chart pane adopts verified terms for the instrument it charts (the worker and pane share `verified_terms_supersede`). Catalog rejections, selection install failures and failed selections are now logged with their lane, provider and reason. Verified live on Rithmic Test and the public Hyperliquid feed with a temporary probe (removed): Hyperliquid BTC installed terms with its price increment, Rithmic MNQZ6 and then NQZ6 each loaded 1,600 bars and went live with depth, two consumers reusing one search generation each received their own result, and no catalog command was rejected. Not verified: the desktop UI against the live feed, and the macOS build.
- 2026-10-22 fix(chart): no stabilization item. The desktop log showed "bounded chart queue overflowed; canonical resnapshot required" and "market worker invalidated the stream" every 40 to 90 seconds. Only a drawn chart frame drained the 64-slot chart bridge, but every pane of the active workspace is polled each frame whether or not its chart is on screen (a pane behind a maximized one, or any pane while the screener page is open), so the queue filled, was discarded and forced a full resnapshot of every bar. `AerisChartView::try_queue_replay_update` now applies a full bridge to the engine before queueing the next update, so an undrawn chart stays current with no overflow or resnapshot; the bridge keeps its bounded overflow contract. Regression: `undrawn_chart_applies_a_full_bridge_instead_of_requesting_a_resnapshot`.
- 2026-10-22 fix(desktop): no stabilization item. A restored study on a Rithmic MNQZ6 chart showed "Study unavailable" and the desktop log recorded it about 165 times a second until the pane switched provider. The desktop series identity check demanded a `rithmic-test:` entitlement prefix, but a catalog selection installs the provider's registered selection entitlement, so every selected Rithmic market failed to form a series key and its studies (and pane recovery requests) could not be queued; the check now requires only a non-empty entitlement, which the runtime owns. The failed deferred registration was also retried on every market poll, each failure posting the notice and requesting another frame. Deferred registrations and automatic reinitializations that cannot be queued now wait 5 seconds (`STUDY_RETRY_DELAY`) before the same series is tried again, a changed series is tried at once, and each failure is reported and logged with its reason once. Regressions: `rithmic_series_keys_accept_the_selected_market_entitlement`, `unqueued_study_work_reports_once_and_waits_before_retrying_the_same_series`, `pane_harness_deferred_study_registers_on_a_selected_rithmic_market`, `pane_harness_unqueueable_deferred_study_reports_once_instead_of_every_poll`.
- 2026-10-22 fix(rithmic): no stabilization item. Selecting a Rithmic market at 16:54 CT (CME pre-open) ended the new session two seconds after login with `MalformedMessage`, so the selection failed with "Market selection unavailable". The canonical market model rejects crossed or locked books, non-positive prices and zero quantities, and the Rithmic session turned any such frame into a terminal failure of the whole session; a pre-open book is legitimately crossed, spreads trade at or below zero, and a zero-size print or quote side carries no liquidity. Such trades, quotes and order-level images are now withheld from publication (logged once per instrument and condition per session) while the session and its order-level image stay live, and the next representable frame publishes; a quote side with zero size publishes as absent. Price precision mismatches and other invalid frames still fail the session. A session that ends while handling a provider message now logs the message kind (`RithmicSessionMessage::kind`, also used by the Rithmic Test smoke tool). The pre-open cause is inferred from the failure time and the code path; it was not reproduced live. Regressions: `crossed_pre_open_quote_is_withheld_and_the_next_quote_publishes`, `zero_size_and_non_positive_frames_do_not_fail_the_session`, `crossed_order_level_image_is_withheld_until_the_book_uncrosses`.
- 2026-10-22 fix(market): no stabilization item. Scrolling a Hyperliquid builder chart (xyz:SPCX) back into history logged 18 "history attempt 1 failed ... HTTP 429" in 29 seconds, then "older history is unavailable". A failed backfill retry was keyed to its range, and every viewport move produced a new range that bypassed the retry deadline and reset the attempt count, so each scroll step sent another request into the provider's rate limit; the log always said attempt 1 because it read the count only from a lane waiting to retry. A failed history lane now holds every range until its retry deadline: a moved viewport retargets the pending retry to the newest range, failed attempts count across ranges until one succeeds, and the exhausted 30-second cooldown also holds new ranges. The log reports the real attempt number. Regressions: `backfill_retry_deadline_holds_moving_viewport_ranges_and_keeps_attempts` and `exhausted_ranged_history_cooldown_holds_every_range_until_it_expires`, which replaces `exhausted_ranged_history_retry_cools_down_exact_range_but_not_new_range` (it asserted the bypass).
- 2026-10-22 fix(hyperliquid): no stabilization item. The same HTTP 429 storm had a second cause: every Hyperliquid info request (the market screen's two context requests every 5 seconds, the catalog bundle with one request per builder dex, and each chart's history pages) went out independently, so together they could exceed the provider's 1,200 weight per minute per address and every chart's history was refused. All info requests now spend from one process-wide sliding-window budget in `hyperliquid_market/src/budget.rs`: 1,000 weight per 60 seconds, 20 per request plus one per 60 returned candles. A request waits for capacity for at most 10 seconds (history waits observe cancellation) and otherwise fails as rate limited without being sent; an HTTP 429 marks the window full, because another program on the address may have spent it. Restriction (AGENTS.md): never send a Hyperliquid info request around this budget. Regressions: `a_full_window_waits_for_its_oldest_spend_to_expire`, `candle_pages_and_provider_rejections_count_against_the_window`, `waiting_ends_on_cancellation_or_when_the_wait_exceeds_its_bound`. Not verified live against the provider's limit.
- 2026-10-22 fix(rithmic): no stabilization item. Scrolling a Rithmic MNQZ6 chart back logged ten "rithmic history attempt 1 failed ... Transport" while the covering history loaded, and a 100-tick chart could not rebuild its open bundle. Every replay failure was reported as `Transport`, which hid four causes found with a live probe on Rithmic Test: (1) Rithmic stamps a second or minute bar with the time its period closes, and a minute replay returns every bar whose period overlaps the requested seconds, so the last bar of a backfill closed one period after the finish and the collector refused it as outside the range; (2) a one-trade tick bar repeats its only key, and tick sequence text is not ordered ("14" then "13"), so the collector called valid bars non-monotonic; (3) a replay over a window with no bars ends with `rp_code` 7 "no data", which was treated as a refusal; (4) every minute and second bar was labelled one period late (its close time instead of its open time), so history disagreed with the live bucket and the open-bar split. The collector now accepts a minute bar closing up to one period after the finish, orders tick keys by time only and drops the repeated key, and code 7 "no data" completes an empty replay. `canonical_rithmic_time_bar` labels and sequences second and minute bars by open time, and one `collect_time_bars` pads each replay by one period and keeps exactly the bars that open in the requested half-open window, for both the chart history path and the capability adapter (a second replay selects bars by close marker, a minute replay by period overlap). Replay errors keep their cause (`Session`, `Rejected`, `ForcedLogout`, `Collection`). Verified live on Rithmic Test (MNQZ6): 1-minute bar volumes equal the trades inside [open, open + 60s), a 6-hour backfill ends exactly one minute before the oldest installed bar, and 5-second, 1-hour and 100-tick charts (with a 36-trade open bundle) load. Regressions: `minute_replay_accepts_the_bar_closing_one_period_after_the_finish`, `tick_history_orders_trades_by_time_not_by_opaque_sequence_text`, `a_replay_over_an_empty_window_completes_while_other_code_seven_refuses`, `chart_history_labels_bars_by_open_time_and_trims_the_replay_padding`, `chart_history_reports_why_the_replay_failed`. Daily, weekly and monthly Rithmic charts still load no bars (the daily replay returns nothing for Unix-second indices); that is a separate fix.
- 2026-10-22 fix(rithmic): no stabilization item. Rithmic 1D, 3D, 1W and 1M charts loaded no history, and a 1W or 1M chart failed outright because its 1,600-bar request needed more daily bars than one replay holds. A live probe on Rithmic Test (MNQZ6) showed that a daily replay is indexed by YYYYMMDD trading date, not Unix seconds; it returns one-day bars marked by their CME trading date with period text `daily`, ignores any period other than 1, returns no weekly bars, and leaves out the trading date still open (minute-bar sums confirmed a date covers 17:00 CT the evening before to 16:00 CT). Every session interval now replays daily period 1 with YYYYMMDD start and finish indices, labels each date by its session open (the new `ExchangeCalendar::trading_day_open`, as live session bars are stamped), and groups dates locally into 3-day buckets counted from the epoch (the new `ExchangeCalendarPeriod::Days`, matching live `day_bucket`), Monday weeks or months. The current replay runs to now: `collect_rithmic_session_history` folds the open trading date from its minute bars and returns the bucket holding it as the forming bar, so today's candle carries the trades from before the chart opened. A replay window keeps its newest 10,000 trading dates, the collector refuses a daily or weekly period other than 1, and `RithmicTimeBarResolution` no longer accepts daily bars as a fixed 86,400-second interval. Verified live on Rithmic Test: 1D returns the closed dates 19, 20 and 21 October plus a forming 22 October bar folded from minute bars, 3D groups 19 to 21 October with the forming 22 to 24 October bucket, and 1W and 1M load (Rithmic Test serves MNQZ6 daily bars only from 19 October). Regressions: `daily_replay_is_one_day_bars_indexed_by_trading_date`, `session_history_replays_trading_dates_and_labels_bars_by_session_open`, `session_history_folds_the_open_trading_date_from_minute_bars`, `session_replay_runs_to_now_within_one_daily_replay`, `cme_trading_day_open_is_the_previous_evening_across_dst`, `day_buckets_group_trading_dates_from_the_epoch`.
- 2026-10-22 feat(rithmic): no stabilization item. The Rithmic order plant can place server-held brackets, the first step of Rithmic order routing. One bracket order (template 330) sends the entry with up to four static target and four static stop levels in ticks, so Rithmic holds the exits and they survive a logout or a crash; the levels on each side must cover the entry quantity with distinct positive ticks, and a bracket with neither side is refused (`ProtocolError::InvalidBracket`). Also added: target and stop level updates addressed by the current level (332, 334), bracket and stop snapshots (338, 340), the bracket update stream (336 with pushed 353 updates), and exit position (3504). Order notifications now carry the bracket link (original basket id and bracket type). OCO (328) and link orders (344) stay off the outbound allowlist. The order session replaces its per-request flags with one outstanding request per reply lane (`ExclusiveRequests`): target and stop level updates share a lane because Rithmic rejects concurrent exit modifications, and fill history shares the execution replay lane. `rithmic_test_smoke --order-bracket-smoke <product>` is the live check: it refuses unless the vault login is the Rithmic Test system and the first account has no working orders and no open position, buys one front-month contract at market with a target and a stop 40 ticks away, logs out, requires both exits still working from a fresh session, then cancels all, exits any open position and checks the account is flat. Passed live on Rithmic Test with ESZ6 on 2026-10-22: the entry filled, Rithmic placed the limit target and the stop, both were still working after the logout, and cancel all plus exit position left the account flat. The maintainer's Rithmic Test account rejects MNQ at Rithmic's risk system ("Product code not permissioned"), which is why the check takes a product. The bracket and stop snapshots (338, 340) returned no rows once the entry had filled, so their row decoding is covered only by tests. The live runs found four defects, now fixed: an account P&L row with empty `margin_balance` text failed the P&L session (empty optional decimal text is now absent, not malformed); a negative `sell_qty` while long one contract with no working sells failed it too (`buy_quantity` and `sell_quantity` are now signed exposure, not counts); cancel all was refused with code 1041 "User Type field is not received" (`CancelAllOrdersRequest` now carries the user type); and the order plant's template 358 push (account risk limits, sent right after a position exit) ended the order session as an unsupported template (it is now recognized and dropped). After a reconnect, the order snapshot reports orders that reached the exchange by their exchange notification, so the check reads both notification kinds. Nothing outside the adapter uses brackets: the futures venue contract and the Rithmic venue relay in `market_runtime` are the next steps, and M8.10's "keep the unused order plant out of the default build" bullet no longer applies once that relay lands. Regressions: `bracket_orders_send_static_levels_with_the_entry`, `bracket_levels_must_cover_the_entry_with_distinct_ticks`, `level_updates_address_the_current_level_and_exits_name_the_instrument`, `bracket_replies_decode_acknowledgements_and_rows`, `bracket_terminal_replies_complete_their_requests`, `pushed_bracket_updates_and_exit_acknowledgements_decode`, `order_notifications_carry_bracket_links`, `exclusive_requests_allow_one_request_per_lane`, `bracket_stream_and_commands_keep_one_reply_per_lane_over_tls`, `cancel_all_names_the_user_and_risk_pushes_are_dropped`, `empty_account_pnl_text_is_absent`, and a negative-exposure case in `instrument_pnl_keeps_provider_scale_for_text_and_double_fields`.
- 2026-10-22 feat(trading): no stabilization item. Broker accounts are now netted futures accounts with one position model, replacing the hedged broker contract (one position per broker position id, close by id, per-position stop loss and take profit, broker swap and commission, unrealized-P&L polling) on the maintainer's decision. Broker fills run through the same `apply_fill` ledger as practice fills (position, exact cost basis, trade cycle, fill accounting, risk and discipline state); `BrokerPosition` and `TradingSnapshot.broker_positions` are removed. The venue contract in `aeris_trading::venue` now carries a `ServerBracket` on the entry order (up to `MAXIMUM_BRACKET_LEVELS`, 4, target and stop levels per side, each side covering the entry quantity with distinct positive ticks), complete-terms amendments, `MoveBracketLevel`, `CancelAll` and `ExitPosition` requests, a signed `VenuePosition` carrying the venue's open P&L, and `BracketLevelMoved` updates. A relay declares `VenueCapabilities` (held bracket levels and client order id length) when it attaches; admission refuses a client order id longer than the venue accepts and a bracket with more targets than the venue holds. A bracket on a broker account is one entry with a server-held bracket: the template's targets split exactly as on a practice account and one stop covers the whole quantity; managed trailing and break-even stops are refused there. Bracket levels persist with the entry order, move only when the venue confirms, and end with the position. Orders Aeris did not place (released bracket exits, orders from other platforms) are mirrored on first report. An account flatten sends `CancelAll` and one `ExitPosition` per ledger position, an instrument flatten cancels that instrument's orders and exits it, and the kill switch does the same after locking; every exit is reported as a request until the venue's fills arrive. A venue position report sets only the ledger position's open P&L in memory; a reconcile snapshot that disagrees with the ledger rebases the position to the venue's net quantity and average price in one transaction, marking that trade cycle's P&L unknown. Broker accounts write no cash-ledger entries; their balance stays the venue's. V28 drops `broker_positions` and `broker_deals.broker_position_id` and adds `broker_bracket_levels`; old hedged fills keep their rows. Desktop: the positions tab lists broker positions from the shared ledger (dated by their open trade, open P&L only while the account is reachable) and its broker Close is an instrument flatten; the DOM position marker, watchlist activity and chart close read the same ledger, and the shared DOM marker now rescales the average entry exactly to the book's price grid (an average finer than the grid shows no marker). TR-12's two-position-model part is resolved; its order-plant part stays with M8.10. Restriction (AGENTS.md): a broker venue declares its capabilities when attached; V28 is in the schema line. Not done: no venue relay exists yet (the Rithmic relay in `market_runtime` is next), so none of this ran against a broker; broker bracket levels are not shown in the desktop yet; a gated pre-group store that has not reached V28 loads no bracket levels. Regressions: `a_broker_bracket_sends_server_levels_at_the_template_distances`, `bracket_levels_move_when_the_venue_confirms_and_end_with_the_position`, `a_broker_flatten_exits_every_ledger_position_and_waits_for_the_fills`, `reconcile_settles_every_open_order_and_rebases_positions_the_venue_disagrees_with`, `connected_broker_positions_carry_the_venue_open_pnl`, `the_kill_switch_reports_venue_requests_it_could_not_make_after_locking`, `broker_brackets_are_venue_held_and_managed_rules_reject_without_state`, `route_capabilities_resolve_day_and_protection_policy`, `flatten_feedback_reports_broker_exits_as_requests_and_partial_work_as_errors`, `positions_list_practice_and_broker_accounts_together`. Trading runtime 283 passed (7 ignored); workspace 2,307 passed, 0 failed (20 ignored); fmt, strict Clippy with and without the Rithmic kit, and naming_check green.
- 2026-10-22 feat(rithmic): no stabilization item. The Rithmic venue relay connects the trading owner's broker accounts to Rithmic. `market_runtime::rithmic_venue` runs on one `aeris-broker-venue` worker started by `MarketService::attach_broker_venue`; the desktop passes it the trading owner's attach and inbox handles and a failed attach is only logged. Each session reads the saved vault login, refuses a live login before opening an order plant (and again from the gateway's login report), logs in to the order and P&L plants, reads the user type, accounts (at most `MAXIMUM_TRACKED_RITHMIC_ACCOUNTS`, only those with a known currency scale) and up to 256 trade routes, subscribes each account to order and P&L updates, then attaches as a new venue session generation with Rithmic's capabilities (four bracket levels, 64-byte client order ids) and reports every account. Requests become new or bracket orders on the account's default route for the exchange, complete-terms modifies, cancels, level moves, cancel all and exit position; a request whose Rithmic reply lane is busy waits (at most 64) and is retried in order. Replies are matched by user tag (new orders) or basket id (modify, cancel); a refusal without either goes to the oldest command of its kind, and at most 256 commands wait for a reply. A reconcile replays executions since the owner's last deal, then gathers the order and position snapshots into one `VenueSnapshot`; a snapshot that was refused, exceeded 1,024 rows or describes a working order or position incompletely is not reported, and a reconcile that does not finish in two minutes drops the session. Live order and exchange notifications become order states, fills carry a deal id of basket and fill id so a replayed fill is recorded once, P&L rows become positions and balances. A plant silent for three heartbeat intervals, a forced logout, a closed request queue or an owner that stops accepting events ends the session; the supervisor reconnects with the Rithmic backoff (at least one second), and waits for a login change after a terminal login failure, a live login or no login. The owner now normalizes venue amounts to its instrument and account scales. Not done: the relay has not run against Rithmic yet (a live Rithmic Test run needs the maintainer's go-ahead), broker bracket levels are not shown in the desktop, and open P&L updates without a net quantity are not reported. Restriction (AGENTS.md): broker orders reach Rithmic only through this relay; naming_check now requires it to read the login from the vault and use the shared application name. Regressions: `requests_convert_exactly_or_are_refused`, `only_test_and_paper_trading_logins_are_demo`, `reported_orders_keep_the_brokers_exact_terms`, `lifecycle_steps_map_to_order_states`, `fills_carry_a_replay_stable_deal_id_and_the_execution_time`, `positions_report_net_quantity_average_and_open_pnl`, `orders_take_the_accounts_default_route_to_the_exchange`, `replies_are_attributed_by_identity_and_refusals_fall_back_to_the_oldest`, `replay_windows_cover_whole_seconds_through_now`, `a_snapshot_reports_working_orders_and_open_positions_once_both_answers_arrive`, `incomplete_or_refused_snapshots_are_not_reported`, `refusals_carry_the_brokers_reason_or_its_code`, `only_refused_states_keep_the_brokers_explanation`, `venue_reports_are_brought_to_the_owner_scales_exactly`. Workspace 2,322 passed, 0 failed (20 ignored); fmt, strict Clippy with and without the Rithmic kit, and naming_check green.
- 2026-10-22 feat(trading): no stabilization item. Rithmic order feedback and a new order ticket. A filled or cancelled broker order said "Practice ..." because the desktop worded every outcome as a practice order; `trading::order_outcome` now names the broker for a broker account and the toaster announces broker results the same way. A Rithmic cancel or reject showed no reason: the venue reports now keep the first non-blank order text or completion reason for rejected, cancel-rejected, cancelled and expired orders (the bare report type only for refusals), `record_broker_transition` stores it cut on a character boundary to the contract's field limit, and a cancel answering Aeris's own pending cancel carries `UserCancel` while any other end is the broker's. An inline bracket from the ticket keeps its entry's order type and prices instead of forcing a market entry. The order entry panel was rebuilt to the maintainer's design: symbol, open quantity and open/daily P/L, quantity stepper, broker (Practice or Rithmic) and account selects, a ticks-offset field, a BUY/SELL grid of MKT, BID/ASK, LMT, STP and STP LMT with Cancel and Flat, and an OCO section with stop and target ticks; where the bracket is held (server for a broker, client for practice) is shown read-only, and only paired SL/TP brackets can be chosen because the owner's bracket template always pairs them. New tokens `buy-subtle*`, `sell-subtle*`, `warning-hover` and `warning-foreground` back the tinted resting-order and Flat buttons. The old ticket's reverse and flatten-all helpers and the unused button trailing slot were removed. Verified: fmt, clippy (with and without the Rithmic kit), workspace tests and naming_check.
- 2026-10-22 fix(rithmic): no stabilization item. Tester reports from Rithmic charts. (1) Tabs, the symbol picker, the legend and the price label showed the reference data's long name ("Front Month for ES - ESZ6.CME"): a selected Rithmic contract is now labelled by its symbol alone, and retained panes and watchlist entries migrate through `migrate_retained_provider_display_symbol` (new `legacy_display_label`); the legend keeps its symbol · interval · venue form. (2) Opening several Rithmic chart tabs ended every chart with "Market worker stopped": all panes share one market worker, and any rejected pane event ended it. The likely trigger was a provider state for a selection's new provider arriving before that selection's outcome; it is now held on the endpoint and delivered once the selection is adopted, and any other rejected pane event fails only that pane and is logged ("Aeris market pane N event was rejected"); a stopped worker now logs its error. (3) The symbol menu injected "MNQ" into the search box on Rithmic and listed raw contracts. An empty Rithmic query now sends one product codes request (templates 111/112, time-of-interest products only, at most 128, deduplicated and sorted) and lists products; expanding a product runs its contract search and nests its contracts under it, nearest expiry first, and a typed search groups contracts under product rows. Catalog rows carry an explicit `kind` (instrument or product) and a product row is never selected; `ProviderPresentationDescriptor::default_listing` is removed. (4) The bottom panel shows only the market-data-by-Rithmic mark; the OMNE mark and notices stay on the login dialog and in About. Not verified: the product codes reply on a live gateway (the template ids and row framing follow the search reply; the kit protos give the fields but not the ids). Regressions: `a_pending_selections_provider_state_waits_for_its_adoption`, `a_rejected_pane_event_fails_only_that_pane`, `retained_long_names_migrate_to_the_contract_symbol`, `pane_harness_rithmic_lists_products_and_expands_contracts_with_an_empty_query`, plus product codes decode, collector and grouped-menu tests. fmt, strict Clippy with and without the Rithmic kit, naming_check and workspace tests green.
- 2026-10-22 fix(market): no stabilization item. Rithmic Test MNQZ6, NQZ6 and ESZ6 charts stayed on "Waiting for a covering market snapshot". The desktop log showed two MNQZ6 trade-built series recovering "Live trade series stalled while the market was open" every ~33 seconds for over an hour (161 recoveries and 166 history-plant logins in 66 minutes), each recovery dropping the chart back to waiting and replaying its full covering history. Rithmic Test trades are sparse, and the watchdog measured a trade-built series' progress only by trade publications. After the storm every time-bar replay on that login timed out (`Session(Deadline)`, 30 s): a live probe saw no replay frames for MNQZ6, NQZ6 or ESZ6 at 12h, 1h, 1m or 1D for over 20 minutes while the same history session answered heartbeats in about 270 ms; the provider-side cause is inferred, not confirmed. Fixed: a newer accepted top of book on the same instrument and entitlement now counts as progress for its trade-built series (`ProviderOrderBook.quote_observed_at`, cleared when the live market is invalidated), and each consecutive stall recovery that no trade followed doubles the open-market threshold up to eight minutes (`watchdog::quiet_recovery_grace`, `TradeModel.quiet_stall_recoveries`, reset by any accepted or buffered trade and by a session reset); the coordinator deadline uses the same progress. An unaccepted time or tick replay reply now logs its template, error, capped response codes and which bar fields were present (no prices or identities), because eight `ResponseCodeShape` and one `MissingField("bar.ohlc")` failures in the log could not be explained from it. Restriction (AGENTS.md): the stall policy line now includes the quote progress and the bounded quiet backoff. Not verified: whether Rithmic Test quotes keep a quiet series alive live, and the reply shapes behind those decode failures. Regressions: `watchdog_quiet_trade_series_with_live_quotes_does_not_recover`, `watchdog_quiet_trade_series_defers_repeated_recoveries_until_a_trade`, `watchdog_quiet_recovery_grace_doubles_the_threshold_up_to_a_bound`, `unaccepted_reply_codes_are_bounded_and_printable`.
- 2026-10-22 fix(rithmic): no stabilization item. The Rithmic symbol menu's product listing opened on numeric and obscure product codes, because the product-code collector kept the alphabetically lowest 128 code/exchange keys, so ES, NQ, GC and other common products could fall outside the bound. Fixed: `ProductCodeCollector` ranks before bounding, featured products first in activity order (the adapter-owned `FEATURED_PRODUCTS` table of code and exchange pairs: CME equity index and micros, NYMEX/COMEX energy and metals, CBOT rates and grains, CME FX, livestock and crypto), then lettered codes, then codes without letters. The expand/collapse chevron on a product row is now its own ghost `Button` with its own hover that toggles the product; the row click still toggles too. Not verified live: the plant's exchange names for every featured pair. Regression: `product_listing_dedupes_ranks_featured_first_and_bounds`.
- 2026-10-22 fix(market): no stabilization item. Rithmic Test MNQZ6/NQZ6/ESZ6 charts sat on "Loading chart · Waiting for a covering market snapshot" forever: every covering time-bar replay ended `Session(Deadline)` after 30 seconds, and each exhausted retry budget reset the lane's attempt count, so attempts 1..4 cycled every few seconds indefinitely while the live ticker streamed. When a trade-built series' covering history exhausts its bounded retries, the runtime now opens a live-only series from the trade stream (the first trade anchors the first bar), publishes the provider-neutral `SeriesPresentationStatus::HistoryUnavailable` ("History unavailable; waiting for live trades" until the first bar, then "showing live bars"), and the tracker keeps one slow probe (`HISTORY_UNAVAILABLE_RETRY_DELAY`, 5 minutes) armed instead of restarting the fast budget; an exhausted backfill lane still waits out its 30-second cooldown for fresh demand. A later covering success merges provider bars with the trade-built ones by timestamp (provider wins overlaps), retains the one canonical forming candle, reseeds the trade handoff from the canonical snapshot and publishes plain Live; retired generations complete without touching the series. Desktop: new `ChartState::HistoryUnavailable` (warning notice, center overlay until bars exist, corner toast once they do); a bar publication alone never clears it, only the runtime's Live readiness does. Regressions: `failed_current_history_installs_live_only_series_and_reports_unavailable`, `live_only_trade_bars_publish_with_the_unavailable_notice`, `covering_history_success_after_live_only_merges_and_clears_the_notice`, `retired_generation_completion_never_touches_the_live_only_series`, `history_tracker_exhausted_current_lane_keeps_one_slow_probe`, `exhausted_current_history_keeps_one_slow_probe` (replaces `exhausted_current_history_keeps_one_cooldown_recovery_ticket`, which asserted the reset), `history_unavailable_notice_distinguishes_live_bars_from_waiting`, `pane_harness_history_unavailable_holds_until_the_runtime_clears_it`. Not verified live: whether Rithmic Test's replay eventually answers and the merge rejoins on the real session (the path is covered by the scripted-clock regressions above). Workspace 2,359 passed, 0 failed; fmt, strict Clippy with and without the Rithmic kit, and naming_check green.
- 2026-10-23 style(design-system): no stabilization item. The platform theme moved to the Theme System's latest tokens: pure-black dark `surface`, `#010101` dark `surface-popover`, opaque `#2E2E2E` dark `border`, `#ededed` dark text and active icons, `#fafafa59` dark muted text, `#fafafa0a` dark `surface-secondary`, light `border` `#0a0a0a33` with `border-popover` pointing at it, and a new `--border-width: 1px`. `--surface-subtle` and `--surface-raised` are gone. `platform.css` and `CSS.md` carry the supplied token blocks verbatim. `build.rs` now projects `--border-width` into `ThemeDimensions::border_width` instead of a hard-coded 1.0, and rejects a dark value that differs from light. The former consumers moved to `surface-secondary` (order ticket fields, screener kind chips, the theme preview tile). `TabList`/`Tab` follow the Theme System's new `.ui-tabs`/`.ui-tab`: underlined tabs over a 1px `border` baseline, `gap-4`, with the selected tab's 2px underline and text in `primary`, replacing the raised segmented track. Aeris Charts is pinned to d23d7480 (0.13.3), which adopts the same refreshed tokens and makes the dashed crosshair the default again; layouts that only stored the former solid default now follow it, and the crosshair provenance test picks a non-default style instead of assuming dashed is custom. The website mirror was not touched because it holds uncommitted in-progress edits to the same stylesheet. Regressions: the design-system palette, popover and order-book contract tests now pin the new values, and `border_width_matches_the_platform_contract` reads the token.
- 2026-10-23 style(design-system): no stabilization item. Following the Theme System's latest change, floating layers (menus, dialogs, popovers, toasts, hints and floating toolbars) draw a 1px `border` edge on the opaque `surface-popover` fill. `--border-popover` and `--border-popover-highlight` are gone from `platform.css`, `CSS.md` and the Rust registry, together with the `popover_highlight` inner-edge overlay, `floating_toolbar_highlight` and `ThemeDimensions::popover_highlight_width`. The chart tokens (`bullish`, `bearish`, `buy-bubble`, `sell-bubble`) stay, matching the Theme System. Regression: `popover_layer_tokens_match_the_platform_contract` now asserts that the retired popover edge tokens stay out of both themes. The `aeris_naming_check` Aeris Charts pin assertion moves to d23d7480 too; the previous commit bumped the dependency without it.
- 2026-10-23 style(design-system): no stabilization item. The platform theme follows the Theme System's retheme: dark `surface-secondary` is the opaque `#171717`, dark `border` `#2c2c2c`, dark `border-secondary` `#404040` and dark `hover-bg` `#fafafa1f`, and `--surface-popover` is gone. Floating layers (menus, dialogs, popovers, toasts, hints, floating toolbars) and tooltips now use the `.ui-popover` look: a `surface-secondary` fill with a 1px `border-secondary` edge; menu rows rest on `surface-secondary`. Text fields and order-ticket fields take `border-secondary` in dark mode through the new `field_border`, matching the Theme System inputs and `.ticket-field`. `CSS.md` drops the translucent dark `surface-secondary` rule. Regressions: the palette test pins the new dark values, `popover_layer_tokens_match_the_platform_contract` requires an opaque `surface-secondary` and keeps `surface-popover` retired, and the hover compositing test covers the opaque dark secondary surface.
- 2026-10-23 style(design-system): no stabilization item. Light `border-secondary` is now `#0a0a0a33` (the same as light `border`), light `hover-bg` `#0a0a0a14` and light `active-bg` `#0a0a0a1f`; dark is unchanged. Large panels (the chart settings panel and the indicator, Volume, Big Trades and Order Flow settings dialogs) now use the same popover surface as every floating layer: `surface_panel`, `popover_edge`, `MenuPanel::on_surface`, `Dialog::panel_surface` and the chrome overlay's `on_surface` style are removed. Settings groups no longer sit on filled cards; each group keeps its title and the groups are split by a 1px `border` divider through the shared `settings_groups` helper (the chart context menu's duplicate group helpers are gone). The accounts panel's connection and account cards lose their fill and are split by a `border` divider, and the Simulated badge is a neutral `surface-secondary` badge with a `border-subtle` edge. `CSS.md` rules updated. Regressions: the palette test pins the new light values and asserts `active-bg` stays stronger than `hover-bg`.
- 2026-10-23 style(ui): no stabilization item. Aeris Charts is pinned to 401c002c (0.13.4 and the light secondary border and interaction fills), and the `aeris_naming_check` pin assertion moves with it. The indicator settings dialogs (study, Volume, Big Trades, Order Flow) use the chart settings panel's row layout: label and description on the left, a compact control on the right; dropdowns, number fields and hex swatches are `SETTINGS_CONTROL_WIDTH` wide instead of stretching across the dialog (shared `described_setting_row`, `setting_block`, `control_strip`, `setting_input`). Paired colors (Up/Down, Bid/Ask, positive/negative, CVD candles) sit side by side as named swatches (`named_color_button`). A study's Style group is one Lines row: the thickness dropdown with a swatch per line beside it, bare swatches named by their tooltips when the study draws several lines (`color_dot_button`), replacing the separate thickness and stacked color rows. The symbol and indicator menus no longer paint `surface` over the overlay's popover surface. Dialogs drop their shadow, and the color picker handles use a `border-secondary` edge instead of a shadow. The color picker's hex row is sized in design rems like its Set button, so the preview, the field and Set stay the same height in rem-scaled panels. The Rithmic card no longer offers Change (a different login is Disconnect then Connect), and New practice account uses the `Default` button. `CSS.md` rules updated (no floating-layer shadows, compact setting rows, side-by-side colors, Default for a panel's add or connect action).
- 2026-10-23 docs(stabilization): no stabilization item. Audit of this tracker and `architecture.md` against the code. Corrected: V24 is applied by the gated finalize step (only the old P&L tables stay deferred); MD-08, MD-11, MD-13, TR-01 and UI-01 are closed by their checked items; the M2.4 reconnect-log gap was resolved by M3.12; the macOS workflows exist; CI is still `disabled_manually` (rechecked). Unchecked M6.3, M6.6–M6.12, M7.4, M8.5 and M8.10 gained status notes for work already done or no longer applicable, without checking any item. Section 5 now records the `AGENTS.md` rules it lacked (`DesktopServices`, trading subscription, V22–V28, `Fenced` envelope, `select_and_demand`, `HistoryUnavailable`, the Hyperliquid budget and others) and the corrected retention rule. `architecture.md` drops the never-built `aeris_clock` crate and the removed broker providers, and describes the as-built module trees, schema sequence, subscription API and Rithmic venue relay. No code changed.
- 2026-10-23 M6.6 Part 6a refactor(desktop): canonical order-book snapshots keep the fenced coordinator envelope to GPUI and project only for the accepted pane selection; deleted the old depth message/worker conversion. Migrated mailbox, worker and GPUI regressions retain order, identity, revision and retired-generation checks. Part 6b tape/studies and Parts 7-8 remain; M6.6 stays unchecked.
- 2026-10-23 feat(desktop): no stabilization item. The Accounts dropdown is now a sidebar panel (Connections, Trading accounts) like chart settings: the Rithmic login, the new practice account form and the delete confirmation render inline, and the Rithmic login dialog and the account create/delete dialogs are removed. Leaving a tab or closing the panel drops an unfinished form (`CancelRithmicLogin` reaches the window-owned login form). The symbol, indicator, chart settings, profile and Accounts panels use `--radius-compact`; other menus keep `--radius-default` through the new `MenuPanel::radius`.
- 2026-10-23 feat(desktop): no stabilization item. The Accounts panel is now a floating panel like chart settings: it opens centered over the whole window and moves by its title bar (the shared `FloatingPanelPlacement`, `centered_floating_panel_origin` and `floating_panel_drag_grip` replace the chart-settings-only versions), and every fresh open starts centered. Its content uses the settings layout: titled groups split by a `border` divider, each row with its label and detail on the left and its control on the right (`described_setting_row`). Connections lists Rithmic (status pill, Connect or Disconnect, the inline login as Server, System, User ID and Password rows) and Hyperliquid (no login needed); Trading accounts lists practice accounts with their environment, a Use / Trading button and Delete, the inline new-account and delete rows, and broker accounts. The sidebar shows the account this workspace trades on. Every `MenuPanel`, chrome overlay, `Dialog` (the indicator, Volume, Big Trades, Order Flow, drawing, alert, shortcut and confirmation dialogs) and the color picker now use `--radius-compact`; `MenuPanel::radius` is removed. `CSS.md` rules updated. Regression: `accounts_panel_opens_centered_follows_a_drag_and_recenters_on_reopen`.
- 2026-10-23 style(desktop): no stabilization item. Every `Switch` now uses the order ticket's geometry (28x16 track, 12px round thumb); `Switch::small` is removed, so the settings dialogs, menus and the order ticket draw the same switch. The chart settings panel takes the Accounts panel frame: a 44px title bar with the settings icon, a 168-wide sidebar with section icons split from the content by a `border-secondary` line, the Template control in a footer under a `border` line, and a section heading with a short description above the groups. The Accounts sidebar no longer shows the Simulated badge under the account traded on. `CSS.md` rules updated.
- 2026-10-23 M5.9 follow-up fix(trading): correct the account shortcut scope for VAL-TRD-064 without changing the instrument Flat action; owner test proves two chartless-interest positions close exactly once and remain flat after restart. Isolated BTC/ETH practice GUI showed two positions before, zero within 2.3 s, two closing history rows and zero after relaunch; all five workspace gates passed. Evidence: mission library `evidence/m5-flatten-account-fix/`. The four pre-existing maintainer commits ahead of origin/main govern the push rule.
- 2026-10-23 M5 coverage follow-up test(trading): persisted maker/taker fee selection, same-quote bracket fence, typed stop/expiry cascades, migrated marks, maker-entry fee chain across restart, checkpoint/delete rollback and ten unchanged equity ticks gained owner assertions. Fixed same-quote stop-limit taker classification and repeated Terms revision drift. The route-less feed-state and open-order retention clauses and isolated paper probe checks remain pending; keep this follow-up explicitly open.
- 2026-10-23 M5.12 follow-up feat(probes): paper probe measures the matched flatten bid, first post-restart quote within 30 s, field-by-field state and revision equality, durable export/CSV parity and an actual live-feed no-quote QuoteUnavailable negative. Fresh isolated passing and negative runs are being archived; route-less migration feed-state and open-order retention owner tests remain outstanding.
- 2026-10-23 M5.12 follow-up fix(probes): match every post-restart flatten sell not only to its recorded bid and provider generation, but also to a quote received after the first new-epoch quote and a fill executed after that quote; a recovered pre-restart quote cannot satisfy the live probe.
- 2026-10-23 M5 coverage follow-up test(trading): a V19 route-less BTC fixture exposes Unavailable/RouteUnknown at the trading QuoteBook until a verified Hyperliquid BTC route is stored, then QuoteMissing until feed Live; a last-target 50/50 bracket fills both targets and cancels its resized stop with typed CoverageShrink/Cascade events. Retention now preserves open-order events as well as fills (a failing partial-fill fixture caught an OR-precedence deletion defect), and a killed export writer leaves the complete previous JSON intact. Probe disconnect and final evidence remain open.
- 2026-10-23 M5.12 follow-up fix(probes): the SOL disconnect check accepts the typed session-unknown admission refusal only after observing the feed leave Live, alongside stale and unavailable-quote refusals; unrelated errors and any accepted order remain failures. A deterministic classification test prevents a reconnect from weakening the admission assertion. Passing and negative isolated runs and clean-HEAD evidence follow this commit.
- 2026-10-23 M5 round-2 test(trading): a pending entry cancellation cascades typed Cancelled events to all three held legs (VAL-TRD-046); route-less persisted BTC orders expose per-order route-unknown Unavailable in owner publications until symbol Terms resolve the route and a fresh quote executes (VAL-TRD-056); added `replay_maker_bracket_fee_chain_golden_v2.txt` for the combined maker/fee/bracket/two-restart transcript (VAL-TRD-057). Existing golden files are unchanged.
- 2026-10-23 M5 route-bypass migration: five group-recovery tests now use verified owner Terms/Live quotes and an eligible resting entry, including a fresh owner feed after restart for the migrated stop. Original recovery assertions and all golden files are unchanged. Service fixtures, replay and other direct-observation cases remain before bypass removal.
- 2026-10-23 M5 route-bypass migration: 21 service fill, fee, bracket and priority tests now deliver every fill-producing quote through the owner inbox on the deterministic clock with shared awaiting/quoted/reopen fixtures, passing with and without the bypasses. One unreachable fixture (a buy limit resting above valid buy stops) became a resting sell limit with the same priority assertion. No golden changed; about 39 service tests and the replay harness remain.
- 2026-10-23 M5 route-bypass migration: 31 further service and feed tests (bracket/protection families, discipline, risk meters, retention, DAY working exposure, always-open far-future clocks, route-less Hyperliquid and chart modify) pass with and without the bypasses through the owner feed path; fixtures now keep quote interest across flat periods, restart risk-session tests on the deterministic clock, and place stops only where not already crossed. Only the 21 replay tests still depend on the bypasses.
- 2026-10-23 M5 route-bypass migration: the replay harness registers the instrument through verified replay-feed Terms and re-primes session/Live after every restart; all 25 replay tests pass with and without the bypasses. `simulated_golden_v2.txt` changes only revisions (-1 from flatten on) and owner-book quote sequences (1/2/3 to 2/4/5); no order, event, fill or P&L value changed. No test depends on the bypasses now.
- 2026-10-23 M5 route-bypass removal fix(trading): deleted the three test-only route bypasses; added route-less admission (QuoteUnavailable while RouteUnknown), never-fills and fills-after-verified-Terms regressions. Test builds now use production route gating.
- 2026-10-23 M6.6 preflight test(desktop): stabilize study state-limit, subtree checkpoint and cached offline-lease fixtures - ordinary study unit tests wait for the worker while the explicit timeout test opts into the unchanged 50 ms production deadline; the cached account view is set before its startup request. The desktop poll policy is unchanged. M6.6 Part 6b follows.
- 2026-10-23 M6.6 Part 6b refactor(desktop): tape and study publications retain the coordinator consumer envelope to GPUI, with exact worker consumer/selection checks and pane-side committed-generation guards; their old pushed-event conversion and mailbox variants are deleted. Study lifecycle control reserve is retained. M6.6 Parts 7-8 and the GUI pass remain open.
- 2026-10-23 M6.6 Part 7a refactor(desktop): price-alert triggers retain their coordinator consumer envelope until the pane matches its alert identity; the old trigger message is removed. A wrong-consumer fixture does not notify or enqueue, while a matching fixture preserves the exact fence. Other Part 7 routes, Part 8 and milestone GUI evidence remain open.

- 2026-10-23 M6.6 Part 7b refactor(desktop): provider state retains its coordinator envelope across pending selection and queue delivery; pane projection checks provider and committed generation, with bridge and GPUI regressions. Deleted the duplicate pushed-event connection conversion; local startup controls remain explicit. M6.6 and VAL-DSK-021/022 remain open.
- 2026-10-23 M6.6 Part 7c fix(desktop): verified catalog terms now flow through the one session-status publisher after the catalog reply, retaining its clock, selection generation and backpressure retry instead of a second best-effort status send. The existing pane generation/instrument projection remains; Part 8 removes its message variant. Remaining Part 7 routes, VAL-DSK-021/022 and guarded GUI remain open.
- 2026-10-23 M6.6 Part 7d refactor(desktop): covering recovery snapshots retain their coordinator consumer epoch to pane projection and exact chart request installation; removed the duplicate worker-built success bootstrap. Local failure and supersession controls retain generation/request fences. The worker and GPUI request/ordering/backpressure regressions pass; catalog, screener, diagnostics, alert-sync and Part 8 remain.
- 2026-10-23 M6.6 Part 7e refactor(desktop): removed the unproduced diagnostic mailbox route and pane logger; real app-scoped catalog failure controls retain FIFO order with market publications and report bounded backpressure. Catalog/screener, alert-sync and Part 8 remain.
- 2026-10-24 M6.6 Part 7f fix(desktop): correlate local alert-sync results with the latest accepted pane command sequence; reject stale success and failure before notice mutation. Catalog/screener and Part 8 remain.
- 2026-10-24 M6.6 timing follow-up test(trading): use a subsequent owner command as the publication completion barrier for every_revision_changing_step_is_published; the test no longer races a five-second wait after an early command reply. Catalog/screener and Part 8 remain.
- 2026-10-24 M6.6 Part 7g refactor(desktop): retained coordinator consumer envelopes for catalog and screener replies to presentation; removed the old worker-built catalog conversion and moved the view projection to desktop. Part 8 and VAL-DSK-021/022 remain open.
- 2026-10-23 UI feat(design-system): platform theme presets (Aeris, Tokyo Night, Catppuccin, GitHub, One, Dracula, Solarized, Gruvbox; light and dark) compile from platform.css into `ThemePreset`/`AerisTheme::resolve` and are picked in the profile menu; chart_integration pins theme-following chart colors to a non-Aeris preset's tokens so Aeris Charts never knows presets. Chart settings gain persisted axis border and axis text colors (appearance tags 28/29, empty follows the theme). Aeris Charts pinned to 2ed6e82 (wheel zoom/pan). Restriction: presets live only in platform.css and the design system; the engine keeps its own defaults. Time-axis labels still follow the chart text color, and the selected preset is not persisted (neither is the color mode).
- 2026-10-23 UI fix(desktop): preset picker swatch dots sit inside their ring by an equal padding instead of flex-centering a fixed size, so they no longer drift off-center by half a pixel.
- 2026-10-23 UI style(design-system): no stabilization item. The Aeris theme follows the updated `themes.md`: light `border-secondary` `#0a0a0a1a`, light hover/active `#0a0a0a0d`; dark `surface` `#171717`, translucent dark `surface-secondary` `#fafafa0f`, dark borders `#fafafa1a`/`#fafafa26`, text `#fafafa`, muted `#fafafa40`, hover `#fafafa13`. The Aeris-only popover layer returns: `popover_surface`, menu rows and tooltips use `surface-popover` with a `border-popover-highlight` inner edge and a 1px solid `border-popover` outer ring; presets resolve the tokens to `surface-secondary`, transparent and `border-secondary`. `CSS.md` and the website's `platform.css` carry the same Aeris tokens. Restriction recorded in section 5.
- 2026-10-24 M6.6 Part 8a refactor(desktop): extracted the one bounded pane projection queue from market_worker, removed its bespoke enqueue, and preserved runtime-event FIFO, the control reserve, wake continuation and overflow in a 10,000-event regression. The old message type remains to collapse in Part 8b; M6.6 and VAL-DSK-021/022 remain unchecked.
- 2026-10-24 M6.6 Part 8b2c refactor(desktop): merged study registration success/failure into one request-sequence-fenced outcome; GPUI covers redacted failure, success and retired outcomes. Metadata-bearing fenced runtime entries, remaining local outcomes and final mailbox/type deletion remain; M6.6 stays unchecked.
- 2026-10-24 M6.6 Part 8b2d refactor(desktop): merged study reinitialization success/failure into one local result, preserving failure's unchanged persisted settings and a retry's successful replacement; GPUI checks both outcomes. Metadata-bearing fenced runtime entries, other local outcomes and message/type deletion remain; M6.6 and VAL-DSK-021/022 stay unchecked.
- 2026-10-24 M6.6 Part 8b3b refactor(desktop): merged local connection and calendar statuses into one generation-stamped pane projection, removed the unused connection detail from the queue, and retained per-selection/RTT/backpressure behavior with a mixed FIFO regression. Provider-state metadata, other local outcomes and final message/channel deletion remain; M6.6 stays unchecked.
- 2026-10-24 feat(accounts): no stabilization item. The Accounts panel Connections rows show each provider's feed level as a neutral pill, read from provider registration: Rithmic "L3 data" (it streams depth by order) and Hyperliquid "L2 data" (aggregated l2Book levels). `ProviderPresentationDescriptor::depth_available` is replaced by `feed_level: ProviderFeedLevel` (Bars, TopOfBook, PriceLevels, OrderLevels); registration validation now also requires the feed level to agree with the quote capability, which also removes one `struct_excessive_bools` flag. Regressions: `feed_levels_name_their_depth`, `registered_providers_declare_their_feed_level`. fmt, Clippy on the affected crates, naming_check and contracts, market_runtime and desktop tests run.
- 2026-10-24 M6.6 Part 8b3c refactor(desktop): provider state now shares the selection-bound fenced runtime projection; the endpoint selection stamp, coordinator envelope, control reserve and FIFO are retained. Local outcomes and final transitional-type deletion still block M6.6 and VAL-DSK-021/022.
- 2026-10-24 fix(trading): no stabilization item. A practice order was refused with "tilt size reduction limits the next entry to 100000000 units at scale 8": an always-on discipline rule capped every new entry at the losing round trip's peak size after any loss, and two losses inside five minutes locked the account for fifteen minutes. Both ran ahead of the risk profile on every account, with no setting. The maintainer removed both: `DisciplineState` keeps only the last stop fill for the fast stop re-entry warning, its rapid-loss and post-loss cap columns are written empty and never read (so a stored cooldown or cap from an older build stops blocking at once), and the synthetic cooldown lock no longer appears in risk locks or meters. Orders accepted with risk warnings now raise a warning toast ("Risk warning on Buy 1 ESZ5" over the warnings and the account) on every venue. Not removed: the trade cycle's now-unread peak quantity column. Regressions: `losing_trades_never_cap_size_or_lock_the_account`, `a_risk_warned_acceptance_is_announced_with_its_warnings`. fmt, strict Clippy on trading_runtime and desktop, naming_check, and trading_runtime and desktop tests green.
- 2026-10-24 M6.6 Part 8b3d refactor(desktop): local catalog and endpoint failures now use one explicitly scoped projection; app failures remain app-scoped and selection failures keep the exact generation fence. A mixed-scope FIFO regression and existing pane redaction/fencing tests cover the change. Recovery, alert and study-removal outcomes and final bridge deletion remain; M6.6 and VAL-DSK-021/022 stay open.
- 2026-10-24 M6.6 Part 8b3e refactor(desktop): four local command-result mailbox variants share one bounded outcome entry; original request and study fences remain, and unused study-removal error text is not queued. FIFO and pane notice/fence regressions cover the route. Recovery/selection outcomes and final bridge deletion remain; M6.6 and VAL-DSK-021/022 stay open.
- 2026-10-24 M6.6 Part 8b3f refactor(desktop): superseded and failed local recovery outcomes now share the bounded local command-result projection instead of a separate mailbox arm. The pane still applies the exact selection lifecycle and recovery request fences, including retired replies, while the covering snapshot stays in its coordinator envelope. Mixed local-outcome FIFO, backpressure and GPUI retired-recovery regressions cover the route. Selection outcomes, final bridge type/channel deletion and VAL-DSK-021/022 remain; M6.6 stays unchecked.
- 2026-10-24 M6.6 Part 8b3g refactor(desktop): initial selection terms share the bounded local command outcome route; exact pending-generation acceptance, duplicate rejection, four-pane restored replies and the FIFO/control reserve are retained. Engine/catalog outcomes, metadata-bearing runtime entries and final message/channel deletion remain; M6.6 and VAL-DSK-021/022 stay open.

## 7. Known gaps / deferred

From `reports/work_items.md` ("Deferred / out of scope") plus mission-accepted limitations:

- Depth-aware partial fills, the slippage model, margin and buying power, currency conversion, and the
  Funding/MarginReserve ledger kinds. Only hooks are left (D3 "solid core").
- Removing the old P&L tables (`trade_pnl_cycles`, `risk_trade_cycles`) and the per-fill P&L columns;
  they stay, unread. V24 itself (dropping `managed_brackets` and `protective_orders`) is applied by the
  gated `finalize_group_migration` step (section 2).
- Removing the legacy data-root migration and the vault legacy-name probes (F19, LC-17) waits until
  the maintainer confirms their machine is migrated.
- Risk profile registration UI (TR-09 desktop part); only runtime rollover is in scope. Nothing in the
  desktop selects the futures `ChicagoClose` boundary either.
- Automatic route resolution for migrated instruments without a stored route (09 Q10); these show
  "feed unavailable" until the instrument is selected again.
- Holiday data for exchange-rule calendars (10 Q3). Exchange rules report Open on holidays; trading is
  still protected by the staleness rule.
- Focusing the existing window on a second launch; it needs IPC.
- An Aeris Charts append operation for order flow (only if measured as the bottleneck in M7.11).
- An Aeris Charts in-place cross-instrument data reset; M6.4 confirmed it remains an Aeris Charts
  operation, to be added with M7.9.
- Workspace-level trading account selection; the per-pane selection is kept (architecture.md 3.7.3).
- Rithmic live qualification, production release publication,
  code-signing certificate and key custody, the AWS release endpoint, and macOS signing and
  notarization (the manual `macos.yml` and `macos_preview.yml` workflows exist).
- Manual checks only the maintainer can run: logoff flush (M8.3), VM install and uninstall (M8.6),
  Rithmic login through the native vault (M6.9).
- As checked with `gh workflow list --all` on 2026-10-13 and again on 2026-10-23, the GitHub `CI`
  workflow (and `Live market gates`) is `disabled_manually`, so pushes get no CI run.
  Re-enabling it requires the maintainer; no Actions setting was changed.
- Resolved: the M2.4 blocked-network reconnect interval log could not be collected through an HTTP
  proxy (it also blocked the startup catalog); M3.12 (6d559cd7) added the probe-feature
  WebSocket-only fault control and recorded the live retry delays.
