# Aeris Trading Platform Feature Roadmap

## Purpose

This roadmap defines the trading features that turn Aeris from a market-data and charting
terminal into a complete local-first futures trading platform. Its product goal is:

> The fastest futures platform, the one that protects traders from breaking their own and their
> prop firm's rules, and the one that shows the order book and the real-world fundamentals behind
> every contract, while running entirely on the user's machine.

It complements `plan/study_runtime_sdk_roadmap.md`, which remains the authority for studies, the
Study SDK and the in-app Study Editor. Chart-side rendering prerequisites live in the Nucleus
Charts `Expansion.md` (items F1–F6, OF1–OF18, CT1–CT6 and the platform-driven PD1–PD10). This
roadmap names those dependencies by ID and does not restate engine design.

Ownership split: Nucleus renders what appears inside a chart. Aeris owns every panel and
product rule outside it. The DOM ladder (`terminal_ui`), time and sales, all risk rules, and the
trading lock are platform features. The lock needs no engine support, because the platform
already decides which chart trading gestures reach Nucleus and whether to act on the resulting
trading intents.

Every item must satisfy the five core principles and the architecture invariants in `AGENTS.md`.
Where a feature conflicts with a current invariant, this roadmap records the conflict as a
maintainer decision instead of designing around it.

## Operating constraints

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
   data or a local simulated venue should proceed first.

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
| Instrument metadata | Provider price increment at install time; Rithmic reference carries minimum price change, expiration date and point value; the canonical instrument model has no tick size, point value or expiry fields | `contracts/src/messages.rs`, rithmic `catalog.rs`, `domain/instruments` |
| Account runtime | Aeris SaaS sign-in (OIDC, lease, vault); disabled in development builds; not a broker trading account | `crates/account_runtime`, `apps/desktop/src/account.rs` |
| Order book panel | Read-only price ladder with aggressor volume columns; the P/L column is an empty placeholder | `crates/ui/terminal_ui`, `SidePanel::OrderBook` |
| Charts in the desktop | Candles, bars, line, area, baseline; drawings; price-alert lines; split workspace; study-runtime indicators. Footprint, volume profile, heatmap and trading lines exist in Nucleus but are not wired into the desktop | `crates/ui/chart_integration` |
| Studies | Study runtime and SDK Phases A–E complete; editor and sandbox pending | `plan/study_runtime_sdk_roadmap.md` |
| Alerts | Price alerts evaluated in `market_runtime` on live trades (32 per consumer) with OS notifications | `PriceAlertRegistry`, `platform_runtime/user_notifications.rs` |
| Persistence | Workspace layouts, chart preferences, studies, alerts, watchlist and the credential vault. Market-history persistence is deliberately banned | `workspace_persistence.rs`, `tools/naming_check` |
| Recording, replay, journal, simulator | **Absent.** "Replay" in code means bar snapshot contracts and Rithmic history requests, not session playback | `application/src/replay_snapshot.rs` |
| Risk controls | **Absent** | — |
| Context data | **Absent** except a CME Globex session-day helper for week and month history buckets | `rithmic_protocol/src/calendar.rs` |
| Workspace UX | Tabs, split panes and their shortcuts, themes, feed diagnostics; no command palette, trading hotkeys, multi-window workspace or linked symbol groups | `apps/desktop`, `crates/observability` |
| Distribution | Launcher, signed release identity and lifecycle code; automatic updates and publication disabled until a release backend exists | `platform_runtime`, `apps/desktop/src/update.rs` |

## Maintainer decisions required

These decisions block specific items. Each is listed with the recommendation from this roadmap.

