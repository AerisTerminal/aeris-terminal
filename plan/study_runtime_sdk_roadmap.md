# TradingPlot Study Runtime + Rust SDK Roadmap

## Purpose

TradingPlot is building a native Rust Study Runtime and Rust Study SDK with one product-level goal:

> Almost any Pine Script **indicator** whose required input data exists inside TradingPlot should be portable to Rust through generic TradingPlot Study SDK capabilities, without indicator-specific changes to the core product.

The target is extensive maturity, not a small built-in indicator catalog. A complex TradingView-style indicator should be able to combine multi-timeframe and cross-symbol data, stateful calculations, dynamic styling, drawings, labels, tables, chart context, alerts, and other bounded presentation semantics through the same public Rust study model.

This is a long-running architecture project, not a one-off indicator patch. The runtime must keep market ownership, execution, persistence, rendering, recovery, dependency behavior, and resource bounds correct while allowing trusted native studies to evolve independently of the desktop shell.

This roadmap is specifically about **indicators/studies**. TradingView-style strategies add broker emulation, orders, fills, positions, commissions, risk, backtesting, and performance reporting; that is a separate product/runtime program and must not be smuggled into the indicator SDK.

The intended ownership split is:

- `market_runtime` owns study scheduling, dependency execution, runtime state, market-data leases, invalidation, rollback, and publication.
- `market_engine` remains the single owner of market demand and canonical bar retention.
- `study_sdk` owns the stable author-facing native Rust study contract and built-in study registrations.
- `chart_integration` projects serial/semantic study outputs and future scene deltas into Nucleus; studies never receive render handles or Nucleus engine ownership.
- Desktop owns durable workspace configuration, product UX, restore/reinitialize/remove commands, and visibility.
- Nucleus Charts owns pane/scale/layout/geometry/rendering and the shared low-level TA formulas that Axius intentionally consumes.

## Product direction: Rust is the language; TradingPlot supplies the Pine-class host contract

TradingPlot should **not** build a Pine clone as the primary path to indicator maturity. Rust is already the authoring language. The product work should concentrate on the part Rust does not provide: market/chart execution semantics and a rich, bounded indicator host API.

Building a new language would force TradingPlot to own and mature a parser, syntax, type checker, compiler/interpreter or VM, diagnostics, formatter, language server, package/import system, debugger story, versioning rules, migration rules, sandboxing story, standard library, and long-term source compatibility. None of that improves market-data ownership, drawing semantics, table rendering, MTF requests, recovery, or indicator correctness by itself.

Rust already provides the general-purpose language layer: functions, modules, crates, structs, enums, traits, generics, iterators, collections, tests, compiler diagnostics, IDE tooling, linting, profiling, native performance, and a mature package ecosystem. TradingPlot therefore gets to solve the easier and more valuable problem: expose Pine-class **host capabilities** through stable Rust contracts.

The intended long-term stack is:

```text
Rust study code
    |
    v
TradingPlot Study SDK
    |  settings, requests, execution context, outputs, scene objects, alerts
    v
Study Runtime
    |  scheduling, state, rollback, dependency graph, bounds, recovery
    v
MarketEngine + chart integration
    |  canonical market demand/state + semantic projection
    v
Nucleus Charts
```

A future simplified TradingPlot scripting language is optional, not foundational. If product demand justifies one, it should compile or lower into the **same Study SDK/runtime model**. It must not create a second calculation engine, second persistence model, second provider-demand path, or second rendering architecture. Rust remains the full-power reference surface; a future DSL would only provide easier syntax over the same semantics.

## Definition of Pine-class indicator maturity

The goal is not syntax compatibility with Pine. The goal is capability compatibility at the host boundary. TradingPlot reaches Pine-class maturity when representative advanced TradingView indicators can be translated to Rust using only generic Study SDK primitives, with no indicator-specific edits to `market_runtime`, desktop, chart integration, or Nucleus ownership code.

The target capability families are:

| Capability family | TradingPlot target |
| --- | --- |
| Numeric/series computation | Canonical OHLCV, fixed-point access, recursive/stateful execution, hard gaps, dirty ranges, study-on-study inputs. |
| Market contexts | Same-symbol MTF, cross-symbol, sessions/time zones, lower-timeframe/intrabar data, bounded request contexts, and setting-driven dependency rebinding. |
| Live microstructure | Quotes, trades, depth, and eventually pure event-owned output timelines where the product requires them. |
| Inputs/settings | Bool, integer, decimal, text, choice plus semantic Color, Symbol, Timeframe, Session, Timestamp/Time, Price, and Source/Study-output inputs. |
| Scalar plots | Line, histogram, area plus additional plot styles, per-point dynamic styling, levels, fills, gaps, offsets where supported by the host. |
| Chart styling | Per-bar candle/bar style overrides and background/style series without granting renderer ownership to studies. |
| Drawings | Bounded semantic lines/rays, boxes, polylines, line fills, labels, markers/shapes/chars/arrows, with stable study-local identities and create/update/delete semantics. |
| Tables/dashboard UI | Bounded table objects with cells, text, value formatting, colors, alignment, borders, position, and transactional updates. |
| Execution context | Historical/realtime/confirmed/new-bar state, exchange/session metadata, bar index/time boundaries, deterministic evaluation time, and explicit intrabar-persistent state where needed. |
| Chart context | Visible-range/window information and other read-only chart context that can deliberately trigger recalculation without transferring chart ownership to the study. |
| Alerts | Typed, bounded alert conditions/events emitted by studies and owned/presented by the product. |
| Durability/recovery | Exact revision restore, dependency rebinding, bounded state/output/scene memory, provider-generation fencing, and no duplicate demand. |
| Authoring/productization | Rust SDK examples, package/revision policy, compatibility corpus, diagnostics, and eventually editor/compile-reload UX. |

Not every Pine API must be copied literally. Equivalent Rust-native abstractions are preferred when they preserve the same authoring power with clearer ownership and stronger typing.

### Pine capability inventory that drives this roadmap

Compatibility planning must track the current official Pine indicator surface rather than a remembered subset of `plot()`. At minimum, periodic research/review must cover these Pine capability families and map them to an TradingPlot equivalent or an explicit non-goal:

