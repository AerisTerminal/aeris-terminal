# 10 - Market data lifecycle rebuild (design)

Scope: `crates/market_runtime`, `crates/market_engine`, `crates/provider_history`,
`crates/domain/market_data`, the desktop market bridge (`apps/desktop/src/engine_market_worker*`,
`market_worker.rs`, `desktop.rs` series/selection glue) and the quote surface consumed by
`crates/trading_runtime`. This is a read-only design; no repository file was changed and no cargo
command was run. Every "current" reference below was read in the code during this audit unless marked
"to verify".

Fixes required by the brief: MD-01 (Day3/Day1 cadence writer/reader mismatch), MD-02 (stall watchdog
is tastytrade-only), MD-03 (session cadences approximated by fixed seconds), MD-04 (no calendar
outside CME group venues, two tastytrade classifiers), MD-12 (reconnect policy).

## 0. Audit corrections carried into this design

| Finding | Audit claim | Code reality | Design consequence |
|---|---|---|---|
| MD-12 | "No backoff; retry limit message is misleading" | cTrader: 3 s x 2^n capped at 48 s, `MAXIMUM_FAILURES = 5`, reset after 1 min healthy. tastytrade `recovery_delay`: same scheme. `ctrader_venue`: 5 s to 300 s. Rithmic adapter `RithmicRetryScheduler`: `RETRY_DELAYS` 250 ms to 8 s, reset on established. | Real gaps: no jitter anywhere; four independent policies; Hyperliquid reconnects every fixed 250 ms forever; Rithmic `serve_demand` uses `Duration::ZERO` for non-transport recovery on top of the adapter scheduler (two stacked policies). |
| MD-02 | "Watchdog only works for tastytrade" | Watchdog `recover_stalled_live_series` (`realtime.rs:3497`) iterates only `SeriesLive::TradeBuilt` and asks `broker_api.market_session_status` (tastytrade-only). Candle providers (cTrader, Hyperliquid) are never candidates; Rithmic always reads `Unknown`. | Broader than stated: stall detection must cover all live models, and the session answer must come from one provider-neutral calendar service. A neutral resolver already exists (`MarketService::market_session_status`, `runtime.rs:1146`) but branches on provider literals. |
| MD-03/04 | Confirmed | `TradeLiveCadence::Calendar{RithmicExchangeCalendar}` exists, but `Session{days}` maps to `Fixed{days*86400}` (`realtime.rs:456`); `RithmicExchangeCalendar::for_venue` is `None` outside CME/CBOT/COMEX/NYMEX; tastytrade has `market_collection` (CME names) and `tastytrade_market_collection` (XCME names). | One calendar service, venue-keyed, provider-independent. |
| MD-01 | Confirmed | Writer `workspace_series` (`desktop.rs:5053`) emits Day3 as `FixedSeconds/259200` via `ChartInterval::aggregation`; reader `pane_interval` (`engine_market_worker.rs:449`) accepts Day3 only as `SessionDays/3`; `series_key` (`replay_conversion.rs:354`) maps Day3 to `session(3)` and Day1 to session or time depending on `daily_bars`. | One mapping owned by the runtime, published with the instrument descriptor. |
| MD-05 | Confirmed | `StoredSeries::snapshot` clones all bars whenever a realtime tail exists; there is no tail accessor. The CI benchmark (`market_engine/examples/market_data_performance.rs`) does not exercise snapshot-per-tick. | Tail accessor plus extended benchmark. |
| MD-15 | Confirmed | `MarketStudyOutputSnapshot`, `StudyRemoved`, `StudyOutputsInvalidated`, `PriceAlertTriggered` carry no demand generation. | Typed `Fenced<T>` envelope on every consumer event. |

## 1. Module layout

### 1.1 Target tree (`crates/market_runtime/src/market_service/`)

```
market_service/
  mod.rs               MarketService facade + public command/event types only
  coordinator.rs       loop, command dispatch, effect execution; no per-series fields
  clock.rs             Clock trait (monotonic + wall), SystemClock; ScriptedClock behind cfg(test)/feature "replay"
  fence.rs             Epoch<Tag>, Fenced<T>, epoch tags (ProviderEpoch, DemandEpoch, CommandEpoch, CatalogEpoch)
  lifecycle/
    mod.rs             SeriesLifecycles: BTreeMap<BarSeriesKey, SeriesLifecycle>   (replaces SeriesLiveMap)
    state.rs           SeriesState + pure step(input, ctx) -> Effects
    live_model.rs      LiveModel { TradeHandoff{continuity}, CandleOverlay }
    forming_bar.rs     FormingBar: the single forming-candle owner before engine install
    watchdog.rs        provider-neutral stall evaluation (uses calendar)
  history/
    tracker.rs         HistoryTracker (replaces the 8 coordinator history maps + retry tuple)
    worker.rs          today's history.rs worker (busy-loop removed)
  calendar/
    mod.rs             MarketCalendarService: resolve(instrument) -> CalendarSource; phase(), session_bounds()
    cme.rs             CME-group rules moved from adapters/rithmic_protocol/src/calendar.rs
    weekly_hours.rs    today's session_hours.rs (cTrader contract hours), provider-neutral input
  provider/
    mod.rs             ProviderSession trait, ProviderCapabilities, ProviderRegistry
    tastytrade.rs | ctrader.rs | rithmic.rs | hyperliquid.rs   impls wrapping existing realtime modules
  reconnect.rs         ReconnectPolicy + Backoff (jittered, capped, budgeted)
  quotes.rs            QuoteBook (top of book per instrument) + QuoteSubscription fan-out
  outbox.rs            ConsumerEvents (moved out of mod.rs) + waitable OutboxWake
  provider_event.rs    KEEP (already the normalization point for four wire enums)
  publication.rs       KEEP (series publication); consumes lifecycle effects
```

