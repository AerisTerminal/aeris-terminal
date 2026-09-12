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

The planned runtime/SDK foundation through Phase E is implemented and qualified. Remaining work is intentionally demand-driven: pure non-bar output timelines, additional rich output shapes, or a sandboxed untrusted-code model should be added only when a concrete product requirement needs them.

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

### Completed: quote, trade, and depth SDK inputs

- Native studies can declare Quotes, Trades, and Depth through the same `StudyMarketInput` stream requirements.
- Calculations receive borrowed current quote state, bounded retained aggressor trades, and direct canonical depth iteration.
- Depth is not cloned merely to execute a study.
- Non-bar market events invalidate the exact containing live bar row by event time, ignore timestamps outside retained bar coverage, and propagate through the DAG according to the study invalidation policy.
- Borrowed quote/trade/depth state is fenced by the authoritative provider generation and provider health; recovery/session replacement clears or hides stale non-bar state before study execution resumes.
- Provider demand remains stream-exact; Hyperliquid quote/BBO demand is separated from L2 depth demand so a quote-only study does not cause unnecessary depth subscriptions.

### Completed: durability and desktop lifecycle

- Workspace state persists durable study identity, implementation revision, typed settings, dependencies, visibility, and stable output identifiers.
- Current-chart, explicit-market-series, and prior-study-output dependencies are persisted without runtime IDs becoming durable identifiers.
- Legacy WMA/Bollinger/SMA persistence migrates to the runtime-managed durable study model without duplicate Nucleus execution.
- Desktop registers, reinitializes, removes, restores, and generation-fences runtime study work through the existing market worker command lane.
- Changing the chart's selected series reinitializes current-chart study dependencies instead of registering a parallel study.
- Study-legend removal is a host request that removes the authoritative runtime subtree before durable desktop cleanup, including pending/deferred durable descendants that reference removed local study outputs; dependency-chain rebind keeps presentation suppressed until each study's own reinitialization invalidation completes.

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

## Completed milestones and demand-gated extensions

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

### 2. Completed — recursive EMA without O(history) conversion

The recursive EMA implementation is complete across the local sibling Nucleus and Axiusflow trees. Nucleus now owns a host-neutral indexed optional-sample EMA state that reuses its existing private recurrence and sparse checkpoints. Axiusflow wraps that state in `NativeStudyState`, converts only visited fixed-point market rows, reads output-backed `Option<f64>` samples directly, and routes the desktop EMA picker through the same durable Study SDK/runtime path as the other migrated built-ins.

The production dependency is pinned to Nucleus `0d7d0ae52760c4a71ea17e81e33ba8d2f71c07fc`, which contains the reviewed indexed EMA/ATR/VWAP/RSI/MACD/Stochastic states, copy-on-write sparse checkpoints, and the renderer-neutral oscillator presentation primitives used by Axiusflow.

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

### 3. Product-driven built-ins through shared primitives

Every shipping picker study that belongs in the Study Runtime has been migrated through the SDK/runtime path. Additional built-ins should be added only for concrete product use, and shared Nucleus primitives remain the required formula source when they exist.

- Completed shipping/runtime-managed families: SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, session VWAP, RSI, MACD, and Stochastic.
- Deferred until concrete demand: RMA/SMMA if a shared primitive and product requirement exist.
- Deferred until semantics are defined: anchored VWAP variants.
- Deferred until a concrete product study exists: order-flow / quote / depth studies beyond the supported bar-aligned input contract.

For every built-in, avoid a second formula implementation in the desktop or runtime.

### 4. Deferred — pure event-driven non-bar study timelines

Current quote/trade/depth inputs are exposed to studies whose output timeline is anchored to an existing bar/study dependency. Remaining capability:

- Explicit trade-driven output timelines.
- Quote/BBO-driven timelines.
- Depth-event-driven timelines.
- Defined retention, coalescing, timestamp ordering, and output bounds for those timelines.
- Durable dependency semantics that do not overload `BarSeriesKey` with non-bar identity.

This should be added only after a concrete product study requires it; do not create a speculative generic event engine.

### 5. Deferred — richer output semantics

Current outputs cover scalar line/histogram/area series plus fixed oscillator threshold regions. Remaining product requirements may include:

- Bands/fills between two outputs.
- Marker/shape events.
- Richer background/band regions beyond the shipped fixed oscillator threshold-region contract.
- Semantic price levels.
- Table/diagnostic values.

These must remain serial product-owned output contracts. A study must never receive direct Nucleus render handles.

### 6. Completed — authoring, versioning, and compatibility policy

The approved static-native product surface now defines:

- SDK source semver plus a separate durable compatibility epoch.
- Explicit implementation-revision restoration rules for persisted workspaces.
- A statically linked, bounded, signed-build trust model with no arbitrary native-library loading.
- Transactional failure isolation, panic containment, and bounded runtime/state/output accounting.
- Author examples for stateless, stateful, multi-output, MTF, and market-microstructure studies.
- No provider/account credentials or render/runtime owner handles in the supported study API.

### 7. Completed — performance and soak coverage