| ID | Decision | Blocks | Recommendation |
| --- | --- | --- | --- |
| D1 | Owner for broker accounts, orders, fills and positions. `account_runtime` is Aeris SaaS identity and should not absorb broker trading | PF1–PF3 and every trading feature | One in-process trading owner beside `market_runtime`, with the same single-session, bounded, generation-fenced rules. Provider sessions for orders stay provider-owned and shared, never per chart or panel |
| D2 | Session recording versus the ban on market-history persistence | PF8, M4 replay and review, realistic simulation, local backtesting | Amend the invariant narrowly: allow an explicit, opt-in, user-owned session recording store that is separate from and never feeds the on-demand history cache. Update `AGENTS.md` and `tools/naming_check` in the same change so the ban on a second market-state model remains enforced |
| D3 | Local storage engine for user-owned records (journal, executions, rule profiles, plans) | PF7 | Choose one embedded store after measuring; do not introduce more than one |
| D4 | Order-level data leaving the Rithmic adapter | PF4, M1.6 | Publish a bounded canonical order-level view through `domain/market_data` with provider identity, local order and sequence evidence kept separate, as `AGENTS.md` requires |
| D5 | First additional provider (CQG or dxFeed) | PF10 | Defer until Rithmic trading is qualified; prioritize by user demand |
| D6 | Calendar distribution without a backend | M5.1 | Ship the curated event calendar as signed data with application releases, or fetch it from a public repository file; decide once a release path exists |

## Foundations

Foundations unblock most modules. Each ships through the real runtime and desktop paths with
deterministic tests, bounded resources and naming-check coverage.

### PF1 — Rithmic Order plant integration

- **Outcome.** Submit, modify and cancel orders; market, limit, stop and stop-limit types;
  server-side brackets, OCO and trailing stops where Rithmic supports them; order status, fill and
  rejection updates; account list.
- **Rules.** Prefer server-side order types so protection survives an application crash. Any
  order managed locally must be labeled as local in the UI. Every command is idempotent through
  client order identifiers and fenced by session generation.
- **Gate.** Rithmic onboarding and conformance. Verify every template and field against the
  licensed Provider Kit before depending on it.

### PF2 — Rithmic PnL plant and account state

- **Outcome.** Positions, realized and unrealized PnL, account balance and margin, and account risk
  limits where the account exposes them (many prop-firm accounts carry loss limits in the broker's
  risk system).
- **Gate.** Rithmic onboarding. Broker-reported limits are authoritative when present; local rule
  profiles (M3.1) complement them.

### PF3 — Canonical trading domain

- **Outcome.** Provider-neutral fixed-point models for accounts, orders, order events, fills,
  positions and PnL, with explicit scales and provenance, in a domain crate owned by D1.
- **Rule.** Chart trading geometry in Nucleus receives projections of these models; Nucleus never
  owns order state.

### PF4 — Canonical order-level book publication

- **Outcome.** A bounded order-level view (order identity, side, price, size, priority evidence and
  sequence) published from the Rithmic adapter per D4, alongside the existing aggregated book.
- **Consumers.** Queue position, iceberg, pulled-liquidity and size-cluster analytics (M1.6), the
  DOM ladder's order counts and queue column, and PD8 in Nucleus.

### PF5 — Trade tape publication

- **Outcome.** A bounded classified trade stream from `market_runtime` to consumers (time and
  sales, footprint, CVD, big-trade detection), coalesced for slow consumers, with generation
  fencing. It feeds the Nucleus shared tape (F2) as a projection.

### PF6 — Contract metadata

- **Outcome.** Canonical tick size, point value, currency, expiry, first notice and last trade
  dates, and session hours per instrument, sourced from provider reference data.
- **Consumers.** PnL in ticks and currency, risk checks, roll calendar (M5.6), DOM tick grid.

### PF7 — Local store for user-owned records

- **Outcome.** Durable storage for executions, journal entries, tags, notes, screenshots, rule
  profiles, rule evaluations, session plans and analytics caches, per D3.
- **Rules.** Background-thread I/O only, schema versioning and migrations, bounded growth with
  user-visible retention settings, and export to open formats (CSV, JSON).

### PF8 — Session recording store

- **Outcome.** Opt-in recording of the user's own subscribed trades, depth updates, quotes and
  bars, compressed on local disk with per-session manifests, integrity checks and retention caps.
- **Gate.** D2 and the data licensing checklist.
- **Rule.** Recordings are replay inputs only. They never become a history cache for live charts.

### PF9 — Local simulated venue

- **Outcome.** A local execution venue with the same order, fill and position contracts as PF1–PF3,
  driven by live or replayed market data. Fill models: simple touch fills first, then queue-aware
  fills from order-level data (M4.5).
- **Why now.** It lets the complete trading, risk and journal experience be built and tested
  before Rithmic order routing is approved, and powers paper trading afterward.
