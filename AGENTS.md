# AGENTS.md

Instructions for coding agents working in the Aeris native repository.

Aeris is a local-first Rust/GPUI trading terminal. The desktop is the single application process.
In-process `market_runtime` and `account_runtime` own provider sessions, canonical market/account state,
and bounded background work; GPUI owns presentation scheduling only. A secondary local market process must not return.

## Non-negotiable core principles

Every change must satisfy all five principles below. They are mandatory acceptance criteria for
features, fixes, refactors, dependency upgrades, tests, tooling, and releases. Never compromise them
for speed, convenience, a temporary workaround, or a passing check.

1. **Proper architecture:** Keep behavior and state at their authoritative ownership boundary.
   Preserve dependency direction, single ownership, and every architecture invariant in this file
   and `tools/naming_check`. Never bypass the owner with duplicate state or parallel implementations.
2. **Clean code:** Use clear names, cohesive responsibilities, explicit errors, and the smallest
   complete implementation. Remove superseded code when replacing it; do not leave dead paths,
   speculative abstractions, or lint suppressions that hide defects.
3. **Scalable:** Keep work, memory, queues, caches, and retries bounded. Share provider sessions and
   canonical state, handle overload and cancellation explicitly, and verify relevant workloads.
   Never claim scalability or performance that has not been measured.
4. **Maintainable:** Make ownership, interfaces, data flow, and failure behavior easy to understand
   and test. Reuse existing boundaries and shared definitions; avoid hidden coupling and changes
   that require unrelated components to know implementation details.
5. **Built for the long run:** Preserve correctness, durable state, recovery, compatibility, and
   reproducible verification across restarts, upgrades, and sustained use. Prefer durable fixes at
   the responsible component over shortcuts that transfer complexity or risk to future work.

Evaluate every proposed implementation against all five principles before editing and again before
delivery. If an approach violates any principle, revise the approach; do not weaken the principle or
its checks. Existing violations are not permission to introduce or extend them. Report unresolved
violations and missing verification explicitly, and never describe them as satisfied or complete.

## Branch and Git workflow

- Work on `main` only. Do not create, switch to, or leave work on feature/continuity/scratch branches
  unless the maintainer explicitly requests a branch for that task.
- Start every task with `git status --short --branch` and confirm the repo is on `main`.
- Preserve unrelated work already present in the worktree. Never reset, checkout over, stash, or
  delete changes you do not own.
- Prefer one coherent implementation batch. Commit only after the relevant checks pass.
- Use the existing `type(scope): outcome` commit style and push verified work directly to `origin/main`.
- Never add agent or harness attribution to commits (no `Co-authored-by`, `Generated-by`, or mentions of Droid, Claude, Cursor, or any other coding agent in commit messages or trailers).
- Never force-push or use destructive Git commands.

## How to work

1. Read the real call path before editing. Identify the component that owns the behavior.
2. Make the smallest complete fix at that ownership boundary. Do not add duplicate state,
   compatibility layers, speculative abstractions, or extra services to avoid changing the owner.
3. Reuse existing code, `std`, platform facilities, and pinned dependencies before adding anything.
4. Add deterministic regression coverage for concrete bugs when practical.
5. Run focused checks while iterating, then broader gates before delivery or release.
6. For behavior that depends on persistence, provider lifecycle, rendering, updates, or the installed
   app, exercise the real path; compilation alone is not proof.
7. Edit files with the agent's native file tools (read, edit, write) so every change is a visible,
   reviewable diff. Do not write Python, `sed`, heredoc, or other scripts to modify source files.
   A script is allowed only for a genuinely bulk mechanical change that native tools cannot do
   practically; say so before running it, and make it fail when any intended replacement does not apply.

## Architecture invariants

- `MarketEngine` is the single market-demand owner inside `market_runtime`. Provider sessions are
  created and owned only by that in-process runtime; never create a session/runtime per chart or UI surface.
- Instrument quote interests and series interests share `MarketEngine`'s one bounded
  `DataLeaseRegistry`; do not add a second market-demand owner for trading or alerts.
  Provider-neutral feed contracts live in `aeris_trading::market` and depend only on
  `aeris_instruments`; `market_engine` may depend on `aeris_instruments` but never on
  `aeris_trading` or a provider adapter.
- `trading_runtime` is the single in-process owner of broker accounts, orders, order events, fills,
  positions, PnL, simulated execution and the embedded user-record store. The desktop holds command/view
  handles only; `account_runtime`, charts and UI surfaces never own or duplicate broker trading state.
- The desktop never constructs `SimulatedMarketObservation` or relays order-book prices for execution.
  `trading_runtime` consumes the market feed through its bounded inbox and its owner `QuoteBook`;
  desktop trading actions identify instruments and explicit order instructions, never authoritative quotes.
- Simulated order admission and fills use the trading owner's verified session and current-generation Live quote.
  Crypto quotes may be at most 10 seconds old and other classes at most 60 seconds old by default;
  owner-local `eligible_after` fences resting orders after acceptance and across process restarts.
- Recurring contract-hour session projection lives in `aeris_instruments::calendar::weekly_hours`;
  market status and trading DAY recovery use that one IANA-zone/DST calculation. A historical
  DAY deadline is derived only from a verified route and the acceptance-time open session;
  unresolved legacy rows stay non-executable rather than receiving a guessed deadline.