- Execution model and bar states: historical versus realtime execution, rollback/confirmation, intrabar persistence, recalculation triggers, and repainting-sensitive time semantics.
- Chart/market data: OHLCV, symbols, timeframes, sessions, time zones, lower-timeframe data, cross-context requests, and specialized data contexts where TradingPlot has a canonical source.
- Inputs: numeric/text/bool/enum plus color, symbol, timeframe, session, time, price, and source selection.
- Plot outputs: lines, histograms/columns, areas, step/discontinuous forms, shapes/chars/arrows, levels, fills, candles/bars, background coloring, and candle/bar coloring.
- Mutable visual objects: lines, line fills, boxes, polylines, labels, and tables with bounded object lifecycles.
- Alerts and chart interaction/context.
- Collections/general programming facilities only insofar as the host must supply something Rust itself does not already provide.

Pine evolves. Before claiming a new compatibility milestone, re-check the official Pine documentation/release notes and update the compatibility corpus when a materially important indicator capability has appeared. This roadmap deliberately targets durable capability families rather than freezing a one-time list of function names.

Research baseline reviewed against TradingView's official Pine v6 documentation on 2026-09-12:

- Visual model: <https://www.tradingview.com/pine-script-docs/visuals/overview/>
- Lines, boxes, polylines, and object limits/behavior: <https://www.tradingview.com/pine-script-docs/visuals/lines-and-boxes/>
- Tables: <https://www.tradingview.com/pine-script-docs/visuals/tables/>
- Inputs: <https://www.tradingview.com/pine-script-docs/concepts/inputs/>
- Other timeframes/data and dynamic requests: <https://www.tradingview.com/pine-script-docs/concepts/other-timeframes-and-data/>
- Execution model: <https://www.tradingview.com/pine-script-docs/language/execution-model/>
- Time and visible chart range: <https://www.tradingview.com/pine-script-docs/concepts/time/>
- Sessions/time zones: <https://www.tradingview.com/pine-script-docs/concepts/sessions/>
- Alerts: <https://www.tradingview.com/pine-script-docs/concepts/alerts/>
- Resource limitations: <https://www.tradingview.com/pine-script-docs/writing/limitations/>
- Pine release notes, including new data surfaces such as 2026 footprint requests: <https://www.tradingview.com/pine-script-docs/release-notes/>

## End-to-end study lifecycle

Every new Pine-class capability must fit one ownership-correct lifecycle. The intended end-to-end path is:

1. **Authoring:** a trusted Rust package defines stable study identity/revision, typed settings, dependency/request declarations, outputs/scene capabilities, invalidation semantics, calculation code, and optional transactional state.
2. **Durable configuration:** desktop/workspace persistence stores only durable identity, revision, settings, stable dependency/source references, visibility, and other reconstructible product state. Runtime IDs, provider sessions, renderer handles, transient checkpoints, and object implementation IDs are not persisted.
3. **Restore/validation:** the trusted package registry resolves the exact implementation revision and validates settings/dependency shape without silently rewriting durable state.
4. **Resource resolution:** semantic Symbol/Timeframe/Session/Source settings resolve to canonical market/study references. Resolution failure leaves the previous authoritative configuration intact.
5. **Demand reconciliation:** `market_runtime` derives the required market leases/request contexts; `MarketEngine` remains the only owner that creates/diffs upstream provider demand and canonical retention.
6. **History/readiness:** required histories/contexts load through the existing bounded history machinery. Provider/session generations and per-series recovery state fence calculation until every required canonical dependency is ready.
7. **Execution:** `StudyRuntime` schedules the study in dependency order and supplies immutable/bounded market views, upstream study outputs, settings, execution/chart context, and a transactional candidate state/output/scene.
8. **Commit/rollback:** calculation success plus resource validation atomically commits candidate state, scalar outputs, semantic scene, and other study-owned result state. Error/panic/memory overflow discards the candidate and preserves the last committed result.
9. **Publication:** runtime emits bounded semantic changes: scalar output generations plus future scene/style/table/alert deltas. Slow or unavailable consumers must not create unbounded queues.
10. **Projection:** `chart_integration` maps semantic output/scene contracts into Nucleus-owned series/drawings/layout without giving study code renderer handles or geometry ownership.
11. **Live change/recovery:** bar revisions/appends, quote/trade/depth changes, viewport/context changes, provider reconnects, history repairs, and setting rebinds invalidate only the required study ranges/subtrees and remain generation-fenced.
12. **Removal:** runtime study removal is authoritative; durable descendant cleanup follows runtime acknowledgement, while canceled pending registrations remain hidden/non-durable and are cleaned up with bounded retry semantics.

The same lifecycle must remain true if a future DSL is added. A new authoring syntax may change step 1 only; steps 2-12 remain shared infrastructure.

### Current capability boundary versus target

| Area | Current qualified foundation | Pine-class expansion still required |
| --- | --- | --- |
| Calculation | Stateful/stateless Rust, fixed-point bars, dirty ranges, recursive checkpoints, study-on-study. | Broader helper ergonomics/library coverage as needed; no new language required. |
| Market data | Static declared bar/quote/trade/depth dependencies, MTF/cross-series graph, recovery fencing. | Semantic resource settings, request contexts, sessions/time zones, lower-TF/intrabar, bounded dynamic contexts where justified. |
| Settings | Bool/int/decimal/text/choice, groups/help/constraints/conditional UI. | Color, Symbol, Timeframe, Session, Time, Price, Source and transactional dependency rebinding. |
| Series presentation | Line/histogram/area, panes/scales, threshold regions, narrow point-style semantics. | General dynamic styling, more plot forms, levels/fills, background and candle/bar styling. |
| Drawing objects | No generic study-owned mutable scene. | Lines/rays, boxes, polylines, line fills, labels/markers and bounded transactional object deltas. |
| Tables/text UI | No generic table output. | Bounded table/cell/text/formatting/position contract. |
| Execution context | Canonical timestamps/generations and existing live invalidation. | Historical/realtime/new/confirmed, deterministic evaluation time, intrabar-persistent state, richer session/bar metadata. |
| Chart context | Study outputs project to chart panes/scales. | Read-only visible-range/window context and deliberate recalculation triggers. |
| Alerts | Not a first-class Study SDK result. | Stable typed alert conditions/events with product-owned delivery. |
| Trust/distribution | Reviewed statically linked trusted Rust packages. | Better Rust editor/compile-reload UX; separate sandbox only if arbitrary untrusted code becomes a requirement. |