- **Rule.** Simulated accounts are always visually distinct from live accounts.

### PF10 — Additional providers

- **Outcome.** CQG and dxFeed adapters behind the same provider-neutral contracts, per D5.

## Feature catalog

Status values: **Present**, **Partial**, **Absent**. "Nucleus" lists chart-engine prerequisites by
their `Expansion.md` ID.

### M1 — Charts and order flow

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M1.1 | Liquidity heatmap: resting depth over time with trades overlaid, color scaling, thresholds, minimum-size filter | Absent | Feed canonical depth publications and PF5 trades into the chart; heatmap settings UI | F3, OF15, PD8, PD9 |
| M1.2 | Footprint charts: bid×ask, delta, total and imbalance views; POC; stacked imbalances | Absent in desktop (present in Nucleus) | Wire the Nucleus footprint series to PF5; settings UI; persist footprint configuration | Existing footprint; F2, OF12, PD10 |
| M1.3 | Volume profile, VWAP bands and TPO: session, composite, fixed-range and anchored | Absent in desktop (visible-range profile present in Nucleus) | Session definitions from PF6; profile settings; drawing tools in the toolbar | OF3–OF10, F6 |
| M1.4 | Cumulative delta and delta divergence | Absent | Divergence alert rule over study outputs (M6.1) | F2, OF1, OF2 |
| M1.5 | Big trades and sweeps: large prints and multi-level sweeps with adaptive per-contract thresholds | Absent | Sweep grouping by timestamp and aggressor from PF5; threshold settings | F2, OF11, PD8 |
| M1.6 | Order-level intelligence: own-order queue position and fill likelihood, iceberg refill detection, pulled-liquidity detection, order-size clustering | Absent | Deterministic, parameterized detectors over PF4 in the platform; labeled as estimates; never presented as certainty. The DOM shows queue position and detections in its own columns | PD8 and PD1 for the chart; none for the DOM |
| M1.7 | Time and sales panel with size, side and price filters | Absent | Bounded filtered window over PF5; GPUI list panel owned by the platform | — |
| M1.8 | Tick, volume and range charts, with footprint on the same bars | Absent | Interval picker and persistence | F1, OF14, CT3 |

Acceptance: every order-flow view renders from shared canonical streams without duplicate provider
demand, and frame work stays bounded during news-release bursts.

### M2 — Trading and execution

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M2.1 | DOM trading ladder: one-click orders, drag to modify, working orders and position inline, P/L column, recent volume at price | Partial (read-only ladder) | Extend the existing `terminal_ui` ladder into a trading ladder over PF1–PF3: order-entry clicks and drags, own orders and queue position (PF4), fill the existing P/L column; measure ladder render cost during news bursts | — (platform widget) |
| M2.2 | Order entry panel: quantity presets, order types, time in force, account selector | Absent | GPUI panel over PF1 | — |
| M2.3 | Bracket and strategy templates: stop and target on entry, OCO, trailing, break-even, scale-out | Absent | Template model in PF7; server-side execution where supported; local-management labels otherwise | Existing brackets and OCO visuals |
| M2.4 | Chart trading: place, drag and cancel orders and positions on the chart | Absent in desktop (present in Nucleus) | Map Nucleus trading intents to PF1 commands with confirmation rules; stop forwarding trading gestures while a lock is active | Existing trading layer; PD1 |
| M2.5 | Trading hotkeys: buy/sell at bid/ask/market, flatten, cancel all, reverse, with per-hotkey confirmation settings | Absent | Keymap owner (M7.2); hotkeys disabled while a text field has focus | — |
| M2.6 | Flatten and kill switch: flatten all positions and cancel all orders per account or globally | Absent | Single command path through PF1, visible at all times | — |
| M2.7 | Multi-account trade copier: mirror orders to several accounts with per-account multipliers and kill switches | Absent | Local copier over PF1 with per-account risk checks (M3.2) before each mirrored order | — |
| M2.8 | Positions and PnL display in currency and ticks | Absent | PF2 and PF6 projections to panels and charts | Existing position chips |

Acceptance: no order leaves the machine without passing M3.2 checks; every trading command is
idempotent across reconnects; simulated and live accounts can never be confused.

