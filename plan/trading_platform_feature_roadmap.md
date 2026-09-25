# Aeris Trading Platform Feature Roadmap

This roadmap defines the trading features that turn Aeris Terminal from a market-data and charting
terminal into a complete local-first futures trading platform. Its product goal is:

> The fastest futures platform, the one that protects traders from breaking their own and their
> prop firm's rules, and the one that shows the order book and the real-world fundamentals behind
> every contract, while running entirely on the user's machine.

How to read this file:

1. **Status at a glance**: where every batch stands and which chart batches it needs.
2. **How work is delivered**: the batch, gate and commit rules.
3. **Batches T1–T8**: the work itself, as checklists with acceptance criteria.
4. **Scope and ownership**, **operating constraints** and **maintainer decisions**.
5. **Current baseline**: what exists today.
6. **Foundations PF1–PF10** and the **feature catalog M1–M7**: detailed requirements.
7. **Verification and completion**.

Related plans: `plan/study_runtime_sdk_roadmap.md` remains the authority for studies, the Study SDK
and the in-app Study Editor. Chart-side prerequisites live in the Aeris Charts repository's
`plan/Expansion.md` (items F1–F6, OF1–OF18, CT1–CT6, I1–I4 and PD1–PD11, grouped into batches
B1–B9). This roadmap names those dependencies by ID and does not restate engine design. Item IDs
here (PF, M, D) are stable; batches group them without renumbering.

## Status at a glance

Updated 2026-09-25. Baseline source-confirmed 2026-09-24.

| Batch | Scope | Needs from Aeris Charts | External gate | Status |
| --- | --- | --- | --- | --- |
| T1 | Trading foundations: D1, D3, PF3, PF6, PF7, PF9 | None | None | **Complete** |
| T2 | Trading, execution and risk on the simulated venue: M2.1–M2.8, M3.1–M3.4, M7.2 | None for basic chart trading (existing Aeris Charts trading layer); B1 (PD11) for multi-account and trailing stops on charts, (PD1) for chart warnings, (PD3) for plan levels | None | **In progress** |
| T3 | Order flow on Hyperliquid data: PF5, M1.2, M1.4, M1.5, M1.7 | B3 (F2, OF1, OF2, OF11, OF12, PD10) | None | Open |
| T4 | Context and workspace: D6, M5.1–M5.5, M7.1, M7.4 | B1 (PD3, PD5, PD7) | Free API keys per user | Open |
| T5 | Rithmic live trading: PF1, PF2, PF4, M1.6, live qualification of T2 | B1 (PD1); B6 (PD8) for chart markers of M1.6 | Rithmic onboarding, conformance, live accounts, D4 | Blocked (onboarding) |
| T6 | Record, replay and review: D2, PF8, M4.1–M4.4 | B1 (PD4, PD6), B5 (PD2, F1) | D2 decision; data licensing checklist | Open |
| T7 | Institutional depth: M1.1, M1.3, M1.8, M4.5, M5.6, M5.7 | B4 (OF9), B5, B6, B7 | Data licensing checklist | Open |
| T8 | Power users: M6.1–M6.5, M7.3, M7.5–M7.8, PF10, study roadmap Phases F–L | B9 (I4) | D5; release path for M7.8 | Open |

Ordering: T1 unblocks T2, T5, T6 and part of T7. T2 and T3 are independent of each other. T5 starts
when Rithmic onboarding clears and reuses T2 unchanged against the live venue. T6 needs T1's store
and T2's execution log. Change the order only when priorities change, and record it here.

## How work is delivered

Work proceeds in **large batches**, one row of the status table at a time, following `AGENTS.md`.

- **Implement the whole batch first.** Build every checklist item, with deterministic regression
  tests written as each item is built. Do not stop between items for broad gates, commits or
  pushes.
- **Focused checks while implementing.** `cargo check`, `cargo test` and `cargo clippy` for the
  touched crates only.
- **One broad gate at the end.** `cargo fmt --all -- --check`, workspace clippy with
  `-D warnings`, and `cargo test --workspace --all-features --locked`, plus real-path evidence for
  provider, persistence and rendering behavior the batch touches. Fix every failure and rerun until
  green.
- **One commit and push per batch.** Use the `type(scope): outcome` style, list delivered IDs and
  verification in the body, and push `main`. Never commit a batch with a failing or skipped gate.
- **Update this file in the same commit.** Tick the checklist and set the batch status. Put
  architecture rules that became enforceable into `tools/naming_check`.
- **Chart dependencies.** When a batch needs an Aeris Charts batch, deliver that chart batch first
  and pin its revision in the same terminal batch. Change only the `aeris_charts_*` revisions.

A batch may be split into two commits only when it is too large to review as one, and each part
must pass the broad gate on its own.

## Batches

An item is ticked only when it works through the real runtime and desktop path, not when a unit
test alone passes.

### T1 — Trading foundations

**Scope:** D1, D3, PF3, PF6, PF7, PF9. **Needs from Aeris Charts:** nothing. **Status:** complete.