## Current completion state

The runtime/SDK **foundation through Phase E is implemented and qualified**. It proves the ownership model, transactional native execution, persistence, recovery, MTF/study dependencies, bar-aligned quote/trade/depth access, settings/editor contract, scalar chart projection, and sustained bounded execution.

That foundation is not the final Pine-class product surface. The next program is to broaden the generic host contract until complex TradingView-style indicators can be ported without core changes. Dynamic drawings, tables, semantic resource inputs, richer style/output channels, broader request contexts, execution/chart context, alerts, and compatibility qualification are now first-class roadmap work rather than optional polish.

Final integrated qualification closed the residual correctness gaps found by read-only review: transitive recovery readiness through prior-study outputs, exact fixed-time internal-gap containment for non-bar events, bounded output-primary incremental mapping without retained-history timestamp materialization, runtime-first durable study removal, and bounded automatic cleanup retry for registrations canceled before acknowledgement.

### Completed: runtime ownership and dependency graph

- Native studies have durable definitions with static market/output dependencies, typed settings, output metadata, and invalidation policies.
- `StudyRuntime` owns registration, reinitialization, removal, topological dependency ordering, output identity, output generations, and bounded output memory.
- Indicator-on-indicator dependencies are supported without giving studies direct access to unrelated runtime internals.
- Runtime instance IDs are transient and are not persisted as durable workspace identity.
- Market dependencies reconcile through `MarketEngine` data leases, so studies do not create fake chart consumers or a second provider-demand registry.

### Completed: transactional native execution and per-instance state

- Trusted Rust studies support runtime-owned typed per-instance state: trivially copyable state uses the safe default constructor, while heap/shared state must provide an explicit mutation-isolated transactional clone contract.
- State construction is factory-owned and deterministic from validated settings.
- Native execution is transactional: candidate state/output is committed only after successful calculation and memory validation.
- Calculation errors, panics, clone failures, and state-memory-accounting failures leave the last committed state/output intact.
- Reinitialization uses subtree checkpoints and exact rollback, including downstream state/output restoration.
- Per-study and total runtime-state memory caps are enforced.
- Covering recalculation creates fresh state; incremental ranges clone committed state before execution.

### Completed: canonical bar inputs and live dirty-range execution

- Bar studies receive zero-copy fixed-point views over canonical `MarketEngine` snapshots.
- Study authors can select OHLCV fields without converting the entire history to `f64`.
- Live bar append/revision maps by exact exchange timestamp rather than assuming row identity across timeframes.
- Multi-timeframe secondary-input changes invalidate the correct primary timeline conservatively.
- Dirty ranges propagate through dependent outputs in registration/dependency order.
- Initial historical hydration and ranged historical repair share the same runtime; ranged repairs execute from the actual provider-returned timestamp span so committed checkpoints/state are reused instead of forcing a covering rebuild.
- Incremental output preparation structurally shares unchanged timeline/value storage, so one-row tail work is bounded by dirty rows and outputs rather than retained history length.
- Output-primary incremental dirty mapping binary-searches the producer's canonical timeline directly; it does not materialize an O(history) timestamp vector merely because the primary dependency is another study output.

### Completed: quote, trade, and depth SDK inputs

- Native studies can declare Quotes, Trades, and Depth through the same `StudyMarketInput` stream requirements.
- Calculations receive borrowed current quote state, bounded retained aggressor trades, and direct canonical depth iteration.
- Depth is not cloned merely to execute a study.
- Non-bar market events invalidate only the actual containing live bar row by event time. Fixed-time internal gaps do not map to the previous bar, and pre-first/post-tail/out-of-coverage timestamps are ignored.
- Borrowed quote/trade/depth state is fenced by the authoritative provider generation, provider health, exact series handoff generation, and `Ready` live-history state.
- A non-bar execution wave is additionally fenced by every reachable market dependency in the study graph, including market ancestry reached through `StudyDependency::Output` producer chains. A ready quote/trade/depth secondary therefore cannot execute a consumer against stale output from a recovering upstream bar study.
- Rithmic classifies a series into recovery before exposing the recovery-triggering trade to non-bar study execution, closing the one-event stale-bar race while keeping provider-session ownership unchanged.
- Provider demand remains stream-exact; Hyperliquid quote/BBO demand is separated from L2 depth demand so a quote-only study does not cause unnecessary depth subscriptions.

### Completed: durability and desktop lifecycle

- Workspace state persists durable study identity, implementation revision, typed settings, dependencies, visibility, and stable output identifiers.
- Current-chart, explicit-market-series, and prior-study-output dependencies are persisted without runtime IDs becoming durable identifiers.
- Legacy WMA/Bollinger/SMA persistence migrates to the runtime-managed durable study model without duplicate Nucleus execution.
- Desktop registers, reinitializes, removes, restores, and generation-fences runtime study work through the existing market worker command lane.
- Changing the chart's selected series reinitializes current-chart study dependencies instead of registering a parallel study.
- Study-legend removal is a host request. Queueing a manual `RemoveStudy` does not remove durable workspace state; the root and its durable descendants remain persisted until authoritative `StudyRemoved` arrives.
- Authoritative `StudyRemoved` applies durable local-ID dependency closure across active, pending, and deferred descendants, then marks persistence dirty. `StudyRemovalFailed` leaves the manual study graph durable and retryable rather than advancing persistence ahead of runtime truth.
- A pending registration canceled before acknowledgement is a different lifecycle: it remains excluded from presentation/count/persistence, retains automatic cancellation intent after registration, retries bounded `RemoveStudy` after queue backpressure/disconnect or `StudyRemovalFailed`, and never emits duplicate in-flight remove commands. Authoritative `StudyRemoved` clears that intent.
- Dependency-chain rebind keeps presentation suppressed until each study's own reinitialization invalidation completes.