### M3 — Risk and discipline

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M3.1 | Prop-firm rule engine: profiles for daily loss limit, trailing drawdown (intraday or end-of-day), max contracts, consistency rules and news-time restrictions; live distance meters | Absent | Declarative, versioned rule profiles in PF7; deterministic evaluation on every fill and price update; broker-reported limits (PF2) take precedence when present | PD1 |
| M3.2 | Pre-trade checks and hard locks: warn on or block any order that would break a rule if its stop is hit; a day lock the trader cannot easily undo | Absent | Check runs inside the single order-command path, including the DOM, chart trading, copier and hotkeys. While locked, the desktop stops forwarding chart trading gestures to Nucleus, rejects any trading intent, disables DOM order entry and shows the lock reason in platform chrome | PD1 for warnings on order lines; lock needs none |
| M3.3 | Tilt detection: rapid losses, rising size after losses, fast re-entry after a stop, trading outside planned hours | Absent | Documented deterministic rules over the local execution log; actions are warn, cool-down timer (through the M3.2 lock) or size reduction | — |
| M3.4 | Session plan and checklist: key levels, bias, max loss and allowed setups before trading; end-of-day adherence review | Absent | Plan model in PF7; plan levels projected as host overlays | PD3 |

Acceptance: rule evaluation is reproducible from recorded fills; a lock survives application
restart; prop-firm profiles are versioned because firms change their rules.

### M4 — Recording, replay and review

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M4.1 | Session recorder | Absent | PF8 | — |
| M4.2 | Market replay at 1× to 100× with practice trading | Absent | Replay data source feeding the same series and study paths; practice orders through PF9 | PD2, F1 |
| M4.3 | Trade review: jump from any past trade to that moment with order book, footprint and executions as they were | Absent | Link PF7 executions to PF8 recordings; the DOM replays from the same recording | PD2, PD4 |
| M4.4 | Automatic journal and analytics: every execution logged; tags, notes and screenshots; win rate, expectancy, maximum adverse and favorable excursion, results by time of day, setup and contract; plain-language findings | Absent | Round-trip grouping and statistics over PF7; screenshots through chart export | PD4, PD6 |
| M4.5 | Realistic paper trading: fills that respect queue position from recorded order-level data | Absent | Queue-aware fill model in PF9 over PF4 and PF8 | — |

Acceptance: replay never shows data after the replay clock; a replayed session produces the same
derived results as the live session did; journal statistics are reproducible from stored records.

### M5 — Fundamentals and context (free public data)

| ID | Feature | Status | Data source | Nucleus |
| --- | --- | --- | --- | --- |
| M5.1 | Economic event calendar with countdowns, importance, and automatic flatten or lock before events when a rule profile requires it | Absent | Official release schedules (BLS, BEA, Federal Reserve, EIA), distributed per D6 | PD3 |
| M5.2 | Energy dashboard: weekly petroleum inventories and natural gas storage with surprise versus the five-year range; heating and cooling degree days | Absent | EIA open data API (user key); NOAA Climate Prediction Center | PD7 |
| M5.3 | Commitments of Traders positioning by trader category beside price | Absent | CFTC public reporting data | PD7 |
| M5.4 | Grains and agriculture: crop progress, WASDE supply and demand, export sales | Absent | USDA (NASS Quick Stats with a user key; WASDE and export-sales publications) | PD7 |
| M5.5 | Macro panel: Treasury yields, dollar index, inflation and employment series | Absent | FRED API (user key); respect per-series copyright terms | PD7 |
| M5.6 | Contract roll and expiry calendar: front month, days to roll, volume migration, first notice and last trade dates | Partial (expiry strings from Rithmic reference data) | PF6 plus provider volume | PD3 |
| M5.7 | Session and trading-hours display: RTH and ETH boundaries, holiday closures | Partial (session-day bucketing helper only) | PF6 and published exchange calendars | PD3 |

Rules: one bounded background owner fetches, caches and schedules public data; nothing blocks the
UI thread; every value carries its release timestamp and source so charts never show look-ahead;
missing keys or source outages degrade to a clear "unavailable" state, never invented values.