- The trading service has one named owner thread whose loop is in `service/owner.rs`; it waits for bounded commands
  or a due deadline and drives synchronous `TradingCore::handle`, `on_market`, and `on_timer`.
- Desktop trading command handles come from the instance's `DesktopServices`, not a
  process-wide `TRADING_SERVICE` or `trading::handle()`/`trading::install()` lookup.
- Desktop trading command feedback belongs to the instance's `DesktopServices`; command
  writers and pane readers use its shared handle, never a process-global
  `TRADING_FEEDBACK` or `trading::latest_feedback()` lookup.
- Desktop trading instruction and pane alert timestamps use the instance's
  `DesktopServices` clock; do not restore `trading::now()` or a process-global
  desktop clock.
- Desktop account clients and refresh quiesce use the instance's `DesktopServices` account
  owner; no process-wide `DesktopAccount::shared()` or account-service singleton lookup.
- Desktop market consumers, bridge wakes and shutdown use the instance's `DesktopServices` market
  owner; no process-wide market runtime or `shared_market_runtime()` lookup.
- Desktop workspace and chart-chrome persistence paths and chart-chrome save receipts come from
  the instance's `DesktopServices` persistence owner. No process-global chart-chrome save state
  or implicit process-wide workspace path lookup may return.
- Saved layouts are owned by the desktop `LayoutLibrary` (bounded at 100; creating past the
  bound is refused, never evicts a user layout). Every workspace tab shows one saved layout, and
  a layout's chart drawings are one per-instrument `LayoutDrawings` set shared by every pane of
  every tab showing it. Panes publish committed edits into that set and import its newer
  revisions; panes never own or remember drawings, and no pane-to-pane drawing sync may return.
  Pane chart state persists no drawings. A layout with autosave off changes only on an explicit
  save and reopens at its last saved version after a restart.
- Trading orders and their client, broker, open-scope and executable indexes are owned together by
  `book::OrderBook`; inserts, status replacements and removals must update those indexes through that owner.
- Simulated fill price and quantity policies belong only to `trading_runtime::execution`;
  the current `TouchPrice` and `TouchLiquidity` implementations use the touch and full order quantity.
- Broker order submissions and transitions reserve capacity in the bounded venue request queue before
  committing their SQLite facts; the relay sees the request only after a successful commit. A full
  or disconnected queue rejects without a database write or compensating undo.
- Trading owner timestamps come from the injected `trading_runtime::clock::Clock`, with wall time clamped
  monotonically per process. Direct system time in trading production code belongs only in `SystemClock`.
- Symbol, timeframe, viewport, tab, and layout changes must not tear down a healthy provider session.
- `market_runtime` is the single owner that merges on-demand history and live state. Preserve one
  canonical forming candle, contiguous completed history, exact generation fencing, and explicit recovery.
- Retired clients, generations, selections, sessions, and publications must never mutate current state.
- `desktop_market_fences_stay_in_accept` scans desktop production for epoch/generation
  comparisons. Pane selection and publication acceptance belongs to `ChartPaneController`;
  unrelated release, app-command and receipt comparisons have exact reasoned, count-checked
  exceptions in `tools/naming_check`. An added comparison or a stale exception fails the gate.
  Failed pane replacements re-adopt the prior instrument and interval through the controller's
  restore target, not a provider-named workspace field. The independent market-summary worker
  checks its initial descriptor generation separately, at the exact reasoned `desktop.rs` exception.
- Production queues, caches, retries, and background work remain bounded with explicit overflow and
  cancellation behavior.
- The desktop must stay provider-neutral. It does not own provider adapters, storage implementations,
  or canonical candle state.
- Desktop provider identities are opaque runtime IDs looked up in
  `market_runtime::published_provider_descriptors()` and
  `built_in_provider_presentations()`. The runtime test
  `every_registered_provider_has_a_complete_published_descriptor` covers every registration;
  `tools/naming_check` rejects a desktop production `enum TerminalProvider` or
  `FALLBACK_TERMINAL_PROVIDER`. Only descriptors with `symbol_browser` enabled enter the
  symbol menu. An unregistered stored provider shows `UnsupportedProvider`, never another
  provider's market.
- `desktop_source_is_provider_neutral` scans production desktop and chart-integration Rust source
  for provider names and provider-specific ID formats; `Logarithmic` and `logarithm` are not
  provider identities. Exact fixture module paths compiled only under `cfg(test)` or
  `cfg(any(test, feature = "diagnostics"))` are outside the production scan, and the rule
  checks their gated declarations; moving the same identity to an ungated path fails.
  The only exception is the `assets.rs` logo mapping in `tools/naming_check`;
  no desktop provider logic may gain an exception. Replay conversion and chart legend projection
  use the bound instrument descriptor and published provider name.
  The bridge files `market_worker.rs`, `engine_market_worker.rs` and its `runtime.rs`
  need no provider-identity exceptions after M6.7; the deleted connection shell
  has no exception either. The final production exception is the logo mapping in `assets.rs`.
- Provider attribution marks and About notice lines come from published presentation
  descriptors only while the matching provider session is held, including recovery.
  Only `assets.rs` maps their logo keys to bundled light/dark artwork. Broker
  ticket venue names come from the matching broker venue's provider presentation.
- A fresh workspace persists one chart pane without a provider or instrument.
  The bridge sends `InstrumentRequest::Default`, adopts the runtime's resolved
  selection, and shows "Choose an instrument" without losing the workspace
  when the runtime has no default. Explicit saved instruments restore as
  `InstrumentRequest::Ref`; wire fields retain their existing numbers.
  `desktop_control_flow_does_not_match_display_text` rejects desktop
  production `if`/`match` conditions comparing known notice literals and
  message constants with `starts_with`, `contains` or `==`. Use typed outcomes
  rather than branching on display text.
