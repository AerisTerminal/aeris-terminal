# Aeris Terminal audit: trading and paper trading

Scope: `crates/trading_runtime`, `crates/domain/trading`, the trading surfaces of `apps/desktop`, and the
`market_runtime` pieces the simulated venue depends on (practice session calendar, broker venue relay).
Method: read-only code reading and `git log`/`git show`; no build, no test run, no repository changes.
Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal`, branch `main`.
All paths below are relative to that root. Line numbers refer to the working tree at audit time.

## Summary

The trading runtime is a single owner thread (`Coordinator`) that holds accounts, orders, order events,
fills, positions, risk locks and managed brackets, persists every mutation to SQLite, and fills simulated
orders when a market observation arrives. The ownership boundary required by `AGENTS.md` is respected:
the desktop holds a handle and polled snapshots, and `market_runtime` only relays broker venue traffic.
Persistence and restart recovery are taken seriously (golden-ledger characterization tests, restart tests
for brackets and flatten, a projection repair path).

The paper trading engine is nevertheless a functional, demo-grade touch simulator rather than a mature
paper venue. The most important problems are structural, not cosmetic:

1. The simulated venue has no market-data subscription of its own. Prices only arrive when a chart pane
   publishes its order book. Close the chart and stops, targets, unrealized PnL and live risk freeze.
2. Closed-market and stale-quote protection exists only for one (since-removed) broker provider. Every other provider fills on
   whatever quote last arrived.
3. The kill switch cancels every working order, including protective stops, and leaves positions open.
   Flatten, flatten-all and economic-event flatten only touch the instrument of the supplied observation.
4. Position protection has grown as layers (managed brackets, standalone protective orders, a resize
   alignment pass, planned-leg projections, a desktop order-type policy, a restored-order fence).
   This is where most of the October 2026 `fix(trading)` churn lands.
5. The hot path does a durable (`synchronous=FULL`) write, a retention sweep and a revision write for
   every quote while a position is open. Retention deletes fills, which are the ledger positions are
   repaired from.
6. The desktop polls a full `TradingSnapshot` every 250 ms and clones it per pane. The snapshot fails
   outright once more than 10,000 orders exist, while retention keeps up to 250,000.

Maturity headline: order types, brackets, net positions, persistence and restart recovery are present.
Fill realism, time in force, fees, margin, cash ledger, market-data ownership and kill/flatten scope are
partial or missing. Treat it as a demo-grade touch simulator, not a paper engine whose results a trader
can rely on.

## Paper trading maturity matrix

Legend: Done = implemented and tested; Partial = implemented with material gaps; Missing = absent.

| Capability | Status | Evidence and notes |
|---|---|---|
| Market orders | Done | Fill at touch (ask for buys, bid for sells): `crates/trading_runtime/src/lib.rs:5035-5073`. |
| Limit orders | Done (touch model) | Full quantity fills when bid/ask crosses; no queue position (plan PF9 accepts this for now). |
| Stop orders | Done | Trigger and fill at touch. |
| Stop-limit orders | Partial | No triggered state. Fills only when the stop and limit conditions hold on the same quote; a gap through the stop followed by a return to the limit never fills. |
| Time in force: Day | Missing | No expiry code anywhere in the runtime. The ticket only sends Day; for the broker path Day is mapped to GTC (`crates/trading_runtime/src/venue/owner.rs:1252`). |
| Time in force: IOC / FOK | Partial | Cancelled only on the next observation (`lib.rs:4171-4198`), not at placement. FOK behaves the same as IOC. Not exposed in the UI. |
| Order ticket | Partial | Market and Limit only (`apps/desktop/src/components/order_ticket.rs:328,352`); stops come from chart brackets. |
| Partial fills | Missing | Every fill is full quantity. |
| Slippage / size-aware fills | Missing | No depth consumption; top-of-book price for any size. |
| Fees and commissions (simulated) | Missing | Not modelled for the simulated venue. |
| Tick-size validation | Missing | `prepare_single_order` (`lib.rs:2462-2547`) does not check limit/stop prices against tick size. |
| Brackets / OCO | Done, fragile | Managed brackets plus standalone protective orders plus alignment layer; see TR-07. |
| Net positions | Done | `Position` for simulated accounts; the removed FX/CFD broker used hedged `BrokerPosition` (dual model, TR-12). |
| Realized / unrealized PnL | Partial | Correct for the touch model, but unrealized PnL updates only while a chart publishes quotes (TR-01). Four parallel PnL trackers (TR-11). |
| Cash ledger, margin, buying power | Missing | Balance is starting equity plus summed realized PnL (`lib.rs:4672-4726`); no cash movements, margin or equity history. |
| Risk limits | Partial | Loss limits and locks exist, but the risk session never rolls over and no app registers a `RiskProfile` (TR-09). |
| Kill switch | Partial, unsafe | Locks and cancels all orders including stops; does not flatten (TR-03). |
| Flatten / flatten all / reverse | Partial, buggy | Only the observed instrument is flattened (TR-04); reverse is non-atomic. |
| Market session awareness | Partial | One removed broker provider only (its `market_service` module); other providers return `Ok(false)` (TR-02). |
| Market data ownership | Missing | Driven by chart panes (TR-01). |
| Persistence and restart | Done | SQLite store, restart tests for brackets, flatten and restored orders; projection repair at `lib.rs:1136-1215`. |
| Retention | Partial, risky | Deletes fills that the ledger is rebuilt from; snapshot limit lower than retention limit (TR-05, TR-06). |
| Deterministic replay | Partial | `observe_market` makes the engine replayable in tests; there is no price-stream harness or UI-to-runtime end-to-end test. |
| Live broker (removed FX/CFD broker, demo) | Partial | Routed through the `market_runtime` session as required. Rithmic order plant (`crates/adapters/rithmic_protocol/src/order_plant.rs`, 1,747 lines) is not used outside its crate. |

## Churn hotspots

Commit counts are for 2026 (from `git log --since`), sizes are approximate line counts.

| File | Lines | Commits | Notes |
|---|---|---|---|
| `crates/trading_runtime/src/lib.rs` | ~5,600 | 36 | God object `Coordinator`: order book, execution, ledger, risk, brackets, flatten, snapshot. |
| `crates/trading_runtime/src/tests.rs` | ~3,100 | 34 | Grows in lockstep with each fix; good sign for coverage, also shows fix-per-symptom pattern. |
| `crates/trading_runtime/src/store.rs` | ~3,650 | 21 | Per-mutation transactions, retention, revision writes. |
| `crates/trading_runtime/src/venue/scripted.rs` | 1,180 | 12 | Scripted broker venue for tests. |
| `crates/domain/trading/src/lib.rs` | ~900 | 10 | Domain types, validation. |
| `crates/trading_runtime/src/venue/owner.rs` | 1,303 | 9 | Broker path with its own admission validation. |
| `crates/trading_runtime/src/protection.rs` | 286 | recent | Added by `7b423f14` to resize protection after position changes. |
| `crates/trading_runtime/src/planned_leg.rs` | 267 | recent | Added by `7a184322` to project planned bracket legs. |
| `apps/desktop/src/desktop.rs` (trading parts) | large | many | Per-pane trading state and chart intent resolution. |

The cluster of fix commits on 10 to 12 October (`7a184322`, `a63fedaa`, `69896eee`, `7b423f14`,
`e085d0d5`, `0dc87755`, `954fb0b8`, `0a230a8a`) touches brackets, protection sizing, restored orders,
planned legs and the desktop resting-entry policy. Each adds a layer instead of consolidating the
order-group model, which is the root cause addressed by TR-07.

## Findings

### TR-01: The simulated venue gets prices only from chart panes

- Severity: Critical. Category: Architecture / correctness.
- Evidence: `publish_simulated_market_observation` is called only from
  `apps/desktop/src/engine_market_worker/publications.rs:126`, which builds the observation from a chart
  pane's order book snapshot. Depth demand is only active while depth is visible
  (`apps/desktop/src/engine_market_worker.rs:91-99`). `TradingHandle::publish_market_observation`
  (`crates/trading_runtime/src/lib.rs:993`) is the only feed into fills, mark-to-market and live risk.
- Impact: With no chart for an instrument (closed pane, different symbol selected, depth hidden), a
  resting stop or target never triggers, unrealized PnL and risk meters freeze, and loss limits stop
  evaluating. Trading correctness depends on UI layout, which contradicts the ownership rules in
  `AGENTS.md` (trading state owned by `trading_runtime`, demand owned by `MarketEngine`).
- Root cause: Market demand for trading was never modelled; the chart publication path was reused.
- Clean fix: `trading_runtime` declares quote demand to `MarketEngine` for every instrument that has an
  open simulated order or position (a bounded subscription set maintained on the owner thread), and
  receives top-of-book observations from `market_runtime` directly. Remove the chart-pane publication.
- Verification: Runtime test with a fake market source: place a stop, drop all UI demand, push quotes,
  assert the stop fills. Desktop manual check: close the chart with a bracketed position and confirm the
  target fills.

### TR-02: Closed-market and stale-quote protection exists only for one broker provider

- Severity: High. Category: Correctness.
- Evidence: `require_practice_market_session` (`lib.rs:2434-2449`) delegates to
  the removed Level 1 broker's `market_service` module, which returns `Ok(false)` for any
  other provider. Placement (`lib.rs:2491`) only propagates the error and discards the
  result. On observation (`lib.rs:3105-3129`) the "require a new quote" filter applies only when the
  result is `true`, so it is effectively limited to that one provider.
- Impact: For Rithmic, Hyperliquid and other-broker instruments, orders can be placed while the market is
  closed and fill on the first quote after placement, even if it is a stale replay or a reconnect
  snapshot. There is no quote age check at all.
- Root cause: Session gating was implemented inside one provider adapter instead of as a provider-neutral
  session and staleness contract.
- Clean fix: Make `market_runtime` expose a provider-neutral session phase and quote freshness for every
  instrument (unknown is an explicit state). The runtime rejects placement and suppresses fills when the
  phase is closed/unknown or the quote is older than a configured bound.
- Verification: Unit tests per provider family for closed, unknown and stale quotes; test that a
  reconnect snapshot older than the order does not fill it.

### TR-03: The kill switch strips protection and leaves positions open

- Severity: High. Category: Risk / safety.
- Evidence: `Coordinator::kill_switch` (`lib.rs:1852-1891`) locks accounts and calls
  `cancel_open_orders`, which includes protective stops and targets. No flatten happens. Automatic risk
  locks (`evaluate_live_risk`, `lib.rs:3186-3264`, and `check_loss_limits`) lock the account without
  flattening or cancelling. The plan (M2.6) describes kill as flatten plus lock.
- Impact: Pressing the kill switch with an open position leaves a naked position and the account is
  locked, so the user cannot place a new stop. Only the administrative flatten path (which itself has
  TR-04) can recover.
- Root cause: Kill was implemented as "lock plus cancel" before flatten existed, and was never revisited.
- Clean fix: Kill switch = lock, cancel non-protective working orders, flatten every position across all
  instruments of the targeted accounts (with runtime-owned prices from TR-01), then cancel remaining
  protection. Return a structured outcome that reports any position that could not be priced.
- Verification: Test with two instruments, each with a bracketed position: after kill, both are flat, no
  orders are open, the account is locked; restart test confirms the same state.

### TR-04: Flatten, flatten-all and event flatten only cover the observed instrument

- Severity: High. Category: Correctness.
- Evidence: `flatten_positions` (`lib.rs:4425-4446`) filters positions by
  `instrument_id == observation.instrument_id`. `flatten_account` and `flatten_all` (`lib.rs:4200` and
  the handle methods at `lib.rs:926-957`) take one observation. The doc comment on
  `TradingHandle::flatten_account` says it "closes one account's simulated positions", which is not
  what happens. `apply_economic_event_risk` has the same single-observation shape (`lib.rs:965-971`).
  Tests only cover single-instrument accounts.
- Impact: "Flatten all" leaves positions in other instruments open silently. Combined with TR-03 this
  makes emergency exits unreliable.
- Root cause: Pricing was supplied by the caller (the chart) because the runtime has no prices of its own.
- Clean fix: Depends on TR-01. Flatten commands take no observation; the runtime prices each position
  from its own latest quote and reports positions that cannot be priced as incomplete instead of skipping
  them.
- Verification: Multi-instrument flatten, flatten-all, and economic-event flatten tests.

### TR-05: Snapshot polling cost and the 10,000-order snapshot failure

- Severity: High. Category: Scalability / reliability.
- Evidence: The desktop polls the full `TradingSnapshot` every 250 ms
  (`apps/desktop/src/components/workspace_layout.rs:715-798`) and clones it into each workspace surface.
  Building the snapshot recomputes round trips, risk meters and adherence over all fills. Snapshot
  construction returns an error when more than 10,000 orders exist (`lib.rs:4592-4600`), while store
  retention keeps up to 250,000 orders. Desktop projections are quadratic: `chart_order_semantics` scans
  order events per order (`apps/desktop/src/desktop.rs:2013`) and `chart_executions` searches for the
  order of every fill (`desktop.rs:2143`). Runtime lookups by client order id are linear
  (`lib.rs:1902-1908`, `order_by_client_id` near `lib.rs:4109`, broker lookups in
  `venue/owner.rs:128`).
- Impact: An active paper trader passes 10,000 orders over weeks; from then on every snapshot errors and
  the trading UI goes blank with no recovery except deleting data. Before that point CPU cost grows
  linearly (runtime) and quadratically (desktop) with history, four times per second.
- Root cause: Pull-based full snapshots instead of an incremental event stream; history and working set
  are not separated.
- Clean fix: Split the snapshot into a bounded working set (open orders, positions, account summary, risk)
  pushed on change, plus paged history queries. Index orders by client order id and broker order id.
  Precompute per-order event lists once per revision instead of scanning.
- Verification: Load test with 50,000 historical orders measuring snapshot time and desktop frame time;
  regression test that snapshot succeeds above 10,000 orders.

### TR-06: Per-quote durable writes and retention that deletes the ledger

- Severity: High. Category: Performance / durability.
- Evidence: While a position is open, each observation calls `update_position` in its own transaction
  with `synchronous=FULL` (`crates/trading_runtime/src/store.rs:368-370`), runs `enforce_retention`
  (`store.rs:2154-2243`, several `DELETE ... ORDER BY ... OFFSET` statements), and writes the revision
  (`lib.rs:4945-4948`). Retention deletes fills, but positions are repaired from fills
  (`lib.rs:1136-1215`).
- Impact: Fsync per quote on a busy instrument limits throughput and wears the disk; observation backlog
  is mitigated only by coalescing. After retention prunes old fills, a projection repair rebuilds a
  position from an incomplete fill history, producing a wrong position or realized PnL.
- Root cause: Mark-to-market values are persisted as if they were ledger facts; retention treats the
  ledger like a log.
- Clean fix: Do not persist mark-to-market per quote (it is derivable); persist only ledger facts (orders,
  events, fills, cash). Run retention on a timer, and replace pruned fills with a durable per-position
  checkpoint so repair starts from the checkpoint.
- Verification: Test that repairs a position after retention prunes its early fills; benchmark quotes per
  second with an open position before and after.

### TR-07: Position protection is built from overlapping patch layers

- Severity: High. Category: Maintainability / correctness.
- Evidence: Protection is spread across: managed brackets with `reconcile_managed_brackets`,
  `recover_managed_brackets` and an "interrupted" repair state (`lib.rs:3266-3337`),
  `complete_managed_bracket` (`lib.rs:3983`); standalone protective orders; the resize pass
  `align_position_protection` (`crates/trading_runtime/src/protection.rs`, commit `7b423f14`); planned
  leg projections (`crates/trading_runtime/src/planned_leg.rs`, commit `7a184322`); the restored-order
  fence (`lib.rs:541-546`, commit `69896eee`); and a desktop policy that decides the resting entry order
  type (`apps/desktop/src/desktop.rs:2530`, commit `a63fedaa`). Broker admission validation is a separate
  copy (`venue/owner.rs:187-289` versus `prepare_single_order` at `lib.rs:2462-2547`).
- Impact: Every bracket bug fix so far has added a layer. Interactions (partial exits, restart in the
  middle of a bracket, resize after a scale-in) are hard to reason about, and the desktop must know
  runtime policy to place an entry correctly.
- Root cause: No first-class order-group model with parent/child links and position-sized protection.
- Clean fix: One order-group model (entry, OCO children, sizing rule) owned by the runtime, used for both
  simulated and broker routes, with a single admission validator. Remove the alignment pass, planned-leg
  projection and the desktop entry-type policy once the group model covers them.
- Verification: Property-style tests over random sequences of fills, cancels, scale-ins and restarts,
  asserting protection quantity equals position quantity and no orphan child orders exist.

### TR-08: Fill model gaps

- Severity: Medium. Category: Simulation realism.
- Evidence: Touch fills at full quantity (`lib.rs:3339-3412`, `touch_price` at `lib.rs:5035-5073`). No
  stop-limit triggered state; Day orders never expire; IOC and FOK are cancelled on the next observation
  (`lib.rs:4171-4198`) and FOK equals IOC; no tick-size check on prices in `prepare_single_order`; no
  partial fills, slippage or simulated fees.
- Impact: Paper results are optimistic, stop-limits behave incorrectly on gaps, and Day orders carry
  over sessions without the user knowing.
- Clean fix: Add an explicit triggered state for stop-limits, TIF expiry driven by the session calendar
  (TR-02), IOC/FOK evaluation at placement against the current quote, tick validation in the shared
  admission validator, and a pluggable fee schedule. Queue-aware fills remain the planned later step
  (PF9).
- Verification: Unit tests for gap-through stop-limit, Day expiry at session close, FOK with insufficient
  size, off-tick rejection, fee accounting.

### TR-09: The risk session never rolls over and no profile is registered

- Severity: Medium. Category: Risk.
- Evidence: `session_start_unix_nanos` and `peak_session_pnl` are set only when a risk profile is
  registered; no daily rollover exists. A search of `apps/` finds no `RiskProfile` registration, so the
  configured limits are only exercised in tests. The "no attached stop" warning (`lib.rs:2895`) fires
  for bracket entries whose stop is attached as a child.
- Impact: Daily loss and drawdown limits, once wired, would accumulate forever; the warning trains users
  to ignore risk warnings.
- Clean fix: Roll the risk session at the account's session boundary (persisted), add profile
  registration to the desktop settings, and treat bracket children as attached stops.
- Verification: Rollover test across a simulated session boundary and restart; warning test for bracket
  entries.

### TR-10: Desktop keeps per-pane copies of trading state and resolves intents against them

- Severity: Medium. Category: Architecture.
- Evidence: Each workspace surface stores a `TradingPnlState` copy (`apps/desktop/src/desktop.rs:914-934`)
  and resolves chart intents against it (`chart_intent_order` near `desktop.rs:3034`,
  `chart_intent_planned_leg` near `desktop.rs:2971`). Lock checks happen client-side. The toaster detects
  new fills by diffing snapshots (`apps/desktop/src/components/toaster.rs:696`). Risk warnings are
  detected by a "RISK WARNING" string prefix.
- Impact: Drag or cancel intents can target an order that already filled (race between snapshot polls),
  and UI behavior depends on message text.
- Clean fix: Intents carry order ids and the runtime resolves them; the runtime publishes typed events
  (fill, reject, warning) consumed by the toaster; the desktop keeps one shared view model.
- Verification: Test that a modify intent against a filled order is rejected with a typed error; toaster
  test driven by events.

### TR-11: Duplicate PnL trackers and no cash ledger

- Severity: Medium. Category: Maintainability / correctness.
- Evidence: Four computations of realized results: `risk_trade_cycles`, `trade_pnl_cycles`,
  `completed_trade_pnl` and `fill_realized_pnl` (`lib.rs:3414-3577`, `lib.rs:3629-3675`). Account
  balance is starting equity plus summed realized PnL (`lib.rs:4672-4726`).
- Impact: Risk, journal and account views can disagree after edge cases (reversal through zero, partial
  closes). No foundation for fees, deposits, margin or equity curves.
- Clean fix: One fill-driven ledger producing realized PnL, cash movements and round trips; risk and
  reporting read from it.
- Verification: Golden-ledger tests asserting all views agree for reversal and scale-out scenarios.

### TR-12: Smaller defects and debt

- Severity: Low. Category: Mixed.
- Cancel events use the order's original observation time, not the cancel time
  (`lib.rs:1917-1924`). Order history therefore shows cancels at placement time.
- `reverse_position` is flatten followed by a new market order; a risk rejection of the second step
  leaves the user flat with an error.
- `Coordinator` has about 150 methods across `lib.rs`, `venue/owner.rs`, `protection.rs` and
  `planned_leg.rs`.
- Two position models (`Position` net for simulated, `BrokerPosition` hedged for the removed FX/CFD broker) cause branching
  throughout; `VenueRoute` only has `Simulated` and one broker demo route.
- The Rithmic order plant (`crates/adapters/rithmic_protocol/src/order_plant.rs`, 1,747 lines) is unused outside its crate (plan item T5).
- Clean fix: Use the command time for cancel events (with a regression test); make reverse a single
  admitted operation; split `Coordinator` (see restructuring); keep the Rithmic plant out of the build
  until it is wired.

## Test coverage notes

Strong: golden ledgers (`crates/trading_runtime/tests/characterization.rs`), restart tests for managed
brackets, flatten and restored orders, risk lock tests, scripted broker venue tests.

Missing: stop-limit gap-through, Day expiry, IOC/FOK at placement, multi-instrument flatten and kill,
snapshot above 10,000 orders, stale or closed-market quotes for the other providers, partial fills,
retention followed by projection repair, and any end-to-end test from a desktop intent to a runtime fill.

## Recommended restructuring

Order matters: each step removes a reason for the patches that follow it.

1. Runtime-owned market data (TR-01, TR-02). `trading_runtime` registers quote demand with `MarketEngine`
   for instruments with open orders or positions and receives a provider-neutral session phase and quote
   age. Remove the chart-pane publication path.
2. Split `Coordinator` into modules with clear ownership: order book (indexed by order id, client order id
   and broker order id), execution engine (fill model), ledger, risk, and order groups. Keep a single
   owner thread; the split is internal.
3. Unified order-group model (TR-07) replacing managed brackets, standalone protection, the alignment
   pass, planned-leg projection and the desktop entry-type policy, with one admission validator for
   simulated and broker routes.
4. Fill-driven ledger with cash and fees (TR-06, TR-11). Persist ledger facts only, batch writes, run
   retention on a timer with per-position checkpoints.
5. Incremental publication to the desktop (TR-05, TR-10): bounded working-set snapshot pushed on change,
   typed events, paged history. Remove per-pane trading state copies.
6. Fill model completion (TR-08): stop-limit trigger state, TIF expiry, IOC/FOK at placement, tick
   validation, fee schedule; then queue-aware fills (PF9).
7. Kill and flatten semantics across all instruments (TR-03, TR-04), with structured incomplete outcomes.
8. Deterministic replay harness feeding recorded quote streams through `observe_market`, plus restart and
   end-to-end tests for each of the above.
