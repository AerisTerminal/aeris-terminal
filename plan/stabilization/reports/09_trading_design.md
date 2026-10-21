# Aeris Terminal: paper trading "solid core" design

Scope: `crates/trading_runtime`, `crates/domain/trading`, the market-side quote feed in `crates/market_engine` and
`crates/market_runtime`, and the desktop trading consumers in `apps/desktop`. This is a design only; no repository
file was changed and no build was run. Paths are relative to `C:\Users\devraj\Downloads\Softwares\aeris-terminal`.
Line numbers refer to the working tree at design time (`main`, 4 commits ahead of `origin/main`).

Inputs: `02_trading.md` (TR-01..TR-12), `03_desktop_ui.md` (UI-01, UI-08, UI-10), `01_market_data.md` (MD-02, MD-11,
MD-15), and a fresh read of the code listed in each section.

Out of scope by user decision: depth-aware partial fills, a slippage model, and margin or buying-power modelling.
Section 3 and section 4 name the hooks where they would attach.

## Facts from the code that shape the design

These were verified while writing this document and are referenced below.

1. `Coordinator` (`crates/trading_runtime/src/lib.rs:532`) is one struct whose `impl` spans `lib.rs`,
   `protection.rs`, `planned_leg.rs` and `venue/owner.rs`. `lib.rs` is 5,606 lines.
2. Every order lookup by client id is a linear scan (`order_by_client_id` at `lib.rs:4109`, idempotency checks in
   `place_order` at `lib.rs:2110`, `cancel_order_internal` at `lib.rs:1897`, `broker_order_id` at
   `venue/owner.rs:128`).
3. `TradingStore::load_state` (`store.rs:536`) loads every retained order, event and fill into memory (up to 250,000
   orders and 500,000 events by default).
4. `observe_market` (`lib.rs:3085`) persists mark-to-market through `update_position` (`lib.rs:4117`,
   `store.rs:2127`), then runs `enforce_retention` (`store.rs:2154`) and `bump_revision` on every changed quote.
5. `touch_price` (`lib.rs:5035`) fills resting limits at the touch (ask or bid), not at the limit price; stop-limit
   fills only when stop and limit conditions hold on the same quote.
6. Trailing stops exist only as a bracket management rule: `TrailingStopRule` and `BreakEvenRule`
   (`strategy.rs:16-27`) applied by `update_managed_stops` (`lib.rs:4033`) through `modify_order`. There is no
   standalone trailing-stop order type.
7. Session gating is `PracticeMarketSession` (`lib.rs:248`), installed by the desktop (`desktop.rs:5487`) and backed by
   `MarketService::require_practice_market_session` (`market_service/runtime.rs:1196`), which only answers for
   one (since-removed) broker provider. A provider-neutral `MarketService::market_session_status` already exists (`runtime.rs:1146`) and
   returns `MarketSessionStatus { phase, source, session_start, session_end, next_open }`
   (`crates/contracts/src/messages.rs:812-844`).
8. `MarketEngine` already has runtime-owned, non-presentation demand: data leases (`market_engine/src/lib.rs:548`),
   keyed by `BarSeriesKey`. Instrument-level demand that is not tied to a series exists only for price alerts, which
   `market_runtime` merges into `ProviderDemand` outside the engine (`market_service/realtime.rs:1855`).
9. `tools/naming_check` (`src/main.rs:2505-2597`) forbids `aeris_market_runtime` in `trading_runtime/Cargo.toml` and
   `aeris_trading_runtime` in `market_runtime/Cargo.toml`, asserts string contracts in `trading_runtime/src/lib.rs`
   (`MAXIMUM_SNAPSHOT_ITEMS`, `mpsc::sync_channel(COMMAND_CAPACITY)`, ...) and in `store.rs`, and allows `rusqlite`
   only under `crates/trading_runtime/src/`. `market_runtime` already depends on `aeris_trading` (domain) for the
   broker venue relay.
10. Hyperliquid instruments report `point_value = 1`, currency `USD`, no session hours, and a `price_increment` that is
    only "the valid increment near the catalog mark price at selection" because Hyperliquid ticks follow
    significant figures (`market_runtime/src/hyperliquid_realtime.rs:486-497`). A fixed tick check would be wrong for
    Hyperliquid.
11. The golden ledger tests (`src/tests/characterization.rs`, `simulated_golden.txt`, `simulated_stop_golden.txt`) say
    "Do not update the expected ledger to accommodate a simulated-path refactor". Several approved behaviors in this
    design (resting-limit fill price, cancel timestamp, typed event details, bracket legs as real orders) change those
    transcripts on purpose. See section 9, Q1.

## 1. Target module layout and public API

### 1.1 Principles of the split

- One owner thread remains (`aeris-trading-owner`). The split is internal; there is still exactly one mutable owner.
- The owner thread is a thin shell around a synchronous `TradingCore` with three entry points:
  `handle(command, now)`, `on_quotes(batch, now)` and `on_timer(now)`. Everything deterministic lives in the core, so
  tests and the replay harness drive the core directly with a manual clock (section 7).
- Each core step computes its changes first, commits them in one SQLite transaction, then applies them to memory and
  publishes. A failed commit leaves memory unchanged (this replaces the current store-then-memory per mutation, which
  can commit half of a fill-plus-bracket-arm sequence).
- Lookups are indexed. Linear scans over orders are removed.
- In memory: only the working set (accounts, instruments, open orders, active groups, positions, ledger balances, risk
  state, quote book, and a bounded recent window for the UI). History is read from SQLite with paged queries.

### 1.2 File layout

Legend: KEEP = file stays with the same role, MOVE = code moves, NEW = new file, DELETE = removed after migration.