### Completed: rendering boundary

- Runtime study outputs are serial scalar series with explicit plot, pane, and scale metadata.
- Line, histogram, and area outputs project into Nucleus-owned chart series.
- Hard gaps remain `None` rather than being silently bridged.
- Sub-second timestamps are rejected at the current Nucleus scalar boundary rather than truncated.
- Multi-output study visibility is owned at the study level, so Bollinger-style outputs hide/show together.

### Completed: initial shared TA proof

- Built-in SMA, WMA, and Bollinger registrations use the same native SDK/runtime contract exposed to external trusted Rust studies.
- WMA and Bollinger delegate formula work to pinned `nucleuscharts_indicators` instead of duplicating formula implementations in Axius.
- Window/gap behavior and output contracts have focused tests.

## Completed foundation and Pine-class expansion

### Completed: first-class study settings schema and desktop editor

The settings contract now carries presentation metadata without exposing formula internals, and the desktop owns one generic transactional editor over the same durable/runtime study identity.

Completed additions:

- Human-readable label/title per setting.
- Optional description/help text.
- Setting groups/sections.
- Choice option labels and stable choice identifiers.
- Numeric constraints where appropriate: min/max/step for integer and exact decimal values.
- Conditional visibility/enabling for settings that only apply when another option is selected.
- Stable ordering.
- Product-owned generic editor that edits durable settings, validates through the SDK registration, and reinitializes the same runtime study instance transactionally.
- Explicit reset-to-default behavior.
- Tests that failed setting edits preserve the previous durable/runtime configuration.

### Completed — recursive EMA without O(history) conversion

The recursive EMA implementation is complete across the local sibling Nucleus and TradingPlot trees. Nucleus now owns a host-neutral indexed optional-sample EMA state that reuses its existing private recurrence and sparse checkpoints. TradingPlot wraps that state in `NativeStudyState`, converts only visited fixed-point market rows, reads output-backed `Option<f64>` samples directly, and routes the desktop EMA picker through the same durable Study SDK/runtime path as the other migrated built-ins.

The production dependency is pinned to Nucleus `e9ab7bc12a14d0e0dbcb0c149f6df3797dc8d35a`, which contains the reviewed indexed EMA/ATR/VWAP/RSI/MACD/Stochastic states, copy-on-write sparse checkpoints, and the renderer-neutral oscillator presentation primitives used by TradingPlot.

Do **not** copy Nucleus private EMA recurrence/checkpoint logic into Axius.

The preferred narrow Nucleus addition is an indexed optional-sample API roughly shaped as:

```rust
pub struct IncrementalEmaState { /* private */ }

impl IncrementalEmaState {
    pub fn new(period: NonZeroUsize) -> Self;

    pub fn rebuild_from_indexed<S, W>(
        &mut self,
        len: usize,
        from: usize,
        sample_at: S,
        write: W,
    )
    where
        S: FnMut(usize) -> Option<f64>,
        W: FnMut(usize, Option<f64>);

    pub fn runtime_bytes(&self) -> usize;
    pub fn last_work_rows(&self) -> usize;
}
```

Expected semantics:

- `Some(value)` participates in normal SMA-seeded EMA.
- `None` is a hard gap: output `None`, reset the recursive accumulator, and require a fresh seed run.
- Sparse recursive checkpoints remain Nucleus-owned.
- Same-tail revision and live append remain bounded incremental work.
- Historical repair replays from the nearest checkpoint rather than rescanning full history.
- Market-backed input converts only visited fixed-point rows; output-backed input reads `Option<f64>` directly.

### Product-driven built-ins through shared primitives

Every shipping picker study that belongs in the Study Runtime has been migrated through the SDK/runtime path. Additional built-ins should be added only for concrete product use, and shared Nucleus primitives remain the required formula source when they exist.

- Completed shipping/runtime-managed families: SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, session VWAP, RSI, MACD, and Stochastic.
- Deferred until concrete demand: RMA/SMMA if a shared primitive and product requirement exist.
- Deferred until semantics are defined: anchored VWAP variants.
- Deferred until a concrete product study exists: order-flow / quote / depth studies beyond the supported bar-aligned input contract.

For every built-in, avoid a second formula implementation in the desktop or runtime.

### Planned expansion — pure event-driven non-bar study timelines

Current quote/trade/depth inputs are exposed to studies whose output timeline is anchored to an existing bar/study dependency. Remaining capability:

- Explicit trade-driven output timelines.
- Quote/BBO-driven timelines.
- Depth-event-driven timelines.
- Defined retention, coalescing, timestamp ordering, and output bounds for those timelines.
- Durable dependency semantics that do not overload `BarSeriesKey` with non-bar identity.

This remains lower priority than the broader Pine-class host surface, but it is part of the maturity target for order-flow/microstructure studies. Add it through bounded event timelines with explicit retention/coalescing/order semantics; do not create an unbounded generic event engine.

### Planned expansion — Pine-class presentation and semantic scene

Current outputs cover scalar line/histogram/area series plus fixed oscillator threshold regions. That is only the first presentation layer. The Pine-class target requires generic, renderer-neutral semantics for:

- Bands/fills between two outputs.
- Marker/shape/char/arrow events.
- Richer background/band regions and dynamic background styling beyond the shipped fixed oscillator threshold-region contract.
- Semantic price levels.
- Per-point dynamic color/style channels instead of indicator-specific styling modes.
- Per-bar candle/bar style overrides.
- Plot variants such as step/discontinuous/column/circle/cross where product semantics justify them.
- Bounded mutable line/ray, box, polyline, line-fill, and label objects with stable study-local IDs.
- Bounded table/dashboard objects and cells.
- Text/value formatting metadata and semantic positioning.

These must remain serial/semantic product-owned output contracts. A study must never receive direct Nucleus/GPUI render handles. Mutable-looking author semantics must be implemented as runtime-owned transactional scene state with bounded create/update/delete deltas to the chart host.

