# Trading platform

Trading batches, features, foundations, decisions and broker coverage. Operating constraints and
the data licensing checklist live in `AGENTS.md`. Studies are in [`study_runtime_sdk_roadmap.md`](study_runtime_sdk_roadmap.md). The
Aeris Charts repository's `plan/Expansion.md` owns chart-engine prerequisites (F1–F6, OF1–OF18,
CT1–CT6, I1–I4, PD1–PD11, batches B1–B9); this file names them by ID only.

> The fastest futures platform, the one that protects traders from breaking their own and their
> prop firm's rules, and the one that shows the order book and the real-world fundamentals behind
> every contract, while running entirely on the user's machine.

Item IDs are stable: **T** batches, **PF** foundations, **M** features, **D** decisions. A status
is **Complete** only when every item works through the real runtime and desktop path, not when a
unit test alone passes. Status as of 2026-10-08.

## Trading batches

| Batch | Scope | Status | Blocked by |
| --- | --- | --- | --- |
| T1 Trading foundations | D1, D3, PF3, PF6, PF7, PF9 | **Complete** | — |
| T2 Trading, execution and risk | M2.1–M2.8, M3.1–M3.4, M7.2 | **Partial**: engine complete; lock release, hotkeys and the profile, plan and template editors deferred until after Rithmic onboarding | — |
| T3 Order flow | PF5, M1.2, M1.4, M1.5, M1.7 | **Complete** | — |
| T4 Context and workspace | D6, M5.1–M5.5, M7.1, M7.4 | **Complete**; keyed sources not live-qualified | Maintainer API keys |
| [T5](#t5--rithmic-live-trading) Rithmic live trading | PF1, PF2, PF4, M1.6, live T2 | **Blocked**; adapter order and PnL plant sessions exist, not wired to `trading_runtime` | Rithmic onboarding and conformance; D4 |
| [T6](#t6--record-replay-and-review) Record, replay and review | D2, PF8, M4.1–M4.4 | **Open** | D2; data licensing checklist |
| [T7](#t7--institutional-depth) Institutional depth | M1.1, M1.3, M1.8, M4.5, M5.6, M5.7 | **Open** | T6; Aeris Charts B4–B7; licensing checklist |
| [T8](#t8--power-users) Power users | M6.1–M6.5, M7.3, M7.5–M7.8 | **Open** | Aeris Charts B9; a release path for M7.8 |
| T9 cTrader forex and CFD | PF10, PF11, D7–D9 | **Partial**; status and checklist in [`ctrader_implementation.md`](ctrader_implementation.md) | D7 Spotware confirmation; D8; D9 |

**Order:** T1 unblocked T2, T5, T6 and part of T7. T5 starts when Rithmic onboarding clears and
reuses T2 unchanged against the live venue. T9 runs in parallel with T5 (D5); PF11, the live-venue
contract, is built once in T9 and reused by T5's PF1. T6 needs T1's store and T2's execution log.

**Batch delivery:** build the whole batch with deterministic tests, run the gates in `AGENTS.md`,
commit once per batch listing the delivered IDs, and update this file in the same commit. Deliver
any Aeris Charts batch first, then pin only the `aeris_charts_*` revisions here.

### T5 — Rithmic live trading

**Needs:** T1, T2; Aeris Charts B1 (PD1), B6 (PD8).

- [~] Order plant and PnL plant sessions in `rithmic_protocol` (`11625ca7`): login info, account
      list, trade routes, order updates and snapshot, submit, modify, cancel, cancel all, execution
      replay, fill history, and position snapshot and updates, with allowlisted outbound templates
      and fixed-point decimals. Verification against the licensed Provider Kit and a live Rithmic
      Test exchange is not recorded.
- [ ] Production endpoints and a system/gateway picker from Rithmic's system list; the adapter
      connects only to the hardcoded Rithmic Test endpoint (`endpoint.rs`).
- [ ] **PF1** Order routing with idempotent client order IDs, as a `trading_runtime` venue on PF11.
- [ ] **PF2** Positions, PnL, balance, margin and broker risk limits.
- [ ] Rithmic conformance passed.
- [ ] T2 features qualified on live accounts, including the copier across several accounts.
- [ ] **D4** decided; **PF4** bounded canonical order-level publication.
- [ ] **M1.6** Queue position, iceberg, pulled-liquidity and size-cluster detectors, labeled as
      estimates.

**Acceptance:** live Rithmic accounts trade from the DOM, chart and hotkeys with enforced rules, and
reconnects never duplicate or lose commands.

### T6 — Record, replay and review

**Needs:** T1, T2; Aeris Charts B1 (PD4, PD6), B5 (PD2, F1).

- [ ] **D2** decided; `AGENTS.md` and `tools/naming_check` amended in the same change.
- [ ] Data licensing checklist reviewed for local recording.
- [ ] **PF8 / M4.1** Opt-in session recorder with manifests, integrity checks and retention caps.
- [ ] **M4.2** Market replay at 1× to 100× with practice trading through PF9.
- [ ] **M4.3** Trade review with book, footprint and executions as they were.
- [ ] **M4.4** Automatic journal and analytics with screenshots through PD6.

**Acceptance:** replay never shows data after the replay clock, replayed sessions reproduce live
derived results, and journal statistics are reproducible from stored records.

### T7 — Institutional depth

**Needs:** T3, T6; Aeris Charts B4, B5, B6, B7.

- [ ] **M1.1** Liquidity heatmap from canonical depth and PF5 trades.
- [ ] **M1.3** Session, composite, fixed-range and anchored profiles, VWAP bands and TPO.
- [ ] **M1.8** Tick, volume and range charts with footprint on the same bars.
- [ ] **M4.5** Queue-aware paper fills from recorded order-level data.
- [ ] **M5.6** Contract roll and expiry calendar.
- [ ] **M5.7** Session and trading-hours display.

**Acceptance:** depth views stay bounded under dense books and match the recorded session in replay.

### T8 — Power users

**Needs:** Aeris Charts B9 (I4); study plan Phases F–L.

- [ ] **M6.1** Order-flow, rule-meter, event and study alerts.
- [ ] **M6.2** Optional Telegram or Discord delivery.
- [ ] **M6.3** Custom indicators and the in-app Study Editor.
- [ ] **M6.4** Strategies and local backtesting over recorded sessions.
- [ ] **M6.5** Optional local AI assistant over user-owned records only.
- [ ] **M7.3** Multi-window and multi-monitor workspaces: detachable windows inside the one desktop
      process, sharing its runtimes, with placement persisted per display.
- [ ] **M7.5** Performance mode and diagnostics overlay.
- [ ] **M7.6** Themes and accessibility.
- [ ] **M7.7** Layout and settings sync through a user-controlled folder.
- [ ] **M7.8** Distribution and updates once a release path is approved.

**Acceptance:** the [definition of completion](#definition-of-completion) is met.

## Trading feature catalog

**Present** works end to end in the desktop. **Partial** exists but misses part of its definition.
**Absent** has not started. The Aeris Charts column lists `Expansion.md` prerequisites. Of 45
features, 17 are Present, 15 Partial and 13 Absent.

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
| M4.4 | Journal and analytics: tags, notes, screenshots, win rate, expectancy, MAE/MFE, breakdowns | Absent (chart image export exists since `aa1660b6`; realized P&L shows on closing fills) | Round-trip statistics over PF7; attach exported chart images | PD4, PD6 | T6 |
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
| M6.1 | Alerts for order flow, rule meters, events and studies | Partial (price and delta divergence) | Extend `market_runtime` alert evaluation; study alerts per study plan Phase J | Alert lines | T8 |
| M6.2 | Delivery to Telegram or Discord through the user's own bot or webhook | Partial (desktop notifications) | Follows the data licensing checklist | — | T8 |
| M6.3 | Custom indicators and the in-app Study Editor | Partial (SDK Phases A–E) | Study plan Phases F–L | I4, F4 | T8 |
| M6.4 | Strategies and local backtesting with order-book-aware fills | Absent | Separate strategy program; needs PF8 and PF9 | — | T8 |
| M6.5 | Optional local AI assistant over the user's own records | Absent | Local model or user's own key; never raw exchange data | — | T8 |

### M7 — Workspace and experience

| ID | Feature | Status | Platform work | Aeris Charts | Batch |
| --- | --- | --- | --- | --- | --- |
| M7.1 | Command palette with mnemonics such as `ES footprint 5m` | Present | One registry for menus, palette and hotkeys | — | T4 |
| M7.2 | Configurable keymap with conflict detection | Partial (no user rebinding) | Single keymap owner | — | T2 |
| M7.3 | Multi-window and multi-monitor workspaces | Absent | Detachable windows in the one desktop process with persisted placement per display | — | T8 |
| M7.4 | Linked symbol groups with synchronized crosshair and time range | Present | Link-group coordinator in the desktop | PD5 | T4 |
| M7.5 | Performance mode and diagnostics overlay | Partial (feed diagnostics) | Overlay over `observability` and chart telemetry | Telemetry | T8 |
| M7.6 | Themes and accessibility: colorblind-safe palettes, font scaling | Partial (dark and light) | Design-system tokens only | — | T8 |
| M7.7 | Layout and settings sync through a user-controlled folder | Absent | Export and import with conflict handling | — | T8 |
| M7.8 | Distribution and updates | Partial (disabled) | Release hosting once a path is approved | — | T8 |

## Trading foundations

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
| PF10 | cTrader Open API, the first additional trading provider | T9 | Partial (adapter and market messages committed; runtime market data uncommitted; no order messages) |
| PF11 | Provider-neutral live venue in `trading_runtime` | T9 | Partial (routing, schema-v18 broker tables and demo venue inbox; defects T-1 to T-6 open) |

**PF1 — Rithmic order routing.** Submit, modify and cancel market, limit, stop and stop-limit
orders; server-side brackets, OCO and trailing stops where Rithmic supports them; status, fill and
rejection updates; account list. Prefer server-side order types so protection survives a crash;
label any locally managed order. Every command is idempotent through client order IDs and fenced by
session generation. Gate: onboarding and conformance; verify every template against the licensed
Provider Kit.

**PF2 — Rithmic PnL plant and account state.** Positions, realized and unrealized PnL, balance,
margin and broker risk limits. Broker-reported limits are authoritative; M3.1 profiles complement
them. Gate: onboarding.

**PF4 — Canonical order-level book.** Bounded order-level view (identity, side, price, size,
priority evidence, sequence) from the Rithmic adapter per D4, beside the aggregated book. Consumers:
M1.6, the DOM's order counts and queue column, and PD8.

**PF8 — Session recording store.** Opt-in, compressed local recording of the user's own subscribed
trades, depth, quotes and bars with manifests, integrity checks and retention caps. Replay input
only; never a history cache for live charts. Gate: D2 and the licensing checklist.

**PF9 — Local simulated venue.** Touch fills now; queue-aware fills later (M4.5). Simulated
accounts are always visually distinct.

**PF10 — cTrader Open API.** Market data and trading for every cTrader broker behind the same
provider-neutral market and trading contracts. Later routes from
[Broker coverage](#broker-coverage) follow the same pattern; CQG and dxFeed remain candidates for
futures data.

**PF11 — Live venue.** The provider-neutral boundary in `trading_runtime` through which a live
broker receives orders and reports executions, positions and balances, beside PF9. Idempotent
client order IDs, generation fencing, reconcile after reconnect, and the M3.2 checks on every
command. cTrader is its first consumer; Rithmic PF1 reuses it unchanged.

## Trading decisions

| ID | Question | Blocks | Status |
| --- | --- | --- | --- |
| D1 | Owner of broker accounts, orders, fills and positions | PF1–PF3, all trading | **Decided:** one in-process trading owner (`trading_runtime`) beside `market_runtime`; `account_runtime` stays Aeris identity only |
| D2 | Session recording versus the ban on market-history persistence | PF8, M4, M6.4 | **Open.** Recommendation: allow an explicit opt-in, user-owned recording store that never feeds the history cache; amend `AGENTS.md` and `tools/naming_check` in the same change |
| D3 | Local storage engine for user-owned records | PF7 | **Decided:** SQLite through the pinned bundled `rusqlite`; do not add another store |
| D4 | Order-level data leaving the Rithmic adapter | PF4, M1.6 | **Open.** Recommendation: a bounded canonical view in `domain/market_data`, keeping provider identity, local order and sequence evidence separate |
| D5 | First additional trading provider | PF10 | **Decided 2026-10-05:** cTrader Open API (T9), built in parallel with Rithmic onboarding because Rithmic onboarding is slow and cTrader reaches many brokers through one application |
| D6 | Calendar distribution without a backend | M5.1 | **Decided:** fetch official BLS, BEA, Federal Reserve and EIA schedules directly through `context_runtime` |
| D7 | Handling of the cTrader client secret, which every desktop connection must send | T9 | **Implemented, awaiting Spotware confirmation:** the AWS broker returns the secret only to a desktop holding a `ready` connection and its proof (`app_credentials`); the desktop keeps it in zeroizing memory, never on disk or in logs. Rotation: rerun `configure:ctrader`; desktops refetch on `CH_CLIENT_AUTH_FAILURE`. Never in source, builds, logs or fixtures |
| D8 | Owner of the cTrader trading session | T9 | **Open.** Recommendation: the `market_runtime` cTrader supervisor keeps the single connection per host and serves trading through the bounded `VenueRequest`/`VenueInbox` boundary; trading state stays in `trading_runtime` |
| D9 | Hedging versus netting cTrader accounts | T9 | **Open.** Decide how `trading_runtime` presents each (positions per id versus net) and which account types the first release allows |

## Broker coverage

Engineering readiness per route, updated 2026-10-08. Commercial targets and legal cautions live in
[`go_to_market_strategy.md`](go_to_market_strategy.md). Never advertise a route before the
broker approves Aeris.

| Route | Reaches | Market data | Accounts and trading | Gate |
| --- | --- | --- | --- | --- |
| Rithmic | Futures prop firms (Bulenox, Tradeify, Alpha Futures, Take Profit Trader, Phidias, TradeDay) and brokers (Discount Trading, AMP Futures, Stage 5) | Present, full depth; Rithmic Test only | Adapter sessions only | Onboarding, production systems, conformance |
| tastytrade | tastytrade customers: futures and equities | Present, Level 1 | Absent | `read` scope approved; `trade` scope only after traction |
| Hyperliquid | Crypto perps and spot; US users blocked | Present, public | Absent | Builder code (100 USDC) and agent-wallet approval |
| Crypto exchanges | OKX, Bybit, KuCoin, Binance, Kraken, Coinbase, Bitget, Gate, Deribit, Delta Exchange India, CoinDCX | Absent; public WebSocket, no login | Absent | Broker program per exchange; Indian legal check on rebates; geo-blocking |
| cTrader Open API | IC Markets, Pepperstone, FP Markets, FxPro, BlackBull and other cTrader brokers; no US brokers | Partial: quotes, bars and depth verified on one demo account in `market_runtime` (uncommitted), no trade tape; not exposed in the desktop | Skeleton only: account routing and demo venue inbox, no order messages; live refused | D7 Spotware confirmation; D8; D9 |
| Other forex/CFD | TradeLocker, DXtrade, Match-Trader, OANDA, IBKR TWS, Capital.com, IG | Absent | Absent | Per route |
| Excluded | MT4/MT5 brokers, Topstep, NinjaTrader/Tradovate | — | — | Not pursued |

Every route reuses T2 unchanged, so the remaining T2 gaps close before broker pilots.

**Rithmic (T5)**

1. Settle onboarding in writing: dev-kit, API and conformance costs, any per-user vendor fee, and
   whether Rithmic accepts an Indian individual or company.
2. Verify every order and PnL plant template against the licensed Provider Kit and exercise the
   adapter sessions on Rithmic Test.
3. Add production endpoints and a system/gateway picker; confirm per-system enablement with Rithmic.
4. **PF1:** a Rithmic venue on PF11, with idempotent client order IDs fenced by session generation
   and every command through the M3.2 checks.
5. **PF2:** positions, PnL, balance, margin and broker limits projected into the canonical trading
   domain.
6. Pass conformance, then qualify T2 on live accounts, including the copier.
7. D4, PF4 and M1.6.
8. Get listed on rithmic.com/platforms and by a broker before prop-firm outreach.

**tastytrade**

1. Finish the US cash-hours timing probe and visual check
   ([`provider_integration_plan.md`](provider_integration_plan.md), Phase 1).
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
3. A `trading_runtime` venue on PF11 for orders, fills and positions through the M3.2 path,
   attaching the broker or builder code each program requires.
4. Geo-blocking per exchange, and the Indian legal and CA check before taking any rebate.

**Other forex/CFD**

1. Adapter, descriptor, vault-held tokens and a PF11 venue, as for crypto.
2. No forex marketing to Indian residents; US users only through CFTC-registered firms.

## Definition of completion

Aeris Terminal meets this plan when a futures trader can trade live Rithmic accounts from the DOM,
chart and hotkeys with enforced prop-firm rules; read order flow, depth and order-level analytics
in real time; replay and review every session with journal analytics; and see the relevant
fundamentals beside each contract, entirely on their own machine, with no Aeris server in the
market-data path and every catalog item qualified.