Domain additions (`crates/domain/market_data`, which currently has no external dependencies; keep it
that way): `SessionPhase { PreOpen, Open, Break, Closed, AlwaysOpen, Unknown }`, `SessionWindow
{ open_utc_ms, close_utc_ms }`, `CalendarSourceKind`, `QuoteSnapshot` (fixed-point bid/ask/last with
explicit scale, `exchange_time`, `received_at`). Time-zone rules and `chrono-tz` stay in
`market_runtime` (it already depends on `chrono` and `chrono-tz`).

Engine additions (`crates/market_engine`): `series_tail(key, n)`, `series_contains_time(key, ts)`,
`MarketDemandKey::{Series(BarSeriesKey), Instrument(InstrumentKey)}` as the `DataLeaseRegistry`
key, and `GenerationId`/`ProviderGeneration` reused as the payload of `Epoch<Tag>`.

### 1.2 Keep / rename / delete

| Current | Fate |
|---|---|
| `SeriesLive { TradeBuilt(TradeLiveHandoff), ProviderCandles(CandleLiveHandoff) }`, `SeriesLiveMap` with duplicated `trade_*`/`candle_*` accessors (`mod.rs:857-986`) | Replaced by `SeriesLifecycle { state, model: LiveModel, forming: FormingBar }`; the duplicated accessors are deleted. |
| `LiveHistoryState { AwaitingHistory, Ready, Reseeding }` | Folded into `SeriesState::{Seeding, Live, Recovering}`. |
| `TradeLiveHandoff` (~25 fields, direct `Instant::now()`) | Split: continuity and buffered trades go to `LiveModel::TradeHandoff`; forming bar to `FormingBar`; timers become `Clock` reads in the step context. |
| `TradeLiveCadence { Fixed, Tick, Calendar{RithmicExchangeCalendar} }` | Renamed `LiveCadence { Fixed(secs), Trades(n), Session{days, calendar: CalendarHandle}, CalendarWeeks, CalendarMonths }`; `Session` no longer degrades to `Fixed`. |
| `realtime.rs` candle handlers (`candle_provider_connecting`, `_online`, `_trade`, `candle_series_recovering`, `candle_provider_recovering`, 2741-2991) and Rithmic handlers (`rithmic_connecting`, `_online`, `_trade`, `rithmic_failed`, 2993-3452), `publish_rithmic_live` (3454), `publish_candle_live` (3695) | Deleted; replaced by one `lifecycle::step` per normalized `ProviderEvent` plus one `publish_live` effect. |
| `recover_stalled_live_series` (3497) | Replaced by `lifecycle/watchdog.rs`, all live models, calendar-gated. |
| `stop_realtime_if_idle` (3863, branches on `LiveModel`) | `ProviderSession::release_idle()` via the registry. |
| `ProviderDescriptor` (~25 fields, `mod.rs:826-855`) | Split into `ProviderCapabilities` (data) + `ProviderSession` (behavior); presentation fields move to `ProviderPresentation`. Bogus Rithmic values (`selection_entitlement_id "crypto_public_realtime"`, `" spot"`) are removed. |
| `ProviderRealtimeChannelSet`, `ProviderRealtimeDispatch`, catalog equivalents (four-arm enums) | `ProviderRegistry` holding `Box<dyn ProviderSession>` keyed by `ProviderId`. |
| `ProviderSessionSlot` (9 flag fields) | `ProviderConnection { state: ConnectionState, epoch: Epoch<ProviderTag>, backoff: Backoff }`. |
| Coordinator `history_inflight`, `history_deferred`, `history_confirmed_empty`, `history_backwards_exhausted`, `history_current_empty`, `detached_history`, `history_cancellations`, `history_retries` (`coordinator.rs:316-335`) | One `HistoryTracker` field. |
| `catalog_searches`, `startup_catalog_searches`, `catalog_selections`, `startup_catalog_selections`, `catalog_screens` (raw `u64`) | `CommandFences: BTreeMap<(ConsumerId, ProviderId, CommandKind), Epoch<CommandTag>>`; the `startup_*` twins and the desktop `STARTUP_CATALOG_COMMAND_GENERATION = u32::MAX` sentinel disappear (startup becomes `CommandKind::StartupResolve`). |
| `broker_api.market_session_status` (watchdog), `require_practice_market_session` (non-tastytrade returns `Ok(false)`) | Both call `MarketCalendarService::phase`. |
| `MarketService::market_session_status` (`runtime.rs:1146`, provider literals) | Becomes a thin wrapper over `MarketCalendarService`. |
| `ChartInterval::rithmic_aggregation` in the domain crate | Moves to the Rithmic provider module (provider vocabulary must not live in the domain crate). |
| `provider_history` `HistoryScheduler`, `CoverageSnapshot`, `HandoffCoordinator` | Open question Q1 (adopt inside `HistoryTracker` or delete the unused parts). |
| `MarketGeneration<T>` (`application/generation.rs`) with no `require_resnapshot` callers | Open question Q4. |

## 2. Per-series state machine and history tracker

### 2.1 Ownership

`SeriesLifecycle` is owned by the coordinator thread, keyed by `BarSeriesKey`, and exists exactly
while `MarketEngine` holds demand for the key (engine remains the single demand owner; the lifecycle
is created on `DemandAdded` and removed on `DemandRemoved`). Canonical bars stay in
`MarketEngine::SeriesStore`; the lifecycle holds only the forming bar, buffered live input that
arrived before the seed, and control state.