```
crates/domain/trading/src/
  lib.rs              KEEP   FixedPoint, ids, Order, OrderEvent, Fill, Position, BrokerPosition, AccountPnl.
                             Order gains: revision, accepted/updated/triggered/terminal timestamps, expires_at,
                             group_id. OrderStatus gains Inactive, Expired. OrderEventKind gains Triggered,
                             Expired, Activated, Resized. New OrderEventReason (typed, replaces free-text detail
                             as the control-flow carrier; detail stays for display only).
  venue.rs            KEEP   Provider-neutral broker venue contract (unchanged).
  market.rs           NEW    Provider-neutral market feed contract shared by market_runtime and trading_runtime:
                             MarketRoute, TradingQuote, FeedState, InstrumentSession, InstrumentTerms,
                             QuoteInterest, MarketFeedEvent. No dependencies beyond aeris_instruments.

crates/domain/instruments/src/lib.rs
                      KEEP   ContractMetadata gains `price_grid: PriceGrid`
                             (Fixed(tick) | SignificantFigures{figures, max_decimals} | Unknown) and
                             `instrument_class: InstrumentClass` (Future, Equity, CryptoPerpetual, CryptoSpot,
                             ForexCfd, Option, Other).

crates/trading_runtime/src/
  lib.rs              KEEP   Re-exports and constants only (keeps the naming_check contract strings, or
                             naming_check is updated in the same commit that moves them).
  clock.rs            NEW    trait Clock { fn now_unix_nanos(&self) -> i64 }, SystemClock, ManualClock
                             (ManualClock behind cfg(test) or feature "replay").
  service/mod.rs      MOVE   TradingService handle, Command enum, request/reply, shutdown (from lib.rs:583-1120).
  service/owner.rs    MOVE   Owner loop: command receive, feed/venue inbox drains, timer deadline
                             (from Coordinator::run, next_command, drain_*).
  service/publish.rs  NEW    TradingSubscription, WorkingSet construction, typed TradingEvent fanout.
  service/history.rs  NEW    Read-only SQLite connection on a small reader thread for paged history queries.
  core/mod.rs         NEW    TradingCore: owns Book, Groups, Ledger, Risk, QuoteBook, Store; step functions;
                             StepChanges (computed changes) and commit-then-apply.
  core/recovery.rs    NEW    Startup recovery: checkpoint + fill replay verification, group invariant check,
                             expiry of DAY orders whose session ended while the app was closed.
  book/mod.rs         NEW    OrderBook: BTreeMap<OrderId, Order> plus indexes: client id -> order id,
                             broker order id -> order id, open orders by (account, instrument),
                             executable simulated orders by instrument.
  book/lifecycle.rs   NEW    The order state machine (section 2): pure transition functions returning
                             (Order, OrderEvent) or a typed TransitionError.
  book/trigger_index.rs NEW  Per instrument and side, BTreeMaps keyed by trigger/limit price so a quote only
                             visits orders whose condition can be met.
  admission/mod.rs    NEW    The single admission validator for simulated and broker routes (section 2.6).
                             Replaces prepare_single_order checks (lib.rs:2462) and the broker copy in
                             place_protected_broker_order (venue/owner.rs:187-289).
  admission/capabilities.rs NEW VenueCapabilities per route (supported TIFs, client id length, native SL/TP,
                             scale-out allowed, managed trailing allowed).
  admission/price_grid.rs NEW Tick and significant-figure validation.
  groups/mod.rs       NEW    OrderGroup model: brackets, OCO, position protection (section 2.4).
  groups/coverage.rs  MOVE   Protection coverage invariant (replaces protection.rs; same sizing rules).
  groups/management.rs MOVE  Trailing and break-even stop management (from update_managed_stops).
  groups/templates.rs MOVE   BracketStrategyTemplate, BracketTarget, TrailingStopRule, BreakEvenRule
                             (from strategy.rs; templates become presets that expand into a group request).
  execution/mod.rs    NEW    SimulatedVenue: pure evaluation of one order against one quote and session.
  execution/rules.rs  NEW    Trigger and fill rules per order type and time in force (section 3.2).
  execution/session.rs NEW   Session and freshness gating, DAY expiry computation.
  market/mod.rs       NEW    QuoteBook (latest quote, feed state, session per instrument), MarketFeedInbox
                             (generalizes PendingObservations, lib.rs:468), interest computation.
  ledger/mod.rs       NEW    Cash ledger: entries, balances, apply_fill (the only PnL implementation).
  ledger/position.rs  MOVE   Net position with exact cost basis (replaces next_position, lib.rs:5117, and
                             realized_pnl, lib.rs:5246).
  ledger/fees.rs      NEW    FeeSchedule per instrument class with per-instrument overrides.
  ledger/cycles.rs    MOVE   Trade cycles and round trips (round_trip.rs plus the four PnL trackers).
  ledger/equity.rs    NEW    Bounded equity history samples.
  ledger/checkpoint.rs NEW   Position and balance checkpoints used by retention and repair.
  risk/mod.rs         MOVE   risk.rs, discipline.rs, plan.rs (logic unchanged except session rollover).
  risk/session.rs     NEW    Persisted risk session with rollover at the account's session boundary.
  emergency.rs        MOVE   Kill switch, flatten (account, instrument, all), atomic reverse, economic-event flatten.
  copier.rs           KEEP
  venue/mod.rs        KEEP   VenueInbox (unchanged).
  venue/broker.rs     MOVE   broker route (from venue/owner.rs); uses admission/ and book/ instead of copies.
  venue/scripted.rs   KEEP   Scripted broker venue for tests (unchanged API, tests keep passing).
  store/mod.rs        MOVE   TradingStore open, pragmas, StepTx (one transaction per core step).
  store/schema.rs     MOVE   Migrations V1..V19 unchanged, new V20..V24 (section 5).
  store/migrate_groups.rs NEW Rust data migration of managed_brackets and protective_orders into groups.
  store/orders.rs, store/fills.rs, store/ledger.rs, store/groups.rs, store/risk.rs, store/user_records.rs,
  store/retention.rs, store/export.rs   MOVE  Split of store.rs (3,690 lines) by table family.
  replay/mod.rs       NEW    Deterministic replay harness (cfg(test) or feature "replay"), section 7.
  tests/...           KEEP   Existing tests; new scenario files per area.

DELETE after migration: protection.rs, planned_leg.rs, strategy.rs (moved), ManagedBracket, ManagedBracketStatus,
ProtectiveOrder, ProtectiveOrderRole (replaced by LegRole), PlannedBracketLeg, MovePlannedBracketLeg,
BracketPlacement, PracticeMarketSession, SimulatedMarketObservation (public), PendingObservations,
ObservationCursor, Coordinator::restored_orders, TradingSnapshot (after the desktop moves to WorkingSet).
```

`crates/market_engine/src/instrument_lease.rs` NEW and `crates/market_runtime/src/market_service/trading_feed.rs` NEW
are described in section 3.4.

### 1.3 Public command and view API (desktop-facing)

The handle stays `TradingService` (cloneable, blocking request methods called from background work). New request
types replace the overlapping ones. Errors become typed: `TradingCommandError { kind: TradingErrorKind, detail:
String }`, where `kind` covers `Rejected(RejectReason)`, `StaleRevision`, `NotFound`, `Overloaded`, `Unavailable`,
`Storage`. Text is produced only for display.

```rust
pub struct TradingServiceConfig {
    pub database_path: PathBuf,
    pub retention: TradingRetention,          // gains history windows and equity sample bounds
    pub clock: Arc<dyn Clock>,                // SystemClock in production
    pub execution: ExecutionPolicy,           // quote age bounds, quote wait for market orders, closed-market policy
}

impl TradingService {
    // composition (desktop main and the probe binary only)
    pub fn start(config: TradingServiceConfig) -> Result<Self, TradingCommandError>;
    pub fn attach_market_feed(&self, interest: QuoteInterestSink) -> Result<Arc<MarketFeedInbox>, TradingCommandError>;
    pub fn attach_demo_venue(&self) -> Result<(u64, Receiver<VenueRequest>), String>;   // unchanged
    pub fn demo_venue_inbox(&self) -> Arc<VenueInbox>;                                  // unchanged

    // accounts and configuration
    pub fn create_practice_account(&self, CreatePracticeAccount) -> Result<TradingAccount, _>;   // unchanged
    pub fn delete_practice_account(&self, TradingAccountId) -> Result<TradingAccount, _>;        // unchanged
    pub fn set_fee_schedule(&self, account: TradingAccountId, FeeSchedule) -> Result<(), _>;     // new
    pub fn register_risk_profile / register_session_plan / register_trade_copier /
           register_strategy_template / put_user_record / export                               // unchanged

    // orders
    pub fn submit(&self, OrderRequest) -> Result<OrderAck, _>;           // single, bracket, template bracket
    pub fn submit_oco(&self, OcoRequest) -> Result<GroupAck, _>;
    pub fn submit_protection(&self, ProtectionRequest) -> Result<GroupAck, _>;  // standalone SL and/or TP
    pub fn modify(&self, ModifyRequest) -> Result<Order, _>;             // carries expected_revision
    pub fn move_group_leg(&self, MoveLegRequest) -> Result<Order, _>;    // inactive or working leg
    pub fn cancel(&self, CancelRequest) -> Result<Order, _>;             // order id or client id, expected_revision
    pub fn cancel_all(&self, CancelScope) -> Result<CancelOutcome, _>;
    pub fn evaluate(&self, OrderRequest) -> Result<RiskEvaluation, _>;   // dry-run admission, no state change

    // position operations: no observation parameter; the runtime prices from its own quotes
    pub fn flatten(&self, FlattenScope) -> Result<EmergencyOutcome, _>;  // account and/or instrument or all
    pub fn reverse(&self, TradingAccountId, InstrumentId) -> Result<EmergencyOutcome, _>;   // atomic
    pub fn kill_switch(&self, KillScope, reason: String) -> Result<EmergencyOutcome, _>;
    pub fn lock_account / unlock_account                                                     // unchanged
    pub fn apply_economic_event_risk(&self, EconomicEventRiskTrigger) -> Result<EconomicEventRiskOutcome, _>;
    pub fn close_broker_position / amend_broker_position_sltp                                // unchanged

    // views
    pub fn subscribe(&self) -> Result<TradingSubscription, _>;          // change-driven, bounded
    pub fn working_set(&self) -> Result<Arc<WorkingSet>, _>;
    pub fn history(&self, HistoryQuery) -> Result<HistoryPage, _>;      // paged, served by the reader thread
    pub fn status(&self) -> Result<TradingServiceStatus, _>;
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String>;
}

pub struct OrderRequest {
    pub client_order_id: ClientOrderId,
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub quantity: FixedPoint,
    pub kind: OrderKind,
    pub time_in_force: TimeInForce,
    pub attached: Option<AttachedProtection>,
    pub submitted_unix_nanos: Option<i64>,       // client time for audit only; the owner clock stamps acceptance
}
pub enum OrderKind {
    Market,
    Limit { price: FixedPoint },
    Stop { trigger: FixedPoint },
    StopLimit { trigger: FixedPoint, limit: FixedPoint },
    /// Rest at `price` until the market reaches it. The runtime chooses Limit (passive side of its own
    /// touch) or Stop (beyond the touch). Replaces the desktop policy resting_entry_order_type (desktop.rs:2530).
    RestingAt { price: FixedPoint },
}
pub enum AttachedProtection {
    Bracket { stop_ticks: u32, targets: Vec<BracketTarget>, management: StopManagement },
    Template { template_id: String },
}
```

`TradingProvenance` is no longer supplied by the caller: the owner stamps venue, provider (from the instrument route),
and the quote revision used at acceptance. This removes the desktop's fabricated provenance
(`apps/desktop/src/trading.rs:690-735`).

Removed from the public API after the desktop migrates: `observe_market`, `publish_market_observation`,
`install_practice_market_session`, `place_order`, `place_bracket`, `place_inline_bracket`, `place_protective_order`,
`move_planned_bracket_leg`, `flatten_account(observation)`, `flatten_all(observation)`, `reverse_position(observation)`,
`snapshot`. During migration both forms exist; old forms delegate to new ones (section 8 says when each is removed).

