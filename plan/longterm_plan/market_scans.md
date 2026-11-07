# Market scans (TC2000-style screener)

Market scans let a trader describe a setup once, such as "daily close above SMA(50), RSI(14) below
30, relative volume above 2", and have Aeris check every listed market against it, list the
matches, keep that list current while the page is open, and open any match in the active chart.
This is the TC2000 "EasyScan" model built into the existing Screener page. Crypto on Hyperliquid
comes first because its data is public and free. US stocks follow once a data partner signs
(ThetaData or AlgoSeek; see [provider_integration_plan.md](provider_integration_plan.md) and
[go_to_market_strategy.md](go_to_market_strategy.md)).

Item IDs are stable: **S** checkpoints. A checkpoint is **Complete** only when it works through the
real runtime and desktop path on the installed app, not when a unit test alone passes.
Status as of 2026-11-07.

**Goal (maintainer, 2026-11-07):** scans reach TC2000 breadth: any formula, the full indicator
library, every timeframe, real time across the whole market. Crypto proves it first, at full
capability; nothing is held back because stock data is not signed yet. Everything stays
provider-neutral so a stock provider (S7) plugs in without desktop or evaluator changes.

## Where we stand against TC2000

What we have (V1, pushed in `a1f44d0a` and `e5b1bef1`):

- 17 operands: open, high, low, close, volume, change %, SMA, EMA, RSI, MACD (3 lines),
  Bollinger (3 bands), ATR, ADX, relative volume, highest high and lowest low of N prior bars, and
  a typed number.
- 5 comparisons (above, below, crosses above, crosses below, between), up to 8 conditions, all or
  any.
- 5 timeframes (15m, 1h, 4h, 1D, 1W), one per scan.
- Hyperliquid perps and spot, top 50 to 500 by volume, progressive results, a rescan 30 s after
  each pass while the tab is shown, 64 saved scans, click to open on the scan's timeframe, the
  compared value shown at the market's precision.

Where we are strong (keep these as everything else grows):

- **Same numbers as the chart.** One indicator library for charts and scans; the live spot check
  matched exactly (closed bars) and within 0.004% (forming bar).
- **Local and free.** No server, no subscription; bars come straight from the provider.
- **Charts never starve.** Scans spend only the background share of the request budget (measured:
  a chart loaded in 2.3 s mid-scan).
- **Provider-neutral contract.** Definition, evaluator and results know no provider; a stock
  provider needs only its own bar fetcher.
- **Bounded and deterministic.** Every list, cache and queue has a bound; no model decides a match.
- **One click to the chart** on the scan's timeframe.

What we lack, compared with TC2000 (each gap maps to a checkpoint below):

| Gap | TC2000 | Aeris today | Checkpoint |
| --- | --- | --- | --- |
| Timeframes | Every chart timeframe, plus custom | Every provider chart interval (14 on Hyperliquid) | S15 |
| Indicators | Hundreds | 10 studies; the chart library already has 68 calculations and 61 studies | S10 |
| Formulas | Personal Criteria Formulas: arithmetic, functions, bars ago | One operand per side | S11, S12 |
| Condition logic | True for N bars, within N bars, nested and/or | Single bar, flat all/any | S13 |
| Multi-timeframe | Conditions on different timeframes in one scan | One timeframe per scan | S13 |
| Speed | Real time across the whole market | A 100-market pass takes about 3 minutes and repeats | S14 |
| Universe | All US stocks and ETFs, lists, sectors | Top 500 Hyperliquid markets by volume | S14, S7 |
| Market data fields | Fundamentals, earnings | None in scans (funding, open interest and 24 h volume exist on the Markets tab) | S16, S7 |
| Results | Custom columns, sort by formula, watchlists, alerts | Fixed columns plus one value per condition | S17 |
| Stocks | Yes | No; waits for a data partner | S7 |

Key facts for this roadmap (checked 2026-11-07):

- Hyperliquid candles (REST and WebSocket) exist for 1m, 3m, 5m, 15m, 30m, 1h, 2h, 4h, 8h, 12h,
  1d, 3d, 1w and 1M (adapter `candles.rs` `hyperliquid_interval_for_period`), and every one maps
  to a `ChartInterval`. `BarPeriod::validate` allows only these fixed periods, and the runtime
  has no bar aggregation, so a custom timeframe such as 10m or 2D needs a new provider-neutral
  aggregation step (S15).
- Hyperliquid limits per IP address: 1,200 REST weight per minute (Aeris uses 1,000, scans 700),
  10 WebSocket connections, 1,000 WebSocket subscriptions, 2,000 sent messages per minute.
  `allMids` and `allDexsAssetCtxs` cover every market in one subscription. The runtime today
  has one realtime socket per provider session with candle, trades, bbo and book subscriptions
  only (`hyperliquid_realtime.rs` `SubscriptionKey`).
- Aeris Charts (pinned `19633d6`) exports 68 indicator calculations from
  `aeris_charts_indicators` and 61 built-in chart studies (`IndicatorKind`). There is no
  formula or expression language anywhere (Aeris Charts or `study_sdk`); S11 and S12 build one
  in `market_runtime::scan`.
