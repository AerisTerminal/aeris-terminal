# Desktop decomposition and provider neutrality: design

Scope: `apps/desktop` and `crates/ui/chart_integration` in `C:\Users\devraj\Downloads\Softwares\aeris-terminal`.
This is a design only. Nothing in the repository was modified. It closes audit findings UI-01 to UI-16
(`03_desktop_ui.md`) and the desktop side of cross-cutting root causes 1 to 4 and 6 (`05_cross_cutting.md`).

Inputs from the other two designers are named here as contracts rather than final type names:

| Ref | Owner | Contract this design consumes |
| --- | --- | --- |
| M1 | market | Every publication delivered to a pane carries the `BindingGeneration` of the demand that produced it (state, snapshot, update, tape, order book, studies, recovery, viewport). The generation is minted by the requester, or returned synchronously by the command, before any publication for it can be delivered. |
| M2 | market | One `select_and_demand(generation, InstrumentRef or DefaultInstrument, interval)` command that resolves the instrument (including startup catalog resolution), installs demand, and replies once with `Selected { generation, descriptor }` or `Failed { generation, error }`. |
| M3 | market | `InstrumentDescriptor` (venue, display symbol, asset class, currency, tick, entitlement token, logo key) and `ProviderDescriptor` (opaque `ProviderId`, display name, logo key, connect method, login form schema, symbol-browser capability). Typed `ProviderEvent::CatalogReady { provider }`. |
| M4 | market | A waitable consumer outbox per endpoint (no 2 ms polling), publications typed by the runtime with revisions. |
| M5 | market | Incremental tape publication (`Append { generation, from_ordinal, trades }` / `Reset { generation, trades }`) and a sweep annotation computed by the runtime. |
| T1 | trading | Revisioned change stream: a waitable revision counter plus `changes_since(revision)` or `snapshot()` when the gap is too large. |
| T2 | trading | Per-command results correlated by `RequestId`, replacing the global `latest_feedback()` slot. |
| T3 | trading | Typed `TradingEvent::RiskWarning { kind, detail }` instead of the `"RISK WARNING"` text prefix (`trading_runtime/src/lib.rs:4994`, matched at `desktop.rs:2017`). |
| T4 | trading + market | Runtime-to-runtime BBO and trade feed for the simulated venue (UI-01), so the desktop relay can be deleted. |

If either designer changes a contract, only the commit in section 9 that consumes it changes.

---

## 1. Target module layout and ownership

### 1.1 Principles

- Controllers are plain structs with private fields and a narrow method API. They are owned by one GPUI
  entity and are not entities themselves, except where state is shared across panes (`TradingStore`,
  `NotificationCenter`). This avoids a second layer of entities and keeps `cx.notify()` decisions in the owner.
- Controller methods return what changed (`Changed::{None, Content, Layout}` or a small enum). The owning
  entity decides whether to notify and whether to bump the layout revision. No controller calls `cx.notify()`.
- The current `#[path]` modules in `desktop.rs` that reach into `WorkspaceSurface` fields through `pub(super)`
  are replaced by real module directories. Fields become private to their controller; only methods are
  `pub(super)`.
- External services (market factory, trading handle, account gate, persistence, clock) are injected through one
  `DesktopServices` value instead of process globals (`trading::handle()`, `DesktopAccount::shared()`,
  `latest_feedback()`). Existing seams are reused (the `MarketDataWorker` trait already used by
  `FixtureMarketWorker`); no new traits are introduced where a real in-memory implementation can be used in tests.

### 1.2 File layout (apps/desktop/src)

```
main.rs, lib.rs                      unchanged entry points (lib pub mods: account, command_registry, market_worker -> see 1.5, trading)
desktop.rs                           module declarations + run(); no struct definitions
desktop/
  app/
    terminal_app.rs                  TerminalApp: services, tabs, scheduler, overlays, notices (target <= 15 fields, no bool clusters)
    services.rs                      DesktopServices { market: WorkspaceMarketFactory, trading: TradingHandle,
                                       account: AccountGate, persistence: WorkspaceDocumentClient, clock: Clock }
    frame_scheduler.rs               FrameScheduler, DirtySet, AppSource (section 3)
    tab_strip.rs                     tab model + reorder/close/rename (from workspace_tabs.rs)
    symbol_entry.rs                  SymbolEntry (search box, results, keyboard state) from workspace_tabs/desktop.rs
    overlay_stack.rs                 OverlayStack: one enum of open overlays (menus, dialogs, pickers) replacing
                                     per-overlay Option fields and bools such as swallow_pane_mouse_up
    chart_menus.rs                   chart context menus (from chart_context_menus*.rs), stateless renderers + MenuTarget
    notices.rs                       NotificationCenter, UserNotice (section 5)
    lifecycle.rs                     unchanged location (naming_check asserts on it)
  pane/
    workspace_surface.rs             WorkspaceSurface: composition + render only; owns the controllers below
    chart_pane.rs                    ChartPaneController, ChartBinding, BindingPhase, ChartTarget (section 2)
    presentation.rs                  PanePresentation: every user-chosen chart property, applied in one place
    order_flow.rs                    OrderFlowPresenter: tape ring, order book, time-and-sales projection
    studies.rs                       StudiesController: study specs, outputs, errors
    alerts.rs                        AlertsController: price alerts, pending alert commands
    chrome_state.rs                  PaneChromeState: header, legend toggles, panel visibility, side panels (enum, not bools)
    context_panel.rs                 ContextPanelController (context snapshot poll today)
    persistence.rs                   PanePersistence: the only mapping between pane controllers and WorkspacePane proto
  trading_store.rs                   TradingStore entity (1.4)
  persistence/
    document.rs                      WorkspaceDocument (in-memory owner of the full WorkspaceState), RestoreOutcome
    migrations.rs                    ordered migration chain (section 4)
    writer.rs                        background writer thread (from workspace_persistence.rs)
    local_state.rs                   path resolution + readiness load (kept file name; naming_check asserts on it)
  test_support/                      #[cfg(any(test, feature = "diagnostics"))]: FixtureMarketWorker, PaneHarness,
                                     fixture builders (section 8)
account/                             dormant account integration, isolated (1.6)
chart_chrome.rs                      ChartChromePreferences type and parse/encode kept for migration only;
                                     its file I/O and save thread are deleted (section 4)
```

