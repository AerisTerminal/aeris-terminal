# Desktop and UI audit

Scope: `apps/desktop` (primary), `crates/ui/chart_integration` and `crates/ui/terminal_ui` (brief).
Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal`, branch `main` (4 commits ahead of `origin/main` at audit time).
Method: read-only source reading, `rg`, `git log`/`git show`. No build, test, or file modification in the repo.
History window covered by churn data: 2026-07-31 to 2026-10-13 (about 2.5 months).

## Summary

- The desktop layer is not provider-neutral. Provider id formats, entitlement strings, default contracts and venue
  wording for Rithmic, Hyperliquid and two since-removed broker providers are parsed and hard-coded in desktop code
  (`engine_market_worker/replay_conversion.rs`, `desktop/workspace_surface.rs`, `desktop.rs`, `trading.rs`). This violates
  the "desktop must stay provider-neutral" invariant directly.
- The practice (simulated) venue receives market observations only through a UI-side relay
  (`engine_market_worker/publications.rs:126`). Simulated stops, limits and brackets for instruments that are not shown in a
  foreground chart pane cannot fill. This is a correctness bug caused by an ownership violation (trading data depends on
  presentation demand).
- Chart symbol/interval switching is a hand-built state machine in the UI (`RithmicSwitchState` plus five companion fields and
  several "stale/ready" guard predicates) layered on worker messages that carry no selection generation. Each guard was added to
  fix a race found after the previous guard (for example commit `f504ab86`). This is the clearest "patch on top of patch" area.
- `WorkspaceSurface` (6,330 lines, about 100 fields) and `TerminalApp` (`desktop.rs`, 5,733 lines, about 60 fields, with
  `#[allow(clippy::struct_excessive_bools)]`) are god objects. About 45 `#[path]` sub-modules share `pub(super)` access to
  their private fields, so there is no internal encapsulation.
- The desktop market worker duplicates runtime concepts: a parallel `MarketWorkerMessage` enum, a custom coalescing mailbox, a
  2 ms busy-poll loop, five latest-value `Mutex` slots per endpoint plus a parallel channel path, and startup catalog resolution.
  This logic belongs in `market_runtime`.
- Trading state is copied into every chart pane (`TradingPnlState`) and refreshed by a 250 ms full-snapshot poll that is
  triggered from the render path (`keep_trading_pnl_fresh`). Command feedback is a global "latest message only" singleton.
- Persistence has a `schema_revision` field that is written but never read; migrations are heuristic; the atomic-write routine
  is duplicated in two files with different recovery behaviour; a corrupt workspace silently resets to defaults.
- Control flow depends on matching human-readable strings (catalog-ready message, `"RISK WARNING"` prefix), and 101 `let _ =`
  sites in production code discard results, including persisted-settings restore failures.
- About 2,100 lines of account/sign-in code are dormant in production builds (`AUTH_BACKEND_CONFIGURED = cfg!(test)`), and
  further placeholders (`CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED = false`, `let authenticated = true;`) keep dead branches alive.
- Churn confirms the structural problems: the five largest non-test desktop files account for 549 commits, 218 of which are fixes
  (about 40%). Tests (122 `#[test]` functions in the two main desktop test files) frequently pin private implementation details,
  which raises the cost of every fix.

## Churn hotspots

Counts are commits touching the file over the history window; "fix" counts commits whose subject starts with `fix`.