## 2. Order state machine and order groups

### 2.1 States

| Status | Meaning | Executable | Open |
|---|---|---|---|
| `Inactive` (new) | Group child waiting for its parent fill, or an OCO member held by its group. Visible on charts. | No | Yes |
| `Pending` | Broker only: submitted, not acknowledged. | No | Yes |
| `Working` | Accepted and live. A stop-limit that has triggered stays `Working` with `triggered_unix_nanos` set. | Yes | Yes |
| `PendingModify`, `PendingCancel` | Broker only: request in flight. | Yes (broker decides) | Yes |
| `PartiallyFilled` | Some quantity filled. The simulator does not produce it in this scope; broker fills and the state machine support it. | Yes | Yes |
| `Filled` | Terminal. | No | No |
| `Cancelled` | Terminal; `OrderEventReason` says why (user, cascade, kill switch, IOC/FOK not marketable, no quote). | No | No |
| `Expired` (new) | Terminal; DAY session ended. Previously DAY never expired. | No | No |
| `Rejected` | Terminal; typed `RejectReason`. | No | No |

`Triggered` is an event and a timestamp, not a status. A triggered stop (market) fills in the same step; a triggered
stop-limit becomes a resting limit. Keeping it out of `OrderStatus` avoids a status that the broker route would need to
map, and keeps `is_open`/`is_executable` (`crates/domain/trading/src/lib.rs:430-447`) simple.

### 2.2 Transitions (simulated route)

```
(new) --admit ok--> Working                       event Accepted{warnings: Vec<RiskWarningKind>}
(new) --admit ok, group child--> Inactive         event Accepted{held_by: group}
(new) --admit fails--> (not stored)               error RejectReason (no row; same as today)
(new) --marketable at placement--> Working -> Filled in the same step   events Accepted, Filled
(new) --IOC/FOK not marketable--> Working -> Cancelled same step        events Accepted, Cancelled{NotMarketable}
Inactive --parent filled--> Working               event Activated{priced_from_fill}
Inactive --parent cancelled/expired/rejected--> Cancelled  event Cancelled{Cascade}
Inactive --leg moved / parent repriced--> Inactive         event Modified{LegMoved | ParentRepriced}
Working --modify--> Working                       event Modified{User | Management | Coverage}
Working --resize by coverage--> Working           event Resized{CoverageShrink | CoverageGrow}
Working --stop condition--> (Triggered event) --> Filled (Stop) or Working+triggered (StopLimit)
Working --fill condition--> Filled                event Filled{fill_id}
Working --cancel--> Cancelled                     event Cancelled{User | Cascade | KillSwitch | Flatten}
Working --DAY session end--> Expired              event Expired{session_end}
Working --market order quote wait elapsed--> Cancelled  event Cancelled{NoQuote}
```

The broker route maps `VenueEvent`s onto the same transition functions (Pending -> Working on ack, PendingModify on
amend request, etc.). Only `book/lifecycle.rs` constructs status changes; `venue/broker.rs` calls it instead of
building `Order` values by hand.

### 2.3 Idempotency, revisions and timestamps