- [x] **D1** decided and recorded: one in-process trading owner beside `market_runtime`.
- [x] **D3** decided after measurement: one embedded local store.
- [x] **PF3** Canonical trading domain: fixed-point accounts, orders, order events, fills,
      positions and PnL with explicit scales and provenance.
- [x] **PF6** Contract metadata: tick size, point value, currency, expiry, first notice, last
      trade and session hours in the canonical instrument model.
- [x] **PF7** Local store for user-owned records with background I/O, schema versions,
      migrations, bounded retention and CSV/JSON export.
- [x] **PF9** Local simulated venue with the PF3 contracts, touch fills, and simulated accounts
      that are always visually distinct.
- [x] `tools/naming_check` enforces the trading-owner boundary; broad gate green; committed and
      pushed.

**Acceptance:** a simulated order can be placed, filled and reflected in positions and PnL through
the trading owner, and records survive restart.

Implementation evidence: `crates/trading_runtime` owns the bounded command worker, SQLite store,
simulated execution and restart/export path. The desktop starts that owner beside `market_runtime`
and installs provider contract metadata through the desktop integration boundary. The release storage
measurement (500 executions and 2,000 user records) completed in 2.619 s, reopened in 4.47 ms, and
used 831,488 bytes. The direct desktop readiness command then exercised the real development binary
successfully, producing schema-4 readiness with market, account and trading services ready and two
configured providers. The command uses the existing development-mode auth boundary, so it does not
open a browser or create an account session.

### T2 — Trading, execution and risk on the simulated venue

**Scope:** M2.1–M2.8, M3.1–M3.4, M7.2. **Needs:** T1; the existing Aeris Charts trading layer
for chart trading; Aeris Charts B1 (PD11, PD1, PD3) for account filtering, trailing and break-even
presentation, order-line warnings and plan levels.
**Status:** in progress.

Implementation progress (2026-09-25): the single `trading_runtime` owner now has durable schema-v4
risk profiles and account locks, pre-trade checks on simulated orders, cancel/modify/cancel-all,
account/global flatten and kill-switch commands, mark-to-market unrealized P/L, restart-safe order
state, and a first desktop order-control surface. The keymap owner validates normalized bindings
and reserved desktop chords, and global buy/sell/cancel/flatten/kill actions now route through that
owner only when the trading chrome owns focus. The order-book surface also renders the runtime's
currency P/L projection through a bounded asynchronous snapshot refresh, including exact position
level tick conversion when instrument tick metadata permits it. Configured accounts now also expose
runtime-owned rule-distance meters for loss, trailing drawdown, contract capacity, restrictions and
durable locks in the selected-account trading controls.
The remaining T2 checklist items are intentionally open: the DOM ladder, bracket UX, chart
dispatch, copier, consistency-rule evaluation, and chart warning wiring still require the real
runtime and desktop paths. The simulated order-entry panel now has quantity presets, all canonical order types
and time-in-force choices, a runtime-backed account selector, and explicit best-ask/best-bid limit
entry controls; per-account and global cancel,
flatten and kill controls share the same owner path. Working simulated orders for the selected
account/instrument are now surfaced in that panel with bounded per-order cancellation controls;
working limit orders also have an explicit best-price reprice command. The DOM ladder now emits
provider-neutral row-price intents that route through the selected account and order-entry runtime
path; full drag UX, position overlays, the P/L column and full inline ladder placement remain open.
The ladder now projects bounded selected-account working-order markers, emits same-side drag/drop
intents, and the desktop resolves them only against a matching selected-account working limit order
before issuing an exact-price modify command; position overlays and the P/L column remain open.

Execution and risk ship together because no order may leave the order-command path without the
M3.2 checks.

- [x] **M7.2** Single keymap owner with conflict detection.
- [ ] **M2.1** DOM trading ladder: one-click orders, drag to modify, inline orders and position,
      P/L column, recent volume at price; render cost measured during bursts.
- [x] **M2.2** Order entry panel: quantity presets, order types, time in force, account selector.
- [ ] **M2.3** Bracket and strategy templates: stop and target, OCO, trailing, break-even,
      scale-out, with local-management labels where not server-side.
- [ ] **M2.4** Chart trading wired to the existing Aeris Charts trading layer: runtime orders,
      positions and fills projected into chart trading snapshots; chart intents (place bracket,
      modify, cancel, stop/target, close) resolved through the M3.2 order-command path with
      confirmation rules. Not blocked on B1. The canonical order states gain partially filled,
      pending modify and pending cancel so the chart shows in-flight commands correctly.
- [x] **M2.5** Trading hotkeys, disabled while a text field has focus.
- [x] **M2.6** Flatten and kill switch per account and globally.
- [ ] **M2.7** Multi-account trade copier with per-account multipliers, kill switches and M3.2
      checks before each mirrored order.
- [x] **M2.8** Positions and PnL in currency and ticks.
- [ ] **M3.1** Prop-firm rule engine with versioned profiles and live distance meters; rule
      warnings on order lines through PD1.
