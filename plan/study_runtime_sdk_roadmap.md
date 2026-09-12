# AxiusFlow Study Runtime + Rust SDK Roadmap

## Purpose

AxiusFlow is building a native Rust Study Runtime and Rust Study SDK with one product-level goal:

> Any indicator whose required input data exists inside AxiusFlow should be implementable through the AxiusFlow Rust SDK without requiring a core-product rewrite.

This is a long-running architecture project, not a one-off indicator patch. The runtime must keep market ownership, execution, persistence, rendering, recovery, and dependency behavior correct while allowing trusted native studies to evolve independently of the desktop shell.

The intended ownership split is:

- `market_runtime` owns study scheduling, dependency execution, runtime state, market-data leases, invalidation, rollback, and publication.
- `market_engine` remains the single owner of market demand and canonical bar retention.
- `study_sdk` owns the stable author-facing native Rust study contract and built-in study registrations.
- `chart_integration` projects serial study outputs into Nucleus; studies never receive render handles or Nucleus engine ownership.
- Desktop owns durable workspace configuration, product UX, restore/reinitialize/remove commands, and visibility.
- Nucleus Charts owns pane/scale/layout/geometry/rendering and the shared low-level TA formulas that Axius intentionally consumes.

## Current completion state

The foundation is substantially implemented. The remaining work is no longer "create a study runtime"; it is to complete the SDK surface, productionize more input classes and recursive indicators, finish first-class settings UX, and harden the authoring/distribution story.

### Completed: runtime ownership and dependency graph

- Native studies have durable definitions with static market/output dependencies, typed settings, output metadata, and invalidation policies.
- `StudyRuntime` owns registration, reinitialization, removal, topological dependency ordering, output identity, output generations, and bounded output memory.
- Indicator-on-indicator dependencies are supported without giving studies direct access to unrelated runtime internals.
- Runtime instance IDs are transient and are not persisted as durable workspace identity.
- Market dependencies reconcile through `MarketEngine` data leases, so studies do not create fake chart consumers or a second provider-demand registry.

### Completed: transactional native execution and per-instance state

- Trusted Rust studies support runtime-owned, cloneable typed per-instance state.
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
- Historical hydration and live execution share the same runtime rather than using duplicate calculation paths.

### Completed: quote, trade, and depth SDK inputs

- Native studies can declare Quotes, Trades, and Depth through the same `StudyMarketInput` stream requirements.
- Calculations receive borrowed current quote state, bounded retained aggressor trades, and direct canonical depth iteration.
- Depth is not cloned merely to execute a study.
- Non-bar market events invalidate the containing live bar row by event time and propagate through the DAG.
- Provider demand remains stream-exact; Hyperliquid quote/BBO demand is separated from L2 depth demand so a quote-only study does not cause unnecessary depth subscriptions.

### Completed: durability and desktop lifecycle

- Workspace state persists durable study identity, implementation revision, typed settings, dependencies, visibility, and stable output identifiers.
- Current-chart, explicit-market-series, and prior-study-output dependencies are persisted without runtime IDs becoming durable identifiers.
- Legacy WMA/Bollinger/SMA persistence migrates to the runtime-managed durable study model without duplicate Nucleus execution.
- Desktop registers, reinitializes, removes, restores, and generation-fences runtime study work through the existing market worker command lane.
- Changing the chart's selected series reinitializes current-chart study dependencies instead of registering a parallel study.

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

## What is still left

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

### 2. Recursive EMA without O(history) conversion

The recursive EMA implementation is complete across the local sibling Nucleus and Axiusflow trees. Nucleus now owns a host-neutral indexed optional-sample EMA state that reuses its existing private recurrence and sparse checkpoints. Axiusflow wraps that state in `NativeStudyState`, converts only visited fixed-point market rows, reads output-backed `Option<f64>` samples directly, and routes the desktop EMA picker through the same durable Study SDK/runtime path as the other migrated built-ins.

The production dependency is pinned to Nucleus `feb6b0c45d448d978c1ef2a32502b2de87610722`, which contains the reviewed indexed optional-sample EMA state and copy-on-write sparse checkpoints used by Axiusflow.

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

### 3. More built-in indicators through shared primitives

After EMA proves the recursive-state contract, migrate/add indicators only through the SDK/runtime path. Likely sequence:

- EMA
- RMA/SMMA if a shared primitive exists
- RSI
- ATR / True Range family
- MACD
- Stochastic family
- VWAP/anchored variants where input semantics are defined
- Order-flow / quote / depth studies using the live input surface

For every built-in, avoid a second formula implementation in the desktop or runtime.

### 4. Pure event-driven non-bar study timelines

Current quote/trade/depth inputs are exposed to studies whose output timeline is anchored to an existing bar/study dependency. Remaining capability:

- Explicit trade-driven output timelines.
- Quote/BBO-driven timelines.
- Depth-event-driven timelines.
- Defined retention, coalescing, timestamp ordering, and output bounds for those timelines.
- Durable dependency semantics that do not overload `BarSeriesKey` with non-bar identity.