- `client_order_id` stays unique (`orders.client_order_id UNIQUE`). Re-submitting the same id with identical
  parameters returns the existing order (today's behavior); with different parameters it fails with
  `RejectReason::ClientIdReused` (today it silently returns the existing order, `lib.rs:2132-2139`).
- `Order.revision: u32` increments on every transition. `ModifyRequest`, `CancelRequest` and `MoveLegRequest` carry
  `expected_revision: Option<u32>`. Chart drags always send it; a mismatch returns `StaleRevision` with the current
  order. This closes the race in TR-10 (a drag against an order that already filled).
- Timestamps on `Order`: `submitted_unix_nanos` (client, audit), `accepted_unix_nanos` (owner clock),
  `updated_unix_nanos`, `triggered_unix_nanos`, `terminal_unix_nanos`, `expires_at_unix_nanos` (DAY only),
  `eligible_after_unix_nanos` (see section 3.3). All owner-produced times come from the injected `Clock`. Fill and
  trigger events use the quote's local receive time. Cancel events use the command time, which fixes the TR-12 defect
  (`lib.rs:1917-1924` uses the order's original observation time).
- Event sequence numbers (`next_sequence`) and id formats (`sim-order-{n}`, `sim-event-{n}`, `sim-fill-{n}`) stay.

### 2.4 Order-group model

One model replaces managed brackets, standalone protective orders, planned-leg projections, the alignment pass and the
restored-order fence.

```rust
pub struct OrderGroup {
    pub group_id: String,                 // bracket: the entry client id (same as today's bracket_id)
    pub account_id: TradingAccountId,
    pub instrument_id: InstrumentId,
    pub kind: GroupKind,                  // Bracket | Oco | Protection
    pub status: GroupStatus,              // Pending | Active | Completed | Cancelled
    pub entry: Option<OrderId>,           // Bracket only
    pub legs: Vec<GroupLeg>,              // stop first, then targets in trigger order; bounded (1 + MAXIMUM_BRACKET_TARGETS)
    pub management: StopManagement,       // trailing and break-even rules (today's template fields)
    pub template: Option<(String, u32)>,  // template id and revision, for audit
    pub revision: u32,
}
pub struct GroupLeg {
    pub order_id: OrderId,
    pub role: LegRole,                    // StopLoss | TakeProfit { index }
    pub offset_ticks: Option<u32>,        // bracket legs: distance from the entry price
    pub allocation_percent: Option<u8>,   // bracket targets
}
```

Semantics:

- **Bracket placement.** Entry is admitted; legs are created at once as real `Inactive` orders (stop, then targets),
  priced at the entry's resting price plus or minus their offsets, with the same client ids the arming path uses today
  (`managed_child_client_order_id`), so a chart line keeps its identity when the leg activates. A market entry has no
  resting price; its legs are created `Inactive` without a price and priced at activation. This removes
  `planned_leg.rs`: the planned legs are the inactive orders.
- **Parent modify.** Repricing the entry reprices inactive legs (keep offsets), in the same step.
- **Leg move while inactive.** Updates the leg's `offset_ticks` and its price (today's `move_planned_bracket_leg`
  semantics: a moved leg keeps its new distance from the entry).
- **Parent fill (activation).** Legs are priced from the actual fill price plus or minus offsets (today's behavior),
  sized to the exposure the entry opened (today's `opened_exposure`, `lib.rs:3760`), and move to `Working`. Zero
  opened exposure completes the group and cancels its legs. If legs cannot be priced or admitted, the group is
  cancelled and a typed `ProtectionNotPlaced` event is raised on the filled entry (today's `reject_managed_bracket`).
  Activation, the fill, the ledger entries and the group status commit in one transaction, so the "interrupted"
  repair path in `recover_managed_brackets` (`lib.rs:3266`) is no longer needed.
- **Parent partial fill** (broker only for now): legs activate with the filled quantity and grow with later fills.
- **Sibling fills.** Stop fill cancels all targets and completes the group. A target fill reduces the stop by the
  filled quantity; the last target fill cancels the stop and completes the group. Explicit OCO groups (for example a
  breakout buy stop and sell stop) cancel the other members on a full fill and reduce them on a partial fill.
- **Cancel cascades.** Cancelling or expiring the entry of a pending bracket cancels its legs. Cancelling an active
  stop cancels the group's targets (today's `reconcile_managed_cancellation` rule). Cancelling a target only removes that
  target.
- **Protection groups.** A standalone stop loss or take profit becomes a one-leg `Protection` group; placing both
  creates a two-leg group. These have no entry.
- **Coverage invariant** (`groups/coverage.rs`, logic moved from `protection.rs`). After any fill that changes a
  position, in the same step: protection orders on the wrong side or for a flat position are cancelled; for each role,
  if any protective order of that role exists, their total equals |net position|. Growth goes to the order that
  triggers first; a shrink is taken from the orders that trigger last (the current rules, `protection.rs:190-237`).
  The invariant spans groups, so a manual partial close shrinks bracket legs and a reversal completes every group.
  Whether targets should grow on a manual scale-in is Q9 in section 9.
- **Stop management.** Trailing and break-even rules ratchet the stop through `Modified{Management}` only when the
  stop moves by at least one tick (persisted as a fact, never per quote). Managed trailing is refused on broker routes
  (today's rule, `lib.rs:2198-2203`).
- **Broker brackets.** Unchanged: a broker bracket is one entry with server-side SL/TP (`place_broker_bracket`,
  `venue/owner.rs:140`); no local group is created because the broker owns that protection.
- **Restart.** Groups and legs are rows (`order_groups`, `order_group_legs`) written in the same transaction as the
  order transitions that change them. Startup runs `core/recovery.rs`: it verifies every active group (legs exist, are
  open, coverage holds) and repairs deterministically with a typed `RecoveryNotice` event if a legacy database violates
  an invariant. No in-memory fence is needed (section 3.3).

### 2.5 Kill switch, flatten and reverse (emergency.rs)

- **Kill switch** (TR-03): lock the targeted accounts; cancel non-protective working orders; flatten every position of
  those accounts across all instruments at the runtime's own quotes; cancel the protection of positions that were
  flattened. A position that cannot be priced (no live quote) keeps its protection and is reported in
  `EmergencyOutcome::unpriced`. Everything commits in one step.
- **Flatten** (TR-04): `FlattenScope { account: Option, instrument: Option }`. No observation parameter. Each position is
  priced from `QuoteBook` (sell at bid, buy at ask). Unpriced positions are listed, never skipped silently. Broker
  accounts keep today's request-based close (`close_broker_positions_on`) for every instrument, not only the observed
  one.
- **Economic-event flatten**: same path, no observation (`apply_economic_event_risk` no longer needs one).
- **Reverse** (TR-12): one admitted operation. Admission validates the opposite position (risk, contracts, lock) before
  any change; then the closing fill and the opening fill commit in one step at the same quote. A rejection leaves the
  position untouched.

### 2.6 One admission validator

`admission::admit(ctx: &AdmissionContext, request: &OrderRequest, route: VenueRoute) -> Result<Admitted, RejectReason>`
is the only place that checks: account registered and route valid (live broker routes stay data-only); account lock and
risk (calls `risk::evaluate`, unchanged rules); instrument registered and scales; price grid (tick or significant
figures); quantity increment (`check_quantity_increment`, `lib.rs:5085`); price relations (buy stop above ask, sell stop
below bid, stop-limit limit relation, bracket legs on correct sides); TIF supported by the route
(`VenueCapabilities`; the broker route keeps Day mapped to GTC, `venue/owner.rs:1252`); client id length (per venue); open-order
capacity (`MAXIMUM_OPEN_ORDERS`); quote-interest capacity; for the simulated route only: currency match, session phase
and quote freshness (section 3.3). Brackets, OCO and protection requests validate every member through the same function.
Hook for later work: an `admission::BuyingPower` step (no-op now) is where margin checks would attach.

## 3. Simulated execution engine

### 3.1 Quote model

```rust
pub struct TradingQuote {                 // aeris_trading::market
    pub instrument_id: InstrumentId,
    pub bid: FixedPoint, pub ask: FixedPoint,           // required, bid < ask, instrument price scale
    pub bid_size: Option<FixedPoint>, pub ask_size: Option<FixedPoint>,   // carried, unused in this scope
    pub last: Option<FixedPoint>,
    pub provider_id: String,
    pub provider_generation: u64,         // provider session generation; a newer generation retires older quotes
    pub source_sequence: u64,             // provider ordering evidence (never inferred from ids)
    pub exchange_unix_nanos: Option<i64>, // provider/exchange time, for audit
    pub received_unix_nanos: i64,         // local receive time stamped by market_runtime; used for eligibility and age
}
pub enum FeedState { Live, Stale { reason: StaleReason }, Unavailable { reason: UnavailableReason } }
pub struct InstrumentSession {
    pub instrument_id: InstrumentId,
    pub phase: SessionPhase,              // mirrors MarketSessionPhase: Regular, PreMarket, PostMarket, Overnight,
                                          // Closed, AlwaysOpen, Unknown
    pub source: SessionSource,            // ProviderCalendar, ProviderHours, Continuous, Unavailable
    pub session_end_unix_nanos: Option<i64>,
    pub next_open_unix_nanos: Option<i64>,
}
```

`QuoteBook` keeps, per instrument: the latest quote, `FeedState`, `InstrumentSession`, and the last provider
generation. It is in memory only. Quotes are validated (positive, bid < ask, scale equals the instrument price scale);
an older generation or a lower `source_sequence` in the same generation is ignored.

### 3.2 Trigger and fill rules

Conventions: a buy is evaluated against the ask, a sell against the bid. "Marketable" means executable against the
current quote. All prices are exact fixed point; no rounding.

| Order | At placement (current fresh quote) | Resting, on each new eligible quote |
|---|---|---|
| Market | Fill at ask (buy) or bid (sell). With no fresh quote: stays Working and fills on the first eligible quote within `ExecutionPolicy::market_quote_wait` (default 5 s), otherwise Cancelled{NoQuote}. | Same as left (wait only). |
| Limit buy at L | If ask <= L: fill at ask (aggressor). | If ask <= L: fill at L (passive; changed from today's touch price, see Q7). |
| Limit sell at L | If bid >= L: fill at bid. | If bid >= L: fill at L. |
| Stop buy at S | Admission requires S > ask. | If ask >= S: Triggered, fill at ask (a gap fills at the gapped ask). |
| Stop sell at S | Admission requires S < bid. | If bid <= S: Triggered, fill at bid. |
| Stop-limit buy S/L | Admission requires S > ask. | If ask >= S: Triggered and becomes a resting limit at L; on the same quote, if ask <= L fill at ask (marketable at trigger); later quotes use the resting-limit rule (fill at L when ask <= L). Fixes the gap case in TR-08. |
| Stop-limit sell S/L | Admission requires S < bid. | Mirror image. |
| RestingAt P | Resolved at admission: buy with P <= ask becomes Limit P, P > ask becomes Stop P (sell mirrored). | As resolved. |

Time in force:

- **GTC**: no expiry.
- **DAY**: `expires_at` is fixed at acceptance from the instrument session: the current session's end if the session is
  open, otherwise the next session's end (`next_open` gives the session; the end comes from the calendar). The owner
  timer expires the order at that time with `Expired{session_end}`; startup recovery expires DAY orders whose time
  passed while the app was closed. Continuous markets and unknown calendars need a decision (Q2).
- **IOC and FOK**: evaluated once, at placement, against the current fresh quote: if marketable, fill fully; otherwise
  Cancelled{NotMarketable} in the same step. They never rest. Without a depth model IOC and FOK behave identically
  (all-or-nothing at the touch); this is stated in the UI help and the docs. Hook: an `execution::Liquidity` trait (only
  `TouchLiquidity` now) is where depth-aware partial fills would attach. An `execution::FillPrice` trait (only
  `TouchPrice` now) is where a slippage model would attach.
- IOC/FOK are allowed only with Market and Limit (admission). Group legs are GTC (today's behavior).

Evaluation order within one quote is deterministic: stops and stop-limits first (by trigger price, then acceptance
sequence), then limits (by price, then sequence). After each fill the group reactions run before the next candidate is
evaluated, so an OCO sibling cancelled by the first fill cannot fill on the same quote. Orders activated or modified
during a step become eligible only from the next quote (today's behavior for armed children).

Gap handling: stops fill at the gapped quote (realistic adverse fill); resting limits fill at their limit price, never
better; the first quote after a session opens is evaluated like any other quote; quotes during a Closed phase update
marks but fill nothing.

### 3.3 Session and freshness gating (provider-neutral)

- **Fill eligibility** for a resting order requires all of: `FeedState::Live`; session phase is not `Closed`; the quote
  is from the current provider generation; `quote.received_unix_nanos > order.eligible_after_unix_nanos`.
  `eligible_after` is set to the owner clock at acceptance, modification or activation. Because both values come from
  the local wall clock, the rule survives restarts without the `restored_orders` fence (`lib.rs:541-546`) and without
  comparing provider generations across processes, which `observe_market` does today (`lib.rs:3105-3129`). The clock
  is made monotonic per process (never earlier than the last stamped time) to tolerate wall-clock steps.
- **Placement** of a marketable order (Market, IOC, FOK, marketable limit) also requires the quote age
  (`now - received`) to be within `ExecutionPolicy::max_quote_age` for the instrument class (Q4). Resting orders are
  accepted without a quote.
- **Closed phase**: resting orders are accepted and become eligible at the open; marketable orders are rejected with
  `RejectReason::MarketClosed` (Q3). **Unknown phase** (no calendar): fills are allowed while the feed is Live (quotes
  flowing is the evidence), and DAY is rejected because its expiry cannot be computed (Q2).
- This replaces `PracticeMarketSession` and the single-provider path. `MarketService::require_practice_market_session`
  and `BrokerApi::require_practice_market_session` are deleted once nothing calls them (TR-02).

### 3.4 How quotes reach the engine

Ownership: `MarketEngine` remains the single owner of market demand; `market_runtime` owns provider sessions and
canonical books; `trading_runtime` owns trading. Neither runtime depends on the other (naming_check). The contract is
the domain module `aeris_trading::market`; the desktop composition root wires the two ends, exactly like
the broker venue attachment (`apps/desktop/src/desktop.rs:4134`).

**Trading side (interest).** After every step, `TradingCore` recomputes its quote interest: the instruments of simulated
accounts that have an open order (including Inactive legs), a non-flat position, or an active group. If the set changed,
it sends `QuoteInterest { generation, routes: Vec<MarketRoute> }` through the `QuoteInterestSink` (a
`Box<dyn Fn(QuoteInterest) -> Result<(), String> + Send>` supplied by `market_runtime`). The set is bounded by
`MAXIMUM_QUOTE_INTEREST` (256, the same bound as today's `MAXIMUM_PENDING_OBSERVATIONS`); admission rejects an order
that would exceed it. `MarketRoute { provider_id, instrument_id, provider_symbol, display_symbol, venue_id,
entitlement_id, price_scale, quantity_scale }` is persisted on the trading instrument row so that after a restart the
market side can subscribe instruments that no chart shows.

**Market side (demand and delivery).**

1. `crates/market_engine/src/instrument_lease.rs` (new): instrument-level leases
   `set_instrument_leases(owner: LeaseOwner, keys: BTreeSet<InstrumentStreamKey>, streams)` with
   `InstrumentStreamKey { provider_id, instrument_id, entitlement_id }`, bounded capacity, and
   `instrument_subscriptions()`. This keeps trading demand inside `MarketEngine` instead of copying the price-alert
   pattern that lives outside it.
2. `crates/market_runtime/src/market_service/trading_feed.rs` (new): holds the attached `TradingFeedLink { events:
   Box<dyn Fn(MarketFeedEvent) -> Result<(), String> + Send> }` (same slot pattern as the broker venue `VenueSlot`) and
   the current interest. On a `QuoteInterest` command the coordinator resolves each route in its catalog, installing it
   from the route if absent (through the existing `install_provider_instrument` path), and replaces the engine's
   instrument leases.
3. `provider_demand` (`realtime.rs:1813`) and `reconcile_order_books` (`coordinator.rs:1085`) include
   `engine.instrument_subscriptions()` with `MarketStream::Quotes`, so providers subscribe top of book and the canonical
   order book exists without a chart. Each provider worker must map a Quotes-only instrument demand to a top-of-book
   subscription; this needs verification per provider (risk R3).
4. When a canonical book's best bid or ask changes for an instrument that is leased by trading or already has an active
   book for a chart, the coordinator emits `MarketFeedEvent::Quote`. Delivering quotes for chart-active books too means
   a user usually has a quote at placement time for the instrument they are looking at; it costs nothing because the
   inbox is latest-wins per instrument.
5. It emits `MarketFeedEvent::Feed { instrument, state }` when the provider session for a leased instrument goes up,
   down, or changes generation, and `MarketFeedEvent::Session(InstrumentSession)` from
   `MarketService::market_session_status` on lease, on calendar revision change, and at the next session boundary
   (coordinator timer).
6. It emits `MarketFeedEvent::Terms(InstrumentTerms)` when an instrument enters its catalog. The conversion from
   `InstallProviderInstrument` to provider-neutral contract terms moves from the desktop (`trading_instrument_from_install`
   and `complete_contract_terms`, `apps/desktop/src/trading.rs:897-1000`) into `market_runtime`, which already owns
   provider metadata. `trading_runtime` registers instruments from these events, so the desktop calls to
   `register_provider_instrument` (`engine_market_worker/runtime.rs:300`, `selection_commands.rs:227`) are deleted.

**Inbox.** `MarketFeedInbox` (trading side) keeps the newest quote per instrument and a bounded FIFO (capacity 1,024)
for Feed, Session and Terms events; it never blocks the market coordinator. Quote overflow is impossible (latest-wins,
bounded by instrument count); FIFO overflow drops the oldest Terms event and sets a resync flag, after which trading
requests a full session and feed refresh through the interest sink. One queued `Command::DrainMarketFeed` wakes the
owner, like `DrainObservations` today.

**Desktop wiring** (desktop.rs, replaces `install_practice_market_session` and `publish_simulated_market_observation`):

```rust
let inbox = trading.attach_market_feed(market.trading_quote_interest_sink())?;
market.attach_trading_feed(aeris_market_runtime::TradingFeedLink {
    events: Box::new(move |event| inbox.push(event)),
});
```

### 3.5 Deterministic clock

`Clock` is injected through `TradingServiceConfig`. The owner loop waits with `recv_timeout(next_deadline - now)`, where
`next_deadline` is the earliest of: DAY expiries (a `BTreeMap<i64, BTreeSet<OrderId>>`), market-order quote waits,
risk session rollover, equity sample flush (60 s), retention run (hourly), coalesced publication (100 ms), and the
existing broker PnL refresh (`UNREALIZED_PNL_INTERVAL`). `TradingCore::on_timer(now)` handles all of them. Tests and the
replay harness never sleep: they set the `ManualClock` and call `on_timer`.

## 4. Ledger and positions

### 4.1 Positions (single PnL implementation)

`ledger/position.rs`:

```rust
pub struct PositionState {
    pub account_id, pub instrument_id,
    pub net_quantity: FixedPoint,
    pub cost_basis: i128,          // sum of price_units * quantity_units of the open quantity, exact
    pub cost_scale: (u8, u8),      // price scale, quantity scale
    pub realized_gross: FixedPoint,// account currency scale, cumulative
    pub fees: FixedPoint,          // cumulative
    pub cycle: TradeCycleState,    // open cycle id, peak quantity, cycle realized
    pub opened_unix_nanos: i64, pub last_fill_unix_nanos: i64,
}
```

`apply_fill(position, fill, terms, fee) -> FillAccounting { position_after, realized_gross, fees: Vec<FeeLine>,
cycle_event }`. Average entry is `cost_basis / |net|` computed for display only, so realized PnL no longer depends on a
rounded average (`weighted_average_entry_price`, `lib.rs:5199`, rounds to price scale today). Realized PnL on a close is
`(exit_units * closed_qty - basis_share) * point_value`, using `project_unrealized_pnl` rounding policy
(`domain/trading/src/lib.rs:146`) only at the final conversion to currency scale. Reversal through zero closes the old
cycle and opens a new one in the same fill. This replaces `next_position`, `realized_pnl`, `risk_trade_cycles`,
`trade_pnl_cycles`, `completed_trade_pnl`, `fill_realized_pnl` and the projections in `round_trip.rs` (TR-11). Risk,
journal, round trips and account views all read `FillAccounting` rows.

Point value and multiplier come from `ContractMetadata::point_value` (today). Currency: the simulated route keeps the
rule that contract currency equals account currency (`lib.rs:2493`); a `CurrencyConversion` hook is reserved for later.

### 4.2 Cash ledger

`ledger_entries(account_id, sequence, kind, amount_units, amount_scale, fill_id?, at_unix_nanos)`, where `kind` is one
of `Deposit` (opening equity), `Adjustment` (user reset or correction), `RealizedPnl`, `Commission`, `ExchangeFee`, and
reserved `Funding` and `MarginReserve` (not produced in this scope).

- balance = sum of entries (kept as a running balance per account in memory and in `ledger_checkpoints`).
- equity = balance + sum of unrealized over the account's positions (in memory only).
- realized today, fees today = sums since the risk session start (indexed by time).
- Broker accounts: balance stays the broker-reported value (`broker_balances`, authoritative); broker deals produce
  informational `RealizedPnl`, `Commission` and `Swap` entries from `RealizedClose`. The broker path is not
  re-computed locally.

### 4.3 Fees

```rust
pub struct FeeSchedule { pub by_class: BTreeMap<InstrumentClass, FeeModel>, pub by_instrument: BTreeMap<InstrumentId, FeeModel> }
pub enum FeeModel {
    None,
    PerContract { per_side: FixedPoint },                              // futures
    PerUnit { per_unit: FixedPoint, minimum: Option<FixedPoint>, maximum_bps: Option<u32> },  // equities
    NotionalBps { maker_bps: u32, taker_bps: u32 },                    // crypto, in hundredths of a basis point if needed
}
```

Maker/taker: a fill at placement or on trigger is taker; a resting limit fill is maker. Fees are computed in the account
currency with explicit rounding (nearest, ties away from zero, same policy as PnL). Schedules are stored per account in
`fee_schedules` and set through `set_fee_schedule`. Default values are Q5.

### 4.4 Unrealized and equity history

- Unrealized PnL is computed on each quote in memory (long marks at bid, short at ask, today's convention) and is never
  persisted. The `positions.unrealized_*` columns are dropped (section 5).
- `ledger/equity.rs`: an in-memory ring per account (last 24 h at 1-minute resolution, at most 1,440 samples) and a
  durable `equity_samples` table written by the 60 s timer only when equity changed, plus one sample at each fill and
  at each risk session end. Retention keeps 1-minute samples for 7 days and hourly samples for 1 year (downsampled by the
  retention job). Bounded per account.

### 4.5 Retention and checkpoints (TR-06)

- Retention runs on the hourly timer and at startup, never on the quote path. Each run deletes at most 5,000 rows per
  table so a run cannot stall the owner.
- A run picks a cutoff sequence. In one transaction it writes `position_checkpoints` (net, cost basis, realized, fees,
  cycle state as of the cutoff) for every (account, instrument) with fills at or before the cutoff, and
  `ledger_checkpoints` (balance as of the cutoff); then it deletes terminal orders, their events, fills, fill accounting
  and ledger entries at or before the cutoff that no open order or active group references.
- Repair (`core/recovery.rs`) rebuilds a position as checkpoint plus the fills after it, and compares it with the stored
  projection. This replaces `rebuild_position_from_retained_fills` (`lib.rs:1197`), which today rebuilds from a history
  that retention may have truncated.
- The snapshot limit problem (TR-05) disappears: the working set has no history, and history is paged.

## 5. Persistence

### 5.1 Write rules

- One `StepTx` per core step (command, quote batch, timer). It contains every fact the step produced: orders, events,
  fills, fill accounting, ledger entries, position projections, group rows, risk locks and state, `next_sequence` and
  `revision`. A step that produced no fact opens no transaction (a pure mark-to-market quote writes nothing).
- Persisted facts: orders and transitions, events, fills, fill accounting, ledger entries, positions (projection, written
  with the fill), groups, risk profiles, locks, risk sessions, checkpoints, equity samples (timer), user records.
- Never persisted: quotes, marks, unrealized PnL, risk meters, the quote book.
- `journal_mode=WAL`, `synchronous=FULL` and `foreign_keys=ON` stay (`store.rs:368-370`); with facts only, fsync happens
  per order action and fill, not per quote.
- The reader thread uses a second, read-only connection for history pages (WAL allows concurrent readers). The owner
  remains the only writer.

### 5.2 Schema changes (from version 19)

Before migrating a database at version 19 or lower, the store writes a copy with `VACUUM INTO
'<name>.v<old>.bak'` next to the database (deleted after a later successful start with the new schema, keeping one).
Migrations are transactional per version, as today (`store.rs:484`). Users' `trading.sqlite3` files are migrated, never
wiped.

| Version | Change | Data migration |
|---|---|---|
| V20 | `fill_accounting(fill_id PK REFERENCES fills ON DELETE CASCADE, account_id, instrument_id, realized_gross_units/scale, commission_units/scale, fees_units/scale, cycle_id, cycle_closed, cycle_realized_units/scale)`; `trade_cycles(account_id, instrument_id, cycle_id, peak_quantity, realized, PRIMARY KEY(account_id, instrument_id))`; indexes `orders(status) WHERE open`, `orders(account_id, instrument_id)`, `fills(account_id, instrument_id, execution_unix_nanos)`, `order_events(order_id, sequence)`. | SQL: backfill from `fills.realized_pnl_*` and `fills.completed_trade_pnl_*`; `trade_cycles` from `trade_pnl_cycles` and `risk_trade_cycles`. Fees zero. |
| V21 | `orders` add `revision`, `accepted_unix_nanos`, `updated_unix_nanos`, `triggered_unix_nanos`, `terminal_unix_nanos`, `expires_at_unix_nanos`, `eligible_after_unix_nanos`, `group_id`; `order_events` add `reason_code`, `reason_json`; status values `inactive`, `expired`; event kinds `triggered`, `expired`, `activated`, `resized`. | SQL: `accepted = submitted`, `updated = submitted`, `revision = 1`; `reason_code` derived from the existing detail strings once (`touch fill`, `cancelled by local command`, `RISK WARNING ...`, `LOCAL-MANAGED ...`); detail kept for display. Open DAY orders get `expires_at` at the first start from the session calendar (recovery), not in SQL. |
| V22 | `order_groups(group_id PK, account_id, instrument_id, kind, status, entry_order_id, management_json, template_id, template_revision, revision)`, `order_group_legs(group_id, order_id UNIQUE, role, target_index, offset_ticks, allocation_percent)`. | Rust (`store/migrate_groups.rs`) in the same transaction: each `managed_brackets` row becomes a Bracket group. AwaitingEntry: insert Inactive leg orders priced from the entry's resting price with the existing child client ids (exactly what `planned_legs` projects today). Active: link existing stop and target orders. Completed/Cancelled: terminal groups. Each `protective_orders` row becomes a one-leg Protection group. Brackets whose legs cannot be priced become Cancelled groups with a `RecoveryNotice` (Q6). |
| V23 | `ledger_entries`, `ledger_checkpoints`, `position_checkpoints`, `fee_schedules`, `equity_samples`, `risk_sessions`; `instruments` add `instrument_class`, `price_grid_json`, `market_route_json`; `positions` rebuilt with `cost_basis_hi/lo`, `cost_price_scale`, `cost_quantity_scale`, `realized_gross`, `fees`, without `unrealized_*`. | Rust: `Deposit` entry per simulated account from `starting_equity` (accounts without it start with no deposit, which matches today's balance); `RealizedPnl` entries from `fill_accounting`; cost basis = average x quantity for open positions (exact for the stored average, so no PnL jump); `market_route_json` null until the next Terms event (instrument shows FeedState Unavailable "route unknown" until then, Q10). |
| V24 | Drop `managed_brackets`, `protective_orders`, `trade_pnl_cycles`, `risk_trade_cycles`, and the per-fill PnL columns on `fills` (table rebuild). | Only after V22/V23 are verified in a released build; can be deferred one release. |

`SCHEMA_VERSION` moves to 24. The existing guard against newer schemas (`store.rs:388`) stays. Each version step gets a
migration test that builds a fixture database at the previous version (the pattern of
`migration_v18_preserves_v17_rows_and_adds_empty_broker_tables`, `store.rs:3501`).

### 5.3 Load at startup

`load_state` loads accounts, instruments, open orders and their events, active groups, positions, latest checkpoints,
ledger balances, risk state, templates, copiers, and the newest N fills and events for the UI window (N = 500). Terminal
history is not loaded. Then `core/recovery.rs` runs: projection verification from checkpoints, group invariants, DAY
expiry, interest publication.

## 6. Desktop integration

### 6.1 Change-driven stream

```rust
pub struct TradingSubscription { /* Arc<(Mutex<SubscriberState>, Condvar)> */ }
impl TradingSubscription {
    pub fn recv(&self, timeout: Duration) -> Option<TradingUpdate>;   // blocks a background task, never the UI thread
}
pub struct TradingUpdate {
    pub revision: u64,
    pub working_set: Arc<WorkingSet>,      // always the latest complete working set
    pub events: Vec<TradingEvent>,         // events since the last recv, oldest first
    pub lagged: bool,                      // events were dropped; working set is still exact
}
pub struct WorkingSet {
    pub accounts: Vec<AccountView>,        // account, AccountSummary { balance, equity, realized_today,
                                           // unrealized, fees_today, locked }, connected flag
    pub instruments: Vec<TradingInstrument>,
    pub open_orders: Vec<Order>,           // includes Inactive legs; bounded by MAXIMUM_OPEN_ORDERS
    pub groups: Vec<OrderGroup>,           // active and pending
    pub positions: Vec<PositionView>,      // with mark, unrealized, ticks, point value
    pub broker_positions: Vec<BrokerPosition>,
    pub risk: Vec<RiskView>,               // profiles, meters, locks, rule states, discipline
    pub feeds: Vec<InstrumentFeedView>,    // FeedState and session per traded instrument
    pub recent_fills: Vec<FillView>,       // newest 500 with accounting
    pub equity_recent: Vec<EquitySampleView>,  // in-memory ring for the selected account panel
}
```

- The owner publishes after every step that changed the revision. Mark-only changes (quotes moving unrealized PnL) are
  coalesced to at most one publication per 100 ms. Idle means no publication and no desktop wake (UI-08).
- Each subscriber holds the latest `Arc<WorkingSet>` and a bounded event queue (1,024). Overflow drops the oldest
  events and sets `lagged`; the desktop then reloads history pages. At most 8 subscribers.
- `TradingEvent` is typed: `OrderAccepted{order, warnings: Vec<RiskWarningKind>}`, `OrderRejected{client_order_id,
  reason: RejectReason}`, `OrderTriggered`, `OrderFilled{fill, accounting}`, `OrderCancelled{order, reason}`,
  `OrderExpired`, `GroupActivated`, `ProtectionResized`, `ProtectionNotPlaced`, `RiskWarning{kind, account}`,
  `RiskLocked{account, reason: RiskLockReason}`, `EmergencyCompleted{outcome}`, `FeedChanged{instrument, state}`,
  `RecoveryNotice{kind}`. Display text is made in the desktop from these values (UI-10). The `"RISK WARNING"` prefix
  (`lib.rs:4994`, `desktop.rs:2017`) is removed.

### 6.2 One workspace-level view model

- A single GPUI entity `TradingStore` (owned by `TerminalApp`) runs one background task that loops on
  `subscription.recv(1 s)` and applies each update through `cx.update` (the background task never mutates GPUI state
  directly). It holds the working set, derived indexes (orders by instrument, fills by order id, events by order id,
  computed once per revision), the selected account, and the latest page of history for the bottom panel.
- `TradingPnlState` (`desktop.rs:914`) loses its copies of accounts, orders, planned legs, positions, broker positions,
  risk and feedback; it keeps only per-pane UI state (`order_entry`, dialogs). `keep_trading_pnl_fresh`,
  `refresh_trading_pnl`, `schedule_trading_refresh` and the render-path poll (`workspace_layout.rs:699-800`) are deleted.
- Chart order lines: projected from `TradingStore` for the pane's instrument: open orders (Inactive legs drawn as today's
  planned legs), group roles from `groups` (replacing `TradingSnapshot::protective_role`,
  `lib.rs:362`). `chart_order_semantics` and `chart_executions` (`desktop.rs:1981`, `2130`) read the per-revision indexes
  instead of scanning events and fills per order.
- Bottom panel: working set for positions and orders; `history()` pages for fills, orders, round trips and ledger.
- Toaster: consumes `OrderFilled`, `OrderRejected`, `RiskLocked`, `ProtectionNotPlaced` events instead of diffing
  snapshots (`toaster.rs:696`).
- Command feedback: each command returns its result to the caller task, which posts it to the issuing pane (order ticket
  status line) or the toaster. The global `latest_feedback` slot (`trading.rs:103`) is deleted.
- Chart intents carry `order_id` and `expected_revision`; the runtime resolves them (TR-10). Bracket placement from a
  chart sends `OrderKind::RestingAt` and `AttachedProtection::Bracket`; `resting_entry_order_type` (`desktop.rs:2530`) is
  deleted.
- Flatten, reverse and flatten-all buttons send scopes; they no longer need an order-book frame
  (`trading.rs:762-860`).

## 7. Deterministic replay harness and live probe

### 7.1 Harness (`crates/trading_runtime/src/replay/`)

Compiled under `cfg(test)` and an opt-in feature `replay` (no new dependencies). It drives `TradingCore` directly,
with a `ManualClock` and a real SQLite file in a temporary directory, so persistence and restart are exercised.

```rust
let mut h = ReplayHarness::new(Fixture::futures_es().with_account("SIM-1", usd(50_000)).with_fees(cme_per_contract(225)));
h.at("2026-10-12T13:30:00Z").session("ES", Regular { ends: "2026-10-12T21:00:00Z" }).feed("ES", Live);
h.quote("ES", "5000.00", "5000.25");
let entry = h.submit(buy("ES", 1).resting_at("4999.00").bracket(stop_ticks(8), target_ticks(8))).expect_ok();
h.at("+1s").quote("ES", "4998.75", "4999.00");           // resting limit fills at 4999.00; legs activate
h.restart();                                             // drop core, reopen same file, run recovery
h.at("+2s").quote("ES", "4997.00", "4997.25");           // stop at 4997.00 triggers on bid
h.assert_position("SIM-1", "ES", flat());
h.assert_ledger("SIM-1", [deposit(50_000), realized(-100.00), commission(-2.25), commission(-2.25)]);
h.assert_transcript_matches("bracket_stop_after_restart.golden");
```

Capabilities:

- Scripted quotes, sessions, feed states and clock moves; quote streams loaded from small sanitized CSV fixtures
  (`t,instrument,bid,ask`) checked into `src/tests/replay/`.
- `restart()` closes the core and reopens it from the same file; `crash_during_commit(n)` uses a store test hook that
  fails the nth commit, then asserts memory equals the last committed state and that a restart recovers it.
- `transcript()` is a deterministic text rendering of events, fills, accounting, ledger, positions and groups, compared
  against golden files.
- Seeded property runs (a small in-crate xorshift generator, no new crate): random sequences of submits, cancels,
  modifies, quotes, partial manual closes, scale-ins, restarts and kill switches, asserting invariants after every step:
  coverage invariant, no orphan legs, ledger balance equals deposits + realized - fees, every position equals checkpoint
  plus replayed fills, no fill on a non-eligible quote, DAY orders never survive their session end.
- Required scenario list (closes the "missing tests" in `02_trading.md`): stop-limit gap-through, DAY expiry at session
  end and across restart, IOC/FOK at placement, marketable limit vs resting limit price, multi-instrument flatten and
  kill switch, reverse rejected by risk leaves the position unchanged, closed and unknown sessions, stale feed, reconnect
  generation change, retention then repair, 50,000-order history with a working-set build time bound, migration from a
  V19 fixture with awaiting, active and completed brackets.
- The broker path keeps its own tests on `venue/scripted.rs`; a parity test runs the same admission cases through both
  routes.

### 7.2 Headless live probe (Hyperliquid public data, no credentials)

Location: a new workspace member `tools/paper_trading_probe` (binary, not shipped). It is a composition root like the
desktop: it depends on `aeris_market_runtime`, `aeris_trading_runtime`, `aeris_trading` and `aeris_contracts`. Neither
runtime gains a dependency on the other, so naming_check is satisfied (its forbidden list names specific manifests;
the probe is added to the allowed composition roots in naming_check explicitly).

Flow:

1. Start `MarketService` and `TradingService` (temporary database path, system clock); wire the feed exactly as the
   desktop does.
2. Resolve a Hyperliquid perpetual (default `BTC`, argument overrides) through the existing catalog search and
   selection commands; wait for the `Terms` event in trading.
3. Create a practice account; set a fee schedule (Hyperliquid-like bps, or zero with `--no-fees`).
4. Place a market buy, a resting limit below the bid, a stop-limit above the ask, and a bracket; print every
   `TradingEvent` as it arrives.
5. After the first fill, shut down and restart `TradingService` on the same file; verify working orders, groups,
   positions and ledger are identical and that quotes resume without UI.
6. Flatten all; assert the account is flat, the ledger balance equals deposit + realized - fees, and print a summary.
   Exit non-zero on any invariant failure or if no quote arrives within 30 s.

It needs network access, so it is a manual and nightly check, not part of `cargo test`. It exercises the real market
path, as AGENTS.md "How to work" item 6 requires.

## 8. Migration plan

Each item is one coherent worker session that ends with the focused checks green
(`cargo check/test/clippy -p <crate> --locked`), and the workspace gates before commit. Behavior-changing items say so
and list the golden files they replace. "Closes" lists audit findings.

| # | Work item | Depends on | Content | Closes |
|---|---|---|---|---|
| 1 | Clock injection and command-time events | - | `clock.rs`; `TradingServiceConfig.clock`; replace `current_unix_nanos()`; cancel events stamped with command time. Behavior change: cancel event times in `simulated_stop_golden.txt` (Q1). | TR-12 (cancel time) |
| 2 | Owner shell and `TradingCore` | 1 | Move the owner loop and `Command` into `service/`; `TradingCore` exposes `handle/on_quotes/on_timer` (still with today's logic and `SimulatedMarketObservation`). No behavior change; goldens unchanged. Update naming_check contract strings. | - |
| 3 | Replay harness v1 | 2 | `replay/` driving `TradingCore` with ManualClock, temp SQLite, restart, transcript; port the two golden scenarios to it (same output). | test gap |
| 4 | Indexed order book | 2 | `book/` with client id, broker id and open-order indexes; remove linear scans in lib.rs and venue/owner.rs. No behavior change. | TR-05 (runtime lookups) |
| 5 | Module split | 4 | Move code into `admission/`, `execution/`, `ledger/`, `risk/`, `emergency.rs`, `venue/broker.rs`, `store/*`. Mechanical, no behavior change. | TR-12 (god object) |
| 6 | Feed contract | - | `aeris_trading::market` types and validation; `PriceGrid` and `InstrumentClass` in `aeris_instruments`. No behavior change. | groundwork |
| 7 | MarketEngine instrument leases | 6 | `instrument_lease.rs`, bounded; tests for share and release with consumers and data leases. | groundwork (MD ownership) |
| 8 | market_runtime trading feed | 7 | `trading_feed.rs`; interest command; demand and book reconciliation; Quote/Feed/Session/Terms emission; terms conversion moved from the desktop; Hyperliquid significant-figure `PriceGrid`; fixture-provider tests; per-provider Quotes-only demand verification. | UI-01 (market side) |
| 9 | Runtime-owned quotes | 3, 5, 8 | `market/` QuoteBook and inbox; interest publication; instruments registered from Terms; desktop wiring; delete `publish_simulated_market_observation`, desktop `register_provider_instrument` calls, `PendingObservations`. Fill rules unchanged (adapter from `TradingQuote`). naming_check: desktop must not reference `SimulatedMarketObservation`. | TR-01, UI-01, MD-11 (trading part) |
| 10 | Provider-neutral gating | 9 | Session and freshness rules (section 3.3); `eligible_after` replaces `restored_orders` and provenance comparison; delete `PracticeMarketSession` and `require_practice_market_session` (trading and market sides). Behavior change for the previously ungated providers (now gated). | TR-02, MD-02 (trading part) |
| 11 | Fill accounting consolidation | 5 | V20; one `apply_fill`; delete the four trackers; round trips from `fill_accounting`. Same math; goldens unchanged except row layout. | TR-11 (part) |
| 12 | Facts-only persistence | 11 | `StepTx` per step with commit-then-apply; unrealized in memory only; revision in the step transaction; retention moved to a timer. Benchmark quotes per second with an open position before and after. | TR-06 (writes) |
| 13 | Lifecycle state machine and typed events | 12 | V21; `book/lifecycle.rs`; `Inactive`, `Expired`, `Triggered`; `Order.revision` and timestamps; `OrderEventReason` and `RejectReason`; broker route uses the transition functions; broker scripted tests unchanged. | UI-10 (risk warning), TR-08 (part) |
| 14 | One admission validator | 13 | `admission/` with `VenueCapabilities` and price grid; broker path uses it; parity tests; `RestingAt` resolution. Behavior change: off-grid prices and wrong-side stops rejected. | TR-07 (validator), TR-08 (tick) |
| 15 | Execution engine v2 | 10, 14 | Placement-time evaluation (Market, IOC, FOK, marketable limit), resting limit at limit price, stop-limit trigger, DAY expiry timers and recovery, trigger index, market-order quote wait. New goldens (Q1, Q7). | TR-08 |
| 16 | Order groups and migration | 15 | V22 with Rust data migration; `groups/` (bracket, OCO, protection, coverage, management); bracket legs as Inactive orders; restart restoration via rows and recovery checks. Old types kept as views for the desktop until item 21. | TR-07 (model) |
| 17 | Remove protection layers | 16 | Delete `protection.rs`, `planned_leg.rs`, `ManagedBracket`, `ProtectiveOrder`, interrupted-bracket repair, `align_loaded_protection`; desktop sends `RestingAt` (delete `resting_entry_order_type`). | TR-07 (cleanup) |
| 18 | Cash ledger, fees, checkpoints | 12, 16 | V23; ledger entries, fee schedules, exact cost basis, equity samples, position and ledger checkpoints, bounded retention runs, repair from checkpoints. Behavior change: fees (default per Q5). | TR-06 (repair), TR-11 |
| 19 | Emergency operations | 9, 16, 18 | Kill switch, flatten scopes, economic-event flatten without observation, atomic reverse, unpriced reporting; multi-instrument tests. | TR-03, TR-04, TR-12 (reverse) |
| 20 | Risk session rollover | 18 | `risk_sessions`; rollover at the account boundary (Q8); bracket legs count as attached stops for the warning. Profile registration UI is a separate desktop item. | TR-09 (runtime part) |
| 21 | Publication API | 13, 18 | `subscribe`, `WorkingSet`, `TradingEvent`, history reader thread and paged queries; `snapshot()` kept as a compatibility view. 50,000-order load test. | TR-05 (runtime) |
| 22 | Desktop store and stream | 21 | `TradingStore` entity; delete the 250 ms poll and per-pane copies; chart lines and bottom panel from the store; toaster from events; per-command feedback. Manual check: idle workspace produces no trading notifies. | UI-08, TR-10, TR-05 (desktop) |
| 23 | Desktop intents and cleanup | 22 | Intents carry order id and expected revision; flatten/reverse/kill buttons send scopes; remove `TradingSnapshot`, old command forms and `MAXIMUM_SNAPSHOT_ITEMS` usage; naming_check updated. V24 when approved. | TR-10, UI-10 |
| 24 | Live probe | 9, 15, 19 | `tools/paper_trading_probe` per section 7.2; run against Hyperliquid and record the output in the commit message. | end-to-end validation |

Items 6 to 8 can run in parallel with items 1 to 5. Items 11 and 13 can start once item 5 lands. The desktop keeps
working after every item because old command forms delegate to new ones until item 23.

## 9. Risks and open questions

### Open questions needing a user decision

- **Q1. Golden ledger tests.** `characterization.rs` forbids updating the frozen ledgers for a refactor. Items 1, 13,
  15, 16 and 18 change those transcripts on purpose (cancel time, typed reasons, resting-limit price, bracket legs created
  as Inactive orders at placement, which shifts sequence numbers, fees). Proposal: keep the old goldens green through
  items 2 to 12 (refactors), then replace them with v2 goldens in the replay harness, each change listed in the commit.
  Needs approval.
- **Q2. DAY boundaries.** Calendared instruments use the provider session end. For continuous markets (Hyperliquid,
  `AlwaysOpen`) proposal: DAY ends at 00:00 UTC. For unknown calendars proposal: reject DAY with a clear reason and
  suggest GTC. Alternatives: treat unknown as 17:00 America/Chicago for futures, or as 00:00 UTC everywhere.
- **Q3. Orders while closed.** Proposal: accept resting orders (eligible at the open, DAY applies to the next session),
  reject marketable orders. Alternative: reject everything while closed.
- **Q4. Quote freshness bounds.** Feed liveness from `market_runtime` is the primary signal. Proposed secondary age
  bound for marketable placement and flatten pricing: 10 s crypto, 60 s futures and equities, configurable per class.
  An illiquid contract can legitimately have an unchanged BBO for longer.
- **Q5. Default fee schedules.** Proposal: zero fees by default with selectable presets (CME-style per contract per
  side, US equities zero commission, Hyperliquid-style maker/taker bps). Alternative: realistic presets on by default.
  Preset values must be checked against current published schedules before shipping; none are verified here.
- **Q6. Existing persisted brackets.** Proposal: migrate (awaiting brackets become pending groups with Inactive legs at
  the planned prices; active ones keep their orders; unpriceable ones are cancelled with a notice shown at first start).
  Alternative: cancel all pending brackets on upgrade and notify.
- **Q7. Resting limit fill price.** Proposal: fill at the limit price (passive) instead of today's touch price, which
  is optimistic on gaps. This lowers paper PnL on gapped limit fills compared with today.
- **Q8. Risk session boundary per account.** Proposal: per account setting, default 17:00 America/Chicago for accounts
  that trade futures and 00:00 UTC otherwise.
- **Q9. Scale-in growth of targets.** Today a manual scale-in grows the first-triggering stop and target
  (`protection.rs:206-210`). Proposal: keep growth for stops, not for targets (a target sized for 1 contract should not
  silently close 3). Needs a decision because it changes current behavior.
- **Q10. Instruments without a stored route after migration.** Old instrument rows lack `entitlement_id` and venue.
  Until the instrument is selected again (or market_runtime can resolve it from provider id and symbol), its working
  orders show "feed unavailable" and cannot fill. Proposal: show this per order in the UI; attempt automatic resolution
  for providers that can resolve by symbol without login (Hyperliquid public catalog).
- **Q11. Bracket leg activation price.** Today legs activate at offsets from the actual entry fill price, even if the
  user dragged them to an absolute price while pending. Proposal: keep offsets (consistent with today). Alternative:
  keep absolute prices once dragged.
- **Q12. Hyperliquid price grid.** Until the adapter reports the significant-figure rule, the probe and the app either
  skip tick validation for Hyperliquid (`PriceGrid::Unknown`) or reject. Proposal: implement the rule in item 8 from
  Hyperliquid documentation, verified against observed responses.

### Risks

- **R1. Scope of the bracket migration (item 16).** The Rust data migration touches live user data. Mitigation: the
  pre-migration `VACUUM INTO` backup, V19 fixture tests covering every bracket status, and the recovery verifier.
- **R2. Behavior changes visible to users** (resting limit price, gating for previously ungated providers, DAY expiry,
  wrong-side stop rejection, fees). These need release notes and in-app notices.
- **R3. Provider Quotes-only demand.** Each provider worker must subscribe top of book for an instrument with no series
  demand. Price alerts prove trade-only demand works; top-of-book needs verification per provider (Rithmic TOB plant
  limits, Hyperliquid `bbo` or `l2Book`). Rithmic
  subscription counts must stay within provider limits (AGENTS.md).
- **R4. Restart routes for providers that need login.** Rithmic and other login-gated instruments cannot be subscribed until
  the user logs in; their working orders stay unfillable and visibly so. This is correct but must be explained in the UI.
- **R5. Exact cost basis** changes realized PnL by sub-tick amounts compared with the rounded average in some scale-in
  sequences. Migration preserves open positions exactly; only future fills use the exact basis.
- **R6. naming_check updates.** Moving constants out of `lib.rs` and adding the probe require deliberate naming_check
  edits in the same commits; they must make the rules stricter (for example, desktop must not reference
  `SimulatedMarketObservation`), not weaker.
- **R7. Performance claims.** The design removes per-quote fsync and full polling, but no benefit is claimed until the
  benchmarks in items 12, 21 and 22 are measured.
- **R8. Broker route parity.** The broker route moves to shared admission and lifecycle code. The scripted venue tests in
  `venue/scripted.rs` and the broker scripted session tests must stay green unchanged at every step; any change to them
  is a red flag.