### M6 — Automation and research

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M6.1 | Alert expansion: order-flow conditions (sweep, delta divergence, iceberg), rule-meter warnings, upcoming events and study alerts | Partial (price alerts) | Extend `market_runtime` alert evaluation; study alerts follow study roadmap Phase J | Alert lines exist |
| M6.2 | Alert delivery: desktop notifications (present) plus optional Telegram or Discord through the user's own bot or webhook | Partial | Outbound messages follow the data licensing checklist | — |
| M6.3 | Custom indicators and the in-app Rust Study Editor | Partial | `plan/study_runtime_sdk_roadmap.md` Phases F–L | I4, F4 |
| M6.4 | Strategies and local backtesting over recorded sessions with order-book-aware fills | Absent | Separate strategy program, as the study roadmap requires; depends on PF8 and PF9 | — |
| M6.5 | Optional local AI assistant: journal summaries, plain-language questions about the user's own trades, study-authoring help | Absent | Runs through a local model runtime or the user's own API key; receives user-owned records only, never raw exchange data leaving the machine | — |

### M7 — Workspace and experience

| ID | Feature | Status | Platform work | Nucleus |
| --- | --- | --- | --- | --- |
| M7.1 | Command palette with searchable actions and Bloomberg-style mnemonics, for example `ES footprint 5m` | Absent | Command registry shared by menus, palette and hotkeys | — |
| M7.2 | Configurable keymap with conflict detection | Partial (window and split shortcuts) | Single keymap owner; trading hotkeys (M2.5) register here | — |
| M7.3 | Multi-window and multi-monitor workspaces | Partial | Detachable windows with persisted placement per display | — |
| M7.4 | Linked symbol groups and synchronized crosshair and time range | Absent | Link-group coordinator in the desktop | PD5 |
| M7.5 | Performance mode and diagnostics overlay: per-panel render and memory cost, feed latency, dropped frames; the promise that the app never freezes during news | Partial (feed diagnostics) | User-facing overlay over `observability` and chart telemetry | Telemetry exists |
| M7.6 | Themes and accessibility: dark, light and colorblind-safe palettes, font scaling | Partial | Design-system tokens only, per the coordination rules in `AGENTS.md` | — |
| M7.7 | Layout and settings sync through a user-controlled folder | Absent | Export and import of workspace state with conflict handling; no Aeris service | — |
| M7.8 | Distribution and updates | Partial (disabled) | Free release hosting when a release path is approved; signed manifests already exist | — |

## Delivery sequence

Each phase ends only when its acceptance criteria pass through the real runtime, desktop and
executor paths.

### Phase 0 — Build now, before Rithmic onboarding

- PF3, PF5, PF6, PF7 and PF9: canonical trading domain, trade tape, contract metadata, local
  record store and the simulated venue.
- M2 against the simulated venue: DOM trading ladder, order entry, brackets, chart trading,
  hotkeys, flatten, copier.
- M3 in full against the simulated venue.
- M1.2, M1.3, M1.4, M1.5 and M1.7 using Hyperliquid public trades, which already carry aggressor
  sides.
- M5.1–M5.5, M7.1, M7.2 and M7.4.
- Nucleus: PD1, PD3, PD5, PD7 and PD10; E1–E2 of `Expansion.md`.

### Phase 1 — Rithmic onboarding and live trading

- PF1 and PF2, conformance, then M2 and M3 qualified on live Rithmic accounts.
- PF4 and M1.6 once order-level publication (D4) is approved.

### Phase 2 — Record, replay and review

- D2 decision, PF8, then M4.1–M4.4.
- Nucleus: PD2, PD4 and PD6.

### Phase 3 — Institutional depth

- M1.1, M1.8, M4.5, M5.6 and M5.7.
- Nucleus: F1, F3, OF14, OF15, PD8 and PD9.

### Phase 4 — Power users

- M6.1–M6.5 and the study roadmap Phases F–L, M7.3, M7.5–M7.8, and PF10.

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
- The broad gates in `AGENTS.md` before delivery.

## Definition of completion

Aeris meets this roadmap when a futures trader can trade live Rithmic accounts from the DOM,
chart and hotkeys with enforced prop-firm rules; read order flow, depth and order-level analytics
in real time; replay and review every session with journal analytics; and see the relevant
fundamentals beside each contract, entirely on their own machine, with no Aeris server in the
market-data path and every catalog item above qualified. Until then, report delivered items and
remaining gaps precisely against the IDs in this roadmap.