| File | Lines | Commits | Fix commits | Fix ratio | Notes |
|---|---:|---:|---:|---:|---|
| `apps/desktop/src/desktop.rs` | 5,733 | 183 | 72 | 39% | Composition root, `TerminalApp`, switch state, catalog autoload |
| `crates/ui/chart_integration/src/view.rs` | 3,261 | 169 | 67 | 40% | Chart glue, order-flow projection, hollow candles |
| `apps/desktop/src/desktop/workspace_surface.rs` | 6,330 | 123 | 52 | 42% | Pane god object, switch guards, legacy colour migration |
| `apps/desktop/src/desktop/chart_context_menus.rs` | 2,700 | 89 | n/a | n/a | Menu fixes recur (menus/tabs repeatedly fixed) |
| `apps/desktop/src/desktop/workspace_tabs.rs` | 3,365 | 88 | 40 | 45% | 139 functions; frame poll, tabs, menus, account UI |
| `apps/desktop/src/engine_market_worker.rs` (+ dir) | 2,874 + 2,101 | 87 | 38 | 44% | Runtime adapter, selection, publications |
| `apps/desktop/src/market_worker.rs` | 3,311 | 68 | 16 | 24% | Mailbox, slots, fixture worker |
| `apps/desktop/src/desktop/local_state.rs` | 1,582 | 42 | n/a | n/a | Persistence format and migrations |
| `crates/ui/terminal_ui/src/order_book_view.rs` | 3,117 | 33 | n/a | n/a | Order book / ladder view |
| `apps/desktop/src/account.rs` | 2,152 | 32 | n/a | n/a | Dormant in production |

Observed fix clusters: the watchlist was fixed five times on 2026-09-13; chart menus and tab behaviour received repeated
same-day fixes; switch/selection races were fixed in `f504ab86` and `1c9375a4`; legacy persisted-colour detection was patched
in `5391ce2d` by inferring legacy state from a missing field.

## Size and responsibility map

| File | Lines | Responsibilities | Proposed split |
|---|---:|---|---|
| `apps/desktop/src/desktop/workspace_surface.rs` | 6,330 (197 decls) | Chart pane state, order book, trade tape, time and sales, context panel, link groups, menus and overlay animation, timeframe flyout, symbol browser, switch state machine, studies, price alerts, persistence dirty flags, trading P&L copy, diagnostics, legacy colour migration | `ChartPaneController` (binding, generation, publication apply), `OrderFlowPresenter` (book/tape/T&S), `StudiesController`, `AlertsController`, `PaneChromeState` (menus, flyouts, symbol browser), `PanePersistence` (snapshot mapping only). Trading P&L removed (see UI-08). |
| `apps/desktop/src/desktop.rs` | 5,733 (306 decls) | `TerminalApp` struct (about 60 fields), about 45 `#[path]` modules, startup, symbol submit, catalog autoload, risk-warning toasts, shutdown/quiesce, instrument labels | Keep only composition root and `TerminalApp` wiring. Move symbol entry to a `SymbolEntry` component, shutdown to `lifecycle`, labels to runtime-provided descriptors. |
| `apps/desktop/src/desktop/tests.rs` | 4,213 (96 tests) | Mixed unit tests of private helpers and integration-style flows | Split per controller; test behaviour through public controller APIs. |
| `apps/desktop/src/desktop/workspace_tabs.rs` | 3,365 (139 fns) | Tabs, drag, window activation, frame poll scheduling, chart context/settings menu assembly, account menu UI | `FrameScheduler` (frame poll + wake), `TabStrip`, `ChartMenus` assembly; delete account UI branches. |
| `apps/desktop/src/market_worker.rs` | 3,311 (about 1,570 prod) | `MarketWorkerMessage` enum, coalescing mailbox, latest-value slots, channel path, `FixtureMarketWorker` | Replace with a thin runtime consumer handle; move fixture worker to `#[cfg(test)]` or a test-support crate. |
| `apps/desktop/src/engine_market_worker.rs` + `engine_market_worker/` | 2,874 + 2,101 | Worker thread loop, runtime event translation, selection commands, startup catalog resolution, replay/provider id parsing, simulated-venue relay | Most moves into `market_runtime` (select-and-demand, catalog resolution, descriptors). The relay is deleted (UI-01). What remains is an adapter of under 500 lines. |
| `apps/desktop/src/desktop/chart_context_menus.rs` | 2,700 | Context, price-axis and settings menu rendering | Split by menu; menu state into `PaneChromeState`. |
| `apps/desktop/src/account.rs` | 2,152 | Dormant account/auth runtime glue | Remove or put behind a cargo feature until a backend exists (UI-12). |
| `apps/desktop/src/desktop/drawing_settings_dialog.rs` | 2,126 | Drawing settings UI | Acceptable size for a dialog; check for duplicated colour/stroke controls with chart settings. |
| `apps/desktop/src/components/terminal_view.rs` | 1,810 | Root render, overlays, fullscreen, header/bottom panel | Overlay stack into its own module; remove render-path side effects (`keep_trading_pnl_fresh`). |
| `apps/desktop/src/toaster.rs` | 1,802 | Toast queue and rendering | Low churn (4); leave. |
| `apps/desktop/src/desktop/local_state.rs` | 1,582 (tests from line 709) | Protobuf workspace format, atomic write, legacy contract migration | Shared `DurableFile` + versioned migration chain (UI-09). |
| `apps/desktop/src/trading.rs` | 1,267 | Trading command handle, global feedback, simulated-observation relay, flatten feedback text | Command handle only; feedback per command; relay deleted. |
| `apps/desktop/src/chart_chrome.rs` | 1,243 | Chart chrome UI plus its own key=value persistence and atomic write | UI only; persistence via shared `DurableFile`. |
| `apps/desktop/src/components/workspace_layout.rs` | 1,031 | Pane grid, trading refresh/poll, P&L projection | Layout only; trading store moves to workspace level (UI-08). |
| `crates/ui/chart_integration/src/view.rs` | 3,261 (+5,431 tests) | Chart view glue, legend, hollow candle transform, order-flow state, pending host requests | Move hollow transform and sweep classification to Aeris Charts / study engine (UI-14). |
| `crates/ui/terminal_ui/src/order_book_view.rs` | 3,117 | Order book and ladder view | Not deeply audited; no hardcoded colours found in `terminal_ui`. |

