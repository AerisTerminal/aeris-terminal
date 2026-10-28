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

53 of 90 items are fully done (2026-10-28).

| Milestone | Done | Left |
|---|---|---|
| M1 foundations | 9 / 9 | 0 |
| M2 market foundations | 10 / 10 | 0 |
| M3 market lifecycle | 12 / 12 | 0 |
| M4 trading core | 9 / 10 | 1 (GUI check) |
| M5 trading engine | 7 / 12 | 5 (GUI checks and probe evidence) |
| M6 desktop bridge | 6 / 13 | 7 (code complete; GUI checks) |
| M7 desktop structure | 0 / 12 | 12 |
| M8 lifecycle and trim | 0 / 12 | 12 |

Every open M4-M6 item has its code landed and waits only on the checks listed on its line. Most are
guarded GUI checks; one guarded GUI pass on an isolated root, with no other Aeris desktop running,
can close several items together. The M6 full gate and CI closeout are recorded; M7 and M8 follow in
order.

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
    newer-version workspace. Chart chrome with a newer schema is left untouched and its
    session saves are skipped. Tests cover a newer workspace opening read-only and refusing
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
  - Left: the writer thread owns `WorkspaceDocument` and folds in chart chrome, deleting
    `ChartChromeShutdownWait`; the chrome key=value fold moves into the chain then.
  - GUI checks: a corrupt workspace shows the notice with Open folder and the layout is not
    silently replaced; a newer-version workspace shows the read-only notice; a copied real
    workspace migrates intact.
  - Workspaces written by 1.0.0-1.2.0 must open unchanged in meaning, including saved layouts,
    watchlist flags and pane heights. Closes UI-09 and PL-18.
- [ ] M7.9 Hollow candles as an Aeris Charts series style
  - Left: add the style in Aeris Charts and push it, then bump only the `aeris_charts_*` pins.
  - Delete `hollow_appearance` and the host transform, and add a naming_check ban.
  - Compare screenshots.
  - Also decide the in-place cross-instrument data reset in Aeris Charts.
- [ ] M7.10 Sweep classification and incremental tape in `market_runtime`
  - Left: move the sweep classification into the runtime, once per publication over the same
    512-trade window.
  - Publish the tape as fenced Append/Reset events.
- [ ] M7.11 Desktop consumes runtime sweeps and incremental tape
  - Left: `OrderFlowPresenter` applies Append/Reset into a bounded ring.
  - Delete the host sweep function and ordinal diffing, and add a naming_check ban.
  - Measure a 100k-trade apply before and after.
  - GUI: Hyperliquid sweep highlights. Closes UI-14 with M7.3 and M7.9.
- [ ] M7.12 Desktop tests against controller boundaries
  - Left: rewrite `desktop/tests.rs`, `workspace_surface_tests.rs` and the chart_integration view
    tests against controllers and `PaneHarness`.
  - Map each deleted test to a behavior test, and add `test_support::providers`. Closes UI-15.

### M8 lifecycle and trim

- [ ] M8.1 Release code moves into a `release_lifecycle` crate
  - Left: move lifecycle, delivery, rollout, release identity and the launcher binary out of
    `platform_runtime`.
  - Put the signers behind a `producer` feature, and add a naming_check dependency rule.
  - Kept, not deleted (D2). Closes PL-01.
- [ ] M8.2 Launcher hygiene and trust
  - Left:
    - LC-01: dispatch commands before loading the key.
    - LC-16: promotion gating and child reaping.
    - LC-13: a retired-binary sweep.
    - LC-14: `MessageBoxW` errors.
    - LC-15: `platform_sys` process wait instead of sysinfo scans.
    - LC-20: build-validated release config.
    - LC-07: `WinVerifyTrust` with a thumbprint.
- [ ] M8.3 Remaining FFI moves to `platform_sys`
  - Left: move the power, network, session-shutdown and `CancelIoEx` FFI.
  - Delete `display_timing.rs` and `NativeSessionShutdownMonitor`.
  - Restore `forbid(unsafe_code)` in `platform_runtime`. Closes PL-02.
  - The logoff flush check is maintainer-only.
- [ ] M8.4 Account runtime vault and lock fixes
  - Left:
    - AC-02: a 2,560-byte secret cap with a precheck.
    - AC-03: vault I/O outside the state mutex, with a generation fence.
    - AC-06: recovery from a poisoned mutex.
  - AC-04 and AC-08 were closed by the Cognito sign-in work.
- [ ] M8.5 Shared readiness report
  - Left: one `ReadinessReport` for the desktop and the launcher.
  - Readiness runs on a `VACUUM INTO` copy, so the real `trading.sqlite3` is unchanged.
  - Decide the `provider_count > 0` rule. Closes LC-11 and part of LC-05.
- [ ] M8.6 Release wire protocol v1 and Axiusflow residue
  - Left:
    - LC-03: a domain-separated raw-bytes signature with `deny_unknown_fields`.
    - LC-04: `AERIS_*_V1` tokens replacing `AXIUSFLOW_*`.
    - LC-19: `LAUNCHER_PROTOCOL_VERSION`.
    - LC-08: typed block errors.
    - LC-12: a "Remove my Aeris data" uninstall choice, with the Axiusflow lines removed from
      `tools/windows/aeris_setup.iss`.
    - A naming_check rule. Also covers the PL-16 tokens part.
- [ ] M8.7 Release packager and end-to-end lifecycle harness
  - Left: `tools/release_packager`, with the key from a file and no publishing.
  - A `ReleaseSource` seam: HTTPS in production, a directory in tests.
  - A CI test covering install, update, readiness, activation, rollback and quarantine.
    Closes LC-02 and LC-21.
- [ ] M8.8 Rollback safety
  - Left:
    - LC-05: a schema-gated rollback.
    - LC-09: a startup-complete marker and probation.
    - LC-18: readiness journaled before Activated, plus `orphaned/`.
    - LC-06: all verification before READY.
    - End-to-end tests for each.
- [ ] M8.9 Dependency direction and `contracts`
  - Left: move `EmbeddedReplaySource` to test support.
  - Delete `GetAccountStatus` and `RefreshAccountProfile`, and reduce `contracts` to the persisted
    schema.
  - Add naming_check direction rules from `cargo metadata`.
  - Record `study_sdk -> market_runtime` as the accepted facade. Closes PL-13 and PL-14.
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
- **Aeris Charts order-flow append:** add it only if M7.11 measures it as the bottleneck.
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