The representative acceptance case is the supplied **Kristjan Suite R6 (KSR6)** style of study. A correct generic SDK should be able to express its four moving-average overlays, pivot/HH-HL state machine, support/resistance line lifecycle and styles, trend candle coloring, same-symbol daily ADR context, cross-symbol relative-strength context, market-timer MTF context, projected relative volume, semantic Color/Symbol/Timeframe settings, and bottom-right table without KSR6-specific product code.

### Planned expansion — semantic resource inputs and request contexts

The current Boolean/Integer/Decimal/Text/Choice setting model is a sound typed base, but Pine-class indicators need settings whose values participate in dependency construction and chart semantics rather than behaving as plain strings.

Add first-class durable setting/control types for:

- Color.
- Symbol/instrument selection.
- Timeframe.
- Session/time zone where the host can represent it correctly.
- Timestamp/time and price inputs.
- Source/study-output references where a study intentionally consumes another study's output.

Changing a resource setting must transactionally resolve and rebind the authoritative study dependency graph. A Symbol or Timeframe setting must never cause study code or desktop code to create provider sessions directly; it resolves to `MarketEngine`-owned demand through the same study lease machinery.

Introduce a bounded `StudyRequestContext`-style contract capable of representing, incrementally and in phases:

- Current chart/symbol context.
- Explicit same-symbol MTF context.
- Cross-symbol context.
- Session/time-zone interpretation.
- Confirmation/gap semantics where needed for reproducible MTF behavior.
- Lower-timeframe/intrabar windows with explicit bounds.
- Eventually bounded dynamic requests if compatibility evidence shows static/setting-resolved requests are insufficient.

Do not copy Pine's exact syntax or implicit behavior when a stronger typed Rust contract is clearer. Preserve equivalent capability and deterministic ownership instead.

### Planned expansion — execution, chart context, alerts, and intrabar semantics

Expose the runtime context advanced indicators need without allowing arbitrary wall-clock or chart ownership leakage:

- Historical versus realtime execution.
- New-bar and confirmed-bar state.
- Bar index, exchange timestamp boundaries, and relevant session metadata.
- Deterministic runtime-supplied evaluation time for calculations such as projected relative volume; studies should not call arbitrary wall-clock time as their market semantics.
- Explicit intrabar-persistent state when a study deliberately needs Pine-like `varip` behavior, distinct from normally transactional/confirmed state.
- Read-only visible-range/chart-window context for visible-range studies, with deliberate bounded recalculation semantics.
- Typed alert conditions/events routed through product-owned alert delivery.

### Planned expansion — advanced data where TradingPlot has real sources

Pine-class maturity means the SDK should not be structurally blocked from advanced indicator categories, but TradingPlot must never pretend data exists when providers do not supply it. Add generic contracts only when there is a legitimate canonical source for the data, for example:

- Lower-timeframe/intrabar arrays.
- Volume profile/footprint/order-flow data.
- Corporate-action/fundamental/economic/currency contexts if TradingPlot later owns trustworthy sources for them.

Provider-specific handles or payloads must not leak into study APIs. Canonicalize data at the owning market/data boundary first.

### Completed — authoring, versioning, and compatibility policy

The approved static-native product surface now defines:

- SDK source semver plus a separate durable compatibility epoch.
- Explicit implementation-revision restoration rules for persisted workspaces.
- A statically linked, bounded, signed-build trust model with no arbitrary native-library loading.
- Transactional failure isolation, panic containment, and bounded runtime/state/output accounting.
- Author examples for stateless, stateful, multi-output, MTF, and market-microstructure studies.
- No provider/account credentials or render/runtime owner handles in the supported study API.

### Completed — performance and soak coverage

- Real Nucleus-backed EMA tail work is measured under an explicit optimized release soak.
- Runtime-level large-history qualification proves append/revision preparation work stays proportional to dirty rows and output count while prior immutable output snapshots remain stable.
- A 16,384-row indicator-on-indicator/MTF regression proves output-primary incremental mapping does not materialize the producer's retained-history timestamp vector and prepares only the changed tail row.
- Sixteen concurrent stateful studies run under sustained revisions while holding one shared `MarketEngine` lease.
- Repeated reinitialization and historical repair keep state/output accounting bounded.
- Quote/trade/depth study execution is burst-qualified, and provider-generation replacement preserves the exact non-bar stream union without duplicate leases.
- One native-study failure cannot starve unrelated ready studies; its dependent subtree is fenced while already successful independent outputs remain publishable in deterministic order.
- Workspace close/reopen preserves durable custom-study graphs; unavailable packages do not block unrelated restore, and runtime/chart ownership remains singular.

### Qualification status

The final combined tree was qualified with the repository-pinned toolchain and locked dependencies:

- `cargo fmt --all -- --check` — pass.
- `git diff --check` — pass apart from local LF-to-CRLF conversion warnings emitted by Git on Windows.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — pass.
- `tradingplot_market_runtime` — 112 tests passed, 4 intentional release-soak tests ignored in the normal debug suite.
- `tradingplot_desktop` — 66 library tests plus 209 main-target tests passed; the intentional live Hyperliquid test remains ignored in the normal suite.
- `tradingplot_study_sdk` — 21 unit tests plus 3 SDK-facade integration tests passed; its explicit release soak remains ignored in the normal suite.
- Explicit optimized release soaks pass for 10,000,000 one-row EMA revisions, 16 concurrent stateful studies over one shared engine lease, 2,000 reinitialize/historical-repair cycles, and 50,000 bar-aligned quote/trade/depth events.

The repository-wide `cargo test --workspace --all-features --locked --no-fail-fast` run has no Study Runtime/SDK failure. It still reports two `tradingplot_chart_integration` theme assertions outside this roadmap: `nucleus_theme_switch_is_atomic_for_data_viewport_drawings_and_indicators` and `platform_default_grid_color_tracks_theme_but_custom_grid_color_does_not`, both observing `#262626` where those tests expect `#f1f1f1`. The Study Runtime changes do not modify the theme path, so those failures are tracked separately rather than weakening or misrepresenting this roadmap's qualification.