- `FixtureMarketWorker` lives only under `apps/desktop/src/desktop/test_support/`; the module is
  compiled only for tests or the explicit `diagnostics` feature. Default desktop builds cannot
  construct fixture market publications. The diagnostic conformance command is gated likewise.
- The desktop stores `ChartInterval` and must not construct `SeriesCadence`; interval-to-cadence mapping
  belongs to `market_runtime`.
- Market history is loaded on demand and bounded; do not restore market-history persistence or a
  second desktop-owned market-state model.
- Do not restore `apps/engine`, `local_engine_client`, a secondary market process, local process transport, or market autostart.
- UI-thread code performs no blocking network, disk, process, or shutdown work. Background workers do
  not mutate GPUI state directly.
- `unsafe` is isolated to target-specific FFI under `crates/platform_sys/src/windows/` and
  `crates/platform_sys/src/macos/`, whose blocks require documented, single-operation safety
  justifications. Until the M8.3 FFI consolidation, only the exact `platform_runtime` FFI files
  allowlisted by `tools/naming_check` are additional exceptions.
- `platform_sys` is a leaf crate. Only `platform_runtime`, `trading_runtime`, and `observability`
  may depend on it directly.
- Secret-bearing types whose names contain `Token`, `Secret`, `Password`, or `Credential` must not
  derive `Debug`, and those values must not use debug formatting. Only the exact reviewed,
  non-secret or transitively redacting wrappers in `tools/naming_check` are exceptions.
- Durable file replacement is owned by `platform_sys::fs`: staging uses a unique temporary in the
  destination directory, synchronizes file contents before replacement, and synchronizes the
  parent directory on Unix. Production code must use this writer instead of raw `fs::write` or
  `fs::rename` except at the exact move/build/platform implementation sites allowlisted by
  `tools/naming_check`. Do not add another atomic-file FFI implementation.