`engine_market_worker/` keeps its role as the bridge to `market_runtime` but shrinks after M2/M4: the slots, the
2 ms loop, `selection_commands.rs` two-step selection and `replay_conversion.rs` provider parsing are deleted.
`naming_check` tests that name `selection_commands.rs` strings (`desktop_startup_catalog_resolution_stays_event_driven`)
move to the runtime-side assertion in the same commit that deletes the desktop code.

### 1.3 WorkspaceSurface split (field ownership)

| Today on `WorkspaceSurface` / `desktop.rs` | New owner |
| --- | --- |
| `chart`, `product`, `interval`, `rithmic_switch`, `rithmic_pending_interval`, `rithmic_pending_product`, `rithmic_pending_sequence`, `rithmic_previous_selection`, `chart_state`, `chart_state_message` | `ChartPaneController` (`ChartBinding`) |
| ~15 properties reapplied in `install_snapshot_chart` (`workspace_surface.rs:2983-3053`) and the restore setters (`1760-1786`) | `PanePresentation` |
| `trade_tape`, `trade_sweeps`, tape generation, order book state, time-and-sales | `OrderFlowPresenter` |
| study specs, study outputs, `indicator_message` | `StudiesController` (errors become notices) |
| price alerts, `price_alert_message`, pending alert sends | `AlertsController` |
| header state, legend flags, panel visibility bools | `PaneChromeState` (enums) |
| `TradingPnlState` (`desktop.rs:914-934`) | deleted; panes read `TradingStore` projections |
| `symbol_message` | `UserNotice` keyed to the pane |
| pending fields filled by `observe_chart` (`desktop.rs:3087-3187`) | deleted; `ChartHostEvent` subscription (section 7) |

`WorkspaceSurface::new` loses `#[allow(clippy::too_many_lines)]`; `TerminalApp` loses
`#[allow(clippy::struct_excessive_bools)]`. Both removals are acceptance criteria for the split commit.

### 1.4 TradingStore (UI-08)

```rust
pub(crate) struct TradingStore {
    revision: TradingRevision,
    snapshot: Arc<TradingSnapshot>,          // shared, never cloned per pane
    selected_account: Option<AccountId>,      // workspace-level, persisted in the workspace document
    in_flight: BTreeMap<RequestId, PendingCommand>, // bounded by MAX_IN_FLIGHT_COMMANDS; overflow rejects the command with a notice
}
pub(crate) enum TradingStoreEvent {
    Changed { revision: TradingRevision },
    Fill(FillNotice),
    CommandResult { request: RequestId, outcome: CommandOutcome },
    RiskWarning { kind: RiskKind, detail: String },
}
impl EventEmitter<TradingStoreEvent> for TradingStore {}
```

- Fed by T1 through `AppSource::Trading` in the frame scheduler. When the source is dirty the store pulls
  `changes_since(revision)`; if the runtime reports a gap it takes one `snapshot()`. Idle produces no work.
- Panes call `store.pane_view(instrument) -> PaneTradingView<'_>` (positions, working orders, PnL for the pane's
  bound instrument). Panes subscribe to `Changed` and re-render only when their instrument's projection changed.
- Fill toasts come from `TradingStoreEvent::Fill` instead of `announce_fills` diffing snapshots in the toaster.
- Command feedback (T2) is returned per `RequestId` and shown in the order ticket or as a notice. Provider-named
  text such as the broker close-request notice (`trading.rs`) is built from `ProviderDescriptor::display_name`.
- Deleted: `keep_trading_pnl_fresh` / `refresh_trading_pnl` (`components/workspace_layout.rs:699-798`) and their
  render-path calls (`workspace_layout.rs:416`, `terminal_view.rs:882`), the 250 ms timer, `latest_feedback()`
  (`trading.rs:14-111`), `TradingPnlState`, the `let _ = terminal.update(...)` discards.
- Open question for the trading designer and maintainer: per-pane account selection exists today only as a copy in
  `TradingPnlState`. This design makes it workspace-level. If per-pane account selection is a product feature,
  it becomes a field of `PanePresentation` and the store exposes `pane_view(account, instrument)`.

### 1.5 FixtureMarketWorker out of production (UI-06 part)

`FixtureMarketWorker` (`market_worker.rs:1570`, `pub`) moves to `desktop/test_support/fixture_market_worker.rs`
under `#[cfg(any(test, feature = "diagnostics"))]`. Its only non-test user, `readiness_conformance.rs`, is already
under the same cfg. Acceptance: `rg FixtureMarketWorker apps/desktop/src` matches only test or diagnostics code; a
`naming_check` assertion enforces that the identifier appears only under `test_support/` and test modules.

### 1.6 Dormant account code: kept, isolated, compile-checked, tested (UI-12)

Maintainer decision: keep it. Design:

- Move `account.rs` (2,007 lines) and the account branches in `workspace_tabs.rs` (around 1716-1855, 2885-2918,
  3120, 3155, 3253), `components/terminal_chrome.rs:272` and `components/platform_menu.rs` into
  `apps/desktop/src/account/` (`mod.rs`, `session.rs`, `gate.rs`, `panel.rs`, `menu.rs`).
- One boundary type, `AccountGate`, is the only thing the rest of the desktop sees:
  ```rust
  pub(crate) enum AccountPresence { Disabled, SignedOut, SignedIn(AccountSummary) }
  impl AccountGate {
      pub(crate) fn presence(&self) -> AccountPresence;
      pub(crate) fn begin_refresh_quiesce(&self) -> RefreshQuiesce;   // live in production today
      pub(crate) fn plan_policy(&self) -> PlanPolicy;                  // replaces current_plan_limits placeholder
  }
  ```
  `AUTH_BACKEND_CONFIGURED = cfg!(test)` stays inside `account/gate.rs`. In production `presence()` is
  `Disabled`, so menus and chrome render no account UI without each call site checking a flag.
- Not a cargo feature. Reason: `begin_refresh_quiesce` is used on live production paths (`lifecycle.rs:93`,
  `workspace_tabs.rs:1763`, `desktop.rs:346`), so the module must compile in the default build. Being always
  compiled also satisfies "compile-checked" without a second CI configuration.