- No released version has scans yet (1.8.1 shipped without them). Everything added to the saved
  scan format before the first scan release costs no compatibility; after it, each change must be
  additive (new tags and enum values; an older build drops only a scan it cannot read).

## Why we are building it

- **Users ask for it first.** A screener with indicator conditions is the first thing an active
  trader looks for after charts. Today the Aeris Screener only lists Hyperliquid markets with
  price, 24-hour change, volume, open interest and funding. There are no conditions.
- **It makes the data-partner pitch concrete.** A working scan engine on crypto shows ThetaData or
  AlgoSeek exactly what their US-stock data would power on day one, and why Aeris needs a bulk or
  snapshot daily-bar endpoint from them.
- **It is the base for the AI assistant.** The AI3 screening checkpoint in
  [ai_assistant.md](ai_assistant.md) turns "show me oversold large caps" into a scan definition.
  The assistant drafts a scan. This engine evaluates it, deterministically.

## Principles

- **Same numbers as the chart.** Indicator values come from the same `aeris_charts_indicators`
  functions the chart uses, over the same provider bars, with the still-forming bar last. A scan
  and a chart showing the same market and timeframe agree on every value.
- **Deterministic, no model.** The engine evaluates a closed set of operands and comparisons. No
  model or remote service decides a match.
- **Provider-neutral contract.** `market_runtime::scan` defines the definition, request and result
  types and the evaluator. Only a provider-named file fetches bars. The desktop never names a
  provider.
- **Charts come first.** Scans spend the shared Hyperliquid request budget at background priority,
  so charts, symbol search and the catalog always keep their share.
- **Bounded.** At most 8 conditions per scan, periods up to 250, 3,000 bars per market, 1,024
  markets per scan, 500 results per publication, 64 saved scans, and a session bar cache bounded
  by total bars (600,000). Nothing about scans is persisted except the saved definitions.
- **Local-first.** Bars are fetched by the desktop process and stay in memory. Saved scans live in
  the user's workspace file. No Aeris server is involved (AGENTS.md operating constraints 1 and 2).

## Locked decisions

The maintainer decided these on 2026-11-06. Do not reopen them without asking.

1. **Indicator math lives in `market_runtime`** (`crates/market_runtime/src/scan.rs`), calling the
   pure `aeris_charts_indicators` functions directly. This required amending the `naming_check`
   rule `provider_wire_and_aeris_charts_boundaries_remain_isolated` so `aeris_charts_indicators`
   is allowed in both `crates/study_sdk/Cargo.toml` and `crates/market_runtime/Cargo.toml`.
   (The rejected alternative was a new crate or the study runtime.)
2. **Saved scans are an additive workspace field with no schema bump**:
   `repeated WorkspaceScreenerScanState screener_scans = 23` on `WorkspaceState`
   (`crates/contracts/src/messages.rs`; tag 23 is the next free tag). Recent additive fields
   (commits `ed296532`, `907d475e`, `8befca7d`) followed the same pattern. The rejected
   alternative was a schema bump to 7. Saved scans must not go in the trading SQLite store;
   that store is for trading workflows.
3. **Version 1 scope** (S9 widened the timeframes and S10 the indicators):
   - Operands: open, high, low, close, volume, change % from the previous close, and the
     indicators SMA, EMA, RSI, MACD (MACD, signal or histogram line), Bollinger (upper, middle
     or lower band), ATR, ADX, relative volume, highest high of N prior bars, lowest low of N
     prior bars, plus a typed number.
   - Comparisons: above, below, crosses above, crosses below, between (inclusive).
   - Timeframes: 15m, 1h, 4h, 1D, 1W.
   - Match all conditions or any condition.
   - Live results while the page is open, saved scans, and click a result to open its chart.
4. **Beyond V1 (2026-11-07):** the maintainer asked for TC2000 breadth on crypto now, without
   waiting for stock data. The V1 bounds and principles above still hold; S9 to S17 extend the
   scope step by step.

## Architecture

```text
Desktop Screener page (Scans tab)
  └─ MarketDataWorker::try_scan_provider(ScanProviderMarkets)        apps/desktop/src/market_worker.rs
      └─ MarketService::scan_provider_markets(client_id, scan)        market_service/runtime.rs
          └─ Command::ScanProviderMarkets → coordinator
              └─ handle_provider_scan → dispatch_market_request (shared with screens):
                 CommandFences(kind Scan) + clear events.catalog_scan
                  └─ ProviderCatalogCommand::Scan → provider catalog channel
                      └─ Hyperliquid catalog worker: ranks the universe by day volume
                         (retained catalog + statistics), forwards a HyperliquidScanJob
                          └─ aeris-hyperliquid-scan worker (hyperliquid_market_scan.rs)
                             session bar cache → background-priority candleSnapshot →
                             scan::evaluate → ScanUpdated publications (≤1/s + one final)
  ◄─ MarketRuntimeEvent::ProviderMarketScan / ProviderMarketScanRejected
     (single slot per consumer, fenced to the latest scan generation)
```

- **Why a separate scan worker.** A cold pass over 100 markets takes minutes at the provider's
  rate limit. Running it on the catalog worker would block symbol search past its 45-second
  deadline.
- **Why not chart demand.** `select_and_demand` per market would install canonical series and is
  capped at 64 series (`MAXIMUM_SERIES`). Scans need a read-only snapshot of recent bars, not
  live canonical demand.
