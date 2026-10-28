# Aeris Terminal stabilization tracker

This file tracks the platform stabilization work: what is done and what is left. Item specifications
live in `reports/work_items.md` (indexed by item id), the binding target design in `architecture.md`,
and the audit and design reports in `reports/`. Detailed per-item history, the old findings index and
the old progress log are in Git: `git show 34698887:plan/stabilization/stabilization_work.md`.

## 1. How to use this file

- **A box is checked only when the item is fully done**: its code has landed on `main` and every
  validation its spec requires has passed (tests, probes, benchmarks and guarded GUI checks with
  their VAL ids). Code complete with a check still to run is not done; its line says what is left.
- A part that a maintainer decision superseded, or that moved to another named item, does not block
  an item. The line says where it went.
- When an item's work lands, update its line in the same commit: shorten "Left" to what remains, or
  check the box and remove "Left" when nothing remains. Record a new rule in the repo `AGENTS.md`
  (section 5 here holds only the few rules that are not in `AGENTS.md`). Git history is the
  progress log; do not add one here.
- **Checks:** while working, run focused checks on the crates a change touches
  (`cargo check|clippy|test -p <crate> --locked`, Clippy with `--all-targets --all-features -- -D
  warnings`, plus `cargo run -p aeris_naming_check --locked` when an architecture rule or a scanned
  file changes). Run the full gate once per milestone, before the milestone's closeout commit:
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (again with
  `AERIS_RITHMIC_KIT_DISABLED=1` in a separate `CARGO_TARGET_DIR`),
  `cargo test --workspace --all-features --locked` and `cargo run -p aeris_naming_check --locked`.
  CI runs the full gate on every push to `main`; fix a red run before starting other work.
- Guarded GUI checks run only on an isolated validation root while no other Aeris desktop is running.
  If one cannot run, it stays open; never record it as passed.

## 2. Status

58 of 90 items are fully done (2026-10-28).

| Milestone | Done | Left |
|---|---|---|
| M1 foundations | 9 / 9 | 0 |
| M2 market foundations | 10 / 10 | 0 |
| M3 market lifecycle | 12 / 12 | 0 |
| M4 trading core | 9 / 10 | 1 (GUI check) |
| M5 trading engine | 7 / 12 | 5 (GUI checks and probe evidence) |
| M6 desktop bridge | 6 / 13 | 7 (code complete; GUI checks) |
| M7 desktop structure | 1 / 12 | 11 (GUI checks and remaining work) |
| M8 lifecycle and trim | 4 / 12 | 8 |

Every open M4-M6 item has its code landed and waits only on the checks listed on its line. Most are
guarded GUI checks; one guarded GUI pass on an isolated root, with no other Aeris desktop running,
can close several items together. The M6 and M7 full gates and CI closeouts are recorded; M8
follows.

## 3. Work items

### M1 foundations

- [x] M1.1 Stabilization tracking document and `AGENTS.md` rule (853a3916)
- [x] M1.2 Hosted CI on push and pull request (63a71463; green `windows` and `ubuntu` jobs, for
  example run 40603641392)
- [x] M1.3 One runtime-owned interval mapping; Day3 restore fixed (ab5ff341)
- [x] M1.4 Bounded exit; secret tokens redacted (3c42981b)
- [x] M1.5 `platform_sys` crate with the single durable writer (3c42981b)
- [x] M1.6 Every hand-rolled file writer replaced by the durable writer (3c42981b)
- [x] M1.7 Rotating log file, panic crash files and diagnostics sink (3c42981b)
- [x] M1.8 Per-data-root instance lock and validation mode (3c42981b)
- [x] M1.9 SQLite busy timeout, integrity check, quarantine and migrations (3c42981b)

### M2 market foundations

- [x] M2.1 Coordinator command/event characterization tests (20abd4cc)
- [x] M2.2 Hot-path and coordinator benchmark baseline (f831707e)
- [x] M2.3 Injected market clock (b2b66d8d)
- [x] M2.4 One jittered, capped, budgeted reconnect policy (a00725b0)
- [x] M2.5 Provider-neutral market calendar and session service (0eae9355)
- [x] M2.6 Provider-neutral stall watchdog (78085525)
- [x] M2.7 Session-day, weekly and monthly live bars follow the calendar (ecc749ab)
- [x] M2.8 Typed `Epoch<Tag>` and `Fenced<T>` identities (09e43c35)
- [x] M2.9 Command fences and one consumer registry (2fd43740)
- [x] M2.10 `HistoryTracker` with current and backfill lanes (1303adbd)

### M3 market lifecycle

- [x] M3.1 Per-series lifecycle state machine, candle path (0c4166cb)
- [x] M3.2 Series lifecycle, trade path; duplicate state machine deleted (8c61798d)
- [x] M3.3 Series-tail accessor; no full-series copies on hot paths (e52e72f8)
- [x] M3.4 Dirty-set publication and deadline timers (30a2d5d1)
- [x] M3.5 `ProviderCapabilities` and `ProviderPresentation` (2f3fd729)
- [x] M3.6 `ProviderSession` trait and `ProviderRegistry` (3d474290)
- [x] M3.7 Instrument leases and trading feed contract (896d3447)
- [x] M3.8 Market-side trading quote feed (db7a572c)
- [x] M3.9 Fenced consumer events and waitable outbox (866032c4)
- [x] M3.10 `select_and_demand` and runtime-published descriptors (c0f4dcfb)
- [x] M3.11 Deterministic market replay harness (0e77fa3c)
- [x] M3.12 Live Hyperliquid lifecycle probe (530a7c8d)

### M4 trading core

- [x] M4.1 Injected trading clock; cancel events at command time (6b248b2f)
- [x] M4.2 Owner thread around a synchronous `TradingCore` (98262ee5)
- [x] M4.3 Deterministic trading replay harness (52124c5a)
- [x] M4.4 Indexed order book (f34adbc0)
- [x] M4.5 Trading core module split (66e52622)
- [x] M4.6 Trading store split by table family (836c9573)
- [x] M4.7 Trading runtime owns its quotes; desktop relay removed (cd54c087)
- [ ] M4.8 Provider-neutral session and freshness gating (7f3e1ba5)
  - Left: guarded GUI check that an order on a closed market or an unknown session shows its typed
    reject reason (spec GUI clause). Closes MD-02 and TR-02.
- [x] M4.9 One fill-accounting implementation, V21 (20be05fd)
- [x] M4.10 Facts-only persistence; no durable write per quote (73b75850)

### M5 trading engine

- [x] M5.1 Complete order state machine, revisions and typed events (73ff4a92)
- [x] M5.2 One admission validator for simulated and broker routes (9b49f28e)
- [x] M5.3 Bid/ask fill rules, stop and stop-limit triggering (140a8b3d)
- [ ] M5.4 DAY expiry at session close or 00:00 UTC; GTC keeps working (05cc55bf)
  - Left: guarded GUI check that an expired DAY order shows the Expired status.
- [x] M5.5 One order-group model with bracket migration (064ca1e4)
- [ ] M5.6 Superseded protection layers deleted (71f36340)
  - Left: guarded GUI check VAL-TRD-053, placing a chart bracket.
- [ ] M5.7 Fill-driven cash ledger with configurable fees (2d8e32ab)
  - Left: the VAL-TRD-058 minimum-quantity GUI clause. The ticket accepts whole contracts only, so
    0.00001 BTC cannot be entered. Fractional ticket quantity input is needed first, and no item
    owns it yet.
- [x] M5.8 Checkpoints before retention, repair and equity history (11085c44)
- [x] M5.9 Kill switch, flatten scopes and atomic reverse (f824e27b)
- [ ] M5.10 Persisted risk session with rollover (f81bcb53)
  - Left: guarded GUI check that a bracket order no longer produces a risk warning.
- [x] M5.11 Change-driven, bounded trading publication with paged history (1cecfa4b)
- [ ] M5.12 Live paper-trading probe on Hyperliquid (a0f2b730)
  - Left: archived clean-HEAD runs of the no-fees case and both negative cases (no quote,
    disconnect). Only the positive run is recorded (22/22 at the M6.11 head). The broker demo probe
    is superseded; its broker was removed.

### M6 desktop bridge

- [x] M6.0 Fresh-workspace default chart draws, VAL-DSK-120 (f467f7a7)
- [x] M6.1 Inject `DesktopServices`; fixture worker in test support only (0db7e973)
- [x] M6.2 One `UserNotice` model and `NotificationCenter` (6987ecbe)
- [ ] M6.3 Per-chart `ChartPaneController` drops stale publications (0947e8b0)
  - Left: guarded GUI check VAL-DSK-014. A six-interval storm (1m, 1h, 1D, 3D, 5m, 3D, each within
    1 s) ends with the interval button, legend and axis on 3D within 10 s. VAL-DSK-013 and 015
    passed. Closes UI-02 and MD-15.