- Trading SQLite persistence is organized under `trading_runtime/src/store/` by table family;
  `schema.rs` retains the V1–V29 migration sequence. V20 adds nullable verified market routes;
  migrated legacy instruments remain route-less until verified Terms arrive. V21 backfills
  `fill_accounting` and `trade_cycles`; V22 adds lifecycle columns and V23 adds
  `order_groups` and `order_group_legs`. V24 drops the legacy `managed_brackets` and
  `protective_orders` tables; it is applied by the gated `finalize_group_migration` step (after
  the group backfill and the runtime conversion of awaiting brackets), never by the migration
  chain, so a failed conversion keeps its rows and retries on the next start. V25 adds the
  `ledger_entries` cash ledger, per-account `fee_schedules`, the exact position cost-basis
  columns and the denormalized instrument class/price-grid columns; its backfill runs once per
  store after the group finalize as part of `complete_post_group_migrations`. A simulated
  account's balance is the exact sum of its cash-ledger entries; only a gated pre-group store
  awaiting conversion keeps the legacy starting-equity math until its backfill. V26 adds
  `position_checkpoints`, `ledger_checkpoints` and the `equity_samples` history; retention
  folds each pruned fill prefix and its ledger entries into those checkpoints inside the
  deletion transaction, and recovery rebuilds a checkpointed position as checkpoint plus the
  retained fill suffix. Each simulated fill carries one equity sample inside its commit
  transaction, the 60-second owner timer adds a sample only when equity changed plus one at
  each risk session boundary, and a 1,440-entry in-memory ring per account is rebuilt from
  the durable history at open; retention downsamples minute samples older than seven days
  into last-of-hour rows and drops hourly rows older than a year, with both windows trailing
  the newest sample so a backward clock correction cannot mass-expire history. V27 adds
  `risk_sessions` (seeded per registered risk profile from the registration baseline) and the
  `risk_locks.kind` column; the owner timer rolls each account's session at its configured
  boundary (UTC midnight by default, 17:00 America/Chicago for futures) in one transaction that
  persists the rolled row with the realized baseline, resets the daily meter, releases only
  daily-meter locks (manual locks survive) and writes the boundary equity sample. V28 makes broker
  accounts netted futures accounts: broker fills run through the same `positions` ledger as
  practice fills, so it drops the hedged `broker_positions` table and
  `broker_deals.broker_position_id` and adds `broker_bracket_levels`, the server-held target and
  stop levels kept with their entry order. A venue position report sets only the open P&L; a
  reconcile snapshot that disagrees with the ledger rebases the position in one transaction.
  Broker accounts write no cash-ledger entries (their balance is the venue's), and a broker venue
  declares its `VenueCapabilities` (held bracket levels, client order id length) when attached.
  V29 adds `broker_unresolved_contracts` and `broker_pending_reverses`. A broker account admits no
  new risk-increasing order until the current venue session has reconciled it, nor while it holds an
  unresolved contract (a fill or position the owner could not account, which also lowers the
  reconcile replay floor); cancels, exits and flatten stay allowed. Broker mirror order IDs come from
  the owner sequence, a fill before its ack binds the unbound order by client id, and a reverse sends
  only its exit until the position is flat; a reverse still waiting at restart is dropped with a
  recovery notice. Venue refusals arrive as `VenueUpdate::OperationFailed` and become bounded owner
  `VenueNotice`s (32) shown on the desktop feedback line; every relay command has a 30-second
  deadline, and an unanswered command forces a reconcile.
  Old P&L tables and per-fill columns remain unread until their deferred removal migration.
  Store-open preflight scratch cleanup may remove only exact preflight directories containing
  solely the expected database copy and SQLite sidecars; preserve unknown contents and report
  cleanup failures through observability.
- Simulated quote-only marks update unrealized P&L in owner memory without cloning all trading
  state or writing SQLite. Retention runs only from the owner timer at startup and hourly; each
  run writes position and ledger checkpoints in the same transaction before pruning fills or
  ledger entries (a crash leaves neither), never deletes rows referenced by an open order or a
  live order group, and deletes at most 5,000 rows from each table per run. Commands, quotes
  and store open never perform retention.
- Trading consumers subscribe through `TradingService::subscribe` instead of polling snapshots:
  the owner publishes a typed `TradingEvent` carrying the bounded `WorkingSet` after every
  revision-changing step, coalesces quote-only mark changes to one publication per 100 ms with
  a trailing flush, and stays silent while idle. At most eight live subscriptions exist, each
  with a 1,024-event slot that overflows into a `lagged` resync with a fresh working set; paged
  order and closed-trade history are served by the `aeris-trading-history` thread on a
  read-only SQLite connection with at most 500-row keyset pages. Trade pages are scoped
  to one account and use exit time and exit fill ID as the stable cursor; they project
  exact per-exit P&L through the same round-trip projection as the owner, using at most
  500 fills per historical cycle prefix. The working set projects registered terms only for
  open orders, open positions and ticket interests; its newest event and fill windows each
  have a 500-row limit, and round trips use at most 500 fills plus open positions. Every
  closed round trip carries its durable exit fill ID, distinct even for partial exits of
  one cycle; an open round trip has no exit fill ID. Active
  bracket/protection views come from a live group-id index maintained with group transitions,
  never a scan of terminal group history. Broker connection, risk profile/meter, session plan
  and market observation error projections remain owner-owned. The compatibility `snapshot()`
  view is not bounded by stored-order count; retention bounds the live set.
- Desktop production trading reads use the subscription-backed `TradingStore`, never
  `service.snapshot()`, `trading_service.snapshot()` or `trading().snapshot()` on the owner.
  `desktop_trading_reads_come_from_trading_store` checks these calls under `apps/desktop/src`.
  Trading updates are applied on subscription wakes, not a render-path or 250 ms timer.
- The desktop bottom panel keeps recent fill-based round trips as its live trade head.
  The desktop currently requests historical order records through the trading service's
  read-only history worker on a background thread; they have no historical trade P&L and
  must not be presented as reconstructed round trips. The owner also exposes historical
  closed-trade pages, awaiting desktop adoption. At most four 500-order pages are retained,
  with a visible cap, and a lagged subscription resync reloads the first page.
- The `trading_runtime/test-observability` feature is enabled only by the desktop's
  dev-dependency to count calls to the instance's real `TradingService::history` API
  in idle regressions. Default production builds carry no history-read counter.
- Validation runs use `platform_runtime::data_root` isolation and an exclusive per-root instance
  lock. They must not overlap the production data root, probe production vault entries, migrate
  legacy state, write installed-app identity, promote launchers, or run updates.

Architecture assertions in `tools/naming_check` are authoritative when they are stricter than prose.
Its source scans strip only balanced `#[cfg(test)]` module bodies, so production code after a test
module is still scanned, and every allowlist entry must carry a written reason; stale entries fail
the check.

## Market-data and provider work

- Keep the four coordinator command/event characterization scenarios under
  `market_service/tests/characterization.rs`; `tools/naming_check` requires their exact names so
  lifecycle refactors cannot silently drop the baseline.
- Keep the deterministic market replay scenarios under `market_service/replay/mod.rs`;
  `tools/naming_check` requires their names so lifecycle changes retain replay coverage.
- Live market probes run only from `tools/aeris_probes` on an isolated validation root with an
  exclusive instance lock. The optional `probe` feature's bounded Hyperliquid fault control
  interrupts only its WebSocket; never use a process-wide proxy that blocks catalog HTTP.
  `paper_trading_probe` exercises the practice trading lifecycle on the public Hyperliquid feed.
- The trading owner's quote-interest revision is durable store metadata: the market coordinator
  fences stale revisions for its whole lifetime, so a trading-owner restart must resume above the
  fence, and a provider reconnect republishes trading sessions before its `Feed::Live`.
- Coordinator-owned watchdog, history retry and trade-handoff timestamps use the injected
  `market_service::clock::Clock`; direct system time belongs only in `SystemClock`.
- Provider reconnect scheduling uses the bounded `market_service::reconnect::Backoff` policy;
  the Rithmic adapter alone retries transport, while its non-transport session replacements
  wait at least one second. Never stack a coordinator transport retry on that adapter.
- Every Hyperliquid info request spends from the adapter's one process-wide weight budget
  (`hyperliquid_market/src/budget.rs`: 1,000 of the documented 1,200 weight per minute per address);
  never send an info request around it.
- Generic market-service lifecycle, history, calendar, quote, outbox, coordinator, realtime,
  runtime and publication code must not contain provider identity literals. Provider-specific
  capabilities and user-facing presentation metadata are declared with provider registration;
  provider wire aggregation belongs to its adapter, not `ChartInterval`.
- Market session and cadence decisions use the provider-neutral market calendar. Exchange
  codes share one normalization table; generic calendar code contains no provider identity
  literals and does not import an adapter-owned exchange calendar. Unknown week/month
  boundaries reject demand explicitly rather than installing a silent live series.
- Dated provider calendar updates enter the coordinator behind the current provider-generation
  fence and expire into the instrument's recurring calendar or Unknown. Session, week and month
  trade-built bars and tape trading dates use that same calendar, not fixed daily seconds.
- Provider events and history completions carry registry-minted `Epoch<ProviderTag>` identities;
  the coordinator rejects retired epochs by equality and counts their drops. Adapter wire
  generation numbers never serve as market event identities.
- Every runtime-to-consumer market event variant leaves the coordinator inside a
  `Fenced<ConsumerTag, MarketRuntimeEvent>` envelope for that registered consumer lifetime;
  selection-bound events are rejected against the exact current demand before delivery.
- Trades, aggregate depth and order levels are separate additive streams. Aggregate depth comes only
  from a provider's price-level feed, never from order-level data. `MarketStream::OrderLevels` is
  explicit consumer demand (depth demand alone never subscribes it); the coordinator admits at most
  the provider's `maximum_order_level_instruments` and reports the rest as `BudgetExhausted` without
  evicting an admitted instrument. A gap or bound violation empties the bounded `MarketOrderLevelView`
  until a covering provider image arrives, and order levels are never persisted.
- Provider catalog controls and responses pass through one bounded `ProviderCatalogChannel`
  boundary; provider-specific command translation stays with each registered channel.
- Catalog callbacks pass through coordinator-owned `CommandFences`, keyed by consumer, provider
  and command kind. Startup resolution uses its own kind, not a numeric generation sentinel;
  consumer retirement clears every pending command through `ConsumerRegistry::remove_consumer`.
  Every search and selection carries a 45-second response deadline after which the coordinator
  rejects it as timed out, and a catalog rejection names its search or selection lane explicitly;
  consumers never infer the lane from the rejection reason.
- Symbol search keeps one catalog-search slot per consumer and provider. The Rithmic catalog runs at
  most one search at a time, keeps only each consumer's latest queued search (rejecting the replaced one
  as superseded) and caches its product listing once per session generation (at most 128 products).