- **Request budget arithmetic** (`crates/adapters/hyperliquid_market/src/budget.rs`): 1,000 weight
  per 60 s for the whole process, 20 per info request, plus 1 per 60 candles. A 300-bar daily
  fetch weighs about 25. Background priority may spend only up to 700 per window, keeping 300 for
  charts. That is about 28 cold fetches per minute when charts are idle. A cold 100-market daily
  scan therefore takes about 3.5 minutes, and results appear progressively. Repeat passes refresh
  only each cached series' tail, and a series fetched within the last 60 s is reused as is.
- **Live results.** One request is one pass. While the Scans tab is shown, the desktop re-sends
  the same definition (with a new generation) a short delay after each completed pass. Leaving the
  page stops scan traffic, which matches how the Markets list refreshes today.
- **Bar semantics.** `MarketBar.exchange_timestamp_unix_nanos` is the candle open time. Hyperliquid
  daily bars are UTC-aligned (`"1d"`) and weekly bars use `"1w"`, the same mapping the chart uses
  (`hyperliquid_interval_for_period`). The forming bar comes from the candle page, never from
  `previous_day_price`, which is a trailing-24-hour reference and not the daily open.
- **Warm-up.** Each study descriptor names the history its current value needs: its window for a
  windowed study, and 10 periods (`RECURSIVE_WARMUP_PERIODS`) of the smoothing period for a
  recursive one (EMA, RSI, MACD, ATR, ADX and the like), plus any window stacked on it; parabolic
  SAR takes 250 bars, Ichimoku 52 and the awesome oscillator 34. `ScanDefinition::history_bars`
  takes the deepest operand, so the current value matches a chart that loaded longer history.

## Checkpoints

| ID | Checkpoint | Status |
| --- | --- | --- |
| S1 | Scan contract and evaluator in `market_runtime` | **Complete** (unit-tested) |
| S2 | Runtime plumbing: command, fences, budget priority, scan worker, cache | **Complete** (unit-tested) |
| S3 | Desktop bridge and saved-scan persistence (tag 23) | **Complete** (unit-tested) |
| S4 | Scan builder UI and results on the Screener page (10 languages) | **Complete** (unit-tested; not yet run on the installed app) |
| S5 | Gates, real-path verification on the installed app, AGENTS.md invariant | **Complete** (UI follow-ups listed) |
| S6 | Release, website copy and changelog | Open |
| S7 | US stocks through a data partner (bulk daily bars), fundamentals | Open; needs ThetaData or AlgoSeek |
| S8 | Small V1 follow-ups (typed adapter error, budget overshoot) | Open |
| S9 | Every provider timeframe (14 on Hyperliquid) | **Complete** (live probe on 15m and 1W, installed app) |
| S10 | The full chart indicator library as scan operands | **Complete** (56 studies; live probe on 1D) |
| S11 | Formula operands: arithmetic, bars ago, functions over any operand | Open |
| S12 | Text formulas (a PCF-style language) with a parser and editor | Open |
| S13 | Condition logic: true for N bars, within N bars, nested groups, per-condition timeframe | Open |
| S14 | Real time across the whole market | Open |
| S15 | Custom timeframes by aggregation (10m, 2D, ...) | Open |
| S16 | Market fields as operands (funding, open interest, 24 h volume, mark/oracle premium) | Open |
| S17 | Results: custom columns, sort by formula, scan on a list, watchlists, alerts | Open |

### S1 — Scan contract and evaluator

- [x] `crates/market_runtime/src/scan.rs`: `ScanTimeframe` (with `bar_period()`; replaced by
      `ChartInterval` in S9), `ScanMatchMode`,
      `ScanPriceField`, `ScanMacdLine`, `ScanBand`, `ScanIndicator`, `ScanNumber` (exact
      `units / 10^scale`), `ScanOperand`, `ScanComparison`, `ScanCondition`, `ScanDefinition` with
      `validate()` and `history_bars()`, `ScanProviderMarkets`, `ProviderMarketScanMatch`,
      `ScanProgress`, `ProviderMarketScan`.
- [x] Bounds: `MAXIMUM_SCAN_CONDITIONS = 8`, `MAXIMUM_SCAN_PERIOD = 250`,
      `MAXIMUM_SCAN_RESULTS = 500`, `MAXIMUM_SCAN_UNIVERSE = 1024`,
      `MAXIMUM_SCAN_NUMBER_SCALE = 9`, `MAXIMUM_SCAN_HISTORY_BARS = 3000`.
- [x] Evaluator: `ScanBars::from_market_bars` (the same fixed-point to f64 conversion as the
      chart), `evaluate`, one computed series per distinct operand, crossings read the previous
      bar, a missing warm-up never matches, highest/lowest exclude the current bar.
- [x] Exported from `crates/market_runtime/src/lib.rs`; `aeris_charts_indicators.workspace = true`
      added to `crates/market_runtime/Cargo.toml`; `Cargo.lock` updated.
- [x] `naming_check` rule amended (`tools/naming_check/src/main.rs`, near line 3348).
- [x] 9 unit tests pass: `cargo test -p aeris_market_runtime --locked --lib scan::`.

### S2 — Runtime plumbing