- Remove: `let authenticated = true` in `schedule_market_frame`; `CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED` and
  `current_plan_limits` (replaced by `PlanPolicy::Unrestricted` returned by the gate while disabled); the account
  poll in the frame loop and in the 100 ms timer (the gate wakes `AppSource::Account` when it has something).
- `retain_account_refresh_quiesce_for_exit` (`desktop.rs:302-341`) must not park a thread forever on failure: it
  returns an error that `lifecycle.rs` reports and exits with.
- Tests: the existing account tests keep running under `cfg(test)` with an installed test session. Add one test
  that the production gate is `Disabled`, opens no browser and issues no network request, and that startup reaches
  the workspace.
- `naming_check` updates in the same commit: `development_build_has_no_cloud_publisher_or_login_gate` points at
  `account/gate.rs`; `phase_five_account_surface_remains_bounded` allows account identifiers only in `account/`,
  `desktop/app/services.rs` and `trading.rs`.

---

## 2. Per-chart ChartBinding (replaces RithmicSwitchState, UI-02, UI-04)

### 2.1 Types

```rust
pub(crate) struct ChartTarget { instrument: InstrumentRef, interval: Interval }

pub(crate) struct ChartBinding {
    generation: BindingGeneration,   // from M1; monotonic per pane
    target: ChartTarget,
    descriptor: Option<InstrumentDescriptor>, // from M2 reply; drives labels, logo, tick, entitlement display
    phase: BindingPhase,
}

enum BindingPhase {
    /// Command sent. The previous chart stays visible; covered only if the instrument changed.
    Requested { previous: Option<LiveChart> },
    /// Snapshot received and applied; waiting for the runtime's ready state for this generation.
    Installing { previous: Option<LiveChart> },
    Live,
    /// Selection failed. The previous binding is restored (2.4) and the error is a notice.
    Failed { error: BindingError },
}

struct LiveChart { generation: BindingGeneration, target: ChartTarget }

pub(crate) struct ChartPaneController {
    binding: ChartBinding,
    chart: Entity<AerisChartView>,
    presentation: PanePresentation,
    retired_drops: u64,          // diagnostics counter
}
```

### 2.2 The single receive point

```rust
impl ChartPaneController {
    pub(super) fn accept(&mut self, envelope: PanePublication) -> Option<AcceptedPublication> {
        if envelope.generation != self.binding.generation {
            self.retired_drops += 1;
            return None;                      // the only place a retired generation is discarded
        }
        Some(AcceptedPublication { target: &self.binding.target, kind: envelope.kind })
    }
}
```

Every pane publication (state, snapshot, update, tape, order book, study output, recovery, viewport) goes through
`accept`. The other pane controllers receive only `AcceptedPublication`, so they cannot see a retired generation.
This deletes `RithmicSwitchState` (`desktop.rs:1672-1718`), `ready_state_can_complete_switch`,
`switch_requires_chart_cover` (kept as a pure `ChartTarget::needs_cover(&previous)`),
`stale_settled_state_during_engine_switch` (`workspace_surface.rs:3830-3905`), `awaits_replacement_chart`,
`apply_rithmic_switch_marker` (`workspace_surface.rs:3950`), `MarketWorkerMessage::EngineSwitchMarker`, and the four
`rithmic_pending_*` / `rithmic_previous_selection` fields. The swapping guard in `apply_publication`
(`3080-3169`) becomes phase-driven.

### 2.3 Transitions

| Event (already generation-checked) | From | To | Effect |
| --- | --- | --- | --- |
| user selects symbol / interval / installed instrument | any | `Requested` | `generation = next()`, send one M2 command, keep chart, cover if `needs_cover` |
| `Selected { descriptor }` | `Requested` | `Requested` | store descriptor; no second command (deletes the `SelectionInstalled -> try_select_engine` round trip) |
| snapshot | `Requested` | `Installing` | reset chart data (2.5), `presentation.apply(chart)`, project tape using `binding.target` |
| ready state | `Installing` | `Live` | drop cover, drop `previous` |
| `Failed { error }` | `Requested`/`Installing` | `Failed` then restore | notice + 2.4 |
| recovery failure | `Live` | `Live` | chart condition notice (`UntilCleared`), no phase change |

Trade-tape projection (`apply_trade_tape_to_chart`, `workspace_surface.rs:4619-4709`) uses `AcceptedPublication.target`
instead of the surface's `product`/`interval`, which removes the mismatch described in UI-02.

### 2.4 Failure and restore

`restore_rithmic_selection_after_failure` (`workspace_surface.rs:4002`) is replaced by `ChartPaneController::restore`:

- If M2 is transactional (the old demand is released only after the new one is installed), restore adopts
  `previous` as the current binding and its generation, with no new command.
- If M2 releases the old demand first, restore issues a new `select_and_demand` for `previous.target` with a new
  generation.

Which one applies is an open question for the market designer (section 10). The pane code is the same either way
apart from that one branch.

### 2.5 Chart entity lifetime and PanePresentation

Repeat-fix chain A in `05_cross_cutting.md` comes from replacing the chart entity on each switch and copying state
across field by field. Preferred design: keep one `AerisChartView` per pane for the pane's lifetime and reset its
data on a new snapshot. Whether Aeris Charts supports a full data reset on an existing chart without losing
engine-owned state (drawings and viewport policy) must be checked against the pinned revision; if it does not, that
is a two-repository addition (section 7).

`PanePresentation` is needed either way. It holds every user-chosen chart property (series style, colors, scale mode,
legend/crosshair options, order-flow options, study visibility, session settings and the rest currently in
`install_snapshot_chart` and `apply_restored_chart_state`). It has one `apply(&self, chart, cx) -> Result<(), PresentationError>`
used by both the restore path and the replacement/reset path. Setter errors are collected and reported as one
pane notice instead of `let _ =` (`workspace_surface.rs:1760-1786`). `PanePersistence` serializes it.

### 2.6 Startup

Startup resolution (`StartupResolution`, `handle_startup_catalog_event`, `StartupInstrumentResolved`) leaves the
desktop. Each restored pane sends one M2 command with its persisted `InstrumentRef`; a pane with no instrument
(new default workspace, see 6.3) sends `DefaultInstrument`. The pane shows the `Requested` state until the reply.

