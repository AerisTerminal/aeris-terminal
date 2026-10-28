# Aeris Terminal stabilization: authoritative architecture

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal` (Rust 1.99.1, edition 2024, branch `main`).
Sibling repositories: `C:\Users\devraj\Downloads\Softwares\aeris-charts` (pinned chart engine) and
`C:\Users\devraj\Downloads\Softwares\aeris-website` (design-token mirror).

This document is binding for every worker in the mission. Where it conflicts with the research designs
(`plan/stabilization/reports/07_platform_design.md`, `09_trading_design.md`, `10_market_design.md`, `11_desktop_design.md`), this
document wins. Where it conflicts with the repo `AGENTS.md`, the repo file wins unless this document explicitly
amends it, and the amendment must land in `AGENTS.md` in the same commit (section 3.9). Finding IDs (MD-, TR-, UI-,
PL-, LC-, AC-) refer to the audits in `plan/stabilization/reports/01..05` and `07`.

---

## 1. Purpose and principles

The mission replaces patch layers with one design per ownership boundary, matures paper trading to the approved
"solid core", hardens the platform, and trims bloat without deleting the update pipeline or the dormant account
runtime.

Every change satisfies the five principles in the repo `AGENTS.md` ("Non-negotiable core principles": proper
architecture, clean code, scalable, maintainable, built for the long run). They are acceptance criteria. Existing
violations never justify new ones.

Root causes this mission removes, and the mechanism that removes each one:

| # | Root cause (05_cross_cutting) | Removing mechanism |
|---|---|---|
| R1 | No explicit chart-data lifecycle (~7 layers, 8 state representations, duplicate connect/recover handlers, 8 history maps, 89 `pending_*` flags) | `SeriesLifecycle` (3.5.1), `HistoryTracker` (3.5.2), `ChartBinding` (3.7.2) |
| R2 | Hand-made generation fencing (4,693 raw-integer uses) | `Epoch<Tag>` / `Fenced<Tag, T>` / `Fence<Tag>` (3.5.3), one drop point per boundary |
| R3 | God objects (`WorkspaceSurface` 96 fields, `TerminalApp` 63, market `Coordinator` ~300 methods, trading `Coordinator` ~150 methods in a 5.4k-line `lib.rs`) | Controllers (3.7.1), lifecycle/history/calendar/provider modules (3.5), `TradingCore` modules (3.6) |
| R4 | Errors as strings (1,196 `Result<_, String>`, 322 `let _ =`, control flow on display text) | Typed error enums on every rebuilt boundary, `UserNotice` (3.7.6), ratchet rules (3.9) |
| R5 | Provider logic outside adapters (100 desktop literals, single-provider gates and watchdog) | `ProviderSession` + `ProviderCapabilities` (3.5.5), `MarketCalendarService` (3.5.4), descriptors (3.5.8), neutrality rule (3.9) |

Working rules that follow from these principles:

- One owner per piece of state. A replacement deletes what it replaces in the same feature (no parallel paths, no
  "compatibility" forwarders that outlive the milestone that introduced them).
- Every queue, map, cache, retry and history window has an explicit bound and an explicit overflow behavior.
- The GPUI UI thread performs no blocking network, disk, process or shutdown work. Background threads never mutate
  GPUI state; they mark a dirty source and the UI drains it.
- Fixed-point values with explicit scales everywhere; required wire fields are validated, never defaulted.
- TDD for bug fixes and new behavior; characterization tests before behavior-preserving refactors.
- No performance claim without before/after numbers recorded in the tracking document.

---

## 2. Target crate and dependency graph

### 2.1 Mission-start snapshot (verified from manifests at mission start)

This graph records the crates as they were when the mission began. The adapter count below is from that snapshot;
today only `crates/adapters/hyperliquid_market` and `crates/adapters/rithmic_protocol` exist.

```
domain:   aeris_account, aeris_instruments, aeris_market_data, aeris_contracts, aeris_provider_history,
          aeris_observability (all leaf); aeris_trading -> instruments
platform: aeris_platform_runtime -> observability   (68 unsafe blocks, lint lowered to deny; also hosts
          lifecycle.rs, release_delivery.rs, rollout.rs, release_identity.rs, bin/aeris_launcher.rs,
          native_data_root(), display_timing.rs, session_shutdown.rs, power/network FFI)
adapters: rithmic_protocol, hyperliquid_market -> platform_runtime, market_data (+obs, provider_history)
runtime:  aeris_market_engine -> market_data
          aeris_market_runtime -> engine, market_data, instruments, trading (domain), contracts, provider_history,
                                  platform_runtime, observability, 4 adapters
          aeris_trading_runtime -> instruments, trading (+ rusqlite)
          aeris_account_runtime -> account, contracts, observability, platform_runtime
          aeris_context_runtime -> platform_runtime
          aeris_study_sdk -> market_data, market_runtime     aeris_application -> instruments, market_data
ui:       design_system; terminal_ui -> instruments, market_data, trading, design_system;
          chart_integration -> application, design_system, observability, terminal_ui, aeris_charts_*
app:      aeris_desktop -> nearly everything (+ gpui_base vendored, excluded from workspace)
tools:    aeris_naming_check
```

### 2.2 Target

```
LEAF (no internal deps)
  aeris_platform_sys        NEW  crates/platform_sys      only crate allowed `unsafe`; windows-sys / macOS FFI
  aeris_instruments              + PriceGrid, InstrumentClass, SessionPhase, SessionWindow, CalendarSourceKind
  aeris_market_data              (unchanged role; loses ChartInterval::rithmic_aggregation)
  aeris_account, aeris_contracts, aeris_design_system
  aeris_provider_history         TRIMMED to model.rs, handoff.rs, error.rs (3.5.2)
DOMAIN
  aeris_trading -> instruments                         + market.rs (trading feed contract, 3.6.4)
PLATFORM
  aeris_observability -> platform_sys                  log sink, panic hook, crash files
  aeris_platform_runtime -> platform_sys, observability  forbid(unsafe) from M8.3 (interim allowlist until then); vault, sockets, http, dns, browser,
                                                         notifications, power/network event streams, DataRoot,
                                                         InstanceLock, DurableDocument container
  aeris_release_lifecycle NEW (M8.1) crates/release_lifecycle -> platform_runtime, observability (durable writes
                                                         through the platform_runtime::fs re-export, not platform_sys)
                                                         lifecycle, release_delivery, rollout, release_identity,
                                                         launcher + thin bin aeris_launcher, release_source, origin_layout
                                                         (M8.7); release_config (M8.2), ReadinessReport (M8.5)
ADAPTERS (unchanged edges)
RUNTIMES
  aeris_market_engine -> market_data, instruments
  aeris_market_runtime -> market_engine, market_data, instruments, trading(domain), contracts, provider_history,
                          platform_runtime, observability, adapters
  aeris_trading_runtime -> trading, instruments, platform_sys, observability   (+ rusqlite, sole user)
  Clocks are owner-local, not a shared crate: `aeris_trading_runtime::clock` (`pub trait Clock`, `SystemClock`,
  `ManualClock`) and `market_runtime`'s crate-private `market_service::clock` (`Clock`, `SystemClock`, `ScriptedClock`).
  aeris_account_runtime, aeris_context_runtime (unchanged edges)
  aeris_study_sdk -> market_data, market_runtime   (accepted facade, see below)
UI: unchanged edges; chart_integration and terminal_ui never depend on a runtime
COMPOSITION ROOTS (may depend on several runtimes)
  aeris_desktop                         apps/desktop
  aeris_probes             NEW          tools/aeris_probes   (binaries, never shipped)
  aeris_release_packager   NEW (M8.7)   tools/release_packager -> release_lifecycle[producer], platform_runtime
  aeris_naming_check                    tools/naming_check (no internal deps)
```

Direction rules (enforced by `naming_check` from `Cargo.toml` files, section 3.9):

1. `aeris_platform_sys` depends on no `aeris_*` crate. Only `observability`, `platform_runtime` and `trading_runtime`
   may depend on it today; `release_lifecycle` joins them when it is created (M8.1 target).
2. `unsafe` and `allow(unsafe_code)` appear only under `crates/platform_sys/src`. Every block has a `// SAFETY:`
   comment; clippy `undocumented_unsafe_blocks` and `multiple_unsafe_ops_per_block` are `deny` in that crate. All
   other crates inherit workspace `unsafe_code = "forbid"`. Until the M8.3 FFI consolidation, the exact `platform_runtime`
   FFI files allowlisted by `tools/naming_check` are interim exceptions and `platform_runtime` keeps `unsafe_code =
   "deny"`; returning it to `forbid` is the M8.3 target.
3. `trading_runtime` never depends on `market_runtime`, `market_engine`, `platform_runtime` or any adapter.
   `market_runtime` never depends on `trading_runtime`. Only composition roots depend on both.
4. Domain and leaf crates depend only on other domain/leaf crates.
5. `chart_integration`, `terminal_ui`, `design_system` never depend on a runtime or adapter.
6. Only `apps/desktop` and `tools/release_packager` depend on `release_lifecycle` (M8.1/M8.7).
7. `rusqlite` appears only in `crates/trading_runtime`.
8. `market_runtime` has no normal (non-dev) dependency on `study_sdk`. PL-13 is resolved as "accepted": `study_sdk` is
   a published-API facade above the in-process study runtime, consumed by the desktop's built-in study packages; it
   is not an inversion while `market_runtime` only dev-depends on it.

---

## 3. Component designs

### 3.1 `platform_sys` and the durable writer (M1)

Crate `crates/platform_sys` (`aeris_platform_sys`). Exposes safe APIs only; FFI lives in `src/windows/*.rs`,
`src/macos/*.rs` (the only files with `#![allow(unsafe_code)]`).

```rust
pub mod fs {
    pub struct StagedFile { /* temp path ".{name}.{pid}-{seq}.tmp" in the destination dir, File, destination */ }
    impl StagedFile {
        pub fn create(destination: &Path) -> io::Result<Self>;   // create_new in the same directory
        pub fn commit(self) -> io::Result<()>;                   // sync_all; replace; unix: fsync parent dir
    }                                                             // Drop without commit removes the temp file
    impl Write for StagedFile {}
    pub fn write_file_durably(destination: &Path, bytes: &[u8]) -> io::Result<()>;
    pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()>; // MoveFileExW REPLACE_EXISTING|WRITE_THROUGH
    pub fn remove_stale_temporaries(directory: &Path, file_name: &str) -> io::Result<usize>;
}
pub mod power   { pub struct SuspendResumeRegistration; }          // from platform_runtime/power_notifications.rs FFI
pub mod network { pub struct InterfaceChangeRegistration; }        // from network_notifications.rs FFI
pub mod process { pub fn wait_for_exit(pid: u32, timeout: Duration) -> io::Result<WaitOutcome>; pub fn is_running(pid: u32) -> bool; }
pub mod ui      { pub fn message_box(title: &str, text: &str, kind: MessageKind); }   // pre-GPUI fatal dialogs
pub mod trust   { pub fn verify_authenticode(path: &Path, thumbprint: &[u8; 20]) -> Result<(), TrustError>; } // M8
pub mod session { pub struct SessionShutdownGuard; }               // moved, deletion gated on manual logoff test
```

Rules:

- The live destination file is never removed or renamed away. The old `.bak` rotation dances in
  `apps/desktop/src/desktop/local_state.rs` (`pb.bak`) and `apps/desktop/src/chart_chrome.rs` are deleted in M1;
  load-time `.bak` fallback stays read-only for one release so existing backups remain usable.
- Adoption in M1 replaces every hand-rolled writer: `trading_runtime` `store.rs` `write_atomic` (TR export, not
  atomic today), `local_state.rs`, `chart_chrome.rs`, readiness report (`desktop.rs` `fs::write`,
  `readiness_conformance.rs`), `platform_runtime` `lifecycle.rs` `write_json_atomic`, launcher writes and download
  commits (LC-10), `live_market_gate.rs`. `atomic_file.rs` is deleted (its `MoveFileExW` moves into `platform_sys`).
- Dead FFI deleted in M1, not moved: `display_timing.rs` (877 lines, stale tools only, plus the `Win32_Graphics_Dwm`
  and `Win32_System_Performance` features) and `NativeSessionShutdownMonitor` (no callers). `io_cancellation.rs`
  moves to `platform_sys::net` unless an abort-latency test in the Rithmic network path proves `shutdown(Both)`
  plus the 100 ms poll already bounds abort latency, in which case it is deleted.
- Tests: commit, drop cleanup, replace existing, pre-existing stale temp, and a child-process crash test (child
  killed between write and commit; destination holds old content, never empty).

### 3.2 Observability: log sink and panic hook (M1)

The existing `diagnostic!` macro (`crates/observability/src/diagnostic_output.rs`) stays the only logging API
(194 call sites). It gains a bounded background file sink:

```rust
pub struct LogSinkConfig { pub directory: PathBuf, pub file_stem: &'static str, pub max_file_bytes: u64, pub max_files: u8 }
pub fn install_log_sink(config: LogSinkConfig) -> io::Result<LogSinkGuard>;  // guard flushes and joins on drop (bounded 2 s)
pub fn install_panic_hook(context: CrashContext);                             // chains the previous hook
pub struct CrashContext { pub crash_directory: PathBuf, pub release_identity: String, pub generation: u64 }
```

- `write_diagnostic` keeps writing stderr and also `try_send`s `(SystemTime, thread name, line)` into a
  `sync_channel(1024)`. Full channel: increment an `AtomicU64` drop counter; the writer thread emits
  `diagnostics dropped N lines` before the next line. No caller ever blocks on disk.
- Files: `<data root>/logs/aeris.log`, rotated at 4 MiB to `aeris.1.log` .. `aeris.4.log` (20 MiB cap). Launcher:
  `<install root>/logs/launcher.log`. Lines carry an RFC 3339 UTC timestamp and thread name.
- Panic hook: synchronously writes `<data root>/logs/crash-<unix_ms>.log` (message, location, thread,
  `Backtrace::force_capture()`, release identity, generation) with `platform_sys::fs::write_file_durably`, keeps the
  newest 10 crash files, then calls the previous hook.
- Fatal startup errors in `desktop::run()` are logged, then shown with `platform_sys::ui::message_box` (pre-GPUI)
  naming the log path; exit code unchanged.
- No secrets: AC-01 lands first in M1. `VerifiedTokens` (`account_runtime` `oidc.rs`) holds `SecretToken`
  (`Zeroizing<String>`, redacted `Debug`, no `Display`, no `Clone`). `naming_check` forbids `derive(Debug)` on types
  whose name contains `Token`, `Secret`, `Password` or `Credential` unless the type is on a reviewed allowlist of
  redacting wrappers.

### 3.3 Single instance, data root and validation mode (M1)

New in `platform_runtime`:

```rust
pub enum DataRootMode { Production, Validation { root: PathBuf } }
pub struct DataRoot { path: PathBuf, mode: DataRootMode }
impl DataRoot {
    pub fn resolve(mode: DataRootMode) -> Result<Self, DataRootError>;   // replaces lifecycle.rs native_data_root()
    pub fn path(&self) -> &Path;                                        // production: %LOCALAPPDATA%\Aeris
    pub fn vault(&self) -> VaultNamespace;
}
pub enum VaultNamespace { Production, Validation { id: String } }
pub struct InstanceLock { file: File }
impl InstanceLock { pub fn acquire(root: &DataRoot) -> Result<Self, InstanceLockError>; } // File::try_lock on <root>/instance.lock
pub enum InstanceLockError { HeldByAnotherProcess { root: PathBuf }, Io(io::Error) }
```

- CLI, accepted by `aeris_desktop` (before any command, including `--desktop-readiness`) and by every
  `tools/aeris_probes` binary: `--validation-root <absolute dir>`. The root
  must be absolute, must not equal or contain the production root, and becomes `<dir>\Aeris`.
- The lock is keyed per data root, so isolated validators can run beside each other and beside the user's instance,
  while two processes on one root cannot. Acquired in `desktop::run()` right after argument parsing and before
  `migrate_legacy_native_data_root` and `start_trading_service`; held by `DesktopLifecycle` until the trading,
  context and workspace stores have shut down. Second instance on the same root: native dialog "Aeris is already
  running for this data folder", exit 0. Readiness also acquires it and fails readiness if held.
- Prerequisite AC-05 (M1): `retain_account_refresh_quiesce_for_exit` (`desktop.rs`) never parks. It waits up to its
  bound, logs, then calls `std::process::exit(code)`. A test proves the exit path returns within the bound.
- Vault namespace: `NativeCredentialVault::new(service)` gets `com.aeris.terminal` in production and
  `com.aeris.terminal.validation.<first 12 hex of sha256(canonical root)>` in validation mode. Validation instances
  therefore see no stored broker connection and never read production vault entries.
- Validation mode also disables launcher promotion and update checks, and skips the notification AUMID registry
  write.

### 3.4 SQLite store hardening and table-driven migrations (M1, extended M4/M5)

`crates/trading_runtime/src/store` (split into `store/mod.rs`, `store/schema.rs` in M1; the table-family split
happens in M4):