Done 2026-11-07 (uncommitted until S5). `cargo test -p aeris_market_runtime --locked` passes:
432 passed, 4 ignored soak tests. Adapter tests: 63 passed. Clippy `-D warnings` and
`naming_check` are clean, and the desktop still compiles.

- [x] Budget priority in the adapter: `HyperliquidRequestPriority { Interactive, Background }`
      on `CandleSnapshotRequest`, `FOREGROUND_RESERVE_WEIGHT = 300`, and
      `WeightWindow::try_spend(weight, limit, now)`. Test:
      `budget::tests::background_requests_leave_the_foreground_reserve_unspent`.
- [x] `crates/market_runtime/src/hyperliquid_market_scan.rs`: the `aeris-hyperliquid-scan`
      worker (`run_scan`, `HyperliquidScanJob`, `SCAN_JOB_CAPACITY = 2`), newest job wins with a
      supersede check after every market, and `ScanBarCache` (60 s reuse, tail refresh merged by
      open time, a full refetch when the series is too short or too old, LRU by total bars at
      600,000, each series capped at 3,000). Each market is evaluated on its newest
      `history_bars()` bars only. Publications come at most once per second plus one final
      `complete`.
- [x] Catalog worker: `HyperliquidCatalogControl::Scan` and `handle_catalog_scan` rank the
      universe and forward the job.
- [x] Coordinator: `ProviderCatalogCommand::Scan`, `Command::ScanProviderMarkets`,
      `MarketService::scan_provider_markets(client_id, ScanProviderMarkets)`,
      `validate_provider_scan`, `CommandKind::Scan` (no 45 s deadline), `handle_provider_scan`,
      `handle_catalog_scan` and its rejection, the `events.catalog_scan` slot, and overflow
      rejection. Consumer retirement clears pending scans.
- [x] `MarketRuntimeEvent::ProviderMarketScan` and `ProviderMarketScanRejected`.
- [x] `ProviderPresentationDescriptor::market_scan_available` (Hyperliquid true, Rithmic false).
- [x] Tests: 7 in `hyperliquid_market_scan::tests`, plus `market_scan_progress_is_fenced_until_its_pass_completes`,
      `market_scan_rejections_end_only_the_current_scan`,
      `overflowed_catalog_rejects_pending_market_scans` and
      `provider_scan_validation_refuses_unbounded_requests`.

Known limits and follow-ups:

- [ ] The adapter's errors are plain strings, so a budget refusal is recognized by its text.
      Since S5 that comparison happens once, at the boundary (`fetch_candles` and
      `classify_fetch_error` in `hyperliquid_market_scan.rs`, test
      `only_a_budget_refusal_is_retried`); the worker itself only sees `SeriesError`. A typed
      adapter error would remove the last text comparison when the adapter's error type is next
      touched.
- [ ] Candle weight is charged after the response arrives, so a background request can push the
      window past 700 by up to about 50 weight for a 3,000-bar page. Charts keep at least about
      250 per minute, not exactly 300.
- Measured in S5 (below): 33 cold markets per minute, so 3 minutes for 100. About 30 minutes for
  1,024 is extrapolated, not measured.

### S3 — Desktop bridge and saved scans

Done 2026-11-07 (uncommitted until S5). The desktop tests pass with
`cargo test -p aeris_desktop --all-features --locked` (964 passed, 1 ignored, plus 132 library and
1 integration test); the contracts tests pass with `cargo test -p aeris_contracts --locked`.

- [x] `MarketDataWorker::try_scan_provider` and a `MarketWorkerCommand::ProviderScan` routed in
      `apps/desktop/src/desktop/engine_market_worker/selection_commands.rs` (it stamps
      `consumer_id`), as `try_screen_provider` does today.
- [x] Project `ProviderMarketScan` and `ProviderMarketScanRejected` in
      `apps/desktop/src/desktop/catalog_projection.rs` (`CatalogEvent::ScanUpdated` and
      `ScanRejected`) and every exhaustive event match (`workspace_surface.rs`,
      `engine_market_worker/runtime.rs`, `workflow_market_search.rs`, `desktop.rs`).
- [x] Prost messages in `crates/contracts/src/messages.rs`: `WorkspaceScreenerScanState` (name,
      timeframe, match mode, universe size, conditions), with condition and operand messages;
      `screener_scans = 23` on `WorkspaceState`. No schema bump.
- [x] Conversions between the prost messages and `aeris_market_runtime::ScanDefinition` in
      `apps/desktop/src/desktop/saved_scans.rs` (`SavedScan::from_state` and `to_state`). Unknown
      enum values make that one scan invalid and drop it; they never fail the workspace.
- [x] Sanitizer (`apps/desktop/src/desktop/local_state.rs`, `sanitize_workspace`, through
      `saved_scans::sanitize_saved_scans`): validate, dedupe by lowercased name, truncate to
      `MAXIMUM_SAVED_SCANS = 64`; names are at most 64 bytes.
- [x] Save through the one workspace writer (`workspace_persistence.rs`
      `WorkspaceSaveContent` and `apply_workspace_save`; content built in `saved_workspace.rs`
      from `WorkspaceSnapshot::screener_scans`). No second writer.