### 2.2 States

```rust
pub(super) enum SeriesState {
    /// Demand accepted; waiting for a provider connection in the current epoch.
    Requested { provider_epoch: Option<Epoch<ProviderTag>> },
    /// Live subscription requested; current-history seed in flight. Live input is buffered.
    Seeding { ticket: HistoryTicket, buffered: LiveBuffer },
    /// Seed merged; live input updates the single forming bar and publishes.
    Live { last_progress: MonotonicInstant },
    /// Market closed per calendar; no stall evaluation, subscription kept.
    Dormant { phase: SessionPhase, until: Option<UtcMillis> },
    /// Continuity lost (gap, overflow, reconnect, detached window). Reseed in flight.
    Recovering { reason: RecoveryReason, ticket: HistoryTicket, buffered: LiveBuffer },
    /// Provider retries exhausted or entitlement/symbol error; terminal until demand changes
    /// or the provider epoch advances.
    Failed { error: SeriesFailure },
}

pub(super) enum RecoveryReason {
    ProviderReconnected, SequenceGap, QueueOverflow, StalledWhileOpen, DetachedWindow, SessionRolled,
}
```

`Stalled` is not a resting state: the watchdog emits `Input::Stalled`, which moves `Live` to
`Recovering{StalledWhileOpen}`. "Retired" is not a state either: removal from the map plus the
fence rule (section 3) guarantees retired work cannot mutate anything.

### 2.3 Inputs and effects (pure transition)

```rust
pub(super) enum SeriesInput<'a> {
    ProviderConnecting(Epoch<ProviderTag>),
    ProviderOnline(Epoch<ProviderTag>),
    ProviderOffline { epoch: Epoch<ProviderTag>, retry_at: Option<MonotonicInstant> },
    ProviderFailed { epoch: Epoch<ProviderTag>, error: &'a ProviderError },
    HistoryCompleted { ticket: HistoryTicket, result: HistoryResult<'a> },
    Trade(&'a NormalizedTrade),          // from ProviderEventKind
    Candle(&'a NormalizedCandle),
    SequenceGap { expected: u64, observed: u64 },
    QueueOverflow,
    Tick,                                 // coordinator tick; reads ctx.clock
    PhaseChanged(SessionPhase),
}

pub(super) enum SeriesEffect {
    SubscribeLive(BarSeriesKey), UnsubscribeLive(BarSeriesKey),
    RequestHistory(HistoryRequest), CancelHistory(HistoryTicket),
    InstallTail(FormingBarSnapshot), PublishDelta(BarSeriesKey), PublishCoveringSnapshot(BarSeriesKey),
    ReportStatus(SeriesStatus),
}

impl SeriesLifecycle {
    pub(super) fn step(&mut self, input: SeriesInput<'_>, ctx: &StepContext<'_>, out: &mut EffectBuf);
}
```

`StepContext` carries `&dyn Clock`, `&MarketCalendarService`, `&ProviderCapabilities`, and the
current epochs. `step` performs no I/O, no `Instant::now()`, and no engine mutation; the coordinator
executes effects. This makes every transition unit-testable and replayable (section 10).
`EffectBuf` is a fixed-capacity `ArrayVec`-style buffer reused across steps (no per-event allocation).

### 2.4 Transition table (abridged)

| From | Input | To | Effects |
|---|---|---|---|
| Requested | ProviderOnline(e) | Seeding | SubscribeLive, RequestHistory(current lane) |
| Seeding | Trade/Candle | Seeding | append to bounded `LiveBuffer`; on buffer full -> drop buffer, mark `needs_reseed` |
| Seeding | HistoryCompleted(ok, matching ticket) | Live or Dormant (by calendar phase) | model.merge_seed(history, buffer); InstallTail; PublishCoveringSnapshot |
| Seeding | HistoryCompleted(err) | Seeding | tracker schedules retry with backoff; Failed after budget |
| Live | Trade/Candle | Live | model.apply -> forming update; on bar close: engine append completed bar; PublishDelta |
| Live | SequenceGap / QueueOverflow | Recovering | RequestHistory(current lane) |
| Live | Tick with no progress > model threshold AND phase == Open | Recovering{StalledWhileOpen} | RequestHistory; ReportStatus(stalled) |
| Live | PhaseChanged(Closed/Break) | Dormant | ReportStatus |
| Dormant | PhaseChanged(Open/PreOpen) or Trade | Live (or Recovering{SessionRolled} if the session boundary passed) | RequestHistory if rolled |
| any | ProviderOffline(e == current) | Requested | CancelHistory; ReportStatus(reconnecting, retry_at) |
| any | ProviderOnline(e > held) | Seeding | SubscribeLive; RequestHistory |
| any | ProviderFailed(budget exhausted) | Failed | ReportStatus(failed) |
| any | input with stale epoch or ticket | unchanged | none (counted in a diagnostic counter) |

### 2.5 Live models (one strategy, three behaviors)

```rust
pub(super) enum LiveModel {
    /// tastytrade (Indexed continuity, FromTimeOnly history) and Rithmic (Sequence continuity).
    TradeHandoff { continuity: TradeContinuity, cadence: LiveCadence },
    /// cTrader and Hyperliquid provider-built candles overlaid on history.
    CandleOverlay { cadence: LiveCadence },
}
pub(super) enum TradeContinuity { Sequence { last: Option<u64> }, Indexed { last_index: Option<TradeIndex> } }
```

Continuity evidence stays separate from provider identity and from local ingestion order (AGENTS.md):
`Sequence` is used only where the provider documents sequence semantics; `Indexed` uses provider
trade indices only as documented; neither infers order from opaque ids. Which model a provider uses
is declared in `ProviderCapabilities::live_model` (section 5), not branched on provider names.