- [ ] **M3.2** Pre-trade checks and hard locks in the single order-command path; the lock survives
      restart and blocks DOM, chart, copier and hotkeys.
- [ ] **M3.3** Tilt detection with documented deterministic rules.
- [ ] **M3.4** Session plan and checklist, with plan levels on charts through PD3.
- [ ] Broad gate green; committed and pushed.

**Acceptance:** the M2 and M3 acceptance criteria pass on the simulated venue: every command is
idempotent across reconnects, simulated and live accounts cannot be confused, and rule evaluation
is reproducible from recorded fills.

### T3 — Order flow on Hyperliquid data

**Scope:** PF5, M1.2, M1.4, M1.5, M1.7. **Needs:** Aeris Charts B3. **Status:** open.

Hyperliquid public trades already carry aggressor sides, so order flow can be built before Rithmic
onboarding.

- [ ] **PF5** Bounded classified trade tape from `market_runtime`, coalesced for slow consumers
      and generation-fenced, feeding the chart's shared tape (F2).
- [ ] **M1.2** Footprint charts wired to PF5, with settings UI and persisted configuration.
- [ ] **M1.4** Cumulative delta; delta divergence alert rule.
- [ ] **M1.5** Big trades and sweeps with adaptive per-contract thresholds.
- [ ] **M1.7** Time and sales panel with size, side and price filters.
- [ ] Burst workload measured in a release build; broad gate green; committed and pushed.

**Acceptance:** every view renders from one shared canonical stream without duplicate provider
demand, and frame work stays bounded during bursts.

### T4 — Context and workspace

**Scope:** D6, M5.1–M5.5, M7.1, M7.4. **Needs:** Aeris Charts B1 (PD3, PD5, PD7).
**Status:** open.

- [ ] **D6** Calendar distribution decided.
- [ ] One bounded background owner for public data fetching, caching and scheduling.
- [ ] **M5.1** Economic event calendar with countdowns and rule-driven flatten or lock.
- [ ] **M5.2** Energy dashboard (EIA, NOAA).
- [ ] **M5.3** Commitments of Traders beside price.
- [ ] **M5.4** Grains and agriculture (USDA).
- [ ] **M5.5** Macro panel (FRED).
- [ ] **M7.1** Command palette with a shared command registry.
- [ ] **M7.4** Linked symbol groups with synchronized crosshair and time range.
- [ ] Broad gate green; committed and pushed.

**Acceptance:** every value carries its source and release time, charts show no look-ahead, and
missing keys or outages degrade to a clear unavailable state.

### T5 — Rithmic live trading

**Scope:** PF1, PF2, PF4, M1.6, live qualification of T2. **Needs:** T1, T2; Rithmic onboarding.
**Status:** blocked until Rithmic grants live accounts and order-routing conformance.

- [ ] Order plant and PnL plant templates verified against the licensed Provider Kit.
- [ ] **PF1** Order routing: submit, modify, cancel, server-side brackets, OCO and trailing stops,
      status, fill and rejection updates, account list; idempotent client order IDs.
- [ ] **PF2** Positions, PnL, balance, margin and broker risk limits.
- [ ] Rithmic conformance passed.
- [ ] T2 features (DOM, chart trading, hotkeys, flatten, copier, rules and locks) qualified on live
      accounts, including the copier across several accounts.
- [ ] **D4** decided; **PF4** bounded canonical order-level publication.
- [ ] **M1.6** Queue position, iceberg, pulled-liquidity and size-cluster detectors, labeled as
      estimates; chart markers through PD8 once Aeris Charts B6 lands.
- [ ] Broad gate green; committed and pushed.

**Acceptance:** live Rithmic accounts trade from the DOM, chart and hotkeys with enforced rules,
and reconnects never duplicate or lose commands.

### T6 — Record, replay and review

**Scope:** D2, PF8, M4.1–M4.4. **Needs:** T1, T2; Aeris Charts B1 (PD4, PD6) and B5 (PD2, F1).
**Status:** open.

- [ ] **D2** decided; `AGENTS.md` and `tools/naming_check` amended in the same change so the
      recording store never feeds live history.
- [ ] Data licensing checklist reviewed for local recording.
- [ ] **PF8 / M4.1** Opt-in session recorder with manifests, integrity checks and retention caps.
- [ ] **M4.2** Market replay at 1× to 100× with practice trading through PF9.
- [ ] **M4.3** Trade review jumping to any past trade with book, footprint and executions.
- [ ] **M4.4** Automatic journal and analytics with screenshots through chart export (PD6).
- [ ] Broad gate green; committed and pushed.

**Acceptance:** replay never shows data after the replay clock, replayed sessions reproduce live
derived results, and journal statistics are reproducible from stored records.

### T7 — Institutional depth

**Scope:** M1.1, M1.3, M1.8, M4.5, M5.6, M5.7. **Needs:** T3, T6; Aeris Charts B4, B5, B6, B7.
**Status:** open.