- [x] Tests: round trip (`workspace_file_round_trip_keeps_valid_saved_scans_and_drops_invalid_ones`,
      `chrome_and_saved_scans_save_through_the_one_workspace_writer`), sanitizer bounds and
      invalid values (`saved_scans::tests`), the scan callbacks' projection
      (`scan_callbacks_project_with_their_request_identity`), and a document written by 1.0.0
      (no field 23) opens unchanged (`crates/contracts/tests/contracts.rs`, the decode of a
      document without tag 23 keeps its tabs and has no saved scans).

### S4 — Scan builder UI and results

Done 2026-11-07 as code and unit tests (`desktop::market_scan::tests`, 11 tests). Nothing here has
run on the installed app yet; S5 covers that.

- [x] Screener header: a tab list with Markets (today's list) and Scans
      (`market_scan_view::screener_tabs`, `ScreenerTab` in `market_screener.rs`).
- [x] Scans tab: the saved scan list (new, rename by editing the name of the selected scan and
      saving, duplicate, two-step delete), and a builder with name, timeframe, match all/any,
      universe size (top 50, 100, 200 or 500 by volume), up to 8 condition rows (operand menu
      with its period fields, comparison menu, a number or operand on the right, an upper bound
      for between), Run and Save.
- [x] Results: progress line ("N matches · scanned N of M · K unavailable · updated" with the
      same capture-time label as the Markets list), a table with market, close, change %,
      volume and one value column per condition, sortable; clicking a row opens it in the active
      chart pane through `MarketScreener::open_market` (the former `open_row` path, now shared
      by both tabs).
- [x] Live refresh while the tab is shown (a new pass `SCAN_RESCAN_DELAY` after each completed
      pass, a silent pass replaced after `SCAN_RESPONSE_TIMEOUT`); leaving the tab or page stops
      it, and a lost consumer or provider session drops the pending pass.
- [x] Built only from `native_ui` components (`Button`, `MenuRow`, `Input`, `TabList`), with every
      color from tokens (CSS.md). New state in `market_scan.rs` and view in `market_scan_view.rs`
      beside `market_screener*.rs`. The result headers and rows are hand-built click targets like
      the Markets table, with `tools/naming_check` exceptions for `market_scan_view.rs`.
- [x] Strings in `crates/localization/catalog/<lang>/panels.lang` for all 10 languages
      (de en es fr hi ja ko pt ru zh), with the same keys and placeholders and the correct plural
      categories.
- [x] Update the `desktop_market_fences_stay_in_accept` exception counts in `tools/naming_check`:
      `market_scan.rs` has one scan-pass generation comparison ("scan pass generation fence");
      `market_screener.rs` keeps its 4.

### S5 — Gates and real-path verification

- [x] `cargo fmt --all -- --check`, workspace clippy `-D warnings`,
      `cargo test --workspace --all-features --locked` and `naming_check` pass (2026-11-07).
      The ratchets in `tools/naming_check/ratchets.toml` stay where they were: the scan fetch
      closure returns `SeriesError`, screen and scan validation share the typed
      `InvalidMarketRequest`, screen and scan dispatch share `dispatch_market_request`, a failed
      scan publication ends the pass, and the scan worker spawn returns `std::io::Result`.
- [x] Live runtime probe: `tools/aeris_probes/src/bin/market_scan_probe/main.rs`
      (`cargo run -p aeris_probes --bin market_scan_probe --locked -- --validation-root
      <%TEMP%\aeris_validation\name> --out <that root>\scan.json`). It runs real scans through
      `MarketService` on the public Hyperliquid feed. Results of 2026-11-07 (daily, top 100,
      definition: close, HighestHigh(20) and SMA(20) above 0, RSI(14) between 0 and 100):
      - Cold pass: 184 s, 33 markets per minute, 25 progressive publications, 0 unavailable,
        100 matches.
      - Chart reserve: a daily ETH chart opened mid-scan loaded its history in 2.3 s; the BTC
        baseline before the scan took 4.4 s, which includes opening the session.
      - Warm pass right after: 234 s (26 markets per minute). See the finding below.
- [x] Spot check against the chart's own bars (the probe's `spot_check_matches_chart`): ETH
      HighestHigh(20) is exactly 2525.0 in both; SMA(20) is 2326.155 in the scan and 2326.055 on
      the chart, because the forming close moved by 2.0 between the two reads (0.004%).
- [x] AGENTS.md architecture invariant for scans (owner, budget priority, bounds, no
      persistence except saved definitions), beside the Hyperliquid budget rule.
- [x] Release build on a validation root (2026-11-07, driven through screenshots and clicks): the
      Scans tab renders with the default builder, a named scan saves (1/64) and runs with
      progressive results ("Scanning 20 of 100 markets"), clicking ZEC-USDC opened it in the
      Terminal chart, and after closing and relaunching the app the saved scan was still listed
      and reloaded into the builder.

UI follow-ups seen on the installed app (fixed and checked on a release build):

- [x] The value column showed the left operand, so "Close above SMA 20" repeated the close.
      The runtime now publishes both sides of each condition (`ScanConditionValues`), and a
      condition whose left side is the close or volume the row already shows, compared with a
      value that varies by market (not a number, not a range), shows the compared value under
      its own header ("SMA(20)"); sorting uses the shown value.