`FormingBar` is the single forming-candle owner. Bar boundaries come from `LiveCadence`:
`Session{days}` asks the calendar for the current session window and closes the bar at session
close/roll, which fixes MD-03 for every provider whose instrument resolves to a calendar.

### 2.6 HistoryTracker

```rust
pub(super) struct HistoryTracker {
    entries: BTreeMap<(BarSeriesKey, Epoch<ProviderTag>), SeriesHistory>,
    budget: HistoryBudget,                 // global in-flight cap, shared with the worker queue
}
struct SeriesHistory {
    current: Lane,                         // covering request toward now; seeds and reseeds
    backfill: Lane,                        // older-edge paging driven by viewport demand
    coverage: Coverage,                    // confirmed-empty suffix, backwards-exhausted, current-empty
    detached: bool,                        // canonical window intentionally away from the live tail
}
struct Lane { state: LaneState, cancel: Arc<AtomicBool>, backoff: Backoff }
enum LaneState { Idle, Deferred(HistoryRange), InFlight(HistoryTicket), RetryAt(MonotonicInstant, HistoryRange) }
pub(super) struct HistoryTicket { series: BarSeriesKey, provider: Epoch<ProviderTag>, demand: Epoch<DemandTag>, lane: LaneKind, seq: u32 }
```

Mapping from today: `history_inflight` -> `Lane::InFlight`; `history_deferred` -> `Deferred`;
`history_confirmed_empty`, `history_backwards_exhausted`, `history_current_empty` -> `Coverage`;
`detached_history` -> `detached`; `history_cancellations` -> `Lane::cancel`; `history_retries`
`(Instant, u8, Option<HistoryRange>)` -> `RetryAt` + `Backoff`. The `attempts = 0` keep-alive trick in
`schedule_history_retry` disappears because `Deferred` is an explicit state. Pruning happens in one
place: `HistoryTracker::retain_demanded(&engine_demand)` called after demand changes and provider
epoch changes.

Worker fixes (`history.rs:470-482`): replace `sleep(1ms)` polling on a full completion channel with
a blocking bounded `send` (the coordinator drains every tick, so back-pressure is bounded by the
channel capacity) and keep the existing policy of serving current requests first with every 4th slot
reserved for the oldest backfill request, now expressed as lane priority.

## 3. End-to-end typed generation fencing

### 3.1 Types

```rust
pub struct Epoch<Tag> { value: NonZeroU64, _tag: PhantomData<Tag> }   // Copy, Ord, no arithmetic exposed
pub enum ProviderTag {}   // session/connection epoch; payload is today's ProviderGeneration
pub enum DemandTag {}     // per-series demand epoch; payload is today's engine GenerationId
pub enum CommandTag {}    // consumer command epoch (catalog search/select/screen, startup resolve)
pub enum ConsumerTag {}   // attach epoch for a consumer endpoint

pub struct Fenced<T> { pub provider: Epoch<ProviderTag>, pub demand: Option<Epoch<DemandTag>>, pub command: Option<Epoch<CommandTag>>, pub value: T }
```

`Epoch` lives in `market_engine` next to `GenerationId`/`ProviderGeneration` (which become type aliases
or are replaced). `ProviderEvent.generation: u64` becomes `Epoch<ProviderTag>`; adapters receive the
epoch when the registry starts a session and cannot fabricate one (constructor is `pub(crate)` in the
registry).

### 3.2 Rules

1. Every provider event, history completion, catalog callback and consumer event carries the epochs
   that produced it.
2. Acceptance is a single function per boundary: `fence.accepts(&incoming) -> bool`, comparing equal
   epochs (not `>=`). A stale value is dropped and counted; it is never merged.
3. Consumer events (`SeriesDelta`, snapshots, `MarketStudyOutputSnapshot`, `StudyRemoved`,
   `StudyOutputsInvalidated`, `PriceAlertTriggered`, catalog results) are emitted as `Fenced<T>`; the
   desktop and `trading_runtime` compare against the epoch they hold for the pane/subscription. This
   closes MD-15.
4. Retirement: removing a lifecycle, cancelling a lane, or advancing a provider epoch makes all prior
   tickets unacceptable by construction; there is no "retired" flag to forget.
5. `naming_check` gains assertions: no `generation: u64` fields in `market_runtime` public events, no
   `u32::MAX` command generation in desktop, and every consumer event variant wraps `Fenced`.

## 4. Provider-neutral calendar and session service

### 4.1 API

```rust
pub struct MarketCalendarService { sources: BTreeMap<InstrumentKey, CalendarSource>, rules: CalendarRules }
pub enum CalendarSource {
    ProviderCalendar(ProviderCalendar),      // provider-published sessions/holidays (tastytrade today)
    ContractHours(WeeklyHours),              // cTrader contract trading hours (session_hours.rs)
    ExchangeRule(ExchangeRuleId),            // static venue rules, e.g. CME Globex 17:00 CT roll with DST
    AlwaysOpen,                              // 24/7 venues (Hyperliquid)
    Unknown,
}
impl MarketCalendarService {
    pub fn phase(&self, instrument: &InstrumentKey, at: UtcMillis) -> SessionPhase;
    pub fn session_window(&self, instrument: &InstrumentKey, at: UtcMillis) -> Option<SessionWindow>;
    pub fn next_transition(&self, instrument: &InstrumentKey, at: UtcMillis) -> Option<(UtcMillis, SessionPhase)>;
}
```

### 4.2 Resolution order (per instrument, resolved at catalog install time)