- [ ] **M1.1** Liquidity heatmap from canonical depth and PF5 trades, with settings UI.
- [ ] **M1.3** Session, composite, fixed-range and anchored volume profiles, VWAP bands and TPO.
- [ ] **M1.8** Tick, volume and range charts with footprint on the same bars.
- [ ] **M4.5** Queue-aware paper fills from recorded order-level data.
- [ ] **M5.6** Contract roll and expiry calendar.
- [ ] **M5.7** Session and trading-hours display.
- [ ] Broad gate green; committed and pushed.

**Acceptance:** depth views stay bounded under dense books and match the recorded session in
replay.

### T8 — Power users

**Scope:** M6.1–M6.5, M7.3, M7.5–M7.8, PF10, study roadmap Phases F–L. **Needs:** Aeris Charts B9
(I4). **Status:** open.

- [ ] **M6.1** Order-flow, rule-meter, event and study alerts.
- [ ] **M6.2** Optional Telegram or Discord delivery following the data licensing checklist.
- [ ] **M6.3** Custom indicators and the in-app Study Editor (study roadmap Phases F–L).
- [ ] **M6.4** Strategies and local backtesting over recorded sessions.
- [ ] **M6.5** Optional local AI assistant over user-owned records only.
- [ ] **M7.3** Multi-window and multi-monitor workspaces.
- [ ] **M7.5** Performance mode and diagnostics overlay.
- [ ] **M7.6** Themes and accessibility.
- [ ] **M7.7** Layout and settings sync through a user-controlled folder.
- [ ] **M7.8** Distribution and updates once a release path is approved.
- [ ] **D5** decided; **PF10** additional provider.
- [ ] Broad gate green; committed and pushed.

**Acceptance:** the Definition of completion below is met.

## Scope and ownership

Aeris Charts renders what appears inside a chart. Aeris Terminal owns every panel and product rule
outside it. The DOM ladder (`terminal_ui`), time and sales, all risk rules, and the trading lock are
platform features. The lock needs no engine support, because the platform already decides which
chart trading gestures reach Aeris Charts and whether to act on the resulting trading intents.

Every item must satisfy the five core principles and the architecture invariants in `AGENTS.md`.
Where a feature conflicts with a current invariant, this roadmap records the conflict as a
maintainer decision instead of designing around it.

### Operating constraints

1. **Local-first and zero server cost.** No Aeris server receives, stores, processes or relays
   market data. There is no deployed Aeris backend today, and no feature may depend on one.
2. **Exchange data stays on the user's machine.** Rithmic (and future CQG or dxFeed) data flows
   from the broker to the desktop process and is processed only there. The user pays exchange fees
   through their broker. Aeris must not redistribute exchange data or anything derived from it
   in a way that makes Aeris a distributor.
3. **Public non-exchange data is fetched by the client directly from its official source** (EIA,
   CFTC, USDA, FRED, NOAA and similar), with the user's own free API key where one is required.
4. **User-owned data is stored locally**: journals, executions, rule profiles, session plans,
   layouts and recordings. Optional sync uses a folder the user controls (for example a cloud-drive
   folder), never an Aeris service.
5. **Provider access is gated.** Rithmic onboarding (production credentials, application
   conformance and order-routing approval) is not complete. Features that need Rithmic are designed
   now and qualified only after onboarding. Features that can be built against Hyperliquid public
   data or a local simulated venue proceed first.

### Data licensing checklist

Before each feature that stores, derives or transmits exchange data ships, confirm against the
current CME market data policies, the Rithmic agreement and any future broker agreements:

- Local storage of recorded trades and depth for the subscriber's personal replay and review.
- Display of derived values (queue position, detections, footprint and profile statistics).
- Alerts that leave the machine (webhooks or chat bots) must carry event descriptions, not
  exchange prices or quantities, unless the agreements explicitly allow it.
- Professional versus non-professional subscriber status and its effect on permitted use.

This checklist is a release gate, not legal advice. Record the reviewed policy versions in the
release notes for the feature.

### Maintainer decisions

These decisions block specific items. Each is listed with the recommendation from this roadmap.

| ID | Decision | Blocks | Batch | Recommendation |
| --- | --- | --- | --- | --- |
| D1 | Owner for broker accounts, orders, fills and positions. `account_runtime` is Aeris SaaS identity and should not absorb broker trading | PF1–PF3 and every trading feature | T1 | One in-process trading owner beside `market_runtime`, with the same single-session, bounded, generation-fenced rules. Provider sessions for orders stay provider-owned and shared, never per chart or panel |
| D2 | Session recording versus the ban on market-history persistence | PF8, M4 replay and review, realistic simulation, local backtesting | T6 | Amend the invariant narrowly: allow an explicit, opt-in, user-owned session recording store that is separate from and never feeds the on-demand history cache. Update `AGENTS.md` and `tools/naming_check` in the same change so the ban on a second market-state model remains enforced |
| D3 | Local storage engine for user-owned records (journal, executions, rule profiles, plans) | PF7 | T1 | **Decided: SQLite via the pinned bundled `rusqlite` dependency.** The release measurement above is the recorded bounded-workload evidence; do not introduce another store |
| D4 | Order-level data leaving the Rithmic adapter | PF4, M1.6 | T5 | Publish a bounded canonical order-level view through `domain/market_data` with provider identity, local order and sequence evidence kept separate, as `AGENTS.md` requires |
| D5 | First additional provider (CQG or dxFeed) | PF10 | T8 | Defer until Rithmic trading is qualified; prioritize by user demand |
| D6 | Calendar distribution without a backend | M5.1 | T4 | Ship the curated event calendar as signed data with application releases, or fetch it from a public repository file; decide once a release path exists |