- Chart selections, including startup and restored instruments, resolve and install demand through
  the coordinator's `select_and_demand` command; desktop code never owns startup catalog resolution
  or interprets provider display text as a readiness signal.
- `HistoryTracker` alone owns per-series current/backfill lanes, retry deadlines, empty coverage,
  detached windows and cancellation. History requests and completions carry the same exact
  provider/demand/sequence ticket; retirement prunes the tracker, never parallel coordinator maps.
  When a trade-built series' covering request exhausts its bounded retry budget, the runtime opens
  a live-only series from the trade stream with the provider-neutral `HistoryUnavailable`
  presentation and keeps one slow probe (`HISTORY_UNAVAILABLE_RETRY_DELAY`) armed instead of
  restarting the fast retry budget; a later covering snapshot merges under the trade-built bars and
  reseeds the live handoff from canonical. An exhausted backfill lane keeps its failed cooldown
  until fresh demand.
- Candle series control and the single forming bar belong to `market_service::lifecycle::SeriesLifecycles`.
  Retired candle handlers must not return, and lifecycle transitions use the injected clock rather than system time.
- Stall recovery considers every live candle and trade series while its calendar is open;
  closed sessions never recover for silence, and unknown sessions use a longer threshold.
  A trade-built series counts a newer quote on its instrument as progress, and its threshold
  doubles after each stall recovery that no trade followed, up to eight minutes.
  Practice session checks use the same calendar for every provider.
- Verify protocol fields, precision, timestamp meaning, ordering, snapshot semantics, and recovery
  against current provider documentation or representative observed responses before depending on them.
- Keep provider identity, local ingestion order, and continuity evidence separate. Never infer sequence
  semantics from an opaque identifier or hash.
- Use fixed-point values with explicit scales and provenance. Do not round provider data merely to fit
  an existing assumption.
- Required wire fields must be validated, not silently defaulted.
- Tests should use minimal sanitized fixtures faithful to real shapes. Never store credentials, account
  data, or raw private payload dumps.
- Rithmic production credentials never enter source, chat, logs, CI, or environment files. Native vault
  storage is the credential boundary. The market runtime publishes provider-owned static login fields
  through `ConnectMethod::VaultCredentials` and exposes provider-id-keyed saved summary, sign-in,
  forget and server/system discovery; the Rithmic implementation alone verifies and writes its
  native-vault entry. Submitted field values never derive `Debug`. Live/test provider sessions must
  respect provider concurrency limits.
- Desktop connection rows come from `published_provider_descriptors()` exactly once per registered
  provider. `components/provider_login.rs` renders the published vault-login field schema and
  calls only provider-id-keyed market-runtime login methods; the desktop never imports a
  provider-specific login summary/server or writes a credential outside the runtime's vault seam.
  `tools/naming_check` keeps this component in the native-vault and rendering-boundary checks;
  its former Accounts-panel, login-form and workspace-surface identity exceptions are removed.