1. Provider-published calendar, if `ProviderCapabilities::calendar` says the provider has one.
2. Provider contract hours (cTrader `weekly_hours_status` input).
3. Exchange rule keyed by normalized venue MIC/exchange code (one normalization table; replaces the
   two tastytrade classifiers `market_collection` and `tastytrade_market_collection`, and Rithmic
   `for_venue`). The CME rule is moved, not rewritten, from `adapters/rithmic_protocol/src/calendar.rs`
   with its tests.
4. `AlwaysOpen` when the provider declares a 24/7 venue.
5. `Unknown`. Consumers must treat `Unknown` explicitly: the watchdog falls back to a conservative
   long stall threshold; the practice gate refuses with a clear message instead of `Ok(false)`.

The resolved `CalendarSourceKind` is published in the instrument descriptor (section 9) so the UI can
show the provenance.

### 4.3 Consumers

- Watchdog (MD-02): `lifecycle/watchdog.rs` evaluates every `Live` series of every model; it only
  declares a stall when `phase == Open` and no progress for `capabilities.stall_threshold(cadence)`.
  `Dormant` series are skipped.
- Session cadence (MD-03): `LiveCadence::Session` uses `session_window`.
- Practice gate: `require_practice_market_session` becomes `calendar.phase(..) == Open` for all
  providers; the `PracticeMarketSession` closure installed at `desktop.rs:5487` keeps its signature,
  so `trading_runtime` is unaffected.
- Quote freshness for trading (section 8) reports `SessionPhase` alongside quote age.

Holidays: only provider calendars carry holidays today. Exchange rules without holiday data report
`Open` on a holiday; this is acceptable for stall gating (it causes at most one extra reseed) but not
for trading gates. Open question Q3.

## 5. Provider capability trait

```rust
pub struct ProviderCapabilities {
    pub id: ProviderId,
    pub live_model: LiveModelKind,             // TradeHandoff{Sequence|Indexed} | CandleOverlay
    pub history: HistoryCapabilities,          // supported cadences, max page, FromTimeOnly vs range, backwards paging
    pub calendar: CalendarCapability,          // Published | ContractHours | None
    pub always_open: bool,
    pub concurrency: ConcurrencyLimits,        // max sessions/logins, max symbols per subscription
    pub reconnect: ReconnectPolicy,
    pub depth: Option<DepthCapabilities>,
    pub quotes: QuoteCapability,               // top-of-book source: dedicated quote stream | derived from depth | last trade only
}

pub trait ProviderSession: Send {
    fn capabilities(&self) -> &ProviderCapabilities;
    fn connect(&mut self, epoch: Epoch<ProviderTag>, sink: ProviderEventSink) -> Result<(), ProviderError>;
    fn apply_demand(&mut self, demand: &ProviderDemandSet) -> Result<(), ProviderError>;  // diff-based
    fn request_history(&mut self, request: HistoryRequest) -> Result<(), ProviderError>;
    fn release_idle(&mut self);
    fn shutdown(&mut self);
}
```

- `ProviderDemandSet` is computed by the engine (single demand owner) and contains series, depth and
  instrument-quote demand; the provider subscribes the difference. Symbol/timeframe/viewport changes
  only change the set and never reconnect (invariant preserved).
- All events cross into the coordinator as the existing normalized `ProviderEvent`/`ProviderEventKind`
  (`provider_event.rs`), now carrying `Epoch<ProviderTag>`.
- The trait is dyn-dispatched once per provider command, not per tick; the hot path is the normalized
  event channel, which stays monomorphic.
- Presentation-only fields (labels, entitlement ids, search hints) move to `ProviderPresentation`; the
  Rithmic `"crypto_public_realtime"` entitlement and `" spot"` suffix are deleted and replaced with
  correct values or `None` (to verify against Rithmic entitlement naming once onboarded).
- `naming_check` keeps "no `provider_id == \"`" in neutral files and adds it for `lifecycle/`,
  `history/`, `calendar/mod.rs`, `quotes.rs`, `outbox.rs`.

## 6. Reconnect policy

```rust
pub struct ReconnectPolicy {
    pub initial: Duration, pub multiplier: u32, pub cap: Duration,
    pub jitter: Jitter,                         // FullJitter or EqualJitter, seeded RNG from Clock for replay
    pub budget: Option<NonZeroU32>,             // None = retry forever (still backed off)
    pub healthy_reset_after: Duration,
    pub classify: fn(&ProviderError) -> RetryClass,  // Transient | RateLimited{retry_after} | Fatal
}
pub struct Backoff { attempt: u32, next_at: Option<MonotonicInstant>, healthy_since: Option<MonotonicInstant> }
```

Per-provider defaults (preserve current behavior, add jitter, close gaps):

| Provider | Today | Proposed |
|---|---|---|
| cTrader | 3 s x 2^n, cap 48 s, 5 failures, reset after 1 min | same values, equal jitter, budget 5, "retry limit reached" message kept (accurate). |
| tastytrade | `recovery_delay`, same scheme | same values, equal jitter. |
| cTrader venue (trading) | 5 s to 300 s | unchanged ownership; reuse `Backoff` type only. |
| Rithmic | adapter `RithmicRetryScheduler` 250 ms to 8 s, reset on established, plus `serve_demand` `Duration::ZERO` non-transport recovery | one policy: adapter scheduler remains the only transport retry; `serve_demand` non-transport recovery goes through `Backoff` (min 1 s) so policies no longer stack. Login concurrency limit from `ConcurrencyLimits` is respected (Q2). |
| Hyperliquid | fixed 250 ms forever | 500 ms x 2^n, cap 30 s, full jitter, no budget (public feed), reset after 1 min healthy. |