- [x] M6.4 `PanePresentation` applies every chart property in one place (33a1af51)
- [ ] M6.5 One `select_and_demand` per selection (973a4fe8)
  - Left: guarded GUI checks VAL-DSK-020 and VAL-CROSS-002.
    - VAL-DSK-020: a four-pane restart is captured within 15 s, with each pane's saved pair
      confirmed.
    - VAL-CROSS-002: both tabs are all live within 30 s on one Hyperliquid session, and ETH is
      chosen within 5 s.
    - UI-04's picker part closes with them.
- [x] M6.6 Desktop bridge message model collapsed (5a6da58c)
- [ ] M6.7 Runtime descriptors drive provider identity (42d99d1e)
  - Left: guarded GUI check VAL-DSK-114. On a fresh root, the symbol button and the legend show
    BTC-USDC and the descriptor's provider name, and both follow a switch to ETH-USDC within 5 s.
    Closes MD-10.
- [ ] M6.8 Typed provider and risk events; default workspace (d1218cae)
  - Left: guarded GUI checks for the VAL-DSK-026 GUI clause and VAL-CROSS-001.
    - VAL-DSK-026: a fresh root shows one tab and one pane on BTC-USDC with live candles within
      15 s.
    - VAL-CROSS-001: a fresh first run reaches a live default chart with no broker venue session.
- [ ] M6.9 Descriptor-driven provider connect, login and symbol menu (4ef50ef0)
  - Left: guarded GUI check VAL-DSK-029.
    - The connect list shows one row per published descriptor, and the vault login form renders.
    - Cancel makes no sign-in attempt and writes no vault entry.
    - The symbol menu lists only symbol-browser descriptors.
    - UI-03 closes with this item and M6.7.
- [ ] M6.10 One workspace `TradingStore` fed by the change stream (bf626069)
  - Left: guarded GUI checks VAL-DSK-034 and VAL-DSK-035.
    - VAL-DSK-034: one Buy gives one fill toast and a position row within 3 s, with no second toast.
    - VAL-DSK-035: a 10,001-order fixture shows trade history within 10 s.
- [ ] M6.11 Revisioned chart intents; snapshots confined to tests (a36f225f)
  - Left: guarded GUI checks VAL-DSK-037, VAL-CROSS-015 and VAL-CROSS-016.
    - VAL-DSK-037: flatten works with the order book off.
    - VAL-CROSS-015: flatten-all closes BTC and ETH, including the instrument with no chart.
    - VAL-CROSS-016: a switch storm ends on SOL with order lines only on SOL.
    - Also record VAL-DSK-110 as passed; its tests exist.
    - Closes TR-10 and UI-10.
- [x] M6.12 No unhandled discards in desktop production code; deny lints (34698887)

M6 closeout: the full gate (fmt, workspace clippy, workspace tests) passed locally on 1b85770e, and
CI run 40624566453 passed on 93e0bd21, which contains all M6 code. The open M6 items still wait
only on the guarded GUI checks on their lines.

### M7 desktop structure

- [ ] M7.1 Chart and picker colors become design tokens
  - Landed: the color-choice tokens (`--swatch-*`, `--swatch-default`, `--session-plan-level`)
    in `CSS.md`, `platform.css` and the Rust registry, and the build rejects them outside `:root`.
    The session-plan level, picker swatches and default, and the watchlist flag default read
    them, and the context-menu hex parser reuses the picker's. naming_check
    `colors_come_from_tokens` allowlists only the picker spectrum and the legacy series fallbacks.
    The website mirror is committed locally (aeris-website c759b71) after its typecheck, lint and
    build passed; per M-D8 it waits for the maintainer to push it.
  - Left: guarded GUI screenshot check that the session-plan level, the picker swatches and the
    watchlist flag look unchanged. Closes UI-13.
- [ ] M7.2 Account integration behind an account boundary
  - Landed: `apps/desktop/src/account/` with `plan_policy()` (unrestricted) replacing
    `CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED` and `current_plan_limits`; the 8-tab and 64-pane
    bounds stay as resource bounds. The account poller paces its own status fetches and runs
    weakly held change wakes, replacing the frame-loop, 100 ms timer and sign-in-window polls.
    Account tests cover the paced fetch, the wake bound and unregistering; naming_check rejects the
    removed polls and placeholders. The dormant-code parts are superseded by mandatory sign-in.
  - Left: guarded GUI check that sign-in, sign-out and the Manage Profile refresh still update
    the sign-in window and the account menu.
- [ ] M7.3 `AerisChartView` emits host events
  - Landed: `AerisChartView` is an `EventEmitter<ChartHostEvent>` (activate, context menu,
    settings, study removal); the four `pending_*` fields, `ActivationRequest` and the
    `take_*_request` drains are deleted, and the workspace surface subscribes per chart. Host
    events, the Y-axis menu and the legend moved out of `view.rs` into `view/host_events.rs`,
    `view/price_axis_menu.rs` and `view/legend.rs`. A gpui test shows one context-menu input
    emits exactly one host event; naming_check rejects the pending names in chart_integration and
    the `take_*` polls in the desktop. Part of UI-07 and UI-14.
  - Left: guarded GUI check that the context menu, settings and study removal still work.
- [ ] M7.4 `WorkspaceSurface` split into pane controllers
  - Landed: `desktop/context_panel_controller.rs` owns the context snapshot, panel visibility
    (an enum), tab, height, API-key dialog and risk message behind change-reporting setters.
    `desktop/alerts_controller.rs` owns the alert list, create dialog, restore-failure notice and
    the 8-entry announcement ring; the surface only relays accepted lists to the market worker.
    `desktop/studies_controller.rs` owns the active, pending and deferred runtime studies, their
    reinitializations, removals, queued settings and retry pacing behind private fields.
    `desktop/order_flow_presenter.rs` owns the accepted trade tape and its ordering, the sweep
    highlights, the Time & Sales filter and rows, the chart trade projection and the order-book
    demand the market worker accepted behind private fields.
    `desktop/pane_chrome_state.rs` owns the open header overlay (one enum with its phase,
    trigger and list navigation), the selected row, the timeframe flyout and its hover close,
    trigger anchors, the Accounts section and placement, the quick-timeframe rejection, the
    scope menu and the docked side-panel layout behind private fields, with enums instead of
    the former menu bools.
    `desktop/pane_persistence.rs` owns the saved chart document and viewport a restore applies,
    the chart's observed user-state revision, the viewport last reported to the market worker
    and the dirty flag behind private fields; the surface's constructor no longer needs the
    `too_many_lines` allow.
  - Left: GUI smoke test of studies, alerts, the order book and time and sales. Part of UI-07.