## Current baseline

Source-confirmed on 2026-09-24. This is the starting point, not a claim of completeness.

| Area | Present today | Evidence |
| --- | --- | --- |
| Rithmic market data | Ticker and History plants; login, heartbeat, symbol search, instrument reference, quotes and top of book, trades with aggressor side, time and tick bar replay and live updates, depth by order with snapshot | `crates/adapters/rithmic_protocol` (`ReadOnlyPlant`, `OutboundRequest`, `history_adapter.rs`) |
| Rithmic order-level book | Assembled inside the adapter (`MboBookAssembler`, up to 131,072 orders) but published only as aggregated price levels with optional order counts; order identities do not leave the adapter | `provider_session/canonical_market.rs`, `domain/market_data` `DepthLevel` |
| Rithmic trading | **Absent.** No Order plant, PnL plant, order routing, positions, fills or account risk information | `protocol.rs` allowlist |
| Hyperliquid | Public candles, L2 snapshots, BBO, trades, catalog and asset context; explicitly no orders or paper trading | `crates/adapters/hyperliquid_market` |
| CQG and dxFeed | **Absent** | — |
| Market ownership | `MarketEngine` demand ownership; `market_runtime` merges history and live state and publishes series, order-book snapshots, study outputs and price-alert triggers; no separate trade-tape stream to the UI | `market_engine`, `market_runtime` |
| Instrument metadata | Provider price increment at install time plus canonical tick size, point value, currency, expiry, first-notice, last-trade and session-hours fields; current provider reference payloads may leave optional fields unavailable rather than guessing | `contracts/src/messages.rs`, rithmic `catalog.rs`, `domain/instruments` |
| Account runtime | Aeris SaaS sign-in (OIDC, lease, vault); disabled in development builds; not a broker trading account | `crates/account_runtime`, `apps/desktop/src/account.rs` |
| Order book panel | Read-only price ladder with aggressor volume columns; the P/L column is an empty placeholder | `crates/ui/terminal_ui`, `SidePanel::OrderBook` |
| Charts in the desktop | Candles, bars, line, area, baseline; drawings; price-alert lines; split workspace; study-runtime indicators. Footprint, volume profile, heatmap and trading lines exist in Aeris Charts but are not wired into the desktop | `crates/ui/chart_integration` |
| Studies | Study runtime and SDK Phases A–E complete; editor and sandbox pending | `plan/study_runtime_sdk_roadmap.md` |
| Alerts | Price alerts evaluated in `market_runtime` on live trades (32 per consumer) with OS notifications | `PriceAlertRegistry`, `platform_runtime/user_notifications.rs` |
| Persistence | Workspace layouts, chart preferences, studies, alerts, watchlist and the credential vault; trading records now use a separate bounded SQLite store. Market-history persistence remains deliberately banned | `workspace_persistence.rs`, `crates/trading_runtime`, `tools/naming_check` |
| Recording, replay, journal, simulator | Session recording/replay remains absent; T1 now provides durable journal/user records and a local simulated venue, while "replay" in market code still means bar snapshot contracts and Rithmic history requests | `crates/trading_runtime`, `application/src/replay_snapshot.rs` |
| Risk controls | Durable simulated-venue profiles, locks, pre-trade checks and kill-switch foundation; full rule meters and UI remain T2 work | `crates/trading_runtime`, `apps/desktop/src/keymap.rs` |
| Context data | **Absent** except a CME Globex session-day helper for week and month history buckets | `rithmic_protocol/src/calendar.rs` |
| Workspace UX | Tabs, split panes and their shortcuts, themes, feed diagnostics; no command palette, trading hotkeys, multi-window workspace or linked symbol groups | `apps/desktop`, `crates/observability` |
| Distribution | Launcher, signed release identity and lifecycle code; automatic updates and publication disabled until a release backend exists | `platform_runtime`, `apps/desktop/src/update.rs` |

## Foundations

Foundations unblock most modules. Each ships through the real runtime and desktop paths with
deterministic tests, bounded resources and naming-check coverage.

### PF1 — Rithmic Order plant integration (T5)

- **Outcome.** Submit, modify and cancel orders; market, limit, stop and stop-limit types;
  server-side brackets, OCO and trailing stops where Rithmic supports them; order status, fill and
  rejection updates; account list.
- **Rules.** Prefer server-side order types so protection survives an application crash. Any
  order managed locally must be labeled as local in the UI. Every command is idempotent through
  client order identifiers and fenced by session generation.
- **Gate.** Rithmic onboarding and conformance. Verify every template and field against the
  licensed Provider Kit before depending on it.