`RateLimited{retry_after}` overrides the computed delay. Exhausting the budget moves the provider
connection to `Failed` and emits `ProviderFailed` to every series of that provider (section 2.4);
new demand or an explicit user retry starts a new epoch with a reset backoff.

## 7. Hot-path performance changes and benchmark plan

Changes:
1. `MarketEngine::series_tail(key, n)` and segmented views so live publication reads the forming bar
   plus the last completed bar instead of `StoredSeries::snapshot` cloning all bars when a tail exists
   (MD-05). Covering snapshots remain for resync/overflow only.
2. `EffectBuf` and `LiveBuffer` are fixed capacity and reused; `step` allocates nothing in steady
   state.
3. Coordinator tick: replace the unconditional per-tick sweep (`publish_rithmic_live`,
   `publish_candle_live`, `recover_stalled_live_series`, `recover_overflowed_series_queues`,
   per-descriptor `flush_demand`, `stop_realtime_if_idle`, `retry_history`) with a dirty set
   (series touched since last publish) and a timer wheel/`BTreeMap<MonotonicInstant, TimerKey>` for
   watchdog, retry and session-transition deadlines. `recv_timeout` waits until the next deadline
   (capped at the 16 ms publication cadence only while the dirty set is non-empty).
4. Desktop polling (section 9) stops sending request/reply commands every 2 ms through the coordinator
   command channel.
5. History worker blocking send (removes the 1 ms spin).

Benchmark plan (extend `crates/market_engine/examples/market_data_performance.rs`, which CI already
runs on Linux and Windows, and add a `market_runtime` example for the coordinator):

| Scenario | Metric | Gate |
|---|---|---|
| 1 series, 50k completed bars, 10k trades/s into forming bar, publish at 16 ms | ns per trade, bytes allocated per publish | publish bytes independent of history length (O(1) in bars) |
| 64 series across 4 providers, mixed models | coordinator CPU per second idle and loaded | idle CPU near zero (no per-tick full sweep) |
| Overflow: consumer outbox at capacity 1024 | time to covering snapshot, events dropped | bounded, explicit, no unbounded growth |
| Reseed under live flow | buffered trades, merge time | `LiveBuffer` cap respected |
| Desktop bridge wake latency | p50/p99 from coordinator publish to bridge receipt | measured, recorded in the benchmark output |

Allocation counting uses a counting global allocator in the example binary only (safe Rust; no
`unsafe_code`, which is forbidden workspace-wide; if a safe counting allocator is not possible, count
via `Vec` capacity deltas on the publish path instead). Numbers are recorded as baselines before
changes; no performance claim is made until measured.

## 8. Quote subscription API for `trading_runtime`

Needed by TR-01/TR-02 (quote demand, session phase, quote freshness). Today `DataLeaseRegistry` is
keyed by `BarSeriesKey`, so instrument-only demand is impossible; price alerts already create
instrument-level demand indirectly.

```rust
// market_engine
pub enum MarketDemandKey { Series(BarSeriesKey), Instrument(InstrumentKey) }

// market_runtime public API
pub struct QuoteSubscription { /* lease guard; drop releases demand */ }
impl MarketService {
    pub fn subscribe_quotes(&self, consumer: ConsumerId, instrument: InstrumentKey) -> Result<QuoteSubscription, MarketError>;
}
impl QuoteSubscription {
    pub fn latest(&self) -> Option<Fenced<QuoteState>>;          // non-blocking read of the shared QuoteBook slot
    pub fn wait_changed(&self, timeout: Duration) -> bool;       // for trading_runtime worker threads only
}
pub struct QuoteState { pub quote: QuoteSnapshot, pub age: Duration, pub phase: SessionPhase, pub source: QuoteSourceKind }
```

- `QuoteBook` is coordinator-owned; each instrument slot is a small `Arc<QuoteSlot>` (seqlock-free:
  `Mutex<QuoteState>` + `Condvar` with a version counter) shared read-only with subscribers. One slot
  per instrument regardless of subscriber count; bounded by instrument demand.
- Source per `ProviderCapabilities::quotes`: dedicated quote stream, derived from canonical L2
  top of book (`order_books`), or last trade only (flagged so trading can refuse marketable simulated
  fills on stale/indirect quotes).
- `age` is computed against `Clock` at read time; `phase` from the calendar. `trading_runtime` owns the
  policy (reject/hold when stale or closed); `market_runtime` owns only the data.
- Dropping the subscription releases the lease; the provider unsubscribes the instrument via the
  `ProviderDemandSet` diff. Provider sessions are shared; no per-consumer session.
- Coordination: the trading design (`02_trading.md`) must consume exactly this surface (Q5).

## 9. Desktop thin-bridge redesign

Problems today: `EVENT_WAIT = 2ms` polling loop; `poll_events` is a request/reply over the coordinator
command channel (polling loads the coordinator); `WorkerEndpoint` has five `Arc<Mutex<Option<..>>>`
slots; `MarketWorkerMessage` (`market_worker.rs:291`) plus a separate `ChartState` enum; desktop-side
cadence mapping (MD-01) and `replay_conversion` with hard-coded provider id formats and entitlements;
`STARTUP_CATALOG_COMMAND_GENERATION = u32::MAX`.

Design:
1. Waitable outbox: `outbox.rs` exposes `ConsumerOutbox::wait(timeout) -> Drained<Fenced<ConsumerEvent>>`
   backed by `Mutex` + `Condvar` on the consumer's `ConsumerEvents`. The coordinator pushes and
   notifies; the bridge thread blocks on the condvar. No command round-trip per poll. Capacity and the
   covering-snapshot overflow policy (1024) are unchanged.