- [ ] M7.5 `TerminalApp` split into app controllers
  - Landed: `desktop/tab_strip.rs` owns the open tabs in order, the selected tab and the tab
    drag behind private fields; selecting, reordering, adding and closing keep the selection on
    the same tab and report what changed, and `TerminalApp` drops from 68 to 66 fields.
    `desktop/symbol_entry.rs` owns each pane's search field, catalog browser, menu target and
    the pending typed-symbol, product-opening and watchlist-install state behind private fields.
    `desktop/overlay_stack.rs` holds the one open app overlay (profile menu, page menu,
    keyboard shortcuts or command palette, with the palette's highlight and message) in one
    enum slot; opening one replaces the other. `desktop/chart_menus.rs` owns the chart context
    menu, the copy-feedback fence and the settings panel with its placement, section and one
    popover enum (dropdown, color picker, template menu or dialog) behind private fields.
    `TerminalApp` drops from 66 to 52 fields.
    `desktop/window_state.rs` owns the chrome focus, window activity, closing phase, profile
    refresh, move gesture, title bar, fullscreen hint and chart fullscreen;
    `desktop/frame_wakes.rs` the market wake, frame poll gate and listener start;
    `desktop/app_notices.rs` the toaster and workspace error; `desktop/app_services.rs` the
    trading store, market factory, updater with its restart phase and the economic-event risk
    schedule; `desktop/app_chrome.rs` the chart chrome preferences, drawing tool menu,
    favorites drag and drawing context toolbar; `desktop/market_watch.rs` the watchlist rows,
    flag filter and picker, row drag and market summaries; and `desktop/saved_workspace.rs`
    the layout library with its drawing sets, the chart settings and drawing templates, and
    the workspace writer with its last saved snapshot and one unsaved-edit set. `AppPages`
    gains the bottom panel and layouts page, `ProviderLoginFlow` the data connections and
    `TabStrip` the linked-chart sync revisions. The former bool clusters are enums,
    `struct_excessive_bools` is removed, and `TerminalApp` drops from 52 to 15 fields.
    The `#[path]` modules are a real module tree: every `desktop` child, including the former
    `components/`, `native_ui/`, `engine_market_worker/` and root-level files, lives under
    `apps/desktop/src/desktop/`, the library's fixtures under `apps/desktop/src/test_support/`,
    and `desktop_modules_follow_the_directory_tree` rejects a new `#[path]`.
  - Left: GUI check of tabs, search and menus. Part of UI-07.
- [ ] M7.6 Dirty-source `FrameScheduler`
  - Landed: `desktop/frame_scheduler.rs` replaces `frame_wakes.rs`. Its `DirtySet` holds one
    bit per `AppSource` (summaries, screener, trading, notices, persistence receipts, account,
    updater) and the marked panes, keyed by surface entity, and wakes the one listener that
    requests a frame; `FramePollGate` is kept as its one-frame dedupe because the diagnostic
    endurance loop measures frames through the same gate. Each pane mailbox, the summary and
    screener workers, the trading subscription, the trading request feedback, the workspace
    writer (a new completion wake), the account change wake and the context feed (all panes)
    mark their own source, and the frame drains only what was marked with the existing
    budgets; the pane queue re-wakes on a remainder, so an over-budget pane drains on the next
    frame. Render and window activation no longer schedule a poll, and a pane returning to
    the foreground is marked. The 100 ms timer is deleted; a 500 ms updater maintenance
    interval runs only in a window with an update client. Layout persistence follows a
    revision: mutations call `record_layout_change`, the per-frame structural compare is
    gone, a busy writer coalesces rather than refuses, a refused save (writer exited or
    generation exhausted, neither recoverable in-process) stays due, and selection adoption,
    failed-switch restore and durable
    viewport changes now mark the pane dirty themselves. UI diagnostics are held and reported
    only with the `diagnostics` feature, only for panes the frame drained. The chart observer
    dispatches recoveries its data bridge finds without waiting for the pane's next message.
    A deferred study registration or reinitialization arms one timer per pane for its
    earliest retry deadline (study times come from the GPUI executor clock), so the retry runs
    without another market message; nothing is armed while no retry waits. An automatic study
    removal refused by a full command lane records its own retry deadline on that timer,
    cleared once every removal is sent. GPUI tests cover an idle terminal requesting no frame
    after setup, an over-budget pane draining on the next frame, a deferred registration and a
    refused automatic removal each retrying on the timer alone, and an idle pane arming no
    study retry.
  - Left: record idle CPU for 1, 4 and 16 panes, before and after. Closes UI-16.
- [ ] M7.7 Versioned, checksummed workspace container
  - Landed: `platform_runtime::DurableContainer` (`durable_container.rs`) is the one container,
    built on `platform_sys`'s `StagedFile` rather than a second `DurableFile` writer. Its
    envelope is `magic | envelope_version u16 | schema_version u32 | len u64 | sha256 | payload`
    (little-endian) with a 12 MB payload bound. A write refuses an oversized payload, skips an
    unchanged file, copies a valid current primary (enveloped or released-version bare) to
    `<name>.bak` and commits the new envelope; a corrupt primary never replaces the backup.
    After a commit it removes staged temporaries of the file and its backup that another
    process left. Loads hand each verified payload to the caller's decoder, trying the
    primary, then up to four exact `StagedFile` temporaries newer than the backup (newest
    first, from a scan of at most 1,024 directory entries), then the backup. A recovering
    load moves an unusable primary (bad magic, header, length, checksum, bound or decoder
    rejection; not an I/O error) to `<name>.corrupt-1`, rotating to keep the newest three;
    `inspect` changes nothing. The magic `\0AERISDC` starts with NUL, which no released
    payload can: a protobuf `WorkspaceState` cannot start with field number 0 and chart chrome
    is ASCII-keyed text. Any NUL-led file must carry the full magic; any other non-empty file
    opens unchanged as a legacy payload and is enveloped on the next save, and the
    released-version `.bak` names are kept. Workspace and chart-chrome I/O (schema 1, payloads
    unchanged) use it; the readiness gate inspects read-only and the legacy `.frame` migration
    also runs when no container candidate was usable. Rejections and non-primary restores are
    reported through observability. `platform_sys::fs` gains `parse_staged_temporary_name`.
    Tests cover the round trip, a released file becoming the first backup, a truncated
    primary restoring the backup and being quarantined (container, workspace and chrome), a
    crash-left staged temporary being used and later cleaned up, three-copy quarantine,
    checksum and partial-magic rejection, an oversized chrome file loading defaults with a
    diagnostic, and that released payloads never start with the magic. The naming_check rule
    `workspace_and_chart_chrome_persist_through_the_durable_container` is new.
  - Left: GUI check with a truncated workspace file in a validation folder (the layout is
    restored from the backup); not run here. An envelope schema newer than 1 is rejected
    and quarantined like corruption until M7.8 adds `NewerVersion` read-only handling.
- [ ] M7.8 Writer-owned workspace document and migration chain
  - Landed: `RestoreOutcome` (`Fresh`, `Restored`, `RestoredFromFallback`, `Migrated`,
    `NewerVersion`, `Defaulted`) in `desktop/local_state.rs`; `DurableContainer` carries the
    newest supported schema and reports a newer-version file without quarantining it or
    offering an older copy, and `write` never replaces a newer file or an unsupported schema.
    A newer-version workspace opens read-only (no writer thread starts) with a persistent
    notice; fallback restores and defaults post a persistent `UserNotice` with an Open folder
    action via the new `platform_runtime::reveal_in_file_manager`. Readiness refuses a
    newer-version workspace. Tests cover a newer workspace opening read-only and refusing
    writes, a corrupt primary restoring the backup with the quarantined copy reported, a
    corrupt document defaulting while keeping the rejected files, and the legacy frame
    reporting `Migrated { from: 0 }`.
  - Landed (migration chain, per M-D5): `desktop/workspace_migrations.rs` owns the ordered
    chain to schema 3 — the legacy `workspace-state.frame` is schema 0, a released bare
    protobuf payload carries its schema in `schema_revision` (at most 2, so a bare payload
    past that is rejected as unrecognizable), and the envelope schema is authoritative for
    enveloped files. 1-to-2 moves the order ticket out of the order book; 2-to-3
    canonicalizes flattened theme-default colors to the `theme` marker, frees the legacy
    series fallbacks only for layouts without the bottom fill, and materializes
    runtime-managed legacy indicator entries into the study graph above existing identities
    (the old restore-time heuristics are deleted; restore reads both plainly). The chain runs
    before the sanitizer on every load and is persisted at the first schema 3 write, which
    preserves the pre-migration payload as `workspace-state.pb.pre-v3` exactly once and keeps
    the frame file. Golden tests cover each step, idempotence, the write-once copy, the
    bare-revision fence and an enveloped schema 1 development file.
  - Landed (chrome fold, one writer): document schema 4 adds `WorkspaceState.chart_chrome`
    (`WorkspaceChromeState`, tag 22) and the final chain step folds the legacy chart-chrome
    `key=value` file into it — read once through `DurableContainer::inspect` (never
    quarantined, moved or rewritten; the file stays in place), skipped when the document
    already has chrome, and absent or unusable files leave the defaults. Enum fields persist
    as the legacy string identifiers so unknown values fall back to defaults. The separate
    chrome save thread, its receipts (`ChartChromeShutdownWait`, `ChartChromeSaveState`) and
    the chrome notice retry action are deleted: pane chrome changes emit
    `ChartChromeChangedEvent`, `WorkspaceTabs` syncs them into `AppChrome` and marks the
    layout save due, and the window's one workspace writer persists layout and chrome in the
    same generation sequence (its condvar-based shutdown wait is the one receipt; a newer
    request wakes a pending wait immediately). Golden tests cover the 1.2.0 chrome file
    byte-for-byte, the read-only fold, a missing file, restart survival without the legacy
    file, chrome saving through the one writer and the condvar wake.
  - Landed (writer-owned document, readiness dry run): the writer thread is seeded with the
    restored document at startup and owns it in memory (`TerminalShellInit.workspace_document`);
    each save applies the request's sections via `apply_workspace_save` (tagged by layout
    generation) and writes — the per-save `load_workspace` read-modify-write is deleted and
    `load_workspace` is test-only. `RestoredWorkspaceShellPlan` no longer carries the
    revisions (they live on the document), and the redundant
    `DesktopPersistence::chart_chrome_path` method is gone (the fold keeps the free path
    function). Readiness is documented and locked in as the write-free dry run of the real
    load: full decode + chain (including the chrome fold) + sanitize, no envelope upgrade,
    no pre-migration copy, no quarantine; an unmigratable legacy frame or unrecognizable
    payload fails readiness before activation. Tests: a save survives the on-disk file
    turning to garbage without losing writer-owned sections or revision arithmetic, and the
    readiness dry run returns exactly the document startup would restore while leaving every
    file byte-identical.
  - Left: the GUI checks below.
  - GUI checks: a corrupt workspace shows the notice with Open folder and the layout is not
    silently replaced; a newer-version workspace shows the read-only notice; a copied real
    workspace migrates intact.
  - Workspaces written by 1.0.0-1.2.0 must open unchanged in meaning, including saved layouts,
    watchlist flags and pane heights. Closes UI-09 and PL-18.
