# Chart Boundary Plan

Aeris Charts owns chart state, time-scale math, drawings and their persistence, indicators, legend
content and series styling. `crates/ui/chart_integration` is product glue only: the market-data
bridge, runtime projections, design tokens, legend and menu UI, and workspace persistence of what
the engine exports. An audit of Terminal against Aeris Charts `b35026b` found host code doing
engine work. This file is the single status and requirements record for removing it.

Item IDs are stable: **CB** items, **AC** Aeris Charts batches, **TB** Terminal batches, **D**
decisions. Engine items are listed here by ID so both repositories can reference them; the
Aeris Charts repository may mirror them in its own `plan/Expansion.md`.

**Contents**

1. [Status](#1-status)
2. [Rules for this work](#2-rules-for-this-work)
3. [Items](#3-items)
4. [Batches](#4-batches)
5. [Persistence compatibility](#5-persistence-compatibility)
6. [Decisions](#6-decisions)
7. [Verification](#7-verification)

---

## 1. Status

Updated 2026-10-04. A batch is **Complete** only when it is exercised through the real desktop path
(load, timeframe switch, restart, restore), not when a unit test alone passes.

| Batch | Repository | Items | Status | Blocked by |
| --- | --- | --- | --- | --- |
| [AC1](#ac1--time-and-drawing-ownership) Time and drawing ownership | Aeris Charts | CB1e, CB2e, CB3e | **Open** | — |
| [AC2](#ac2--missing-engine-operations) Missing engine operations | Aeris Charts | CB4e, CB5e, CB6e, CB7e, CB8e, CB9e, CB10e, CB15e | **Open** | — |
| [P1](#p1--pin-bump) Pin bump | Terminal | — | **Open** | AC1, AC2 pushed |
| [TB1](#tb1--drawing-persistence-and-shadow-series-removal) Drawing persistence, shadow series removal | Terminal | CB1t, CB2t, CB3t | **Open** | P1 |
| [TB2](#tb2--replace-host-patches-and-internal-access) Host patches and internal access | Terminal | CB4t, CB5t, CB6t, CB7t, CB8t, CB13, CB15t | **Open** | P1 |
| [TB3](#tb3--single-catalog-and-typed-state) Single catalog and typed state | Terminal | CB9t, CB10 | **Open** | P1 |
| [TB4](#tb4--misplaced-product-code) Misplaced product code | Terminal | CB11, CB12, CB13 | **Open** | — |
| [TB5](#tb5--boundary-guards) Boundary guards | Terminal | CB14 | **Open** | TB1–TB4 |

**Order:** AC1 and AC2 are independent of each other and can ship in one Aeris Charts push. P1 bumps
every `aeris_charts_*` revision together. TB1–TB3 follow P1 in any order. TB4 has no engine
dependency and may start at once. TB5 lands last so its guards pass on the cleaned code.

---

## 2. Rules for this work

- Engine first. A missing operation is added to Aeris Charts, gated there with its full batch gates
  (`AGENTS.md` in that repository), pushed, and only then consumed here through a pin bump. Never
  compensate on the Terminal side while waiting.
- Pin bumps change only the `aeris_charts_*` revisions in `Cargo.toml`, `Cargo.lock` and the
  expected revision in `tools/naming_check`. GPUI is not updated incidentally.
- Every removal of Terminal code deletes the superseded path in the same batch. No compatibility
  layer survives except the persisted-document reader in [section 5](#5-persistence-compatibility).
- Hosts receive behavior, not mechanisms: if Terminal would call a sequence of engine methods in a
  fixed order, that sequence is one engine operation.
- Keep `docs/Architecture.md` in Aeris Charts synchronized with any new public operation.

---

## 3. Items

Each item names the evidence, the owner, the required change and the completion condition. A
suffix `e` is the Aeris Charts half, `t` is the Terminal half.

### CB1 — Drawing persistence and time anchoring are re-implemented in Terminal

- **Evidence:** `view.rs:2053-2151` exports `drawings_json`, injects an `aeris_anchor_time` field per
  anchor, and re-maps time to logical index on import through `ProductPriceBars`
  (`engine_bridge.rs:33-82`). Locks travel in a separate `locked_drawing_ids` list
  (`workspace_surface.rs:1387`, `:1469`).
- **Engine today:** versioned `export_state_json` / `import_state_json` with `anchor_times_micros`
  exist (`persistence.rs:550`, `:1629`), but `apply_persisted_drawing_anchor_times`
  (`lib.rs:3899-3954`) resolves only exact bar open/close matches, and `sequence_points` is set only
  on sequence-projected series (footprint, synthetic trade bars). Ordinary time series installed
  through `set_series_data` get no anchors, and anchors cannot survive a timeframe change.
- **CB1e:** the engine records a continuous exchange-time anchor for every drawing point on any
  time-based series, resolves it by interpolation between bars (fractional placement preserved, for
  example logical 2.5 on 1m becomes 0.5 on 5m), extends resolution into the past and future
  projections, and re-resolves automatically whenever the series data is replaced. Locks are part of
  exported drawing state. `export_state_json` / `import_state_json` carry all of it.
- **CB1t:** Terminal persists the engine document verbatim and restores it verbatim. Delete
  `export_semantic_state_json`, `import_semantic_state_json`, `locked_drawing_ids` and the
  `aeris_anchor_time` key. Old documents are migrated per section 5.
- **Done when:** a drawing placed on 1m reappears at the same time on 5m, 1h and after restart,
  including anchors left of loaded history and right of the newest bar; locks survive restart; no
  Terminal code reads or writes drawing JSON fields.

### CB2 — Terminal keeps a shadow copy of the price series

- **Evidence:** `ProductPriceBars` (`engine_bridge.rs:23-153`) duplicates times and OHLC of series 0.
  It is used to re-send data after `convert_series_kind` (which already keeps data, engine
  `lib.rs:2522`), for drawing anchors (CB1), visible-range restore (CB3), footprint alignment (CB4).
  `update_bar` does a linear scan plus `Vec::insert` per delta.
- **CB2e:** nothing new beyond CB1e, CB3e and CB4e; confirm `convert_series_kind` round-trips every
  `ChartType` kind (Candlestick, Bar, Line, Area, Baseline) without data loss and add an engine test
  that proves it.
- **CB2t:** delete `ProductPriceBars` and every caller. `install_product_price_series` becomes
  `convert_series_kind` plus marker option; data is installed once per snapshot and updated through
  `update_series_bars` only.
- **Done when:** `ProductPriceBars` no longer exists; chart-type switching does not re-upload data;
  live updates cost no per-delta scan in Terminal.

### CB3 — Visible-range time math lives in Terminal

- **Evidence:** `visible_time_range_unix_nanos` (`view.rs:1298-1321`) extrapolates logical indices
  past loaded data; `set_visible_time_range_unix_nanos` (`view.rs:1324-1347`) maps time to logical
  through `ProductPriceBars` when a future projection is active.
- **CB3e:** one engine query and one setter for the visible range in exchange time that are exact
  inside data and use the configured past/future projections outside it.
- **CB3t:** both methods become direct calls; delete the extrapolation.
- **Done when:** workspace restore reproduces the saved range, including right-offset space, on time
  bars; tick, volume and monthly bars fall back to the engine's clamped behavior.

### CB4 — Footprint alignment is supplied by the host

- **Evidence:** `order_flow.rs:197-219` passes `anchor_micros` from the newest price bar and
  `recent_median_price_range` computed by Terminal (`engine_bridge.rs:94-108`).
- **CB4e:** `add_order_flow_presentation` derives the bar-grid anchor and the automatic row size from
  the series it is attached to. The options lose `anchor_micros` and `recent_median_price_range`.
- **CB4t:** delete `chart_aggregation`'s anchor argument, `AUTO_ROW_SAMPLE_BARS` and
  `recent_median_range`.
- **Done when:** weekly footprint bars align with weekly candles (existing test
  `footprint_time_bars_share_the_price_series_bar_opens` moves to the engine) and automatic row size
  matches today's output on the same fixture.

### CB5 — Terminal writes and reads engine internals

- **Evidence:** `engine.series.iter_mut()` sets `visible`, `histogram_updown`, `title`,
  `title_visible` (`engine_bridge.rs:208-227`), `point_markers` with a production `panic!`
  (`:319-328`), and `title` (`:378-390`). `legend_rows` reads `engine.crosshair` and
  `engine.time_scale.coordinate_to_index` (`indicators.rs:153-156`). Legend layout reads
  `engine.pane_left`, `engine.pane_w`, `engine.panes` (`indicators.rs:444-463`); context menus read
  `engine.pane_left` (`input.rs:75`). `set_theme` reads `engine.time_visible` (`view.rs:1283`);
  `rebuild` reads `engine.options.get().layout` (`view.rs:2301`).
- **CB5e:** typed operations for series title, title visibility, point markers and histogram up/down
  coloring; `financial_legend` resolves the crosshair position itself; a pane-layout query returning
  plot rectangles; context-menu events carry chart-surface coordinates; read accessors for time
  visibility and layout font. After the bump, the engine may narrow these fields to `pub(crate)`.
- **CB5t:** replace every access above with the typed operation. Remove the `panic!`.
- **Done when:** no `engine.<field>` access remains in `chart_integration` (guarded by CB14).

### CB6 — Volume is assembled by hand instead of being an engine indicator

- **Evidence:** `install_volume_series` (`engine_bridge.rs:208-227`) builds a hidden histogram on an
  overlay scale with hard-coded margins and volume price format; `apply_volume_chrome`
  (`indicators.rs:361-374`) applies indicator label policy by JSON because the engine's
  `IndicatorChromeOptions` does not cover it. VWAP and Volume Profile bind to this series.
- **CB6e:** a native volume indicator (up/down coloring from the price series, overlay placement,
  volume format) that receives `IndicatorChromeOptions` like every other indicator, appears in
  `financial_legend`, and can serve as the volume input for VWAP and the volume profile while
  hidden.
- **CB6t:** add Volume through the indicator API; delete `install_volume_series`,
  `apply_volume_chrome`, `VOLUME_LEGEND_IDENTITY` and the host-legend leading row. The persisted
  identifier `volume` is unchanged.
- **Done when:** showing, hiding, removing and restoring Volume behaves as today; VWAP and the
  profile still compute with Volume hidden.

### CB7 — Volume Profile legend row is inserted by the host

- **Evidence:** `insert_volume_profile_legend_row` (`indicators.rs:241-263`).
- **CB7e:** `financial_legend` emits a row for native price-pane primitives such as the volume
  profile, with visibility and removal identity.
- **CB7t:** delete the insertion and `LegendItem::VolumeProfile` special cases that the identity
  makes redundant.

### CB8 — Other host patches for missing engine operations

| Patch | Evidence | CB8e (engine) | CB8t (Terminal) |
| --- | --- | --- | --- |
| Study line widths kept in a Terminal map and reapplied after reset | `studies.rs:83-123`, `view.rs:1790` | Per-study output style (line width) owned by the engine, preserved by `reset_style_to_defaults` only when explicitly host-set, included in study state | Delete `study_line_widths` and `reapply_study_line_widths` |
| Price-axis state restored by replaying menu toggles | `view.rs:1192-1274` | A typed price-scale state snapshot and one restore operation | `restore_price_axis_menu_state` becomes one call |
| Initial window forces a second layout pass | `view.rs:2263-2273`, `:2343-2347` | Fit option "open on the newest N bars" applied inside the first layout | Delete `narrow_to_initial_window` and the second `prepare_frame` |
| Price format chosen by repeated per-scale calls | `view.rs:1692-1705` | Keep; this is product policy over a real engine API | No change; recorded so it is not re-audited |
| Session plan levels removed and re-added on every series reinstall | `view.rs:2167-2197` | Price lines survive `convert_series_kind` (verify; add test) | After CB2t, install once and remove the reinstall dance |

### CB9 — The indicator catalog is defined three times

- **Evidence:** engine `IndicatorKind`; Terminal `ChartIndicator` with labels, parameter text and
  defaults (`view.rs:146-304`); desktop `IndicatorKind`, `IndicatorParameters`, `IndicatorLocation`
  and `INDICATOR_SPECS` (`chart_chrome.rs:20-226`). They already disagree (VWAP "Session anchored"
  vs "Session volume weighted price"; profile text with and without "POC"). Volume Profile text
  restates engine defaults (`VolumeProfileIndicatorOptions::default()`, `volume_profile.rs:26-38`).
- **CB9e:** an engine catalog for built-in indicators: stable identifier, label, default kind with
  parameters, pane placement, and parameter description derived from the defaults.
- **CB9t:** one Terminal catalog that maps product availability onto the engine catalog. Delete the
  desktop duplicate and the hand-written labels and parameter strings. Persisted identifiers are
  unchanged.
- **Done when:** labels and parameter text come from one source; a test fails if a product indicator
  has no engine catalog entry.

### CB10 — Engine enums are copied as raw numbers

- **Evidence:** crosshair mode `0..=3`, crosshair style `<= 4`, crosshair width `1..=4`
  (`local_state.rs:500`, `:535-536`, `workspace_tabs.rs:1064-1175`, `workspace_surface.rs:325-326`,
  `:1435-1437`, `chart_context_menus.rs:1983-2044`); price-scale mode codes and `u16` flag bits
  (`view.rs:503-586`, `workspace_surface.rs:1425-1431`).
- **CB10e:** typed enums with bounds for crosshair mode, line style and width range; the price-scale
  snapshot from CB8e.
- **CB10t:** Terminal uses the typed values internally and converts to numbers only at the
  persistence boundary, through one mapping each.

### CB11 — Sweep classification lives in the chart crate

- **Evidence:** `classify_order_flow_sweeps` and its threshold (`order_flow.rs:447-519`) feed only
  the Time & Sales panel (`workspace_surface.rs:4196`, `time_sales_panel.rs:279`) and run on the UI
  thread.
- **Change:** move classification next to the canonical tape in `market_runtime` (published with
  `MarketTradeTapeSnapshot`, computed off the UI thread, bounded by the tape), with
  `OrderFlowSweep` in `domain/market_data`. Threshold ownership per D1.
- **Done when:** `chart_integration` exports no sweep type; the panel highlights the same trades on a
  recorded fixture.

### CB12 — Hard-coded radius in the legend

- **Evidence:** `view.rs:2661` uses `rounded(px(3.0))`, contrary to `CSS.md`.
- **Change:** use the matching `RadiusToken`.

### CB13 — Production panics and expects in chart glue

- **Evidence:** `panic!` at `engine_bridge.rs:326`; `expect` on options parsing at `view.rs:114`,
  `:139`.
- **Change:** CB5t removes the panic. Options become typed engine setters (CB5e) or return errors to
  the caller; no `expect` remains outside tests.

### CB14 — Boundary guards do not cover these patterns

- **Evidence:** `chart_integration_routes_interaction_through_aeris_charts` in `tools/naming_check`
  bans interaction primitives only.
- **Change:** add assertions that `chart_integration` production code contains no `engine.series`,
  `engine.panes`, `engine.pane_left`, `engine.pane_w`, `engine.crosshair`, `engine.time_scale`,
  `engine.options`, `drawings_json`, `ProductPriceBars`, and that `apps/desktop` defines no indicator
  catalog.

### CB15 — Footprint mode draws candles where no footprint data exists

- **Requirement:** a footprint chart never shows candlesticks. Bars without real trade data are
  blank. Footprint bars appear only where the canonical trade tape has trades: live trades as they
  arrive, plus the trades a provider already returns in the current session (today only the bounded
  tastytrade TimeAndSale backfill). Prior-session footprint is out of scope: no connected provider
  supplies it without paid historical tick data. Nothing is synthesized and nothing falls back to
  another series kind.
- **Evidence:**
  - Terminal opts in: `ChartType::Footprint` maps the price series to `SeriesKind::Candlestick`
    (`view.rs:472-482`) and adds the footprint as a second series (`order_flow.rs:206-230`), with the
    stated intent that the price series "draws them as candles and hands its tail to the footprint".
  - The engine implements the hand-off: `update_order_flow_presentation` sets the price series'
    `render_before_time` to the first footprint bar (`footprint.rs:1906-1910`), so every bar before
    it is drawn as a candle. This is documented ("candles before a live footprint", engine
    `lib.rs:1031-1033`) and tested (`footprint.rs:5316`).
  - Before the first trade, `render_before_time` stays `None`, so the whole chart is candles.
  - When the bounded tape evicts old trades, the first footprint bar moves right and evicted bars
    turn back into candles.
  - Hidden rows still take part in price-scale fitting (engine `lib.rs:3201-3202`), and Terminal
    opens on the newest 600 bars (`view.rs:93`, `:2263-2273`). With candles removed, the live
    footprint would be squashed vertically and squeezed into one column at the right edge.
- **CB15e (Aeris Charts):** a footprint-owned price pane as one engine operation, selected through
  `OrderFlowPresentationOptions`:
  - the primary series keeps its data, time-axis, crosshair, last-value and price-line
    participation, but draws no bars anywhere while the footprint owns the pane;
  - bars without footprint data render blank; tape eviction leaves blank space, never candles;
  - automatic price scaling fits what is drawn (footprint bars), not the hidden price series;
  - the initial and "scroll to latest" view frame the footprint bars at a legible bar width
    instead of the full price history;
  - an engine-owned empty-pane state ("Waiting for trades") while the footprint has no bars;
  - remove the candle hand-off through `render_before_time` and its "candles before a live
    footprint" documentation, and update `docs/Architecture.md`.
- **CB15t (Terminal):**
  - request the footprint-owned pane; delete the candle hand-off comment and the Candlestick
    mapping intent for `ChartType::Footprint`;
  - drop the legend coupling that toggles series 0 together with the footprint
    (`indicators.rs:287-290`) if the engine's footprint-owned pane makes it redundant;
  - keep feeding the canonical tape unchanged, including provider trade history where it exists;
  - when the instrument has no price increment, keep today's "Footprint is unavailable" message
    (`workspace_surface.rs:4062-4081`) and show an empty pane, not candles.
- **Done when:**
  - switching to Footprint with no trades shows an empty pane with the waiting state, no candles;
  - the first live trade draws one footprint bar, legibly sized and scaled;
  - over a long session, evicted bars become blank, never candles;
  - on a provider with trade backfill, history appears as real footprint bars only for the
    backfilled window;
  - switching back to Candles restores the full candle history with no reload.

---

## 4. Batches

### AC1 — Time and drawing ownership

Aeris Charts. CB1e, CB2e, CB3e. Engine tests: cross-timeframe anchor round trip (1m, 5m, 1h, 1D),
anchors outside loaded history, lock persistence, kind conversion round trip, visible-range round
trip with projections. Update `docs/Architecture.md`.

### AC2 — Missing engine operations

Aeris Charts. CB4e, CB5e, CB6e, CB7e, CB8e, CB9e, CB10e, CB15e. Engine tests per operation, frame
fixtures for the volume indicator, legend rows and the footprint-owned pane (empty, first trade,
after eviction), GPUI parity for any visual change.

### P1 — Pin bump

Terminal. Bump all four `aeris_charts_*` revisions and the `tools/naming_check` expected revision.
After the bump, list engine capabilities Terminal is not yet using and add any to this file.

### TB1 — Drawing persistence and shadow series removal

Terminal. CB1t, CB2t, CB3t, plus the reader in section 5. Exercise on the desktop: draw on 1m,
switch to 5m and 1h, restart, restore; restore a workspace saved by the current build.

### TB2 — Replace host patches and internal access

Terminal. CB4t, CB5t, CB6t, CB7t, CB8t, CB13, CB15t.

### TB3 — Single catalog and typed state

Terminal. CB9t, CB10t.

### TB4 — Misplaced product code

Terminal. CB11, CB12.

### TB5 — Boundary guards

Terminal. CB14.

---

## 5. Persistence compatibility

Saved workspaces in the field contain `chart_state_json` in the current Terminal format (engine
`drawings_json` items plus `aeris_anchor_time`) and `locked_drawing_ids`. TB1 must:

1. Detect the document format: the engine document carries a schema version; the legacy format is a
   bare JSON array.
2. Convert a legacy array once into the engine document (anchors from `aeris_anchor_time`, locks from
   `locked_drawing_ids`) and import it through `import_state_json`.
3. Write only the engine document from then on and stop writing `locked_drawing_ids`.
4. Keep the legacy reader in one function with a fixture test built from a sanitized document saved
   by the current build. Removal is decided under D2.

Indicator identifiers, chart-type identifiers, crosshair numbers and price-axis numbers already
persisted keep their meaning; CB9t and CB10t change only in-memory types.

---

## 6. Decisions

| ID | Question | Options | Status |
| --- | --- | --- | --- |
| D1 | Who owns the sweep volume threshold once classification leaves the chart crate? | (a) `market_runtime` owns its own documented rule; the panel and bubbles may differ. (b) Move the adaptive rule into `domain/market_data` and have Aeris Charts accept the threshold from the host for bubbles. | **Decided:** (a). Bubbles are the Aeris Charts big-trades indicator with its own filter; the sweep rule (90th percentile of the tape's print volumes, in `order_flow.rs` today) moves with classification under CB11 |
| D2 | When can the legacy drawing reader be removed? | (a) Keep indefinitely. (b) Remove after one released version has rewritten documents. | **Open**; no release path exists yet, so (a) until one does |

---

## 7. Verification

Aeris Charts batches use that repository's complete gates. Terminal batches run focused checks while
iterating:

```text
cargo check -p aeris_chart_integration --locked
cargo test -p aeris_chart_integration --locked
cargo clippy -p aeris_chart_integration --all-targets --all-features --locked -- -D warnings
cargo test -p aeris_naming_check --locked
```

and before each Terminal commit:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Manual desktop checks per batch: open a market, switch every chart type, switch timeframes with
drawings present, toggle and remove every indicator from the legend and the menu, open the price
axis menu, switch to Footprint before and after the first live trade and confirm no candle is
drawn, restart and confirm the workspace restores identically. Record anything not exercised
in the status table instead of marking the batch complete.