2. One command per user intent: `select_and_demand { pane, instrument, interval, viewport, epoch }`
   replaces the sequence of catalog-select, series-key-build and demand commands. The runtime resolves
   the series key.
3. Runtime-published `InstrumentDescriptor { key, display, tick/price scale, offered intervals,
   calendar_source, quote_capability }` and `ResolvedSeries { interval: ChartInterval, key:
   BarSeriesKey, cadence: SeriesCadence }`. The desktop never builds `BarSeriesKey` or
   `SeriesCadence` itself.
4. Single interval mapping (MD-01): `market_runtime` owns `fn resolve_cadence(interval: ChartInterval,
   caps: &HistoryCapabilities, calendar: CalendarSourceKind) -> SeriesCadence`. Day1/Day3 map to
   `SessionDays(n)` when the instrument has a session calendar and to `FixedSeconds` only for
   `AlwaysOpen` venues; Week1 -> `CalendarWeeks(1)`, Month1 -> `CalendarMonths(1)`. Workspace
   persistence stores `ChartInterval` (the user intent), not the cadence; on load the runtime
   re-resolves. `pane_interval`, `workspace_series`, `series_key` in `replay_conversion` are deleted
   or reduced to calls into this mapping. A migration reader accepts both legacy encodings of Day3
   (`FixedSeconds/259200` and `SessionDays/3`) and maps them to `ChartInterval::Day3` once.
5. Bridge state: `WorkerEndpoint` keeps one `ConsumerOutbox` handle and one command sender. The five
   `Arc<Mutex<Option>>` slots disappear; their contents arrive as events.
6. `MarketWorkerMessage` collapses to the `Fenced<ConsumerEvent>` stream plus GPUI-side projection
   (`ChartState` derived in the UI from events; `EngineSwitchMarker` is replaced by the epoch change
   itself).
7. Startup resolution (`StartupResolution::Searching`, `handle_startup_catalog_event`, asserted by
   `naming_check`) is preserved as a name but driven by `CommandKind::StartupResolve` epochs; the
   `naming_check` assertion is updated in the same commit to require the epoch-fenced form.

## 10. Deterministic replay harness

Boundary: the coordinator's input boundary (`Command` + normalized `ProviderEvent` + history
completions + clock ticks) and its output boundary (consumer outboxes + provider effects).

```rust
#[cfg(any(test, feature = "replay"))]
pub struct ReplayHarness { clock: ScriptedClock, coordinator: Coordinator<'static>, providers: ScriptedRegistry }
pub enum ReplayStep {
    Advance(Duration), SetWall(UtcMillis),
    Command(Command),
    Provider(ProviderId, ProviderEventKind),        // epoch filled by the scripted registry
    HistoryReply { matching: TicketMatcher, result: FixtureHistory },
    ExpectEvent(EventMatcher), ExpectEffect(EffectMatcher), ExpectState(BarSeriesKey, StateMatcher),
}
```

- Fixtures: small sanitized RON/JSON files under `crates/market_runtime/tests/fixtures/replay/`,
  faithful to real shapes, no credentials or private payloads (AGENTS.md).
- `ScriptedClock` drives both monotonic and wall time, and seeds jitter, so backoff and watchdog are
  reproducible.
- The scripted registry implements `ProviderSession` and records demand diffs, so "symbol/timeframe
  change does not reconnect" becomes an assertion.
- Required scenarios (first wave): MD-01 Day3 round trip through persistence; MD-02 stall on cTrader
  candle series while open, no stall while closed; MD-03 Day1 bar closes at CME 17:00 CT roll across
  a DST change; MD-04 non-CME venue resolves to contract hours/AlwaysOpen/Unknown; MD-12 Hyperliquid
  backoff schedule and Rithmic single-policy; stale-epoch history completion ignored; overflow
  produces a covering snapshot; reseed during live flow preserves one forming bar and contiguous
  completed history.
- The desktop is outside the harness; its bridge is tested with a fake outbox.

## 11. Strangler migration plan

Each item is one commit-sized change that compiles, passes `cargo fmt`, clippy `-D warnings`, the
affected crate tests and `naming_check`, and leaves no dead path. "NC" lists `tools/naming_check`
changes required in the same commit.