### PF2 — Rithmic PnL plant and account state (T5)

- **Outcome.** Positions, realized and unrealized PnL, account balance and margin, and account risk
  limits where the account exposes them (many prop-firm accounts carry loss limits in the broker's
  risk system).
- **Gate.** Rithmic onboarding. Broker-reported limits are authoritative when present; local rule
  profiles (M3.1) complement them.

### PF3 — Canonical trading domain (T1)

- **Outcome.** Provider-neutral fixed-point models for accounts, orders, order events, fills,
  positions and PnL, with explicit scales and provenance, in a domain crate owned by D1.
- **Rule.** Chart trading geometry in Aeris Charts receives projections of these models; Aeris
  Charts never owns order state.

### PF4 — Canonical order-level book publication (T5)

- **Outcome.** A bounded order-level view (order identity, side, price, size, priority evidence and
  sequence) published from the Rithmic adapter per D4, alongside the existing aggregated book.
- **Consumers.** Queue position, iceberg, pulled-liquidity and size-cluster analytics (M1.6), the
  DOM ladder's order counts and queue column, and PD8 in Aeris Charts.

### PF5 — Trade tape publication (T3)

- **Outcome.** A bounded classified trade stream from `market_runtime` to consumers (time and
  sales, footprint, CVD, big-trade detection), coalesced for slow consumers, with generation
  fencing. It feeds the Aeris Charts shared tape (F2) as a projection.

### PF6 — Contract metadata (T1)

- **Outcome.** Canonical tick size, point value, currency, expiry, first notice and last trade
  dates, and session hours per instrument, sourced from provider reference data.
- **Consumers.** PnL in ticks and currency, risk checks, roll calendar (M5.6), DOM tick grid.

### PF7 — Local store for user-owned records (T1)

- **Outcome.** Durable storage for executions, journal entries, tags, notes, screenshots, rule
  profiles, rule evaluations, session plans and analytics caches, per D3.
- **Rules.** Background-thread I/O only, schema versioning and migrations, bounded growth with
  user-visible retention settings, and export to open formats (CSV, JSON).

### PF8 — Session recording store (T6)

- **Outcome.** Opt-in recording of the user's own subscribed trades, depth updates, quotes and
  bars, compressed on local disk with per-session manifests, integrity checks and retention caps.
- **Gate.** D2 and the data licensing checklist.
- **Rule.** Recordings are replay inputs only. They never become a history cache for live charts.

### PF9 — Local simulated venue (T1)

- **Outcome.** A local execution venue with the same order, fill and position contracts as PF1–PF3,
  driven by live or replayed market data. Fill models: simple touch fills first, then queue-aware
  fills from order-level data (M4.5).
- **Why now.** It lets the complete trading, risk and journal experience be built and tested
  before Rithmic order routing is approved, and powers paper trading afterward.
- **Rule.** Simulated accounts are always visually distinct from live accounts.

### PF10 — Additional providers (T8)

- **Outcome.** CQG and dxFeed adapters behind the same provider-neutral contracts, per D5.

## Feature catalog

Status values: **Present**, **Partial**, **Absent**. The "Aeris Charts" column lists chart-engine
prerequisites by their `Expansion.md` ID.

### M1 — Charts and order flow

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M1.1 | Liquidity heatmap: resting depth over time with trades overlaid, color scaling, thresholds, minimum-size filter | Absent | Feed canonical depth publications and PF5 trades into the chart; heatmap settings UI | F3, OF15, PD8, PD9 | T7 |
| M1.2 | Footprint charts: bid×ask, delta, total and imbalance views; POC; stacked imbalances | Absent in desktop (present in Aeris Charts) | Wire the footprint series to PF5; settings UI; persist footprint configuration | Existing footprint; F2, OF12, PD10 | T3 |
| M1.3 | Volume profile, VWAP bands and TPO: session, composite, fixed-range and anchored | Absent in desktop (visible-range profile present in Aeris Charts) | Session definitions from PF6; profile settings; drawing tools in the toolbar | OF3–OF10, F6 | T7 |
| M1.4 | Cumulative delta and delta divergence | Absent | Divergence alert rule over study outputs (M6.1) | F2, OF1, OF2 | T3 |
| M1.5 | Big trades and sweeps: large prints and multi-level sweeps with adaptive per-contract thresholds | Absent | Sweep grouping by timestamp and aggressor from PF5; threshold settings | F2, OF11, PD8 | T3 |
| M1.6 | Order-level intelligence: own-order queue position and fill likelihood, iceberg refill detection, pulled-liquidity detection, order-size clustering | Absent | Deterministic, parameterized detectors over PF4 in the platform; labeled as estimates; never presented as certainty. The DOM shows queue position and detections in its own columns | PD8 and PD1 for the chart; none for the DOM | T5 |
| M1.7 | Time and sales panel with size, side and price filters | Absent | Bounded filtered window over PF5; GPUI list panel owned by the platform | — | T3 |
| M1.8 | Tick, volume and range charts, with footprint on the same bars | Absent | Interval picker and persistence | F1, OF14, CT3 | T7 |