- [x] Values printed 4 decimals while the Close column used the market's precision. Each match
      now carries `price_decimals` (the fractional digits its bars' prices use), and the close
      and every price-scale value in the row use it, at least two ("613.80" / "607.69",
      "0.004925" / "0.004684").
- [x] Opening a result kept the pane's interval. It now opens on the scan's timeframe
      (the scan's `ChartInterval`, nearest offered by the provider); a chart already showing
      the market only changes timeframe. Panes synced to that pane follow the symbol; a new
      market's timeframe is not relayed with it.

Finding: repeated passes are budget-bound. A tail refresh costs the same 20 base weight as a
full fetch, and a 100-market pass takes longer than the 60-second reuse window, so the cache
saves response size but not budget. With a 30-second pause between passes, each result is
refreshed about every 4 minutes, and scans keep spending the full 700 background weight per
minute while the Scans tab is shown. Charts keep their reserve, as measured. To make results
truly live, a follow-up (S8) can keep the forming bar current from the provider's free
WebSocket candle stream for the universe instead of re-fetching it.

### S6 — Release and website

- [ ] Ship in the next minor release (it adds a user-visible capability) through the AGENTS.md
      release workflow.
- [ ] Website (`C:\Users\devraj\Downloads\Aeris\Aeris-website`; the AGENTS.md path is out of
      date): update `src/content/comparisons.ts` (the "Does Aeris have a screener?" answer near
      line 506), `src/lib/seo.ts`, `src/app/page.tsx` near line 110, `public/llms.txt` and a
      changelog entry, only once the release that carries scans is published.

### S7 — US stocks

- [ ] Ask the data partner for a bulk or snapshot daily-bar endpoint (one request for the whole
      market). Per-symbol history cannot scan about 5,000 stocks inside a rate limit.
- [ ] Add the partner as a provider with `market_scan_available`, its own scan worker and budget.
      The desktop needs no change beyond the universe choices.
- [ ] Data licensing: confirm that displaying derived scan values locally is allowed under the
      partner agreement and exchange policies (AGENTS.md data licensing checklist).

- [ ] Fundamentals (market cap, sector, earnings) as operands once the partner supplies them,
      through the same provider-neutral field operands as S16.

### S8 — Small V1 follow-ups

- [ ] The typed adapter error and the post-response weight charge listed under S2.
- [ ] Hyperliquid candle volume overflows for high-supply tokens on long bars. Volume is held at
      the normalized quantity scale (8 places), so one bar holds at most about 92 billion units
      (`i64::MAX / 10^8`). On 1W, PUMP, kPEPE and the spot market `@188` exceed it and the
      adapter refuses the page ("market decimal value overflowed"); the scan counts them as
      unavailable (3 of 100 on 2026-11-07), and charts read the same decoder at the same scale.
      The durable fix is a per-market quantity scale from `szDecimals` (0 for these tokens),
      which also touches instrument terms the trading store registers, so it is planned on its
      own rather than inside a scan change.

### S9 — Every provider timeframe

- [x] `ScanTimeframe` is gone: `ScanDefinition.timeframe` is a `ChartInterval`, the interval
      a chart uses, so every interval a provider publishes (on Hyperliquid 1m, 3m, 5m, 15m, 30m,
      1h, 2h, 4h, 8h, 12h, 1D, 3D, 1W, 1M) is a scan timeframe, and opening a match needs no
      mapping.
- [x] The provider's catalog dispatch resolves the interval to its bar period through the one
      `resolve_cadence` mapping and refuses an interval it does not offer before dispatch
      ("hyperliquid does not offer a 5s market scan"); the scan job carries the resolved
      `BarPeriod`, and the worker cache is keyed by it.
- [x] The builder lists only the scanning provider's published chart intervals, labelled as the
      chart toolbar labels them (`ChartInterval::label`, language-neutral), so the five
      `scan_timeframe_*` label keys left all ten catalogs; `scan_timeframe_aria` stays.
- [x] The worker's tail estimate uses the shortest span a bar of its period covers (a month
      counts 28 days), so a tail request never passes one page; the history window already
      handled every period.
- [x] Persistence: saved scans store the interval label in `WorkspaceScreenerScanState.interval`
      (tag 6), the form a chart pane stores. Tag 2, a five-value enumeration that no release
      wrote, is retired; a scan saved with it decodes without an interval and is dropped alone.
- [x] Unit tests: every offered interval has a history window and tail estimate, an intraday
      tail refresh, the unoffered refusal and the weekly period at dispatch, the builder's
      interval list, every interval round-tripping through persistence, and the retired tag.
- [x] Live probe (`market_scan_probe --interval`, 100 markets, 2026-11-07): 15m cold pass
      187 s and warm 233 s, 100 of 100 evaluated, chart history during the scan 2.7 s (baseline
      2.1 s), ETH highest-high(20) equal to the chart and SMA(20) within the forming bar's move;
      1W cold 184 s and warm 235 s, 97 evaluated and 3 unavailable (the volume overflow under
      S8), chart history during the scan 1.8 s, the same ETH spot check passing. The unoffered 5s
      interval was refused on both runs.
- [x] Installed app (release build, validation root, 2026-11-07): the timeframe menu lists all
      14 Hyperliquid intervals, a 15m scan streams its results, and opening a match shows its
      chart on 15m.

### S10 — The full indicator library

- [x] Replace the closed `ScanIndicator` list with a descriptor table: each study names its
      parameters (kind, default, bounds), its outputs (one or several lines) and its inputs
      (OHLCV), and calls its `aeris_charts_indicators` function. The evaluator, the builder
      menus and persistence read the table, so adding a study is one table entry.
      Done as `market_runtime/src/scan/studies.rs` (`SCAN_STUDIES`, 56 descriptors in six
      families), each with its schema identifier, parameters, lines, calculation and history.
- [x] Every single-series calculation the library exports: moving averages (SMA, EMA, DEMA,
      TEMA, SMMA/RMA, HMA, WMA, VWMA, KAMA, McGinley, ALMA, linear regression), oscillators
      (RSI, MACD, stochastic, stochastic RSI, CCI, Williams %R, momentum, rate of change, Chande
      momentum, DPO, awesome oscillator, Aroon, TRIX, KST, TSI, Fisher, ultimate oscillator,
      Coppock), trend (ADX/DMI, parabolic SAR, SuperTrend, Ichimoku, vortex, choppiness, mass
      index), volatility (ATR, standard deviation, historical volatility), bands and channels
      (Bollinger and its %B/width, Donchian, Keltner, envelopes, ATR bands), volume (OBV,
      accumulation/distribution, PVT, Chaikin, relative volume, Klinger, volume oscillator,
      Elder force, ease of movement, CMF, MFI). Session-dependent studies (VWAP and its bands,
      pivots, session levels) follow once their calendar input is wired.
      Shipped: every listed study except OBV, accumulation/distribution and PVT, which are
      cumulative from the first loaded bar, so their level depends on where history starts and a
      scan on recent bars cannot match the chart; they return with S11's "change over N bars".
      Every chart line is offered except Ichimoku's lagging close, which is drawn 26 bars back
      and has no current value. Highest high and lowest low of N prior bars stay as studies.
- [x] Persisted as a study identifier plus parameter values, so new studies need no new message
      fields. Grouped, searchable operand menu in the builder.
      `WorkspaceScanOperandState` carries `study` (tag 10, the schema identifier such as
      `sma`), `study_parameters` (tag 11, name plus exact `units / 10^scale`) and
      `study_output` (tag 12, the line). Operand kinds 7 to 16 and tags 2 to 7 named the fixed
      V1 indicators, which no release wrote; they are retired, and a scan saved with one is
      dropped alone. An unknown study, line or out-of-bounds parameter drops its scan; a
      parameter not stored takes its default. The builder's operand menu groups the studies by
      family with a search field (Enter picks the first match), each row shows its description,
      a line menu chooses among a study's lines, and each parameter has its own labelled input
      (a switch for on/off parameters). Labels read like the chart's, such as "Envelopes
      Upper(20, 2, Exponential)", in all ten languages.
- [x] Each study checked against the chart's value on the same bars (unit tests on fixtures and
      one live probe spot check per family).
      Unit tests: every line of every study equals the library formula on a 400-bar fixture
      (1e-9 relative), the history each study requests settles its current value against 3,000
      bars (1e-4 relative), every chart line is offered, and the table is indexed and bounded.
      Live probe (`market_scan_probe`, 1D, 100 markets, 2026-11-07): cold pass 185 s and warm
      234 s, 100 of 100 evaluated, chart history during the scan 2.7 s (baseline 5.2 s; an
      earlier run measured 11.3 s against 2.8 s, so this latency varies with the provider). ETH
      matched independent reference math on the same bars for one study per family: highest
      high(20) and SMA(20) exactly, Bollinger(20, 2) bands exactly, RSI(14) within 0.0003,
      vortex(14) VI+ and CMF(20) exactly, ATR(14) within 0.0001%.