- Open: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON` (unchanged), plus `busy_timeout=5000`.
- `PRAGMA quick_check` at open (before GPUI). Measure the cost on a 50k-order fixture and record it; if it exceeds
  250 ms, run it after open on the owner thread before the first command and fail the service into
  `StoreError::Corrupt` there. Failure never recreates the file. The desktop shows the error dialog with the path
  and the newest backup; quarantine (`trading.corrupt-<ts>.sqlite3` plus `-wal`/`-shm`) happens only after the user
  confirms.
- Migrations: `const MIGRATIONS: &[Migration { from: u32, apply: fn(&Transaction<'_>) -> Result<(), StoreError> }]`.
  V1..V19 are transcribed unchanged from today's `if` chain in M1. Each step runs in its own transaction and sets
  `user_version = from + 1`. Unit tests assert contiguity from the oldest supported version to `SCHEMA_VERSION` and
  migrate a fixture for every historical version to head.
- Pre-migration backup: when `user_version < SCHEMA_VERSION`, `VACUUM INTO
  '<data>/trading/backups/trading-v{old}-{unix}.sqlite3'` before the first step; keep the newest 3. Newer-than-known
  `user_version` stays a hard typed error (`StoreError::NewerSchema { found, supported }`).
- `pub const SCHEMA_VERSION` exported for release tooling (M8 manifest `max_store_schema`).
- Clean shutdown runs `PRAGMA wal_checkpoint(TRUNCATE)`.
- The export path (`TradingService::export`) writes through `platform_sys::fs::StagedFile`.
- Schema as built (`store/schema.rs`, `SCHEMA_VERSION = 28`; mission start was V19): V20 nullable verified market
  routes (migrated legacy instruments stay route-less until verified Terms arrive); V21 `fill_accounting` and
  `trade_cycles` backfill; V22 order lifecycle columns; V23 `order_groups` and `order_group_legs`; V24 drops
  `managed_brackets` and `protective_orders`, applied only by the gated `finalize_group_migration` step after the group
  backfill and the runtime conversion of awaiting brackets, never by the migration chain; V25 `ledger_entries`,
  `fee_schedules`, exact position cost-basis columns and instrument class/price-grid columns (backfilled once after
  the group finalize by `complete_post_group_migrations`); V26 `position_checkpoints`, `ledger_checkpoints` and
  `equity_samples`; V27 `risk_sessions` and `risk_locks.kind`; V28 netted broker accounts (drops `broker_positions` and
  `broker_deals.broker_position_id`, adds `broker_bracket_levels`). Only the old P&L tables and per-fill P&L columns
  remain, unread, until a later removal migration. Section 5 details data migration.

### 3.5 `market_runtime`

As-built layout of `crates/market_runtime/src/`:

```
lib.rs, cadence.rs     crate root; `fence` re-exports aeris_market_engine::fence::{ConsumerTag, EpochSequence, Fenced};
                       cadence.rs = resolve_cadence, the single ChartInterval -> SeriesCadence mapping
hyperliquid_*.rs, rithmic_history.rs, rithmic_realtime.rs   provider worker modules wrapping the adapters
rithmic_venue/{mod,session,requests,reports,translate,reconcile}.rs   broker order relay (3.6.12)
study.rs, study/execution.rs
market_service/
  mod.rs               MarketService facade, public command/event types
  coordinator.rs       loop, dispatch, effect execution
  command_fences.rs    CommandFences (45 s response deadline)
  consumer_registry.rs one retirement path for consumer resources
  instrument_selection.rs   select_and_demand resolution
  lifecycle/{mod,handlers,trade_model}.rs   SeriesLifecycles, single forming bar, trade aggregation
  watchdog.rs          provider-neutral, clock-driven stall policy
  history.rs, history/tracker.rs            history worker and HistoryTracker
  calendar/{mod,venue,weekly_hours}.rs      MarketCalendarService, venue normalization, contract-hour status
  provider_session.rs  ProviderRegistry, ProviderConnection, per-provider sessions
  provider_event.rs, realtime.rs, runtime.rs, publication.rs
  reconnect.rs         ReconnectPolicy, Backoff
  clock.rs             crate-private Clock, SystemClock, ScriptedClock
  quotes.rs            QuoteBook
  trading_feed.rs      bounded latest-wins delivery to trading
  alerts.rs            price alerts
  outbox.rs            ConsumerOutbox (waitable) + ConsumerEvents
  descriptors.rs       ProviderDescriptor, InstrumentDescriptor, ResolvedSeries publication
  broker_venue.rs      MarketService::attach_broker_venue
  rithmic_login.rs     runtime-owned Rithmic login and vault entry
  scripted_provider.rs offline scripted-provider composition (not in ordinary builds)
  replay/mod.rs        deterministic replay scenarios
  tests/{characterization,boundary,recovery}.rs
```

Deleted by the end of M3: `SeriesLive`, `SeriesLiveMap` and the duplicated `trade_*`/`candle_*` accessors
(`mod.rs`), `LiveHistoryState`, `TradeLiveHandoff` as a 25-field struct, the candle handlers and Rithmic handlers in
`realtime.rs` (`candle_provider_*`, `candle_series_recovering`, `rithmic_connecting/_online/_trade/_failed`,
`publish_rithmic_live`, `publish_candle_live`), `recover_stalled_live_series`, `stop_realtime_if_idle`, the internal
`ProviderDescriptor` flag struct (~25 fields, `mod.rs`), `ProviderSessionSlot` (9 flags), the four-arm
`ProviderRealtimeChannelSet`/dispatch enums, the eight coordinator history maps
(`history_inflight/deferred/confirmed_empty/backwards_exhausted/current_empty`, `detached_history`,
`history_cancellations`, `history_retries`), `catalog_searches`/`startup_catalog_*`/`catalog_screens` raw `u64`
maps, `require_practice_market_session` (market and `BrokerApi` sides), the provider literals in
`market_session_status`, and the `EVENT_WAIT` 2 ms sleeps in `rithmic_realtime.rs` where they poll.

#### 3.5.1 SeriesLifecycle (M3)

Owned by the coordinator thread in `SeriesLifecycles: BTreeMap<BarSeriesKey, SeriesLifecycle>`, created on engine
`DemandAdded`, removed on `DemandRemoved`. `MarketEngine` stays the single demand owner; canonical bars stay in
`SeriesStore`. The lifecycle holds only control state, the single `FormingBar`, and a bounded `LiveBuffer`.

```rust
enum SeriesState {
    Requested { provider: Option<Epoch<ProviderTag>> },
    Seeding   { ticket: HistoryTicket, buffered: LiveBuffer },
    Live      { last_progress: MonotonicInstant },
    Dormant   { phase: SessionPhase, until: Option<UtcMillis> },
    Recovering{ reason: RecoveryReason, ticket: HistoryTicket, buffered: LiveBuffer },
    Failed    { error: SeriesFailure },
}
enum RecoveryReason { ProviderReconnected, SequenceGap, QueueOverflow, StalledWhileOpen, DetachedWindow, SessionRolled }
enum LiveModel { TradeHandoff { continuity: TradeContinuity, cadence: LiveCadence }, CandleOverlay { cadence: LiveCadence } }
enum LiveCadence { Fixed(NonZeroU32), Trades(NonZeroU32), Session { days: NonZeroU8 }, CalendarWeeks(NonZeroU8), CalendarMonths(NonZeroU8) }
impl SeriesLifecycle { fn step(&mut self, input: SeriesInput<'_>, ctx: &StepContext<'_>, out: &mut EffectBuf); }
```

- `step` is pure: no I/O, no `Instant::now()`, no engine mutation. `StepContext` carries `&dyn Clock`,
  `&MarketCalendarService`, `&ProviderCapabilities` and current epochs. The coordinator executes `SeriesEffect`s
  (`SubscribeLive`, `UnsubscribeLive`, `RequestHistory`, `CancelHistory`, `InstallTail`, `PublishDelta`,
  `PublishCoveringSnapshot`, `ReportStatus`). `EffectBuf` and `LiveBuffer` are fixed-capacity and reused; a full
  `LiveBuffer` drops the buffer and forces a reseed (explicit overflow).
- Transition table: `plan/stabilization/reports/10_market_design.md` section 2.4 is adopted verbatim, plus: inputs carrying a stale
  epoch or ticket are dropped and counted (`stale_inputs` counter in diagnostics).
- Continuity evidence (`Sequence` where the provider documents sequences, `Indexed` for documented trade indices) is
  never inferred from opaque ids. Which model applies comes from `ProviderCapabilities::live_model`.
- `LiveCadence::Session` asks the calendar for the session window (fixes MD-03, MD-04 weekly/monthly). The
  `Session -> Fixed{days*86400}` mapping (`realtime.rs`) is deleted.
- Port order: candle path first (Hyperliquid), then trade path (Rithmic Sequence).
- The watchdog (`market_service/watchdog.rs`) evaluates every `Live` series of every model; it declares a stall only when
  the calendar phase is trading-open and no progress for the open threshold (30 s). `Unknown` phase uses a longer
  threshold (300 s); closed phases never recover for silence (fixes MD-02). A trade-built series counts a newer quote on
  its instrument as progress, and its threshold doubles after each stall recovery that no trade followed, up to eight
  minutes.

#### 3.5.2 HistoryTracker and provider_history trim (M2)

```rust
struct HistoryTracker { entries: BTreeMap<(BarSeriesKey, Epoch<ProviderTag>), SeriesHistory>, budget: HistoryBudget }
struct SeriesHistory { current: Lane, backfill: Lane, coverage: Coverage, detached: bool }
struct Lane { state: LaneState, cancel: Arc<AtomicBool>, backoff: Backoff }
enum LaneState { Idle, Deferred(HistoryRange), InFlight(HistoryTicket), RetryAt(MonotonicInstant, HistoryRange) }
struct HistoryTicket { series: BarSeriesKey, provider: Epoch<ProviderTag>, demand: Epoch<DemandTag>, lane: LaneKind, seq: u32 }
```

- Replaces the eight coordinator maps one-to-one (mapping in `10_market_design.md` 2.6). Pruning happens only in
  `retain_demanded(&engine_demand)` after demand or provider-epoch changes. The `attempts = 0` keep-alive trick
  disappears (`Deferred` is explicit).
- The history worker replaces its 1 ms sleep on a full completion channel with a blocking bounded send; current-lane
  priority with every 4th slot for the oldest backfill request is kept as lane priority.
- Decision on `aeris_provider_history` (user requested trim): `HistoryTracker` does **not** adopt
  `HistoryScheduler`; scheduling is owned by the tracker plus the market_runtime history worker. Kept: `model.rs`
  (`HistoryRange`, `HistoryCapabilities`, page types, `ProviderHistoryAdapter`, `RateLimit` used by the Rithmic
  adapter), `handoff.rs` (`HandoffCoordinator` etc., used by `rithmic_protocol/src/history_adapter.rs`), `error.rs`.
  Deleted: `scheduler.rs`, `scheduling_model.rs`, `coverage.rs`, `rate_gate.rs`, `page_validation.rs`, their
  re-exports, `tests/provider_history_conformance` (scheduler-only) and `tools/run_provider_history_conformance.sh`
  (or its scheduler half). `tests/handoff_conformance` stays. `ProviderCapabilities::history` reuses
  `aeris_provider_history::HistoryCapabilities` if it fits; no parallel type.
- `HistoryUnavailable`: when a trade-built series' covering request exhausts its bounded retry budget, the runtime opens
  a live-only series from the trade stream with the provider-neutral `HistoryUnavailable` presentation and keeps one
  slow probe (`HISTORY_UNAVAILABLE_RETRY_DELAY`, 300 s, `market_service/mod.rs`) armed instead of restarting the fast
  retry budget; a later covering snapshot merges under the trade-built bars and reseeds the live handoff from canonical.
  An exhausted backfill lane keeps its failed cooldown until fresh demand.

#### 3.5.3 Epoch, Fenced and Fence (M2)

Location: `crates/market_engine/src/fence.rs`, re-exported as `aeris_market_runtime::fence`. Rationale: every
producer and consumer of market epochs (engine, runtime, desktop, probes) already depends on the engine or runtime;
trading uses its own order `revision` and does not need market epochs; adapters do not see epochs (provider workers in
`market_runtime` stamp them).

```rust
pub struct Epoch<Tag> { value: NonZeroU64, _tag: PhantomData<fn() -> Tag> }  // Copy, Eq, Ord, Hash, Debug; no arithmetic
pub struct EpochSequence<Tag> { next: NonZeroU64 }                           // the only minting API; next() -> Epoch<Tag>
pub struct Fenced<Tag, T> { pub epoch: Epoch<Tag>, pub value: T }
pub struct Fence<Tag> { current: Option<Epoch<Tag>>, dropped: u64 }
impl<Tag> Fence<Tag> {
    pub fn advance(&mut self, epoch: Epoch<Tag>);
    pub fn accept<'a, T>(&mut self, msg: &'a Fenced<Tag, T>) -> Option<&'a T>; // equality, never >=; counts drops
}
pub enum ProviderTag {}  // provider session epoch (payload of today's ProviderGeneration)
pub enum DemandTag {}    // per-series demand epoch (payload of today's GenerationId)
pub enum CommandTag {}   // catalog search/select/screen, startup resolve
pub enum ConsumerTag {}  // registered consumer lifetime
// M6.3 target (not built): pub enum BindingTag {}  // per-pane binding epoch, minted by the desktop ChartBinding
```

- `GenerationId` and `ProviderGeneration` (`market_engine/src/lib.rs`) become `Epoch<DemandTag>` /
  `Epoch<ProviderTag>`; `ProviderEvent.generation: u64` becomes `Epoch<ProviderTag>`.
- As built, every runtime-to-consumer market event variant leaves the coordinator as
  `Fenced<ConsumerTag, MarketRuntimeEvent>` for that registered consumer lifetime, and selection-bound events are rejected
  against the exact current demand before delivery. The per-pane design that follows is the M6.3 target: pane-scoped
  publications (state, snapshot, delta, tape, order book, study outputs, `StudyRemoved`, `StudyOutputsInvalidated`, recovery, viewport) are
  `Fenced<BindingTag, PanePublication>`; command replies are `Fenced<CommandTag, _>`; `PriceAlertTriggered` is fenced
  by its alert `CommandTag` (closes MD-15). The fence is a required field of the envelope type, not of each variant.
- `CommandFences` (`market_service/command_fences.rs`), keyed by consumer, provider and command kind, replaces the
  catalog maps; the desktop `STARTUP_CATALOG_COMMAND_GENERATION = u32::MAX` sentinel is gone (startup resolution has its
  own kind). Every search and selection carries a 45-second response deadline after which the coordinator rejects it
  as timed out; consumer retirement clears every pending command through `ConsumerRegistry::remove_consumer`.
- Retirement is by construction: removing a lifecycle, cancelling a lane or advancing a fence makes prior messages
  unacceptable; no "retired" flags.
- `MarketGeneration<T>` (`crates/application/src/generation.rs`): `require_resnapshot` (no callers) is deleted in M2;
  the type is deleted in M6 when the desktop bridge stops using it.

#### 3.5.4 MarketCalendarService (M2)

```rust
pub struct MarketCalendarService { sources: BTreeMap<InstrumentKey, CalendarSource>, rules: CalendarRules }
pub enum CalendarSource { ProviderCalendar(ProviderCalendar), ContractHours(WeeklyHours), ExchangeRule(ExchangeRuleId), AlwaysOpen, Unknown }
impl MarketCalendarService {
    pub fn phase(&self, i: &InstrumentKey, at: UtcMillis) -> SessionPhase;
    pub fn session_window(&self, i: &InstrumentKey, at: UtcMillis) -> Option<SessionWindow>;
    pub fn next_transition(&self, i: &InstrumentKey, at: UtcMillis) -> Option<(UtcMillis, SessionPhase)>;
    pub fn source_kind(&self, i: &InstrumentKey) -> CalendarSourceKind;
}
```

- Domain types in `aeris_instruments` (shared by market and trading without a market-to-trading edge):
  `SessionPhase { Open, Extended, PreOpen, Break, Closed, AlwaysOpen, Unknown }`, `SessionWindow { open_utc_ms,
  close_utc_ms }`, `CalendarSourceKind`. Mapping from today's `MarketSessionPhase` (`contracts/src/messages.rs`):
  Regular -> Open, PreMarket/PostMarket/Overnight -> Extended, Closed -> Closed, AlwaysOpen, Unknown.
  `MarketSessionPhase`/`MarketSessionStatus` are deleted once nothing uses them.
- Resolution order at catalog install: provider-published calendar, provider contract hours (the recurring
  contract-hour projection is owned by `aeris_instruments::calendar::weekly_hours`, shared by market status and
  trading DAY recovery; `market_service/calendar/weekly_hours.rs` derives market session status from it), exchange rule by normalized venue code (CME Globex 17:00
  America/Chicago roll with DST, moved unchanged with its tests from `adapters/rithmic_protocol/src/calendar.rs`;
  one venue table replaces the per-adapter venue collections and Rithmic `for_venue`),
  `AlwaysOpen` for declared 24/7 venues (Hyperliquid), else `Unknown`.
- `MarketService::market_session_status` becomes a thin wrapper; the watchdog, `LiveCadence::Session`, the
  instrument descriptor and the trading feed all read this one service. `chrono-tz` stays in `market_runtime`.
- Holidays: only provider calendars carry them. An exchange rule reports Open on an unlisted holiday; trading is still
  protected because no quotes arrive and the staleness rule rejects (3.6.7). Recorded as a known limitation.

#### 3.5.5 ProviderSession, capabilities, ReconnectPolicy (M2 policy, M3 trait)

```rust
pub struct ProviderCapabilities {
    pub id: ProviderId, pub live_model: LiveModelKind, pub history: HistoryCapabilities,
    pub calendar: CalendarCapability, pub always_open: bool, pub concurrency: ConcurrencyLimits,
    pub reconnect: ReconnectPolicy, pub depth: Option<DepthCapabilities>, pub quotes: QuoteCapability,
}
pub trait ProviderSession: Send {
    fn capabilities(&self) -> &ProviderCapabilities;
    fn connect(&mut self, epoch: Epoch<ProviderTag>, sink: ProviderEventSink) -> Result<(), ProviderError>;
    fn apply_demand(&mut self, demand: &ProviderDemandSet) -> Result<(), ProviderError>;   // diff-based
    fn request_history(&mut self, request: HistoryRequest) -> Result<(), ProviderError>;
    fn release_idle(&mut self);
    fn shutdown(&mut self);
}
pub struct ReconnectPolicy { initial: Duration, multiplier: u32, cap: Duration, jitter: Jitter,
                             budget: Option<NonZeroU32>, healthy_reset_after: Duration,
                             classify: fn(&ProviderError) -> RetryClass }   // Transient | RateLimited{retry_after} | Fatal
```

- `ProviderRegistry` holds `Box<dyn ProviderSession>` by `ProviderId`; dyn dispatch per command, never per tick.
  `ProviderDemandSet` is computed by the engine; symbol, timeframe, viewport, tab and layout changes only change the
  set and never reconnect (asserted in replay).
- Reconnect defaults (`reconnect.rs` has exactly `hyperliquid()` and `rithmic_recovery()`): Hyperliquid 500 ms x2 cap
  30 s full jitter, no budget; Rithmic recovery 1 s x2 cap 30 s equal jitter. Rithmic: the
  adapter's `RithmicRetryScheduler` remains the only transport retry, and `serve_demand` non-transport recovery goes
  through `Backoff` (min 1 s) so policies no longer stack; `ConcurrencyLimits` respects Rithmic login limits. Jitter
  is seeded from the injected clock so replay is deterministic. Budget exhaustion moves the connection to `Failed`
  and fails its series; error text states what happened ("<provider>: 5 reconnect attempts failed; last error: ...").
- Presentation fields move to the published `ProviderDescriptor` (3.5.8). Bogus Rithmic values
  (`selection_entitlement_id "crypto_public_realtime"`, `" spot"` suffix) are deleted. Rithmic wire aggregation belongs to
  the Rithmic adapter (`rithmic_chart_aggregation` in `adapters/rithmic_protocol/src/history_adapter.rs`), not
  `ChartInterval`.
- Every Hyperliquid info request spends from the adapter's one process-wide weight budget
  (`adapters/hyperliquid_market/src/budget.rs`: 1,000 of the documented 1,200 weight per minute per address); no info
  request bypasses it.

#### 3.5.6 Hot path (M3)

- `MarketEngine::series_tail(key, n)` and `series_contains_time(key, ts)`: live publication reads the forming bar and
  the last completed bar; `StoredSeries::snapshot` full clones remain only for covering snapshots (resync,
  overflow, new consumer). Fixes MD-05.
- The coordinator keeps a dirty set of series touched since the last publish and publishes only those; deadlines
  (watchdog, retries, session transitions, backoff) live in `BTreeMap<MonotonicInstant, TimerKey>`; the loop waits with
  `recv_timeout(next_deadline)`, capped at the 16 ms publication cadence only while the dirty set is non-empty.
  The unconditional per-tick sweep is deleted.
- Benchmarks: extend `crates/market_engine/examples/market_data_performance.rs` (snapshot-per-publish, bytes per
  publish independent of history length) and add `crates/market_runtime/examples/coordinator_performance.rs` (64 series,
  idle CPU, overflow to covering snapshot, reseed under flow, bridge wake latency p50/p99). Baselines recorded in M2
  before any hot-path change; after numbers recorded in M3.

#### 3.5.7 Instrument leases, QuoteBook and the trading feed (M3)

One demand owner, one lease registry:

```rust
// market_engine: DataLeaseRegistry is re-keyed (single bound, single registry)
pub enum MarketDemandKey { Series(BarSeriesKey), Instrument(InstrumentStreamKey) }
pub struct InstrumentStreamKey { pub provider: ProviderId, pub instrument: InstrumentId, pub entitlement: Option<EntitlementId> }
pub enum LeaseOwner { Consumer(ConsumerId), Trading, PriceAlerts }
impl MarketEngine { pub fn set_instrument_leases(&mut self, owner: LeaseOwner, keys: BTreeSet<InstrumentStreamKey>, streams: StreamRequirements) -> Result<(), EngineError>;
                    pub fn instrument_subscriptions(&self) -> impl Iterator<Item = (&InstrumentStreamKey, StreamRequirements)>; }

// market_runtime
pub struct QuoteSubscription { /* lease guard; drop releases the lease */ }
impl MarketService { pub fn subscribe_quotes(&self, owner: LeaseOwner, key: InstrumentStreamKey) -> Result<QuoteSubscription, MarketError>; }
impl QuoteSubscription { pub fn latest(&self) -> Option<QuoteState>; }
pub struct QuoteState { pub quote: TradingQuote, pub phase: SessionPhase, pub source: QuoteSourceKind }
```

- `provider_demand` (`realtime.rs`) and `reconcile_order_books` (`coordinator.rs`) include
  `instrument_subscriptions()` with `MarketStream::Quotes`; each `ProviderSession` maps a Quotes-only instrument demand
  to its top-of-book subscription (Hyperliquid `bbo`/`l2Book`, Rithmic TOB within login
  limits). Price alerts migrate to `LeaseOwner::PriceAlerts`, deleting their
  out-of-engine merge in `realtime.rs`.
- `QuoteBook` (coordinator-owned) keeps one slot per leased or chart-active instrument (bounded by demand).
  `QuoteCapability` declares the source (dedicated stream, derived from canonical L2, last trade only); last-trade-only
  sources are flagged so trading refuses them.
- Trading feed (`trading_feed.rs`) implements the domain contract in `aeris_trading::market` (3.6.4). It is attached
  by the composition root:

```rust
let inbox = trading.attach_market_feed(market.trading_quote_interest_sink())?;   // QuoteInterest -> set_instrument_leases(Trading, ..)
market.attach_trading_feed(TradingFeedLink { events: Box::new(move |e| inbox.push(e)) });
```

  It emits `MarketFeedEvent::{Quote, Feed, Session, Terms}`: `Quote` when best bid/ask of a leased or chart-active
  instrument changes; `Feed` on provider up/down/epoch change; `Session` from `MarketCalendarService` at lease time,
  on calendar change and at each `next_transition` (coordinator deadline); `Terms` when an instrument enters the
  catalog. The conversion `InstallProviderInstrument` -> provider-neutral contract terms moves from the desktop
  (`apps/desktop/src/trading.rs` `trading_instrument_from_install`, `complete_contract_terms`) into
  `market_runtime`, including Hyperliquid's significant-figure `PriceGrid` (verified against current Hyperliquid
  documentation and observed responses before use). `market_runtime` stamps `received_unix_nanos` from its clock.
  Session facts come only from the calendar; staleness and session policy live in `trading_runtime`.

#### 3.5.8 Waitable outbox, select_and_demand, descriptors (M3 runtime side, M6 desktop side)

- `ConsumerOutbox::wait(&self, timeout) -> Drained<ConsumerEvent>` backed by `Mutex` + `Condvar` on the consumer's
  `ConsumerEvents`; the coordinator pushes and notifies. Capacity 1,024 and the covering-snapshot overflow policy are
  unchanged. Only bridge worker threads wait; never the UI thread. `poll_events` request/reply is deleted.
- `MarketCommand::SelectAndDemand { consumer, epoch: Epoch<BindingTag>, instrument: InstrumentRequest, interval:
  ChartInterval, viewport }` where `InstrumentRequest = Ref(InstrumentRef) | Default`. It resolves the instrument
  (including startup catalog resolution), resolves the series via `resolve_cadence`, installs demand and replies once:
  `Fenced<BindingTag, SelectionOutcome::{Selected { descriptor, series: ResolvedSeries }, Failed { error }}>`.
  Decision: the command is transactional; the previous demand of that consumer is released only after the new demand
  is installed, so a failed selection leaves the previous chart live and the desktop restores by re-adopting the
  previous binding epoch with no second command.
- Published descriptors: `ProviderDescriptor { id, display_name, logo: LogoKey, connect: ConnectMethod
  (VaultCredentials { form } | Public), symbol_browser: bool, autoload_catalog: bool }`,
  `InstrumentDescriptor { key, display_symbol, venue, asset_class, currency, price_scale, price_grid, offered_intervals,
  calendar_source, quote_capability, logo }`, `ResolvedSeries { interval, key: BarSeriesKey, cadence }`. Typed
  `ProviderEvent::CatalogReady { provider }` replaces `RITHMIC_CATALOG_READY_MESSAGE` text matching.
- `cadence::resolve_cadence(interval, &HistoryCapabilities, CalendarSourceKind) -> SeriesCadence` is the only
  interval mapping (introduced in M1 for MD-01): Day1/Day3 -> `SessionDays(n)` for session calendars, `FixedSeconds` for
  `AlwaysOpen` (Hyperliquid UTC-day alignment verified from documentation first); Week1 -> `CalendarWeeks(1)`,
  Month1 -> `CalendarMonths(1)`. Workspace persistence stores `ChartInterval`; a legacy reader maps both legacy Day3
  encodings (`FixedSeconds/259200`, `SessionDays/3`) to `ChartInterval::Day3`. The desktop mappings in
  `workspace_series` (`desktop.rs`), `pane_interval` (`engine_market_worker.rs`) and `series_key`
  (`replay_conversion.rs`) are deleted.
- Order-flow tape: published incrementally (`TapePublication::{Append { from_ordinal, trades }, Reset { trades }}`)
  with a runtime-computed sweep annotation (M7, 3.7.8). The sweep classifier moves with its tests and exact semantics
  (512-trade window, 90th percentile, 100 ms grouping).

#### 3.5.9 Market replay harness (M3)

Boundary: coordinator inputs (`Command`, normalized `ProviderEvent`, history completions, clock) and outputs (consumer
outboxes, provider demand diffs). `MarketService::start_composed` gains injectable realtime sessions via a
`ScriptedRegistry` implementing `ProviderSession`.

```rust
#[cfg(any(test, feature = "replay"))]
pub struct ReplayHarness { clock: ScriptedClock, coordinator: Coordinator<'static>, providers: ScriptedRegistry }
pub enum ReplayStep { Advance(Duration), SetWall(UtcMillis), Command(Command), Provider(ProviderId, ProviderEventKind),
                      HistoryReply { matching: TicketMatcher, result: FixtureHistory },
                      ExpectEvent(EventMatcher), ExpectEffect(EffectMatcher), ExpectState(BarSeriesKey, StateMatcher) }
```

Required scenarios: Day3 persistence round trip (MD-01); candle stall while open, none while closed (MD-02);
Day1 close at the CME 17:00 CT roll across a DST change (MD-03); non-CME venue resolution (MD-04); Hyperliquid backoff
schedule and Rithmic single policy (MD-12); stale-epoch history completion ignored; overflow -> covering snapshot;
reseed under live flow keeps one forming bar and contiguous completed history; switch storm (50 alternating
selections) never mutates the current binding with retired data; demand changes never reconnect; history
exhaustion; quote lease share/release with charts and alerts.

### 3.6 `trading_runtime`

#### 3.6.1 Owner thread, TradingCore and Clock (M4)

- Exactly one owner thread (`aeris-trading-owner`) is a thin shell around a synchronous `TradingCore` with three
  entry points: `handle(command, now)`, `on_market(batch, now)`, `on_timer(now)`.
- Each step computes `StepChanges`, commits them in one SQLite transaction (`StepTx`), then applies them in memory and
  publishes. A failed commit leaves memory unchanged. A step that produced no fact opens no transaction (a
  mark-to-market quote writes nothing).
- `aeris_trading_runtime::clock::Clock` (owner-local; `SystemClock` in production, `ManualClock` in tests) is injected through `TradingServiceConfig { database_path, retention, clock, execution:
  ExecutionPolicy }`. The owner waits with `recv_timeout(next_deadline - now)`; deadlines: DAY expiries
  (`BTreeMap<i64, BTreeSet<OrderId>>`), risk session rollover, equity sample flush (60 s), retention (hourly),
  coalesced publication (100 ms), broker PnL refresh. Owner-produced times are made monotonic per process. Cancel
  events use command time (fixes TR-12).

#### 3.6.2 Module layout (M4 split, M5 content)

As built:

```
crates/trading_runtime/src/
  lib.rs                re-exports and public constants
  clock.rs                                  Clock trait, SystemClock, ManualClock
  service/{mod,owner}.rs                    Command, owner loop (`aeris-trading-owner`)
  core/mod.rs                               TradingService handle, TradingCore
  publication.rs                            WorkingSet, TradingEvent, TradingSubscription
  history.rs                                read-only `aeris-trading-history` reader thread
  book/{mod,lifecycle,trigger_index}.rs     indexed OrderBook, order state machine, per-side price-keyed trigger index
  admission/{mod,capabilities}.rs
  execution/mod.rs                          fill price/quantity policies (TouchPrice, TouchLiquidity)
  market/{mod,feed,gating}.rs               QuoteBook, MarketFeedInbox, interest, session/quote gating
  groups/{mod,model,oco,owned,recovery}.rs
  ledger/{mod,position,fees,equity}.rs
  risk/{owner,session}.rs, risk.rs, discipline.rs, plan.rs
  emergency.rs, copier.rs, round_trip.rs, strategy.rs
  venue/{mod,broker,scales,scripted}.rs
  store/{mod,schema,migrate_groups,migrate_ledger,orders,fills,ledger,groups,risk,quotes,user_records,retention,export}.rs
  replay/mod.rs                             trading replay harness
```

Deleted after migration (M5): `protection.rs`, `planned_leg.rs`, `strategy.rs` (moved), `ManagedBracket`,
`ManagedBracketStatus`, `ProtectiveOrder`, `ProtectiveOrderRole`, `PlannedBracketLeg`, `MovePlannedBracketLeg`,
`BracketPlacement`, `PracticeMarketSession`, public `SimulatedMarketObservation`, `PendingObservations`,
`ObservationCursor`, `Coordinator::restored_orders`, `TradingSnapshot` (still present; its removal is the M6.11 target), `MAXIMUM_SNAPSHOT_ITEMS`, the four PnL trackers
(`next_position`, `realized_pnl`, `risk_trade_cycles`, `trade_pnl_cycles`, `completed_trade_pnl`,
`fill_realized_pnl`), `rebuild_position_from_retained_fills`, `recover_managed_brackets`, and every linear order scan.
Removed public API: `observe_market`, `publish_market_observation`, `install_practice_market_session`, `place_order`,
`place_bracket`, `place_inline_bracket`, `place_protective_order`, `move_planned_bracket_leg`, observation-taking
`flatten_account`/`flatten_all`/`reverse_position`, `snapshot`, `latest_feedback`. As built, `TradingService` still
exposes `observe_market`, `snapshot()` (compatibility view, removal is the M6.11 target) and the `place_*` command
names (`place_order`, `place_bracket`, `place_oco`, `place_inline_bracket`, `place_protective_order` and their
`*_from_market` forms).

Public API (desktop-facing; errors are `TradingCommandError { kind: TradingErrorKind, detail }` with `kind` in
`Rejected(RejectReason) | StaleRevision { current } | NotFound | Overloaded | Unavailable | Storage`):
`start`, `attach_market_feed`, `attach_demo_venue`, `demo_venue_inbox`, `create/delete_practice_account`,
`set_fee_schedule`, `set_ticket_interest`, `submit(OrderRequest)`, `submit_oco`, `submit_protection`,
`modify(ModifyRequest{expected_revision})`, `move_group_leg`, `cancel`, `cancel_all(CancelScope)`, `evaluate`
(dry-run admission), `flatten(FlattenScope)`, `reverse`, `kill_switch(KillScope)`, `lock/unlock_account`,
`set_risk_session_boundary`, `apply_economic_event_risk`, `move_bracket_level`, existing record/template/copier
registration, `subscribe`, `working_set`, `history(HistoryQuery)`, `status`, `shutdown(timeout)`.
`OrderRequest { client_order_id, account_id, instrument_id, side, quantity, kind: OrderKind, time_in_force,
attached: Option<AttachedProtection>, submitted_unix_nanos }`; `OrderKind = Market | Limit{price} | Stop{trigger} |
StopLimit{trigger, limit} | RestingAt{price}`. Provenance is stamped by the owner, never supplied by the caller.

#### 3.6.3 Order state machine (M5)

| Status | Executable | Open | Notes |
|---|---|---|---|
| `Inactive` (new) | no | yes | group child awaiting parent fill, or OCO member held by its group |
| `Pending` | no | yes | broker only, unacknowledged |
| `Working` | yes | yes | a triggered stop-limit stays `Working` with `triggered_unix_nanos` set |
| `PendingModify`, `PendingCancel` | broker decides | yes | broker only |
| `PartiallyFilled` | yes | yes | broker fills only in this scope |
| `Filled`, `Cancelled{reason}`, `Expired` (new), `Rejected{reason}` | no | no | terminal |

- `Triggered` is an event plus timestamp, not a status. Events: `Accepted{warnings}`, `Activated`, `Modified{User |
  Management | Coverage | LegMoved | ParentRepriced}`, `Resized{CoverageShrink | CoverageGrow}`, `Triggered`,
  `Filled{fill_id}`, `Cancelled{User | Cascade | KillSwitch | Flatten | NotMarketable}`, `Expired{session_end}`,
  `Rejected`. `OrderEventReason` is typed; `detail` text is display-only. `RejectReason` includes `MarketClosed {
  phase, next_open }`, `SessionUnknown`, `QuoteUnavailable`, `QuoteStale { age_ms, limit_ms }`, `OffGrid`,
  `WrongSideTrigger`, `TifNotSupported`, `ClientIdReused`, `CapacityExceeded`, `RiskLimit(..)`, `AccountLocked`,
  `CurrencyMismatch`, `InstrumentNotRegistered`, `RouteUnknown`.
- Only `book/lifecycle.rs` constructs status changes; the broker route maps `VenueEvent`s through the same functions.
- `Order.revision: u32` increments on every transition; modify/cancel/move carry `expected_revision`; mismatch returns
  `StaleRevision` with the current order (TR-10). Same client id with identical parameters returns the existing order;
  different parameters -> `ClientIdReused`.
- Timestamps: `submitted` (client, audit), `accepted`, `updated`, `triggered`, `terminal`, `expires_at` (DAY),
  `eligible_after`. Id formats (`sim-order-{n}`, `sim-event-{n}`, `sim-fill-{n}`) and `next_sequence` stay.

#### 3.6.4 Market contract and runtime-owned quotes (M4)

`crates/domain/trading/src/market.rs` (`aeris_trading::market`), depending only on `aeris_instruments`:
`MarketRoute { provider_id, instrument_id, provider_symbol, display_symbol, venue_id, entitlement_id, price_scale,
quantity_scale }`, `TradingQuote { instrument_id, bid, ask, bid_size, ask_size, last, provider_id,
provider_generation, source_sequence, exchange_unix_nanos, received_unix_nanos }`, `FeedState { Live, Stale{reason},
Unavailable{reason} }`, `InstrumentSession { instrument_id, phase: SessionPhase, source, session_end_unix_nanos,
next_open_unix_nanos }`, `InstrumentTerms` (with `InstrumentClass`, `PriceGrid`, point value/multiplier, currency),
`QuoteInterest { revision, routes }`, `MarketFeedEvent::{Quote, Feed, Session, Terms}`, `QuoteInterestSink`.

- `QuoteInterest` = instruments of simulated accounts with an open order (including `Inactive` legs), a non-flat
  position or an active group, plus the desktop ticket interest (`set_ticket_interest`, the instruments of panes with
  order entry, bounded 64). Total bounded by `MAXIMUM_QUOTE_INTEREST = 256`; admission rejects an order that would
  exceed it. `MarketRoute` is persisted on the instrument row so restart re-subscribes instruments no chart shows.
  This fixes the critical bug where closing a chart stops stops/targets from firing (TR-01, UI-01).
- `MarketFeedInbox`: latest-wins quote per instrument plus a bounded FIFO (1,024) for Feed/Session/Terms; FIFO
  overflow drops the oldest `Terms` and sets a resync flag that requests a full refresh through the sink. It never
  blocks the market coordinator; one queued `Command::DrainMarketFeed` wakes the owner.
- `QuoteBook` validates quotes (positive, bid < ask, scale equals instrument price scale) and ignores older
  `provider_generation` or lower `source_sequence` within a generation. Instruments are registered from `Terms`;
  desktop `register_provider_instrument` calls and `publish_simulated_market_observation` are deleted.

#### 3.6.5 Order groups (M5)

`OrderGroup { group_id, account_id, instrument_id, kind: Bracket | Oco | Protection, status: Pending | Active |
Completed | Cancelled, entry: Option<OrderId>, legs: Vec<GroupLeg> (bounded 1 + MAXIMUM_BRACKET_TARGETS), management:
StopManagement, template: Option<(String, u32)>, revision }`, `GroupLeg { order_id, role: StopLoss | TakeProfit{index},
offset_ticks, allocation_percent }`. Semantics are those of `09_trading_design.md` 2.4, with these binding decisions:

- Bracket legs are real `Inactive` orders created at placement (priced from the entry's resting price plus offsets,
  unpriced for market entries) using today's child client ids (`managed_child_client_order_id`), so chart lines keep
  identity. Activation prices legs from the actual fill price plus offsets (today's behavior; dragged legs keep their
  new offset), sized to the opened exposure; activation, fill, ledger entries and group status commit in one step.
  Unpriceable/inadmissible legs cancel the group with a typed `ProtectionNotPlaced` event.
- Sibling rules, cancel cascades, OCO semantics and the cross-group coverage invariant (moved from `protection.rs`
  with today's sizing rules) are as in the design. Scale-in growth keeps today's behavior (first-triggering stop and
  target grow); the target question is recorded as an open product question, not changed.
- Managed trailing/break-even ratchets persist only when the stop moves at least one tick; refused on broker routes.
  Broker brackets remain broker-owned SL/TP (no local group); since V28 the server-held target and stop levels are
  persisted in `broker_bracket_levels` with their entry order until the venue releases them.
- Restart: groups are rows written with the transitions that change them; `core/recovery.rs` verifies invariants and
  repairs deterministically with a typed `RecoveryNotice`. No in-memory fence.

#### 3.6.6 Admission (M5)

`admission::admit(ctx, request, route) -> Result<Admitted, RejectReason>` is the only validator for simulated and
broker routes and for every member of brackets, OCO and protection requests: account and route (broker orders route only through demo Rithmic logins; the venue relay refuses a live
login before opening an order plant, 3.6.12), lock and risk (`risk::evaluate`, unchanged rules), instrument registration and scales, price grid
(`PriceGrid::Fixed(tick)` exact multiple; `SignificantFigures{figures, max_decimals}`; `Unknown` -> `OffGrid`
rejection is not applied, a warning is attached), quantity increment, trigger sides (buy stop/stop-limit trigger above
ask, sell below bid; bracket legs on correct sides), TIF capability (`VenueCapabilities`; IOC/FOK only with Market and
Limit; group legs GTC; broker DAY mapped to GTC where required), client id length (per venue), open-order and quote-interest
capacity, and for the simulated route: currency match, session gating and quote freshness (3.6.7). `RestingAt{price}`
resolves to Limit (passive side of the touch) or Stop (beyond the touch); the desktop's `resting_entry_order_type` is
deleted. Hook: `admission::BuyingPower` step, a no-op now (margin is out of scope). A parity test runs the same cases
through both routes; `venue/scripted.rs` must stay green unchanged.

#### 3.6.7 Market gating and execution rules (M4 gating, M5 rules)

Gating policy (binding user decision; policy lives here, facts come from `MarketCalendarService` via the feed):

- A new simulated order of any type (including resting and bracket entries) is admitted only when the instrument's
  session phase is `Open`, `Extended` or `AlwaysOpen` and the latest quote is fresh. Otherwise:
  `PreOpen | Break | Closed` -> `MarketClosed{phase, next_open}`; `Unknown` -> `SessionUnknown`; no quote ->
  `QuoteUnavailable`; age above bound -> `QuoteStale`. `FeedState` not `Live` -> `QuoteUnavailable`.
- Staleness bound (`ExecutionPolicy::max_quote_age` by `InstrumentClass`): 10 s for `CryptoPerpetual` and
  `CryptoSpot`, 60 s for all other classes. Age = owner clock now - `received_unix_nanos`.
- Fill eligibility for resting orders on each quote: `FeedState::Live`, phase in {Open, Extended, AlwaysOpen}, quote
  from the current provider generation, and `received_unix_nanos > order.eligible_after_unix_nanos` (stamped at
  acceptance, modification or activation; replaces the `restored_orders` fence and cross-process generation
  comparison). Quotes during other phases update marks only ("hold").
- Emergency pricing (flatten, kill switch, reverse) requires a fresh quote; positions without one are reported in
  `EmergencyOutcome::unpriced`, never skipped silently.
- DAY expiry: `expires_at` fixed at acceptance as the current session's `session_end` for exchange instruments and the
  next 00:00 UTC for `AlwaysOpen` (24/7 crypto). Since orders are rejected when the session is unknown, every accepted
  DAY order has an expiry. The owner timer expires it (`Expired{session_end}`); startup recovery expires DAY orders
  whose time passed while the app was closed. A historical DAY deadline is derived only from a verified route and the
  acceptance-time open session (`aeris_instruments::calendar::weekly_hours`); unresolved legacy rows stay
  non-executable rather than receiving a guessed deadline.

Execution rules table (buy evaluates the ask, sell the bid; exact fixed point; "fresh" per the gating rules):

| Order | TIF | At placement (fresh quote) | Resting, on each eligible quote |
|---|---|---|---|
| Market | DAY/GTC/IOC/FOK | fill at ask (buy) / bid (sell), taker | never rests |
| Limit buy L | DAY/GTC | ask <= L: fill at ask (taker); else rest | ask <= L: fill at L (maker) |
| Limit sell L | DAY/GTC | bid >= L: fill at bid (taker); else rest | bid >= L: fill at L (maker) |
| Limit | IOC/FOK | marketable: fill fully at touch; else `Cancelled{NotMarketable}` same step | never rests |
| Stop buy S | DAY/GTC | admission requires S > ask | ask >= S: `Triggered`, fill at ask (gap fills at the gapped ask), taker |
| Stop sell S | DAY/GTC | admission requires S < bid | bid <= S: `Triggered`, fill at bid, taker |
| Stop-limit buy S/L | DAY/GTC | admission requires S > ask | ask >= S: `Triggered`, becomes resting limit L; same quote: ask <= L fills at ask (taker); later quotes: ask <= L fills at L (maker) |
| Stop-limit sell S/L | DAY/GTC | mirror | mirror |
| Any | IOC/FOK with Stop/StopLimit | rejected `TifNotSupported` | - |

- Without depth, IOC and FOK are identical (all-or-nothing at the touch); stated in UI help. Hooks:
  `execution::Liquidity` (only `TouchLiquidity`) for depth-aware partial fills, `execution::FillPrice` (only
  `TouchPrice`) for slippage. Both out of scope.
- Deterministic evaluation within one quote: stops and stop-limits first (trigger price, then acceptance sequence),
  then limits (price, then sequence); group reactions run after each fill before the next candidate; orders
  activated or modified in a step become eligible from the next quote.
- Contract multiplier: realized and unrealized PnL multiply by `ContractMetadata::point_value`; the simulated route
  keeps "contract currency equals account currency" (a `CurrencyConversion` hook is reserved).
- Behavior changes vs today (release-noted): resting limits fill at the limit price (was touch), wrong-side and
  off-grid orders rejected, all providers gated, DAY expires, closed/unknown sessions reject.

#### 3.6.8 Ledger, fees, checkpoints, retention (M4 accounting, M5 ledger)

- `ledger/position.rs` is the single PnL implementation: `PositionState { net_quantity, cost_basis: i128, cost_scale,
  realized_gross, fees, cycle, opened/last_fill timestamps }`; `apply_fill(position, fill, terms, fee) ->
  FillAccounting`. Realized PnL uses exact cost basis (no rounded average); rounding (nearest, ties away from zero)
  only at the final currency conversion. Reversal through zero closes and opens a cycle in the same fill. Risk,
  journal, round trips and account views read `FillAccounting` rows.
- Cash ledger `ledger_entries(kind: Deposit | Adjustment | RealizedPnl | Commission | ExchangeFee | Funding(reserved) |
  MarginReserve(reserved))`. Balance = deposits + adjustments + realized PnL - fees; equity = balance + unrealized
  (memory only; longs marked at bid, shorts at ask). Broker accounts keep the broker-reported balance as authority and
  write no cash-ledger entries; since V28 broker fills run through the same netted `positions` ledger, a venue position
  report sets only the open P&L, and a disagreeing reconcile snapshot rebases the position in one transaction.
- Fees: `FeeSchedule { by_class: BTreeMap<InstrumentClass, FeeModel>, by_instrument }`, `FeeModel = None |
  PerContract{per_side} | PerUnit{per_unit, minimum, maximum_bps} | NotionalBps{maker_bps, taker_bps}`. Default is zero
  for every class; presets ship disabled and are not claimed accurate until verified against published schedules.
- Equity history: in-memory ring per account (1,440 one-minute samples); durable `equity_samples` written by the 60 s
  timer only when equity changed plus one per fill and per risk session end; retention keeps 1-minute samples 7 days,
  hourly samples 1 year.
- Retention runs on the hourly timer and at startup, never on the quote path, at most 5,000 rows per table per run.
  In one transaction it writes `position_checkpoints` and `ledger_checkpoints` as of the cutoff, then deletes terminal
  orders/events/fills/accounting/ledger rows at or before the cutoff that no open order or active group references.
  Recovery rebuilds a position as checkpoint + later fills and compares with the stored projection (TR-06).

#### 3.6.9 Emergency operations and risk rollover (M5)

- Kill switch (TR-03): lock targeted accounts; cancel non-protective working orders; flatten every position across
  all instruments at the runtime's own fresh quotes; cancel protection of flattened positions; unpriced positions keep
  protection and are reported. One step.
- Flatten (TR-04): `FlattenScope { account: Option, instrument: Option }`, no observation parameter. Broker accounts
  use request-based close for every instrument.
- Reverse (TR-12): admitted as one operation (risk, contracts, lock validated for the opposite position first); the
  closing and opening fills commit in one step at the same quote; rejection leaves the position untouched.
- Risk session (TR-09): `risk_sessions` rows (V27) with rollover at the account's
  `RiskSessionBoundary::{UtcMidnight (default), ChicagoClose (17:00 America/Chicago, futures)}` (`risk/session.rs`,
  set with `set_risk_session_boundary`). The owner timer rolls each session in one transaction that persists the rolled
  row with the realized baseline, resets the daily meter, releases only daily-meter locks (manual locks survive) and
  writes the boundary equity sample; bracket legs count as attached stops for warnings.

#### 3.6.10 Publication API (M5)

As built in `crates/trading_runtime/src/publication.rs`:

```rust
pub struct TradingSubscription { /* slot: Arc<SubscriberSlot> (Mutex + Condvar); drop to unsubscribe */ }
impl TradingSubscription { pub fn try_recv(&self) -> Option<SubscriptionUpdate>;
                           pub fn recv_timeout(&self, timeout: Duration) -> Option<SubscriptionUpdate>; }   // background threads only
pub struct SubscriptionUpdate { pub lagged: bool, pub event: TradingEvent }
pub struct TradingEvent { pub revision: u64, pub cause: TradingEventCause, pub working_set: Box<WorkingSet> }
pub enum TradingEventCause { Snapshot, Command, Market, Mark, Timer, Resync }
```

- `WorkingSet { revision, accounts, orders (incl. Inactive held legs), order_groups, positions, fills (newest
  `WORKING_SET_FILL_LIMIT` = 500), position_pnl, account_pnl, risk_locks, recovery_notices }` - no history, so its size is
  bounded by open state (fixes the 10k-order blank UI, TR-05). Every event carries the exact full working set at its
  revision.
- Consumers subscribe through `TradingService::subscribe` instead of polling snapshots. The owner publishes after every
  revision-changing step, coalesces quote-only mark changes to one publication per 100 ms
  (`MARK_PUBLICATION_INTERVAL`) with a trailing flush, and stays silent while idle. At most eight live subscriptions
  (`MAXIMUM_SUBSCRIPTIONS`) exist, each with a 1,024-event slot (`SUBSCRIPTION_QUEUE_CAPACITY`) that drops the oldest
  on overflow and resyncs with `lagged = true` and a fresh working set.
- `TradingEvent` carries no per-event payload variants (`OrderAccepted`, `OrderFilled` and similar); risk warnings are
  typed through `OrderEventReason::RiskWarning` on order events and `aeris_trading::RiskWarning` values in
  `RiskEvaluation`.
- `history(HistoryQuery) -> HistoryPage` is served by the `aeris-trading-history` thread on a read-only SQLite
  connection (WAL) with 500-row keyset pages (`MAXIMUM_HISTORY_PAGE`); the owner remains the only writer. The
  compatibility `snapshot()` view is not bounded by stored-order count (retention bounds the live set); its removal is
  the M6.11 target.

#### 3.6.11 Trading replay harness and live probe (M4 harness, M5 scenarios and probe)

- `crates/trading_runtime/src/replay/` drives `TradingCore` with `ManualClock` and a real SQLite file in a temp dir:
  scripted quotes/sessions/feed states/clock moves; quote fixtures as CSV (`t,instrument,bid,ask`) in
  `crates/trading_runtime/src/tests/replay/`; `restart()`; `crash_during_commit(n)` via a store test hook; deterministic
  `transcript()` compared with `.golden` files beside the fixtures; seeded property runs (in-crate xorshift) asserting
  after every step: coverage invariant, no orphan legs, balance identity, position = checkpoint + replayed fills, no
  fill on a non-eligible quote, no DAY order beyond its session end.
- Goldens: `src/tests/simulated_golden.txt` and `simulated_stop_golden.txt` stay byte-identical through M4
  refactors. M5 replaces them with v2 goldens when behavior intentionally changes; each commit lists every
  transcript difference and its cause in the commit message and the tracking document.
- Required scenarios: stop-limit gap-through, DAY expiry at session end and across restart, DAY at 00:00 UTC for
  crypto, IOC/FOK at placement, marketable vs resting limit price, closed/unknown/stale rejections, multi-instrument
  flatten and kill switch, reverse rejected by risk, reconnect generation change, retention then repair, 50,000-order
  history with a bounded working-set build time, V19 fixture migration with awaiting/active/completed brackets,
  restart with working orders after the chart closes.
- Live probe: `tools/aeris_probes` binary `paper_trading_probe` (section 6.2).

#### 3.6.12 Broker venue relay

- Broker orders reach Rithmic only through the venue relay in `crates/market_runtime/src/rithmic_venue/`, started once
  by `MarketService::attach_broker_venue` (`market_service/broker_venue.rs`) on its `aeris-broker-venue` worker. It
  reads the same vault login, routes only demo logins (the Rithmic Test gateway and Rithmic Paper Trading) and refuses a
  live login before opening an order plant.
- Each connection attaches a new venue session generation with Rithmic's `VenueCapabilities` (held bracket levels,
  client order id length); a lost connection drops its request receiver, so the trading owner refuses orders until the
  next session attaches and reconciles every account. The desktop only connects the trading owner's attach and inbox
  handles to the relay.
- Broker submissions and transitions reserve capacity in the bounded venue request queue before committing their SQLite
  facts; a full or disconnected queue rejects without a database write. Broker accounts write no cash-ledger entries.

### 3.7 Desktop

#### 3.7.1 Controllers layout (M6 bridge pieces, M7 structure)

```
apps/desktop/src/
  desktop.rs                     module declarations + run(); no struct definitions (M7)
  desktop/app/{terminal_app,services,frame_scheduler,tab_strip,symbol_entry,overlay_stack,chart_menus,notices,lifecycle}.rs
  desktop/pane/{workspace_surface,chart_pane,presentation,order_flow,studies,alerts,chrome_state,context_panel,persistence}.rs
  desktop/trading_store.rs
  desktop/market_bridge.rs       thin bridge over ConsumerOutbox (replaces engine_market_worker slots and loop)
  desktop/persistence/{document,migrations,writer,local_state}.rs
  desktop/test_support/          cfg(any(test, feature = "diagnostics")): FixtureMarketWorker, PaneHarness, providers fixture data
  account/{mod,session,gate,panel,menu}.rs
```

- Controllers are plain structs with private fields, owned by one entity, returning `Changed::{None, Content, Layout}`;
  only the owning entity calls `cx.notify()`. Shared entities: `TradingStore`, `NotificationCenter`.
- Services are injected via `DesktopServices { market: MarketService, trading: TradingService, trading_feedback:
  TradingFeedback, account_service: AccountService, account: Option<DesktopAccount>, persistence: DesktopPersistence,
  clock: Arc<dyn aeris_trading_runtime::clock::Clock> }` (`apps/desktop/src/services.rs`), replacing the globals
  `trading::handle()`, `DesktopAccount::shared()`, `latest_feedback()`. Wrapping the account fields in `AccountGate` is
  the M7.2 target.
- Acceptance for the M7 split: `TerminalApp` <= 15 fields with no `struct_excessive_bools` allow;
  `WorkspaceSurface::new` without `too_many_lines`; field ownership per `11_desktop_design.md` 1.3.
- `engine_market_worker.rs`, `engine_market_worker/{publications,replay_conversion,runtime,selection_commands}.rs` and
  `market_worker.rs` collapse into `market_bridge.rs` plus pane controllers; `replay_conversion.rs` provider parsing and
  the two-step selection are deleted (M6).
- Fake-instrument CLI modes `--rithmic` and `--multi-chart` and their worker starters (`start_rithmic_catalog`,
  `start_multi_chart`) are deleted in M6. `--workspace-tabs` remains an accepted no-op alias of the default only if a
  tool still passes it; otherwise it is deleted too.

#### 3.7.2 ChartBinding (M6)

```rust
struct ChartTarget { instrument: InstrumentRef, interval: ChartInterval }
struct ChartBinding { fence: Fence<BindingTag>, epochs: EpochSequence<BindingTag>, target: ChartTarget,
                      descriptor: Option<InstrumentDescriptor>, phase: BindingPhase }
enum BindingPhase { Requested { previous: Option<LiveChart> }, Installing { previous: Option<LiveChart> }, Live, Failed { error: BindingError } }
impl ChartPaneController {
    fn accept(&mut self, msg: Fenced<BindingTag, PanePublication>) -> Option<AcceptedPublication<'_>>;  // the only drop point
}
```

- Every pane-scoped runtime message passes through `accept`; other pane controllers receive only
  `AcceptedPublication { target, kind }`, so they cannot observe retired data. App-scoped replies (symbol search,
  screener) use `Fence<CommandTag>::accept` in `SymbolEntry`/screener; no other comparison of epochs exists in the
  desktop (naming_check).
- Transitions per `11_desktop_design.md` 2.3. Failure restores the previous binding by re-adopting its epoch (the
  runtime command is transactional, 3.5.8) and posts a notice.
- Deleted: `RithmicSwitchState` (`desktop.rs`), `ready_state_can_complete_switch`,
  `stale_settled_state_during_engine_switch`, `awaits_replacement_chart`, `apply_rithmic_switch_marker`,
  `MarketWorkerMessage::EngineSwitchMarker`, the `rithmic_pending_*`/`rithmic_previous_selection` fields,
  `restore_rithmic_selection_after_failure`, `StartupResolution`/`handle_startup_catalog_event` in the desktop.
- Chart entity lifetime: one `AerisChartView` per pane, reset in place on a new snapshot, if the pinned Aeris Charts
  supports a full data reset preserving engine-owned drawings and viewport policy; otherwise the reset operation is
  added in Aeris Charts first (M7 cross-repo). `PanePresentation::apply` is the single place user chart properties
  are applied; setter errors become one pane notice.

#### 3.7.3 TradingStore (M6)

- One workspace-level entity. A dedicated bridge thread loops `subscription.recv(1 s)`, stores the latest
  `TradingUpdate` in a shared slot and marks `AppSource::Trading`; the UI frame takes it, rebuilds per-revision indexes
  (orders by instrument, fills and events by order id) once, and emits `TradingStoreEvent::{Changed, Fill,
  CommandResult, RiskWarning}`. On `lagged`, history pages are reloaded.
- Panes read `store.pane_view(account, instrument)`; account selection stays per pane (stored in `PanePresentation`)
  because it exists per pane today; this avoids an unrequested product change.
- Commands are issued from background tasks with a `RequestId`; results return to the issuing pane or the notice
  center (bounded `in_flight` map, overflow rejects with a notice).
- Deleted: `TradingPnlState` (`desktop.rs`), `keep_trading_pnl_fresh`/`refresh_trading_pnl`
  (`components/workspace_layout.rs`) and their render-path calls, the 250 ms timer, toaster snapshot diffing
  (`announce_fills`), `latest_feedback`, `resting_entry_order_type`, fabricated provenance in `trading.rs`.

#### 3.7.4 FrameScheduler (M7)

`AppSource { Summaries, Screener, Trading, Notices, PersistenceReceipts, Account, Updater, Diagnostics }`;
`DirtySet { app: AtomicU32, panes: Mutex<BTreeSet<PaneId>>, wake: UiWake }`; producers hold a
`DirtyHandle`. The wake listener requests at most one frame; the frame drains only dirty sources with existing budgets
(`MARKET_MESSAGES_PER_FRAME = 64` per pane); a source over budget re-marks itself. Layout persistence is driven by a
layout revision, not the per-frame structural compare (`persist_workspace_layout_if_changed` deleted). The 100 ms
timer is deleted; the updater registers its own timer only when update checks are enabled. `FramePollGate` is
deleted unless its tests cover a case the scheduler does not. Acceptance (measured): N idle panes produce zero drains
and zero frames per idle second; before/after idle CPU for 1, 4, 16 panes recorded.

#### 3.7.5 Workspace persistence container (M1 writer adoption, M7 container)

- `platform_runtime::durable_document::DurableDocument` is the versioned, checksummed container. It is built on
  `platform_sys::fs` and contains no rename or fsync logic of its own:
  envelope `magic "AERISWS\0" (8) | envelope_version u16 | schema_version u32 | payload_len u64 | sha256 (32) | payload`.
  `write(schema_version, payload)`: if the current primary verifies, copy it to `<file>.bak` with
  `write_file_durably`; then write the new envelope with `StagedFile` and commit. `read(max_len)`: primary, else
  `.bak`; a corrupt primary is quarantined (`<file>.corrupt-<ts>`, newest 3 kept), never overwritten.
  `RestoreOutcome::{Restored, Migrated{from}, RestoredFromBackup{quarantined}, NewerVersion{found}, Defaulted{reason}}`.
- `WorkspaceDocument` (full `WorkspaceState` proto, `crates/contracts/src/messages.rs`) is owned by the writer thread;
  the UI sends whole sections (Layout, ChartChrome, per-pane presentation) tagged with the layout revision; 100 ms
  debounce. The read-before-save in `workspace_persistence.rs` is deleted. The initial load happens in `run()` before
  the GPUI event loop starts; every later IO is on the writer thread. Shutdown waits on a condvar with a deadline.
- Chart chrome preferences fold into `WorkspaceState` (next free tag) in M7; the chart-chrome save thread,
  `ChartChromeShutdownWait` and the second writer in `apps/desktop/src/chart_chrome.rs` are deleted; its key=value
  parser survives only as a migration step.
- Migrations (`persistence/migrations.rs`), ordered pure functions with golden fixtures: step 0 legacy
  `workspace-state.frame` (v21 envelope) -> schema 1; step 1 -> 2: `LEGACY_SERIES_DEFAULT_COLORS`, area-bottom inference,
  `legacy_*_contract` study conversions, Day3/Day1 legacy cadence -> `ChartInterval`, chart chrome fold-in. Detection:
  magic present -> envelope; raw `WorkspaceState` -> schema 1 (0 if `schema_revision` unset); else legacy `.frame`.
  `CURRENT_SCHEMA_VERSION = 2`; `schema_revision` (tag 8) mirrors it. `sanitize_workspace` runs after migration.
- Corruption UX: `RestoredFromBackup` and `Defaulted` post a persistent notice with "Open folder"; `NewerVersion` opens
  defaults in memory, disables writes for the session, and shows a persistent notice. The layout is never silently
  replaced. Readiness uses the same reader in dry-run mode.

#### 3.7.6 Notifications and errors (M6)

- `UserNotice { key: NoticeKey, severity, tone, source: Pane | Workspace | Trading(RequestId) | Persistence |
  Provider(ProviderId), title, detail, action: OpenFolder | Retry | ConnectProvider(ProviderId), lifetime: Timed |
  UntilCleared | UntilDismissed }`; `NotificationCenter` (bounded; timed notices evicted first; `UntilCleared` bounded by
  key) renders through the existing toaster. Every notice is also logged; tokens never appear in notice text.
- The notice contract is staged at real ownership boundaries: M6.2 supplies the bounded center,
  sanitized pane/workspace/persistence projection and the existing chart-preference `Retry`;
  M6.9 adds descriptor-backed `ConnectProvider(ProviderId)`; M6.10 adds stable request IDs
  and `Trading(RequestId)` feedback; M7.8 adds guarded `OpenFolder` for the trusted
  persistence location. Do not render an action before its owning dispatch exists.
- Per-pane strings `indicator_message`, `symbol_message`, `price_alert_message`, `chart_state_message` are deleted.
- M6.2 resolves discarded failures at its notice and bridge paths; M6.12 enforces the
  remaining desktop-wide discard policy: user-visible failure -> notice; expected-harmless (closed
  channel at shutdown) -> explicit `if let Err` with a reason; internal -> `diagnostic!`. Dead parameters silenced with
  `let _ = x;` are removed. Active `selection_pending` state is retained to guard in-flight
  selection controls; it is not a dead parameter. Only documented, tested startup invariants
  retain production `expect`; other fallible UI-thread operations return errors.
  `apps/desktop/Cargo.toml` adds `let_underscore_must_use = "deny"` when the count reaches zero.
- No control flow on display text: `should_autoload_rithmic_catalog` -> `ProviderEvent::CatalogReady` +
  `ProviderDescriptor::autoload_catalog`; `starts_with("RISK WARNING")` -> `TradingEvent::RiskWarning`.

#### 3.7.7 Provider neutrality (M6)

- Replace `TerminalProvider`, `terminal_provider_id` literals and `FALLBACK_TERMINAL_PROVIDER` with opaque `ProviderId`
  and runtime descriptors; per-provider `Connect*` actions such as `ConnectRithmic` -> `ConnectProvider(ProviderId)`;
  `components/rithmic_login.rs` -> `components/provider_login.rs` (form from `ConnectMethod::VaultCredentials`, still
  vault-only); `rithmic_shell.rs` -> `provider_symbol_browser.rs`; entitlement strings (`rithmic-test:`), the
  `worker_identity` branches, `default_rithmic_contract_index`, `DEFAULT_RITHMIC_LISTING_QUERY` are deleted; logos by
  `LogoKey` in `assets.rs`; flatten feedback text from `ProviderDescriptor::display_name`.
- Default workspace for a fresh root: one pane sending `InstrumentRequest::Default`; the runtime's default policy picks a
  public provider instrument (today Hyperliquid BTC). If none resolves, the pane shows "choose an instrument" and
  startup still reaches the workspace.
- Interim allowlist entries are removed by the commit that deletes the named file; final allowlist: `assets.rs` only.

#### 3.7.8 Chart work relocation (M7)

| Item | Destination | Repository |
|---|---|---|
| `pending_context_menu`/`pending_activate`/`pending_settings_request`/`pending_study_remove` polled by `observe_chart` | `impl EventEmitter<ChartHostEvent> for AerisChartView`; pane subscribes once | local `chart_integration` |
| Hollow candles (`hollow_appearance`, `hollow_base`, transparent body in `view.rs`) | engine series style (e.g. `CandleBody::Hollow`) | Aeris Charts first, push, then bump only `aeris_charts_*` revisions; delete host transform |
| `classify_order_flow_sweeps` (`view/order_flow.rs`) | runtime sweep annotation at tape ingestion, exact semantics, tests moved | `market_runtime` |
| Full tape resend + ordinal diff | incremental `Append`/`Reset` (3.5.8); bounded ring in `OrderFlowPresenter`; Aeris Charts append op only if the 100k-trade replace is measured as the bottleneck | local; cross-repo only on evidence |
| `SESSION_PLAN_LEVEL_COLOR`, picker swatches and default `#335CFF`, `rgba` literal in `chart_context_menus.rs` | design-system tokens | local + `CSS.md` + `platform.css` + website mirror `aeris-website/src/styles/platform.css` (run its sync/tests) |
| Chart data reset in place (3.7.2) | engine operation if missing | Aeris Charts, same procedure |

Aeris Charts procedure: follow `aeris-charts/AGENTS.md` (work on `main`, run its full gates including wasm32 clippy,
`perf_gate --release` and the npm package scripts, update `docs/architecture.md` in the same commit, stage only owned
files, push to the `github` remote without force), then bump only the `aeris_charts_*` revisions here and check for
newly available engine capabilities.

#### 3.7.9 Account module boundary (M7; AC fixes in M1/M8)

`apps/desktop/src/account/` holds `account.rs` and the account branches from `workspace_tabs.rs`,
`components/terminal_chrome.rs`, `components/platform_menu.rs`. The rest of the desktop sees only:

```rust
pub(crate) enum AccountPresence { Disabled, SignedOut, SignedIn(AccountSummary) }
impl AccountGate { fn presence(&self) -> AccountPresence; fn begin_refresh_quiesce(&self) -> RefreshQuiesce; fn plan_policy(&self) -> PlanPolicy; }
```

Always compiled (refresh quiesce is live in production); `AUTH_BACKEND_CONFIGURED = cfg!(test)` stays inside
`account/gate.rs`; production presence is `Disabled`. Deleted: `let authenticated = true` in `schedule_market_frame`,
`CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED`, `current_plan_limits` (-> `PlanPolicy::Unrestricted`), account polling in
the frame loop and the 100 ms timer. Test: production gate opens no browser, makes no request, reaches the workspace.

### 3.8 `release_lifecycle` and account runtime fixes (M8; AC-01/AC-05 in M1)

M8.1 created the crate; release_config arrives with M8.2 and ReadinessReport with M8.5. New crate `crates/release_lifecycle` (`aeris_release_lifecycle`): `lifecycle.rs` (minus `native_data_root`, which
became `platform_runtime::DataRoot` in M1), `release_delivery.rs`, `rollout.rs`, `release_identity.rs`, new
`release_config.rs` (one `build.rs`-validated reader of `AERIS_RELEASE_VERIFYING_KEY`, `AERIS_RELEASE_BASE_URL`,
`AERIS_BOOTSTRAP_MIN_GENERATION`, `AERIS_RELEASE_IDENTITY`/`AERIS_INSTALL_GENERATION`, `AERIS_AUTHENTICODE_CERT_SHA1`,
`AERIS_AUTH_ISSUER`; any set -> all required), shared `ReadinessReport`, and `src/bin/aeris_launcher.rs` (moved from
`platform_runtime/src/bin`; since M8.7 a thin binary over the library's `launcher.rs`). `ed25519-dalek`, `semver`, `same-file` and `sysinfo` leave `platform_runtime`; the owned-root check stays in `platform_runtime::data_root`. Signers move behind a
`producer` feature. Nothing is deleted for being dormant; the "kept and sound" items in `07_platform_design.md` A2 stay.

Fixes, one commit each or small batches: LC-01 command-first dispatch (uninstall/identity never need the key);
LC-03 signature over raw manifest bytes with domain prefix `AERIS_RELEASE_MANIFEST_V1\0`, `deny_unknown_fields`;
LC-04 `AERIS_*_V1` wire tokens, transitional aliases and every `AXIUSFLOW` token deleted; LC-05 readiness migrates a
`VACUUM INTO` copy, never the real DB, plus manifest `max_store_schema` rollback gate; LC-06 all verification before
READY; LC-07 `platform_sys::trust` WinVerifyTrust, release-configured build fails without thumbprint; LC-08 typed,
logged block-delivery errors; LC-09 durable `startup-complete` marker probation (60 s bound) so hangs roll back;
LC-10 durable publishes; LC-11 one `ReadinessReport`, durable write, round-trip contract test; LC-12 Inno uninstall
page "Remove my Aeris data" unchecked by default, Axiusflow lines deleted (`tools/windows/aeris_setup.iss`); LC-13
`*.retired-<pid>` sweep replaces the `.bat`; LC-14 `message_box`; LC-15 `platform_sys::process` replaces sysinfo
polling (sysinfo removed if `cargo tree -i sysinfo` is empty); LC-16 promotion only for installed builds, child reaped;
LC-18 readiness outcome journaled before `Activated`, orphans moved to `orphaned/<ts>/`; LC-19
`LAUNCHER_PROTOCOL_VERSION`; LC-20 `release_config`; LC-02/LC-21 `tools/release_packager` (M8.7; emits signed manifest,
block plan, channel pointer, rollback data, Inno invocation; never publishes) and a `ReleaseSource` seam with a
`DirectorySource` end-to-end test (install, update, readiness, activation, forced crash, rollback, quarantine)
running in Windows CI. Readiness on the database copy is covered by the desktop's tests; the probation and hang
scenarios join the harness with LC-09 in M8.8 (D23). LC-17 (legacy data-root migration, legacy vault probes) is deleted only
after the maintainer confirms their machine is migrated.

Account runtime: AC-01 `SecretToken` (M1); AC-05 bounded exit (M1); AC-02 `credential_vault::MAX_SECRET_BYTES = 2560`
on Windows with an explicit error and const-asserts on account limits (`MAXIMUM_PROFILE_VAULT_BYTES` lowered within
it); AC-03 vault IO outside the account mutex (compute under lock, IO unlocked, re-apply with the existing generation
fence); AC-04 issuer from `release_config` at build time, runtime override only under `debug_assertions`, delete the
`AXIUSFLOW_AUTH_ISSUER` tests; AC-06 poison recovery (`unwrap_or_else(PoisonError::into_inner)`) with a poisoning
test; AC-07 avatar bytes via ureq in `account_runtime`, Zed `reqwest_client` removed from the desktop and
`http_wiring_tests.rs` updated; AC-08 test that an empty issuer starts no network work.

M8 trims: `third_party/gpui_base/src/input/editor/lsp` and any other unused editor code (about 5.4k lines; verify no
references first), stale tools (`prepare/run/finalize/verify/test_physical_pacing_*`, `verify_native_transition_capture.ps1`
and its test, `run_diagnostics_overhead_benchmark.sh`) with their CI steps, `live_market_gate.rs` moved out of the
production library into test support for its test and `rithmic_test_smoke`, unused deps (`cargo tree -i` evidence per
removal), unused prost messages (`GetAccountStatus`, `RefreshAccountProfile`) if unreferenced, PL-17 user agent from
`CARGO_PKG_VERSION` and a bounded context request wait, `EmbeddedReplaySource` under test support (G5).

### 3.9 `naming_check` target rules

`tools/naming_check` stays authoritative. Each rule changes in the same commit as the code it guards, with a positive
and a negative unit test, and every new restriction is recorded in the repo `AGENTS.md` in that commit (section
"Architecture invariants" or a new "Stabilization restrictions" list). Rules only get stricter during the mission,
except the deliberate CI and source-pin replacements below.

Structural rules (target set by M8):

1. Dependency direction table from `Cargo.toml` files (2.2 rules 1-8), including the composition-root list.
2. `unsafe`/`allow(unsafe_code)` only under `crates/platform_sys/src` (until M8.3, plus the exact `platform_runtime` FFI
   files on the `naming_check` allowlist); `platform_sys` lint config present.
3. No `fs::write(`, `fs::rename(`, `File::create(` followed by rename, or `MoveFileExW` outside `platform_sys` in
   production code of desktop, runtimes, platform, release crates (explicit allowlist: directory moves in lifecycle,
   user-chosen PNG export).
4. Provider neutrality: no provider names (`rithmic`, `hyperliquid` or any other registered provider, case-insensitive) or
   provider id formats in production sources of `apps/desktop/src`, `crates/ui/chart_integration/src` and the neutral
   `market_runtime` modules (`lifecycle/`, `history/`, `calendar/mod.rs`, `quotes.rs`, `outbox.rs`, `trading_feed.rs`,
   `coordinator.rs`); allowlist with reasons, stale entries fail the test.
5. Fencing: no `generation: u64` (or `u32`) fields in public `market_runtime` event types; every consumer event type
   is a `Fenced<..>`; desktop compares epochs only via `Fence::accept`; no `u32::MAX` command generation.
6. Desktop: no `SeriesCadence::`/`BarSeriesKey {` construction, no `SimulatedMarketObservation`, no
   `service.snapshot()`, no `starts_with("`/`contains(` against `*_MESSAGE` constants, `FixtureMarketWorker` only in
   `test_support/` or test modules.
7. `chart_integration_has_no_order_flow_analytics` (`classify_order_flow_sweeps`, `hollow_appearance`, `hollow_base`,
   `pending_context_menu`, `take_pending_`); colors from tokens (no `Color::rgb(`, `rgba(`, hex literals outside
   `design_system`, the picker spectrum and `persistence/migrations.rs`).
8. No `AXIUSFLOW` in production code; no `derive(Debug)` on secret-bearing type names except redacting wrappers.
9. `rusqlite` only in `trading_runtime`; `trading_runtime` and `market_runtime` never depend on each other.
10. Ratchets in `tools/naming_check/ratchets.toml`: per-crate maximum counts of production `Result<_, String>` and
    `let _ =`; a count above its ratchet fails; a lower count requires lowering the ratchet in the same commit.
11. Aeris Charts pin: all `aeris_charts_*` revisions in `Cargo.toml` are identical and match `Cargo.lock`
    (replaces the hard-coded `expected_revision` string that churned in 135 commits).
12. CI shape (3.10): triggers, hosted runner labels, read-only permissions, concurrency, `AERIS_RITHMIC_KIT_DISABLED=1`,
    no `pull_request_target`, no `secrets.` on the pull-request path, self-hosted runners only in
    `workflow_dispatch`-only workflows, and the `macos_preview.yml` single-secret kit exception.

Removed during the mission: source-text pins on implementation details (for example `MAXIMUM_SNAPSHOT_ITEMS`,
`mpsc::sync_channel(COMMAND_CAPACITY)` strings, `series_live: super::SeriesLiveMap::default()`, file-path pins that
move) are replaced by behavior tests in the owning crate in the commit that moves the code; pins on dormant release
code move to behavior tests in `release_lifecycle` (M8). The rule `workflows_never_run_fork_pull_requests` is
replaced by rule 12 in M1.

### 3.10 CI (M1)

`.github/workflows/ci.yml`, header comment rewritten (public repos; hosted runners are free):

```yaml
on: { push: { branches: [main] }, pull_request: { branches: [main] }, workflow_dispatch: {} }
permissions: { contents: read }
concurrency: { group: ci-${{ github.ref }}, cancel-in-progress: true }
jobs:
  windows:  runs-on: windows-latest, timeout-minutes: 120
    steps: checkout; `rustup show` (toolchain from rust-toolchain.toml);
           cargo fmt --all -- --check
           cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
           cargo test --workspace --all-features --locked
           cargo run -p aeris_naming_check --locked
  ubuntu:   runs-on: ubuntu-latest, timeout-minutes: 30
    steps: cargo fmt --all -- --check; cargo test -p aeris_naming_check --locked; cargo run -p aeris_naming_check --locked
  market_performance: if workflow_dispatch, runs-on: windows-latest   (market engine and coordinator baselines)
env: { AERIS_RITHMIC_KIT_DISABLED: "1", CARGO_INCREMENTAL: "0", CARGO_PROFILE_DEV_DEBUG: line-tables-only }
```

- Third-party actions are pinned by full commit SHA. No secrets. `CARGO_INCREMENTAL=0` and
  `CARGO_PROFILE_DEV_DEBUG=line-tables-only` keep the hosted disk within limits; if the Windows job still runs out of
  disk, workers escalate rather than drop a gate.
- Hosted runners lack the Rithmic kit; the workflow sets `AERIS_RITHMIC_KIT_DISABLED=1`, so kit-gated code and tests are
  skipped there and remain local-only. This is documented in the workflow and `Readme`.
- Live, credentialed and perf lanes (`live_market_gates.yml`, the `market_performance` job, macOS workflows) stay
  `workflow_dispatch`-only. `pull_request_target`, `secrets.` references and self-hosted runners are forbidden on the
  pull-request path, and any workflow with a self-hosted runner stays `workflow_dispatch`-only in every trigger form.
- The manual `macos_preview.yml` tester build is the one hosted lane that may receive the Rithmic kit: only when its
  `rithmic_kit` input is set does it unpack the kit's `proto` folder from the `AERIS_RITHMIC_KIT_TAR_XZ_B64` repository
  secret (its only secret), and it deletes the kit right after the build and fails if the kit was not compiled in.
  `tools/naming_check` enforces this shape across every workflow.
  Stale verifier steps are removed with their tools in M8.

---

## 4. Cross-cutting invariants (testable)

1. Exactly one `MarketEngine` per process owns all market demand; series, instrument-quote, alert and trading demand
   are leases in one bounded `DataLeaseRegistry`.
2. Exactly one provider session per provider host; selection, interval, viewport, tab, layout and trading-interest
   changes never reconnect a healthy session (replay assertion on demand diffs).
3. Each demanded series has exactly one `SeriesLifecycle` and at most one forming bar; completed history stays
   contiguous across seed, reseed and reconnect.
4. No market message reaches pane state unless `ChartPaneController::accept` matched its `Epoch<BindingTag>`;
   epoch comparison is equality.
5. A history completion, provider event or command reply with a superseded epoch or ticket never mutates state.
6. Every session phase used by market or trading code comes from `MarketCalendarService`.
7. Exactly one trading owner thread mutates trading state; every trading state change is committed to SQLite before it
   is visible in memory or published.
8. A new simulated order is rejected with a typed reason unless the phase is Open, Extended or AlwaysOpen and the quote
   is fresh (10 s crypto, 60 s otherwise).
9. A resting simulated order never fills on a quote received at or before its `eligible_after` time, from a superseded
   provider generation, while the feed is not Live, or while the phase is not tradable.
10. Buys fill at the ask, sells at the bid; resting limits fill at their limit price; stops fill at the touch,
    including through gaps; no fill price is ever better than the limit.
11. No DAY order remains open after its `expires_at`, including across restarts.
12. For every simulated account: balance = deposits + adjustments + realized PnL - fees; every position equals its
    latest checkpoint plus subsequent fills.
13. After every fill, for each protective role the total protective quantity equals |net position| or is zero; no
    protective order exists for a flat position; no orphan group legs exist.
14. Orders, positions and groups survive restart unchanged; stops and targets keep firing with no chart open.
15. Users' `trading.sqlite3`, `workspace-state.pb` and chart chrome are migrated, never wiped; every schema migration
    is preceded by a backup.
16. Every production queue, map, cache, retry and history window has a named bound and a tested overflow behavior.
17. The UI thread performs no blocking IO and no blocking wait; background threads only mark dirty sources.
18. An idle workspace produces zero frames, zero trading publications and zero pane drains per idle second.
19. No `unsafe` outside `crates/platform_sys`; every `unsafe` block has a `// SAFETY:` comment.
20. Every durable file write goes through `platform_sys::fs`; a crash at any point leaves either the old or the new
    complete content.
21. One process per data root; a second instance on the same root exits without touching state.
22. A validation-mode process never reads or writes production vault entries.
23. No secret (token, password, credential) is ever formatted into a log line, crash file, notice or evidence file.
24. The desktop production source contains no provider names except the allowlisted logo file mapping.
25. Every production panic is captured in a crash file; every `diagnostic!` line reaches the rotating log unless
    counted as dropped.
26. Dependency direction holds per 2.2; `trading_runtime` and `market_runtime` never depend on each other.
27. Every price is fixed point with an explicit scale; required wire fields are validated, never defaulted.
28. Readiness never writes the real trading database (M8 onward); a candidate build cannot brick rollback.
29. Workspace restore never silently resets: corruption, backup use and newer versions produce a persistent notice.
30. Superseded code is deleted in the feature that replaces it; `naming_check` ratchets never increase.

---

## 5. Data migration and compatibility

| Artifact (production root `%LOCALAPPDATA%\Aeris`) | Today | Migration | Safety net |
|---|---|---|---|
| `trading\trading.sqlite3` (+ `-wal`, `-shm`; existing `.pre-repair-*` files untouched) | `user_version` 19 at mission start; `SCHEMA_VERSION` 28 now | M1: table-driven V1..V19 (no data change). As built (`store/schema.rs`): V20 nullable `market_route_json` (migrated legacy instruments stay route-less until verified `Terms` arrive; such instruments show "feed unavailable" per order until re-resolved). V21 `fill_accounting`, `trade_cycles`, indexes, backfilled from `fills.realized_pnl_*`/`completed_trade_pnl_*` and the two cycle tables. V22 order lifecycle columns (`revision=1`, `accepted=updated=submitted`, `reason_code` derived once from known detail strings, detail kept). V23 `order_groups`/`order_group_legs` with Rust data migration (`store/migrate_groups.rs`): awaiting brackets -> Pending groups with `Inactive` legs priced exactly as `planned_legs` projected, active brackets link existing orders, terminal brackets -> terminal groups, `protective_orders` -> one-leg Protection groups, unpriceable -> Cancelled group + `RecoveryNotice` shown at first start. V24 drops `managed_brackets` and `protective_orders`, applied only by the gated `finalize_group_migration` step after the group backfill and the runtime conversion of awaiting brackets (a failed conversion keeps its rows and retries next start). V25 `ledger_entries` (`Deposit` from `starting_equity`, `RealizedPnl` from `fill_accounting`), `fee_schedules` (zero), exact position cost-basis columns (cost basis = stored average x quantity, no PnL jump) and instrument `instrument_class`/`price_grid_json`, backfilled once by `complete_post_group_migrations` (`store/migrate_ledger.rs`). V26 `position_checkpoints`, `ledger_checkpoints`, `equity_samples`. V27 `risk_sessions` (seeded from each risk profile's registration baseline) and `risk_locks.kind`. V28 netted broker accounts: drops `broker_positions` and `broker_deals.broker_position_id`, adds `broker_bracket_levels`. Only the old P&L tables (`trade_pnl_cycles`, `risk_trade_cycles`) and per-fill P&L columns remain, unread, until a later removal migration. Open DAY orders get `expires_at` at recovery only from a verified route and the acceptance-time open session; unresolved legacy rows stay non-executable rather than receiving a guessed deadline. | `VACUUM INTO trading\backups\trading-v{old}-{unix}.sqlite3` before the first step (newest 3 kept); per-step transactions; a fixture test per version step (pattern `migration_v18_preserves_v17_rows_and_adds_empty_broker_tables`); newer schema refused with a typed error |
| `workspace-state.pb` (raw proto, `schema_revision` 1); legacy `workspace-state.frame`; `workspace-state.pb.bak` | no real versioning | M1: written through the durable writer (format unchanged); legacy Day3 reader maps `FixedSeconds/259200` and `SessionDays/3` to `ChartInterval::Day3` (fixes the broken restore). M7: first save after upgrade writes the `DurableDocument` envelope, schema 2, after the migration chain; `workspace-state.pb.pre-v2` copy kept once | old `.bak` read as fallback; corrupt primaries quarantined; newer versions never overwritten |
| `desktop\chart-chrome` (unversioned key=value) | own writer | M1: durable writer. M7: folded into the workspace document by migration step 1->2; the old file is left untouched (not deleted) for one release and no longer written | parse failures reported as a notice, not silently defaulted |
| Vault `com.aeris.terminal` (Windows Credential Manager) | legacy `tradingplot`/`axiusflow` probes | No production key changes. Validation namespaces are new. AC-02 size cap rejects oversize writes explicitly (existing entries are already within the OS limit). Legacy probes removed only after maintainer confirmation | production namespace never written by validation instances |
| `logs\aeris*.log`, `logs\crash-*.log` | none | new; rotating 4 MiB x 5, newest 10 crash files | - |
| `instance.lock` | none | new, per data root | released on exit |
| `trading` export directory files | non-atomic | durable writes (M1) | - |
| Release install root (`active/`, journal, `.Aeris-lifecycle.lock`) | dormant | M8 fixes keep the on-disk layout; journal gains the readiness outcome before `Activated`; orphans moved, never deleted | readiness on a DB copy |
| Validation roots under `%LOCALAPPDATA%\Temp\aeris_validation\` | helper-managed | `--validation-root` replaces the `LOCALAPPDATA` override (the override keeps working for data isolation) | - |

Golden test files (`trading_runtime/src/tests/simulated_golden.txt`, `simulated_stop_golden.txt`) may be updated only
when behavior intentionally changes (M5), with every difference explained in the commit and the tracking document.

---

## 6. Testing architecture and headless probes

### 6.1 Harnesses, fixtures, clocks

| Layer | Location | Notes |
|---|---|---|
| Clock | owner-local: `crates/trading_runtime/src/clock.rs` and `crates/market_runtime/src/market_service/clock.rs` | Trading: `pub trait Clock { fn wall_unix_nanos(&self) -> i64; fn monotonic(&self) -> Duration; }`, `SystemClock`, `ManualClock`; used by the trading core and, as `Arc<dyn aeris_trading_runtime::clock::Clock>`, by desktop `DesktopServices`. Market: crate-private `Clock { monotonic() -> Instant; wall_nanos() -> Option<i64> }`, `SystemClock`, `ScriptedClock`; used by market lifecycle, watchdog, history retries and backoff jitter seeding. Direct system time belongs only in each `SystemClock`. No direct `Instant::now()`/`SystemTime::now()` in `lifecycle/`, `history/`, `calendar/`, `reconnect.rs`, `trading_runtime/src/core`, `book`, `execution`, `ledger` (naming_check) |
| Market characterization (M2) | `crates/market_runtime/src/market_service/tests/characterization.rs` | coordinator-boundary tests before structural changes |
| Market replay (M3) | `crates/market_runtime/src/market_service/replay/`; fixtures `crates/market_runtime/tests/fixtures/replay/*.json` | sanitized JSON faithful to provider shapes, recorded from Hyperliquid public data only; no credentials or account data |
| Trading replay (M4/M5) | `crates/trading_runtime/src/replay/`; fixtures and `.golden` transcripts in `crates/trading_runtime/src/tests/replay/` | CSV quotes, temp SQLite, restart, crash hooks, seeded property runs |
| Store migrations | `crates/trading_runtime/src/store/tests/` | fixture DB per historical version built in-test |
| Desktop controllers | pure tests per controller; GPUI `#[gpui::test]` with `VisualTestContext` and `PaneHarness` in `apps/desktop/src/desktop/test_support/` | assert rendered projections, never private fields; interleaving tests over all permutations of N/N+1 publications |
| Persistence | `persistence/migrations.rs` golden fixtures (legacy `.frame` v21, schema-1 proto, chart-chrome key=value) | sanitized |
| Platform | `platform_sys` crash child-process tests; observability rotation/overflow/panic child tests; instance-lock tests | - |
| Benchmarks | `market_engine/examples/market_data_performance.rs`, `market_runtime/examples/coordinator_performance.rs`, trading quote-throughput bench in `trading_runtime` (ignored test, release) | before/after numbers recorded in the tracking document |

### 6.2 Headless probes (`tools/aeris_probes`, workspace member, binaries, never shipped)

All probes: `--validation-root <dir>` (required; refuses the production root), `--out <file.json>`, exit 0 only when
every check passed, network required (not part of `cargo test`). Evidence envelope (JSON):

```json
{ "schema": "aeris.probe.v1", "probe": "<name>", "git_revision": "<sha>", "source_clean": true,
  "started_at": "<rfc3339>", "finished_at": "<rfc3339>", "validation_root": "<dir>",
  "vault_namespace": "validation",
  "outcome": "passed|failed",
  "checks": [ { "id": "<stable id>", "outcome": "passed|failed|skipped", "detail": "<text>", "measured": { } } ],
  "metrics": { } }
```

No secrets, account numbers (masked to last 2 digits), or raw private payloads in evidence.

1. Market lifecycle probe: `cargo run -p aeris_probes --bin market_lifecycle_probe --locked -- --validation-root <dir>
   --out <json> [--instrument BTC] [--duration-secs 120]`. Live Hyperliquid. Checks: `catalog_resolved`,
   `select_and_demand_selected`, `history_contiguous` (1m, 1h, 1d; interior gaps 0), `live_forming_bar_seen` (within
   30 s), `bar_close_contiguous`, `symbol_switch_no_reconnect` (BTC->ETH->SOL, provider epoch unchanged),
   `interval_switch_no_reconnect`, `stale_publications_dropped` (count of fenced drops recorded), `day1_alignment`,
   `quote_subscription_live` (QuoteSubscription latest age < 10 s), `outbox_bounded`, `shutdown_latency_ms` (< 2,000).
2. Paper trading probe: `cargo run -p aeris_probes --bin paper_trading_probe --locked -- --validation-root <dir>
   --out <json> [--instrument BTC] [--no-fees]`. Live Hyperliquid quotes through the real feed wiring. Checks:
   `terms_registered`, `account_created`, `market_buy_filled_at_ask`, `resting_limit_working`,
   `stop_limit_working_untriggered`, `bracket_legs_inactive`, `ioc_not_marketable_cancelled`, `stale_or_closed_rejections`
   (via an instrument with no quotes: `QuoteUnavailable`), `restart_state_identical` (orders, groups, positions,
   ledger, revision), `quotes_resume_without_ui`, `day_expiry_utc_midnight_scheduled`, `flatten_all_flat`,
   `ledger_identity` (balance = deposit + realized - fees), `export_atomic_parity`.
3. Retired: the broker demo regression probe was removed together with its broker provider.
4. Desktop readiness: `aeris_desktop --validation-root <dir> --desktop-readiness <report.json>`. Report schema 5 from
   M1 (adds `instance_lock`, `log_path`, `vault_namespace`, `validation_mode`), shared `ReadinessReport` type from M8;
   exit 0 in under 5 s with all `services.*` true; from M8 the real trading DB `user_version` is unchanged.

GUI validation uses `aeris_desktop --validation-root <dir>` through the mission helper scripts (window-only capture,
posted input, UIA), one GUI validator at a time; stderr `AERIS_LIVE_*` lines are capped at 256 per view, so liveness is
asserted early or through probes.

---

## 7. Milestone map

| Milestone | Components (sections) |
|---|---|
| M1 foundations | tracking doc `plan/stabilization/stabilization_work.md` + reports copied to `plan/stabilization/reports/` (account identifiers redacted, the repo is public) + this document as `plan/stabilization/architecture.md`, `AGENTS.md` allows `plan/stabilization`; CI (3.10) with naming_check rule 12; `platform_sys` + all FFI moved, dead FFI deleted, `platform_runtime` back to `forbid` (3.1); durable-writer adoption incl. atomic trading export; AC-01 `SecretToken`; log sink + panic hook + fatal dialog (3.2); AC-05 bounded exit; `DataRoot`, `InstanceLock`, validation mode + vault namespace (3.3); SQLite hardening + migration table + backups (3.4); MD-01 fix via `market_runtime::cadence::resolve_cadence` + legacy Day3 reader; readiness schema 5 |
| M2 market foundations | characterization tests; benchmark baselines; `market_service::clock` adoption in market_runtime; `MarketCalendarService` + watchdog for all models + session cadences (MD-02/03/04); `ReconnectPolicy`/`Backoff` (MD-12); `Epoch`/`Fenced`/`Fence` + `CommandFences` (removes the `u32::MAX` sentinel with a minimal desktop adaptation); `HistoryTracker` + worker blocking send; `provider_history` trim |
| M3 market lifecycle | `SeriesLifecycle` (candle path, then trade path); hot path (`series_tail`, dirty set, deadline timers) with measured numbers; `ProviderSession`/`ProviderCapabilities`/`ProviderRegistry`; `MarketDemandKey::Instrument`, `QuoteBook`, `QuoteSubscription`, price alerts as leases, `trading_feed.rs` + `aeris_trading::market` contract + terms conversion; waitable outbox, `select_and_demand`, descriptors, `CatalogReady` (runtime side; the desktop bridge consumes them minimally, full desktop rework in M6); `Fenced` study/alert events; replay harness + scenarios; `market_lifecycle_probe` |
| M4 trading core | `trading_runtime::clock` injection, owner shell + `TradingCore` with commit-then-apply; trading replay harness v1 (goldens unchanged); indexed `book/`; module split; runtime-owned quotes (`market/`, inbox, interest incl. ticket interest, desktop wiring, delete relay and desktop registration); provider-neutral gating (3.6.7 policy, `eligible_after`, delete `PracticeMarketSession` and `require_practice_market_session`); V21 single fill accounting; facts-only persistence + retention timer + quote-throughput bench |
| M5 trading engine | V22 state machine + typed events/reasons; single admission + price grid + capabilities; execution rules v2 + DAY/IOC/FOK + trigger index; V23 order groups + migration, then the gated V24 drop of the protection tables; V25 ledger/fees, V26 checkpoints/equity/retention; emergency ops; V27 risk rollover; publication API + paged history + 50k-order test; v2 goldens; `paper_trading_probe` |
| M6 desktop bridge | `DesktopServices` + `test_support`; `NotificationCenter` + discarded-result resolution; `ChartPaneController`/`ChartBinding`; `market_bridge.rs` on the waitable outbox; single `select_and_demand` per selection; descriptor rendering, typed provider/risk events, provider login/symbol browser from descriptors, neutrality rule; delete `--rithmic`/`--multi-chart`; `TradingStore` consuming `subscribe()`; delete `MarketGeneration<T>` |
| M7 desktop structure | controller split of `TerminalApp`/`WorkspaceSurface`/`AerisChartView` glue; `AccountGate` module; `ChartHostEvent`; `FrameScheduler`; `DurableDocument` + versioned workspace document + migrations + chart chrome fold-in + corruption UX; Aeris Charts hollow style (and chart reset if needed) with pin bump; runtime sweep annotation + incremental tape; color tokens with website mirror; desktop tests rewritten against controller boundaries |
| M8 lifecycle and trim | `release_lifecycle` crate split + LC fixes + `release_packager` + e2e harness; AC-02/03/04/06/07/08; naming_check reduced to the structural set (3.9) with ratchets; trims (gpui_base LSP editor, stale tools, deps, `live_market_gate`, unused prost messages, Axiusflow residue); README and `AGENTS.md` final update |

Cross-milestone dependency order:

- M1 precedes everything (durable writer, logging, instance lock and validation mode are used by every validator).
  Inside M1: `platform_sys` -> writer adoption; AC-01 -> log sink; AC-05 -> instance lock; migration table -> backups.
- M2 -> M3 (lifecycle needs clock, calendar, epochs, tracker). M3 trading feed -> M4 runtime-owned quotes.
- M4 -> M5. M5 publication API -> M6 `TradingStore`. M3 outbox/select/descriptors -> M6 bridge and neutrality.
- M6 `ChartBinding` + notices -> M7 controller split and FrameScheduler. M7 Aeris Charts work requires the user's
  staged aeris-charts work to be committed first.
- M8 release split may start after M1 but lands last to avoid churn in moved files; LC-05/LC-11 depend on the M1
  store and readiness work.
- Parallelism: M2 and M4-preparatory domain work (`aeris_trading::market`, `PriceGrid`, `InstrumentClass`) can overlap;
  no other milestone overlaps because of shared files.

---

## 8. Open risks and handling (decide vs escalate)

Workers decide locally when the choice stays inside one ownership boundary, keeps every invariant in section 4, and
does not change user-visible behavior beyond what this document approves. Workers escalate to the orchestrator (with
evidence and options) otherwise.

| Risk | Handling |
|---|---|
| Lifecycle port regressions (detached windows, FromTimeOnly history, sequence gaps) | Decide: characterization tests first, candle path before trade path. Escalate if a characterization test must change expected output. |
| Calendar correctness (DST, holidays, venue normalization) | Decide: move CME rules unchanged with tests; publish calendar provenance. Escalate any instrument class that would resolve to `Unknown` for a provider that trades today (it becomes untradable in paper trading). |
| Equities extended hours mapping to `Extended` | Decide per provider evidence; escalate if a provider cannot distinguish extended from regular sessions. |
| Provider Quotes-only demand per provider (Hyperliquid `bbo`, Rithmic TOB limits) | Decide for Hyperliquid with fixture-backed tests; Rithmic is verified with scripted sessions only. Escalate before any live Rithmic session (excluded). |
| Hyperliquid significant-figure price grid and Day1 alignment | Verify from current Hyperliquid documentation and observed public responses; until verified, `PriceGrid::Unknown` (warn, do not reject). Escalate if docs and responses disagree. |
| Bracket data migration (V22) on live user data | Decide within the approved rules; never delete a user row without a migrated equivalent; backup first. Escalate any case that would cancel a working order. |
| Golden transcript changes | Decide only in M5 with per-difference explanations; escalate any golden change in M4. |
| Scale-in target growth, risk session default, per-pane vs workspace account | Decided here (keep today / `RiskSessionBoundary::UtcMidnight` default, `ChicagoClose` for futures / per pane). Escalate if implementation evidence contradicts. |
| Hosted CI disk/time limits on `windows-latest` | Decide caching/profile settings; escalate before dropping or splitting any gate. |
| Chart entity reset requiring Aeris Charts changes | Decide the engine API in Aeris Charts following its AGENTS.md; escalate if the user's staged aeris-charts work is still uncommitted. |
| `NativeSessionShutdownGuard` and `CancelIoEx` removal | Delete only with proof (abort-latency test; manual logoff flush test by the user). Otherwise keep in `platform_sys`. |
| Legacy data-root migration and vault probes (LC-17) | Escalate; delete only after the maintainer confirms. |
| Readiness `provider_count > 0` on fresh installs, uninstall Inno page | Decide implementation; manual install/uninstall verification is user-only and reported as unverified. |
| `quick_check` cost, handshake timing, idle CPU, tape apply, hot-path numbers | Measure and record; never claim improvement without numbers. |
| Public repository disclosure (reports, fixtures, evidence) | Decide redaction; escalate if any credential, token or full account number is found in history or fixtures. |
| Rithmic live behavior, production release publication | Out of scope for validation; report as user-only. |
| A worker finds that an approved design element breaks an invariant in section 4 | Stop and escalate with the conflicting invariant; do not weaken the invariant. |