- [ ] M7.9 Hollow candles as an Aeris Charts series style
  - Landed: aeris-charts `50bde98` (`feat(chart): engine-owned hollow candle bodies`) adds the
    engine `CandleBody` style (`Solid`/`Hollow`/`HollowBullish`/`HollowBearish`) with
    `set_candle_body`/`candle_body`; a hollow direction leaves its body unfilled while the frame
    and wick carry the direction color. Terminal bumps the five `aeris_charts_*` pins to
    `50bde982233a6f7e3dff09b8bc7bb4635fa5f635`, deletes the host `hollow_appearance`/`HOLLOW_BODY`
    transform, applies `chart_type.candle_body()` in `apply_product_series_kind` and the
    appearance-reset path, and naming_check bans `hollow_appearance`/`hollow_base`
    (`chart_integration_candle_bodies_are_engine_owned`).
  - Capability review: the in-place data reset M6.4 asked about already exists as
    `ChartEngine::set_series_data` (full column replacement on the same series), and the
    `PanePresentation` apply path reaches it through `install_product_price_series`; no new
    engine operation was needed. No other unused engine capability arrived between `8924f8d`
    and `50bde98` (the pin bump carries only the hollow-candle commit).
  - Left: GUI check of the hollow style (screenshot comparison before/after). The wasm binding
    and web demo keep the transparent-body color path; exposing `CandleBody` there is a possible
    follow-up, not required by Terminal.
- [x] M7.10 Sweep classification and incremental tape in `market_runtime` (012d2c5c)
  - Landed: the classifier moved to `market_service/trade_sweeps.rs` unchanged (512-trade
    trailing window, 90th-percentile size, 100 ms grouping, moved tests included) and runs once
    per ingested trade plus on every retention eviction and session reset, so
    `ProviderOrderBook::trade_sweeps` always describes the retained window. The tape publishes
    as fenced `TradeTapeUpdate` events carrying
    `MarketTradeTapeOperation::Reset { trades }` / `Append { from_ordinal, trades }`, the full
    continuity metadata of the snapshot and the current sweeps; a consumer's bounded (64) update
    queue overflows into one covering `Reset`, and a new selection generation, provider session,
    prefix rewrite or eviction gap restarts the stream with a `Reset`. Tests: Append/Reset
    sequencing across rewrite and selection-generation changes, overflow-replay reconstructs the
    exact tape, sweep classification over the retained trailing window. M7.11 made
    `TradeTapeUpdate` the only tape publication and removed the legacy full snapshot.