- [x] Installed app (release build, validation root, 2026-11-07): searching "macd" in the operand
      menu and pressing Enter picks MACD with Fast 12, Slow 26 and Signal 9 inputs; the line
      menu offers MACD, Signal and Histogram; "MACD above Signal" on 1D streams its matches with
      a "MACD(12, 26, 9)" column, a match opens its 1D chart, and the saved scan reopens after a
      restart with the same study, line and parameters.

### S11 — Formula operands

- [ ] An operand becomes a bounded expression tree: numbers, bar fields, studies, `+ - * /`,
      unary minus, and functions over any operand: value N bars ago, average/sum/highest/lowest/
      standard deviation over N bars, change and percent change over N bars, absolute value,
      min/max of two operands. Examples: close > SMA(50) × 1.05; volume > 2 × average of volume
      over 50 bars; RSI(14) 5 bars ago < 30.
- [ ] Bounds: at most 64 nodes and depth 8 per expression; the history depth needed is the sum
      of offsets and windows along the deepest path, capped at the scan history bound. Division
      by zero and missing warm-up yield no value, never a match.
- [ ] Builder: each side is a small formula editor (operand, then optional "× number",
      "N bars ago", or a function wrapper), saved as the tree.

### S12 — Text formulas

- [ ] A PCF-style text language over the S11 tree (`C > AVGC50 * 1.05`, `RSI14.5 < 30` style
      shorthand plus readable names such as `close > sma(50) * 1.05 and rsi(14)[5] < 30`), with a
      hand-written parser that reports the position of every error, and a formatter that prints
      any saved tree back to text.