Acceptance: every order-flow view renders from shared canonical streams without duplicate provider
demand, and frame work stays bounded during news-release bursts.

### M2 — Trading and execution

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M2.1 | DOM trading ladder: one-click orders, drag to modify, working orders and position inline, P/L column, recent volume at price | Partial (read-only ladder) | Extend the existing `terminal_ui` ladder into a trading ladder over PF1–PF3: order-entry clicks and drags, own orders and queue position (PF4), fill the existing P/L column; measure ladder render cost during news bursts | — (platform widget) | T2 |
| M2.2 | Order entry panel: quantity presets, order types, time in force, account selector | Absent | GPUI panel over PF1 | — | T2 |
| M2.3 | Bracket and strategy templates: stop and target on entry, OCO, trailing, break-even, scale-out | Absent | Template model in PF7; server-side execution where supported; local-management labels otherwise | Existing brackets and OCO visuals; PD11 for trailing and break-even presentation | T2 |
| M2.4 | Chart trading: place, drag and cancel orders and positions on the chart | Absent in desktop (present in Aeris Charts) | Project trading-runtime snapshots into the chart and resolve chart trading intents through the single order-command path (simulated venue first, PF1 in T5) with confirmation rules; stop forwarding trading gestures while a lock is active | Existing trading layer for basic chart trading; PD11 for accounts and exact tick prices; PD1 for warnings | T2 |
| M2.5 | Trading hotkeys: buy/sell at bid/ask/market, flatten, cancel all, reverse, with per-hotkey confirmation settings | Absent | Keymap owner (M7.2); hotkeys disabled while a text field has focus | — | T2 |
| M2.6 | Flatten and kill switch: flatten all positions and cancel all orders per account or globally | Absent | Single command path through PF1, visible at all times | — | T2 |
| M2.7 | Multi-account trade copier: mirror orders to several accounts with per-account multipliers and kill switches | Absent | Local copier over PF1 with per-account risk checks (M3.2) before each mirrored order | PD11 for per-account chart filtering | T2 |
| M2.8 | Positions and PnL display in currency and ticks | Absent | PF2 and PF6 projections to panels and charts | Existing position chips | T2 |

Acceptance: no order leaves the machine without passing M3.2 checks; every trading command is
idempotent across reconnects; simulated and live accounts can never be confused.

### M3 — Risk and discipline

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M3.1 | Prop-firm rule engine: profiles for daily loss limit, trailing drawdown (intraday or end-of-day), max contracts, consistency rules and news-time restrictions; live distance meters | Absent | Declarative, versioned rule profiles in PF7; deterministic evaluation on every fill and price update; broker-reported limits (PF2) take precedence when present | PD1 | T2 |
| M3.2 | Pre-trade checks and hard locks: warn on or block any order that would break a rule if its stop is hit; a day lock the trader cannot easily undo | Absent | Check runs inside the single order-command path, including the DOM, chart trading, copier and hotkeys. While locked, the desktop stops forwarding chart trading gestures, rejects any trading intent, disables DOM order entry and shows the lock reason in platform chrome | PD1 for warnings on order lines; lock needs none | T2 |
| M3.3 | Tilt detection: rapid losses, rising size after losses, fast re-entry after a stop, trading outside planned hours | Absent | Documented deterministic rules over the local execution log; actions are warn, cool-down timer (through the M3.2 lock) or size reduction | — | T2 |
| M3.4 | Session plan and checklist: key levels, bias, max loss and allowed setups before trading; end-of-day adherence review | Absent | Plan model in PF7; plan levels projected as host overlays | PD3 | T2 |

Acceptance: rule evaluation is reproducible from recorded fills; a lock survives application
restart; prop-firm profiles are versioned because firms change their rules.

### M4 — Recording, replay and review

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M4.1 | Session recorder | Absent | PF8 | — | T6 |
| M4.2 | Market replay at 1× to 100× with practice trading | Absent | Replay data source feeding the same series and study paths; practice orders through PF9 | PD2, F1 | T6 |
| M4.3 | Trade review: jump from any past trade to that moment with order book, footprint and executions as they were | Absent | Link PF7 executions to PF8 recordings; the DOM replays from the same recording | PD2, PD4 | T6 |
| M4.4 | Automatic journal and analytics: every execution logged; tags, notes and screenshots; win rate, expectancy, maximum adverse and favorable excursion, results by time of day, setup and contract; plain-language findings | Absent | Round-trip grouping and statistics over PF7; screenshots through chart export | PD4, PD6 | T6 |
| M4.5 | Realistic paper trading: fills that respect queue position from recorded order-level data | Absent | Queue-aware fill model in PF9 over PF4 and PF8 | — | T7 |

Acceptance: replay never shows data after the replay clock; a replayed session produces the same
derived results as the live session did; journal statistics are reproducible from stored records.

### M5 — Fundamentals and context (free public data)