- Broker orders reach Rithmic only through the venue relay in `market_runtime::rithmic_venue`, started
  once by `MarketService::attach_broker_venue` on its `aeris-broker-venue` worker. It reads the same
  vault login, routes only demo logins (the Rithmic Test gateway and Rithmic Paper Trading) and refuses a
  live login before opening an order plant. Each connection attaches a new venue session generation with
  Rithmic's `VenueCapabilities`, so the owner reconciles every account after a reconnect; the desktop
  only connects the trading owner's attach and inbox handles to it.

The licensed Rithmic Provider Kit remains outside Git. The canonical local copy is
`C:\axiusflow-deps\provider-kit`; do not vendor or modify that permanent copy.

## Operating constraints

1. **Local-first, no server in the market-data path.** No Aeris server receives, stores, processes
   or relays market data, orders or account state, and no feature may depend on one.
2. **Exchange data stays on the user's machine.** Broker data is processed only in the desktop
   process. Aeris must not redistribute exchange data or its derivatives.
3. **Public non-exchange data is fetched directly from its official source**, with the user's own
   free API key where required.
4. **User-owned data is stored locally.** Optional sync uses a folder the user controls.
5. **Provider access is gated.** Features that need a gated provider (Rithmic) are designed now and
   qualified after onboarding; anything buildable on public data or the simulated venue proceeds
   first.

### Data licensing checklist

A release gate, not legal advice. Before a feature that stores, derives or transmits exchange data
ships, confirm against current CME policies, the Rithmic agreement and any broker agreements:

- Local storage of recorded trades and depth for the subscriber's personal replay and review.
- Display of derived values (queue position, detections, footprint and profile statistics).
- Alerts leaving the machine carry event descriptions, not exchange prices or quantities, unless
  explicitly allowed.
- Professional versus non-professional subscriber status.

Record the reviewed policy versions in the feature's release notes.

## Aeris Charts

Aeris Charts is a separate pinned Git dependency. Do not clone or vendor it into this repository.
Host integration belongs in `crates/ui/chart_integration`. When intentionally updating Aeris Charts, change
only the `aeris_charts_*` revisions required by that update; do not update GPUI incidentally.

Chart behavior is owned by Aeris Charts, never re-implemented here:

- `crates/ui/chart_integration` is product glue only: the market-data bridge, trading, alert, study, and
  order-flow projections from the runtimes, design tokens, legend and menu UI, and workspace persistence.
- Pointer, wheel, and keyboard input reach the chart through `aeris_charts_render_gpui::input` and the
  engine's `input_*` controller, one adapter call per GPUI listener. Never add gesture routing, hover or
  cursor arbitration, key bindings, drawing-tool mirrors, host-side drawing state (locks, selection,
  creation), brush styling, or other engine-owned state to Terminal. `tools/naming_check` rejects the
  engine's low-level interaction primitives in this crate.
- When a chart feature needs more from Terminal than data, product policy, or a platform-only effect, the
  missing operation belongs in Aeris Charts. Add it there first (a coordinated two-repository task), push
  it, then bump the pinned revision here; never compensate with Terminal-side wiring.
- After bumping Aeris Charts, check for engine capabilities Terminal is not yet using (for example a new
  controller behavior or host request) instead of assuming the pin bump alone delivered them.

## Authentication and website coordination

Desktop sign-in is mandatory and uses the Aeris Cognito user pool (`us-east-1_05pNG4EOF`, public
native client `tlnlg2amke4k2v9v2k84vvg87`, hosted sign-in at `https://auth.aeristerminal.com`). These
values are compile-time constants in `account_runtime` (`AccountServiceConfig::production()`); no
environment variable selects an issuer. The runtime discovers endpoints from the exact issuer, uses
PKCE with `scope=openid email profile` and a loopback redirect `http://localhost:<port>/callback` on
the fixed ports 47210, 47211 or 47212 (bound on both `127.0.0.1` and `[::1]`), and verifies the
RS256 ID token with `ring` against the issuer JWKS (issuer, audience, `token_use=id`, expiry, nonce).
Only the refresh token and the cached profile (`sub`, email, name, picture) are stored, in the native
vault; there is no lease, device key or entitlement, and every signed-in account shows the
"Unlimited" plan. An unreachable provider keeps a saved session signed in (Offline) with bounded backoff;
only a rejected refresh grant signs out. Sign-out revokes the refresh token, deletes the vault entries
and returns to the sign-in gate. `apps/desktop/src/account.rs::sign_in_required` is the one policy:
every process except a validation data root installs the account session. Before the first usable
session (signed in or the cached Offline one) only the local trading store, the account session and
persistence paths exist: the desktop entrypoint shows the sign-in window, and
`start_signed_in_runtime` (market runtime and provider sessions, context sources, the
market-demanding workspace, the trading market feed and the broker venue relay) runs only when the
`services::SignedInStart` gate releases it, exactly once per process; a validation root runs it
directly. After a later sign-out the workspace window renders `components/sign_in_gate.rs` (no
workspace handlers or trading hotkeys attach behind it), but provider sessions and the venue stay up
until exit because `MarketService::shutdown` is final and cannot restart in-process; the next sign-in
starts nothing new. `tools/naming_check` keeps these owners out of the entrypoint. Tests must not call `start_restore` or touch
the native vault; use injected vaults and `AccountService` test seams. Manage Profile opens
`https://aeristerminal.com/account/`.

The sibling website is at `C:\Users\devraj\Downloads\Softwares\aeris-website`. Inspect both repositories for future auth/profile/subscription changes, and preserve unrelated edits there. Keep tokens and credentials out of UI and logs.

## Design system coordination