- [ ] A formula editor with inline errors; the builder and the text view edit the same tree.

### S13 — Condition logic

- [ ] Modifiers on any condition: true for N consecutive bars, true at least once within the last
      N bars (crossed within N bars), rising or falling for N bars.
- [ ] Nested all/any groups (bounded depth and total conditions).
- [ ] A timeframe per condition (daily trend up and 1h RSI below 30), each market fetching every
      timeframe the scan names.

### S14 — Real time across the whole market

- [ ] After a market's first fetch, keep its forming bar current from the provider's live candle
      stream instead of re-fetching it: a scan-owned, bounded set of candle subscriptions on the
      runtime's provider session (within Hyperliquid's 1,000 subscriptions, leaving room for
      charts), re-evaluating only the markets that changed and publishing at most once per
      second. A whole-market price feed (`allMids`) covers markets beyond the subscription
      budget.
- [ ] Universe choices up to every listed market (perps, spot and builder dexes), and a named
      list (a watchlist) as the universe.
- [ ] Measure: time from a provider candle update to the result row, and request weight per
      minute while live (target: close to zero REST weight once warm).

### S15 — Custom timeframes

- [ ] A provider-neutral aggregation step in `market_runtime` that builds N-minute, N-hour and
      N-day bars from the nearest provider period that divides them, aligned like the chart.
- [ ] Coordinated with Aeris Charts and `ChartInterval` so a custom-timeframe match opens on the
      same custom chart interval.

### S16 — Market fields as operands

- [ ] Provider-published per-market fields as operands: funding rate, open interest, 24 h
      notional volume, mark and oracle prices and premium, and their change. The Markets tab
      already receives these (`ProviderMarketStatistics`). A stock provider later publishes
      market cap, sector and earnings through the same field operands (S7).

### S17 — Results

- [ ] Custom result columns (any operand or formula), sort by any of them.
- [ ] Scan on the results of another scan or on a watchlist; save results as a watchlist.
- [ ] Alerts when a market enters or leaves a scan, through the existing notification feed
      (event descriptions only, per the data licensing checklist).
- [ ] Export results to CSV.

## How to resume (for a new agent)

1. Read `AGENTS.md`, then this file, then `crates/market_runtime/src/scan.rs`.
2. Run `git status --short --branch` and `git diff --stat`. Uncommitted scan work may be in the
   tree: `scan.rs`, `hyperliquid_market_scan.rs`, edits under `crates/market_runtime/src/market_service/`,
   `crates/adapters/hyperliquid_market/src/budget.rs` and `http.rs`, and `tools/naming_check`.
   Treat it as this plan's work in progress. Never reset it.
3. Run `cargo test -p aeris_market_runtime --locked` and tick the S2 boxes that are actually done.
   A box is done only when its code exists and its tests pass.
4. Continue at the first open box. Update the status table and the "Status as of" date in this
   file in the same change as the code.
5. Keep `target/` under 25 GB (AGENTS.md "Build output hygiene"). Prefer per-crate
   `cargo check`/`cargo test` while iterating.

## Code map

- Screen path to mirror: `market_service/runtime.rs` `screen_provider_markets` (near 1366),
  `instrument_selection.rs` `handle_provider_screen` (near 679) and `handle_catalog_screen`
  (near 885), `command_fences.rs`, `publication.rs` (`catalog_screen`), `lib.rs`
  `MarketRuntimeEvent`.
- Hyperliquid: `hyperliquid_realtime.rs` (catalog worker), `hyperliquid_market_screen.rs`
  (`screen_rows`, `instrument_summary`, whose `symbol` is the wire coin), `hyperliquid_history.rs`
  (`history_window`), adapter `http.rs` (`fetch_candle_snapshot`), `candles.rs`, `budget.rs`.
- Indicators: `aeris_charts_indicators` (pinned Aeris Charts revision), plain functions `sma`,
  `ema`, `rsi`, `macd`, `bollinger`, `atr`, `adx_dmi`, `relative_volume`.
- Desktop: `apps/desktop/src/desktop/market_screener.rs`, `market_screener_view.rs`,
  `catalog_projection.rs`, `apps/desktop/src/market_worker.rs`, `workspace_tabs.rs`
  (`select_watchlist_instrument`), `native_ui/`, `workflow_canvas.rs` (field, menu and
  compare-and-value patterns).
- Persistence: `crates/contracts/src/messages.rs`, `apps/desktop/src/desktop/local_state.rs`,
  `workspace_persistence.rs`, `saved_workspace.rs`, `workspace_migrations.rs`
  (`CURRENT_WORKSPACE_SCHEMA = 6`, unchanged).

## Open questions

- Default universe size: 100 is planned. A larger default makes the first pass slower (about
  3.5 minutes for 100 cold daily scans).
- Delay between live passes: about 30 s after a completed pass is planned.
- Whether a scan should keep refreshing while the user is on the Terminal page. It is planned not
  to, which matches the Markets list. S14 and the S17 alerts may revisit this: an alerting scan
  has to keep running in the background, within its own bound.