- Real Nucleus-backed EMA tail work is measured under an explicit optimized release soak.
- Runtime-level large-history qualification proves append/revision preparation work stays proportional to dirty rows and output count while prior immutable output snapshots remain stable.
- Sixteen concurrent stateful studies run under sustained revisions while holding one shared `MarketEngine` lease.
- Repeated reinitialization and historical repair keep state/output accounting bounded.
- Quote/trade/depth study execution is burst-qualified, and provider-generation replacement preserves the exact non-bar stream union without duplicate leases.
- One native-study failure cannot starve unrelated ready studies; its dependent subtree is fenced while already successful independent outputs remain publishable in deterministic order.
- Workspace close/reopen preserves durable custom-study graphs; unavailable packages do not block unrelated restore, and runtime/chart ownership remains singular.

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
- [x] Recalculate bar-aligned studies from intrabar non-bar events.
- [ ] Add pure non-bar timelines only when required by a concrete study.
- [x] Add richer scalar presentation only behind concrete product requirements: fixed oscillator threshold regions and momentum-histogram state/color semantics are serial host contracts rendered by Nucleus.
- [ ] Add future bands-between-outputs, markers, semantic levels, or table outputs only when a concrete study requires them.

### Phase E — SDK productization

- [x] Define native study packaging/loading trust model.
  - Approved external native studies are statically linked into the signed product build and listed in one immutable bounded product allowlist; Axiusflow does not discover or load arbitrary native libraries at runtime.
  - This is a source/dependency review trust boundary, not a sandbox: untrusted/user-installable native code would require a separate sandboxed architecture.
- [x] Define SDK compatibility and implementation-revision migration policy.
  - Source/API compatibility follows the Study SDK crate version; a separate SDK compatibility epoch fences incompatible durable host/package contracts.
  - Persisted implementation revisions resolve explicitly. Packages may not silently rewrite persisted dependency graphs/settings; incompatible revisions remain durable and fail restore until a compatible signed build is present.
- [x] Add author documentation and examples.
  - The Study SDK README documents trust, versioning, revision, failure-isolation, bounded-resource, and approval rules.
  - Compiling examples cover stateless, stateful, multi-output, multi-timeframe, and market-microstructure studies using only the SDK facade.
- [x] Add sustained performance/soak qualification.
  - Explicit release-only soaks measure the real Nucleus-backed EMA adapter across 10,000,000 one-row tail revisions, hold 16 concurrent stateful studies over one real shared `MarketEngine` lease across 20,000 tail revisions, run 2,000 reinitialize + historical-repair cycles, and drive 50,000 bar-aligned quote/trade/depth events while asserting bounded output/state accounting and demand.
- [x] Add composed workspace restore/rebind and provider-reconnect qualification for representative native studies.
  - Workspace-file round trips preserve unavailable external package state; product-registry restore and current-series rebind preserve durable identity/output contracts; a missing package cannot starve unrelated study restore.
  - Shipping provider capabilities expose the already-implemented quote/BBO paths alongside bars/trades/depth. A newer provider session preserves registered bar-only and quote/trade/depth native studies, their exact stream demand, and a single shared engine lease without duplicate demand. These persistence, resolver, and runtime recovery tests deliberately cover their owning boundaries rather than pretending one desktop test owns provider recovery.

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

The runtime foundation, generic settings declaration/editor contract, recursive-state bridge, and Phase C migration of every shipping picker study that belongs to the Study Runtime are implemented and verified across both repositories. SMA, EMA, EMA Ribbon, WMA, Bollinger, ATR, session VWAP, RSI, MACD, and Stochastic now use the same durable Study SDK/runtime path; Volume remains a native market-volume presentation rather than a formula study. Nucleus retains formula/checkpoint and render ownership; Axius retains durable/runtime orchestration and lazily converts only rows Nucleus actually replays. RSI/Stochastic threshold channels and MACD momentum-histogram styling are now expressed as serial study presentation semantics instead of legacy indicator-specific desktop paths.

Phase E is now implemented and qualified for the approved static-native model. External native studies restore through one immutable product-owned package registry; durable dependencies/settings remain authoritative; missing packages preserve workspace state and do not block unrelated studies; author examples compile only against the SDK facade; transactional state candidates require mutation-isolated cloning; runtime tail output preparation structurally shares unchanged history; ranged provider repairs reuse dirty-range execution; independent calculation failures remain isolated; release qualification measures the real recursive EMA path and verifies bounded concurrent/shared-lease, reinitialization/repair, and quote/trade/depth burst workloads; shipping provider capabilities expose their implemented quote paths; stale non-bar views are provider-generation/recovery fenced; and composed workspace persistence/rebind/removal coverage verifies that runtime study/lease ownership is not duplicated.

The remaining roadmap items are intentionally demand-driven rather than incomplete productization:

1. Keep pure trade/quote/depth output timelines deferred until a concrete study requires a non-bar-owned timeline; the existing bar-aligned borrowed non-bar input contract remains supported.
2. Add future bands-between-outputs, markers, semantic levels, or table outputs only when a concrete product study requires them.
3. If user-installable/untrusted study code becomes a product requirement, design a separate sandboxed execution model; the current trusted-native contract deliberately does not claim isolation from malicious in-process Rust code.