This qualification is source/runtime qualification, not a production deployment claim. No release was published or installed as part of this roadmap completion, and no credentialed Rithmic live session was used as evidence for these Study Runtime/SDK completion claims. Provider recovery and generation statements above are backed by deterministic owner-boundary/runtime tests unless a separate live-market gate is explicitly cited.

## Execution status

### Phase A — SDK contract completion

- [x] Typed durable setting values.
- [x] Static typed dependencies.
- [x] Serializable output metadata.
- [x] Per-instance transactional native state.
- [x] Canonical bar inputs.
- [x] Borrowed quote/trade/depth inputs.
- [x] Add setting labels, descriptions, groups, options, constraints, and conditional presentation metadata.
- [x] Add generic desktop study-settings editor and transactional durable/runtime reinitialization.
- [x] Add reset-to-default and validation/error UX.

### Phase B — recursive indicator proof

- [x] Land/review the narrow indexed optional-sample EMA API in Nucleus.
- [x] Wrap Nucleus incremental EMA state in Axius-owned `NativeStudyState`.
- [x] Prove market-backed fixed-point EMA without O(history) conversion.
- [x] Prove output-backed EMA with `Option<f64>` hard gaps.
- [x] Add live append, same-tail revision, historical repair, rollback, and memory-bound tests.
- [x] Migrate built-in EMA through the same SDK registration surface.

### Phase C — production indicator library

- [x] Prioritize indicators by product use rather than breadth alone.
  - First production slice: EMA Ribbon, ATR, and session VWAP because the existing scalar output contract preserves their shipping presentation without introducing a second formula path.
  - RSI/Stochastic/MACD follow only with their existing threshold-band / histogram semantics preserved through product-owned richer output metadata.
- [x] Reuse Nucleus/shared primitives for every migrated formula that already exists.
  - [x] EMA Ribbon uses five Nucleus `IncrementalEmaState` instances.
  - [x] ATR and session VWAP use Nucleus-owned indexed sparse-checkpoint states; Axius lazily converts only replayed fixed-point rows.
  - [x] RSI/MACD/Stochastic use owner-correct indexed Nucleus states with hard-gap reset, bounded tail work, and checkpointed historical repair; recurrence logic is not copied into Axius.
- [x] Add durable implementation revisions and migration tests per migrated built-in.
  - [x] SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, and VWAP have exact revision binding plus legacy-picker migration coverage.
  - [x] RSI/MACD/Stochastic have the same exact revision binding, durable restore, unsupported-revision rejection, and legacy-picker migration contract.
- [x] Add multi-output/pane/scale integration coverage where needed.
  - [x] EMA Ribbon proves five stable price-pane outputs; Bollinger proves multi-output price-pane projection; ATR proves a dedicated primary-scale pane.
  - [x] RSI/Stochastic preserve their threshold channels and dotted boundaries; MACD preserves its four-state momentum histogram palette; multi-output studies keep one grouped legend row and shared visibility/settings controls.

### Phase D — non-bar studies and richer outputs

- [x] Declare provider Quotes/Trades/Depth through Study market leases.
- [x] Expose current canonical non-bar state as borrowed execution views.
- [x] Recalculate bar-aligned studies from intrabar non-bar events with exact containing-row semantics, fixed-time internal-gap rejection, provider/session/series recovery fencing, and transitive market-readiness through prior-study output ancestry.
- [ ] Add bounded pure non-bar timelines for concrete order-flow/microstructure studies.
- [x] Add richer scalar presentation only behind concrete product requirements: fixed oscillator threshold regions and momentum-histogram state/color semantics are serial host contracts rendered by Nucleus.
- [ ] Superseded by Phases F-G: generalize presentation and semantic scene outputs rather than adding indicator-specific rendering exceptions.

### Phase E — SDK productization

- [x] Define native study packaging/loading trust model.
  - Approved external native studies are statically linked into the signed product build and listed in one immutable bounded product allowlist; TradingPlot does not discover or load arbitrary native libraries at runtime.
  - This is a source/dependency review trust boundary, not a sandbox: untrusted/user-installable native code would require a separate sandboxed architecture.
- [x] Define SDK compatibility and implementation-revision migration policy.
  - Source/API compatibility follows the Study SDK crate version; a separate SDK compatibility epoch fences incompatible durable host/package contracts.
  - Persisted implementation revisions resolve explicitly. Packages may not silently rewrite persisted dependency graphs/settings; incompatible revisions remain durable and fail restore until a compatible signed build is present.
- [x] Add author documentation and examples.
  - The Study SDK README documents trust, versioning, revision, failure-isolation, bounded-resource, and approval rules.
  - Compiling examples cover stateless, stateful, multi-output, multi-timeframe, and market-microstructure studies using only the SDK facade.
- [x] Add sustained performance/soak qualification.
  - Explicit release-only soaks pass on the final combined tree: the real Nucleus-backed EMA adapter across 10,000,000 one-row tail revisions, 16 concurrent stateful studies over one real shared `MarketEngine` lease across 20,000 tail revisions, 2,000 reinitialize + historical-repair cycles, and 50,000 bar-aligned quote/trade/depth events while asserting bounded output/state accounting and demand.
- [x] Add composed workspace restore/rebind and provider-reconnect qualification for representative native studies.
  - Workspace-file round trips preserve unavailable external package state; product-registry restore and current-series rebind preserve durable identity/output contracts; a missing package cannot starve unrelated study restore.
  - Shipping provider capabilities expose the already-implemented quote/BBO paths alongside bars/trades/depth. A newer provider session preserves registered bar-only and quote/trade/depth native studies, their exact stream demand, and a single shared engine lease without duplicate demand. Per-series recovery and transitive producer ancestry fence non-bar study execution until every required canonical bar dependency is `Ready`. These persistence, resolver, and runtime recovery tests deliberately cover their owning boundaries rather than pretending one desktop test owns provider recovery.

### Phase F — generalized visual series and style channels