`crates/ui/design_system/platform.css` and the Rust tokens in `crates/ui/design_system/src/lib.rs` are
the native platform design-system source. The website keeps a mirrored portable stylesheet at
`src/styles/platform.css` in the sibling repo.

When changing shared platform tokens such as colors, typography, radii, or interaction states:

1. update the native source and Rust token mapping together;
2. update the website mirror in the same coordinated task;
3. run the website sync/tests so auth receives the same stylesheet;
4. do not reintroduce one-off duplicate platform tokens in landing/auth/account CSS.

Theme presets are the `[data-theme="<id>"]` blocks in `platform.css`, compiled into `ThemePreset` and
`AerisTheme::resolve`; `themes.md` is their palette source and the preset blocks must match it.
The popover layer (`--surface-popover`, `--border-popover`, `--border-popover-highlight`) is
Aeris-only: it lives solely in `:root:not([data-theme])` and `.dark:not([data-theme])`, the
design-system build rejects it in any other block, and presets resolve it to the CSS fallbacks
(`surface-secondary`, `border-secondary`, transparent). They are platform-only: `crates/ui/chart_integration` projects a non-default
preset onto Aeris Charts through its existing appearance and options APIs, and Aeris Charts never
receives a preset identity and keeps its own default theme.

`CSS.md` holds the exact theme CSS from the Theme System and the rules for using it. Read it before
any UI work and always follow it: every color, radius and border width comes from a token, never a
hardcoded value or an ad-hoc alpha tint. Keep `CSS.md`, `platform.css` and the Theme System identical.

## Verification

Use focused checks first:

```text
cargo check -p <crate> --locked
cargo test -p <crate> --locked
cargo clippy -p <crate> --all-targets --all-features --locked -- -D warnings
```

Before a completed native delivery, run the relevant broad gates; for release-quality changes use:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Do not suppress a new failure just to make a gate green. Fix failures caused by the change and report
independent baseline failures precisely.

CI runs the same gates on GitHub-hosted runners. `.github/workflows/ci.yml` triggers on `push` and
`pull_request` to `main` and on `workflow_dispatch`: `windows-latest` runs fmt, clippy, the workspace
tests and `aeris_naming_check`, and `ubuntu-latest` runs fmt and naming_check.
Windows workspace tests use the default parallel `cargo test --workspace --all-features --locked`.
Desktop bridge wake listeners must unregister their GPUI task waker when a notification
completes or the listener is dropped, so a late publication cannot wake a retired test
scheduler. GPUI interaction fixtures use an injected empty trading store, not a live
subscription thread that can wake GPUI's deterministic test scheduler from outside it;
the real subscription bridge has separate owner-level tests. The workflow keeps
`permissions: contents: read`, concurrency cancellation and `AERIS_RITHMIC_KIT_DISABLED=1` (hosted
runners have no Rithmic kit; kit-gated code is skipped there). `pull_request_target`, `secrets.`
references and self-hosted runners are forbidden on the pull-request path, and any workflow that uses
a self-hosted runner must stay `workflow_dispatch`-only in every trigger form; live, credentialed and
performance lanes stay `workflow_dispatch`-only. The manual `macos_preview.yml` and
`windows_preview.yml` tester builds are the only hosted lanes that may receive the Rithmic kit: only
when a lane's `rithmic_kit` input is set does it unpack the kit's `proto` folder from the
`AERIS_RITHMIC_KIT_TAR_XZ_B64` repository secret (its only secret), and it deletes the kit right
after the build and fails if the kit was not compiled in. The Windows lane packages the release binary
with Inno Setup from `apps/desktop/packaging/windows/aeris_terminal.iss` (a per-user installer that
installs the desktop directly, shares the lifecycle installer's AppId and folder, and never deletes
user data on uninstall) and smoke-tests a silent install, launch and uninstall; the macOS lane
smoke-launches the app bundle and ships a disk image with an Applications link. Both keep their
output only as a 14-day workflow artifact.
`tools/naming_check` enforces this shape across every workflow.

## Build output hygiene

Cargo never removes superseded artifacts. Every distinct command, profile and feature set adds a full
new copy under `target/`, and GPUI debug builds have grown it past 70 GB in a few hours. Keep it bounded:

- Before building a new desktop binary (`cargo build` or `cargo run`, debug or release), run
  `cargo clean` so the old `target/` is removed completely and only the new build remains.
- Before each verification batch, check the size of `target/`; above 25 GB, run `cargo clean` first.
  Clean `third_party/gpui_base/target` the same way when it exists.
- Never run `cargo clean` while another Cargo process is building in this workspace.
- Delete each validation root under `%LOCALAPPDATA%\Temp\aeris_validation\` once its run is finished
  and its evidence is recorded; do not leave per-attempt roots behind.
- The maintainer's machine also runs an hourly `Aeris Target Cleanup` scheduled task that enforces the
  25 GB cap and removes validation roots untouched for two days. It is a backstop, not a substitute.

## Release workflow

Do not publish, install, or point clients at the former Axiusflow endpoint, and the retired
publication workflow must not be restored. Releases reach users through the website download page:
after the fix is pushed to `main`, run both preview lanes with `-f rithmic_kit=true`, check that
each smoke test passed, download `Aeris-Terminal-<version>.dmg` from the
`aeris-terminal-rithmic-macos-arm64-preview` artifact and `Aeris-Terminal-Setup-<version>.exe` from
the `aeris-terminal-rithmic-windows-x64-preview` artifact, and publish them from the sibling website
with `bun run publish:desktop -- <dmg> <setup exe>` (it uploads them under `/downloads/` and
rewrites `/downloads/latest.json`, which `https://aeristerminal.com/download/` reads). Public file
names carry only the product and the workspace version (`[workspace.package] version`, semantic
versioning from 1.0.0); never put a provider name or "preview" in them. CI never holds AWS
credentials; publication runs on the maintainer's machine. Builds are ad-hoc signed on macOS and
unsigned on Windows, so Gatekeeper and SmartScreen warn on first launch.

