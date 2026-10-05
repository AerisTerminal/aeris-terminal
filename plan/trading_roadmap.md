# Aeris Trading Roadmap

Aeris Terminal is becoming a complete local-first futures trading platform:

> The fastest futures platform, the one that protects traders from breaking their own and their
> prop firm's rules, and the one that shows the order book and the real-world fundamentals behind
> every contract, while running entirely on the user's machine.

This file is the single status and requirements record for that work. Related documents:

- `plan/study_runtime_sdk_roadmap.md` owns studies, the Study SDK and the in-app Study Editor.
- The Aeris Charts repository's `plan/Expansion.md` owns chart-engine prerequisites (F1–F6,
  OF1–OF18, CT1–CT6, I1–I4, PD1–PD11, grouped into batches B1–B9). This file names them by ID only.

Item IDs are stable: **T** batches, **PF** foundations, **M** features, **D** decisions.

**Contents**

1. [Status](#1-status)
2. [Next work](#2-next-work)
3. [Batches](#3-batches)
4. [Feature catalog](#4-feature-catalog)
5. [Foundations](#5-foundations)
6. [Decisions](#6-decisions)
7. [Constraints](#7-constraints)
8. [Current baseline](#8-current-baseline)
9. [Broker coverage](#9-broker-coverage)
10. [Delivery and verification](#10-delivery-and-verification)

---

## 1. Status

Updated 2026-10-05. A status is **Complete** only when every item works through the real runtime and
desktop path, not when a unit test alone passes.

| Batch | Scope | Status | Blocked by |
| --- | --- | --- | --- |
| [T1](#t1--trading-foundations) Trading foundations | D1, D3, PF3, PF6, PF7, PF9 | **Complete** | — |
| [T2](#t2--trading-execution-and-risk-on-the-simulated-venue) Trading, execution and risk | M2.1–M2.8, M3.1–M3.4, M7.2 | **Partial**: engine complete, desktop gaps | — |
| [T3](#t3--order-flow-on-hyperliquid-data) Order flow | PF5, M1.2, M1.4, M1.5, M1.7 | **Complete** | — |
| [T4](#t4--context-and-workspace) Context and workspace | D6, M5.1–M5.5, M7.1, M7.4 | **Complete**; keyed sources not live-qualified | Maintainer API keys (qualification only) |
| [T5](#t5--rithmic-live-trading) Rithmic live trading | PF1, PF2, PF4, M1.6, live T2 | **Blocked**; adapter order and PnL plant sessions exist, not wired to `trading_runtime` | Rithmic onboarding and conformance; D4 |
| [T6](#t6--record-replay-and-review) Record, replay and review | D2, PF8, M4.1–M4.4 | **Open** | D2; data licensing checklist |
| [T7](#t7--institutional-depth) Institutional depth | M1.1, M1.3, M1.8, M4.5, M5.6, M5.7 | **Open** | T6; Aeris Charts B4–B7; licensing checklist |
| [T8](#t8--power-users) Power users | M6.1–M6.5, M7.3, M7.5–M7.8, PF10 | **Open** | D5; Aeris Charts B9; a release path for M7.8 |

**Progress:** 3 of 8 batches complete, T2 partial. Of 45 catalog features, 17 are Present, 15 are
Partial and 13 are Absent (see [Feature catalog](#4-feature-catalog)). Foundations: 5 of 10 delivered.

**Order:** T1 unblocks T2, T5, T6 and part of T7. T2 and T3 are independent. T5 starts when Rithmic
onboarding clears and reuses T2 unchanged against the live venue. T6 needs T1's store and T2's
execution log. Record any change of order here.

---

## 2. Next work

Close T2 before starting new batches. None of this has an external blocker.

| Gap | Affects | Current state | Required |
| --- | --- | --- | --- |
| Rule-profile editor | M3.1, M3.2, M5.1 | `TradingService::register_risk_profile` has no desktop caller, so no profile can be created and event flatten/lock rules can never activate | Create, edit and version prop-firm profiles per account in the desktop |
| Session-plan editor | M3.3 (planned hours), M3.4 | `register_session_plan` has no desktop caller; the panel always shows "Plan · None" | Create the plan and checklist before the session; show the adherence review |
| Bracket-template editor | M2.3 | `register_strategy_template` has no desktop caller; only ad hoc chart brackets exist, so the ticket's Bracket selector is always "Off" | Create, edit and enable templates, including trailing, break-even and scale-out |
| Lock lifecycle | M2.6, M3.2 | `RiskLock` has no expiry; `trading::unlock_simulated_account` wraps `unlock_account` but no UI action calls it, so a kill switch or tripped rule locks the account permanently | Session-boundary reset per profile, and a deliberate, confirmed unlock path |
| Hotkey coverage | M2.5, M7.2 | Only buy and sell at market, cancel all, flatten and kill are bound; bindings cannot be changed | Buy and sell at bid and ask, reverse, per-hotkey confirmation, user rebinding with conflict detection |
| Keyed context qualification | T4 | EIA, USDA NASS/FAS and FRED paths never exercised with real keys | Live-qualify with maintainer keys |

When these land, set T2 to Complete, update the affected catalog rows, and record the evidence
under T2.

---

## 3. Batches

Each batch lists its scope, dependencies, checklist and acceptance criteria. Evidence records what
was measured when the batch was delivered.

### T1 — Trading foundations

**Scope:** D1, D3, PF3, PF6, PF7, PF9 · **Needs:** nothing · **Status:** Complete

- [x] **D1** One in-process trading owner beside `market_runtime`.
- [x] **D3** One embedded local store, chosen after measurement.
- [x] **PF3** Canonical fixed-point accounts, orders, order events, fills, positions and PnL.
- [x] **PF6** Tick size, point value, currency, expiry, first notice, last trade and session hours in
      the canonical instrument model.
- [x] **PF7** Local store with background I/O, schema versions, migrations, bounded retention and
      CSV/JSON export.
- [x] **PF9** Local simulated venue with touch fills and visually distinct simulated accounts.
- [x] `tools/naming_check` enforces the trading-owner boundary; broad gate green; pushed.

**Acceptance:** a simulated order is placed, filled and reflected in positions and PnL through the
trading owner, and records survive restart.

**Evidence:** `crates/trading_runtime` owns the bounded command worker, SQLite store, simulated
execution and restart/export path. Release storage measurement: 500 executions and 2,000 user
records written in 2.619 s, reopened in 4.47 ms, 831,488 bytes on disk. The desktop readiness
command produced schema-4 readiness with market, account and trading services ready.

### T2 — Trading, execution and risk on the simulated venue

**Scope:** M2.1–M2.8, M3.1–M3.4, M7.2 · **Needs:** T1; the existing Aeris Charts trading layer;
Aeris Charts B1 (PD11, PD1, PD3) · **Status:** Partial — see [Next work](#2-next-work)

Execution and risk ship together: no order may leave the order-command path without the M3.2 checks.

- [x] **M2.1** DOM trading ladder: one-click orders, drag to modify, inline orders and position,
      P/L column, recent volume at price; render cost measured during bursts.
- [x] **M2.2** Order entry panel: quantity presets, order types, time in force, account selector.
- [ ] **M2.3** Bracket and strategy templates. *Runtime complete; no template editor.*
- [x] **M2.4** Chart trading through the M3.2 order-command path, with partially filled, pending
      modify and pending cancel states.
- [ ] **M2.5** Trading hotkeys, disabled while a text field has focus. *Bid/ask, reverse and
      per-hotkey confirmation missing.*
- [ ] **M2.6** Flatten and kill switch per account and globally. *Works; lock cannot be released.*
- [x] **M2.7** Multi-account copier with per-account multipliers, kill switches and M3.2 checks.
- [x] **M2.8** Positions and PnL in currency and ticks.
- [ ] **M3.1** Prop-firm rule engine with versioned profiles and live meters. *Runtime complete; no
      profile editor.*
- [x] **M3.2** Pre-trade checks and hard locks in the single order-command path, surviving restart
      and blocking DOM, chart, copier and hotkeys.
- [ ] **M3.3** Tilt detection. *Cooldown, size cap and fast re-entry work; planned-hours rule needs
      a session plan.*
- [ ] **M3.4** Session plan and checklist with plan levels on charts. *Runtime complete; no plan
      editor.*
- [ ] **M7.2** Single keymap owner with conflict detection. *Owner and conflict checks exist; no
      user rebinding.*
- [ ] Broad gate green after the gaps close; pushed.

**Acceptance:** every command is idempotent across reconnects, simulated and live accounts cannot
be confused, and rule evaluation is reproducible from recorded fills.

**Evidence (runtime, 2026-09-26):** `trading_runtime` carries schema-v13 durable accounts, orders,
fills, managed strategies, copier configurations, versioned risk profiles, rule state, consistency
cycles, session plans and discipline state. Risk evaluation covers daily loss, intraday and
end-of-day trailing drawdown, all-working-order maximum contracts, completed-trade consistency, news
restrictions and exact bracket loss-at-stop. The DOM rendered 512 changing frames in 1.490 s with a
maximum render-tree cost of 56,000 ns against a 16 ms budget.

**Fixes (2026-09-27):** the DOM never received depth when the order book opened while a restored
Hyperliquid instrument was still resolving (`b88110d`); order-entry controls reorganized into
aligned sections (`4f33c00`).

### T3 — Order flow on Hyperliquid data

**Scope:** PF5, M1.2, M1.4, M1.5, M1.7 · **Needs:** Aeris Charts B3 · **Status:** Complete

Hyperliquid public trades carry aggressor sides, so order flow is built before Rithmic onboarding.

- [x] **PF5** Bounded, classified, generation-fenced trade tape, coalesced for slow consumers and
      feeding the chart's shared tape (F2).
- [x] **M1.2** Footprint charts with settings UI and persisted configuration.
- [x] **M1.4** Cumulative delta and a delta-divergence alert rule.
- [x] **M1.5** Big trades and sweeps with adaptive per-contract thresholds.
- [x] **M1.7** Time and sales with size, side and price filters.
- [x] Burst workload measured in a release build; broad gate green; pushed.

**Acceptance:** every view renders from one shared canonical stream without duplicate provider
demand, and frame work stays bounded during bursts.

**Evidence:** one tape per active market, bounded to 65,536 trades and eight minutes. A release
workload applied 65,536 trades in 512 bursts in 467.53 ms total; the largest frame was 2.26 ms
against a 16 ms budget.

**Fixes (2026-09-27):** Aeris Charts B3 `dcb86c8` drew large-trade bubbles as arrows, and
Hyperliquid instruments carried no price increment, so footprint rows used a `1e-8` step. Aeris
Charts `5d9bd60` draws price-centred, volume-scaled bubbles, and the adapter publishes the documented
significant-figure increment. The terminal pins `5d9bd60`.

### T4 — Context and workspace

**Scope:** D6, M5.1–M5.5, M7.1, M7.4 · **Needs:** Aeris Charts B1 (PD3, PD5, PD7) · **Status:**
Complete; keyed sources not live-qualified

- [x] **D6** Calendar distribution decided.
- [x] One bounded background owner for public data fetching, caching and scheduling.
- [x] **M5.1** Economic calendar with countdowns and rule-driven flatten or lock. *Rule actions
      need a profile; see [Next work](#2-next-work).*
- [x] **M5.2** Energy dashboard (EIA, NOAA).
- [x] **M5.3** Commitments of Traders.
- [x] **M5.4** Grains and agriculture (USDA).
- [x] **M5.5** Macro panel (FRED).
- [x] **M7.1** Command palette with a shared command registry.
- [x] **M7.4** Linked symbol groups with synchronized crosshair and time range.
- [x] Broad gate green; pushed.

**Acceptance:** every value carries its source and release time, charts show no look-ahead, and
missing keys or outages degrade to a clear unavailable state.

**Evidence:** `context_runtime` owns a 64-command queue, staggered refresh schedule, 4 MiB response
cap and a 4,096-item-per-collection view. Keys are held only by the native credential vault. Live
shape tests passed against BEA, Federal Reserve, NOAA CPC, CFTC and USDA WASDE; EIA, NASS, FAS and
FRED were not exercised because no maintainer keys were present.

**Fixes (2026-09-27):** the context panel rendered under the drawing toolbar, closed through a text
button and had a fixed height; it now follows the toolbar inset, uses the shared close icon and is
resizable with a persisted height (`5cb343a`).

### T5 — Rithmic live trading

**Scope:** PF1, PF2, PF4, M1.6, live qualification of T2 · **Needs:** T1, T2; Aeris Charts B1 (PD1),
B6 (PD8) · **Status:** Blocked until Rithmic grants live accounts and order-routing conformance

- [~] Order plant and PnL plant sessions in `rithmic_protocol` (`11625ca7`): login info, account
      list, trade routes, order updates and snapshot, submit, modify, cancel, cancel all, execution
      replay, fill history, and position snapshot and updates, with allowlisted outbound templates
      and fixed-point decimals. Verification against the licensed Provider Kit and a live Rithmic
      Test exchange is not recorded.
- [ ] Production endpoints and a system/gateway picker from Rithmic's system list; the adapter
      connects only to the hardcoded Rithmic Test endpoint (`endpoint.rs`).
- [ ] **PF1** Order routing with idempotent client order IDs, as a `trading_runtime` venue beside
      the simulated venue.
- [ ] **PF2** Positions, PnL, balance, margin and broker risk limits.
- [ ] Rithmic conformance passed.
- [ ] T2 features qualified on live accounts, including the copier across several accounts.
- [ ] **D4** decided; **PF4** bounded canonical order-level publication.
- [ ] **M1.6** Queue position, iceberg, pulled-liquidity and size-cluster detectors, labeled as
      estimates.
- [ ] Broad gate green; pushed.

**Acceptance:** live Rithmic accounts trade from the DOM, chart and hotkeys with enforced rules, and
reconnects never duplicate or lose commands.

### T6 — Record, replay and review

**Scope:** D2, PF8, M4.1–M4.4 · **Needs:** T1, T2; Aeris Charts B1 (PD4, PD6), B5 (PD2, F1) ·
**Status:** Open, blocked on D2

- [ ] **D2** decided; `AGENTS.md` and `tools/naming_check` amended in the same change.
- [ ] Data licensing checklist reviewed for local recording.
- [ ] **PF8 / M4.1** Opt-in session recorder with manifests, integrity checks and retention caps.
- [ ] **M4.2** Market replay at 1× to 100× with practice trading through PF9.
- [ ] **M4.3** Trade review with book, footprint and executions as they were.
- [ ] **M4.4** Automatic journal and analytics with screenshots through PD6.
- [ ] Broad gate green; pushed.

**Acceptance:** replay never shows data after the replay clock, replayed sessions reproduce live
derived results, and journal statistics are reproducible from stored records.

### T7 — Institutional depth

**Scope:** M1.1, M1.3, M1.8, M4.5, M5.6, M5.7 · **Needs:** T3, T6; Aeris Charts B4, B5, B6, B7 ·
**Status:** Open

- [ ] **M1.1** Liquidity heatmap from canonical depth and PF5 trades.
- [ ] **M1.3** Session, composite, fixed-range and anchored profiles, VWAP bands and TPO.
- [ ] **M1.8** Tick, volume and range charts with footprint on the same bars.
- [ ] **M4.5** Queue-aware paper fills from recorded order-level data.
- [ ] **M5.6** Contract roll and expiry calendar.
- [ ] **M5.7** Session and trading-hours display.
- [ ] Broad gate green; pushed.

**Acceptance:** depth views stay bounded under dense books and match the recorded session in replay.

### T8 — Power users

**Scope:** M6.1–M6.5, M7.3, M7.5–M7.8, PF10, study roadmap Phases F–L · **Needs:** Aeris Charts B9
(I4) · **Status:** Open

- [ ] **M6.1** Order-flow, rule-meter, event and study alerts.
- [ ] **M6.2** Optional Telegram or Discord delivery.
- [ ] **M6.3** Custom indicators and the in-app Study Editor.
- [ ] **M6.4** Strategies and local backtesting over recorded sessions.
- [ ] **M6.5** Optional local AI assistant over user-owned records only.
- [ ] **M7.3** Multi-window and multi-monitor workspaces.
- [ ] **M7.5** Performance mode and diagnostics overlay.
- [ ] **M7.6** Themes and accessibility.
- [ ] **M7.7** Layout and settings sync through a user-controlled folder.
- [ ] **M7.8** Distribution and updates once a release path is approved.
- [ ] **D5** decided; **PF10** additional provider.
- [ ] Broad gate green; pushed.

**Acceptance:** the [definition of completion](#definition-of-completion) is met.

---

## 4. Feature catalog

**Present** works end to end in the desktop. **Partial** exists but misses part of its definition.
**Absent** has not started. The Aeris Charts column lists `Expansion.md` prerequisites.

### M1 — Charts and order flow

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M1.1 | Liquidity heatmap: resting depth over time with trades, color scaling, thresholds, minimum size | Absent | Feed canonical depth and PF5 trades into the chart; settings UI | F3, OF15, PD8, PD9 | T7 |
| M1.2 | Footprint: bid×ask, delta, total and imbalance views; POC; stacked imbalances | Present | Consumes PF5; configurable and persisted | F2, OF12, PD10 | T3 |
| M1.3 | Volume profile, VWAP bands and TPO: session, composite, fixed-range, anchored | Absent (Volume Profile (Visible Range) is in the indicator menu since `df9f2cef`) | Session definitions from PF6; profile settings; toolbar tools | OF3–OF10, F6 | T7 |
| M1.4 | Cumulative delta and delta divergence | Present | Shared-tape delta and completed-bar divergence alerts | F2, OF1, OF2 | T3 |
| M1.5 | Big trades and sweeps with adaptive per-contract thresholds | Present | Bounded sweep grouping; adaptive or explicit threshold | F2, OF11, PD8 | T3 |
| M1.6 | Order-level intelligence: queue position, icebergs, pulled liquidity, size clustering | Absent | Deterministic detectors over PF4, labeled as estimates; DOM columns | PD8, PD1 | T5 |
| M1.7 | Time and sales with size, side and price filters | Present | Bounded filtered window over PF5 | — | T3 |
| M1.8 | Tick, volume and range charts with footprint on the same bars | Absent | Interval picker and persistence | F1, OF14, CT3 | T7 |

**Acceptance:** every order-flow view renders from shared canonical streams without duplicate
provider demand, and frame work stays bounded during news bursts.

### M2 — Trading and execution

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M2.1 | DOM ladder: one-click orders, drag to modify, inline orders and position, P/L, recent volume | Present | `terminal_ui` ladder over PF3; queue column awaits PF4 | — | T2 |
| M2.2 | Order entry: quantity presets, order types, time in force, account selector | Present | GPUI panel over the order-command path | — | T2 |
| M2.3 | Bracket and strategy templates: stop and target, OCO, trailing, break-even, scale-out | Partial (no template editor) | Template model in PF7; local-management labels | PD11 | T2 |
| M2.4 | Chart trading: place, drag and cancel orders and positions | Present | Runtime snapshots projected into the chart; intents through M3.2 | PD11, PD1 | T2 |
| M2.5 | Trading hotkeys: buy/sell at bid/ask/market, flatten, cancel all, reverse, per-hotkey confirmation | Partial (market, cancel all, flatten, kill only) | Registered with the M7.2 keymap owner; off while a text field has focus | — | T2 |
| M2.6 | Flatten and kill switch per account or globally | Partial (lock cannot be released) | Single command path, visible at all times | — | T2 |
| M2.7 | Multi-account copier with multipliers and kill switches | Present | M3.2 checks before each mirrored order | PD11 | T2 |
| M2.8 | Positions and PnL in currency and ticks | Present | PF2 and PF6 projections | Position chips | T2 |

**Acceptance:** no order leaves the machine without M3.2 checks; every command is idempotent across
reconnects; simulated and live accounts can never be confused.

### M3 — Risk and discipline

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M3.1 | Prop-firm rules: daily loss, trailing drawdown, max contracts, consistency, news restrictions; live meters | Partial (no profile editor) | Versioned profiles in PF7; broker limits (PF2) take precedence | PD1 | T2 |
| M3.2 | Pre-trade checks and hard locks | Present | Inside the single order-command path for DOM, chart, copier and hotkeys | PD1 | T2 |
| M3.3 | Tilt detection: rapid losses, rising size, fast re-entry, outside planned hours | Partial (planned hours needs M3.4) | Deterministic rules over the local execution log | — | T2 |
| M3.4 | Session plan and checklist with end-of-day adherence review | Partial (no plan editor) | Plan model in PF7; plan levels as host overlays | PD3 | T2 |

**Acceptance:** rule evaluation is reproducible from recorded fills; locks survive restart; profiles
are versioned because firms change their rules.

### M4 — Recording, replay and review

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M4.1 | Session recorder | Absent | PF8 | — | T6 |
| M4.2 | Market replay at 1× to 100× with practice trading | Absent | Replay source feeding the same series and study paths; PF9 orders | PD2, F1 | T6 |
| M4.3 | Trade review with book, footprint and executions as they were | Absent | Link PF7 executions to PF8 recordings | PD2, PD4 | T6 |
| M4.4 | Journal and analytics: tags, notes, screenshots, win rate, expectancy, MAE/MFE, breakdowns | Absent | Round-trip statistics over PF7; chart export screenshots | PD4, PD6 | T6 |
| M4.5 | Queue-aware paper fills from recorded order-level data | Absent | Fill model in PF9 over PF4 and PF8 | — | T7 |

**Acceptance:** replay never shows data after the replay clock; replay reproduces live derived
results; journal statistics are reproducible from stored records.

### M5 — Fundamentals and context

| ID | Feature | Status | Data source | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M5.1 | Economic calendar with countdowns, importance and rule-driven flatten or lock | Present (rule actions need M3.1 editor) | BLS, BEA, Federal Reserve, EIA schedules per D6 | PD3 | T4 |
| M5.2 | Energy: petroleum and gas storage versus the five-year range; degree days | Present | EIA (user key); NOAA CPC | PD7 | T4 |
| M5.3 | Commitments of Traders by trader category | Present | CFTC | PD7 | T4 |
| M5.4 | Grains: crop progress, WASDE, export sales | Present | USDA NASS (user key), WASDE, FAS | PD7 | T4 |
| M5.5 | Macro: yields, dollar index, inflation, employment | Present | FRED (user key) | PD7 | T4 |
| M5.6 | Contract roll and expiry calendar | Partial (Rithmic expiry strings only) | PF6 plus provider volume | PD3 | T7 |
| M5.7 | Session and trading-hours display, holiday closures | Partial (session-day bucketing only) | PF6 and exchange calendars | PD3 | T7 |

**Rules:** one bounded background owner; nothing blocks the UI thread; every value carries source
and release time; missing keys or outages show "unavailable", never invented values.

### M6 — Automation and research

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M6.1 | Alerts for order flow, rule meters, events and studies | Partial (price and delta divergence) | Extend `market_runtime` alert evaluation; study alerts per study roadmap Phase J | Alert lines | T8 |
| M6.2 | Delivery to Telegram or Discord through the user's own bot or webhook | Partial (desktop notifications) | Follows the data licensing checklist | — | T8 |
| M6.3 | Custom indicators and the in-app Study Editor | Partial (SDK Phases A–E) | Study roadmap Phases F–L | I4, F4 | T8 |
| M6.4 | Strategies and local backtesting with order-book-aware fills | Absent | Separate strategy program; needs PF8 and PF9 | — | T8 |
| M6.5 | Optional local AI assistant over the user's own records | Absent | Local model or user's own key; never raw exchange data | — | T8 |

### M7 — Workspace and experience

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M7.1 | Command palette with mnemonics such as `ES footprint 5m` | Present | One registry for menus, palette and hotkeys | — | T4 |
| M7.2 | Configurable keymap with conflict detection | Partial (no user rebinding) | Single keymap owner | — | T2 |
| M7.3 | Multi-window and multi-monitor workspaces | Absent | Detachable windows with persisted placement per display | — | T8 |
| M7.4 | Linked symbol groups with synchronized crosshair and time range | Present | Link-group coordinator in the desktop | PD5 | T4 |
| M7.5 | Performance mode and diagnostics overlay | Partial (feed diagnostics) | Overlay over `observability` and chart telemetry | Telemetry | T8 |
| M7.6 | Themes and accessibility: colorblind-safe palettes, font scaling | Partial (dark and light) | Design-system tokens only | — | T8 |
| M7.7 | Layout and settings sync through a user-controlled folder | Absent | Export and import with conflict handling | — | T8 |
| M7.8 | Distribution and updates | Partial (disabled) | Release hosting once a path is approved | — | T8 |

---

## 5. Foundations

| ID | Foundation | Batch | Status |
| --- | --- | --- | --- |
| PF1 | Rithmic order routing | T5 | Partial (adapter sessions only) |
| PF2 | Rithmic PnL plant and account state | T5 | Partial (adapter sessions only) |
| PF3 | Canonical trading domain | T1 | Delivered |
| PF4 | Canonical order-level book publication | T5 | Absent |
| PF5 | Trade tape publication | T3 | Delivered |
| PF6 | Contract metadata | T1 | Delivered |
| PF7 | Local store for user-owned records | T1 | Delivered |
| PF8 | Session recording store | T6 | Absent |
| PF9 | Local simulated venue | T1 | Delivered |
| PF10 | Additional trading providers (see [Broker coverage](#9-broker-coverage)) | T8 | Absent |

**PF1 — Rithmic order routing.** Submit, modify and cancel market, limit, stop and stop-limit
orders; server-side brackets, OCO and trailing stops where Rithmic supports them; status, fill and
rejection updates; account list. Prefer server-side order types so protection survives a crash;
label any locally managed order. Every command is idempotent through client order IDs and fenced by
session generation. Gate: onboarding and conformance; verify every template against the licensed
Provider Kit.

**PF2 — Rithmic PnL plant and account state.** Positions, realized and unrealized PnL, balance,
margin and broker risk limits. Broker-reported limits are authoritative; M3.1 profiles complement
them. Gate: onboarding.

**PF3 — Canonical trading domain.** Provider-neutral fixed-point accounts, orders, order events,
fills, positions and PnL with explicit scales and provenance. Aeris Charts receives projections and
never owns order state.

**PF4 — Canonical order-level book.** Bounded order-level view (identity, side, price, size,
priority evidence, sequence) from the Rithmic adapter per D4, beside the aggregated book. Consumers:
M1.6, the DOM's order counts and queue column, and PD8.

**PF5 — Trade tape.** Bounded classified trades from `market_runtime`, coalesced for slow consumers
and generation-fenced; feeds time and sales, footprint, delta, big trades and the chart's F2 tape.

**PF6 — Contract metadata.** Tick size, point value, currency, expiry, first notice, last trade and
session hours from provider reference data. Consumers: PnL, risk checks, M5.6, the DOM tick grid.

**PF7 — Local store.** Executions, journal entries, tags, notes, screenshots, rule profiles and
evaluations, session plans and analytics caches per D3. Background I/O only, schema migrations,
bounded growth with user-visible retention, CSV and JSON export.

**PF8 — Session recording store.** Opt-in, compressed local recording of the user's own subscribed
trades, depth, quotes and bars with manifests, integrity checks and retention caps. Replay input
only; never a history cache for live charts. Gate: D2 and the licensing checklist.

**PF9 — Local simulated venue.** Same contracts as PF1–PF3, driven by live or replayed data. Touch
fills now, queue-aware fills later (M4.5). Simulated accounts are always visually distinct.

**PF10 — Additional trading providers.** Broker, exchange or futures-commission-merchant (FCM) routes
behind the same provider-neutral market and trading contracts, chosen per D5 from the candidates in
[Broker coverage](#9-broker-coverage). tastytrade market data already runs through the provider
registry; CQG and dxFeed remain candidates for futures data.

---

## 6. Decisions

| ID | Question | Blocks | Status |
| --- | --- | --- | --- |
| D1 | Owner of broker accounts, orders, fills and positions | PF1–PF3, all trading | **Decided:** one in-process trading owner (`trading_runtime`) beside `market_runtime`; `account_runtime` stays Aeris identity only |
| D2 | Session recording versus the ban on market-history persistence | PF8, M4, M6.4 | **Open.** Recommendation: allow an explicit opt-in, user-owned recording store that never feeds the history cache; amend `AGENTS.md` and `tools/naming_check` in the same change |
| D3 | Local storage engine for user-owned records | PF7 | **Decided:** SQLite through the pinned bundled `rusqlite`; do not add another store |
| D4 | Order-level data leaving the Rithmic adapter | PF4, M1.6 | **Open.** Recommendation: a bounded canonical view in `domain/market_data`, keeping provider identity, local order and sequence evidence separate |
| D5 | First additional trading provider | PF10 | **Open.** Recommendation: defer until Rithmic trading is qualified; choose by user demand. Conflicts with `plan/go-to-market-strategy.md`, whose 90-day plan ships cTrader and starts Hyperliquid builder-code revenue before Rithmic is qualified; the maintainer must pick one order and record it here |
| D6 | Calendar distribution without a backend | M5.1 | **Decided:** fetch official BLS, BEA, Federal Reserve and EIA schedules directly through `context_runtime` |

---

## 7. Constraints

### Scope and ownership

Aeris Charts renders what appears inside a chart. Aeris Terminal owns every panel and product rule
outside it: the DOM ladder (`terminal_ui`), time and sales, all risk rules and the trading lock. The
lock needs no engine support, because the platform decides which chart trading gestures reach Aeris
Charts and whether to act on the resulting intents. Every item satisfies the core principles and
architecture invariants in `AGENTS.md`; a conflict with an invariant becomes a decision in
[section 6](#6-decisions), never a workaround.

### Operating constraints

1. **Local-first, zero server cost.** No Aeris server receives, stores, processes or relays market
   data, and no feature may depend on one.
2. **Exchange data stays on the user's machine.** Broker data is processed only in the desktop
   process. Aeris must not redistribute exchange data or its derivatives.
3. **Public non-exchange data is fetched directly from its official source**, with the user's own
   free API key where required.
4. **User-owned data is stored locally.** Optional sync uses a folder the user controls.
5. **Provider access is gated.** Rithmic features are designed now and qualified after onboarding;
   anything buildable on Hyperliquid public data or the simulated venue proceeds first.

### Data licensing checklist

A release gate, not legal advice. Before a feature that stores, derives or transmits exchange data
ships, confirm against current CME policies, the Rithmic agreement and any broker agreements:

- Local storage of recorded trades and depth for the subscriber's personal replay and review.
- Display of derived values (queue position, detections, footprint and profile statistics).
- Alerts leaving the machine carry event descriptions, not exchange prices or quantities, unless
  explicitly allowed.
- Professional versus non-professional subscriber status.

Record the reviewed policy versions in the feature's release notes.

---

## 8. Current baseline

Source-confirmed 2026-10-05. This records what exists, not a claim of completeness.

| Area | Present today | Where |
| --- | --- | --- |
| Rithmic market data | Ticker and History plants: login, heartbeat, symbol search, reference data, quotes, trades with aggressor, tick and time bars, depth by order with snapshot; Rithmic Test endpoint only | `crates/adapters/rithmic_protocol` |
| Rithmic order-level book | Assembled in the adapter (up to 131,072 orders); published only as aggregated levels | `provider_session/canonical_market.rs` |
| Rithmic trading | Order and PnL plant sessions in the adapter (accounts, trade routes, submit, modify, cancel, fills, positions); **not wired** into `trading_runtime` or the desktop | `order_plant.rs`, `pnl_plant.rs`, `order_session.rs` |
| tastytrade | Level 1 futures and equities through DXLink: candles, quotes, every `TimeAndSale` with aggressor side, runtime-owned futures catalog; trusted third-party authorization with the `read` scope, access tokens issued by the Aeris AWS broker; no depth, no account data, no orders | `crates/adapters/tastytrade_market`, `market_service/tastytrade.rs` |
| Hyperliquid | Public candles, L2 snapshots, BBO, trades, catalog and asset context; no orders | `crates/adapters/hyperliquid_market` |
| CQG, dxFeed, crypto exchanges, forex/CFD | **Absent** | — |
| Market ownership | `MarketEngine` demand; `market_runtime` merges history and live state and publishes series, books, study outputs, alerts and one trade tape | `market_engine`, `market_runtime` |
| Instrument metadata | Price increment plus canonical tick, point value, currency, expiry and session fields | `contracts`, `domain/instruments` |
| Trading | Simulated venue, accounts, orders, fills, positions, PnL, copier, rules, locks, discipline, plans and templates in one owner; editors for profiles, plans and templates absent | `crates/trading_runtime`, `apps/desktop/src/trading.rs` |
| Order book panel | Trading ladder, order entry, copier, P/L, account actions, time and sales | `crates/ui/terminal_ui`, `components/chart_surface.rs` |
| Charts | Candles, bars, line, area, baseline, footprint; drawings; trading and plan overlays; delta, CVD, bubbles; split workspace; study indicators; visible-range volume profile. Session profiles and heatmap unwired | `crates/ui/chart_integration` |
| Studies | Runtime and SDK Phases A–E; editor and sandbox pending | `plan/study_runtime_sdk_roadmap.md` |
| Alerts | Price and delta-divergence alerts with OS notifications | `market_runtime`, `platform_runtime` |
| Context data | Official schedules, EIA/NOAA, CFTC, USDA and FRED through one bounded owner | `crates/context_runtime`, `components/context_panel.rs` |
| Workspace | Tabs, splits, themes, feed diagnostics, command palette, link groups, trading hotkeys; no multi-window | `apps/desktop`, `crates/observability` |
| Persistence | Workspace state and credential vault; SQLite for trading records; market-history persistence banned | `workspace_persistence.rs`, `trading_runtime`, `tools/naming_check` |
| Recording and replay | **Absent** ("replay" in market code means bar snapshot contracts) | `application/src/replay_snapshot.rs` |
| Account runtime | Aeris sign-in, disabled in development builds; not a broker account | `crates/account_runtime` |
| Distribution | Launcher and signed lifecycle code; updates and publication disabled | `platform_runtime`, `apps/desktop/src/update.rs` |

---

## 9. Broker coverage

Engineering readiness per route, updated 2026-10-05. Commercial targets, pricing and legal cautions
live in `plan/go-to-market-strategy.md`. Never advertise a route before the broker approves Aeris.

| Route | Reaches | Market data | Accounts and trading | Gate |
| --- | --- | --- | --- | --- |
| Rithmic | Futures prop firms (Bulenox, Tradeify, Alpha Futures, Take Profit Trader, Phidias, TradeDay) and brokers (Discount Trading, AMP Futures, Stage 5) | Present, full depth; Rithmic Test only | Adapter sessions only | Onboarding, production systems, conformance |
| tastytrade | tastytrade customers: futures and equities | Present, Level 1 | Absent | `read` scope approved; `trade` scope only after traction |
| Hyperliquid | Crypto perps and spot; US users blocked | Present, public | Absent | Builder code (100 USDC) and agent-wallet approval |
| Crypto exchanges | OKX, Bybit, KuCoin, Binance, Kraken, Coinbase, Bitget, Gate, Deribit, Delta Exchange India, CoinDCX | Absent; public WebSocket, no login | Absent | Broker program per exchange; Indian legal check on rebates; geo-blocking |
| cTrader Open API | IC Markets, Pepperstone, FP Markets, FxPro, BlackBull and other cTrader brokers; no US brokers | Absent | Absent | Spotware app review |
| Other forex/CFD | TradeLocker, DXtrade, Match-Trader, OANDA, IBKR TWS, Capital.com, IG | Absent | Absent | Per route |
| Excluded | MT4/MT5 brokers, Topstep, NinjaTrader/Tradovate | — | — | Not pursued |

### Completing a route

Every route reuses T2 unchanged, so close [Next work](#2-next-work) first: prop-firm and broker
pilots depend on rule profiles, unlock and templates working. The order between routes is D5.

**Rithmic (T5)**

1. Settle onboarding in writing: dev-kit, API and conformance costs, any per-user vendor fee, and
   whether Rithmic accepts an Indian individual or company.
2. Verify every order and PnL plant template against the licensed Provider Kit and exercise the
   adapter sessions on Rithmic Test.
3. Add production endpoints and a system/gateway picker; confirm per-system enablement with Rithmic.
4. **PF1:** a Rithmic venue in `trading_runtime` beside the simulated venue, with idempotent client
   order IDs fenced by session generation and every command through the M3.2 checks.
5. **PF2:** positions, PnL, balance, margin and broker limits projected into the canonical trading
   domain.
6. Pass conformance, then qualify T2 on live accounts, including the copier.
7. D4, PF4 and M1.6.
8. Get listed on rithmic.com/platforms and by a broker before prop-firm outreach.

**tastytrade**

1. Finish the US cash-hours timing probe and visual check (`plan/provider_integration_plan.md`,
   Phase 1).
2. Define the provider-neutral, read-only account observation contract in `trading_runtime`
   (positions, PnL, journal, balances) under the existing `read` scope, with contract and restart
   tests; then wire tastytrade to it.
3. Order execution only after traction: `trade` scope, sandbox testing and legal review.

**Hyperliquid and crypto exchanges**

1. One adapter crate per exchange under `crates/adapters`, registered through a
   `ProviderDescriptor`; the registry needs no coordinator change for a new provider.
2. Credentials only in the native vault: OAuth with PKCE where offered, otherwise trade-only API
   keys, rejecting keys with withdrawal permission on connect. Hyperliquid trades through an
   approved agent wallet that cannot withdraw.
3. A `trading_runtime` venue for orders, fills and positions through the M3.2 path, attaching the
   broker or builder code each program requires.
4. Geo-blocking per exchange, and the Indian legal and CA check before taking any rebate.

**cTrader and other forex/CFD**

1. Register the cTrader Open API app early; Spotware's review gates production use.
2. Adapter, descriptor, vault-held tokens and a `trading_runtime` venue, as for crypto.
3. No forex marketing to Indian residents; US users only through CFTC-registered firms.

---

## 10. Delivery and verification

### How a batch is delivered

Work proceeds one batch at a time, following `AGENTS.md`.

1. **Build the whole batch**, with deterministic regression tests written alongside each item.
2. **Run focused checks while building**: `cargo check`, `cargo test` and `cargo clippy` for the
   touched crates.
3. **Run the broad gate once at the end**: `cargo fmt --all -- --check`, workspace clippy with
   `-D warnings`, `cargo test --workspace --all-features --locked`, and real-path evidence for any
   provider, persistence or rendering behavior touched. Fix and rerun until green.
4. **Commit and push once per batch** in `type(scope): outcome` style, listing delivered IDs and
   verification. Split into two commits only when too large to review, each passing the gate.
5. **Update this file in the same commit**: tick items, set statuses, and move enforceable
   architecture rules into `tools/naming_check`.
6. **Chart dependencies first**: deliver the Aeris Charts batch, then pin only the `aeris_charts_*`
   revisions in the terminal batch.

### Verification for every item

- Deterministic tests at the owning boundary with sanitized fixtures faithful to real provider
  shapes; no credentials or private payloads.
- Real-path evidence for provider, persistence, rendering and installed-app behavior; compilation
  alone is not proof.
- Bounded queues, caches, stores and retries with explicit overflow, measured under burst and soak
  workloads in release builds.
- Locks, rule state, journal records and recordings survive restart and partial failure.

### Definition of completion

Aeris Terminal meets this roadmap when a futures trader can trade live Rithmic accounts from the
DOM, chart and hotkeys with enforced prop-firm rules; read order flow, depth and order-level
analytics in real time; replay and review every session with journal analytics; and see the relevant
fundamentals beside each contract, entirely on their own machine, with no Aeris server in the
market-data path and every catalog item qualified. Until then, report delivered items and remaining
gaps against the IDs in this file.