- [ ] Replace narrow indicator-specific point-style modes with generic bounded per-point style/color metadata.
- [ ] Add semantic plot variants needed by representative Pine indicators: step/discontinuous lines, columns, circles/crosses and other low-cost series forms where Nucleus can own rendering cleanly.
- [ ] Add generic semantic levels and fill-between-series contracts.
- [ ] Add dynamic background styling and per-bar candle/bar style override series.
- [ ] Preserve hard gaps, pane/scale ownership, visibility, value-label behavior, timestamp validation, and bounded point accounting across every new output family.
- [ ] Ensure existing SMA/EMA/RSI/MACD/Stochastic/etc. can migrate to generic style channels without special-case presentation logic.

### Phase G — transactional semantic drawing scene

- [ ] Add bounded study-local semantic object identities independent of Nucleus object IDs.
- [ ] Add line/ray objects with endpoints, extension, style, width, color, visibility, and create/update/delete semantics.
- [ ] Add boxes, polylines, line fills, labels/text/markers, and semantic positioning.
- [ ] Add bounded table/dashboard objects with cells, formatting, text/background colors, alignment, borders, and product-owned positions.
- [ ] Make scene mutation transactional with study state/output: failed/panicking calculation cannot partially mutate committed drawings/tables.
- [ ] Publish compact bounded object deltas (`Create`/`Update`/`Delete`) rather than full-scene copies on every live update.
- [ ] Define per-study and global object/table/cell/text-memory limits plus deterministic overflow behavior.
- [ ] Project scene objects through `chart_integration` without exposing Nucleus/GPUI handles to studies.

### Phase H — semantic settings and transactional dependency rebinding

- [ ] Add first-class Color setting/control.
- [ ] Add Symbol/Instrument setting/control resolved through product instrument selection and canonical identity.
- [ ] Add Timeframe setting/control.
- [ ] Add Session/Time-zone, Timestamp/Time, and Price controls where product semantics are defined.
- [ ] Add Source/Study-output references without persisting transient runtime IDs.
- [ ] Make resource-setting changes validate, resolve, acquire/reconcile new `MarketEngine` demand, generation-fence new dependencies, and commit durable/runtime state transactionally.
- [ ] On resolution/authorization/provider failure, preserve the previous working study/dependency graph rather than partially rebinding it.

### Phase I — Pine-class market request and execution context

- [ ] Formalize `StudyRequestContext` semantics for current-symbol, explicit MTF, and cross-symbol calculations.
- [ ] Define reproducible gap/confirmation semantics for higher-timeframe requests; do not silently introduce lookahead/repainting behavior.
- [ ] Add bounded lower-timeframe/intrabar views where canonical data availability permits them.
- [ ] Add historical/realtime/new-bar/confirmed-bar execution metadata.
- [ ] Add deterministic runtime evaluation time, exchange/session timestamps, and relevant bar-index metadata.
- [ ] Add explicit intrabar-persistent state semantics distinct from normal transactional/confirmed state.
- [ ] Add read-only visible-range/chart-window context with bounded recalculation on viewport changes.
- [ ] Consider bounded dynamic request contexts only after compatibility-corpus evidence shows static plus setting-resolved requests are insufficient.

### Phase J — alerts and advanced data contracts

- [ ] Add typed study alert condition/event contracts with bounded payloads and stable durable identity.
- [ ] Keep alert delivery/product UX outside native calculation code; studies emit semantics, product owns notification side effects.
- [ ] Add canonical advanced data contracts only for data TradingPlot actually owns: intrabar arrays, footprint/profile/order-flow, and later corporate/fundamental/economic/currency contexts if trustworthy sources exist.
- [ ] Keep provider wire types, credentials, sessions, and raw adapter ownership outside the SDK.

### Phase K — Pine compatibility corpus and maturity qualification

Do not claim a percentage such as “95% Pine compatible” from API counting alone. Prove maturity by translating representative advanced indicators using only public SDK primitives.

- [ ] KSR6-class composite overlay: MAs, pivot state machine, dynamic S/R lines, bar coloring, MTF/cross-symbol requests, projected RVol, semantic settings, and table dashboard.
- [ ] ZigZag/pivot indicator with long-lived mutable line/label lifecycles.
- [ ] Smart-money/FVG-style study with bounded boxes, fills, labels, deletion/replacement, and session logic.
- [ ] Multi-symbol/MTF heatmap or dashboard using semantic Symbol/Timeframe inputs and tables.
- [ ] Visible-range volume/profile-style study driven by chart-window context.
- [ ] Session/opening-range study with boxes, levels, time-zone/session inputs and alerts.
- [ ] Custom candle/bar overlay exercising OHLC-style output and per-bar dynamic colors.
- [ ] Lower-timeframe/intrabar study exercising bounded sub-bar data and deterministic historical/realtime behavior.
- [ ] Label/marker-heavy event study exercising bounded object churn and scene delta efficiency.
- [ ] Order-flow/footprint study when canonical provider data exists, proving advanced market-data extensibility without provider leakage.

For every corpus port, the acceptance rule is strict: if the indicator requires an indicator-specific change in desktop, `chart_integration`, `market_runtime`, or Nucleus ownership code, first identify and implement the missing **generic primitive**, then port the indicator through that primitive. Compatibility code must not accrete as named-study exceptions.

Qualification must include:

- [ ] Historical equivalence fixtures for calculation/output semantics where a trustworthy reference result is available.
- [ ] Live append/revision/recovery/reconnect behavior.
- [ ] Workspace save/restore and settings/dependency rebind.
- [ ] Bounded state/output/scene/object/request memory.
- [ ] Slow/overloaded chart consumer behavior and bounded publication.
- [ ] Failure/panic isolation without partial state or scene commit.
- [ ] Large-history and high-object-count optimized soaks.
- [ ] No duplicate provider demand and no renderer/provider ownership leakage.

### Phase L — authoring UX and optional future DSL

- [ ] Improve Rust author ergonomics with higher-level SDK builders/macros/helpers only where they reduce boilerplate without hiding ownership or bounds.
- [ ] Add an TradingPlot Study Editor/workspace when product priority warrants it: Rust source editing, compiler diagnostics, formatting, tests, package qualification, and controlled compile/reload workflow.
- [ ] Keep the current trusted static-native package model until a separate untrusted-code execution design is deliberately built.
- [ ] If broad non-programmer scripting becomes a product requirement, design a small Pine-like/Axius-specific DSL as **syntax over the same runtime contracts**, not a second study engine.
- [ ] Any future DSL must lower to the same settings, dependencies/request contexts, execution state, semantic outputs/scene, alerts, persistence, bounds, and recovery semantics used by Rust studies.