The local launcher and signed lifecycle code remain for future integration; development builds run the desktop directly and automatic update checks are disabled. Do not claim installed-app or update behavior was verified unless exercised on that path.

If something fails, the tester sends back `~/Library/Application Support/Aeris/logs/aeris.log` on
macOS or `%LOCALAPPDATA%\Aeris\logs\aeris.log` on Windows.

## Rust and documentation conventions

- Rust edition/toolchain is repository-pinned; dependencies stay pinned to intentional versions.
- Avoid `unwrap`/`expect` outside tests and unavoidable startup invariants.
- Propagate errors intentionally; do not silently discard failures.
- Keep file/directory names snake_case.
- Fix Clippy findings rather than adding broad lint suppression.
- Do not create planning documents, architecture diaries, nested agent files, or duplicate README files.
  Durable architecture rules belong in code/tests/`tools/naming_check`; implementation notes belong near
  the code they describe.
- `plan/stabilization/` is the one sanctioned exception to the rule above: it holds the stabilization
  tracking document `stabilization_work.md` and the audit/design report copies under
  `plan/stabilization/reports/` (redacted; the repo is public). Every feature commit must update
  `plan/stabilization/stabilization_work.md` in the same commit: check the work item, note the findings
  it closes, append a progress-log line, and record any new restriction there.
- Every commit that introduces or changes an architecture restriction (a `tools/naming_check` rule, a
  crate boundary, a writer or CI rule) must also update this `AGENTS.md` in the same commit, so the
  rule lives where future agents read it.

## Business outreach CRM

Outreach to prop firms, brokers and providers is tracked in a self-hosted Twenty CRM on the
maintainer's machine. Twenty is the source of truth for outreach status, not this repository.

- Twenty runs in Docker from `C:\Users\devraj\twenty-crm` (compose project `twenty`, image tag pinned
  in `.env`) at `http://localhost:4747`, bound to loopback only (`127.0.0.1` and `[::1]`; Windows
  resolves `localhost` to `::1` first, so dropping the IPv6 binding adds ~2s to every connection).
  Containers restart automatically. The REST API allows 100 requests per 60s per key; batch and retry.
- Use the REST API at `http://localhost:4747/rest/` with the bearer key stored in
  `C:\Users\devraj\twenty-crm\api_key.txt`. Never print, log or commit that key, the `.env` file, or
  any contact data. Contact names, emails and deal notes never enter this public repository.
- Data model: a Company per firm, a Person per contact linked to the company, and one Opportunity per
  outreach relationship, named after the firm (chart-engine deals end in ` - Chart engine`).
  Company `companyType` says what the firm is (prop firm, futures FCM or IB, stock, FX/CFD or
  multi-asset broker, crypto exchange, data feed / infrastructure, trading platform, broker tech
  vendor, exchange, professional trading firm, fintech/media). Opportunity custom fields:
  `aerisRole` (supported in Aeris, pitch the Aeris platform, broker connection, data/infra supplier,
  competing platform, chart engine licensing, not a target), `fit` (strong, good, check connection
  first, good fit via NinjaTrader request, later, not a fit) with a one-line `fitReason`, plus `outreachStatus`, `priority`, `platforms`, `sentAs`,
  `firstContactAt`, `lastActivityAt`, `whatHappened` and `nextAction`; the standard `amount` holds
  any quoted annual price.
- After sending outreach or reading a reply, update the matching Opportunity in the same task: set
  `outreachStatus` and `lastActivityAt`, append a dated line to `whatHappened`, set `nextAction`, and
  revise `fit` and `fitReason` when the reply changes them. Create the Company (with `companyType`),
  Person and Opportunity (with `aerisRole`, `fit` and `fitReason`) first for a firm that is not in
  the CRM yet.
- Supported in Aeris is only for connections that are approved and working (Rithmic). A firm that
  has closed gets `outreachStatus` out of business and `fit` not a fit, with the closure date and
  source in `fitReason`. A firm we dropped ourselves gets `outreachStatus` rejected by us,
  `aerisRole` not a target, `fit` not a fit, the decision date in `fitReason` and no `nextAction`. Saved Opportunity views split the list into Supported
  in Aeris, Active pipeline, Later (needs new work) and Dead ends; keep a deal's role and fit accurate
  so it lands in the right view.
- Tradovate/NinjaTrader-only firms are not out of reach: Tradovate gives no API access for prop or
  evaluation accounts, so approval comes from NinjaTrader, and prop-firm requests are the lever. Pitch
  them and ask them to request Aeris support from NinjaTrader/Tradovate.
- Email only addresses confirmed from the firm's own site or an existing thread, and never claim live
  order routing or performance that has not been verified.

## Delivery

Report exactly what changed, what was verified, what was deployed/published/installed if applicable,
and what still requires maintainer-only credentials or visual/manual confirmation. Do not present an
untested assumption as a completed result.