| ID | Feature | Status | Data source | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M5.1 | Economic event calendar with countdowns, importance, and automatic flatten or lock before events when a rule profile requires it | Absent | Official release schedules (BLS, BEA, Federal Reserve, EIA), distributed per D6 | PD3 | T4 |
| M5.2 | Energy dashboard: weekly petroleum inventories and natural gas storage with surprise versus the five-year range; heating and cooling degree days | Absent | EIA open data API (user key); NOAA Climate Prediction Center | PD7 | T4 |
| M5.3 | Commitments of Traders positioning by trader category beside price | Absent | CFTC public reporting data | PD7 | T4 |
| M5.4 | Grains and agriculture: crop progress, WASDE supply and demand, export sales | Absent | USDA (NASS Quick Stats with a user key; WASDE and export-sales publications) | PD7 | T4 |
| M5.5 | Macro panel: Treasury yields, dollar index, inflation and employment series | Absent | FRED API (user key); respect per-series copyright terms | PD7 | T4 |
| M5.6 | Contract roll and expiry calendar: front month, days to roll, volume migration, first notice and last trade dates | Partial (expiry strings from Rithmic reference data) | PF6 plus provider volume | PD3 | T7 |
| M5.7 | Session and trading-hours display: RTH and ETH boundaries, holiday closures | Partial (session-day bucketing helper only) | PF6 and published exchange calendars | PD3 | T7 |

Rules: one bounded background owner fetches, caches and schedules public data; nothing blocks the
UI thread; every value carries its release timestamp and source so charts never show look-ahead;
missing keys or source outages degrade to a clear "unavailable" state, never invented values.

### M6 — Automation and research

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M6.1 | Alert expansion: order-flow conditions (sweep, delta divergence, iceberg), rule-meter warnings, upcoming events and study alerts | Partial (price alerts) | Extend `market_runtime` alert evaluation; study alerts follow study roadmap Phase J | Alert lines exist | T8 |
| M6.2 | Alert delivery: desktop notifications (present) plus optional Telegram or Discord through the user's own bot or webhook | Partial | Outbound messages follow the data licensing checklist | — | T8 |
| M6.3 | Custom indicators and the in-app Rust Study Editor | Partial | `plan/study_runtime_sdk_roadmap.md` Phases F–L | I4, F4 | T8 |
| M6.4 | Strategies and local backtesting over recorded sessions with order-book-aware fills | Absent | Separate strategy program, as the study roadmap requires; depends on PF8 and PF9 | — | T8 |
| M6.5 | Optional local AI assistant: journal summaries, plain-language questions about the user's own trades, study-authoring help | Absent | Runs through a local model runtime or the user's own API key; receives user-owned records only, never raw exchange data leaving the machine | — | T8 |

### M7 — Workspace and experience

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M7.1 | Command palette with searchable actions and Bloomberg-style mnemonics, for example `ES footprint 5m` | Absent | Command registry shared by menus, palette and hotkeys | — | T4 |
| M7.2 | Configurable keymap with conflict detection | Partial (window and split shortcuts) | Single keymap owner; trading hotkeys (M2.5) register here | — | T2 |
| M7.3 | Multi-window and multi-monitor workspaces | Partial | Detachable windows with persisted placement per display | — | T8 |
| M7.4 | Linked symbol groups and synchronized crosshair and time range | Absent | Link-group coordinator in the desktop | PD5 | T4 |
| M7.5 | Performance mode and diagnostics overlay: per-panel render and memory cost, feed latency, dropped frames; the promise that the app never freezes during news | Partial (feed diagnostics) | User-facing overlay over `observability` and chart telemetry | Telemetry exists | T8 |
| M7.6 | Themes and accessibility: dark, light and colorblind-safe palettes, font scaling | Partial | Design-system tokens only, per the coordination rules in `AGENTS.md` | — | T8 |
| M7.7 | Layout and settings sync through a user-controlled folder | Absent | Export and import of workspace state with conflict handling; no Aeris service | — | T8 |
| M7.8 | Distribution and updates | Partial (disabled) | Free release hosting when a release path is approved; signed manifests already exist | — | T8 |

## Verification

For every item:

- Deterministic tests at the owning boundary with sanitized fixtures faithful to real provider
  shapes; no credentials or raw private payloads in fixtures.
- Real-path evidence for provider, persistence, rendering and installed-app behavior; compilation
  alone is not proof.
- Bounded queues, caches, stores and retries with explicit overflow behavior, verified under burst
  and soak workloads in release builds.
- Restart and recovery: locks, rule state, journal records and recordings survive restart and
  partial failure.

The broad gates in `AGENTS.md` run once at the end of each batch, before its commit.

## Definition of completion

Aeris Terminal meets this roadmap when a futures trader can trade live Rithmic accounts from the
DOM, chart and hotkeys with enforced prop-firm rules; read order flow, depth and order-level
analytics in real time; replay and review every session with journal analytics; and see the
relevant fundamentals beside each contract, entirely on their own machine, with no Aeris server in
the market-data path and every catalog item above qualified. Until then, report delivered items and
remaining gaps precisely against the batch checklists and IDs in this roadmap.