The architecture should therefore support both of these eventually:

```text
Rust SDK ---------------------+
                              |
Future simple Axius DSL ------+--> one Study Runtime --> one MarketEngine --> one chart host
```

Rust remains the maximum-capability reference path. The future DSL, if built, exists for ease of authoring rather than to unlock capabilities that the Rust SDK/runtime cannot already express.

## Guardrails that must not regress

- `MarketEngine` remains the single market-demand owner.
- Studies do not open provider sessions or own canonical market/account state.
- The desktop does not become an alternate calculation runtime.
- Nucleus owns rendering/layout/geometry; Axius owns study orchestration and durable product semantics.
- New Pine-class capabilities are added as generic semantic SDK/runtime primitives, never as named-indicator special cases.
- Semantic scene objects use study-local identities; studies never receive Nucleus/GPUI object IDs or mutable render handles.
- Resource settings and request contexts reconcile through `MarketEngine`; dynamic authoring power must not create a second demand registry.
- State, scalar outputs, scene objects, tables/cells/text, request contexts, alerts, queues, retries, and publication all remain explicitly bounded.
- Rust SDK and any future DSL share one runtime/persistence/request/output model. A future scripting language must not introduce a parallel engine.
- Built-ins and SDK studies use one formula source when a shared Nucleus primitive exists.
- Runtime state/checkpoints are transient; durable settings/dependencies/implementation revision are the reconstruction source.
- Panics/errors/reinitialization failures cannot partially commit study state or output.
- Panics/errors/reinitialization failures also cannot partially commit future semantic scene objects, dependency rebinds, or alerts.
- Work and memory stay bounded.
- Hard gaps remain explicit.
- Sub-second time is never silently truncated.
- Recursive indicators must not perform O(history) conversion/rescan on every live update merely to fit an API.
- Wall-clock-sensitive calculations use explicit runtime context; studies must not smuggle nondeterministic system time into canonical market semantics.
- Pine compatibility means equivalent capability and deterministic semantics, not blind reproduction of Pine syntax or implementation quirks.

## Current checkpoint

The runtime foundation, generic settings declaration/editor contract, recursive-state bridge, and Phase C migration of every shipping picker study that belongs to the Study Runtime are implemented and verified across both repositories. SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, session VWAP, RSI, MACD, and Stochastic now use the same durable Study SDK/runtime path; Volume remains a native market-volume presentation rather than a formula study. Nucleus retains formula/checkpoint and render ownership; Axius retains durable/runtime orchestration and lazily converts only rows Nucleus actually replays. RSI/Stochastic threshold channels and MACD momentum-histogram styling are now expressed as serial study presentation semantics instead of legacy indicator-specific desktop paths.

Phase E is implemented and qualified for the approved static-native model. External native studies restore through one immutable product-owned package registry; durable dependencies/settings remain authoritative; missing packages preserve workspace state and do not block unrelated studies; author examples compile only against the SDK facade; transactional state candidates require mutation-isolated cloning; runtime tail output preparation structurally shares unchanged history; output-primary incremental mapping remains bounded without retained-history timestamp materialization; actual provider-returned ranged repairs reuse dirty-range execution; and independent calculation failures remain isolated.

Recovery and desktop lifecycle qualification now cover the final ownership-sensitive cases as well: stale non-bar views are fenced by provider/session/series readiness and by transitive market ancestry through study outputs; the Rithmic recovery-triggering event cannot execute against stale bars; manual study removal remains durable until runtime acknowledgement; acknowledged removal closes the durable descendant subtree; canceled-before-registration studies remain hidden/non-durable while automatic removal retries are bounded and deduplicated; and reinitialization presentation stays suppressed until each study's own invalidation completes.

Release qualification on the final combined tree verifies the real recursive EMA path plus bounded concurrent/shared-lease, reinitialization/repair, and quote/trade/depth burst workloads. Workspace Clippy, formatting, Study Runtime, Study SDK, and desktop gates are green. The only known repository-wide test failures are the two separate chart-theme assertions documented in **Qualification status** above; they are not Study Runtime/SDK regressions and are not counted as completed roadmap work.

The strategic direction is now explicit: Phases A-E are the completed **foundation**, not the end of indicator productization. Phases F-L expand that foundation into a Pine-class Rust indicator platform. The immediate focus is generic visual/style channels, transactional semantic drawings/tables, resource-aware settings and dependency rebinding, richer market/execution/chart context, alerts, and compatibility-corpus qualification.

The maturity target is intentionally ambitious: if TradingPlot has the underlying data required by an indicator, the default expectation should be that the indicator can be ported to Rust without changing core product code. Exceptions should be explainable by missing data, a deliberately unsupported product class such as strategy/broker emulation, or a clearly documented host capability that is still on this roadmap—not by arbitrary SDK limitations.

Rust is the reason this target is tractable. TradingPlot does not need to invent and mature an entire programming language before it can support advanced indicators. Rust supplies the language/compiler/tooling ecosystem; TradingPlot concentrates engineering effort on the host capabilities TradingView indicators depend on. That makes broad indicator compatibility materially easier to reach and maintain than building a Pine clone first.

The proof standard is the Phase K compatibility corpus. KSR6 is a first representative composite acceptance case, not a special-case implementation target. ZigZag/drawing-heavy, FVG/box-heavy, dashboard, visible-range, custom-candle, lower-timeframe, alert-heavy, and eventually order-flow/footprint studies must exercise the same generic primitives. When those studies can be ported through public Rust SDK APIs without named-study core changes, TradingPlot can credibly describe the platform as broadly Pine-class for indicators.

User-installable/untrusted executable studies remain a separate trust problem. If they become a product requirement, design a sandboxed/WASM/process model or other explicit isolation boundary. Do not weaken the current trusted-native contract to fake arbitrary-code safety.