This should be added only after a concrete product study requires it; do not create a speculative generic event engine.

### 5. Richer output semantics

Current outputs cover scalar line/histogram/area series. Remaining product requirements may include:

- Bands/fills between two outputs.
- Marker/shape events.
- Threshold/background regions.
- Semantic price levels.
- Table/diagnostic values.

These must remain serial product-owned output contracts. A study must never receive direct Nucleus render handles.

### 6. Authoring, versioning, and compatibility policy

Before third-party native studies are treated as a supported product surface:

- Define SDK semver/compatibility policy.
- Define implementation revision migration rules for persisted workspaces.
- Define trusted-native loading/distribution policy.
- Define failure isolation expectations and telemetry boundaries.
- Publish minimal author examples for stateless, stateful, multi-output, MTF, and market-microstructure studies.
- Keep provider/account credentials and render/runtime owner handles inaccessible to study code.

### 7. Performance and soak coverage

- Measure sustained live recursive workloads rather than infer performance from unit tests.
- Measure many concurrent studies over shared market leases.
- Verify bounded state/output memory under repeated reinitialization and historical repair.
- Verify quote/trade/depth studies under burst traffic and provider reconnects.
- Verify desktop close/reopen restores durable study graphs without duplicate subscriptions or duplicate Nucleus series.

## Execution plan / TODO

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
- [ ] Reuse Nucleus/shared primitives for every formula that already exists.
  - [x] EMA Ribbon uses five Nucleus `IncrementalEmaState` instances.
  - [x] ATR and session VWAP use Nucleus-owned indexed sparse-checkpoint states; Axius lazily converts only replayed fixed-point rows.
  - [ ] Add equivalent owner-correct Nucleus state for the remaining recursive RSI/MACD/Stochastic migrations rather than copying recurrence into Axius.
- [ ] Add durable implementation revisions and migration tests per built-in.
  - [x] SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, and VWAP have exact revision binding plus legacy-picker migration coverage.
  - [ ] Complete the same contract for RSI/MACD/Stochastic when their richer output metadata lands.
- [ ] Add multi-output/pane/scale integration coverage where needed.
  - [x] EMA Ribbon proves five stable price-pane outputs; Bollinger proves multi-output price-pane projection; ATR proves a dedicated primary-scale pane.
  - [ ] Preserve oscillator threshold regions and MACD histogram semantics before those legacy paths are retired.

### Phase D — non-bar studies and richer outputs

- [x] Declare provider Quotes/Trades/Depth through Study market leases.
- [x] Expose current canonical non-bar state as borrowed execution views.
- [x] Recalculate bar-aligned studies from intrabar non-bar events.
- [ ] Add pure non-bar timelines only when required by a concrete study.
- [ ] Add semantic/marker/fill outputs only behind concrete product requirements.

### Phase E — SDK productization

- [ ] Define native study packaging/loading trust model.
- [ ] Define SDK compatibility and implementation-revision migration policy.
- [ ] Add author documentation and examples.
- [ ] Add sustained performance/soak qualification.
- [ ] Add end-to-end workspace restore/reconnect validation with a representative custom SDK study.

## Guardrails that must not regress

- `MarketEngine` remains the single market-demand owner.
- Studies do not open provider sessions or own canonical market/account state.
- The desktop does not become an alternate calculation runtime.
- Nucleus owns rendering/layout/geometry; Axius owns study orchestration and durable product semantics.
- Built-ins and SDK studies use one formula source when a shared Nucleus primitive exists.
- Runtime state/checkpoints are transient; durable settings/dependencies/implementation revision are the reconstruction source.
- Panics/errors/reinitialization failures cannot partially commit study state or output.
- Work and memory stay bounded.
- Hard gaps remain explicit.
- Sub-second time is never silently truncated.
- Recursive indicators must not perform O(history) conversion/rescan on every live update merely to fit an API.

## Current checkpoint

The runtime foundation, generic settings declaration/editor contract, recursive EMA proof, and first Phase C production migration are implemented and verified across both repositories. EMA Ribbon, ATR, and session VWAP now use the same durable Study SDK/runtime path as SMA/EMA/WMA/Bollinger. Nucleus retains recursive formula/checkpoint ownership; Axius retains durable/runtime orchestration and lazily converts only rows Nucleus actually replays.

The immediate next architecture milestones are:

1. Preserve the shipping RSI/Stochastic threshold regions and MACD histogram semantics through serial product-owned richer-output metadata, then migrate those remaining picker studies through the SDK/runtime path.
2. Keep pure trade/quote/depth timelines deferred until a concrete study requires one; the existing bar-aligned non-bar execution path remains the supported contract.
3. Complete Phase E with a static trusted-native packaging policy, compatibility/revision rules, author examples, sustained bounded-load qualification, and restore/reconnect validation.