- [ ] M7.11 Desktop consumes runtime sweeps and incremental tape
  - Landed: `OrderFlowPresenter` applies the runtime's fenced `TradeTapeUpdate` stream into a
    ring bounded at the runtime's `MAXIMUM_RECENT_LADDER_TRADES` (65,536): a `Reset` installs
    the covering window, an `Append` must continue the ring exactly at `from_ordinal` (a gap
    leaves the tape untouched and the runtime's next covering `Reset` restores it), and a newer
    selection generation supersedes regardless of provider-session numbering. The chart applies
    the projected change as `OrderFlowTapeChange::ReplaceWindow` (covering window) or `Append`
    (new suffix only) through chart_integration's `apply_order_flow_trades`; an append ahead of
    the covering window is rejected for a resend (`OrderFlowApplyError::AwaitingCoveringWindow`).
    The host sweep classifier and ordinal-diffing (`classify_order_flow_sweeps`,
    `order_flow_resume_ordinal`, `invalidate_order_flow_prefix`) are deleted, the Time & Sales
    panel highlights the runtime's `MarketTradeSweep` spans directly, and naming_check's new
    `chart_integration_has_no_order_flow_analytics` rejects their return. The legacy
    `TradeTapeSnapshot` is removed from `market_runtime` entirely (the desktop was its last
    consumer). Tests: presenter Reset/Append/overflow-Reset/ring-bound/stale-fencing/
    selection-generation supersede, engine-worker tape fencing, Time & Sales rows and
    runtime-sweep highlights, chart_integration covering-window/append/tick-size-rebuild
    behavior, interval-switch fencing against the superseded chart.
  - Measurement (`order_flow_tape_apply_100k_measurement`, an ignored deterministic harness in
    `chart_integration` run with `cargo test -p aeris_chart_integration --locked --lib
    order_flow_tape_apply_100k_measurement -- --ignored --nocapture`, debug build, no GUI):
    the covering 100k-trade window apply took 131.1 s on the old full-snapshot path and 143.0 s
    on the new `ReplaceWindow` path (same order; the engine's window replacement dominates, and
    a covering window arrives only on selection/session changes or overflow). Steady state is
    the win: the old path re-projected the 513-trade window per revision (22.2 us for 10
    no-op revisions only because the chart diffed and discarded already-applied trades), while
    the new path applies 10 single-print `Append`s in 24.8 ms total (~2.5 ms per print, inside
    one frame budget) with no re-projection and no ordinal diffing.
  - Left: GUI Hyperliquid sweep-highlight check. Closes UI-14 with M7.3 and M7.9.
- [ ] M7.12 Desktop tests against controller boundaries
  - Landed: `test_support::provider_instruments` renamed to `test_support::providers` per the
    spec (design 11 §8 does not name the module; the work item does), with
    `tools/naming_check` fixture paths and gate-test strings updated. The last guard-specific
    tests are deleted from `desktop/tests.rs`: `should_apply_rithmic_worker_stop` plus
    `dead_rithmic_worker_stop_transition_is_applied_once` (the production guard is gone; the
    test asserted a test-local copy, so the invariant is structural; stop-transition behavior
    is covered by `pane_harness_repeated_connection_stop_has_one_redacted_notice_and_streaming_clears`
    and `worker_stop_preserves_a_concrete_startup_error`) and
    `rithmic_catalog_ready_autoloads_once_without_owning_reconnects` (replaced by the harness
    behavior test `pane_harness_catalog_ready_starts_one_first_listing_and_stops`; projection
    pass-through stays covered by `catalog_rejection_lane_comes_from_the_runtime_not_the_reason`).
    Added `pane_harness_seeded_switch_interleavings_project_only_the_committed_generation`
    (seeded xorshift interleavings of commit/snapshot/tail-update/readiness from the retired
    and committed generations; the tape leg of the interleaving requirement stays covered by
    `interval_switch_keeps_order_flow_off_the_superseded_chart`).
  - Left: tape events are not yet part of the seeded interleavings mix; GUI checks none.
    Closes UI-15.

M7 closeout: the full gate (fmt, workspace clippy, workspace tests, naming_check) passed locally on
24dc339a, and CI run 40677305725 passed on 24dc339a, which contains all M7 code. The open M7 items
still wait on the GUI checks and the remaining work on their lines.

### M8 lifecycle and trim

- [x] M8.1 Release code moves into a `release_lifecycle` crate
  - Landed: `crates/release_lifecycle` (`aeris_release_lifecycle`) holds `lifecycle.rs`,
    `release_delivery.rs`, `rollout.rs`, `release_identity.rs`, the `aeris_launcher` binary and
    its icon `build.rs`, moved unchanged apart from imports. `sign_release_manifest` and
    `sign_block_plan` compile only under `cfg(test)` or the `producer` feature, which the
    crate's own dev-dependency enables for the launcher tests. `ed25519-dalek`, `semver`,
    `same-file`, `sysinfo` and `embed-resource` left `platform_runtime`; `cargo tree -i` shows
    `ed25519-dalek` and `semver` reach only `release_lifecycle` and the desktop, never an
    adapter. The owned-root safety check moved into `platform_runtime::data_root`, which now
    reports its own `DataRootError` (`UnsafeOwnedRoot`, `StateMigrationFailed`) instead of the
    lifecycle's error, and the two legacy-merge tests moved with it. The desktop imports release
    identity, install root and publisher verification from the new crate. New naming_check rule
    `release_lifecycle_has_only_allowed_dependents_and_signers`: only the desktop,
    `tools/release_packager` and the crate itself may depend on it, only the packager and the
    crate may enable `producer`, the moved dependencies and files cannot return to
    `platform_runtime`, and the signers stay gated. Kept, not deleted (D2). Closes PL-01.
    CI passed the workspace gate on 27dd1f50, which includes this commit.
- [ ] M8.2 Launcher hygiene and trust
  - Landed (LC-01, LC-16, LC-14): the launcher parses its command into `LauncherCommand` before
    any key loads; `--launcher-identity` and `--remove-all-local-data` never load the verifying
    key, and every release command loads it in `run_verified`. Uninstall moved to the key-free
    `ReleaseUninstaller` (shared lifecycle paths, no `VerifyingKey`), and the launcher's
    uninstall hook waits for every installed desktop instead of auditing the active release's
    signature. The desktop spawns launcher promotion only for a plain start of an installed
    release (`update::launcher_promotion_applies`, install generation above 0) and reaps the
    child on the `aeris-launcher-promotion` thread, logging an unsuccessful exit. LC-14 was
    already met: launch errors use `platform_runtime::message_box` (`MessageBoxW` in
    `platform_sys`). Tests: per-command parsing, identity running with no key, every release
    command stopping at a missing key, malformed commands rejected before the key, the
    key-free uninstall round trip, and the promotion gate; naming_check pins the key-free
    dispatch and uninstaller.
  - Landed (LC-13, LC-15): `platform_sys::process` (re-exported by `platform_runtime`) opens the
    processes whose image matches once (Toolhelp snapshot plus `OpenProcess` with
    `SYNCHRONIZE`), waits on their handles with `WaitForSingleObject` and re-enumerates once to
    confirm none started meanwhile; a matching process that grants query but not wait access
    fails closed. macOS uses `proc_listallpids`/`proc_pidpath` and Linux `/proc`, both polling
    the pid's image at 50 ms. The launcher's 20 ms sysinfo scans are gone (stable-launcher
    promotion, restart, update and uninstall waits, and the absence audit), and sysinfo left
    `release_lifecycle`; `cargo tree -i sysinfo@0.39.6` now shows only the desktop (its OS
    version string and readiness conformance), and GPUI's target-specific 0.31 stays. Uninstall
    renames the running launcher to `aeris_launcher.retired-<pid>` in the sibling
    `.<name>-uninstall-stage` directory; the `.bat` deleter is deleted. Every launcher start
    and each relocation sweep that directory (exact retired names and the 1.x leftovers
    `aeris_uninstall.exe`, `finish-uninstall.bat`, `deleter.log`; unknown files are kept, at
    most 64 entries), and `aeris_setup.iss` deletes the retired launcher after its uninstall
    step. Tests: spawned-child enumeration and exit wait, current-process and unmatched-image
    exclusion, relocation naming, the sweep keeping unknown files; macOS `platform_sys` clippy
    passes for `aarch64-apple-darwin`. naming_check rejects a returning batch deleter and a
    sysinfo dependency in `release_lifecycle`.
  - Landed (LC-20): `release_lifecycle/build.rs` alone reads the six packaging variables and
    validates them with `release_config_rules.rs`: none set is a development build, any set
    requires all (the Authenticode thumbprint only on Windows targets, and it is rejected
    elsewhere), with a 32-byte base64url key, an https base URL without trailing slash, query
    or fragment, positive generations and a 16-128 character release identity; anything else
    fails the build naming the variable. It generates the one static behind
    `release_config()`, which the launcher (key, base URL, bootstrap generation), the release
    identity and the Authenticode audit read; the five `option_env!` sites are gone. Tests: the
    rules (development build, exact complete config, each missing variable, each invalid value,
    the Windows-only thumbprint); a complete release-configured build passed the release_config
    tests and a build missing the thumbprint failed with that message. naming_check rejects
    any other read of the variables and keeps them all out of every workflow.
  - Landed (LC-07): `platform_sys::trust::verify_publisher` (re-exported by `platform_runtime`)
    verifies the embedded signature and chain in-process with `WinVerifyTrust` (no UI, no
    revocation check, cache-only URL retrieval), then requires the configured signer's SHA-1
    thumbprint (read with `CertGetCertificateContextProperty`) and a timestamp countersigner.
    The release configuration now carries the thumbprint as 20 bytes. The spawned PowerShell
    `Get-AuthenticodeSignature` check, its System32 path probing and 15-second poll are
    deleted; a failed audit logs the typed reason. With LC-20 a Windows release build cannot
    omit the thumbprint, so only a development build skips the audit. Tests: an embedded-signed
    Microsoft executable verifies, its thumbprint matching PowerShell's independent reading;
    a different signer, the unsigned test binary, a catalog-only system file and a modified
    signed copy are all refused; the installed-inventory audit refuses an unsigned `.exe` with
    a configured signer. naming_check rejects PowerShell in `release_lifecycle` production.
  - Left:
    - Check: a development desktop start spawns no launcher child (process list); not run here.
- [x] M8.3 Remaining FFI moves to `platform_sys`
  - Landed: `display_timing.rs` (display inventory, QPC and DWM composition probes) is deleted
    with its `Win32_Graphics_Dwm` and `Win32_System_Performance` features and its
    `display-info`, `wayland-client` and `wayland-protocols` dependencies (also dropped from the
    workspace pins); no production or tool code used it. Its interim unsafe allowlist entry is
    removed. Closes PL-10 (display_timing).
  - Landed: `NativeSessionShutdownMonitor` (no callers) is deleted with its Unix signal and
    logind workers and the `signal-hook` dependency. `NativeSessionShutdownGuard` is kept,
    because no maintainer logoff flush test has proved GPUI's own end-session handling drains
    account refresh rotation; it moved to `platform_sys::session` with its window procedure in
    `platform_sys/src/windows/session.rs` (documented single-operation `SAFETY` blocks) and is
    re-exported by `platform_runtime`. A second guard in one process now reuses the registered
    window class instead of failing. Its interim unsafe allowlist entry is removed.
  - Landed: the suspend/resume and network-availability FFI moved to
    `platform_sys::power::SuspendResumeRegistration` and
    `platform_sys::network::InterfaceChangeRegistration` (Windows power-manager and
    connectivity-hint callbacks; macOS `IOKit` and `SCNetworkReachability` run-loop threads),
    which also own `PowerEvent` and `NetworkEvent`. The coalescing event streams, cancellation and
    the Linux logind/NetworkManager backends stay in `platform_runtime`. The macOS reachability
    thread now runs its run loop in
    one-second slices, so a stop requested just before the loop starts is no longer lost. Both
    interim unsafe allowlist entries are removed. Checked: Windows tests and clippy; macOS
    `platform_sys` clippy with `--target aarch64-apple-darwin`. macOS `platform_runtime`
    cannot be cross-checked here (`ring` needs a C compiler) and nothing ran on macOS; the next
    macOS preview lane is its first macOS build.
  - Landed: `CancelIoEx` (`io_cancellation.rs`) is deleted. The new Rithmic abort-latency test
    blocks a session read on a silent local peer and aborts it five times: `shutdown(Both)` plus
    the 100 ms read poll ended every read within 87 ms (bound: twice the poll interval). The
    driver-stop fixture now reads with that poll, as session reads do, instead of a raw blocking
    read only `CancelIoEx` could end. `platform_runtime` inherits the workspace
    `unsafe_code = "forbid"` and has no windows-sys dependency; the interim allowlist is gone, and
    naming_check rejects any manifest other than `platform_sys` that lowers `unsafe_code`.
    Closes PL-02.
  - Maintainer-only: the logoff flush test that would allow deleting `NativeSessionShutdownGuard`.
- [x] M8.4 Account runtime vault and lock fixes
  - AC-02: `credential_vault::MAX_SECRET_BYTES` is 2,560 bytes (the Windows blob limit) on every
    platform; `store()` rejects a larger secret with `SecretTooLarge` before the backend runs.
    Const asserts keep the account profile cache (now 2,560, was 4,096), the refresh-token bound
    (now 2,560, was 8,192) and the Rithmic credential blob within it. A macOS profile entry
    above 2,560 bytes now reads as absent, so that restore verifies online first.
  - AC-03: no vault or rejection-marker I/O runs under the account state mutex. A separate
    vault-commit lock covers each fence check and the writes it admits, so a stale worker's
    write always lands before a newer session's; the state is re-locked and the fence re-checked
    before applying. A login cancelled, expired or signed out during its vault write now retires
    what it stored. Sign-out deletes (and reads the token to revoke) under the same lock.
  - AC-06: the vault access mutex recovers from poisoning; `AccessSerialized` is removed.
  - Tests: oversize and poisoned-lock vault tests, a probe proving no vault call runs under the
    state lock across restore, refresh, rejection, login and sign-out, a cancel-during-commit
    test, profile and refresh-token bound tests. AC-04 and AC-08 were closed by the Cognito
    sign-in work.
- [x] M8.5 Shared readiness report
  - LC-11: `aeris_release_lifecycle::ReadinessReport` (schema 6, unchanged wire shape) is the one
    type the desktop writes and the launcher reads; `accept_candidate` holds the launcher's checks
    and names the failed one (`ReadinessRejection`). The report is written with
    `write_file_durably`. Contract tests: encode/decode round trip, the exact schema-6 JSON,
    unknown and missing fields rejected, one case per rejection.
  - LC-05 (readiness part): readiness starts the trading owner only on a
    `trading_runtime::ReadinessDatabaseCopy` beside the database, under the instance lock. It is a
    file copy, not `VACUUM INTO` (D21). The scratch is removed after the owner stops, and store
    open sweeps leftovers with the same exact-contents rule as preflight scratch. Tests: a v20
    database keeps its bytes and `user_version` while its copy migrates to head, WAL frames are
    copied without changing the database or sidecars, a fresh install never creates the
    database, and stale copies are swept while foreign contents stay.
  - `provider_count > 0` stays (D20). naming_check requires the copy and the shared type.
  - Checked: `aeris_desktop --validation-root <dir> --desktop-readiness <report>` exited 0 in
    about 1 s on a sanitized v18 database. Its SHA-256 and `user_version` 18 were the same
    before and after, and the trading folder held only `trading.sqlite3`. On a fresh root it
    exited 0 and created no database. The launcher-driven readiness of an installed candidate is
    covered by M8.7's end-to-end harness.
- [ ] M8.6 Release wire protocol v1 and Axiusflow residue
  - Landed: LC-04. The update-restart lines are `AERIS_UPDATE_RESTART_READY_V1` and
    `AERIS_UPDATE_RESTART_COMMIT_V1`, defined once in `aeris_release_lifecycle` for the launcher
    and the desktop; the transitional aliases are deleted. Block plans sign under
    `AERIS_RELEASE_BLOCK_PLAN_V1`. naming_check rejects `AXIUSFLOW` in Rust sources (PL-16
    tokens part). No launcher has shipped, so no old token is still accepted.
  - Landed: LC-03. A signed manifest carries its exact bytes as `manifest_b64url` and an Ed25519
    signature over `AERIS_RELEASE_MANIFEST_V1\0` followed by those bytes. The bytes decode with
    `deny_unknown_fields`, the fields are private, and `verify_release_manifest` checks the
    signature before any field (D22). The signature-only predecessor check is deleted; it had no
    caller. Tests pin a signature vector and cover tampered bytes, unknown fields inside and
    outside the signature, and the size bound.
  - Landed: LC-19. Manifests carry `minimum_launcher_protocol`, compared with
    `LAUNCHER_PROTOCOL_VERSION` (1) instead of the product version; `semver` is removed.
  - Landed: LC-08. `verified_block_delivery` returns a typed `BlockDeliveryError` (unverified
    source, base URL, request, HTTP status, unreadable or oversized body, rejected plan), and the
    launcher logs it and each failed file reconstruction before falling back to full downloads.
    A test covers each response failure; naming_check rejects `.ok()?` there.
  - Landed: LC-12. `tools/windows/aeris_setup.iss` asks before uninstalling, with "Remove my Aeris
    data" unchecked. By default, and always for a silent uninstall, it runs the new launcher
    `--remove-application` (`ReleaseUninstaller::remove_application`), which removes the install
    and lifecycle roots and keeps every data, cache, log and legacy root and vault entry; a ticked
    box runs `--remove-all-local-data`. A pending full uninstall is never downgraded. The Axiusflow
    install-delete, Run value, close-resource and pending-uninstall lines are deleted, and the
    script requires Inno Setup 6.6 or newer. A lifecycle test covers the kept data; naming_check
    checks the choice and rejects Axiusflow in the script. The script compiles with Inno Setup 6.7.
  - Left: the manual install and uninstall on a Windows VM (F17), checking the prompt and that
    data is kept by default and removed only when ticked. Not run; maintainer-only.
- [x] M8.7 Release packager and end-to-end lifecycle harness
  - Landed: LC-21. The launcher is library code (`aeris_release_lifecycle::launcher`,
    `run_launcher`) behind a thin `aeris_launcher` binary. It reads the origin only through
    `ReleaseSource` (channel, signed objects, byte ranges, artifact bodies);
    `HttpsReleaseSource` is the production adapter, and `origin_layout` holds the channel and
    release paths the launcher and the packager share.
  - Landed: LC-02. `tools/release_packager` packages one release (payload executables,
    rollback-compatibility data, signed manifest, and a signed block plan against a predecessor),
    writes the channel pointer and prints the Inno Setup defines. The key comes only from a key
    file, and the packager has no network dependency and publishes nothing; naming_check
    enforces both.
  - Landed: the `lifecycle_e2e` test runs the launcher against a `DirectorySource`: install,
    a readiness-rejected update rolled back inside the update, a block-plan update that downloads
    only the changed desktop block, activation, a crash on first launch that quarantines the
    release and rolls back, an update that skips the quarantined offer, a later update and
    recovery. The installed programs are copies of the `aeris_lifecycle_fixture` binary; copying
    the test executable itself was quarantined by Windows Defender as `Bearfoos.A!ml`. Passed
    three local runs on Windows. Probation, hang and schema-gated rollback join it in M8.8 (D23).
  - Verified: the test passed on the windows-latest CI run for d69cd264 (run 40716432998,
    83 s).
- [x] M8.8 Rollback safety
  - Landed: LC-05. The packager writes `max_store_schema` (the payload's `SCHEMA_VERSION`) into
    the rollback-compatibility data. Before a crash rollback the launcher reads the live trading
    store's `user_version` from the SQLite header (`store_schema`); a write-ahead log holding
    frames counts as the failed release's schema, and an unreadable store fails closed. A retained
    release that cannot open the live store is refused with `RollbackStoreSchemaNewer` before
    quarantine, so the failed release stays active. Unit tests cover the header read and the gate;
    `lifecycle_e2e` opens the real store through the trading owner and adds a release that
    migrates to schema 31, crashes and is not rolled back.
  - Landed: LC-09. An installed desktop records `.startup-complete.json` in the install root
    (generation, release identity, process ID) on the background executor once its first
    sign-in or workspace window opens; validation roots and development builds record nothing.
    With a retained release, the launcher waits up to 60 s for its own child's marker; an
    unsuccessful exit or no marker in time (the child is stopped) quarantines and rolls back, and
    a clean exit without a marker (a second instance) neither accepts nor rolls back (D24). A
    unit test covers the marker, the process and release match and the bound; `lifecycle_e2e`
    adds a release that hangs, is stopped after 60 s and rolled back. The installed desktop
    writing its marker is not yet exercised on an installed app.
  - Landed: LC-18. An update journals `ReadinessPassed` after readiness passes on the data copy
    and before the active pointer moves; recovery from it re-audits the candidate and either
    finishes the update or restores the previous release, never running candidate code (D25).
    Recovery then moves entries in `versions/`, `active/` and `manifests/` that no release
    record owns into `orphaned/<ms>/` (newest 8 batches kept), so a stray entry no longer blocks
    launch. Unit tests cover each journal state, a candidate changed after readiness and the
    batch bound; `lifecycle_e2e` adds a stray release directory and manifest, and the launch
    moves them aside and starts the active release.
  - Landed: LC-06. The `--update-and-restart` helper does all verification in one step before
    READY: one active-release audit (it was two), the prepared release's signature, files and
    eligibility, and the current channel offer. After READY it only waits for COMMIT and the
    desktop's exit, which naming_check enforces. The desktop waits for READY up to the shared
    `UPDATE_RESTART_VERIFICATION_TIMEOUT` (2 minutes) instead of 20 s and stays usable meanwhile;
    READY to COMMIT stays inside the desktop's durability waits (20 s account quiesce, 2 s
    workspace) and the launcher's 60 s commit wait (D26). A desktop test spawns a fake helper that
    spends 25 s verifying before READY and the restart proceeds. The real verification time on an
    installed app is not measured.
  - Verified: `lifecycle_e2e` with the crash, hang and orphaned-entry cases passed on the
    windows-latest CI run for d69cd264 (run 40716432998).
- [x] M8.9 Dependency direction and `contracts`
  - Landed: G5. `EmbeddedReplaySource` and the demo chart constructor compile only for tests or
    the new `test-support` features of `application` and `chart_integration`; only
    dev-dependencies and the desktop `diagnostics` feature enable them, which naming_check checks
    in every manifest (`embedded_demo_replay_is_test_support_only`).
  - Landed: G4. `contracts` keeps only the persisted workspace schema. The in-process catalog,
    status and provider-presentation types moved to `market_runtime`, the account views to
    `account_runtime`, and the unused `GetAccountStatus`, `RefreshAccountProfile`,
    `BeginLogin`, `CancelLogin` and `SignOut` messages were deleted. naming_check allows only the
    desktop and `market_runtime` to depend on `contracts` and rejects a non-prost type in it.
  - Landed: direction rules. naming_check reads every `Cargo.toml` and checks its normal and
    build dependencies against the architecture §2.2 layer order and rules 3, 5, 7 and 8
    (`crate_dependencies_follow_the_direction_rules`, with fixture tests); every workspace
    package must have a layer. Rules 1, 2 and 6 keep their existing checks.
  - Landed: `study_sdk -> market_runtime` recorded as the accepted facade (D27), so
    `cargo tree -p aeris_study_sdk` still lists the adapters by decision. Closes PL-13 and PL-14.
- [ ] M8.10 Dependency hygiene
  - Left:
    - Fetch the avatar with ureq and remove `reqwest_client` (AC-07).
    - Remove unused crates and put `rpassword` behind a feature.
    - Centralize pins and dedupe sysinfo.
    - Give the context `request_reply` a timeout (PL-17).
    - GUI smoke test.
  - Also covers the remaining TR-12 items and PL-15.
- [ ] M8.11 Vendor and tooling trim
  - Left: trim the `gpui_base` LSP editor, then lint and pin it.
  - Delete the physical-pacing scripts.
  - Move `live_market_gate` out of the production `platform_runtime` library.
  - Prune `.gitignore`.
  - GUI text-input check. Closes PL-10, PL-12 and PL-19.
- [ ] M8.12 Structural naming_check and final docs
  - Left: structural checks with a recursive markdown inventory, with text pins replaced by
    behavior tests.
  - Finalize `Readme` and `AGENTS.md`, and close out this tracker. Closes PL-08.

## 4. Decisions

Original decisions (2026-10-13). They override the research design documents.

| # | Topic | Decision |
|---|---|---|
| D1 | Scope | Paper-trading maturity, market-data lifecycle rebuild, desktop decomposition and provider neutrality, platform foundations and bloat trim. |
| D2 | Kept code | The launcher, updater, rollout and signed lifecycle are kept: review, fix, isolate and trim them, never delete them for being dormant. The account runtime is the live mandatory Cognito sign-in since 2026-10-25. |
| D3 | Paper realism | "Solid core": correct lifecycle, market/limit/stop/stop-limit, brackets, OCO, DAY/GTC/IOC/FOK, bid/ask fills, tick checks, multiplier, fees, cash ledger, restart safety. Out: depth-aware partial fills, slippage, margin and buying power (hooks only). |
| D4 | Closed market | Paper orders on a closed market or unknown session are rejected with a typed reason. |
| D5 | DAY expiry | At session close for exchange instruments, 00:00 UTC for 24/7 crypto; DAY is rejected when the session is unknown. |
| D6 | Stale quotes | Stale after 10 s for crypto and 60 s otherwise; new orders are rejected and fills held with a typed reason. |
| D7 | Fees | Configurable per instrument class; default zero. |
| D8 | Existing data | Trading databases, workspace state, chart chrome and vault entries are migrated, never wiped; practice brackets migrate to order groups. |
| D9 | Fill prices | Market and marketable limits fill at the touch; resting limits at their limit price; stops trigger on the correct side and fill at the touch, gaps included. |
| D10 | Golden files | Trading golden files may change only when behavior intentionally changes; list each change in the commit. Replay goldens stay byte-identical (M-D26). |
| D11 | Fake CLI modes | `--rithmic` and `--multi-chart` fake-instrument modes are removed. |
| D12 | provider_history | Unused parts trimmed per `architecture.md`. |
| D13 | Aeris Charts | Chart changes are made in Aeris Charts first and pushed to its `github` remote (never `origin`, never forced) when its clone is clean and its gates pass; then only the `aeris_charts_*` pins are bumped here. |
| D14 | CI | Hosted CI on push and pull request (see `AGENTS.md`). Only the two manual preview lanes may receive the Rithmic kit. |
| D15 | plan/ | `plan/stabilization/` stays in the repo with this tracker and the reports. |
| D16 | Git | Work is committed on `main` and pushed to `origin/main` after its checks pass; never force-push. |
| D17 | Validation | GUI checks run on an isolated data root while no other Aeris desktop runs. Public Hyperliquid data may be used live; live Rithmic is excluded. |
| D18 | Checks | Focused checks per change; the full gate once per milestone (section 1). Replaces the per-commit full gate (2026-10-28). |
| D19 | Order | M6, then M7, then M8; one commit per item or coherent part of an item. |
| D20 | Readiness providers | (2026-10-28) Readiness keeps `provider_count > 0`. It counts providers registered with the market runtime, which needs no sign-in, login or network, so a fresh install passes; zero means a broken build. |
| D21 | Readiness copy | (2026-10-28) The readiness copy is a file copy of the database and its sidecars, not `VACUUM INTO`: a read-only SQLite connection to a WAL database creates `-wal` and `-shm` beside it. The instance lock keeps the copied files consistent. |
| D22 | Manifest decode | (2026-10-28) Reading a signed-manifest envelope decodes its exact bytes into the typed manifest before the key is known, so a manifest with an unknown field is refused as early as possible. No outcome depends on a field until `verify_release_manifest` has checked the signature over those bytes, and the private fields keep the decoded manifest equal to the signed bytes. |
| D23 | Lifecycle harness scope | (2026-10-28) M8.7's end-to-end harness covers install, update, readiness rejection, activation, crash rollback and quarantine. The probation, hang and schema-gated rollback scenarios need LC-09's startup-complete marker and LC-05's manifest `max_store_schema`, so they are added to the same harness with those fixes in M8.8. Readiness on the database copy stays covered by the desktop's tests. |
| D24 | Probation acceptance | (2026-10-28) A release generation is accepted once a desktop it started records the startup-complete marker; later launches of that generation skip probation, so only a release that has never finished starting is rolled back for a crash or a hang. The marker is written when the first window opens (the sign-in window counts) so a user who has not signed in within 60 s does not trigger a rollback. |
| D25 | Update recovery | (2026-10-28) An update runs readiness on a copy of the user's data before the active pointer moves and journals `ReadinessPassed` first, so recovery finishes the update (or discards a candidate whose files changed since) without running candidate code. The `Activated` and `HealthChecked` journal states are removed; no released version wrote them, because 1.0.0 installs through Inno Setup without the lifecycle. A candidate that fails readiness never ran on the user's data, so its rollback needs no state-compatibility check. The launch audit stays exact; every launcher command recovers first, and recovery moves entries no release record owns into `orphaned/<ms>/`, keeping the newest 8 batches. |
| D26 | Restart verification bound | (2026-10-28) The restart helper's verification (a 15 s-bounded channel fetch plus hashing the prepared and active releases) runs before READY, and the desktop waits for READY up to a shared 2-minute bound rather than the former 20 s handshake, so a slow disk or channel no longer fails a restart. The READY-to-COMMIT window keeps its existing bounds, since the desktop's durability waits (up to 22 s) already fit the launcher's 60 s commit wait. |
| D27 | Study SDK facade | (2026-10-28) PL-13 is resolved as accepted, as `architecture.md` section 2.2 rule 8 records: `study_sdk` is the published-API facade above the in-process study runtime and is used only by the desktop's built-in study packages, so it keeps its `market_runtime` dependency and with it the adapters (`cargo tree -p aeris_study_sdk` still lists them). It is not an inversion while `market_runtime` only dev-depends on it, which naming_check enforces. Moving the study types into a contract crate waits until third-party study authors exist. |

Reconciled defaults from `architecture.md`: per-pane account selection is kept; the risk session
boundary defaults to 00:00 UTC (`ChicagoClose` exists but no desktop code selects it); V24 runs in
the gated `finalize_group_migration` step, and only removal of the old P&L tables and per-fill P&L
columns stays deferred; the Hyperliquid price grid is `Unknown` with a warning until verified; on
scale-in the first-triggering stop and target both grow to the position size; bracket legs keep
offsets from the entry fill.

Mission decisions (2026-10-26 to 2026-10-28). Commits and old notes cite them as plain D numbers;
they are prefixed M- here because those numbers collide with the table above.

| # | Topic | Decision |
|---|---|---|
| M-D2 | Neutrality scan | `desktop_source_is_provider_neutral` skips modules compiled only under `cfg(test)` or `cfg(any(test, feature = "diagnostics"))`; the final production exception is the `assets.rs` logo mapping. |
| M-D3 | Reverse UI | No new Reverse UI. VAL-DSK-037 covers flatten with the order book closed. |
| M-D4 | M7.2 | Re-scoped for mandatory sign-in: plan policy at the account boundary, an account module, and an account-owned wake replacing the UI polls. |
| M-D5 | M7.8 | The migration chain targets schema 3, keeps a `workspace-state.pb.pre-v3` copy and turns today's unversioned cleanups into versioned steps. |
| M-D6 | M7.10 | Sweeps are classified once per publication in `market_runtime` over the last 512 trades. |
| M-D8 | M7.1 website | Mirror only shared platform tokens; commit in the website only when its worktree is clean, run its lint, typecheck and build, and do not push without the maintainer. |
| M-D9 | macOS | `platform_sys` can be checked for `aarch64-apple-darwin`; other macOS-only checks are maintainer or hosted-CI only. |
| M-D10 | Repo hygiene | `plan/longterm_plan/*.md` is allowlisted, not deleted; the README keeps the name `Readme`; `gpui_base` stays outside the workspace; the Linux `sha2` duplicate is accepted; `resvg` is aligned only if chart export still works. |
| M-D11 | AC-02 | The Windows credential blob cap is 2,560 bytes, sized against realistic Cognito tokens; tests never touch the native vault. |
| M-D15 | M6.10 baselines | The `trading_projection_baseline_*` assertions and expected outputs stay identical. |
| M-D16 | CI | CI gates are never weakened (no serial, quiet, skip or retry workarounds). |
| M-D17 | Pane notifications | Account-level changes notify every pane selecting the account; instrument-level changes only matching selections. |
| M-D20 | Snapshots | Trading snapshot types and commands exist only behind `cfg(any(test, feature = "test-support"))`; account admission has its own 10,000 bound. |
| M-D21 | Discards | The three preview/cancel discards from bbdf6e06 keep their intent and are handled explicitly; a `#[cfg(test)]` statement inside a production function counts as test code. |
| M-D22 | Charts overflow | The Aeris Charts off-viewport line overflow was fixed in Charts (8924f8d), never worked around here. |
| M-D24 | Line drags | Only protective (stop and target) lines are draggable; entry-line dragging is out of scope. |
| M-D25 | Command surface | Production `TradingService` keeps only owner-priced methods under neutral names (`place_order`, `place_bracket`, `place_inline_bracket`, `place_chart_bracket`, `place_protective_order`, `modify_order`, `modify_order_checked`, `place_oco`). |
| M-D26 | Test seam | Owner-priced ordering is production behavior. Caller-priced semantics survive only through one `#[cfg(test)]` crate-private seam for lifecycle, risk, retention, group, venue, recovery, replay and restart tests. |

## 5. Rules not stated in AGENTS.md

All other stabilization rules live in the repo `AGENTS.md` and `tools/naming_check`.

- Provider login summaries and server/system discovery never contain the user ID or password.
- The live group-id index lives in `StoredState`, changes atomically with every owner group
  transition and is seeded on recovery; open-order projections never scan terminal order history.
- Every data root, production included, has exactly one process owner; vault namespaces are isolated
  per validation root.
- The stall threshold for an unknown session is 300 seconds.
- The trading owner bounds ticket interest at 64 routes and combined quote interest at 256.
- The `eligible_after` fence on restored orders never compares provider generations across processes.
- A reverse still waiting for flat at restart is dropped with an `InterruptedReverse` recovery notice.
- A catalog search may carry an exchange filter validated by the coordinator; a replaced queued
  search is rejected as `SupersededSearch` and a lost session as `DispatchUnavailable`.
- Rithmic clocks stay in their own fields: gateway `ssboe/usecs` only as `provider_unix_nanos`,
  exchange `source_*` only as `exchange_unix_nanos`, and `jop_*` is never published. No clock stands
  in for a missing one. Live trades meet history on `QualifiedTimestamp::history_clock_unix_nanos`.
- A refused DBO snapshot never ends the session or publishes its image; it spends the bounded
  resnapshot budget and then disables order-level depth for that session.
- The desktop demands `MarketStream::OrderLevels` only for a shown Queue column on the docked book.
  Provider caps: Rithmic 8 instruments per session, Hyperliquid 0. A duplicate or unknown order ID or
  a refusal also empties the view. Views keep at most 32 prices per side and 64 orders per price, and
  are delivered only in-process inside the fenced order-book snapshot.

## 6. Known gaps and deferred work

Outside the work items; each needs a maintainer decision or access before it can move.

- **Maintainer-only checks:**
  - logoff flush (M8.3)
  - VM install and uninstall (M8.6)
  - Rithmic login through the native vault (M6.9)
  - real Cognito sign-in
  - update and rollback on an installed app
  - macOS signing and notarization
  - code-signing certificate and key custody
  - Rithmic live qualification and conformance approval before production order routing
- **Shutdown retirement warning:** after a graceful window close,
  `market consumer could not be removed during retirement` was logged twice
  (`engine_market_worker/runtime.rs::retire_endpoint`). Investigate retirement ordering and
  idempotence; never suppress the warning.
- **Out of scope by D3:** depth-aware partial fills, slippage, margin and buying power, currency
  conversion, and the Funding/MarginReserve ledger kinds (hooks only).
- **Old P&L data:** the `trade_pnl_cycles` and `risk_trade_cycles` tables and the per-fill P&L
  columns stay, unread, until a later removal migration.
- **Legacy migrations (LC-17, PL-16 migrations part):** the legacy data-root migration and the vault
  legacy-name probes stay until the maintainer confirms their machine is migrated.
- **Risk profile UI (TR-09 desktop part):** risk profile registration is not built, and no desktop
  code selects the futures `ChicagoClose` boundary.
- **Migrated instruments without a route:** these show "feed unavailable" until selected again;
  there is no automatic route resolution.
- **Exchange holiday data:** exchange rules report Open on holidays, and the staleness rule still
  protects trading.
- **Second launch:** it does not focus the existing window (that needs IPC).
- **Aeris Charts order-flow append:** resolved by M7.11 without an engine change: the existing
  append API applies one print in ~2.5 ms (debug), inside a frame budget; only covering-window
  installs walk the full tape, and those are selection/session-level events.
- **Workspace-level account selection:** the per-pane selection is kept.
- **Broker relay edge cases:**
  - the reverse 30-second deadline has no deterministic test
  - an exclusive lane stays held after a timeout until the reconcile
  - a late cancel-all reply matches the next pending one
  - none of it has run against a live Rithmic gateway
- **Rithmic catalog:**
  - search and listing cache are tested only against a fake session
  - the listing keeps at most 128 products and is not refreshed in a session
  - an in-flight search cannot be cancelled
  - exchanges with spaces in their names cannot be exchange scopes
  - failed searches are not retried
- **Rithmic DBO and order levels:**
  - trade order IDs and implicit BBO sizes are not in the neutral contracts yet
  - JOP stays evidence only
  - begin/middle/end, clear and no-book frames were not observed live
  - the Queue column has not run against a live DBO stream
  - the 8-instrument budget is a default, not a measured limit
  - no queue position, iceberg or sweep detection is derived
  - raw-event persistence and book replay are unsupported
- **`Live market gates` workflow:** it stays `disabled_manually`; no agent changes GitHub Actions
  settings.

## 7. Release record

Each `feat(release)` commit adds one row here.

| Version | Date | Commit | Shipped |
|---|---|---|---|
| 1.0.0 | 2026-10-25 | 1c2c9971 | Launch release for Windows x64 and macOS Apple silicon. It shipped a per-user Inno Setup installer with silent install, launch and uninstall smoke tests, a macOS disk image with an Applications link, and clean public file names. The user agent version comes from `CARGO_PKG_VERSION`, and the plan shows as Unlimited. |
| 1.1.0 | 2026-10-27 | da4fe3d0 | Multi-drawing selection, live preview in settings dialogs and kept separate-pane heights. The EMA Ribbon, Market Structure, Fair Value Gaps and Order Blocks studies were removed. |
| 1.2.0 | 2026-10-27 | 7425b7e0 | High Contrast light and dark themes, watchlist flags (one color per symbol, with a filter by flag), and order, bracket and position lines redrawn right after a timeframe or symbol switch. A regression check covers the 2 px default line width. The earlier 1.2.0 commit 5eafba5b was never published. |