---

## 3. Frame scheduling (UI-16, UI-05 desktop side)

### 3.1 Today

`schedule_market_frame` (`workspace_tabs.rs:3147-3214`) runs on every `on_next_frame`: account poll, persistence
receipts, market summaries, screener, every pane's `poll_market_worker` (context snapshot poll, up to 64 messages,
recovery/search/studies dispatch), `persist_workspace_layout_if_changed` (full structural layout compare), and UI
diagnostics. `start_market_wake_listener` also runs a 100 ms timer that polls the updater and the account.

### 3.2 Design

```rust
pub(crate) enum AppSource { Summaries, Screener, Trading, Notices, PersistenceReceipts, Account, Updater, Diagnostics }

pub(crate) struct DirtySet {
    app: AtomicU32,                      // one bit per AppSource
    panes: Mutex<BTreeSet<PaneId>>,      // bounded by the number of open panes
    wake: UiWake,                        // existing Arc<AtomicBool + Waker> (desktop.rs:381-436)
}
impl DirtySet {
    pub(crate) fn mark(&self, source: AppSource);   // callable from any thread; sets bit, then UiWake::wake
    pub(crate) fn mark_pane(&self, pane: PaneId);
    fn take(&self) -> DirtyWork;                    // UI thread only
}

pub(crate) struct FrameScheduler { dirty: Arc<DirtySet>, frame_requested: bool, layout_revision: u64, persisted_revision: u64 }
```

- Producers get a `DirtyHandle { set: Arc<DirtySet>, source }` at construction: each pane mailbox, the summary and
  screener workers, the trading subscription (T1), the persistence writer (receipts), the account gate, the notice
  center. Nothing polls a source that has not marked itself.
- The UiWake listener requests at most one frame (`frame_requested` dedupes). In the frame the scheduler calls
  `take()` and drains only those sources, each with its existing budget (`MARKET_MESSAGES_PER_FRAME = 64` per pane).
  A source that still has work after its budget re-marks itself, which requests the next frame. This keeps the
  current bound and makes overload explicit rather than silent.
- Context snapshots, recovery dispatch, search results and study outputs are delivered through the pane mailbox, so
  they wake the pane like any other publication instead of being polled every frame.
- Layout persistence is driven by a revision counter. Controllers report `Changed::Layout`; the owning entity calls
  `scheduler.layout_changed()`. At the end of a frame, if `layout_revision != persisted_revision`, the scheduler asks
  `PanePersistence` for the document section and sends it to the writer. The per-frame structural compare in
  `persist_workspace_layout_if_changed` (`workspace_tabs.rs:2268`) is deleted.
- The 100 ms timer is deleted. The updater registers a maintenance timer only when automatic update checks are
  enabled (they are disabled in development builds), with its own interval, and marks `AppSource::Updater`. The
  account no longer needs polling (1.6).
- UI diagnostics run only under `feature = "diagnostics"` and only on frames that did work.
- `FramePollGate` (`frame_poll_gate.rs`) is subsumed by `frame_requested` and deleted, or kept as the dedupe primitive
  if its tests cover a case not covered above (check during the commit).
- Toast motion and other animations keep requesting their own frames; they are presentation, not sources.

Acceptance (measured, not assumed): with N idle panes, a `diagnostics` counter shows zero drains and zero frames
per idle second, and a `#[gpui::test]` asserts that `run_until_parked` after setup schedules no further frame.
Record before/after idle CPU for 1, 4 and 16 panes; do not claim an improvement without the numbers.

---

## 4. Versioned persistence (UI-09)

### 4.1 DurableFile in platform_runtime

`platform_runtime` already has `replace_file_atomically` (`atomic_file.rs:7`, `MoveFileExW` with
`REPLACE_EXISTING | WRITE_THROUGH` on Windows) and depends on `sha2`. Add one helper there:

```rust
pub struct DurableFile { path: PathBuf, magic: [u8; 8] }
pub struct DurableRead { pub schema_version: u32, pub payload: Vec<u8>, pub source: DurableSource }
pub enum DurableSource { Primary, Staged, Backup }
pub enum DurableReadError { Missing, Corrupt { path: PathBuf, reason: CorruptReason }, Io(io::Error) }

impl DurableFile {
    pub fn read(&self, max_len: u64) -> Result<DurableRead, DurableReadError>;
    pub fn write(&self, schema_version: u32, payload: &[u8]) -> io::Result<()>;
    pub fn quarantine(&self, corrupt: &Path) -> io::Result<PathBuf>;   // keeps the last 3 corrupt copies
}
```

Format: `magic (8) | envelope_version u16 | schema_version u32 | payload_len u64 | sha256 (32) | payload`.

- `write`: write `path.tmp`, fsync, verify by re-reading the header, rotate the current primary to `path.bak` only if
  the primary verifies, then `replace_file_atomically(tmp, path)`, then fsync the directory. The backup is the
  last-good copy and is not deleted after a successful write (today it is).
- `read`: try primary, then a staged `.tmp` that verifies and is newer (a crash between fsync and rename), then
  `.bak`. A corrupt primary is quarantined, not overwritten.
- The 12 MB bound from `local_state.rs` becomes `max_len`.
- `chart_chrome.rs:762-830` and `local_state.rs:175-213` atomic-write copies are deleted. `trading_runtime`'s own
  `write_atomic` can adopt `DurableFile` later; that is the trading designer's call and not part of this plan.

### 4.2 One workspace document, owned by the writer

- `WorkspaceDocument` holds the full `WorkspaceState` proto (`crates/contracts/src/messages.rs:162`). The writer
  thread owns the authoritative copy. The UI sends whole sections (`Layout`, `ChartChrome`, `SelectedAccount`) tagged
  with the layout revision; the writer replaces the section and debounces (100 ms, as today). The read-modify-write
  in `workspace_persistence.rs:278` (`load_workspace` before each save) is deleted.
- Chart chrome preferences move into the same document as a new `WorkspaceState` field (next free tag after 20).
  This deletes the second save thread, its generation bookkeeping, and `ChartChromeShutdownWait`
  (`chart_chrome.rs:467-760`). The key=value parser stays only as a migration step.
