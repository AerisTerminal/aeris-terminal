# Aeris Terminal stabilization: reconciled work-item list

Sources: `mission.md`, `07_platform_design.md` (F00-F20, LC/AC), `10_market_design.md` §11 (#1-#20),
`09_trading_design.md` §8 (#1-#24), `11_desktop_design.md` §9 (D0-D16), audits 01-04 (MD, TR, UI, PL).
Each item is one worker session that ends in one commit with green gates (fmt, clippy workspace `-D warnings`,
workspace tests, `cargo run -p aeris_naming_check --locked`). Every commit also updates
`plan/stabilization/stabilization_work.md`. Any new architecture restriction (for example a naming_check rule)
is recorded in `AGENTS.md` in the same commit.

| Milestone | Items | Key outcomes |
|---|---|---|
| M1 `foundations` | 9 | Tracking doc and AGENTS rule; hosted CI on push/PR; MD-01 Day3 restore fix; bounded exit and secret tokens; `platform_sys` (sole unsafe crate) with one durable writer adopted everywhere (atomic trading export); rotating log and panic hook; per-data-folder single instance plus validation mode with isolated vault; hardened SQLite with table-driven migrations and backups |
| M2 `market-foundations` | 10 | Characterization tests and benchmark baseline; injected clock; jittered, budgeted reconnect policy; provider-neutral calendar; watchdog for every provider; session-correct daily, weekly and monthly live bars; typed `Epoch<Tag>`/`Fenced<T>`; command fences and consumer registry; `HistoryTracker` (plus `provider_history` trim) |
| M3 `market-lifecycle` | 12 | One per-series state machine (candle path, then trade-built path); series-tail accessor and dirty-set publication (measured); provider capabilities and session registry; instrument leases plus the trading quote feed; fenced events with a waitable outbox (no 2 ms poll); `select_and_demand` with descriptors; replay harness; live Hyperliquid lifecycle probe |
| M4 `trading-core` | 10 | Injected clock; `TradingCore` owner shell; trading replay harness; indexed order book; module and store split; runtime-owned quotes (closing a chart no longer stops stops); provider-neutral gating (closed or unknown session rejected, stale after 10 s crypto / 60 s others); one fill-accounting path; facts-only persistence |
| M5 `trading-engine` | 12 | Full order lifecycle with typed events and revisions; one admission validator; bid/ask fill rules; DAY/GTC/IOC/FOK; order groups with bracket migration; removal of the old protection layers; cash ledger with configurable fees (default zero); checkpoints and bounded retention; emergency operations; risk rollover; change-driven publication API; live paper probes |
| M6 `desktop-bridge` | 12 | `DesktopServices` and test support; `NotificationCenter`; generation-fenced `ChartBinding` and `PanePresentation`; one-step selection; bridge message collapse; descriptor-driven provider neutrality (fake CLI modes removed, naming_check rule); typed events; `TradingStore` and revisioned intents; zero unhandled discards |
| M7 `desktop-structure` | 12 | Design tokens; `AccountGate`; chart host events; pane and app controller splits; dirty-source `FrameScheduler`; versioned workspace container and document migrations; hollow candles in Aeris Charts (cross-repo, pin bump); runtime sweeps and incremental tape; desktop test rewrite |
| M8 `lifecycle-and-trim` | 12 | `release_lifecycle` crate; launcher, trust, FFI, account, readiness, wire-protocol, packager and rollback fixes (LC-*, AC-*); dependency-direction fixes and contracts split; dependency hygiene; `gpui_base` and tooling trim; structural naming_check; final README and AGENTS |

Total: 89 items.

Binding reconciliations applied throughout:
- **One durable writer.** It lives in the new leaf crate `platform_sys`, the only crate allowed `unsafe`. The desktop
  workspace container in M7.7 is built on top of it; there is no second `DurableFile` in `platform_runtime`
  (this supersedes 11 D12).
- **One quote surface.** `market_runtime` (M3.7, M3.8) provides MarketEngine instrument leases (`QuoteSubscription`
  lease guards) and the interest-driven trading feed (`MarketFeedEvent`). `trading_runtime` (M4.7) consumes it
  through `aeris_trading::market` domain types. The two are wired at the desktop composition root. There is no
  dependency from `trading_runtime` to `market_runtime`. Market item 15 and trading items 6-8 are deduplicated
  this way.
- **Gating.** User decision overrides 09 Q3. Every order is rejected when the session is Closed or Unknown.
- **Placement exceptions.**
  - AC-01 (token redaction) lands in M1 with AC-05, because the log sink depends on it.
  - The other AC fixes are in M8.

---

## M1 foundations

### M1.1 stabilization-tracking-doc
Title: Create the stabilization tracking document and allow `plan/stabilization` in AGENTS.md
Milestone: foundations   Crates: none (docs, tools/naming_check if needed)   Depends on: -
Scope: Create `plan/stabilization/stabilization_work.md`. It holds every finding ID (MD, TR, UI, PL, LC, AC), the binding user decisions, all milestones and items from this list with checkbox status, a progress log, active restrictions and resume instructions. Copy into `plan/stabilization/`:
- audits 01-05;
- baseline 06;
- designs 07 and 09-11;
- this list.
Copies must contain no account identifiers. Update `AGENTS.md` to:
- allow `plan/stabilization/` as the one sanctioned planning location;
- require each feature commit to update the doc;
- require recording every new architecture restriction in AGENTS.md in the same commit.
Adjust the naming_check markdown inventory only if it rejects the new files.
Design refs: mission.md "Tracking document"; 07 §A4 (`plan/` row)
Closes findings: PL-11 (resolved by a deliberate rule change; `plan/` kept per user decision)
Key tests:
- naming_check and full gate green with the new files.
User-visible/validatable behavior:
- The tracking doc exists in the repo with every finding ID, decision and checkbox; AGENTS.md states the new rules.

### M1.2 hosted-ci-push-pr
Title: CI on push and pull request with GitHub-hosted runners
Milestone: foundations   Crates: .github/workflows, tools/naming_check   Depends on: M1.1
Scope: Rewrite `.github/workflows/ci.yml`:
- Triggers: `push` (main), `pull_request` and `workflow_dispatch`. Never `pull_request_target`.
- Settings: `concurrency` with cancel-in-progress, `permissions: contents: read`, no secrets.
- `windows-latest` job: fmt, clippy workspace `-D warnings`, workspace tests (all features, locked) and naming_check, with step timeouts and `AERIS_RITHMIC_KIT_DISABLED=1`.
- `ubuntu-latest` job: fmt and naming_check.
Replace the "self-hosted only" header comment. Keep the stale pacing/evidence verifier steps off the push path (they are removed in M8.11). Update the naming_check CI assertions (`workflows_never_run_fork_pull_requests` and the shape checks) to require this shape.
Design refs: 07 §B5, F00 (adapted to hosted runners per user decision); 12 §6a, §6c
Closes findings: PL-09
Key tests:
- naming_check unit tests for the CI shape (positive and negative).
- The first push after the milestone produces green runs on both runners (run URL recorded in the tracking doc).
User-visible/validatable behavior:
- A push to `origin/main` shows a green GitHub Actions run on windows-latest and ubuntu-latest.

### M1.3 unified-interval-mapping
Title: Fix Day3 workspace restore with one runtime-owned interval/cadence mapping
Milestone: foundations   Crates: market_runtime, domain/market_data, apps/desktop   Depends on: -
Scope: `market_runtime` owns `resolve_cadence(ChartInterval, history caps, calendar kind)` and the legacy inverse. Until M2.5 provides `CalendarSourceKind`, the provider's existing daily-bars capability is the calendar input. Workspace persistence stores `ChartInterval` and re-resolves on load. A legacy reader accepts both Day3 encodings (`FixedSeconds/259200`, `SessionDays/3`).
Delete the desktop tables:
- `workspace_series` cadence logic;
- `pane_interval`;
- the `series_key` interval branch in `replay_conversion.rs`;
- the `workspace_surface.rs:1223` mapping.
Restore and readiness skip or repair an unsupported pane with a notice instead of failing the whole workspace. Before choosing Hyperliquid AlwaysOpen Day1/Day3 alignment, confirm it from the Hyperliquid documentation (10 Q6). naming_check: no `SeriesCadence::` construction in the desktop.
Design refs: 10 §11 #3 (+ Day3 part of #1), §9.4; 01 MD-01
Closes findings: MD-01
Key tests:
- Every `ChartInterval` for every provider presentation round-trips through writer and reader.
- A legacy Day3 fixture (both encodings) loads as Day3.
- A workspace with one bad pane restores the others and reports one notice.
User-visible/validatable behavior:
- GUI: open a Hyperliquid 3D chart, quit and relaunch with the same validation folder. The workspace restores with the 3D pane intact.
- `--desktop-readiness` exits 0 with a saved Day3 pane.

### M1.4 bounded-exit-and-secret-tokens
Title: Exit never parks forever; tokens are redacted and zeroized
Milestone: foundations   Crates: apps/desktop, account_runtime   Depends on: -
Scope: AC-05: replace the thread park in `retain_account_refresh_quiesce_for_exit` (`desktop.rs:302-342`). The new path waits up to the timeout, logs, returns an error that `lifecycle.rs` reports, and exits with a code. This applies to the readiness path and every exit path. AC-01: add a `SecretToken` type (`Zeroizing<String>`, redacted `Debug`, no `Display`, no `Clone`) for the `VerifiedTokens` access, id and refresh tokens. Both land in M1 because the log sink (M1.7) and the instance lock (M1.8) depend on them.
Design refs: 07 §A5 AC-01, AC-05; F04
Closes findings: AC-05, AC-01, PL-07 (part), UI-11 (exit park part)
Key tests:
- With a stalled quiesce fake, the exit path returns within the bound.
- `format!("{:?}", tokens)` contains no token text.
User-visible/validatable behavior:
- Closing the desktop always ends the `aeris_desktop` process within the bound; no zombie process remains.

### M1.5 platform-sys-durable-writer
Title: New `platform_sys` crate with the single durable file writer and unsafe isolation rule
Milestone: foundations   Crates: new crates/platform_sys, platform_runtime, tools/naming_check   Depends on: M1.1
Scope: Create the leaf crate `platform_sys`:
- Lints: `unsafe_code = "deny"`, with `allow` only in `src/windows/*` and `src/macos/*`; deny `undocumented_unsafe_blocks` and `multiple_unsafe_ops_per_block`; every block carries a `// SAFETY:` comment.
- `fs::{StagedFile, write_file_durably, replace_file, remove_stale_temporaries}`: a temp file in the same directory, `sync_all`, `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)`, a parent fsync on Unix, and temp removal on Drop.
Move the `atomic_file.rs` FFI here and delete it from `platform_runtime`; `platform_runtime` re-exports `fs`.
naming_check:
- `unsafe` and `allow(unsafe_code)` appear only under `crates/platform_sys/src`, with an interim allowlist for the remaining `platform_runtime` FFI files (removed in M8.3).
- Allowed dependents: `platform_runtime`, `trading_runtime` and `observability`.
Design refs: 07 §B3, §B6, F01
Closes findings: PL-02 (start), PL-06 (primitive), LC-10 (primitive)
Key tests:
- Unit tests for commit, replace-existing, drop cleanup and pre-existing stale temp.
- A child process killed between write and commit leaves the old content, never an empty file.
User-visible/validatable behavior:
- Internal only.

### M1.6 adopt-durable-writer
Title: Replace every hand-rolled file writer, including the non-atomic trading export
Milestone: foundations   Crates: trading_runtime, apps/desktop, chart_integration, platform_runtime, launcher bin, tools/naming_check   Depends on: M1.5
Scope: Adopt `platform_sys::fs` for:
- `local_state.rs:175-213` and `chart_chrome.rs:765-815`. Drop the `.bak` dance; the live file is never removed.
- The trading export `write_atomic` (`store.rs:3373-3383`). Urgent: today it deletes the target and renames with no fsync.
- The readiness report writers (`desktop.rs:4095`, `readiness_conformance.rs:756`) and `live_market_gate.rs:263-266`.
- Lifecycle `write_json_atomic`, and the launcher writes and download commits (`:300-323`, `:596-599`, `:946-949`, `:1420-1425`, `:1595`, `:1640`).
naming_check rule: no `fs::write(` or `fs::rename(` in production code of the desktop, `trading_runtime`, `account_runtime`, `platform_runtime` and `chart_integration`, except an explicit allowlist (lifecycle directory moves).
Design refs: 07 §B3, F02
Closes findings: PL-06, LC-10, LC-11 (durable-write part)
Key tests:
- A trading export crash test keeps the previous export intact.
- Existing persistence and restore tests pass.
- A loader test confirms no `.bak` fallback is needed.
User-visible/validatable behavior:
- Headless: a trading export produces a complete file.
- GUI: workspace and chart chrome still restore after a restart.

### M1.7 diagnostics-log-sink-panic-hook
Title: Rotating log file, panic crash files and a fatal-startup dialog
Milestone: foundations   Crates: observability, platform_sys (ui::message_box), apps/desktop, launcher bin, tools/naming_check   Depends on: M1.4, M1.5
Scope: `install_log_sink(LogSinkConfig)`:
- A bounded `sync_channel(1024)` feeds a writer thread. On overflow a dropped counter increments and the next written line reports the count.
- Files rotate at 4 MiB, keeping 5, under `native_data_root()/logs` (launcher: `<install_root>/logs`). stderr output is kept.
`install_panic_hook` chains the previous hook and synchronously writes `crash-<unix_ms>.log` (message, location, thread, backtrace, release identity) through `StagedFile`. Fatal startup errors (`desktop.rs:5439-5504`) open a `MessageBoxW` dialog that names the log path. A debug-only flag forces a panic for validation. naming_check: no `{:?}` of `*Token*`/`*Secret*` types.
Design refs: 07 §B1, F03
Closes findings: PL-03
Key tests:
- Rotation and overflow unit tests.
- A re-spawned child test binary panics, and the crash file exists.
- The log contains no secret text.
User-visible/validatable behavior:
- A launch with a validation folder creates `logs/aeris.log` with timestamped lines.
- A forced startup failure shows a dialog that names the log path.

### M1.8 single-instance-and-validation-mode
Title: Per-data-folder instance lock and validation mode with an isolated vault namespace
Milestone: foundations   Crates: platform_runtime (data_root, credential_vault), apps/desktop   Depends on: M1.4, M1.7
Scope: Move `native_data_root` and its siblings out of `lifecycle.rs` into `platform_runtime::data_root`.
Validation mode:
- An explicit data-root argument or environment override plus `--validation`.
- It selects an isolated credential-vault namespace keyed by the data root.
- Reading the user's real vault requires an explicit opt-in flag.
`InstanceLock::acquire(root)` uses `File::try_lock` on `<root>/instance.lock`:
- It is acquired right after argument parsing, before any store opens, and held until the trading and context stores shut down.
- Readiness also acquires it and fails when it is held.
- A second instance shows an "Aeris is already running" dialog and exits 0.
Design refs: 07 §B2, F05; mission "Single instance", validation boundaries
Closes findings: PL-04
Key tests:
- Acquire, then a second acquire fails, then acquire succeeds after drop.
- Two different roots lock independently.
- A fake vault shows that validation mode never reads the default namespace without the opt-in.
User-visible/validatable behavior:
- A second launch on the same validation folder shows the dialog and exits.
- Two instances on different validation folders run side by side.
- A validation instance shows no stored provider credentials.

### M1.9 sqlite-hardening-migrations
Title: SQLite busy timeout, integrity check, quarantine, table-driven migrations and pre-migration backup
Milestone: foundations   Crates: trading_runtime, apps/desktop   Depends on: M1.6, M1.7
Scope: At open:
- Set `busy_timeout=5000`.
- Run `PRAGMA quick_check`, and record its measured cost on a realistic journal.
- A failure returns `StoreError::Corrupt`. The desktop dialog shows the path and the newest backup. Quarantine to `trading.corrupt-<ts>.*` happens only after the user confirms. The journal is never silently recreated.
Migrations:
- Replace the chained `if user_version < N` blocks with `MIGRATIONS: &[Migration{from, apply}]`, one transaction per step, plus a contiguity test.
- Run `VACUUM INTO <data>/backups/trading-v{old}-{unix}.sqlite` before the first step; keep the newest 3.
- Export `pub const SCHEMA_VERSION`.
- Run `wal_checkpoint(TRUNCATE)` on clean shutdown.
- Keep the newer-version guard.
Design refs: 07 §B4, F10; 09 §5.2 (backup rule)
Closes findings: PL-05
Key tests:
- Every historical fixture version migrates to head.
- A truncated or corrupt file returns `Corrupt` and stays byte-identical.
- A backup exists before migrating.
User-visible/validatable behavior:
- A corrupted `trading.sqlite3` in a validation folder produces an error dialog and the file is unchanged.
- A v18 fixture database migrates and a backup appears in `backups/`.

---

## M2 market-foundations

### M2.1 market-characterization-tests
Title: Characterization tests at the coordinator command/event boundary
Milestone: market-foundations   Crates: market_runtime, tools/naming_check   Depends on: M1.3
Scope: Add boundary-level tests (no private-map inspection) for current behavior:
- reseed under live flow keeps one forming bar and contiguous history;
- outbox overflow produces a covering snapshot;
- a stale-generation history completion is ignored;
- a symbol or timeframe change does not reconnect the provider session.
Register the required test names in naming_check.
Design refs: 10 §11 #1
Closes findings: MD-17 (part)
Key tests:
- The four scenarios pass on unchanged production code.
User-visible/validatable behavior:
- Internal only.

### M2.2 market-benchmark-baseline
Title: Hot-path and coordinator benchmark baseline
Milestone: market-foundations   Crates: market_engine (examples), market_runtime (examples)   Depends on: -
Scope: Extend `market_engine/examples/market_data_performance.rs` with:
- snapshot-per-publish (50k bars, 10k trades/s);
- study-per-tick copies;
- allocation per publish, counted with a safe counting allocator or capacity deltas.
Add a `market_runtime` coordinator example covering 64 series idle and loaded, and outbox overflow. Record the numbers in the tracking doc.
Design refs: 10 §7, §11 #2
Closes findings: MD-05 (measurement only)
Key tests:
- Both examples run and print stable numbers.
User-visible/validatable behavior:
- Internal only (baseline numbers recorded).

### M2.3 market-injectable-clock
Title: Injected clock for the market coordinator
Milestone: market-foundations   Crates: market_runtime   Depends on: M2.1
Scope: Add `market_service/clock.rs`: `Clock` (monotonic and wall), `SystemClock`, and `ScriptedClock` (cfg(test) or feature `replay`). Replace direct `Instant::now()` and `now_nanos` in `TradeLiveHandoff`, the watchdog, history retry and the session-status paths. The coordinator receives the clock at construction.
Design refs: 10 §11 #5
Closes findings: (replay prerequisite)
Key tests:
- Watchdog and retry tests driven by `ScriptedClock`, with no sleeps.
User-visible/validatable behavior:
- Internal only.

### M2.4 reconnect-policy
Title: One jittered, capped, budgeted reconnect policy
Milestone: market-foundations   Crates: market_runtime, adapters (rithmic)   Depends on: M2.3
Scope: `reconnect.rs` adds `ReconnectPolicy` and `Backoff`: exponential, jitter seeded through `Clock`, cap, attempt budget, healthy reset, and a `RateLimited{retry_after}` override.
Per-provider defaults:
- cTrader: current values, equal jitter, budget 5.
- tastytrade: current values plus jitter.
- Hyperliquid: 500 ms x 2^n, cap 30 s, full jitter, no budget.
- Rithmic: the adapter scheduler stays the only transport retry. `serve_demand` non-transport recovery goes through `Backoff` (min 1 s), so the policies no longer stack, and the login limits are respected.
An exhausted budget gives an explicit provider `Failed` state. "Market data retry limit reached" is replaced with text that states the real cause. `ctrader_venue` reuses `Backoff`.
Design refs: 10 §6, §11 #4
Closes findings: MD-12
Key tests:
- Deterministic backoff schedule per provider.
- Rithmic single policy.
- Budget exhaustion produces a typed failure.
User-visible/validatable behavior:
- With the network blocked for a validation instance, the log shows Hyperliquid reconnect attempts at growing intervals instead of every 250 ms.

### M2.5 market-calendar-service
Title: Provider-neutral market calendar and session service
Milestone: market-foundations   Crates: market_runtime, domain/market_data, adapters/rithmic_protocol   Depends on: M2.3
Scope: The `calendar/` module provides `MarketCalendarService` (`phase`, `session_window`, `next_transition`) over the sources ProviderCalendar, ContractHours, ExchangeRule, AlwaysOpen and Unknown.
- Move the CME rules and their tests from `rithmic_protocol/src/calendar.rs`.
- Move `session_hours.rs` to `weekly_hours.rs`.
- Add one venue normalization table, replacing `market_collection`, `tastytrade_market_collection` and `for_venue`.
- Add the domain types `SessionPhase`, `SessionWindow` and `CalendarSourceKind` (no new dependencies).
- `market_session_status` delegates to the service.
- An unsupported week or month period without a calendar returns an explicit `MarketDemandError` instead of silence.
- `resolve_cadence` (M1.3) now takes `CalendarSourceKind`.
naming_check: no provider literals in `calendar/mod.rs`, and generic code does not import `RithmicExchangeCalendar`.
Design refs: 10 §4, §11 #6
Closes findings: MD-04 (demand part), MD-09
Key tests:
- Calendar matrix: CME Globex with DST, CFE, US equities, crypto 24/7, FX contract hours, Unknown.
- A tastytrade equity 1W demand either goes live or returns an explicit error.
User-visible/validatable behavior:
- Internal; validated by the runtime tests (no silent non-updating weekly charts).

### M2.6 provider-neutral-watchdog
Title: Stall watchdog and practice session answer for every provider
Milestone: market-foundations   Crates: market_runtime   Depends on: M2.3, M2.5
Scope: The watchdog evaluates every live series of every model (trade-built and candle). It declares a stall only when the calendar phase is Open and there has been no progress past the threshold; Unknown uses a conservative long threshold. The coordinator no longer calls `broker_api.market_session_status`. `require_practice_market_session` answers from the calendar for all providers (it is deleted in M4.8).
Design refs: 10 §4.3, §11 #7
Closes findings: MD-02 (market part)
Key tests:
- A cTrader candle series stalled while open triggers recovery.
- No recovery while closed.
- A scripted Rithmic CME series stalled while open triggers recovery.
User-visible/validatable behavior:
- Internal; validated by the tests and by M3.11 replay.

### M2.7 calendar-session-live-bars
Title: Session-day, weekly and monthly live bars follow the calendar
Milestone: market-foundations   Crates: market_runtime   Depends on: M2.5
Scope: `LiveCadence::Session{days, calendar}` closes bars at the calendar session roll. Delete the `Session -> Fixed{days*86400}` mapping (`realtime.rs:456`). Week and Month cadences use the calendar for every venue that has one, and the tape `trading_day` comes from the calendar.
Design refs: 10 §2.5, §11 #8
Closes findings: MD-03, MD-04 (live part)
Key tests:
- CME fixture trades at 16:59 and 17:01 CT produce two session bars that match the provider daily bars, including a DST week.
- A non-CME weekly live update.
User-visible/validatable behavior:
- CME daily bars roll at 17:00 CT (tests).
- Hyperliquid daily charts are unchanged (GUI spot check).

### M2.8 typed-epoch-fencing
Title: `Epoch<Tag>` and `Fenced<T>` replace raw generations at the provider boundary
Milestone: market-foundations   Crates: market_engine, market_runtime, adapters   Depends on: M2.1
Scope: Add `Epoch<Tag>` (NonZeroU64, Copy, Ord, no arithmetic) with ProviderTag, DemandTag, CommandTag and ConsumerTag next to `GenerationId`/`ProviderGeneration`, plus the `Fenced<T>` envelope.
- `ProviderEvent.generation: u64` becomes `Epoch<ProviderTag>`. Only the registry mints it; adapters cannot fabricate one.
- History completions carry epochs.
- One `accepts()` per boundary, comparing for equality. Stale values are dropped and counted.
naming_check: no `generation: u64` in `market_runtime` public events.
Design refs: 10 §3, §11 #9
Closes findings: MD-15 (part); root cause 2 (market side)
Key tests:
- A stale-epoch provider event and a stale history completion are dropped and counted.
User-visible/validatable behavior:
- Internal only.

### M2.9 command-fences-consumer-registry
Title: Command fences and one consumer registry; remove the startup sentinel
Milestone: market-foundations   Crates: market_runtime, apps/desktop, tools/naming_check   Depends on: M2.8
Scope: Collapse `catalog_searches`, `startup_catalog_searches`, `catalog_selections`, `startup_catalog_selections` and `catalog_screens` into `CommandFences`, keyed by (ConsumerId, ProviderId, CommandKind) with `CommandKind::StartupResolve`. Delete the desktop `STARTUP_CATALOG_COMMAND_GENERATION = u32::MAX` sentinel. Extract `ConsumerRegistry` with one `remove_consumer`, replacing the duplicated cleanup in `detach_client` and `handle_remove`. Update the `StartupResolution` naming_check assertions to the epoch-fenced form.
Design refs: 10 §1.2, §3.2, §11 #9; 01 MD-13
Closes findings: MD-13 (fence and cleanup part)
Key tests:
- Removing a consumer leaves zero entries in every per-consumer map.
- A superseded startup search result is dropped.
User-visible/validatable behavior:
- GUI smoke: a validation workspace restores its panes and symbol search returns results.

### M2.10 history-tracker
Title: `HistoryTracker` with current and backfill lanes; trim unused `provider_history`
Milestone: market-foundations   Crates: market_runtime, provider_history   Depends on: M2.4, M2.8
Scope: `history/tracker.rs` replaces the eight coordinator maps and the retry tuple (`attempts = 0` keep-alive):
- current and backfill lanes, with lane states Idle, Deferred, InFlight and RetryAt plus a `Backoff`;
- `Coverage` and a `detached` flag;
- `HistoryTicket` carrying the epochs;
- one `retain_demanded` prune.
The worker uses a blocking bounded send instead of the 1 ms spin and expresses priority as lanes (every 4th slot goes to the oldest backfill). Per the user decision, trim the unused `provider_history` parts and keep only what the Rithmic adapter uses. Fix the misattached doc comments in `history.rs`.
Design refs: 10 §2.6, §11 #10, #20 (Q1)
Closes findings: MD-07, MD-16 (history doc comments)
Key tests:
- A backfill failure does not affect current-edge recovery.
- A cancellation removes all state.
- Tests target the tracker API, not private maps.
User-visible/validatable behavior:
- GUI: scrolling back on a Hyperliquid chart still pages older history.

---

## M3 market-lifecycle

### M3.1 series-lifecycle-candle-path
Title: One per-series state machine, ported for candle providers
Milestone: market-lifecycle   Crates: market_runtime, tools/naming_check   Depends on: M2.6, M2.7, M2.10
Scope: Add `lifecycle/`:
- `SeriesLifecycle {state, model, forming}`;
- `SeriesState`: Requested, Seeding, Live, Dormant, Recovering{reason}, Failed;
- a pure `step(input, ctx, &mut EffectBuf)` with fixed-capacity buffers;
- `FormingBar` as the single owner of the forming candle;
- a published runtime presentation status (Loading, ConnectingLive, Live, Recovering, AwaitingData).
Port cTrader and Hyperliquid, and delete the `candle_*` handlers and `publish_candle_live`. naming_check: assert `series: lifecycle::SeriesLifecycles`, and no `Instant::now()` in `lifecycle/`.
Design refs: 10 §2.1-2.5, §11 #11
Closes findings: MD-06 (candle half), MD-15 (presentation-state part)
Key tests:
- Transition-table unit tests on `ScriptedClock`.
- The M2.1 characterization tests still pass.
User-visible/validatable behavior:
- Hyperliquid charts load, go live and recover after a reconnect (GUI and M3.12 probe).
- The cTrader demo chart loads.

### M3.2 series-lifecycle-trade-path
Title: Port trade-built providers and delete the duplicated lifecycle
Milestone: market-lifecycle   Crates: market_runtime   Depends on: M3.1
Scope: Port tastytrade (Indexed continuity) and Rithmic (Sequence continuity) to `LiveModel::TradeHandoff`. Delete:
- the `rithmic_*` handlers and `publish_rithmic_live`;
- `SeriesLive`, `SeriesLiveMap`, `LiveHistoryState` and `TradeLiveHandoff`;
- the `history.rs` `start_*`/`seed_*`/`finish_*` duplicates.
Also:
- cache the provider wire symbol in the demand instead of calling `format!` per event;
- prune recent trades by time for Indexed continuity;
- fix the double `demand_dirty`;
- fix the "Rithmic" text on tastytrade paths.
naming_check asserts that `SeriesLiveMap` is absent.
Design refs: 10 §2.5, §11 #12
Closes findings: MD-06, MD-14, MD-16 (prune, demand_dirty)
Key tests:
- The same table-driven lifecycle suite runs against both models.
- A scripted Rithmic sequence gap leads to Recovering.
User-visible/validatable behavior:
- Internal (tastytrade and Rithmic are validated with scripted sessions; Rithmic live is excluded).

### M3.3 engine-series-tail
Title: Series-tail accessor; remove full-series copies from hot and existence paths
Milestone: market-lifecycle   Crates: market_engine, market_runtime   Depends on: M2.2, M3.2
Scope: Add `MarketEngine::series_tail(key, n)`, `series_contains_time` and a segmented view for studies. Replace the full copies at:
- `study.rs:3618`, `3733`, `3747` and `3769`;
- `realtime.rs:1766`;
- the existence checks in `history.rs` and `publication.rs:856`.
Collapse the five `execute_study_*` wrappers into one.
Design refs: 10 §7 (change 1), §11 #13; 01 MD-05
Closes findings: MD-05 (copy part), MD-13 (study wrappers)
Key tests:
- The benchmark shows publish bytes independent of history length, compared with the M2.2 baseline (numbers in the tracking doc).
- Study outputs are unchanged.
User-visible/validatable behavior:
- Internal (measured numbers recorded).

### M3.4 coordinator-dirty-publication
Title: Dirty-set publication and deadline timers in the coordinator loop
Milestone: market-lifecycle   Crates: market_runtime   Depends on: M3.3
Scope: Replace the unconditional per-tick sweeps (live publish, watchdog, overflow recovery, per-descriptor flush, idle stop, history retry) with:
- a dirty set of series touched since the last publish;
- a `BTreeMap<MonotonicInstant, TimerKey>` for watchdog, retry and session-transition deadlines.
`recv_timeout` waits until the next deadline, capped at 16 ms only while the dirty set is non-empty.
Design refs: 10 §7 (change 3), §11 #13
Closes findings: MD-05 (idle CPU part)
Key tests:
- On `ScriptedClock`, an idle coordinator has zero wakeups between deadlines.
- 64-series idle and loaded CPU are recorded against the baseline.
User-visible/validatable behavior:
- Idle CPU with charts open is measured lower; before and after numbers are in the tracking doc.

### M3.5 provider-capabilities
Title: `ProviderCapabilities` and `ProviderPresentation` replace descriptor flags and literals
Milestone: market-lifecycle   Crates: market_runtime, domain/market_data, adapters/rithmic   Depends on: M3.2
Scope: `ProviderCapabilities` (live_model, history caps, calendar capability, always_open, concurrency limits, reconnect policy, depth, quotes) and `ProviderPresentation` replace about 25 `ProviderDescriptor` flags.
- Remove provider literals from realtime, runtime, coordinator, history and publication.
- Fix the Rithmic presentation values (`crypto_public_realtime`, `" spot"`).
- Move `ChartInterval::rithmic_aggregation` into the Rithmic module, after verifying its callers (10 Q8).
- Fix the misattached doc comment at `mod.rs:814`.
naming_check extends the provider-literal ban to `lifecycle/`, `history/`, `calendar/mod.rs`, `quotes.rs`, `outbox.rs`, `coordinator.rs` and `realtime.rs`.
Design refs: 10 §5, §11 #14
Closes findings: MD-08 (flags and literals), MD-16 (doc comment)
Key tests:
- naming_check literal-ban tests.
- Lifecycle and characterization suites stay green.
User-visible/validatable behavior:
- Internal only.

### M3.6 provider-session-registry
Title: `ProviderSession` trait and `ProviderRegistry`
Milestone: market-lifecycle   Crates: market_runtime   Depends on: M3.5
Scope: Add the `ProviderSession` trait (connect with an epoch, diff-based `apply_demand`, `request_history`, `release_idle`, `shutdown`) and a `ProviderRegistry` of `Box<dyn ProviderSession>` keyed by `ProviderId`.
- Delete the four-arm `ProviderRealtimeChannelSet`, `ProviderRealtimeDispatch` and catalog enums.
- Replace `ProviderSessionSlot` with `ProviderConnection {state, epoch, backoff}`.
- `stop_realtime_if_idle` becomes `release_idle`, and the idle-stop overlap check is per stopping provider.
- tastytrade `BrokerApi` no longer holds its state mutex across HTTP calls.
Design refs: 10 §5, §1.2, §11 #14
Closes findings: MD-08, MD-16 (idle overlap, BrokerApi mutex)
Key tests:
- A scripted registry records demand diffs.
- Symbol and interval changes cause no reconnect.
User-visible/validatable behavior:
- Switching symbols and intervals on Hyperliquid keeps one provider session; the log shows no reconnect.

### M3.7 instrument-leases-feed-contract
Title: Trading feed domain contract and MarketEngine instrument leases
Milestone: market-lifecycle   Crates: domain/trading, domain/instruments, market_engine   Depends on: M2.5
Scope: Add the domain module `aeris_trading::market`, with validation: MarketRoute, TradingQuote, FeedState, InstrumentSession, InstrumentTerms, QuoteInterest and MarketFeedEvent. Add `PriceGrid` and `InstrumentClass` to `ContractMetadata`. In `market_engine`, add `MarketDemandKey::{Series, Instrument}` and `instrument_lease.rs` (bounded, shared and released per owner), plus `instrument_subscriptions()`. `QuoteSubscription` is the lease guard; dropping it releases the demand.
Design refs: 09 §3.1, §3.4, §8 #6-7; 10 §8, §11 #15
Closes findings: groundwork for TR-01 and TR-02
Key tests:
- Lease share and release across multiple owners.
- Capacity overflow is an explicit error.
- Quote validation (bid < ask, scale equals the instrument scale).
User-visible/validatable behavior:
- Internal only.

### M3.8 trading-quote-feed
Title: Market-side trading quote feed driven by instrument demand
Milestone: market-lifecycle   Crates: market_runtime, tools/naming_check   Depends on: M3.6, M3.7
Scope: `trading_feed.rs` and the `quotes.rs` QuoteBook form the single trading quote surface:
- A QuoteInterest sink installs instrument leases through MarketEngine, and resolves or installs routes from the catalog.
- `provider_demand` and `reconcile_order_books` include `instrument_subscriptions()` with `MarketStream::Quotes`. Order-book demand becomes incremental and keyed by entitlement.
- Quote, Feed, Session and Terms events are emitted latest-wins through the attached `TradingFeedLink`.
- Terms conversion moves from the desktop into the runtime.
- Hyperliquid uses a significant-figure `PriceGrid` (09 Q12), verified against the docs and observed responses.
- Price alerts move to instrument demand.
Verify Quotes-only demand per provider: Hyperliquid bbo/l2Book, and cTrader spots on the shared session. tastytrade and Rithmic are scripted only. naming_check: still no `aeris_trading_runtime` in `market_runtime`.
Design refs: 09 §3.4, §8 #8; 10 §8, §11 #15
Closes findings: TR-01 and UI-01 (market side), MD-11 (trading-feed part), MD-13 (order-book demand)
Key tests:
- On a fixture provider, a lease with no chart subscribes top of book and emits Quote events.
- Dropping the lease unsubscribes.
- A provider reconnect emits a Feed state change.
User-visible/validatable behavior:
- A `market_runtime` example prints live Hyperliquid BBO for an instrument with no chart demand.

### M3.9 fenced-events-waitable-outbox
Title: Every consumer event is fenced; the bridge blocks on a waitable outbox
Milestone: market-lifecycle   Crates: market_runtime, apps/desktop (engine_market_worker), tools/naming_check   Depends on: M2.9, M3.4
Scope: Every consumer event is `Fenced<ConsumerEvent>`, with the generation required on the envelope: series deltas and snapshots, study outputs, removals and invalidations, price alerts, catalog results, order book, tape, recovery and viewport.
- `outbox.rs`: `ConsumerOutbox::wait(timeout)` on a Mutex plus Condvar. Capacity and the 1024 covering-snapshot overflow are unchanged.
- The bridge thread blocks on it, with one ordered command queue per endpoint and latest-wins coalescing.
- Delete `EVENT_WAIT = 2ms` and the `poll_events` round trip.
- Resolve 10 Q4: delete `MarketGeneration<T>` if `require_resnapshot` is still unused.
- The UI thread never waits on the condvar.
naming_check: every consumer event variant is fenced.
Design refs: 10 §3.2, §9.1, §11 #16-17; 11 contract M1/M4
Closes findings: MD-15, MD-11, UI-05 (runtime and bridge part)
Key tests:
- A study output from generation N-1 delivered after selection N is not published.
- A click followed by a queued command keeps its order.
- Wake latency p50/p99 is measured.
User-visible/validatable behavior:
- Symbol clicks remain responsive.
- The measured idle wakeups per second of the bridge drop to near zero.

### M3.10 select-and-demand-descriptors
Title: `select_and_demand` command and runtime-published instrument and provider descriptors
Milestone: market-lifecycle   Crates: market_runtime, apps/desktop (bridge only)   Depends on: M3.5, M3.9
Scope: `select_and_demand {pane, generation, InstrumentRef | DefaultInstrument, interval, viewport}` replies exactly once, with `Selected{generation, descriptor}` or `Failed{generation, error}`. It is transactional: the old demand is released only after the new one installs (answers 11 risk 1).
Runtime-published types:
- `InstrumentDescriptor`: display, venue, asset class, currency, tick and price scale, offered intervals, calendar source, quote capability, entitlement token, logo key.
- `ResolvedSeries`.
- `ProviderDescriptor`: opaque id, display name, logo key, connect method, login schema, symbol-browser capability.
- The typed `ProviderEvent::CatalogReady`.
Startup catalog resolution moves into the runtime. `WorkerEndpoint` mutex slots collapse to one outbox and one command sender. A runtime test checks that every registered provider has a complete descriptor.
Design refs: 10 §9.2-9.7, §11 #18; 11 contracts M2/M3
Closes findings: UI-04 (runtime part), MD-10 (series identity part)
Key tests:
- One reply per generation.
- A superseded selection never replies `Selected`.
- `DefaultInstrument` resolves without a gated login.
User-visible/validatable behavior:
- GUI: picking a symbol from search opens it in one step.

### M3.11 market-replay-harness
Title: Deterministic market replay harness with permanent scenarios
Milestone: market-lifecycle   Crates: market_runtime, tools/naming_check   Depends on: M3.6, M3.10
Scope: `ReplayHarness` (cfg(test) or feature `replay`) uses `ScriptedClock`, a `ScriptedRegistry` implementing `ProviderSession`, and `ReplayStep` scripts, with sanitized fixtures under `tests/fixtures/replay/`.
Scenarios:
- Day3 round trip;
- stall while open and while closed;
- CME roll across DST;
- non-CME calendar resolution;
- backoff schedules;
- stale-epoch completion;
- overflow covering snapshot;
- reseed during live flow;
- switch storm, gap recovery, history exhaustion and superseded generations.
Move the brittle private-map coordinator tests onto the harness. naming_check lists the required replay test names.
Design refs: 10 §10, §11 #19
Closes findings: MD-17
Key tests:
- The scenario list above.
User-visible/validatable behavior:
- Validators run `cargo test -p aeris_market_runtime --features replay replay_` and see every scenario pass.

### M3.12 hyperliquid-lifecycle-probe
Title: Live Hyperliquid market lifecycle probe with JSON evidence
Milestone: market-lifecycle   Crates: market_runtime (example) or tools   Depends on: M3.8, M3.10
Scope: A headless probe starts MarketService on an isolated data root and selects a Hyperliquid perpetual through `select_and_demand`. It then waits for Live, forces a reconnect, switches interval and symbol, pages history back and takes a quote lease. It writes JSON evidence: transitions with epochs, timings, bar-continuity checks and the provider session count. It exits non-zero on any invariant failure.
Design refs: mission M3 "Replay harness" (live probe)
Closes findings: (validation surface)
Key tests:
- A live run against Hyperliquid, with its JSON recorded in the tracking doc.
User-visible/validatable behavior:
- The probe exits 0 and writes JSON evidence with one provider session and contiguous bars.

---

## M4 trading-core

### M4.1 trading-clock-command-time
Title: Injected trading clock; cancel events at command time
Milestone: trading-core   Crates: trading_runtime   Depends on: M1.9
Scope: Add `clock.rs` (`Clock`, `SystemClock`, `ManualClock`) and `TradingServiceConfig.clock`. Replace `current_unix_nanos()`, and make the clock monotonic per process. Cancel events use the command time (fixes `lib.rs:1917-1924`). The cancel-time change in `simulated_stop_golden.txt` is listed in the commit (09 Q1 policy).
Design refs: 09 §2.3, §3.5, §8 #1
Closes findings: TR-12 (cancel time)
Key tests:
- A regression test asserts the cancel event time equals the command time.
User-visible/validatable behavior:
- GUI: order history shows a cancel at the actual cancel time.

### M4.2 trading-core-owner-shell
Title: Thin owner thread around a synchronous `TradingCore`
Milestone: trading-core   Crates: trading_runtime, tools/naming_check   Depends on: M4.1
Scope: Move the owner loop and `Command` into `service/{mod,owner}.rs`. `TradingCore` exposes `handle`, `on_quotes` and `on_timer`, and the owner waits with `recv_timeout` until the next deadline. No behavior change; goldens unchanged. Update the naming_check contract strings in the same commit.
Design refs: 09 §1.1, §8 #2
Closes findings: TR-12 (god object, start)
Key tests:
- Existing goldens and restart tests are unchanged.
User-visible/validatable behavior:
- Internal only.

### M4.3 trading-replay-harness
Title: Deterministic trading replay harness
Milestone: trading-core   Crates: trading_runtime   Depends on: M4.2
Scope: `replay/` drives `TradingCore` with `ManualClock` and a temporary SQLite file. It provides `restart()`, a `crash_during_commit(n)` store hook, a transcript with golden compare, and seeded property runs (in-crate xorshift). Port the two existing golden scenarios with identical output.
Design refs: 09 §7.1, §8 #3
Closes findings: (test gap in 02)
Key tests:
- Ported goldens are identical.
- A restart scenario passes.
User-visible/validatable behavior:
- Internal only.

### M4.4 indexed-order-book
Title: Indexed order book; remove linear scans
Milestone: trading-core   Crates: trading_runtime   Depends on: M4.2
Scope: `book/OrderBook` indexes orders by client id, broker order id, open orders by (account, instrument), and executable orders by instrument. Remove the scans at `lib.rs:4109`, `2110` and `1897`, and `venue/owner.rs:128`. No behavior change.
Design refs: 09 §1.2, §8 #4
Closes findings: TR-05 (runtime lookups)
Key tests:
- An index-consistency property test.
- A 50k-order lookup benchmark.
User-visible/validatable behavior:
- Internal only.

### M4.5 trading-core-module-split
Title: Split `lib.rs` and the Coordinator into focused modules
Milestone: trading-core   Crates: trading_runtime, tools/naming_check   Depends on: M4.3, M4.4
Scope: A mechanical move of `lib.rs` (5,606 lines) and the Coordinator impl spread over `protection.rs`, `planned_leg.rs` and `venue/owner.rs` into `admission/`, `execution/`, `ledger/`, `risk/`, `groups/` (moved protection), `emergency.rs`, `venue/broker.rs` and `market/`. No behavior change.
Design refs: 09 §1.2, §8 #5
Closes findings: TR-12 (god object)
Key tests:
- Goldens and the scripted cTrader venue tests are unchanged.
User-visible/validatable behavior:
- Internal only.

### M4.6 trading-store-module-split
Title: Split `store.rs` by table family
Milestone: trading-core   Crates: trading_runtime, tools/naming_check   Depends on: M4.5
Scope: Split `store.rs` (3,690 lines) into `store/{mod,schema,orders,fills,ledger,groups,risk,user_records,retention,export}.rs` on top of the M1.9 migration table. Introduce the `StepTx` type; it is used by every step from M4.10. Move the rusqlite allowance paths in naming_check.
Design refs: 09 §1.2, §5.1, §8 #5
Closes findings: TR-12 (store size)
Key tests:
- Migration and persistence tests are unchanged.
User-visible/validatable behavior:
- Internal only.

### M4.7 runtime-owned-quotes
Title: Trading runtime owns its quotes; the desktop relay is removed
Milestone: trading-core   Crates: trading_runtime, apps/desktop (composition root only), tools/naming_check   Depends on: M3.8, M4.5
Scope: This is the consumer side of the M3.8 feed:
- `market/` adds a QuoteBook (validated quotes, generation, source_sequence) and a latest-wins `MarketFeedInbox`, replacing `PendingObservations`.
- Interest is recomputed after every step for instruments with open orders, inactive legs, non-flat positions or active groups (bounded at 256).
- `MarketRoute` is persisted on instrument rows, and instruments are registered from Terms.
- The desktop root wires `attach_market_feed` and `attach_trading_feed`.
- Delete `publish_simulated_market_observation`, the desktop `register_provider_instrument` calls and the relay in `publications.rs` (D9b).
- Fill rules are unchanged (adapter).
naming_check: the desktop does not reference `SimulatedMarketObservation`.
Design refs: 09 §3.4, §8 #9; 11 §9 D9b
Closes findings: TR-01, UI-01, MD-11 (trading part)
Key tests:
- Replay: a stop is placed, the chart is closed (no series demand), quotes keep arriving and the stop fills.
- After a restart, interest is republished and quotes resume.
User-visible/validatable behavior:
- GUI: place a stop on a Hyperliquid chart and close the pane. The stop still triggers when price crosses.

### M4.8 provider-neutral-gating
Title: Session and freshness gating for every provider
Milestone: trading-core   Crates: trading_runtime, market_runtime, apps/desktop   Depends on: M4.7
Scope: Gating rules (user decisions):
- Every order is rejected with `RejectReason::MarketClosed` when the phase is Closed, and with `SessionUnknown` when it is Unknown.
- Marketable placement and every fill require `FeedState::Live`, the current provider generation, and a quote age of at most 10 s for crypto or 60 s for other classes (`ExecutionPolicy`, configurable per `InstrumentClass`).
- Resting orders never fill while Closed.
- `eligible_after` replaces the `restored_orders` fence and the cross-process generation comparison.
This item introduces the minimal `RejectReason` enum, which is extended in M5.1. Delete `PracticeMarketSession`, `install_practice_market_session` and both `require_practice_market_session` functions.
Design refs: 09 §3.3, §8 #10 (overridden by the user decisions)
Closes findings: TR-02, MD-02 (trading part)
Key tests:
- Closed and unknown sessions reject.
- An 11 s crypto quote and a 61 s futures quote reject marketable orders.
- After a restart, no fill happens on a pre-restart quote.
User-visible/validatable behavior:
- GUI: an order on a closed cTrader demo market (weekend) or an unknown session shows a clear reject reason.
- Hyperliquid orders are accepted while quotes are fresh.

### M4.9 fill-accounting-consolidation
Title: One fill-accounting implementation (V20)
Milestone: trading-core   Crates: trading_runtime   Depends on: M4.6
Scope: The V20 migration adds `fill_accounting`, `trade_cycles` and the open-order, fill and event indexes, with backfill. One `apply_fill` in `ledger/position.rs` replaces `risk_trade_cycles`, `trade_pnl_cycles`, `completed_trade_pnl` and `fill_realized_pnl`. Round trips read from `fill_accounting`. The math is unchanged.
Design refs: 09 §4.1, §5.2 (V20), §8 #11
Closes findings: TR-11 (single PnL path)
Key tests:
- V19 fixture migration.
- Reversal through zero and scale-out: risk, journal, round trips and the account view all agree.
- Goldens unchanged except the row layout.
User-visible/validatable behavior:
- Journal and account panels show the same realized PnL after a reversal (headless test; GUI spot check).

### M4.10 facts-only-persistence
Title: Commit-then-apply steps; no durable writes per quote
Milestone: trading-core   Crates: trading_runtime   Depends on: M4.9
Scope:
- One `StepTx` per core step, commit then apply; a failed commit leaves memory unchanged.
- Unrealized PnL lives in memory only, with no `update_position` per quote.
- The revision bump is part of the step transaction.
- Retention moves to an hourly timer and deletes at most 5,000 rows per table per run.
- Benchmark quotes per second with an open position, before and after.
Design refs: 09 §5.1, §4.5, §8 #12
Closes findings: TR-06 (per-quote writes)
Key tests:
- After `crash_during_commit`, memory equals the last committed state.
- A pure quote step opens no transaction.
- Benchmark numbers are recorded.
User-visible/validatable behavior:
- Internal (measured throughput recorded).

---

## M5 trading-engine

### M5.1 order-lifecycle-typed-events
Title: Complete order state machine, revisions and typed reasons (V21)
Milestone: trading-engine   Crates: domain/trading, trading_runtime   Depends on: M4.10
Scope: Add the V21 columns and pure transitions in `book/lifecycle.rs`:
- statuses `Inactive` and `Expired`;
- events Triggered, Expired, Activated and Resized;
- `Order.revision` and the order timestamps;
- typed `OrderEventReason` and `RejectReason`;
- `ClientIdReused` when a client id is resubmitted with different parameters;
- a typed `RiskWarning`, with the runtime "RISK WARNING" prefix deleted.
The broker route uses the transition functions; the scripted cTrader tests stay unchanged. Replace the old goldens with v2 goldens in the harness and list each change (09 Q1).
Design refs: 09 §2.1-2.3, §5.2 (V21), §8 #13
Closes findings: TR-08 (part), UI-10 (runtime part of the risk warning)
Key tests:
- Transition-table tests.
- V20 to V21 fixture migration.
- An identical resubmission returns the existing order; different parameters are rejected.
User-visible/validatable behavior:
- Internal; visible through M6 typed notices.

### M5.2 admission-validator
Title: One admission validator for simulated and broker routes
Milestone: trading-engine   Crates: trading_runtime, domain/instruments   Depends on: M5.1
Scope: `admission::admit()` checks:
- `VenueCapabilities`: TIFs, client id length 50 on cTrader, native SL/TP, managed trailing;
- the price grid (fixed tick or significant figures) and the quantity increment;
- side relations (for example, a buy stop must be above the ask);
- open-order and quote-interest capacity.
It also resolves `RestingAt` and provides a no-op `BuyingPower` hook. The broker path stops duplicating checks (`venue/owner.rs:187-289`).
Design refs: 09 §2.6, §8 #14
Closes findings: TR-07 (validator), TR-08 (tick checks)
Key tests:
- One parity suite runs through both routes.
- Off-grid prices and wrong-side stops are rejected.
User-visible/validatable behavior:
- GUI: an off-tick price or a wrong-side stop in the order ticket is rejected with a specific reason.

### M5.3 execution-fill-rules
Title: Bid/ask fill rules, stop and stop-limit triggering, multipliers
Milestone: trading-engine   Crates: trading_runtime   Depends on: M4.8, M5.2
Scope: The `execution/` SimulatedVenue implements:
- Placement-time fills: market at the touch, marketable limit at the touch, and IOC/FOK all-or-nothing at placement.
- Market orders wait up to 5 s for a quote, then `Cancelled{NoQuote}`.
- A resting limit fills at its limit price.
- Stops trigger on the correct side and fill at the gapped touch.
- A stop-limit gets a real triggered state, then rests at its limit.
- Contract multipliers come from `point_value`.
- Deterministic evaluation order, with group reactions running before the next candidate.
- A `trigger_index`, and `Liquidity`/`FillPrice` traits with touch-only implementations as hooks.
Design refs: 09 §3.2, §8 #15
Closes findings: TR-08
Key tests:
- Stop-limit gap-through.
- Marketable limit versus resting limit price.
- IOC/FOK.
- Multiplier PnL.
- An OCO sibling cancelled within the same quote.
User-visible/validatable behavior:
- On the paper probe, a resting limit fills at its limit and a market buy fills at the ask (JSON output).

### M5.4 time-in-force-expiry
Title: DAY expiry at session close or 00:00 UTC; GTC stays working
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.3
Scope: DAY `expires_at` is fixed at acceptance: the session close for calendared instruments, or 00:00 UTC for AlwaysOpen crypto. An owner timer expires the order. Startup recovery expires DAY orders whose session ended while the app was closed, and assigns `expires_at` to migrated open DAY orders at first start. GTC never expires. An unknown session is already rejected (M4.8).
Design refs: 09 §3.2 (TIF), §3.5, §8 #15; user decision (Q2)
Closes findings: TR-08 (DAY never expired)
Key tests:
- DAY expires at session end and across a restart.
- A crypto DAY order expires at 00:00 UTC on `ManualClock`.
User-visible/validatable behavior:
- GUI and probe: a DAY order's status shows Expired after its boundary passes (harness test proves the timing).

### M5.5 order-groups-and-bracket-migration
Title: One order-group model with migration of existing brackets (V22)
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.3
Scope: V22 tables and `groups/` for Bracket, OCO and Protection groups:
- Legs are real `Inactive` orders, activated at fill offsets (Q11: offsets kept).
- Sibling reductions and cancel cascades.
- A coverage invariant across groups. Per the 09 Q9 proposal, stops grow on scale-in and targets do not; this is recorded as an assumption.
- Management modifies the stop only on moves of one tick or more.
- Broker brackets are unchanged.
`store/migrate_groups.rs` migrates the existing `managed_brackets` and `protective_orders` rows. Unpriceable rows become Cancelled groups with a `RecoveryNotice`. `core/recovery.rs` verifies the groups at startup.
Design refs: 09 §2.4, §5.2 (V22), §8 #16; user decision (migrate brackets)
Closes findings: TR-07 (model)
Key tests:
- A V19 fixture with awaiting, active and completed brackets migrates exactly.
- Golden: bracket stop after restart.
- Coverage property runs.
User-visible/validatable behavior:
- GUI with a copied fixture database in a validation folder: existing practice brackets survive the upgrade and their legs are drawn on the chart.

### M5.6 remove-protection-layers
Title: Delete the superseded protection layers
Milestone: trading-engine   Crates: trading_runtime, apps/desktop   Depends on: M5.5
Scope: Delete:
- `protection.rs` and `planned_leg.rs`;
- `ManagedBracket` and its status, `ProtectiveOrder` and its role;
- `PlannedBracketLeg`, `MovePlannedBracketLeg` and `BracketPlacement`;
- interrupted-bracket repair, `align_loaded_protection` and the restored-order fence.
The desktop sends `RestingAt` plus `AttachedProtection`; `resting_entry_order_type` is deleted. Views derived from groups serve the desktop until M6.11.
Design refs: 09 §8 #17
Closes findings: TR-07 (cleanup)
Key tests:
- Harness and goldens stay green.
- Chart bracket placement still works through the desktop harness.
User-visible/validatable behavior:
- GUI: placing a bracket from the chart still shows the entry, stop and target lines.

### M5.7 cash-ledger-and-fees
Title: Fill-driven cash ledger with configurable fees, default zero (V23)
Milestone: trading-engine   Crates: trading_runtime, domain/trading   Depends on: M5.5
Scope: The V23 migration adds:
- ledger entries and fee schedules;
- positions rebuilt with an exact i128 cost basis;
- instrument class, price grid and market route columns.
Ledger behavior:
- Deposit entries come from the starting equity.
- Fills produce RealizedPnl, Commission and ExchangeFee entries.
- `FeeSchedule` applies per class with per-instrument overrides, default `FeeModel::None`, set through `set_fee_schedule`, with maker/taker.
- Balance = deposits + realized - fees. Equity = balance + unrealized.
Broker balances stay authoritative. A migrated instrument without a route shows "feed unavailable" (09 Q10).
Design refs: 09 §4.1-4.3, §5.2 (V23), §8 #18
Closes findings: TR-11
Key tests:
- A ledger-invariant property run.
- The V22 to V23 migration preserves open positions exactly.
- With zero fees the goldens change only in their ledger lines.
User-visible/validatable behavior:
- Probe: with a configured fee, the balance drops by exactly the fee.
- Account panel figures match the ledger.

### M5.8 checkpoints-retention-equity
Title: Checkpoints before retention, repair from checkpoints, bounded equity history
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.7
Scope: Position and ledger checkpoints are written in the same transaction before retention prunes fills. Retention runs bounded, hourly and at startup. Repair is checkpoint plus later fills; it replaces `rebuild_position_from_retained_fills`. Equity samples:
- a durable table written by the 60 s timer when equity changed, at each fill, and at session end;
- an in-memory ring of 1,440 samples;
- downsampling to 7 days at 1 minute and 1 year at 1 hour.
Design refs: 09 §4.4-4.5, §8 #18
Closes findings: TR-06 (retention and repair)
Key tests:
- Retention then repair equals the projection before retention.
- Equity sample counts stay within their bounds.
User-visible/validatable behavior:
- Internal (headless tests).

### M5.9 emergency-operations
Title: Kill switch, flatten scopes and atomic reverse across all instruments
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.5, M5.7
Scope:
- The kill switch locks the account, cancels non-protective orders, flattens every instrument at runtime quotes, cancels the protection of flattened positions and reports unpriced positions.
- `FlattenScope{account, instrument}`, and flatten-all covers every instrument.
- Economic-event flatten needs no observation.
- Reverse is one admitted operation; the close and open fills commit in one step.
- Broker accounts close every instrument.
Design refs: 09 §2.5, §8 #19
Closes findings: TR-03, TR-04, TR-12 (reverse)
Key tests:
- Multi-instrument flatten and kill switch.
- A reverse rejected by risk leaves the position untouched.
- Unpriced positions are reported.
User-visible/validatable behavior:
- GUI and probe: flatten-all with positions on two instruments, one of them with no chart open, flattens both.

### M5.10 risk-session-rollover
Title: Persisted risk session with rollover; bracket legs count as attached stops
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.7
Scope: `risk_sessions` is persisted and rolls over at the account boundary (09 Q8 proposal: 17:00 America/Chicago for futures accounts, 00:00 UTC otherwise; recorded as an assumption). Bracket stop legs count as attached stops, so the "no attached stop" warning no longer fires for brackets.
Design refs: 09 §8 #20; 02 TR-09
Closes findings: TR-09 (runtime part)
Key tests:
- Rollover across a boundary and a restart.
- A bracket entry raises no warning.
User-visible/validatable behavior:
- GUI: a bracket order no longer produces a risk warning.

### M5.11 trading-publication-api
Title: Change-driven, bounded publication with paged history
Milestone: trading-engine   Crates: trading_runtime   Depends on: M5.1, M5.7
Scope:
- `subscribe()` returns a `TradingSubscription` with a bounded event queue of 1,024 and a `lagged` flag; at most 8 subscribers.
- `WorkingSet` carries no history.
- `TradingEvent` is typed.
- Mark-only publications are coalesced to at most one per 100 ms.
- A history reader thread serves paged `HistoryQuery` on a read-only connection.
- `snapshot()` stays as a compatibility view until M6.11.
- Fixes the 10k-order snapshot failure.
Design refs: 09 §6.1, §8 #21
Closes findings: TR-05 (runtime)
Key tests:
- With 50,000 orders of history, working-set build time stays within a bound.
- An idle runtime publishes nothing.
- The lagged path.
User-visible/validatable behavior:
- Internal until M6.10 (headless test).

### M5.12 paper-trading-live-probes
Title: Live paper-trading probe on Hyperliquid with restart, plus cTrader demo regression
Milestone: trading-engine   Crates: new tools/paper_trading_probe, tools/naming_check   Depends on: M5.4, M5.9, M5.11
Scope: A composition root (allowed by naming_check) runs on Hyperliquid:
- a practice account with a fee schedule (or `--no-fees`);
- a market buy, a resting limit, a stop-limit and a bracket;
- a restart on the same file, verifying an identical working set and ledger;
- flatten-all and the ledger invariant;
- a JSON summary; it exits non-zero on failure or when no quote arrives within 30 s.
A cTrader demo mode on the stored connection checks broker-route parity (demo orders allowed).
Design refs: 09 §7.2, §8 #24
Closes findings: (end-to-end validation)
Key tests:
- Live runs recorded in the tracking doc.
User-visible/validatable behavior:
- The probe exits 0 with JSON showing fills, restart equality and a flat account.
- The cTrader demo mode exits 0.

---

## M6 desktop-bridge

### M6.1 desktop-services-test-support
Title: Inject `DesktopServices`; move `FixtureMarketWorker` to test support
Milestone: desktop-bridge   Crates: apps/desktop, tools/naming_check   Depends on: M3.10
Scope: `DesktopServices` (market factory, trading handle, account gate, persistence client, clock) replaces the process globals `trading::handle()`, `DesktopAccount::shared()` and `latest_feedback()`. Add `desktop/test_support/` with `PaneHarness` and `FixtureMarketWorker` under cfg(any(test, feature="diagnostics")). naming_check: `FixtureMarketWorker` appears only in test or diagnostics code.
Design refs: 11 §1.1, §1.5, §8, §9 D0
Closes findings: UI-06 (fixture part)
Key tests:
- `PaneHarness` smoke test.
- The naming_check rule has positive and negative tests.
User-visible/validatable behavior:
- Internal only.

### M6.2 notification-center
Title: One `UserNotice` model and `NotificationCenter`
Milestone: desktop-bridge   Crates: apps/desktop   Depends on: M6.1
Scope: `NotificationCenter` holds a bounded queue of `UserNotice` (key, severity, tone, source, title, detail, action, lifetime), and the toaster renders it.
- Delete the per-pane `indicator_message`, `symbol_message`, `price_alert_message` and `chart_state_message`.
- Handle the `let _ =` discards in the restore setters, `try_set_order_book_visible`, `process_pending_price_alerts` and the search paths.
- Remove the dead parameters and production `expect`/`unwrap` listed in UI-11.
- Notices are also logged, without secrets.
Design refs: 11 §5.1-5.2, §9 D1
Closes findings: UI-11 (most)
Key tests:
- A restore-setter failure posts exactly one pane notice.
- Dedupe and queue bounds.
User-visible/validatable behavior:
- GUI: errors appear as toasts or pane notices instead of per-pane text lines.

### M6.3 chart-binding-generation-fence
Title: Per-chart `ChartBinding` drops stale publications in one place
Milestone: desktop-bridge   Crates: apps/desktop   Depends on: M3.9, M3.10, M6.2
Scope: `ChartPaneController` and `ChartBinding` (generation from the M2/M3 epochs, `ChartTarget`, phases Requested, Installing, Live and Failed) receive every publication through the single `accept()`. Tape projection uses `AcceptedPublication.target`. Restore adopts the previous binding (transactional select).
Delete:
- `RithmicSwitchState`, `ready_state_can_complete_switch`, `stale_settled_state_during_engine_switch` and `awaits_replacement_chart`;
- `apply_rithmic_switch_marker` and `EngineSwitchMarker`;
- the `rithmic_pending_*` fields.
Rewrite the switch tests as interleaving tests.
Design refs: 11 §2.1-2.4, §2.6, §9 D5
Closes findings: UI-02, MD-15 (desktop part)
Key tests:
- Permutation and seeded interleavings of N and N+1 publications render only N+1.
- Failure restore.
- An interval-only switch stays uncovered.
User-visible/validatable behavior:
- GUI switch storm: rapid symbol and interval changes never show the previous symbol's bars or trades.

### M6.4 pane-presentation
Title: `PanePresentation` applies every user chart property in one place
Milestone: desktop-bridge   Crates: apps/desktop, chart_integration   Depends on: M6.3
Scope: `PanePresentation` holds all user-chosen chart properties, with one `apply()` used by the restore and replacement paths. Setter errors become one notice, and `PanePersistence` serializes it. Check whether the pinned Aeris Charts can reset data in place (11 §2.5); if not, record the operation for M7.9.
Design refs: 11 §2.5, §9 D5
Closes findings: UI-02 (repeat-fix chain A), UI-11 (restore setters)
Key tests:
- `PanePresentation` round trip.
- Style, colors and scale mode survive a switch.
User-visible/validatable behavior:
- GUI: chart style, colors and scale mode persist across symbol switches and restarts.

### M6.5 select-and-demand-panes
Title: One `select_and_demand` per selection; startup resolution leaves the desktop
Milestone: desktop-bridge   Crates: apps/desktop, tools/naming_check   Depends on: M6.3
Scope: Panes send one command per selection, and each restored pane sends one command with its persisted `InstrumentRef`. Delete the two-step selection and startup resolution in `selection_commands.rs`. Replace the naming_check `desktop_startup_catalog_resolution_stays_event_driven` with the runtime-side assertion.
Design refs: 11 §2.6, §9 D6
Closes findings: UI-04
Key tests:
- A restored pane sends exactly one command and applies one reply.
User-visible/validatable behavior:
- GUI: a restart with 4 panes restores all of them.

### M6.6 bridge-message-collapse
Title: Collapse the parallel desktop market message model
Milestone: desktop-bridge   Crates: apps/desktop   Depends on: M6.3
Scope: `MarketWorkerMessage` and the `ChartState` re-declarations collapse into the `Fenced<ConsumerEvent>` stream plus a GPUI-side projection. Delete the bespoke mailbox coalescing and linear-scan `enqueue` (`market_worker.rs:427-505`). Replace the `publications.rs:428` `expect` and the remaining bridge `runtime.rs` discards.
Design refs: 11 §9 D8; 10 §9.6
Closes findings: UI-05, UI-06, MD-16 (enqueue scans)
Key tests:
- Bridge tests with a fake outbox.
- Ordering preserved under load.
User-visible/validatable behavior:
- Internal only.

### M6.7 descriptor-driven-identity
Title: Render runtime descriptors; remove fake CLI modes; provider-neutrality rule
Milestone: desktop-bridge   Crates: apps/desktop, chart_integration, tools/naming_check   Depends on: M6.5
Scope: Render `InstrumentDescriptor`. Delete:
- the `replay_conversion` id parsing and entitlement building;
- the `worker_identity` branches;
- `default_rithmic_contract_index` and `default_product`;
- the `--rithmic` and `--multi-chart` fake modes (user decision).
Add naming_check `desktop_source_is_provider_neutral` with an interim allow-list whose stale entries fail.
Design refs: 11 §6.1-6.2, §9 D7a; 01 MD-10
Closes findings: UI-03 (identity), MD-10
Key tests:
- The naming_check rule has positive and negative tests.
- Descriptor rendering tests through `PaneHarness`.
User-visible/validatable behavior:
- `aeris_desktop --rithmic` exits 2 with "unsupported argument".
- Chart headers show the descriptor labels.

### M6.8 typed-provider-and-risk-events
Title: Typed provider and risk events; default workspace without an instrument
Milestone: desktop-bridge   Crates: apps/desktop, tools/naming_check   Depends on: M5.1, M6.7
Scope:
- A `ProviderDescriptor` list replaces `TerminalProvider` and `FALLBACK_TERMINAL_PROVIDER`.
- Delete `should_autoload_rithmic_catalog`; the desktop uses `CatalogReady` plus a descriptor capability instead.
- Delete the `starts_with("RISK WARNING")` match; the desktop consumes `TradingEvent::RiskWarning`.
- The default workspace has no instrument and sends `DefaultInstrument`.
naming_check:
- Add `desktop_control_flow_does_not_match_display_text`.
- Replace `every_runtime_provider_is_mapped...` with the runtime descriptor test.
Design refs: 11 §5.3, §6.1, §9 D7b
Closes findings: UI-10, UI-03 (enum and defaults)
Key tests:
- A typed risk warning becomes a Warning notice.
- A fresh workspace resolves the default instrument.
User-visible/validatable behavior:
- GUI: a fresh validation folder opens a default chart resolved by the runtime.
- Risk warnings appear as warning notices.

### M6.9 descriptor-login-and-symbol-browser
Title: Descriptor-driven provider connect, login form and symbol browser
Milestone: desktop-bridge   Crates: apps/desktop, tools/naming_check   Depends on: M6.8
Scope:
- `ConnectProvider(ProviderId)` replaces the Connect{Tastytrade, Ctrader, Rithmic} actions.
- `provider_login.rs` renders the form from the login schema and writes only to the native vault.
- `provider_symbol_browser.rs` follows the symbol-browser capability.
- The allow-list shrinks to `assets.rs`.
- Update `rithmic_logins_stay_in_the_native_vault` and `desktop_rendering_remains_component_owned`.
Design refs: 11 §6.1-6.2, §9 D7c
Closes findings: UI-03 (remaining)
Key tests:
- A vault-only write test for every `VaultCredentials` provider.
User-visible/validatable behavior:
- GUI: the connect menu lists providers from descriptors, and the cTrader stored connection still connects.
- The Rithmic form renders; no live login is attempted.

### M6.10 trading-store
Title: One workspace `TradingStore` fed by the change stream
Milestone: desktop-bridge   Crates: apps/desktop, tools/naming_check   Depends on: M5.11, M6.2
Scope: `TradingStore` is owned by `TerminalApp`. A background task reads `TradingSubscription` and applies updates through `cx.update`.
- Per-revision indexes; chart order lines, the bottom panel and history pages come from the store.
- Toasts come from the OrderFilled, OrderRejected, RiskLocked and ProtectionNotPlaced events.
- Command feedback is correlated by `RequestId`.
- The `TradingStore` is one workspace-level entity; trading ACCOUNT SELECTION STAYS PER PANE
  (architecture.md section 3.7.3 overrides the earlier workspace-level note; panes read
  `store.pane_view(account, instrument)`).
Delete `TradingPnlState`, `keep_trading_pnl_fresh`, `refresh_trading_pnl`, `schedule_trading_refresh`, the render-path poll, the 250 ms timer, `latest_feedback` and the `announce_fills` diffing. naming_check: no `service.snapshot()` in `components/`.
Design refs: 11 §1.4, §9 D9; 09 §6.2, §8 #22
Closes findings: UI-08, TR-05 (desktop), TR-10 (part)
Key tests:
- One fill produces one store change, one notice and one notify per affected pane.
- An idle minute produces zero trading notifies.
User-visible/validatable behavior:
- GUI: a fill toasts once.
- Panels update without polling.
- A fixture database with more than 10k orders shows a populated bottom panel.

### M6.11 trading-intents-and-snapshot-removal
Title: Revisioned chart intents and scoped emergency buttons; remove `TradingSnapshot`
Milestone: desktop-bridge   Crates: apps/desktop, trading_runtime, tools/naming_check   Depends on: M6.10, M5.9
Scope:
- Chart intents carry `order_id` and `expected_revision`; `StaleRevision` becomes a notice.
- The flatten, reverse and kill buttons send scopes and no longer need an order-book frame.
- Provider-named feedback text comes from descriptors.
Remove `TradingSnapshot`, the old command forms (`place_order`, `place_bracket`, `observe_market` and the rest) and the `MAXIMUM_SNAPSHOT_ITEMS` usage. Update naming_check.
Design refs: 09 §6.2, §8 #23; 11 §1.4
Closes findings: TR-10, UI-10 (trading part)
Key tests:
- Dragging a filled order returns a typed `StaleRevision`.
- An end-to-end `PaneHarness` test goes from intent to runtime fill.
User-visible/validatable behavior:
- GUI: flatten and reverse buttons work with no order book open.
- A stale drag shows a notice.

### M6.12 discard-policy-enforcement
Title: Zero unhandled discards in desktop production code
Milestone: desktop-bridge   Crates: apps/desktop   Depends on: M6.6, M6.11
Scope: Resolve the remaining production `let _ =` in `apps/desktop` per the policy: a notice, a documented ignore, or a diagnostic. This includes the 19 in `update.rs` and those in `workspace_surface` and the selection paths. Add `let_underscore_must_use` and `let_underscore_future` at deny level to the `apps/desktop` lints.
Design refs: 11 §5.2, §9 D1
Closes findings: UI-11
Key tests:
- The clippy gate passes with the new lints.
User-visible/validatable behavior:
- Internal only.

---

## M7 desktop-structure

### M7.1 design-tokens-for-chart-colors
Title: Remaining chart and picker colors become design tokens
Milestone: desktop-structure   Crates: design_system, chart_integration, apps/desktop, sibling aeris-website mirror   Depends on: -
Scope: Add tokens for the session-plan level, the picker swatches and default, and the context-menu `rgba`. Update `CSS.md`, `platform.css`, the Rust tokens and the website mirror `aeris-website/src/styles/platform.css`, and run the website sync and tests. naming_check `colors_come_from_tokens`, with the migration path allow-listed until M7.8.
Design refs: 11 §7, §9 D3
Closes findings: UI-13
Key tests:
- The naming_check rule has positive and negative tests.
- The website sync tests pass.
User-visible/validatable behavior:
- Screenshot comparison shows the session-plan level and picker colors visually unchanged.

### M7.2 account-gate-isolation
Title: Dormant account integration isolated behind `AccountGate`
Milestone: desktop-structure   Crates: apps/desktop, tools/naming_check   Depends on: M6.1
Scope: Move `account.rs` and the account branches into `apps/desktop/src/account/`. `AccountGate` provides `presence` (Disabled in production), `begin_refresh_quiesce` and `plan_policy`. Remove `let authenticated = true`, `CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED`, `current_plan_limits`, and the account polling in the frame loop and the 100 ms timer. Update the account naming_check rules.
Design refs: 11 §1.6, §9 D2
Closes findings: UI-12
Key tests:
- The production gate is Disabled, opens no browser, issues no network request, and startup reaches the workspace.
User-visible/validatable behavior:
- GUI: no account UI appears and startup is unchanged.

### M7.3 chart-host-events
Title: `AerisChartView` emits host events; split its host state
Milestone: desktop-structure   Crates: chart_integration, apps/desktop, tools/naming_check   Depends on: M6.4
Scope: Implement `EventEmitter<ChartHostEvent>` for `AerisChartView`; `process_input_events` emits and `ChartPaneController` subscribes. Delete `pending_context_menu`, `pending_activate`, `pending_settings_request`, `pending_study_remove` and the `observe_chart` drains. Split the `view.rs` host state into focused modules. naming_check rejects the pending names in `chart_integration`.
Design refs: 11 §7 (first row), §9 D4
Closes findings: UI-14 (requests), UI-07 (AerisChartView part)
Key tests:
- A gpui test shows a context-menu input produces exactly one host event.
User-visible/validatable behavior:
- GUI: the chart context menu, settings and study removal still work.

### M7.4 pane-controllers-split
Title: `WorkspaceSurface` becomes composition and render over pane controllers
Milestone: desktop-structure   Crates: apps/desktop, tools/naming_check   Depends on: M6.10, M7.3
Scope: Add `OrderFlowPresenter`, `StudiesController`, `AlertsController`, `PaneChromeState` (enums instead of bools), `ContextPanelController` and `PanePersistence`. Fields are private to their controller. Remove `#[allow(clippy::too_many_lines)]` and update the naming_check paths.
Design refs: 11 §1.2-1.3, §9 D10
Closes findings: UI-07 (WorkspaceSurface)
Key tests:
- Controller unit tests.
- `PaneHarness` tests stay green.
User-visible/validatable behavior:
- GUI smoke: studies, alerts, the order book and time-and-sales still work.

### M7.5 app-controllers-split
Title: `TerminalApp` split into app controllers
Milestone: desktop-structure   Crates: apps/desktop, tools/naming_check   Depends on: M7.4
Scope: Add `TabStrip`, `SymbolEntry`, a one-enum `OverlayStack` and `ChartMenus`; notices are wired in. `TerminalApp` has at most 15 fields. Remove the `struct_excessive_bools` allow. Real module directories replace the `#[path]` modules.
Design refs: 11 §1.2, §9 D10
Closes findings: UI-07 (TerminalApp)
Key tests:
- Tab and overlay controller tests.
User-visible/validatable behavior:
- GUI: tab create, rename, reorder and close, symbol search and menus behave as before (UI Automation).

### M7.6 frame-scheduler
Title: Dirty-source `FrameScheduler`; delete the 100 ms timer
Milestone: desktop-structure   Crates: apps/desktop   Depends on: M6.6, M7.5
Scope: Add `DirtySet`, `AppSource` and `FrameScheduler`:
- Producers mark their source; per-source budgets re-mark when work remains.
- Layout persistence is driven by a revision; the per-frame structural compare is deleted.
- The 100 ms timer is deleted. An updater maintenance timer exists only when update checks are enabled.
- Diagnostics run only under the `diagnostics` feature.
- `FramePollGate` is subsumed, or kept with a stated reason.
Record idle CPU for 1, 4 and 16 panes.
Design refs: 11 §3, §9 D11
Closes findings: UI-16
Key tests:
- After setup, `run_until_parked` schedules no further frame when idle.
- An over-budget pane drains on the next frame.
User-visible/validatable behavior:
- Measured idle CPU for 4 panes drops; before and after numbers are in the tracking doc.

### M7.7 workspace-durable-container
Title: Versioned, checksummed workspace container on top of `platform_sys`
Milestone: desktop-structure   Crates: apps/desktop (persistence/), chart_integration   Depends on: M1.6, M7.4
Scope: The envelope is `magic | envelope_version | schema_version | len | sha256 | payload`, written through `StagedFile`.
- The last-good backup is kept.
- Reads try the primary, then a valid newer staged temp, then the backup.
- A corrupt primary is quarantined; the newest 3 are kept.
- `max_len` is 12 MB.
Adopt it for workspace and chart-chrome I/O without changing the payload format. This replaces the 11 D12 `DurableFile` in `platform_runtime` (one-writer reconciliation).
Design refs: 11 §4.1, §9 D12; 07 §B3
Closes findings: UI-09 (duplication and backup), PL-18 (bounds)
Key tests:
- A truncated primary restores the backup and is quarantined.
- A staged temp left by a crash is used.
- An oversized chrome file loads defaults with a diagnostic.
User-visible/validatable behavior:
- GUI with a truncated workspace file in a validation folder: the layout is restored from the backup.

### M7.8 workspace-document-migrations
Title: Writer-owned workspace document, migration chain and visible restore outcomes
Milestone: desktop-structure   Crates: apps/desktop, contracts, chart_integration, tools/naming_check   Depends on: M6.2, M7.7
Scope: The writer thread owns `WorkspaceDocument` (no read-modify-write), with sections tagged by layout revision.
- Chart chrome folds into `WorkspaceState`. The second save thread and `ChartChromeShutdownWait` are deleted, and shutdown waits on a condvar.
- Ordered migrations: legacy `.frame` to 1, then 1 to 2. The 1-to-2 step holds the legacy color, area-bottom and `legacy_*_contract` heuristics, and the chrome key=value fold.
- The pre-v2 copy is kept.
- `RestoreOutcome` produces notices. `NewerVersion` makes the session read-only.
- Readiness runs a dry run. All I/O stays off the UI thread.
Design refs: 11 §4.2-4.4, §9 D13
Closes findings: UI-09, PL-18
Key tests:
- A golden fixture for each migration step.
- A future version refuses to write.
- A corrupt document produces a notice with an Open folder action.
User-visible/validatable behavior:
- GUI: a corrupt workspace shows a notice and the layout is not silently replaced.
- A copied real workspace migrates intact.

### M7.9 aeris-charts-hollow-candles
Title: Hollow candles as an Aeris Charts series style (cross-repo) and pin bump
Milestone: desktop-structure   Crates: aeris-charts (sibling repo), chart_integration, Cargo.toml pins, tools/naming_check   Depends on: M7.3
Scope: In aeris-charts, add an engine `CandleBody::Hollow` series style. Follow its AGENTS.md gates, update `docs/architecture.md`, stage only owned files, and push to the `github` remote. Here, bump only the `aeris_charts_*` revisions and delete the host `hollow_appearance`/`hollow_base` transform. naming_check rejects the hollow names. Review the new engine capabilities, including data reset in place from M6.4, and record them.
Design refs: 11 §7 (hollow row), §9 D14; AGENTS.md "Aeris Charts"
Closes findings: UI-14 (hollow)
Key tests:
- aeris-charts gates green.
- A terminal render test of the hollow style.
User-visible/validatable behavior:
- Screenshot comparison shows the hollow candle style rendering identically before and after.

### M7.10 runtime-tape-sweeps
Title: Sweep classification and incremental tape in `market_runtime`
Milestone: desktop-structure   Crates: market_runtime   Depends on: M3.9
Scope: The sweep annotation is computed once per trade at ingestion. Semantics are identical: 512-trade window, 90th percentile, 100 ms grouping. The classifier tests move with it. The tape is published incrementally as fenced `Append{generation, from_ordinal, trades}` and `Reset{generation, trades}`.
Design refs: 11 §7 (sweep and tape rows), contract M5
Closes findings: UI-14 (sweeps, runtime side)
Key tests:
- The moved classifier tests are unchanged.
- Append/Reset sequencing across a generation change.
User-visible/validatable behavior:
- Internal only.

### M7.11 desktop-incremental-tape
Title: Desktop consumes runtime sweeps and incremental tape
Milestone: desktop-structure   Crates: apps/desktop, chart_integration, tools/naming_check   Depends on: M7.4, M7.10
Scope: `OrderFlowPresenter` applies Append and Reset into a bounded ring. Delete `classify_order_flow_sweeps` and the ordinal diffing from `chart_integration`; naming_check rejects them. Measure the 100k-trade apply before and after. An Aeris Charts append operation is added only if the replace is measured as the bottleneck.
Design refs: 11 §7, §9 D15
Closes findings: UI-14 (tape)
Key tests:
- Apply tests for Append and Reset.
- 100k-trade numbers are recorded.
User-visible/validatable behavior:
- GUI on Hyperliquid: time-and-sales highlights sweeps as before.

### M7.12 desktop-test-rewrite
Title: Desktop tests against controller boundaries
Milestone: desktop-structure   Crates: apps/desktop, chart_integration   Depends on: M7.6, M7.8, M7.11
Scope: Finish migrating `desktop/tests.rs`, `workspace_surface_tests.rs` and the `chart_integration` view tests to controller behavior through `PaneHarness`. Delete the guard-specific tests, mapping each to a behavior test. Add shared fixture builders, with `test_support::providers` holding the provider fixture ids.
Design refs: 11 §8, §9 D16
Closes findings: UI-15
Key tests:
- The required test list in 11 §8 is present and green.
User-visible/validatable behavior:
- Internal only.

---

## M8 lifecycle-and-trim

### M8.1 release-lifecycle-crate-split
Title: Move release code into a `release_lifecycle` crate
Milestone: lifecycle-and-trim   Crates: new crates/release_lifecycle, platform_runtime, apps/desktop, tools/naming_check   Depends on: M1.8
Scope: Move lifecycle, release_delivery, rollout, release_identity and the `aeris_launcher` bin into `release_lifecycle`. The signers sit behind a `producer` feature. ed25519, semver and same-file leave `platform_runtime`; data roots are already there from M1.8. naming_check: only the desktop and `tools/release_packager` may depend on `release_lifecycle`.
Design refs: 07 §B8, F06
Closes findings: PL-01 (kept and isolated per user decision), G3 (part)
Key tests:
- Workspace build and tests.
- `cargo tree` shows ed25519 and semver gone from the adapters.
User-visible/validatable behavior:
- Internal only.

### M8.2 launcher-hygiene-and-trust
Title: Launcher command dispatch, process handling, release config and Authenticode
Milestone: lifecycle-and-trim   Crates: release_lifecycle, platform_sys, apps/desktop   Depends on: M8.1
Scope:
- LC-01: dispatch the command first; load the key only for commands that verify signatures.
- LC-16: promote only when the installed generation is above 0, and reap or log the child.
- LC-13: sweep `*.retired-*` binaries instead of using a `.bat`.
- LC-14: use `MessageBoxW`.
- LC-15: wait on the Child handle or an `OpenProcess` wait in `platform_sys`, and drop sysinfo.
- LC-20: a `build.rs`-validated `release_config`.
- LC-07: `WinVerifyTrust` plus a thumbprint check in `platform_sys::trust`; the build fails when a release config lacks the thumbprint.
Design refs: 07 §A2, F07, F15
Closes findings: LC-01, LC-07, LC-13, LC-14, LC-15, LC-16, LC-20
Key tests:
- A per-command test with no key present.
- Signed and unsigned fixture trust tests.
- `cargo tree -i sysinfo`.
User-visible/validatable behavior:
- A development desktop start spawns no launcher child process (checked in the process list).

### M8.3 ffi-consolidation
Title: Remaining FFI moves to `platform_sys`; `platform_runtime` forbids unsafe
Milestone: lifecycle-and-trim   Crates: platform_sys, platform_runtime, market_runtime, adapters/rithmic, tools/naming_check   Depends on: M8.1
Scope:
- Move the power and network notification FFI to `platform_sys`; the safe event streams stay in `platform_runtime`.
- Delete `display_timing` and its windows-sys features, and `NativeSessionShutdownMonitor`.
- Delete `CancelIoEx` only if an abort-latency test passes; otherwise move it.
- Delete `NativeSessionShutdownGuard` only after a manual logoff flush test; otherwise move it and record the reason.
- `platform_runtime` returns to the workspace `forbid`, and the interim unsafe allowlist is removed.
Design refs: 07 §A3, §B6, F08
Closes findings: PL-02, PL-10 (display_timing), PL-19 (part)
Key tests:
- Rithmic abort latency.
- `unsafe` appears only in `platform_sys`.
User-visible/validatable behavior:
- Internal only. The manual logoff check is noted for the maintainer if it was not done.

### M8.4 account-runtime-safety
Title: Account runtime vault, lock and issuer fixes
Milestone: lifecycle-and-trim   Crates: account_runtime, platform_runtime (credential_vault)   Depends on: M8.2
Scope:
- AC-02: `MAX_SECRET_BYTES` of 2560, with a precheck and const asserts.
- AC-03: vault I/O outside the state mutex, protected by a generation fence.
- AC-04: the issuer is read at build time through `release_config`; a runtime override is allowed only in debug. Delete the `AXIUSFLOW_AUTH_ISSUER` tests.
- AC-06: recover from a poisoned mutex.
- AC-08: an empty issuer starts no network work.
Design refs: 07 §A5, F09
Closes findings: AC-02, AC-03, AC-04, AC-06, AC-08, PL-07 (rest)
Key tests:
- Unit tests for each fix, including a poisoned-lock test.
User-visible/validatable behavior:
- Internal only.

### M8.5 readiness-contract
Title: Shared readiness report; readiness never touches the real database
Milestone: lifecycle-and-trim   Crates: release_lifecycle, apps/desktop, trading_runtime   Depends on: M8.1, M1.9
Scope: One `ReadinessReport` type serves both the desktop and the launcher. Readiness migrates a `VACUUM INTO` temp copy under the instance lock and is durably written. Re-evaluate the `provider_count > 0` requirement.
Design refs: 07 §B4 (readiness), F11
Closes findings: LC-05 (probe part), LC-11
Key tests:
- A round-trip contract test.
- The real database `user_version` is unchanged after readiness.
User-visible/validatable behavior:
- `--desktop-readiness` exits 0 and `trading.sqlite3` hashes the same before and after.

### M8.6 release-wire-protocol-and-residue
Title: Release wire protocol v1, uninstall safety and Axiusflow residue removal
Milestone: lifecycle-and-trim   Crates: release_lifecycle, apps/desktop (update.rs), installer script, tools/naming_check   Depends on: M8.2, M8.5
Scope:
- LC-03: a domain-separated signature over the raw manifest bytes, with `deny_unknown_fields`.
- LC-04: `AERIS_*_V1` tokens. Delete the TRANSITIONAL aliases and the Axiusflow block-plan domain.
- LC-19: a `LAUNCHER_PROTOCOL_VERSION`.
- LC-08: typed, logged block errors.
- LC-12: an Inno uninstall page with "Remove my Aeris data" unchecked by default. Delete the Axiusflow cruft from `aeris_setup.iss`.
naming_check: no AXIUSFLOW in production code.
Design refs: 07 §A2, F12, F17, F18
Closes findings: LC-03, LC-04, LC-08, LC-12, LC-19, PL-16 (tokens)
Key tests:
- Signature vectors, tamper and unknown-field tests.
User-visible/validatable behavior:
- Internal. The installer change is checked by script review; a VM install and uninstall is manual for the maintainer.

### M8.7 release-packager-e2e
Title: Release producer tool and end-to-end lifecycle harness (no publishing)
Milestone: lifecycle-and-trim   Crates: new tools/release_packager, release_lifecycle, .github/workflows   Depends on: M8.6
Scope: `tools/release_packager` takes the key from a file. It emits the manifest, block plan, channel pointer and rollback-compatibility data with `max_store_schema` from `SCHEMA_VERSION`, and publishes nothing. Add a `ReleaseSource` seam: ureq HTTPS in production, `DirectorySource` in tests. The end-to-end test covers install, update, readiness on a copy, activation, crash rollback and quarantine, and runs in windows-latest CI.
Design refs: 07 §B9, F13
Closes findings: LC-02, LC-21
Key tests:
- The full-cycle end-to-end test.
User-visible/validatable behavior:
- Internal (the end-to-end test is green in CI).

### M8.8 rollback-safety
Title: Schema-gated rollback, startup-complete probation, deterministic recovery
Milestone: lifecycle-and-trim   Crates: release_lifecycle, apps/desktop   Depends on: M8.7
Scope:
- LC-05: a `max_store_schema` rollback gate.
- LC-09: the desktop writes a startup-complete marker, and probation waits for it.
- LC-18: the readiness outcome is journaled before Activated, and unknown entries move to `orphaned/`.
- LC-06: all verification runs before READY, with a timing test using a slow fake verifier.
Design refs: 07 §A2, F14
Closes findings: LC-05, LC-06, LC-09, LC-18
Key tests:
- End-to-end hang and crash rollback cases.
- The schema gate test.
User-visible/validatable behavior:
- Internal (end-to-end tests).

### M8.9 dependency-direction-and-contracts
Title: Fix dependency direction and reduce `contracts` to the persisted schema
Milestone: lifecycle-and-trim   Crates: study_sdk, market_runtime, platform_runtime, adapters, application, contracts, tools/naming_check   Depends on: M8.1, M7.8
Scope:
- `study_sdk` depends on the `market_data` contracts, not `market_runtime` (G1).
- `hosted_broker` moves out of `platform_runtime` (G2).
- The `EmbeddedReplaySource` demo data in `application` moves to test support (G5).
- `contracts` keeps only the persisted workspace schema. In-process DTOs move to their owning crates, and `GetAccountStatus` and `RefreshAccountProfile` are deleted (G4).
- naming_check gets direction rules from `Cargo.toml`.
Design refs: 04 PL-13, PL-14, §G1-G5; 07 §B8
Closes findings: PL-13, PL-14
Key tests:
- `cargo tree -p study_sdk | rg adapter` is empty.
- Workspace tests pass.
User-visible/validatable behavior:
- Internal only.

### M8.10 dependency-hygiene
Title: Remove reqwest stack, unused crates and duplicate pins; small context_runtime fixes
Milestone: lifecycle-and-trim   Crates: apps/desktop, account_runtime, context_runtime, adapters/rithmic, Cargo.toml   Depends on: M8.9
Scope:
- AC-07: fetch the avatar with ureq and remove `reqwest_client`.
- Remove unused crates.
- Put `rpassword` behind a bin feature.
- Centralize pins in `[workspace.dependencies]`, align `resvg` with GPUI, and dedupe sysinfo and sha2.
- Keep the unused Rithmic `order_plant` out of the default build.
- PL-17: derive the user agent from `CARGO_PKG_VERSION`, and use `recv_timeout` in the context request/reply.
Design refs: 07 F16; 04 PL-15, PL-17; 02 TR-12
Closes findings: PL-15, AC-07, PL-17, TR-12 (order plant)
Key tests:
- `cargo tree -i reqwest` is empty.
- A stalled-worker context test returns a timeout.
User-visible/validatable behavior:
- GUI: the desktop starts and charts work as before (smoke).

### M8.11 vendor-and-tooling-trim
Title: Trim `gpui_base`, stale tooling and test-only exports
Milestone: lifecycle-and-trim   Crates: third_party/gpui_base, tools, platform_runtime, .gitignore   Depends on: M8.3
Scope:
- Trim `gpui_base` to the used modules: drop the LSP editor (about 5.4k lines) and `lsp-types`, add it to the workspace with lints, and pin its dependencies.
- Delete the pacing scripts, their verifiers and the unreferenced `tools/*.ps1` and `*.sh` scripts.
- Move `live_market_gate` to test support or the smoke bin.
- Prune stale `.gitignore` entries.
Design refs: 04 PL-10, PL-12, PL-19; 07 §A4
Closes findings: PL-12, PL-10, PL-19
Key tests:
- `cargo tree -i lsp-types` is empty.
- Every remaining tool is referenced by CI or the docs.
User-visible/validatable behavior:
- GUI: text inputs (symbol search, order ticket) still work.

### M8.12 naming-check-structural-and-docs
Title: Structural naming_check and final README/AGENTS update
Milestone: lifecycle-and-trim   Crates: tools/naming_check, docs   Depends on: M8.10, M8.11
Scope: Reduce naming_check to structural checks:
- dependency direction from `cargo metadata`, forbidden crates and paths, file naming;
- unsafe location, durable writes and provider neutrality;
- a recursive markdown inventory that allows `plan/stabilization`.
Replace the remaining text pins with behavior tests in the owning crates, and keep the `gpui_base` pin. Finalize the README (`Readme` becomes `README.md` if allowed) and AGENTS.md (`platform_sys` exception, `release_lifecycle`, validation mode). Close out the tracking doc.
Design refs: 07 §B7, F20; 04 PL-08, PL-11
Closes findings: PL-08, PL-11 (Readme and .gitignore part)
Key tests:
- naming_check's own tests pass.
- A pure rename in `desktop.rs` needs no naming_check edit.
User-visible/validatable behavior:
- Internal only (docs and gate).

---

## Deferred / out of scope

- Depth-aware partial fills, the slippage model, margin and buying power, currency conversion, and the Funding/MarginReserve ledger kinds. Only hooks are left (user decision "solid core").
- V24: dropping the legacy trading tables. It needs V22/V23 verified in a released build first (09 §5.2).
- Removing the legacy data-root migration and the vault legacy-name probes (F19, LC-17). This waits until the maintainer confirms their machine is migrated.
- Risk profile registration UI (TR-09 desktop part); only runtime rollover is in scope.
- Automatic route resolution for migrated instruments without a stored route (09 Q10). These show "feed unavailable" until the instrument is selected again.
- Holiday data for exchange-rule calendars (10 Q3). Exchange rules report Open on holidays, which affects stall gating and trading gating on holidays.
- Focusing the existing window on a second launch; it needs IPC.
- An Aeris Charts append operation for order flow (only if measured as the bottleneck in M7.11).
- An Aeris Charts in-place data reset, if the M6.4 check shows the engine lacks it.
- Workspace-level trading account selection; the per-pane selection is kept (architecture.md section 3.7.3).
- Rithmic live qualification, fresh cTrader OAuth (hosted broker 404), production release publication, code-signing certificate and key custody, the AWS release endpoint, and the macOS workflows.
- Manual checks only the maintainer can run: logoff flush (M8.3), VM install and uninstall (M8.6), Rithmic login through the native vault (M6.9).

## Unmapped findings (not closed by any item)

| Finding | Reason |
|---|---|
| LC-17 | Deferred. Deleting the legacy data-root migration and vault probes needs maintainer confirmation that migration is complete (07 F19). |
| PL-16 (legacy migrations part) | Same as LC-17. The tokens and installer residue are closed in M8.6. The never-migrated Asceify brand is left as is. |
| TR-12 (two position models) | Intentional. The simulated route uses net positions and cTrader uses hedged broker positions; the design keeps both behind one ledger. |
| PL-01 "delete or feature-gate" fix | Superseded by the user decision to keep the pipeline. It is closed as isolation plus fixes in M8.1-M8.8, not deletion. |
| PL-07 / UI-12 "cargo feature" fix | Superseded by the keep decision and 11 §1.6. The account code is isolated behind `AccountGate` (M7.2) and fixed (M1.4, M8.4), not feature-gated. |
| 05 root causes 7 and 8 (commit rhythm, two-repo chart cost) | These are process findings. They are addressed by the one-commit-per-item discipline and the tracking doc, with no code item. |

All other IDs map to at least one item: MD-01..17, TR-01..12, UI-01..16, PL-02..15, PL-17..19, LC-01..16 and LC-18..21, AC-01..08.