| # | Item | Depends on | Closes | NC |
|---|---|---|---|---|
| 1 | Characterization tests for current lifecycle: reseed, overflow covering snapshot, stale history ignored, Day3 persistence round trip (failing test marked as the MD-01 reproduction is added in item 3, not ignored). | - | - | add required test names |
| 2 | Extend `market_data_performance.rs` with snapshot-per-publish and allocation scenarios; record baseline. | - | (MD-05 measurement) | - |
| 3 | MD-01 fix: runtime-owned `resolve_cadence`; persistence stores `ChartInterval`; legacy Day3 reader; delete desktop-side mapping in `workspace_series`/`pane_interval`/`series_key`. | 1 | MD-01 | assert desktop has no `SeriesCadence::` construction |
| 4 | `ReconnectPolicy` + `Backoff` with jitter; apply to Hyperliquid; route Rithmic `serve_demand` non-transport recovery through `Backoff`. | - | MD-12 | - |
| 5 | `clock.rs` (`Clock`, `SystemClock`, `ScriptedClock`); replace direct `Instant::now()` in `TradeLiveHandoff` and the watchdog. | - | (replay prerequisite) | assert no `Instant::now()` in `lifecycle/` (applies once item 9 lands) |
| 6 | `calendar/` module: move CME rules from `rithmic_protocol/src/calendar.rs`, move `session_hours.rs`, venue normalization table replacing both tastytrade classifiers and `for_venue`; `MarketService::market_session_status` delegates. | - | MD-04 | assert no provider literal in `calendar/mod.rs` |
| 7 | Watchdog uses calendar for all live models (trade and candle); practice gate uses calendar. | 5, 6 | MD-02 | - |
| 8 | `LiveCadence::Session` uses calendar session windows; delete `Session -> Fixed` mapping. | 6 | MD-03 | - |
| 9 | `fence.rs`: `Epoch<Tag>`, `Fenced<T>`; `ProviderEvent.generation` typed; catalog maps collapse into `CommandFences`; remove `u32::MAX` sentinel. | 1 | MD-15 (partially), UI startup sentinel | update `StartupResolution` assertions; forbid `generation: u64` in public events |
| 10 | `HistoryTracker` replaces the eight coordinator maps; worker blocking send. | 9 | history map sprawl, 1 ms spin | update `ProviderRuntimeRegistry`/coordinator field assertions |
| 11 | `lifecycle/` with `SeriesState` + pure `step`; port the candle path first (`candle_*` handlers deleted). | 5, 9, 10 | duplicate handlers (candle half) | replace `series_live: super::SeriesLiveMap::default()` assertion with `series: super::lifecycle::SeriesLifecycles::default()` |
| 12 | Port trade-built path (tastytrade Indexed, Rithmic Sequence); delete `rithmic_*` handlers, `SeriesLive`, `SeriesLiveMap`, `LiveHistoryState`. | 11 | duplicate handlers (trade half) | assert `SeriesLiveMap` absent |
| 13 | Dirty-set publication and deadline timers in the coordinator loop; `series_tail` engine accessor. | 12, 2 | MD-05, idle CPU | - |
| 14 | `ProviderCapabilities` + `ProviderSession` + `ProviderRegistry`; delete four-arm channel enums and descriptor flags; fix Rithmic presentation values. | 12 | provider coupling | update registry field assertions; extend provider-literal ban |
| 15 | `MarketDemandKey::Instrument`, `QuoteBook`, `QuoteSubscription`; price alerts migrate to instrument demand. | 14, 6 | TR-01/TR-02 data side | - |
| 16 | `Fenced<T>` on study/alert consumer events. | 9 | MD-15 | assert every consumer event variant is fenced |
| 17 | Waitable outbox; bridge blocks on condvar; remove `EVENT_WAIT` polling and `poll_events` round trip. | 9 | UI-02..UI-06 bridge findings (to map precisely) | - |
| 18 | `select_and_demand`, `InstrumentDescriptor`, `ResolvedSeries`; collapse `WorkerEndpoint` slots and `MarketWorkerMessage`; remove provider formats from `replay_conversion`. | 3, 17 | UI-14, provider neutrality | ban provider id formats in desktop |
| 19 | Replay harness + first-wave scenarios as permanent tests. | 5, 11, 12, 14 | regression net | add required replay test names |
| 20 | Decide and apply Q1 (`provider_history` adopt/trim) and Q4 (`MarketGeneration<T>`). | 10 | dead code | - |

Ordering rationale: bug fixes (3, 4, 6-8) land early on the current structure because they are small
and user-visible; structural moves (9-14) follow behind characterization tests; desktop changes (17,
18) wait until the runtime publishes the data they need.

## 12. Risks and open questions

Risks:
- R1. Lifecycle port regressions in reseed/merge edge cases (detached window, FromTimeOnly history,
  sequence gaps). Mitigation: items 1 and 19, candle path ported before trade path.
- R2. Calendar correctness: wrong session windows produce wrong daily bars, which is worse than
  today's fixed-second approximation. Mitigation: move the existing tested CME rules unchanged; DST
  fixtures; publish calendar provenance to the UI.
- R3. Persistence migration for Day3/Day1 must not lose saved workspaces; legacy reader required and
  tested before the writer changes.
- R4. Waitable outbox introduces a cross-thread condvar; the UI thread must never block on it (only
  the bridge worker thread waits).
- R5. Rithmic behavior cannot be qualified until onboarding (gated provider); changes to its retry path
  are designed now and verified with scripted sessions only.
- R6. `naming_check` updates in many commits; each must strengthen, never weaken, the assertion.

Open questions:
- Q1. `provider_history` `HistoryScheduler`/`CoverageSnapshot`/`HandoffCoordinator`: adopt inside
  `HistoryTracker` (the Rithmic adapter already uses the handoff types) or trim the unused parts?
- Q2. Rithmic login/session concurrency limits and whether history and live may share one login;
  needed for `ConcurrencyLimits` and the single-policy retry.
- Q3. Holiday source of truth for exchange-rule calendars (provider calendar only, bundled table, or
  official exchange source fetched directly per operating constraint 3), and whether trading gates
  must refuse on `Unknown`.
- Q4. Does `MarketGeneration<T>` survive? It is a versioned immutable bar collection, not a fence, and
  `require_resnapshot` has no callers; candidates: delete, or keep as the snapshot payload type.
- Q5. Coordination with the trading design: confirm `QuoteSubscription` (section 8) satisfies TR-01
  and TR-02 and that `trading_runtime` owns staleness policy.
- Q6. Hyperliquid Day1/Day3 semantics: confirm from current Hyperliquid documentation whether daily
  candles are UTC-day aligned (then `AlwaysOpen` + `FixedSeconds` alignment is correct) before item 3.
- Q7. Remove the fake-instrument CLI modes, or move them behind the replay harness?
- Q8. Whether `ChartInterval::rithmic_aggregation` callers outside Rithmic exist (to verify before
  moving it in item 14).