## Findings

### UI-01: Practice venue market data is relayed through the chart UI

- **Severity:** High
- **Category:** Correctness bug / architecture (ownership)
- **Evidence:**
  - The only producer of `SimulatedMarketObservation` is
    `apps/desktop/src/engine_market_worker/publications.rs:126`, which calls
    `aeris_desktop::trading::publish_simulated_market_observation(&frame)` inside `apply_order_book_snapshot`.
  - `crates/market_runtime/src/market_service/publication.rs:625-674` (`publish_order_book_to_consumer`) publishes order books only to
    consumers whose `resource_class.publishes_ui()` is true and that hold series demand.
  - The market mailbox drops market publications while a pane is in the background (`set_market_publications_enabled`).
  - Selection commands also call `register_provider_instrument` on the trading runtime from desktop worker threads
    (`engine_market_worker/runtime.rs:300`, `engine_market_worker/selection_commands.rs:227`).
- **Root cause:** The simulated venue needs a BBO feed, but rather than adding a runtime-to-runtime subscription, the feed was
  piggy-backed on chart order-book publications in the desktop.
- **Impact:** Working simulated orders (stops, limits, brackets) on an instrument that is not visible in a foreground chart never
  fill or trigger. Fill behaviour depends on which tab is active.
- **Proposed clean fix:** `trading_runtime` declares market demand for every instrument with working orders or positions;
  `market_runtime` delivers BBO/trade observations directly to `trading_runtime` in-process, independent of UI resource classes.
  Delete `publish_simulated_market_observation` and the desktop `register_provider_instrument` calls.
- **Verification:** Integration test: place a simulated stop on instrument A, show only instrument B, push trades through A in a
  fixture provider, assert the stop fills. Add a `tools/naming_check` assertion that `apps/desktop` does not reference
  `SimulatedMarketObservation`.

### UI-02: Symbol/interval switching is a UI-owned state machine without generations

- **Severity:** High
- **Category:** Patch-on-patch / architecture
- **Evidence:**
  - `RithmicSwitchState` (`Idle/Pending/Swapping/Initializing`) in `apps/desktop/src/desktop.rs:1560-1829`, used for every provider despite
    the name, with companion fields `rithmic_pending_interval`, `rithmic_pending_product`, `rithmic_pending_sequence`,
    `rithmic_previous_selection` on `WorkspaceSurface`.
  - Guard predicates `ready_state_can_complete_switch`, `stale_settled_state_during_engine_switch`, `awaits_replacement_chart`
    (`desktop/workspace_surface.rs:3334-3493`, `3830-4229`).
  - Commit `f504ab86` added `awaits_replacement_chart`, moved `rithmic_switch = Initializing` ahead of tape projection, and added 206
    test lines for that single guard.
  - `MarketWorkerMessage::State { state, message }` carries no selection generation; ordering is inferred from an
    `EngineSwitchMarker` that is sent with `let _ =` (`engine_market_worker/selection_commands.rs`, around line 452). If the send
    fails, the pane stays in `Pending`.
  - Trade tape projection uses the surface's current `product`/`interval` instead of the identity the chart was bound to
    (`apply_trade_tape_to_chart`, `workspace_surface.rs:4619-4713`).
