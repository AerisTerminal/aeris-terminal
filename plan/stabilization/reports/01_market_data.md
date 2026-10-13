# Aeris Terminal market-data path audit (01)

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal` (branch `main`, 4 commits ahead of `origin/main`, 1189 commits).
Method: read-only. Files were read directly, `git log`/`git show` were used for history, and churn was counted over the last 300 commits. No build or test was run, so every "needs verification" item below still needs a test.

Scope: `crates/market_runtime/src/market_service/*` (coordinator, realtime, history, runtime, mod, tastytrade, publication), `crates/market_runtime/src/{rithmic_realtime,hyperliquid_realtime,study}.rs`, `crates/market_engine/src/series_store.rs`, `crates/adapters/rithmic_protocol/src/calendar.rs`, and the desktop bridge (`apps/desktop/src/market_worker.rs`, `apps/desktop/src/engine_market_worker.rs` plus its `publications.rs`, `runtime.rs`, `replay_conversion.rs`, and `selection_commands.rs`).

---

## Summary

The market-data path works for the common case but is in a "fix the symptom where it shows" phase. Three structural problems drive most of the recent churn:

1. **Two parallel live models in one coordinator.** Rithmic-style trade-built series and provider-candle series (tastytrade, cTrader, Hyperliquid) each have their own copy of the lifecycle (connecting, online, recovering, failed, series recovering, publish). Every handler branches on `LiveModel`. Fixes land in one branch and miss the other, and the "rithmic_*" names are now used for every provider.
2. **History state spread across eight parallel maps keyed by `(series, generation)`.** Retry, deferral, empty answers, exhaustion, detachment, and cancellation are tracked separately and pruned separately. Current-edge recovery and older backfill share one retry ticket, and recent fixes (`dfe6c287`) added guards instead of separating those two lanes.
3. **Provider and calendar knowledge leaks into generic layers and into the desktop.** The generic handoff depends on the Rithmic adapter's CME calendar. The coordinator calls tastytrade-only session status. The desktop holds four separate interval/cadence mapping tables and more than 100 provider string literals.

These produce concrete defects:
- A persisted 3-day chart pane cannot be restored (MD-01).
- The stalled-live watchdog never fires for trade-built providers (MD-02).
- Session-day bars are bucketed from the last bar instead of the exchange trading day (MD-03).
- Weekly and monthly live updates are silently missing outside CME venues (MD-04).
- Live ticks copy the whole canonical series several times on hot paths (MD-05).

None of the scalability or performance claims in the code are backed by measurements I could find.

---

## Churn hotspots (fix-type commits in the last 300 commits)

| File | Fix commits / 300 | Size (lines) | Notes |
|---|---|---|---|
| `crates/market_runtime/src/market_service/tastytrade.rs` | 20 | 2692 | Candle overlay, session status, and calendar heuristics |
| `crates/market_runtime/src/market_service/realtime.rs` | 13 | 5179 | Lifecycle duplication; handoff heuristics (`4f885153`) |
| `crates/market_runtime/src/market_service/tastytrade_tests.rs` | 12 | - | Tests track the tastytrade patches |
| `crates/market_runtime/src/market_service/coordinator.rs` | 11 | 5183 (about 3870 of them inline tests) | God struct with about 30 maps |
| `crates/market_runtime/src/market_service/mod.rs` | 10 | 1827 | Descriptor flags keep growing |
| `apps/desktop/src/engine_market_worker.rs` | 10 | 2874 | Desktop compensations (`771a8d73`, `f504ab86`, `6222102d`) |
| `apps/desktop/src/engine_market_worker/publications.rs` | 8 | 547 | Desktop re-derives load and live state |
| `crates/market_runtime/src/market_service/history.rs` | 6 | 2379 | History ledger (`dfe6c287`) |
| `crates/market_runtime/src/market_service/publication.rs` | 6 | - | |
| `crates/adapters/tastytrade/src/{lib,session}.rs` | 6 / 6 | - | |
| `apps/desktop/src/market_worker.rs` | 4 | 3311 | Mailbox coalescing |
| `crates/market_runtime/src/market_service/runtime.rs` | 4 | 2087 | Provider-literal session status |
| `provider_event.rs` | 4 | - | |

For context, desktop files outside the market path churn more: `desktop.rs` had 24 fix commits, `workspace_surface.rs` 21, and `tools/naming_check` 21.

---

## Findings

### MD-01: A persisted 3-day chart pane cannot be restored because the workspace writer and reader disagree
- **Severity:** High
- **Category:** Bug
- **Evidence:**
  - The writer is `apps/desktop/src/desktop.rs:5053-5070` (`workspace_series`). It encodes everything except Week1 and Month1 through `ChartInterval::aggregation()`. `crates/domain/market_data/src/chart_interval.rs:164` maps `Day3` to `fixed_seconds(259_200)`, so a Day3 pane is saved as `FixedSeconds/259_200` (`desktop.rs:5106`).
  - The reader is `apps/desktop/src/engine_market_worker.rs:449-492` (`pane_interval`). It has no `259_200` arm, so that value falls to `Err(unsupported())`. It accepts Day3 only as `SessionDays/3` (lines 482-487), which the writer never produces.
  - `restored_workspace_pane_specs` (`engine_market_worker.rs:412`) propagates the error with `?`, so the whole restore fails. `start_workspace_tabs` is the default startup path and also propagates with `?` (`desktop.rs:5408`). The readiness check `validate_workspace_boot_for_readiness` (`desktop.rs:3941-3945`) fails the same way.
  - Commit `6222102d` added the `SessionDays/3` arm and the test `restored_hyperliquid_day3_pane_keeps_its_native_interval` (`engine_market_worker.rs:1183-1198`). The test builds a `SeriesKey` by hand in a shape production never writes, so it passes while the real round trip is broken.
- **Root cause:** Interval/cadence identity is encoded independently in four places: `desktop.rs:5053` (`workspace_series`), `engine_market_worker.rs:449` (`pane_interval`), `engine_market_worker/replay_conversion.rs:354` (`series_key`), and `desktop/workspace_surface.rs:1223-1227` (cadence to `BarPeriod`). No test round-trips writer output through the reader.
- **Proposed clean fix:** Define one bidirectional mapping between `ChartInterval` and the persisted `SeriesKey` cadence. Either persist the resolved `BarSeriesKey` period, or add a single `ChartInterval::to_series_cadence`/`from_series_cadence` pair in `aeris_market_data`. Remove the other three tables, and add a property test that round-trips every `ChartInterval` for every provider presentation. Restore should skip or repair an unsupported pane with a visible notice rather than fail the whole workspace.
- **Verification:** Add a unit test: `pane_interval(Some(&workspace_series(ChartInterval::Day3, &hyperliquid_instrument)), "hyperliquid")` should return `Ok(Day3)`. It currently should return `Err`. Then confirm manually: open a Hyperliquid 3D chart, quit, and relaunch.

### MD-02: The stalled-live recovery watchdog only works for tastytrade
- **Severity:** High
- **Category:** Bug
- **Evidence:** `realtime.rs:3497-3556` (`recover_stalled_live_series`) decides whether a market is open by calling `self.broker_api.market_session_status(...)` and `super::tastytrade::now_nanos`. The broker API implementation (`tastytrade.rs:587-627`) returns `Unknown` for any non-tastytrade instrument, so Rithmic (TradeBuilt), Hyperliquid, and cTrader series never pass the "market open" gate. A provider-neutral resolver already exists at `runtime.rs:1146` (`MarketService::market_session_status`). The logic was introduced in `4f885153`.
- **Root cause:** A feature built for one provider was placed in the generic coordinator but wired to that provider's API.
- **Proposed clean fix:** Have the coordinator ask a provider-neutral session-calendar owner (see MD-09) whether a market is open, and apply the stall policy per descriptor. The coordinator should not call `broker_api` directly.
- **Verification:** Add a coordinator test with a Rithmic live series, an open CME session, and no trades past the stall threshold. The test should expect a recovery to be issued. It currently should not issue one.

### MD-03: Session-day live bars are bucketed from the last bar, not from the exchange trading day
- **Severity:** High (likely; needs fixture confirmation)
- **Category:** Bug
- **Evidence:** `realtime.rs:456` maps `BarPeriod::Session{days}` to `TradeLiveCadence::Fixed{days*86_400}`. Bucket boundaries are then the last canonical bar's timestamp plus k times the period (`realtime.rs:884-907` and `1145-1173`). The CME trading day starts at 17:00 CT, so evening-session trades fold into the previous day's bar, and DST transitions shift every boundary by an hour. A real calendar exists (`crates/adapters/rithmic_protocol/src/calendar.rs`, `cme_trading_day`) but is only used for Week/Month. Commit `3f528960` switched the desktop to request session days but did not change live aggregation.
- **Root cause:** A session period is modeled as a fixed-length time period on the live path.
- **Proposed clean fix:** Add a `TradeLiveCadence::Session` variant that buckets by calendar trading day, using the same calendar that history uses.
- **Verification:** Use a fixture with a CME history daily bar, then trades at 16:59 CT and 17:01 CT on a weekday, plus a DST-boundary week. Expect two distinct session bars with timestamps equal to the provider's daily bars.

### MD-04: Weekly and monthly live updates are silently missing for non-CME venues
- **Severity:** Medium
- **Category:** Bug / maturity gap
- **Evidence:** `TradeLiveHandoff::new` returns `None` when `RithmicExchangeCalendar::for_venue` does not match `CME|CBOT|COMEX|NYMEX` (`calendar.rs:29-33`; `realtime.rs:459-467`). `ensure_realtime` (`realtime.rs:1999-2004`) then creates no handoff and raises no error, so tastytrade equities and CFE 1W/1M charts never go live. The tape's `trading_day` (`realtime.rs:282-289`) is also `None` for those venues.
- **Root cause:** Calendar availability is treated as optional instead of as an explicit capability.
- **Proposed clean fix:** Make "has a calendar for this venue" a checked capability. Reject unsupported periods at demand time with a `MarketDemandError` (stage `Handoff`), or provide calendars per venue (MD-09).
- **Verification:** Add a coordinator test that subscribes to a tastytrade equity 1W series. Expect either a live handoff or an explicit demand error, never silence.

### MD-05: Hot paths copy the full canonical series on every live tick
- **Severity:** High (performance; unmeasured)
- **Category:** Scalability
- **Evidence:**
  - `crates/market_engine/src/series_store.rs:594-631` (`StoredSeries::snapshot`) returns a cheap `Arc` only when there is no tail. A live series always has a tail, so every `MarketEngine::series_snapshot` call (`market_engine/src/lib.rs:1014`) builds a new `Vec` of all bars, up to `HISTORY_SERIES_HIGH_WATERMARK` = 16,384 (`market_service/mod.rs:89`).
  - Callers on per-tick or per-event paths include:
    - `study.rs:3618` (the dependency snapshot) and `study.rs:3733`, `3747`, and `3769` (`primary_timeline_len`, `primary_lower_bound`, `primary_upper_bound`). These run through `execute_study_bar_change` (`coordinator.rs:972-990`), which `install_live_tails` calls for every bar (`realtime.rs:1734`).
    - `realtime.rs:1766`, which copies the series only to inspect the last bar.
  - Existence-only checks also pay the full copy: `history.rs:1003`, `1023`, `1050`, `1160`, and `1288`, and `publication.rs:856`. These only need `series_bar_count` or a tail accessor.
- **Root cause:** The engine API offers only a materialized snapshot, with no borrowed or tail view.
- **Proposed clean fix:** Add `MarketEngine::series_tail(&key) -> Option<MarketBar>`, `series_contains(&key)`, and a borrowed or segmented view (covering slice, completed tail, and forming tail) for studies to binary-search. Replace the existence checks with `series_bar_count(...).is_some()`.
- **Verification:** Write a Criterion or endurance benchmark: 16k-bar series, 5 studies, 1,000 ticks per second. Measure allocation bytes per tick before and after, with a target of O(1) per tick.

### MD-06: Duplicate lifecycle state machines per `LiveModel`
- **Severity:** High
- **Category:** Patch-on-patch
- **Evidence:**
  - Two near-identical sets of handlers exist. `rithmic_connecting`, `rithmic_online`, `rithmic_recovering`, `rithmic_failed`, `rithmic_series_recovering`, and `publish_rithmic_live` are at `realtime.rs:2741-2991`. Their `candle_provider_*` equivalents are at `realtime.rs:2993-3495`.
  - Every handler branches on `LiveModel`: `handle_provider_event` (`2048-2133`), `handle_provider_disconnected` (`2615-2676`), `handle_idle_stop_overlap` (`2678-2732`), `stop_realtime_if_idle` (`3863-3922`), and `drain_coordinator_events` (`coordinator.rs:215-244`).
  - `rithmic_failed` is the failure path for every provider (`coordinator.rs:236`).
  - `history.rs` duplicates the same split: `start_rithmic_realtime_from_snapshot` / `start_candle_realtime_from_snapshot`, the matching `seed_*_history` and `finish_*` functions (`1668-1830`), and the coverage and reseed helpers (`831-858`), all branching on `provider_uses_candles`.
- **Root cause:** Candle providers were added by cloning the Rithmic path instead of extracting a shared series lifecycle.
- **Proposed clean fix:** Create one `SeriesLiveState` machine (`Resolving → Seeding → Live → Recovering{reason} → Failed`). The live-model difference becomes a strategy object that converts provider events into tail installs. Rename the provider-neutral handlers.
- **Verification:** Run one table-driven lifecycle test suite against both live models. Line count for the lifecycle code should drop sharply.

### MD-07: The history ledger is spread across eight parallel maps
- **Severity:** High
- **Category:** Patch-on-patch / maintainability
- **Evidence:**
  - The maps are `history_inflight`, `history_deferred`, `history_confirmed_empty`, `history_backwards_exhausted`, `history_current_empty`, `detached_history`, `history_cancellations`, and `history_retries` (`coordinator.rs:316-335`).
  - They are pruned separately in `prune_history_tracking` (`history.rs:1832-1901`), which does not prune `history_cancellations`.
  - `history_retries` stores a tuple that uses `attempts = 0` as a magic keep-alive cooldown value (`history.rs:618-663`).
  - `retry_history` handles one key per tick (`history.rs:665-701`).
  - Current-edge recovery and older backfill share one retry slot per `(series, generation)`. `dfe6c287` patched this with `range.is_none()` guards and `MarketDemandErrorScope` instead of separating the lanes.
  - The history worker busy-loops with `sleep(1 ms)` when its completion channel is full (`history.rs:470-482`).
  - Tests assert on the private tuple layout (`coordinator.rs:3547`, `3589`), which locks the structure in place.
- **Proposed clean fix:** Introduce a `HistoryLedger` per series and generation, with two explicit lanes (`current` and `backfill`). Each lane is an enum state with typed retry and backoff values. A single prune drops the whole ledger entry. Use a blocking or wake-based completion channel instead of the sleep loop.
- **Verification:** Write tests at the ledger API boundary (not private maps) for: a backfill failure that does not affect current recovery, and a cancellation that removes all state.

### MD-08: Descriptor flags keep growing while the coordinator still hard-codes providers
- **Severity:** Medium
- **Category:** Architecture drift
- **Evidence:**
  - `ProviderDescriptor` (`mod.rs:826-855`) has about 25 policy fields, many of them used by a single provider.
  - Four-way concrete channel enums remain anyway (`mod.rs:1337-1529`; `runtime.rs:533-560`).
  - Provider string literals sit in generic code: `realtime.rs:1934-1952`, `3601-3693`, and `3818-3836` (`send_rithmic_demand`, `flush_rithmic_demand`, `flush_hyperliquid_demand`), and `runtime.rs:723`, `1017`, `1039`, `1061`, `1151`, and `1171`.
  - The Rithmic presentation has `selection_entitlement_id: "crypto_public_realtime"` and `ready_label_suffix: " spot"` (`mod.rs:1086`, `1088`). These look copy-pasted from a crypto provider.
  - A doc comment at `mod.rs:814-816` is attached to the wrong item.
- **Proposed clean fix:** Replace the flags with a `ProviderSession` trait object (demand flush, recovery, idle policy, session status, period support). The coordinator then holds `BTreeMap<ProviderId, Box<dyn ProviderSession>>` and contains no string literals. Fix the Rithmic presentation values.
- **Verification:** Extend `tools/naming_check` to reject provider id literals in `coordinator.rs`, `realtime.rs`, `history.rs`, and `publication.rs`.

### MD-09: The generic layer depends on provider-specific calendars, and there are three calendars
- **Severity:** Medium
- **Category:** Architecture drift
- **Evidence:**
  - Generic `ProviderOrderBook` and `TradeLiveHandoff` import `aeris_rithmic_protocol_adapter::RithmicExchangeCalendar`.
  - tastytrade has two more classifiers with different venue vocabularies: `market_collection` (`tastytrade.rs:687`) uses CME names and `tastytrade_market_collection` (`tastytrade.rs:611`) uses XCME names.
  - `session_hours.rs` is a third calendar source.
  - `runtime.rs:1146-1171` branches on provider strings to pick a session source.
- **Proposed clean fix:** Add a provider-neutral `ExchangeCalendar` in `crates/domain` keyed by canonical venue (MIC), used by history, live bucketing (MD-03/04), session status, and the watchdog (MD-02). Providers map their venue codes to MICs once at the adapter boundary.
- **Verification:** Run one calendar test matrix (CME Globex, CFE, US equities, crypto 24/7, FX) that every consumer uses.

### MD-10: The desktop is not provider-neutral and owns series identity
- **Severity:** Medium
- **Category:** Architecture drift (violates the AGENTS.md invariant that the desktop stays provider-neutral)
- **Evidence:**
  - There are 100 provider id literals across 12 non-test desktop files. The largest counts are `engine_market_worker.rs` (31), `readiness_conformance.rs` (15), `replay_conversion.rs` (14), and `market_worker.rs` (14).
  - `replay_conversion.rs:209-344` hard-codes the provider id set, entitlement ids (`"tastytrade-authorized"`, `"hyperliquid-public"`, `"ctrader-authorized"`), and instrument-id prefixes.
  - `series_key` (`replay_conversion.rs:354-420`) decides `BarPeriod` per provider.
  - `worker_identity` (`engine_market_worker.rs:695-701`) labels tastytrade and cTrader workers as "Rithmic market runtime" (`engine_market_worker.rs:54-57`).
  - The `--rithmic` and `--multi-chart` startup modes (`desktop.rs:5365-5386`) build fake instruments through the non-test `default_product` (`engine_market_worker.rs:712-727`: entitlement `rithmic-test:CME:MNQ`, `price_scale: 2`).
  - Four interval tables exist (see MD-01).
- **Proposed clean fix:** Have the runtime publish a resolved `SeriesKey` and validation result in the selection reply, so the desktop sends `ChartInterval` plus the instrument and receives a key. Move identity validation into `market_runtime`. Remove the fake-instrument CLI modes or route them through the catalog.
- **Verification:** Add a `naming_check` rule banning provider id literals in `apps/desktop/src` outside assets and logos.

### MD-11: The desktop bridge polls every 2 ms and mixes mutex slots with a command channel
- **Severity:** Medium
- **Category:** Scalability / patch-on-patch
- **Evidence:**
  - `EVENT_WAIT = 2 ms` (`engine_market_worker.rs:60-63`). Its comment says it is short "so a symbol click cannot sit behind a half-frame polling quantum", which is a workaround for having no wake signal.
  - The loop (`engine_market_worker/runtime.rs:54-131`) does the following on every iteration:
    - locks four `Arc<Mutex<Option<_>>>` latest-value slots per endpoint (`engine_market_worker.rs:494-513`);
    - takes only one command per endpoint;
    - allocates a budget `Vec`;
    - calls `poll_events`.
  - Because the slots are processed before the channel, a newer slot value can overtake an older queued command, so ordering across the two paths is not preserved.
  - Errors from price alerts are discarded (`runtime.rs:412`, `let _ = process_command(...)`).
  - Practice-trading inputs pass through this presentation worker:
    - `publish_simulated_market_observation` runs from `publications.rs:126`, so the simulated venue only receives prices when a chart is receiving depth;
    - `register_provider_instrument` runs from `runtime.rs:299-303`.
- **Root cause:** There is no single command mailbox with a wake signal, and trading_runtime has no direct market-observation subscription.
- **Proposed clean fix:** Use one ordered command queue per endpoint with latest-wins coalescing inside the queue, and have the runtime outbox signal a condvar or wake. Subscribe `trading_runtime` to `market_runtime` best bid/offer directly.
- **Verification:** Measure idle CPU wakeups per second with one and eight workspaces. Add a test where a click is followed by a queued command and check ordering.

### MD-12: Reconnect policy has no backoff, and the "retry limit" message is misleading
- **Severity:** Medium
- **Category:** Maturity / scalability
- **Evidence:**
  - `PROVIDER_RECONNECT_DELAY = 250 ms` (`mod.rs:77`) applies to Rithmic and Hyperliquid (`mod.rs:1108`, `1159`).
  - The Hyperliquid loop reconnects forever at that fixed delay (`hyperliquid_realtime.rs:530-620`).
  - Rithmic uses `Duration::ZERO` for non-transport recovery (`rithmic_realtime.rs:964-966`, `1100-1106`).
  - tastytrade and cTrader use a fixed 3 s delay. No exponential backoff or jitter exists anywhere.
  - `coordinator.rs:233-245` reports "Market data retry limit reached". Its only producer is a cTrader publish failure while paused (`ctrader.rs:2224`). No retry limit actually exists.
- **Risk:** Rithmic enforces login and concurrency limits (AGENTS.md). A 250 ms or zero-delay loop during an outage risks lockout and wastes CPU and network.
- **Proposed clean fix:** Add a shared `ReconnectPolicy` (exponential backoff with jitter, a cap, and a reset on a healthy session) to the descriptor or provider trait, plus a real attempt budget that produces an explicit failure state. Rename the failure message to match the actual cause.
- **Verification:** Add a deterministic clock test of the backoff sequence. With the network unplugged, record the observed connect cadence for each provider.

### MD-13: The coordinator is a god struct with duplicated cleanup
- **Severity:** Medium
- **Category:** Maintainability
- **Evidence:**
  - The coordinator holds about 30 maps (`coordinator.rs:308-359`).
  - Per-consumer cleanup is duplicated between `detach_client` (`374-401`) and `handle_remove` (`759-790`). It covers five catalog fence maps, including `startup_*` duplicates that were added to fix fence collisions (`mod.rs:672-675`).
  - There are five near-identical `execute_study_*` wrappers (`930-1043`).
  - `reconcile_order_books` (`1085-1170`) is O(subscriptions × catalog), clones data, and runs on many hot paths. `order_books` is keyed by `(provider, instrument)` without entitlement.
- **Proposed clean fix:** Extract `ConsumerRegistry` (with a single `remove_consumer`), `CatalogFences`, `OrderBookDemand` (incremental, keyed with entitlement), and `HistoryLedger` (MD-07). The coordinator then only routes.
- **Verification:** Add a test that removing a consumer leaves zero entries in every per-consumer map.

### MD-14: `TradeLiveHandoff` is overloaded and allocates per event
- **Severity:** Low-Medium
- **Category:** Bloat
- **Evidence:**
  - The handoff struct has about 25 fields (`mod.rs:759-790`). It mixes Rithmic sequence trades with tastytrade's indexed overlay and candle reconciliation heuristics (`realtime.rs:931-1092`, including the "wait for older bucket" special cases from `4f885153`).
  - The candle symbol `format!("{}{{={}}}")` runs on every event (`realtime.rs:1994`, `2241`, `2570`) and duplicates `tastytrade.rs:1175-1184`.
  - Error strings say "Rithmic" on tastytrade paths.
- **Proposed clean fix:** Split the handoff into `SequenceTradeHandoff` and `CandleOverlayHandoff` behind the MD-06 strategy. Cache the provider wire symbol in the demand.

### MD-15: The desktop re-derives runtime state to work around fencing gaps
- **Severity:** Medium
- **Category:** Patch-on-patch
- **Evidence:**
  - Commit `771a8d73` added `MarketRecoveryFailure::Superseded` handling in desktop files.
  - Commit `f504ab86` holds the trade tape in the desktop until the replacement chart installs.
  - `WorkerEndpoint.live` (`engine_market_worker.rs:506-508`) and `apply_realtime_series_state` (`publications.rs:60-103`) re-interpret `SeriesLoadState` (for example, `Ready` while realtime means "connecting the live edge").
  - Study outputs, invalidations, removals, and price alerts are checked for consumer only, not generation (`publications.rs:181-217`). It needs verification whether the runtime fences these by generation.
  - Stale or malformed order-book frames are dropped silently (`publications.rs:119-125`).
- **Proposed clean fix:** Have the runtime publish an explicit presentation state (`Loading`, `ConnectingLive`, `Live`, `Recovering`, `AwaitingData`) and generation-fence every consumer event, so the desktop only renders.
- **Verification:** Add a runtime-boundary test that a study output from generation N-1 delivered after selection N is not published.

### MD-16: Smaller defects and inconsistencies
- **Severity:** Low
- **Category:** Bugs / code quality
- **Evidence:**
  - `prune_recent_trades` prunes by time only for `Sequence` continuity (`realtime.rs:224-227`), so tastytrade tapes rely on the 65,536-entry count cap alone.
  - `candle_provider_recovering` sets `demand_dirty` twice (`realtime.rs:2972-2975`).
  - `handle_idle_stop_overlap` checks `!self.order_books.is_empty()` across all providers instead of the stopping provider (`realtime.rs:2696`).
  - Doc comments are attached to the wrong items at `mod.rs:814-816` and `history.rs:860-880`.
  - `BrokerApi` holds its state mutex across HTTP calls (`tastytrade.rs:263-288`, `444-492`). Only catalog-thread callers were found, so this is a latent risk.
  - The desktop mailbox `enqueue` does several linear scans per message (`market_worker.rs:427-505`). This is acceptable at capacity 256, but it is O(n) per message.

### MD-17: The test structure locks in the current implementation
- **Severity:** Medium
- **Category:** Maturity
- **Evidence:**
  - About 3,870 of the 5,183 lines in `coordinator.rs` are inline tests, and `realtime.rs` tests start at line 3925.
  - Tests inspect private maps and tuples (`coordinator.rs:3547`, `3589`), and `ctrader_tests.rs` is 3,003 lines long.
  - The Day3 round-trip test uses a hand-built shape (MD-01).
  - No benchmark or endurance measurement covers the per-tick path (MD-05).
- **Proposed clean fix:** Move tests to the coordinator command/event boundary using provider fixtures, and add a small number of measured workloads (tick throughput, allocation per tick, idle wakeups).

---

## Recommended restructuring (ordered)

1. **Fix the shipped defects first, each with a regression test.** These are small and local:
   - MD-01: one interval/cadence mapping plus a round-trip test, and stop failing the whole workspace restore on one bad pane.
   - MD-02: use the provider-neutral session status in the watchdog.
   - MD-04: return an explicit demand error when no calendar exists.
   - MD-12: add backoff with jitter.
2. **Add an engine tail/view API (MD-05)** and remove the full-series copies from per-tick and existence-check paths. Measure before and after.
3. **Add a provider-neutral `ExchangeCalendar` in the domain (MD-09)** and use it for session-day live bucketing (MD-03), Week/Month handoffs (MD-04), session status, and the watchdog.
4. **Unify the series lifecycle (MD-06, MD-14).** Build one `SeriesLiveState` machine, with live-model strategies for sequence trades and the candle overlay.
5. **Create a `HistoryLedger` (MD-07)** with separate current and backfill lanes, typed retry state, and a single prune.
6. **Replace descriptor flags with a provider trait (MD-08) and split the coordinator (MD-13).** Ban provider literals in generic files through `naming_check`.
7. **Make the desktop render-only for market identity (MD-10, MD-11, MD-15).** The runtime resolves the series key and publishes presentation state, with generation fencing on all events. Use a single ordered mailbox with a wake signal. Feed trading_runtime directly from market_runtime.
8. **Rebuild the tests at the coordinator boundary (MD-17)** and add measured workloads for ticks per second, allocation per tick, and idle wakeups.