- Shutdown: one `shutdown_wait()` that blocks on a condvar with a deadline instead of the 5 ms sleep loop. Keep the
  call `await_workspace_persistence(persistence.shutdown_wait())` in `workspace_tabs.rs` (or update the
  `local_clients_versions_and_workspace_writes_remain_fenced` assertion in the same commit).

### 4.3 Versions and migrations

- Authority is the envelope `schema_version`. `WorkspaceState.schema_revision` (tag 8) is written with the same
  value for diagnostics. `CURRENT_SCHEMA_VERSION = 2` at introduction.
- `migrations.rs` is an ordered chain of pure functions, each with a golden-fixture test:
  - step 0: legacy `workspace-state.frame` (protocol v21 envelope, `LegacyWorkspaceEnvelope`) to schema 1;
  - step 1 to 2: today's heuristics move here and only here: `LEGACY_SERIES_DEFAULT_COLORS`
    (`workspace_surface.rs:524`), the area-bottom inference from `5391ce2d`, the `legacy_*_contract` study
    conversions in `local_state.rs`, and folding the `desktop/chart-chrome` key=value file into the document;
  - later steps append; a step never edits an earlier one.
- Detection: magic present means envelope; otherwise a raw `WorkspaceState` proto is schema 1 (or 0 when
  `schema_revision` is unset); otherwise the legacy `.frame` file. The pre-migration file is kept once as
  `workspace-state.pb.pre-v2` so a downgrade or a bad migration can be recovered by hand.
- `sanitize_workspace` (generation rebasing) stays as the last step after migration.
- `load_workspace_for_readiness` uses the same reader in dry-run mode (migrates in memory, never writes).
- `naming_check` `workspace_persistence_has_no_general_transport_layer` asserts on `WorkspaceState::decode`,
  `encode_to_vec`, `LegacyWorkspaceEnvelope` in `local_state.rs`; update the asserted file to
  `persistence/migrations.rs` / `persistence/document.rs` in the same commit.

### 4.4 Restore outcomes are visible

```rust
pub(crate) enum RestoreOutcome {
    Restored,
    Migrated { from: u32 },
    RestoredFromBackup { quarantined: PathBuf },
    NewerVersion { found: u32 },           // written by a newer build
    Defaulted { reason: RestoreFailure },  // nothing readable
}
```

- `RestoredFromBackup` and `Defaulted` post a persistent `UserNotice` with an "Open folder" action pointing at the
  quarantined copy. The user's layout is never silently replaced.
- `NewerVersion`: open the workspace from defaults in memory, disable writes for the session, and show a persistent
  notice that changes will not be saved. Never overwrite a document whose version is unknown.

---

## 5. Error, notification and typed-event model (UI-10, UI-11)

### 5.1 UserNotice

```rust
pub(crate) struct UserNotice {
    pub key: NoticeKey,                  // dedupe and replace-in-place (e.g. Pane(id) + ChartCondition)
    pub severity: Severity,              // Info, Warning, Error
    pub tone: NoticeTone,                // Buy, Sell, Warning, Loss, Muted (maps to ToastTone)
    pub source: NoticeSource,            // Pane(PaneId), Workspace, Trading(RequestId), Persistence, Provider(ProviderId)
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub action: Option<NoticeAction>,    // OpenFolder(PathBuf), Retry(RetryTarget), ConnectProvider(ProviderId)
    pub lifetime: NoticeLifetime,        // Timed(Duration), UntilCleared, UntilDismissed
}
```

- `NotificationCenter` is a workspace-level entity with `post(notice)` and `clear(key)`. It holds a bounded queue
  (oldest timed notices evicted first; `UntilCleared` notices are bounded by their keys). It marks
  `AppSource::Notices`.
- The toaster (`components/toaster.rs`) renders notices. Its existing semantics map directly:
  `show_chart_status` is the `UntilCleared` case keyed per pane; `push_fill` and `push_price_alert` are `Timed`
  notices from `TradingStoreEvent::Fill` and `AlertsController`. `ToastKind` becomes derived from `NoticeKey`.
  The centred chart notice (`ChartNoticePlacement::Center`) remains a pane render of the current chart condition.
- The per-pane strings `indicator_message`, `symbol_message`, `price_alert_message`, `chart_state_message` are deleted.
- Every notice is also written to diagnostics. Tokens and credentials never enter notice text (AGENTS.md).

### 5.2 Discarded results policy

Each `let _ =` in production desktop code (101 counted) is resolved as one of:

1. user-visible failure: post a notice (restore setters, `try_set_order_book_visible` at `workspace_surface.rs:5126`,
   `process_pending_price_alerts`, `search_symbol_query` in `apply_connection_state`);
2. expected and harmless (send on a closed channel during shutdown): explicit `match`/`if let Err` with a one-line
   reason;
3. internal failure: log a diagnostic.

Dead parameters silenced with `let _ = provider;` / `selection_pending` (`desktop.rs:3206`, `3680`) are removed from
the signatures. Production `expect`/`unwrap` (`components/terminal_view.rs:1500`,
`engine_market_worker/publications.rs:428`, `command_registry.rs:613`, `desktop.rs:3763/5619/5625/5642`) return
errors. Add `[lints.clippy] let_underscore_must_use = "deny"` (and `let_underscore_future`) to
`apps/desktop/Cargo.toml` in the commit that brings the count to zero for must-use values.

### 5.3 Typed events instead of text

- `should_autoload_rithmic_catalog` (`desktop.rs:3236`, text match on `RITHMIC_CATALOG_READY_MESSAGE`,
  `engine_market_worker.rs:58`) is replaced by `ProviderEvent::CatalogReady { provider }` (M3). Whether the
  desktop autoloads anything on that event is decided by `ProviderDescriptor` capability, not by provider name.
- `starts_with("RISK WARNING")` (`desktop.rs:2017`) is replaced by `TradingEvent::RiskWarning` (T3) surfaced as
  `TradingStoreEvent::RiskWarning` and a `Warning` notice.
- `naming_check` rule `desktop_control_flow_does_not_match_display_text`: reject `starts_with("` and
  `.contains(` against `*_MESSAGE` constants in `apps/desktop/src` production code.