- **Root cause:** Messages are not fenced by generation, so the UI has to reconstruct ordering from side signals. Each newly observed
  race adds a guard.
- **Proposed clean fix:** Every worker/runtime message carries the selection generation that produced it. Introduce
  `ChartBinding { instrument, interval, generation, chart }` owned by a `ChartPaneController`. Drop messages with a retired
  generation in one place on receipt. Delete `RithmicSwitchState`, the pending fields and all three guard predicates. This also
  satisfies the invariant "retired generations must never mutate current state".
- **Verification:** Property-style test that interleaves state, snapshot and tape messages from generation N and N+1 in random order
  and asserts the pane only ever renders N+1 data after the switch. Delete the guard-specific tests.

### UI-03: Provider-specific identity, entitlement and wording in the desktop

- **Severity:** High
- **Category:** Architecture (provider neutrality)
- **Evidence:**
  - `engine_market_worker/replay_conversion.rs` `snapshot_instrument` parses Rithmic, Hyperliquid and removed-provider id formats;
    `validate_product_identity` hard-codes entitlement strings (`rithmic-test:`, `hyperliquid-public` and the
    removed providers' `-authorized` strings).
  - `desktop/workspace_surface.rs:4882-4890` builds `rithmic-test:{exchange}:{symbol}` entitlements and `provider: "rithmic"`.
  - `worker_identity` branches on Hyperliquid; `default_product` hard-codes Rithmic MNQ/ES; `default_rithmic_contract_index` matches
    `"MNQ"` (`desktop.rs:737`).
  - `TerminalProvider` enum enumerates four providers; `trading.rs` flatten feedback names a removed broker in its close-request text;
    `local_state` default workspace is Hyperliquid; `rithmic_shell`/`rithmic_login` components live in the desktop.
  - `terminal_instrument_label`'s `provider_presentation` check is effectively always true.
- **Root cause:** The runtime does not publish presentation metadata or entitlements, so the desktop reconstructs them from id strings.
- **Proposed clean fix:** `market_runtime` publishes an `InstrumentDescriptor` (venue, display symbol, asset class, currency, tick,
  entitlement token, default-contract policy) and a provider registry descriptor (display name, login form schema). The desktop
  renders descriptors only. Default product comes from the runtime's catalog. Provider-specific login UI becomes a
  descriptor-driven form.
- **Verification:** Extend `tools/naming_check` to reject provider names (`rithmic`, `hyperliquid` and every other registered provider) and
  entitlement literals in `apps/desktop/src` outside an explicit allow-list (for example, vault/login glue pending migration).

### UI-04: Symbol selection takes two round trips and startup catalog resolution lives in the desktop

- **Severity:** Medium
- **Category:** Architecture / maturity
- **Evidence:** The UI sends `ProviderSelect`, waits for `SelectionInstalled`, then calls `try_select_engine`
  (`engine_market_worker/selection_commands.rs:50-529`). `StartupResolution` and `handle_startup_catalog_event` resolve the persisted
  instrument against the catalog inside the desktop worker.
- **Root cause:** Selection and demand were added as separate runtime operations, and the desktop orchestrates them.
- **Proposed clean fix:** A single runtime command, `select_and_demand(binding_generation, instrument_ref, interval)`, that resolves the
  instrument (including startup catalog resolution), installs demand and replies with one generation-tagged result.
- **Verification:** Runtime-level test for persisted-instrument resolution at startup; desktop test reduced to "sends one command, applies
  one reply".

### UI-05: Busy-polling worker loop and duplicated slot/channel command paths

- **Severity:** Medium
- **Category:** Scalability / clean code
- **Evidence:** `engine_market_worker/runtime.rs` loops with `try_recv` on additions and commands per endpoint and sleeps `EVENT_WAIT`
  (2 ms) between passes. `market_worker.rs` keeps five latest-value `Mutex` slots per endpoint (resource class, depth, provider
  selection, engine selection, price alerts) and also a channel path; every `try_*` method has both a slot and a channel branch with
  repeated `map_err` and `unreachable!` boilerplate.
- **Root cause:** No waitable outbox from the runtime, so the worker polls; coalescing was added per command type as overload appeared.
- **Proposed clean fix:** The runtime exposes a waitable consumer outbox (condvar or async notify). One command lane per endpoint with an
  explicit coalescing policy enum (`Latest`, `Queue(bound)`). Delete the slots and the 2 ms sleep loop.
- **Verification:** Measure idle CPU with N panes before and after (the 2 ms loop wakes 500 times per second per worker). Unit tests for
  coalescing policy and overflow.

### UI-06: Parallel message model and bespoke mailbox in the desktop

- **Severity:** Medium
- **Category:** Bloat / duplication
- **Evidence:** `MarketWorkerMessage` re-declares runtime events for the UI; the mailbox in `market_worker.rs` has per-variant coalescing
  rules (`StudyOutput`, `OrderBook`, `TradeTape`, `Diagnostics`, `CONTROL_RESERVE = 8`). `FixtureMarketWorker` is compiled into the
  production module. `publications.rs` contains `expect("recovery was installed…")` at line 428.
- **Root cause:** The desktop worker grew as a translation layer rather than a thin consumer of runtime publications.
- **Proposed clean fix:** Consume runtime publication types directly (they already carry revisions); keep only a bounded, latest-wins
  presentation inbox per pane keyed by publication kind. Move fixtures to test support. Replace the `expect` with an explicit error.
- **Verification:** Mailbox overflow tests at the runtime boundary; `rg FixtureMarketWorker` finds only test code.

### UI-07: `WorkspaceSurface` and `TerminalApp` are god objects with no internal encapsulation

- **Severity:** High
- **Category:** Maintainability / maturity
- **Evidence:** `WorkspaceSurface` (`desktop/workspace_surface.rs`, struct in `desktop.rs:796-1164`) has about 100 fields covering every
  pane concern; `WorkspaceSurface::new` needs `#[allow(clippy::too_many_lines)]` (`workspace_surface.rs:90`). `TerminalApp` has about
  60 fields and `#[allow(clippy::struct_excessive_bools)]` (`desktop.rs:4954`), including patch flags such as `swallow_pane_mouse_up`.
  About 45 `#[path]` modules under `desktop.rs` all read and write private fields through `pub(super)`.
- **Root cause:** Features were added as fields and free functions on the nearest large struct; no controller boundaries were introduced.
- **Proposed clean fix:** Extract controllers with private state and narrow APIs: `ChartPaneController`, `OrderFlowPresenter`,
  `StudiesController`, `AlertsController`, `PaneChromeState`, `PanePersistence`; at the app level `FrameScheduler`, `SymbolEntry`,
  `OverlayStack`. Replace boolean clusters with enums. Remove the lint allows once the structs shrink.
- **Verification:** Clippy passes without the two allows; each controller has its own test module; churn per file drops in follow-up history.

### UI-08: Trading state duplicated per pane and polled from the render path

- **Severity:** Medium
- **Category:** Architecture / scalability
- **Evidence:** `TradingPnlState` on every `WorkspaceSurface` copies accounts, orders, positions, broker positions, risk and
  `selected_account_id`. `keep_trading_pnl_fresh` is called during render (`components/workspace_layout.rs:416`,
  `components/terminal_view.rs:882`) and spawns a full `service.snapshot()` every 250 ms (`workspace_layout.rs:715-800`), clones the
  snapshot into every pane, and re-arms a timer that calls `cx.notify()` (forcing a re-render) every 250 ms even when nothing changed.
  `aeris_desktop::trading::latest_feedback()` is a global singleton holding only the latest message, so a later message overwrites an
  earlier error. `let _ = terminal.update(...)` discards update failures.
- **Root cause:** No revision-based subscription from `trading_runtime`; polling was the quickest way to keep panes consistent.
- **Proposed clean fix:** `trading_runtime` exposes a revisioned change stream (or a waitable revision counter plus snapshot). One
  workspace-level `TradingStore` entity subscribes, and panes read projections from it. Command feedback is returned per command
  (correlated by request id) and shown as toasts or in the order ticket. Remove the render-path side effect.
- **Verification:** Test that a fill produces exactly one store update and one notify; idle workspace produces zero trading notifies.

### UI-09: Persistence lacks versioning, duplicates atomic-write logic, and silently resets on corruption

- **Severity:** Medium
- **Category:** Durability / long-run correctness
- **Evidence:**
  - `schema_revision` is set to 1 but never checked on load.
  - Migrations are heuristic: `LEGACY_SERIES_DEFAULT_COLORS` (`workspace_surface.rs:524`), commit `5391ce2d` infers legacy state from a
    missing area-bottom field, and `legacy_*_contract` functions in `local_state.rs`.
  - Atomic write (tmp, rename, backup deletion) in `desktop/local_state.rs:175-213` is duplicated almost verbatim in
    `chart_chrome.rs:762-830` with a different format (protobuf vs key=value text). The chrome loader does not try the `.tmp`/backup
    file and silently falls back to defaults.
  - The persistence worker does read-modify-write (`load_workspace`, then overwrite fields; `workspace_persistence.rs:278`) with a
    100 ms debounce. No checksum. The backup is deleted after a successful write, so there is no last-good copy.
  - A corrupt file produces only a diagnostic and the default workspace; user layout is lost without a visible message.
- **Root cause:** Each persisted surface grew its own file handling; no migration framework.
- **Proposed clean fix:** One `DurableFile` helper (write tmp, fsync, rename, keep last-good backup, verify checksum, fall back to
  backup on read failure). Explicit `schema_version` with an ordered migration chain and tests per version step. Writer owns the full
  document in memory instead of read-modify-write. Surface restore failures to the user with an option to open the backup.
- **Verification:** Tests for: truncated file restores backup; unknown future version refuses to overwrite; each migration step;
  crash between tmp write and rename.

### UI-10: Control flow depends on matching display strings

- **Severity:** Medium
- **Category:** Clean code / fragility
- **Evidence:** `should_autoload_rithmic_catalog` checks whether the connection message contains `RITHMIC_CATALOG_READY_MESSAGE`
  (`desktop.rs:3236`). Risk warnings are detected with `starts_with("RISK WARNING")` (`desktop.rs:2017`). Tests assert on the same
  message constant.
- **Root cause:** Runtime emits text where a typed event is needed.
- **Proposed clean fix:** Typed events: `ProviderEvent::CatalogReady { provider }` and `TradingEvent::RiskWarning { kind, detail }`.
  Text is produced only at presentation.
- **Verification:** `rg 'starts_with\("RISK|_MESSAGE\)'` finds no control-flow matches; tests use typed events.

### UI-11: Discarded results and fragmented error presentation

- **Severity:** Medium
- **Category:** Error handling
- **Evidence:** 101 `let _ =` in non-test desktop code (runtime.rs 17, update.rs 19, workspace_surface.rs 15, desktop.rs 12,
  selection_commands.rs 9). Notable: restoring persisted settings discards errors (`workspace_surface.rs:1761-1784`),
  `try_set_order_book_visible` (`workspace_surface.rs:5126`), `process_pending_price_alerts`, trading/context shutdown results.
  Dead parameters silenced with `let _ = provider;` / `selection_pending` (`desktop.rs:3206`, `3680`). Production `expect`/`unwrap` at
  `components/terminal_view.rs:1500`, `engine_market_worker/publications.rs:428`, `command_registry.rs:613`,
  `desktop.rs:3763/5619/5625/5642`. `retain_account_refresh_quiesce_for_exit` parks a thread forever on failure
  (`desktop.rs:302-341`). Errors reach the user through separate per-pane strings (`indicator_message`, `symbol_message`,
  `price_alert_message`, `chart_state_message`).
- **Root cause:** No shared error-reporting path; individual fixes chose local strings or discarded results.
- **Proposed clean fix:** A single `UserNotice` channel (severity, source, message, optional action) rendered by the toaster or the pane
  status area. Replace `let _ =` on fallible operations with explicit handling or a logged diagnostic; remove dead parameters; replace
  production `expect` with errors.
- **Verification:** Clippy `let_underscore_must_use` / `let_underscore_future` at deny level for `apps/desktop`; count of `let _ =`
  in production code tracked downward.

### UI-12: Dormant account and placeholder code compiled into production

- **Severity:** Medium (bloat; not a runtime risk)
- **Category:** Dead code
- **Evidence:** `AUTH_BACKEND_CONFIGURED = cfg!(test)`, `MANAGE_PROFILE_URL = ""`; `DesktopAccount::install` is called only in tests,
  so `DesktopAccount::shared()` is always `None` in production. Account branches remain in `workspace_tabs.rs` (around lines
  1716-3253 and in the frame poll and wake listener at 3118-3160), `terminal_chrome`, and `platform_menu`. `account.rs` is 2,152 lines.
  `CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED = false`, `current_plan_limits` is a placeholder, and `let authenticated = true;` in
  `schedule_market_frame` (`workspace_tabs.rs`) keeps an always-true branch.
- **Root cause:** Auth was disabled by flag instead of being removed or feature-gated (AGENTS.md says not to restore sign-in until a
  backend exists).
- **Proposed clean fix:** Move account integration behind a cargo feature (`account-backend`) that is off by default, or delete it and
  recover it from Git when the backend exists. Remove the `authenticated` constant and plan-limit placeholders.
- **Verification:** Default build contains no `DesktopAccount` references; feature build still compiles in CI.

### UI-13: Hardcoded colours outside the design system

- **Severity:** Low
- **Category:** CSS.md / design tokens
- **Evidence:** `crates/ui/chart_integration/src/view.rs:65` `SESSION_PLAN_LEVEL_COLOR = Color::rgb(245, 166, 35)`;
  `color_picker.rs:31` swatch palette and default `"#335CFF"` (line 171); legacy colours at `workspace_surface.rs:524` (migration
  only); an `rgba` literal at `chart_context_menus.rs:2585`. The picker's HSLA spectrum gradient is acceptable. `terminal_ui` has no
  colour literals.
- **Proposed clean fix:** Add tokens for session-plan level and the swatch palette in `design_system` (and mirror per AGENTS.md);
  replace the `rgba` literal with a token. Keep legacy colours only inside the migration step (UI-09).
- **Verification:** Extend `tools/naming_check` to reject `Color::rgb(`, `rgba(` and hex literals outside `design_system`, the colour
  picker spectrum and migration modules.

### UI-14: Chart analytics and transforms in the integration glue

- **Severity:** Medium
- **Category:** Architecture (chart_integration is glue only)
- **Evidence:** Input routing is compliant (`chart_integration/src/view/input.rs` uses the engine `input_*` controller; `naming_check`
  enforces the primitive ban). However: hollow-candle transformation (`hollow_appearance`, `hollow_base`) is done host-side in
  `view.rs`; `classify_order_flow_sweeps` (`view/order_flow.rs:588`) runs analytics on the UI thread for every tape revision;
  the full trade tape is re-sent each revision and diffed by ordinal in the UI; host polls `pending_*` request fields on
  `AerisChartView` (context menu, activate, settings, study remove) instead of receiving events.
- **Proposed clean fix:** Hollow candles become an Aeris Charts series style. Sweep classification moves to the study/order-flow engine
  in `market_runtime` (published as a derived series). Tape publications become incremental (append since ordinal, with generation).
  Request fields become emitted events from Aeris Charts (two-repository change per AGENTS.md).
- **Verification:** Benchmark tape apply with 100k trades before and after; `naming_check` rule for `classify_order_flow_sweeps` /
  hollow helpers in `chart_integration`.

### UI-15: Tests are coupled to implementation details

- **Severity:** Medium
- **Category:** Maintainability / verification
- **Evidence:** `desktop/tests.rs` (4,213 lines, 96 tests) and `workspace_surface_tests.rs` (1,574 lines, 26 tests); 20 references in
  `desktop/tests.rs` to switch internals (`RithmicSwitchState`, `rithmic_switch`, `should_autoload_rithmic_catalog`,
  `stale_settled_state_during_engine_switch`, `ready_state_can_complete_switch`). `f504ab86` added 206 test lines for one guard.
  `chart_integration/src/view/tests.rs` is 5,431 lines against a 3,261-line module.
- **Root cause:** Tests were written against private helpers because there are no controller boundaries to test against.
- **Proposed clean fix:** After UI-02 and UI-07, rewrite tests against controller behaviour (message in, rendered projection out),
  using shared fixture builders. Delete tests of removed guards.
- **Verification:** Refactors inside a controller no longer require test edits; test count per behaviour, not per guard.

### UI-16: Frame loop does unrelated work on every frame

- **Severity:** Low
- **Category:** Scalability / clarity
- **Evidence:** `schedule_market_frame` (`desktop/workspace_tabs.rs:3146-3205`) polls account, persistence, market summaries,
  screener and every pane's worker, then persists the layout, on each `on_next_frame`. A second 100 ms timer loop polls the updater
  and account. `poll_market_worker` also polls context snapshots and dispatches recovery, search and studies per frame
  (bounded by `MARKET_MESSAGES_PER_FRAME`).
- **Proposed clean fix:** A `FrameScheduler` that drains only sources that signalled a wake (per-source dirty bits set by the wake
  notifier), and a separate low-frequency maintenance tick for updater/persistence.
- **Verification:** Count `poll` calls per frame with idle panes (target: zero work when no source is dirty).

## Recommended restructuring

Ordered; each step lists what it depends on.

1. **Generation-tagged messages and `ChartBinding` (UI-02).** Add generation to every runtime/worker message; drop stale messages in
   one place; delete `RithmicSwitchState` and its guards. No dependencies. Highest leverage for the "patch on patch" problem.
2. **Runtime-to-runtime feed for the practice venue (UI-01).** `trading_runtime` demands instruments with working orders;
   `market_runtime` feeds it directly; delete the desktop relay and `register_provider_instrument` calls. Independent of step 1;
   fixes a live bug, so it can run in parallel.
3. **Instrument and provider descriptors from the runtime (UI-03, UI-10).** Publish presentation metadata, entitlements, typed
   catalog-ready and risk events; delete provider parsing in the desktop; add the `naming_check` provider-literal rule.
   Depends on step 1 for binding identity.
4. **Single select-and-demand command and waitable outbox (UI-04, UI-05, UI-06).** Move startup catalog resolution into the runtime;
   replace slots and busy-poll with a waitable outbox; consume runtime publication types directly; move fixtures to test code.
   Depends on steps 1 and 3.
5. **Workspace-level trading store (UI-08).** Revisioned subscription from `trading_runtime`; per-command feedback; remove the
   render-path poll. Depends on step 2 for the trading runtime changes.
6. **Split `WorkspaceSurface` and `TerminalApp` into controllers (UI-07, UI-16).** Easier once steps 1, 4 and 5 have removed the
   switch, worker and trading fields. Introduce `FrameScheduler` here.
7. **Unified durable persistence with explicit versions (UI-09).** `DurableFile` helper, migration chain, last-good backup,
   user-visible restore errors. Independent; can run after step 6 so `PanePersistence` is the single mapping point.
8. **Remove or feature-gate dormant account code and placeholders (UI-12) and consolidate error presentation (UI-11).**
   Independent; small; can be done at any time, ideally before step 6 to shrink the structs.
9. **Move sweeps, hollow candles and incremental tape into Aeris Charts / study engine (UI-14) and tokenise colours (UI-13).**
   Coordinated two-repository work; depends on step 1 for generation-tagged incremental tape.
10. **Rewrite desktop tests against controller behaviour (UI-15).** Follows step 6; delete guard-specific tests as their code is removed
    in steps 1 through 5.