---

## 6. Provider neutrality (UI-03) and the naming_check rule

### 6.1 Removals and replacements

| Today | Replacement |
| --- | --- |
| `TerminalProvider` enum, `terminal_provider_id` literals, `FALLBACK_TERMINAL_PROVIDER = Hyperliquid` (`desktop.rs:1600-1661`) | opaque `ProviderId` + `ProviderDescriptor` list from M3 |
| `rithmic-test:{exch}:{sym}` entitlement and `provider: "rithmic"` (`workspace_surface.rs:4878-4890`) | `InstrumentDescriptor.entitlement` from the runtime |
| `replay_conversion.rs:209-344` id parsing and `validate_product_identity` literals | deleted; runtime descriptors |
| `engine_market_worker.rs:696,714,733` provider branches (`worker_identity`) | descriptor capabilities |
| `default_rithmic_contract_index` ("MNQ"), `DEFAULT_RITHMIC_LISTING_QUERY`, `default_product` | `DefaultInstrument` in M2; the catalog's default-contract policy |
| Per-provider connect actions (`ConnectRithmic` and the removed brokers' actions), `HostedBroker` | `ConnectProvider(ProviderId)`; behavior from `ProviderDescriptor.connect` (`HostedBrokerOAuth`, `VaultCredentials { form }`, `Public`) |
| `components/rithmic_login.rs` | `components/provider_login.rs`: a form rendered from the descriptor's login schema; it still writes only to the native vault |
| `rithmic_shell.rs` / `RithmicSymbolBrowser`, `FeedIdentity "rithmic"` | `provider_symbol_browser.rs` driven by the descriptor's symbol-browser capability |
| Provider-named close-request text in `trading.rs` flatten feedback | text built from `ProviderDescriptor::display_name` |
| `local_state` default workspace (Hyperliquid BTC) | default workspace with one pane and no instrument; the pane sends `DefaultInstrument` (2.6) |
| `assets.rs` `logo_for` provider mapping | `LogoKey` from descriptors; `assets.rs` maps keys to embedded files |

### 6.2 naming_check rule

New test in `tools/naming_check/src/main.rs`, using the existing `production_sources_under` and `production_prefix`
helpers (so `#[cfg(test)]` modules are excluded):

```text
desktop_source_is_provider_neutral
  scan: production_sources_under("apps/desktop/src") and ("crates/ui/chart_integration/src"),
        each cut at production_prefix, with // comments stripped
  reject (case-insensitive substring): "rithmic", "hyperliquid" (plus any other registered provider name)
  reject (literal): "-authorized\"", "-public\"", "rithmic-test:"
  allow-list: const PROVIDER_NAME_ALLOWLIST: &[(&str, &str)] = &[(path, reason)]
  the test also fails when an allow-list entry no longer matches anything (stale entries are removed)
```

Allow-list at the end of the migration:

- `apps/desktop/src/assets.rs`: embedded logo file names. Only file paths, keyed by `LogoKey`.

Interim allow-list entries, each removed by a named commit in section 9: `components/rithmic_login.rs` (D7c),
`rithmic_shell.rs` (D7c), `engine_market_worker/replay_conversion.rs` (D7a), `readiness_conformance.rs`
(diagnostics harness; iterate descriptors in D7b or keep allow-listed with reason "qualification of a gated
provider" if it must name one).

Existing `naming_check` tests touched by this section, in the same commits:

- `every_runtime_provider_is_mapped...` asserts `HIDDEN_RUNTIME_PROVIDERS` in `desktop/tests.rs`. Once the desktop
  has no provider mapping, its intent ("every runtime provider is presentable") becomes a `market_runtime` test that
  every registered provider has a complete `ProviderDescriptor`; the desktop assertion is deleted.
- `rithmic_logins_stay_in_the_native_vault` points at `components/provider_login.rs` and asserts the vault-only write
  path for every `VaultCredentials` provider.
- `desktop_rendering_remains_component_owned` component list updated for renamed components.

Tests may keep real provider ids as fixture data, but through one `test_support::providers` module so the
production rule stays simple.

---

## 7. Chart integration relocation (UI-14, UI-13 part)

| Item | Decision | Repository | Reason |
| --- | --- | --- | --- |
| `pending_context_menu`, `pending_activate`, `pending_settings_request`, `pending_study_remove` on `AerisChartView` (`view.rs:1055`), set in `view/input.rs` `process_input_events` from the engine's `take_input_events()` and drained by `observe_chart` (`desktop.rs:3087-3187`) | `impl EventEmitter<ChartHostEvent> for AerisChartView`; `process_input_events` calls `cx.emit`; `ChartPaneController` subscribes once at creation | local (`chart_integration`) | The engine already emits input events; only the host translation is polled. GPUI events are already used in `terminal_ui` `order_book_view`. No Aeris Charts change. |
| Hollow candles (`hollow_appearance`, `hollow_base`, transparent body in `view.rs`) | Aeris Charts series style (for example `CandleBody::Hollow`) | cross-repo: Aeris Charts first, push, then bump only the `aeris_charts_*` revisions | Series rendering is engine-owned; the pinned Aeris Charts has no hollow style. |
| `classify_order_flow_sweeps` (`view/order_flow.rs:588`, 90th-percentile size threshold over the last 512 trades, 100 ms window) | Sweep annotation computed in `market_runtime` once per trade at ingestion, published with the tape (M5) | market designer, same repository | The only consumer is the desktop time-and-sales highlight (`surface.trade_sweeps`), not the chart, and analytics on market data belong to the canonical tape owner. Moving it to Aeris Charts would put a non-chart consumer behind the chart. Preserve current semantics exactly and cover them with the existing classifier tests moved to the runtime. Display stays local (licensing checklist unchanged: derived value displayed to the subscriber). |
| Full tape re-sent each revision and diffed by ordinal in the UI | Incremental `Append`/`Reset` with generation (M5); `OrderFlowPresenter` keeps a bounded ring at the current cap | market designer; Aeris Charts only if needed | Aeris Charts exposes `replace_order_flow_window` (`footprint.rs:2582`, full replace). Measure the 100k-trade apply first; add an append operation to Aeris Charts only if the replace is measured as the bottleneck. |
| `SESSION_PLAN_LEVEL_COLOR = Color::rgb(245, 166, 35)` (`view.rs:65`), color picker swatches and default `#335CFF` (`native_ui/color_picker.rs:31,171`), `rgba` literal (`chart_context_menus.rs:2585`) | design_system tokens; `LEGACY_SERIES_DEFAULT_COLORS` moves into the migration step | local + website mirror (`aeris-website/src/styles/platform.css`) and `CSS.md` | AGENTS.md design-system coordination. The HSLA spectrum gradient in the picker stays. |

`naming_check` additions: `chart_integration_has_no_order_flow_analytics` (rejects `classify_order_flow_sweeps`,
`hollow_appearance`, `hollow_base`, `pending_context_menu`, `take_pending_`), and
`colors_come_from_tokens` (rejects `Color::rgb(`, `rgba(`, `rgb(` and `#` hex literals outside `design_system`,
the picker spectrum and `persistence/migrations.rs`).

Chart entity reset (2.5) may be a further cross-repo item if the pinned engine cannot reset data in place.

---

## 8. Test strategy (UI-15)

Infrastructure exists: `gpui` is a dev-dependency with `test-support`, and `#[gpui::test]` with `TestAppContext` /
`VisualTestContext` is already used in `desktop/tests.rs`, `workspace_surface_tests.rs`, `native_ui`,
`fullscreen_hint`, `platform_menu`, `toaster` and `terminal_ui`.

Layers:

1. Pure controller tests, no GPUI: `ChartBinding::accept` and transitions, `ChartTarget::needs_cover`,
   `PanePresentation` round trip through `PanePersistence`, migration steps, `DurableFile`, `DirtySet`,
   `NotificationCenter` dedupe and bounds, `TradingStore` change application.
2. Entity tests with `#[gpui::test]`: `PaneHarness` (in `test_support/`) builds a `WorkspaceSurface` with
   `FixtureMarketWorker`, an in-memory `trading_runtime` on the simulated venue, and a test `DesktopServices`.
   API: `harness.select(target)`, `harness.publish(generation, kind)`, `harness.rendered()` returning a projection
   (label, phase, cover, notices, tape rows), never private fields.
3. Runtime contract tests live with the runtimes (select-and-demand startup resolution, descriptors, sweeps,
   change stream); the desktop does not re-test them.

Required tests:

- Generation fencing: deterministic interleavings (all permutations of a small set plus seeded random orders; no new
  dependency) of state, snapshot, update and tape from generation N and N+1; assert that after the switch the pane
  renders only N+1 data and the chart data never contains N trades.
- Failure restore: N+1 fails; pane renders N's target and a notice; no second failure notice on retry.
- Interval-only switch keeps the chart uncovered; instrument switch covers until ready.
- Startup: restored pane sends exactly one command and applies one reply.
- TradingStore: one fill produces one store change, one fill notice and one notify per affected pane; an idle
  workspace produces zero trading notifies over a simulated minute.
- FrameScheduler: idle panes produce zero drains; a pane over budget re-marks itself and drains on the next frame;
  layout persistence is sent once per revision.
- Persistence: truncated primary restores backup and quarantines; staged `.tmp` after a simulated crash is used when
  valid; unknown future version refuses to write; each migration step against a sanitized golden fixture (legacy
  `.frame` v21, schema 1 raw proto, chart-chrome key=value); writer never reads the file while running.
- Notices: restore-setter failure posts one pane notice; risk warning arrives as a typed event.
- Account: production gate is `Disabled` and startup reaches the workspace without network; existing account tests
  keep running with the test gate.
- `naming_check`: each new rule has a positive and a negative unit test, like the existing rules.

Deleted with the code they cover: tests in `desktop/tests.rs` that reference `RithmicSwitchState`, `rithmic_switch`,
`should_autoload_rithmic_catalog`, `stale_settled_state_during_engine_switch`, `ready_state_can_complete_switch`
(20 references), the 206 lines from `f504ab86`, and per-guard tests in `workspace_surface_tests.rs`. Each deleted
behavior must map to one of the required tests above before deletion.

---

## 9. Ordered migration plan (commit-sized)

Each commit passes `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
`cargo test --workspace --all-features --locked`, and `naming_check`. Commits with persistence, provider lifecycle or
rendering effects also need a run of the real desktop path (AGENTS.md "How to work" step 6).

| # | Commit | Depends on | Closes | naming_check changes in the same commit |
| --- | --- | --- | --- | --- |
| D0 | `refactor(desktop): inject DesktopServices and add test_support harness`; move `FixtureMarketWorker` to `test_support` | none | UI-06 (fixture part) | new: `FixtureMarketWorker` only in test/diagnostics code |
| D1 | `refactor(desktop): unify user notices`; `NotificationCenter`, delete per-pane message strings, resolve `let _ =` in restore and alert paths, remove dead params and production `expect` | D0 | UI-11 (most) | none |
| D2 | `refactor(desktop): isolate dormant account integration behind AccountGate`; remove `authenticated`, plan placeholders, account polling, the parking thread | D0 | UI-12 | update `development_build_has_no_cloud_publisher_or_login_gate`, `phase_five_account_surface_remains_bounded` |
| D3 | `refactor(ui): tokenise remaining chart and picker colors` (with website mirror and `CSS.md`) | none | UI-13 | new `colors_come_from_tokens` (migration path allow-listed until D12) |
| D4 | `refactor(chart_integration): emit ChartHostEvent instead of pending requests`; delete `observe_chart` drains | none | UI-14 (requests) | new part of `chart_integration_has_no_order_flow_analytics` (pending names) |
| D5 | `refactor(desktop): ChartPaneController and generation-fenced ChartBinding`; `PanePresentation`; delete `RithmicSwitchState`, guards, `EngineSwitchMarker`, pending fields; rewrite switch tests | M1, D0, D1 | UI-02 | none expected; check `HIDDEN_RUNTIME_PROVIDERS` test still compiles |
| D6 | `refactor(desktop): single select_and_demand per selection`; delete two-step selection and startup resolution from `selection_commands.rs` | M2, D5 | UI-04 | replace `desktop_startup_catalog_resolution_stays_event_driven` with runtime-side assertion |
| D7a | `refactor(desktop): render runtime instrument descriptors`; delete `replay_conversion` parsing, entitlement building, `worker_identity` branches, `default_rithmic_contract_index` | M3, D6 | UI-03 (identity) | add `desktop_source_is_provider_neutral` with interim allow-list |
| D7b | `refactor(desktop): typed provider and risk events`; replace `TerminalProvider` with descriptors, delete `should_autoload_rithmic_catalog` and the risk prefix match; default workspace without instrument | M3, T3, D7a | UI-10, UI-03 (enum, defaults) | add `desktop_control_flow_does_not_match_display_text`; replace `every_runtime_provider_is_mapped...`; shrink allow-list |
| D7c | `refactor(desktop): descriptor-driven provider login and symbol browser`; `ConnectProvider(ProviderId)` | M3, D7b | UI-03 (remaining) | update `rithmic_logins_stay_in_the_native_vault`, `desktop_rendering_remains_component_owned`; allow-list reduced to `assets.rs` |
| D8 | `refactor(desktop): consume runtime publications through the waitable outbox`; delete slots, 2 ms loop, `MarketWorkerMessage` re-declarations, bespoke coalescing; replace `publications.rs:428` `expect` | M4, D5 | UI-05, UI-06 | none expected |
| D9 | `refactor(desktop): workspace TradingStore`; delete `TradingPnlState`, render-path poll, `latest_feedback`, snapshot-diff fill announcements | T1, T2, D1 | UI-08 | add assertion: no `service.snapshot()` call in `components/` |
| D9b | `fix(desktop): remove simulated venue relay`; delete `publish_simulated_market_observation` and desktop `register_provider_instrument` calls | T4 | UI-01 | add: `apps/desktop` does not reference `SimulatedMarketObservation` |
| D10 | `refactor(desktop): split pane and app controllers`; `OrderFlowPresenter`, `StudiesController`, `AlertsController`, `PaneChromeState`, `PanePersistence`, `ContextPanelController`, `SymbolEntry`, `OverlayStack`, `TabStrip`, `ChartMenus`; remove both lint allows | D5 to D9 | UI-07 | update file paths in `local_clients_versions_and_workspace_writes_remain_fenced` and `durable_migration_test_boundaries` if functions move |
| D11 | `refactor(desktop): dirty-source FrameScheduler`; delete the 100 ms timer and per-frame layout diff; record idle CPU numbers | D8, D9, D10 | UI-16 | none |
| D12 | `feat(platform_runtime): DurableFile with last-good backup and checksums`; adopt in workspace and chart chrome I/O without format change yet | none (can run any time) | UI-09 (duplication, backup) | none |
| D13 | `feat(desktop): versioned workspace document and migration chain`; writer owns the document, chart chrome folded in, `RestoreOutcome` notices, newer-version refusal | D1, D10, D12 | UI-09 (versioning, visibility) | update `workspace_persistence_has_no_general_transport_layer` file paths; remove migration allow-list duplication from D3 |
| D14 | Aeris Charts: hollow series style (separate repository commit and push), then `chore(charts): bump aeris_charts revisions for hollow candles` and delete host hollow transform | none for desktop; Aeris Charts work | UI-14 (hollow) | add hollow names to `chart_integration_has_no_order_flow_analytics` |
| D15 | `refactor(desktop): consume runtime sweeps and incremental tape`; delete `classify_order_flow_sweeps` from `chart_integration` | M5, D10 | UI-14 (sweeps, tape) | add `classify_order_flow_sweeps` to the same rule |
| D16 | `test(desktop): remove guard-specific tests and finish harness migration` (most deletion happens in D5 to D9; this closes the remainder) | D5 to D15 | UI-15 | none |

Parallelism: D0 to D4 and D12 have no external dependencies and can start now. D5 is the highest-leverage
commit and waits only on M1. D9b can land as soon as T4 lands.

---

## 10. Risks and open questions

1. M2 transactionality (2.4): does the runtime keep the old demand until the new selection succeeds? This decides
   whether restore is "adopt previous generation" or "reselect previous".
2. Chart entity lifetime (2.5): can the pinned Aeris Charts reset series data on an existing view without losing
   engine-owned drawings and viewport policy? If not, a cross-repo operation is needed to stop entity replacement;
   until then `PanePresentation` bounds the damage but chain A's root cause remains.
3. Incremental tape (M5) is a market-designer decision. If the runtime keeps full-window publications, D15 shrinks
   to the sweep move and the UI keeps ordinal diffing inside `OrderFlowPresenter`.
4. Sweep semantics: moving the classifier must preserve the 512-trade window and 100 ms grouping exactly; changing
   them is a product decision, not part of the move.
5. `DurableFile` ownership in `platform_runtime`: it is the existing home of `replace_file_atomically` and already
   depends on `sha2`. Confirm the maintainer wants persistence helpers there rather than in a new crate (this design
   avoids a new crate).
6. Folding chart chrome into the workspace document changes two files into one; the first run after upgrade writes
   the new envelope. Keep `workspace-state.pb.pre-v2` and the old chrome file untouched for one release; decide when
   to stop reading the legacy `.frame` file.
7. Default workspace without an instrument (6.1) depends on `DefaultInstrument` resolving without a gated provider
   login. If no public provider is available, the pane must show an empty "choose an instrument" state, and startup
   must still reach the workspace (AGENTS.md authentication rule).
8. Per-pane trading account selection (1.4): confirm whether it is a feature before making it workspace-level.
9. Account isolation as a module rather than a cargo feature (1.6): chosen because refresh quiesce is live in
   production. If the maintainer prefers a feature flag, the live quiesce path must first move out of `account/`.
10. Provider login as a descriptor-driven form (D7c) touches the vault boundary. It needs a manual check with real
    Rithmic credentials through the native vault, which only the maintainer can do; hosted-broker OAuth stays
    unverifiable until the AWS broker exists.
11. Measured claims: idle CPU (D11) and the 100k-trade tape apply (D15) must be recorded before and after. No
    performance improvement may be reported without those numbers.
12. Interleaving tests rely on M1 putting a generation on every publication kind, including viewport and recovery.
    Any untagged kind would reopen UI-02; the market design should make the generation a required field of the
    envelope type rather than of each variant.
