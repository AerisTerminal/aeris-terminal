# AXIUSFLOW LOCAL ENGINE ARCHITECTURE MIGRATION SPECIFICATION

## Migration checklist status

Every numbered section is a migration task or verification gate. Its status marker appears immediately below its heading:

- `[ ]` means the section is not yet verified complete.
- `[x]` means every requirement in the section is implemented and supported by the required tests, runtime evidence, cleanup, and documentation.

Partial implementation remains unchecked. Existing desktop-owned behavior does not count as completion when the section requires engine ownership. When a task is completed, change only its marker to `[x]` and add a short evidence note with the validating test, command, or runtime result.

**Verified progress: 103 of 180 tasks complete.**

You are working on Axiusflow, a local-first professional trading platform written in Rust with GPUI.

This task is NOT:

- a feature patch,
- a quick chart fix,
- a request to add more abstraction,
- permission to build another parallel architecture,
- permission to retain every legacy path,
- permission to rewrite everything blindly.

This task is to deliberately migrate Axiusflow from an over-layered desktop-owned market-data architecture into a much simpler local client/engine architecture with one authoritative market backend.

The new architecture must preserve the primary advantages of the previous working backend model while remaining entirely local to the user's machine.

There is no Axiusflow cloud backend.

There is no required Docker deployment.

There is no remote Axiusflow market-data service.

The local machine is the backend host.

The final product consists conceptually of:

1. `axiusflow_desktop`
2. `axiusflow_engine`

`axiusflow_desktop` is the GPUI presentation process.

`axiusflow_engine` is the local market-data backend.

The engine may optionally stay running after the UI exits.

The engine may optionally start automatically with the user's OS session.

When the engine is warm, reopening Axiusflow should attach to already-running market state and render immediately from memory/local data.

When the user chooses complete shutdown, both processes must terminate cleanly.

The architecture must also support multiple workspace tabs, with multiple charts per workspace, without creating a provider session, history worker, or independent market-data stack per chart.

---

# 1. PRIMARY DESIGN PRINCIPLE

- [x] **Status: Verified complete**

Evidence (2026-08-18): the shipping dependency graph and production call paths now enforce the stated ownership split. Desktop depends on the provider-neutral engine client/protocol and presentation crates but no provider adapter, history/storage implementation, or `market_engine`; the engine application alone composes providers, canonical market state, local storage, and IPC service ownership. Repository conformance rejects UI dependencies in backend layers, backend dependencies in desktop presentation, duplicate mutable `MarketEngine` authority, and the return of transitional runtime crates.

The most important architectural rule is:

DESKTOP EXPRESSES DEMAND.

ENGINE OWNS MARKET STATE.

PROVIDERS EXECUTE PROVIDER PROTOCOLS.

STORAGE PRESERVES DATA.

UI NEVER OWNS MARKET-DATA LIFECYCLES.

Everything implemented during this migration must follow that rule.

If a design violates that sentence, reject the design.

---

# 2. CURRENT PROBLEM CONTEXT

- [ ] **Status: Not verified complete**

Axiusflow currently has approximately 80,000+ lines of platform-side Rust code while basic market-data behavior is unreliable.

The separate charting engine is not the target of this migration.

The charting engine is an independent framework-agnostic Rust library and should remain isolated.

Current platform symptoms include:

- chart remains indefinitely in Loading,
- historical data may arrive from provider but never appear,
- symbol switching may stall,
- timeframe switching may stall,
- streaming may fail to start or stop progressing,
- provider/runtime/state lifecycles have become difficult to reason about,
- multiple worker/runtime abstractions overlap,
- persistence appears capable of preventing already-valid market data from reaching the chart.

Recent forensic evidence showed a concrete Coinbase path:

history.fetch_started
→ history.fetch_completed successfully
→ history.completed_received ok=true
→ history.install_begin
→ history.persist_begin
→ history.install_failed
→ chart remains Loading

Actual candle values were observed during the forensic run.

This means this migration must not assume provider networking is the primary issue.

The platform currently appears able to obtain at least some valid provider data but can fail to deliver it through the rest of the application.

The new architecture must dramatically shorten the path between valid canonical market data and visible chart data.

---

# 3. DO NOT USE THE MIGRATION TO HIDE CURRENT BUGS

- [ ] **Status: Not verified complete**

Before deleting the current runtime path, preserve enough forensic instrumentation to answer:

- where valid bars first enter the system,
- where they are normalized,
- where they are stored in memory,
- where they are published,
- where the UI receives them,
- where the chart consumes them,
- why `history.install_failed` occurs,
- why that failure currently leaves the UI in Loading.

The migration is allowed to eliminate the faulty architecture, but it must not simply make the evidence disappear.

Create regression tests for the behavior.

The new engine must explicitly prove:

provider data
→ canonical bars
→ in-memory series installation
→ engine publication
→ IPC
→ desktop chart model
→ Origin chart rendering

A persistent storage failure must be tested separately and must not leave an already-valid chart permanently Loading.

---

# 4. TARGET PROCESS TOPOLOGY

- [x] **Status: Verified complete**

Evidence (2026-08-18): the workspace contains exactly two application packages, `axiusflow_desktop` and `axiusflow_engine`. The desktop reaches the resident engine only through `local_engine_client` plus the versioned `engine_protocol`; its manifest excludes provider, provider-history, local-history, local-storage, and `market_engine` crates. The engine manifest composes both provider adapters, `market_engine`, local storage, platform runtime, and the protocol while excluding GPUI and UI crates. Conformance tests preserve these dependency and process-topology boundaries.

The final high-level topology is:

```text
                    USER MACHINE

┌────────────────────────────────────────────────────┐
│                                                    │
│              axiusflow_engine                      │
│                                                    │
│  ProviderManager                                   │
│      ├── Coinbase                                  │
│      └── Rithmic                                   │
│                                                    │
│  MarketEngine                                      │
│      ├── DemandRegistry                            │
│      ├── SubscriptionRegistry                      │
│      ├── HistoryCoordinator                        │
│      ├── SeriesStore                               │
│      ├── DerivedSeriesCache                        │
│      ├── OrderBookRegistry                         │
│      ├── LiveRouter                                │
│      ├── HotSetManager                             │
│      ├── ResourcePolicy                            │
│      └── PublicationManager                        │
│                                                    │
│  Local Persistence                                 │
│      ├── history segments                          │
│      ├── catalog                                   │
│      ├── workspace metadata                        │
│      └── hot-set state                             │
│                                                    │
└───────────────────────┬────────────────────────────┘
                        │
                   local IPC only
                        │
┌───────────────────────▼────────────────────────────┐
│                                                    │
│             axiusflow_desktop                      │
│                                                    │
│  GPUI                                              │
│      ├── app shell                                 │
│      ├── workspace tabs                            │
│      ├── pane/layout management                    │
│      ├── chart views                               │
│      ├── DOM views                                 │
│      └── user interaction                          │
│                                                    │
│  EngineClient                                      │
│      └── IPC connection to axiusflow_engine        │
│                                                    │
│  Origin chart integration                          │
│                                                    │
└────────────────────────────────────────────────────┘
```

The market engine must not depend on GPUI.

The provider adapters must not depend on GPUI.

The history subsystem must not depend on GPUI.

The storage subsystem must not depend on GPUI.

The order-book subsystem must not depend on GPUI.

The desktop must not own provider sessions.

The desktop must not own provider history workers.

The desktop must not perform market-data persistence.

The desktop must not create Rithmic or Coinbase network sessions.

---

# 5. TARGET PROCESS OWNERSHIP

- [x] **Status: Verified complete**

Evidence (2026-08-18): desktop production sources own GPUI state, bounded engine-client bridges, application projections, and Origin integration only. The resident engine coordinator owns demand/subscription registries, provider sessions, provider history, canonical bars, live aggregation, depth reconstruction, local persistence, recovery, resource policy, and per-consumer publication. Static conformance rejects provider sockets, storage APIs, `HistoryStore`, and provider connection types from presentation layers, while runtime regressions prove shared provider work, independent consumers, disconnect cleanup, storage-failure isolation, and generation-fenced publication.

## `axiusflow_desktop`

The desktop owns:

- GPUI application lifetime,
- windows,
- keyboard/mouse input,
- tabs,
- workspace layout,
- visible panes,
- chart presentation,
- DOM presentation,
- drawing tools,
- UI selection state,
- rendering,
- IPC client connection,
- small presentation-oriented models,
- UI-side generation validation as defense in depth.

It does NOT own:

- provider sockets,
- Rithmic sessions,
- Coinbase sessions,
- provider authentication lifecycle,
- market-data caches,
- historical backfill,
- history/live handoff,
- order-book reconstruction,
- provider sequence validation,
- bar aggregation,
- persistent market history,
- subscription reference counting,
- provider reconnect policy.

---

## `axiusflow_engine`

The engine owns:

- provider connectivity,
- provider session lifecycles,
- canonical realtime market state,
- historical market data,
- provider history requests,
- local history cache,
- local persistent history,
- derived timeframe caches,
- shared subscriptions,
- order books,
- footprint source data,
- history/live handoff,
- resource policy,
- hot-set retention,
- consumer demand registry,
- publication generation,
- recovery from gaps/reconnects,
- engine diagnostics,
- optional warm lifetime after UI shutdown.

There must be only ONE authoritative answer to:

> What market-data work is currently active?

That answer comes from `axiusflow_engine`.

---

# 6. FINAL REPOSITORY SHAPE

- [x] **Status: Verified complete**

Evidence (2026-08-18): the repository has one desktop app, one engine app, provider-neutral domain/application/engine/protocol/history/storage crates, isolated provider adapters, platform/transport/observability crates, and the three UI crates. The final transitional crate identities were removed in this slice: `desktop_history` became `local_history`, `desktop_storage` became `local_storage`, and `local_engine_protocol` became `engine_protocol`. A conformance regression requires the replacement manifests and rejects restoration of the retired directories or workspace members; the deleted desktop runtime crates remain forbidden.

The final platform repository should converge toward approximately this shape.

Do not create additional crates without a demonstrated architectural reason.

```text
apps/
├── desktop/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── app.rs
│       ├── engine_client.rs
│       ├── engine_supervisor.rs
│       └── shutdown.rs
│
└── engine/
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── bootstrap.rs
        ├── service.rs
        └── shutdown.rs


crates/
├── domain/
│   ├── instruments/
│   └── market_data/
│
├── adapters/
│   ├── coinbase_market/
│   └── rithmic_protocol/
│
├── market_engine/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── engine.rs
│       ├── command.rs
│       ├── event.rs
│       ├── demand.rs
│       ├── provider_manager.rs
│       ├── subscription_registry.rs
│       ├── series_store.rs
│       ├── history.rs
│       ├── live.rs
│       ├── order_books.rs
│       ├── hot_set.rs
│       ├── resource_policy.rs
│       ├── publication.rs
│       └── recovery.rs
│
├── engine_protocol/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── codec.rs
│       ├── messages.rs
│       └── version.rs
│
├── provider_history/
│
├── local_history/
│
├── local_storage/
│
├── application/
│
├── observability/
│
├── platform_runtime/
│
├── transport/
│
└── ui/
    ├── chart_integration/
    ├── design_system/
    └── terminal_ui/
```

This represents the final conceptual structure.

Migration may temporarily retain current crate names.

However, after a new owner replaces an old owner, remove the old implementation.

Do not leave permanent:

- `legacy_*`,
- `old_*`,
- `v2_*`,
- duplicate runtime paths,
- compatibility wrappers,
- dead feature flags,
- unused provider runtimes.

---

# 7. CURRENT CRATE MIGRATION

- [x] **Status: Verified complete**

Evidence (2026-08-18): all callers, manifests, imports, scripts, tests, package identities, and lockfile entries were migrated from `desktop_history`, `desktop_storage`, and `local_engine_protocol` to `local_history`, `local_storage`, and `engine_protocol`. Public error identities were corrected to `LocalHistoryError` and `LocalStorageError`, so no backend API retains desktop ownership language. Repository conformance fails if any retired crate directory or workspace identity returns.

At migration start, the repository contained the transitional crate identities:

- `desktop_history`
- `desktop_storage`
- `local_engine_protocol`

Their deliberate migration is complete; the retired names remain here only as historical input to the completed task.

---

# 8. `desktop_market_runtime` MIGRATION

- [x] **Status: Verified complete**

At migration start, `desktop_market_runtime` contained files such as:

```text
market_worker.rs
live_market_worker.rs
rithmic_history.rs
rithmic_market_worker.rs
rithmic_series.rs
rithmic_shell.rs

live_market_worker/
    composition.rs
    conformance.rs
    diagnostics.rs
    forensic.rs
    history.rs
    lifecycle.rs
    provenance.rs
    publication.rs
```

This crate contained too many overlapping concepts.

Its execution responsibilities migrated into `market_engine`; its desktop-only presentation contracts moved beside their sole consumer.

Do NOT copy all files into `market_engine` unchanged.

Instead consolidate responsibilities.

Target mapping:

```text
market_worker.rs
live_market_worker.rs
lifecycle.rs
composition.rs
    ↓
market_engine/engine.rs
market_engine/live.rs


history.rs
rithmic_history.rs
    ↓
market_engine/history.rs
provider_history/


publication.rs
provenance.rs
    ↓
market_engine/publication.rs


diagnostics.rs
forensic.rs
    ↓
observability/
temporary diagnostic instrumentation


rithmic_market_worker.rs
rithmic_shell.rs
    ↓
DELETE after provider lifecycle is owned by ProviderManager
and provider-specific protocol lifecycle is behind rithmic_protocol


rithmic_series.rs
    ↓
either generic market_engine series logic
or rithmic_protocol if truly provider-specific
```

Migration evidence (2026-08-11): the resident engine owns provider lifecycle, history, live aggregation, catalog, depth, and persistence, while provider-specific desktop execution modules were deleted. The engine-protocol-only `rithmic_engine_client.rs` and `rithmic_engine_history.rs` live beside the desktop application, and `rithmic_series.rs` collapsed into the app's presentation series browser. The final bounded mailbox/application-model handoff and deterministic disconnected fixture moved unchanged beside their sole desktop consumer, all 23 focused regression tests passed, and the now-empty `desktop_market_runtime` package, workspace member, dependency, and lockfile entry were deleted. No second Rithmic product runtime or compatibility crate remains.

There must not remain a second Rithmic product runtime inside the generic market engine.

---

# 9. `desktop_provider_runtime` MIGRATION

- [x] **Status: Verified complete**

Current:

```text
desktop_provider_runtime/
    provider_diagnostics.rs
    session_contract.rs
```

Evidence (2026-08-11): every remaining caller and dependency was traced. The Rithmic adapter was the only production implementation and the resident engine was its only product caller, so the generation-fenced lifecycle and bounded session contract moved into `rithmic_protocol`; the desktop-era `desktop_driver.rs` module was renamed `provider_session.rs`. The redundant `ProviderFeedDiagnostics` wrapper was deleted because the lifecycle already owns generation validation and now records directly into `observability::FeedDiagnostics`. The stale standalone conformance script, workspace member, dependent manifest entries, lockfile package, and the entire `desktop_provider_runtime` crate were deleted. Focused adapter and engine suites pass with 103 adapter library tests, 11 adapter binary tests, and 36 engine library/integration tests.

This crate must be reviewed extremely critically.

The target design does NOT want:

```text
market_engine worker
    ↓
desktop provider worker
        ↓
Rithmic desktop driver
            ↓
Rithmic session
```

That is too many ownership layers.

Provider lifecycle contracts should move into the adapter boundary or `market_engine::provider_manager`.

Diagnostics move into `observability`.

If `desktop_provider_runtime` has no remaining unique responsibility after migration, DELETE THE CRATE.

Do not keep it simply because code already exists.

---

# 10. `application` CRATE

- [x] **Status: Verified complete**

Evidence (2026-08-11): every source file, public export, dependency, and workspace caller was audited. `application` contains only provider-neutral generation/provenance validation, replay snapshot and delta models, client-model state transitions, and embedded deterministic replay input. It has no threads, sockets, network worker, provider session, Tokio runtime, persistent storage, queue, or market-data worker lifetime. The misleading `stream_runtime.rs` name described only an immutable publication validator; its sole remaining consumer was an unconsumed desktop event/publication queue inside the transitional provider runtime. Both were deleted, and the later removal of the entire `desktop_provider_runtime` crate leaves no provider-runtime dependency on `application`.

`application` should remain intentionally boring.

It may contain:

- generation identifiers,
- provenance identifiers,
- replay snapshot models,
- consumer identifiers,
- application-level pure state,
- provider-neutral data model helpers.

It must NOT own:

- threads,
- sockets,
- network workers,
- provider sessions,
- Tokio runtime creation,
- persistent storage,
- market-data worker lifetime.

`application/src/stream_runtime.rs` must be reviewed.

If it owns actual execution/lifecycle, migrate that responsibility to `market_engine` and delete or simplify it.

Do not maintain a second stream runtime above `market_engine`.

---

# 11. `provider_kit`

- [x] **Status: Verified complete**

Evidence (2026-08-18): `provider_kit` remains vendor build/reference input only. It is not a Cargo workspace member and contains no Rust source or Cargo manifest; the Rithmic adapter build script reads only its pinned protobuf directory, and the standalone protocol conformance script validates that input. A repository regression prevents the directory from becoming an Axiusflow Rust/application layer.

`provider_kit` contains Rithmic vendor reference/protocol material.

It is NOT an Axiusflow application layer.

It must remain isolated.

It must not gain:

- chart code,
- engine code,
- application state,
- UI code,
- storage code,
- provider orchestration.

Treat it as vendor material/build input/reference material.

---

# 12. RITHMIC BOUNDARY

- [x] **Status: Verified complete**

Evidence (2026-08-18): Rithmic vendor protobuf generation and the private `generated` module remain confined to `crates/adapters/rithmic_protocol`; the module is not publicly exported. The adapter converts decoded trades, quotes, depth, catalog, history, timestamps, and session outcomes into canonical domain or bounded adapter contract values before the engine consumes them. Desktop, application, domain, market-engine, storage, and chart crates have no production dependency on either provider adapter. Repository conformance now fails if Rithmic protobuf escapes its adapter or an Origin/presentation layer acquires a provider implementation dependency.

Rithmic protobuf/wire types must stop inside:

```text
crates/adapters/rithmic_protocol
```

The Rithmic adapter owns:

- protobuf generation/build integration,
- Rithmic message encoding,
- Rithmic message decoding,
- WebSocket/network framing,
- authentication,
- Rithmic session state,
- Rithmic sequence semantics,
- provider-specific history requests,
- provider-specific catalog translation,
- provider-specific rate limits,
- reconnect protocol details.

Immediately after decoding, convert provider types into canonical Axiusflow domain types.

Above the adapter boundary, there must not be:

```rust
rithmic::proto::Something
```

inside:

- `market_engine`,
- `local_history`,
- `local_storage`,
- `application`,
- `chart_integration`,
- GPUI code.

The same principle applies to Coinbase-specific types.

---

# 13. CANONICAL MARKET TYPES

- [x] **Status: Verified complete**

Evidence (2026-08-18): `crates/domain/market_data` owns validated provider-neutral fixed-point trades, quotes, depth snapshots/deltas, bars, exact timestamps, periods, and `BarSeriesKey`. Series identity carries provider, canonical instrument, entitlement, exact cadence, and definition revision; hot bars carry no repeated provider strings or provider wire objects. Both provider adapters normalize decoded events at their boundaries, while engine memory, IPC conversion, application validation, and chart publication operate on canonical values.

All providers normalize into provider-neutral domain types.

Examples conceptually include:

```rust
InstrumentId
Timestamp
Price
Quantity
Trade
Quote
DepthUpdate
MarketBar
OrderBookSnapshot
ChartInterval
```

Hot-path market values should avoid unnecessary:

- repeated strings,
- repeated allocations,
- provider-specific objects,
- floating-point values where exact tick/fixed-point representation is required.

Provider symbol strings become stable canonical instrument identities at the adapter boundary.

The rest of Axiusflow should not repeatedly carry `"BTC-USD"` or Rithmic symbol strings through every hot-path object when an `InstrumentId` is sufficient.

---

# 14. `market_engine` IS THE PRIMARY OWNER

- [x] **Status: Verified complete**

Evidence (2026-08-18): one coordinator-owned `MarketEngine` contains the authoritative demand, provider, series, shared-subscription, and publication registries. `apps/engine` drives that owner and executes bounded provider/history/storage work; desktop and provider workers cannot mutate it directly. A new structural conformance check requires the cohesive owner fields and rejects `Mutex`, `RwLock`, mutable statics, or lazy lock owners anywhere in production `market_engine`, preserving message-driven single ownership rather than an `Arc<Mutex<_>>` graph.

The target `market_engine` crate owns all market-data orchestration.

It must have one authoritative mutable coordinator.

Conceptually:

```rust
struct MarketEngine {
    consumers: DemandRegistry,
    providers: ProviderManager,
    subscriptions: SubscriptionRegistry,
    series: SeriesStore,
    books: OrderBookRegistry,
    history: HistoryCoordinator,
    hot_set: HotSetManager,
    resources: ResourcePolicy,
    publications: PublicationManager,
}
```

Do not implement this exact shape merely because it appears here.

Use it as the ownership model.

The important property is that the state is cohesive and clearly owned.

Avoid a graph of:

```rust
Arc<Mutex<ProviderState>>
Arc<Mutex<HistoryState>>
Arc<Mutex<SubscriptionState>>
Arc<Mutex<ChartState>>
Arc<Mutex<CacheState>>
```

mutated by unrelated tasks.

Prefer:

ONE OWNER

+

MESSAGES/COMMANDS

+

IMMUTABLE SNAPSHOTS

where practical.

---

# 15. ENGINE COMMAND MODEL

- [x] **Status: Verified complete**

Evidence (2026-08-18): protocol v10 and `local_engine_client::EngineClient` expose a compact product-intent command surface for client attach/detach, consumer register/remove, series replacement, viewport and visibility updates, bounded event polling, provider-neutral catalog search/selection, resource mode, and complete shutdown. The resident server authorizes those commands against the attached client before forwarding typed values to `MarketService`; desktop code depends on the local client/protocol and never invokes provider implementations. Depth lifetime is derived from explicit engine stream requirements instead of adding redundant provider-specific socket commands. The typed visibility client method and authenticated two-workspace regression close the final command-surface gap.

The desktop communicates intent.

It does not invoke provider implementations.

Define a small engine command model.

Conceptually:

```rust
enum EngineCommand {
    AttachClient { ... },
    DetachClient { ... },

    RegisterConsumer { ... },
    UpdateSeriesDemand { ... },
    UpdateViewport { ... },
    UpdateVisibility { ... },
    RemoveConsumer { ... },

    RequestDepth { ... },
    ReleaseDepth { ... },

    SetResourceMode { ... },

    Shutdown { ... },
}
```

Do not make a separate command for every internal implementation detail.

The protocol communicates product intent.

---

# 16. ENGINE EVENT MODEL

- [x] **Status: Verified complete**

Evidence (2026-08-18): the engine publishes `EngineReady`, workspace/resource state, provider state, generation-qualified series state, covering snapshots, incremental tails, conflated order-book images, provider-neutral catalog results/rejections, stage-specific `DemandError`, and bounded idle replies. `engine_protocol` depends only on the shared transport and Prost and contains no SQLite operation, Rithmic wire type, cache entry, history scheduler, worker handle, or synchronization primitive. Desktop consumers validate identity, generation, continuity, and fixed-point values before converting these product events into application/chart models.

The engine sends useful state/publications.

Conceptually:

```rust
enum EngineEvent {
    EngineReady { ... },
    ProviderState { ... },

    SeriesState { ... },
    SeriesSnapshot { ... },
    SeriesUpdate { ... },

    DepthSnapshot { ... },
    DepthUpdate { ... },

    DemandError { ... },

    ResourceState { ... },
}
```

Do not expose:

- SQLite operations,
- Rithmic protobuf messages,
- cache implementation details,
- history scheduler internals,
- worker handles,
- internal locks.

IPC is a product boundary.

---

# 17. EVERY UI CONSUMER HAS A STABLE IDENTITY

- [x] **Status: Verified complete**

Evidence (2026-08-18): `ClientId`, `WorkspaceId`, `ConsumerId`, `GenerationId`, and canonical `SeriesKey` identities cross protocol v10 and map into the bounded `DemandRegistry`. The engine contains no global active symbol, interval, or chart; each consumer owns independent generation, series, viewport, visibility, and stream requirements, while shared subscriptions remain separately ref-counted. Tests cover two workspaces under one authenticated client, visibility isolation, one-chart switching/removal without mutating another, client-scoped disconnect cleanup, and twenty independent consumers across five workspace identities.

Multiple tabs and multiple charts require this architecture from the beginning.

There must NOT be one global:

```text
active_symbol
active_interval
active_chart
```

inside the engine.

Instead define concepts equivalent to:

```text
ClientId
WorkspaceId
ConsumerId
GenerationId
SeriesKey
```

A chart consumer is identified independently.

Conceptually:

```text
ConsumerId {
    workspace_id,
    chart_id,
}
```

Each chart has its own current demand.

Example:

```text
Workspace A

Chart 1 → ES 1m
Chart 2 → NQ 5m
Chart 3 → BTC-USD 15m

Workspace B

Chart 4 → ES 5m
Chart 5 → CL 1h
```

These are five independent UI consumers.

But they must NOT automatically become five independent provider sessions.

---

# 18. MULTIPLE WORKSPACE TABS

- [ ] **Status: Not verified complete**

Axiusflow must support TradingView-like multi-workspace/multi-tab behavior using GPUI.

A desktop window can contain:

```text
Workspace Tab A
Workspace Tab B
Workspace Tab C
...
```

Each workspace tab can contain multiple panes.

Each pane may contain:

- chart,
- DOM,
- future order-entry view,
- watchlist,
- other terminal components.

Workspace tabs are presentation concepts.

They do not own provider connections.

They do not create engines.

They do not create database instances.

They do not create Tokio runtimes.

---

# 19. MULTI-CHART ARCHITECTURE

- [ ] **Status: Not verified complete**

Suppose the user opens:

```text
Tab 1:
    ES 1m
    ES 5m
    NQ 1m
    NQ 15m

Tab 2:
    ES 1h
    BTC 1m
    BTC 5m
    ETH 1m
```

The engine sees eight consumers.

It does NOT necessarily create eight provider subscriptions.

Instead:

```text
Consumers
    ↓
DemandRegistry
    ↓
SubscriptionRegistry
    ↓
shared upstream data
```

For example:

```text
ES charts:
ES 1m
ES 5m
ES 1h
```

should ideally share the same underlying compatible ES realtime source.

Then:

```text
ES realtime source
        ↓
canonical ES events
        ↓
SeriesStore
   ├── 1m
   ├── 5m
   └── 1h
        ↓
three chart consumers
```

The implementation must not reconnect to Rithmic simply because a second ES chart opens.

---

# 20. SUBSCRIPTION REFERENCE COUNTING

- [x] **Status: Verified complete**

Evidence (2026-08-15): `MarketEngine` owns the bounded consumer-demand registry and publishes one immutable canonical series to every matching consumer, while the resident coordinator owns one Coinbase realtime worker and canonical handoffs keyed by `BarSeriesKey`. Twenty-chart and symbol/timeframe regressions prove one upstream generation is shared. The last-reference regression proves removing one of two matching consumers retains realtime, removing the last market consumer releases it even while an unrelated idle consumer remains registered, and the explicit markets-live policy alone may retain a detached hot handoff.

`SubscriptionRegistry` owns shared upstream demand.

Conceptually:

```text
ProviderSubscriptionKey
    provider/account
    instrument
    stream kind
```

Multiple consumers may reference one subscription.

Example:

```text
Chart A: ES
Chart B: ES
DOM:     ES

              ┌── Chart A
ES stream ────┼── Chart B
              └── DOM
```

Do not create:

```text
3 WebSockets
3 independent ES books
3 independent copies of the same stream
```

unless the provider protocol specifically requires it.

When a consumer closes:

```text
reference count -= 1
```

Only release the upstream subscription when no remaining consumer or hot-set policy needs it.

---

# 21. CHART GENERATIONS ARE PER CONSUMER

- [x] **Status: Verified complete**

Evidence (2026-08-15): `DemandRegistry` stores an independent `GenerationId` under each stable `ConsumerId`; `set_series_demand` makes a strictly newer generation authoritative synchronously, clears the prior publication, and rejects stale or conflicting equal-generation updates. Every state and snapshot carries consumer, generation, and series identity. Core and coordinator regressions prove late history remains cacheable but cannot publish to a replacement demand, rapid churn publishes only generation 7, and generation 2 becomes authoritative while generation 1 cleanup is deliberately held open.

Each chart has a generation.

Example:

```text
Chart 7 generation 101
BTC 1m

user selects 5m

Chart 7 generation 102
BTC 5m
```

Generation 101 becomes stale immediately.

Do not wait for generation 101 to finish shutting down before generation 102 becomes current.

Logical supersession is immediate.

Cleanup is asynchronous.

Any result carries:

```text
ConsumerId
GenerationId
SeriesKey
```

Before delivering it:

```rust
if result.generation != current_generation {
    discard();
}
```

A stale result must never overwrite a newer chart.

---

# 22. CANCELLATION MUST NOT BLOCK NEW WORK

- [x] **Status: Verified complete**

Evidence (2026-08-15): replacement demand first updates the authoritative generation, then removes the old waiter and sets the existing atomic cancellation token for every now-unobserved Coinbase or Rithmic history request without waiting or joining. The bounded replacement path proceeds immediately; a cancelled failure is retired silently instead of being misreported as provider recovery, while any late successful canonical result remains generation-fenced. A deterministic regression holds obsolete cleanup blocked after it observes cancellation and proves the newer demand has already returned successfully before cleanup is released and publishes only generation 2.

This is non-negotiable.

BAD:

```text
user selects 5m
↓
cancel 1m
↓
await old history request
↓
await unsubscribe
↓
await old worker shutdown
↓
start 5m
```

GOOD:

```text
user selects 5m
↓
generation increments immediately
↓
5m becomes authoritative immediately
↓
new work begins
↓
old work is cancelled/cleaned in background
↓
late old results are discarded
```

Cancellation is not allowed to create a serialization barrier between user selections.

---

# 23. ENGINE COORDINATOR MUST REMAIN RESPONSIVE

- [x] **Status: Verified complete**

Evidence (2026-08-15): the process-owned coordinator performs only bounded state transitions, nonblocking submissions, fixed-budget realtime/catalog drains, publication, and one command per tick. Blocking Coinbase and Rithmic history, realtime/provider I/O, catalog work, and storage execute on separate named workers and report completions through bounded channels. Deterministic delayed-cancellation, slow-consumer, cross-provider cancellation, queue-saturation, consumer-removal, and shutdown tests prove new demand and control remain serviceable while provider work is active or blocked.

Do not write a coordinator loop equivalent to:

```rust
while let Some(command) = rx.recv().await {
    handle_history(command).await;
}
```

if `handle_history()` can take hundreds of milliseconds or seconds.

While provider/history work is running, the coordinator must still be able to receive:

- new chart demand,
- timeframe change,
- symbol change,
- consumer removal,
- disconnect,
- provider failure,
- shutdown,
- visibility change.

Long-running work must execute independently and report results back to the owner.

The owner remains responsive.

---

# 24. ONE ENGINE ASYNC RUNTIME

- [x] **Status: Verified complete**

Evidence (2026-08-15): Axiusflow uses the specification's allowed alternative execution mechanism rather than adding Tokio: one process-owned, reference-counted `MarketRuntime` is constructed with the resident `MarketService` and owns the single coordinator plus bounded named provider/history/storage workers for its full lifetime. Every `MarketService` clone shares that runtime. The engine manifest has no async-runtime or GPUI dependency, the desktop has no `block_on`, and charts create only consumer identities and background IPC endpoints—not provider or per-request runtimes.

`axiusflow_engine` should own one long-lived asynchronous runtime/process execution environment.

Do not:

- create a Tokio runtime per chart,
- create a runtime per request,
- create a runtime per provider operation,
- invoke `block_on()` from the UI,
- couple runtime lifetime to GPUI entities.

Provider I/O belongs to the engine.

GPUI should not have to know whether provider code uses Tokio or another mechanism.

---

# 25. THREAD/EXECUTOR RESPONSIBILITIES

- [x] **Status: Verified complete**

Evidence (2026-08-15): GPUI owns input, presentation models, render preparation, and completed-snapshot installation. Blocking authenticated IPC runs on named desktop background workers. The engine coordinator exclusively owns demand, generations, live-handoff references, resource policy, bounded incremental handoff updates, and publication; named provider workers own network/authentication/heartbeat/reconnect work; separate history workers own blocking fetch/decode and provider historical transformation; and the bounded local-history worker owns encryption and disk I/O. Dependency-direction conformance prevents provider/storage implementations from entering the desktop, and responsiveness regressions exercise selection, visibility, removal, provider failure, and shutdown without synchronous GPUI work.

Conceptually:

```text
GPUI FOREGROUND
----------------
input
layout
presentation state
render preparation
small state mutations
installing completed snapshots


ENGINE COORDINATOR
------------------
consumer demand
subscription reference counts
generation state
provider orchestration
resource policy
publication decisions


PROVIDER I/O
------------
WebSockets
HTTP
authentication
heartbeats
provider protocol
reconnects


HISTORY/CPU WORKERS
-------------------
large history decode
aggregation
resampling
profile calculation
historical transformations


STORAGE WORKER
--------------
segment writes
catalog updates
encryption
disk persistence
compaction
```

No GPUI callback may synchronously wait for provider/network/storage work.

---

# 26. CRITICAL DATA PATH

- [x] **Status: Verified complete**

Evidence (2026-08-18): the shipping path is provider or retained-local input -> adapter/domain validation -> fixed-point canonical bars/book -> bounded in-memory `SeriesStore`/order-book owner -> generation-qualified engine snapshot or tail -> authenticated local IPC -> desktop application model -> `chart_integration` -> Origin Charts -> GPUI frame. Provider and storage work stay on bounded engine workers, persistence is not a publication gate, and the desktop performs only validation/projection before frame-conflated presentation. Authenticated IPC snapshot tests, desktop fixed-point/tail projection tests, Origin frame-boundary tests, and the release windowed benchmark exercise the same boundaries without a compatibility backend path.

The critical visible chart path should be:

```text
provider or local data
        ↓
validate
        ↓
canonical market values
        ↓
in-memory SeriesStore
        ↓
SeriesSnapshot
        ↓
engine publication
        ↓
local IPC
        ↓
desktop chart model
        ↓
Origin chart
        ↓
GPUI frame
```

This must remain short and easy to trace.

---

# 27. PERSISTENCE IS NOT A FIRST-PIXEL GATE

- [x] **Status: Verified complete**

Evidence (2026-08-15): provider and retained-history results are validated and installed into `MarketEngine` before the coordinator submits a bounded asynchronous `StorageRequest::Persist`. Consumers receive the covering snapshot with `Ready/PersistenceState::Pending` before persistence completion; a full or disconnected storage queue immediately changes only the independent persistence axis to `Degraded`. A deterministic storage-failure regression receives the valid snapshot, observes `Ready/Degraded`, then re-demands the same series from memory without another provider fetch.

This is one of the most important changes.

For validated market data used for visualization:

BAD:

```text
provider
↓
canonical bars
↓
encrypt
↓
write segments
↓
fsync
↓
SQLite commit
↓
cache install
↓
publish
↓
chart
```

GOOD:

```text
provider
↓
validate
↓
canonical bars
↓
install in memory
├──────────────→ publish to chart
└──────────────→ asynchronous persistence
```

Storage durability remains important.

However:

A local cache write failure must not automatically destroy valid in-memory market data.

A storage failure must produce an observable degraded state.

Example:

```text
SeriesState = Ready
PersistenceState = Degraded(error)
```

not:

```text
SeriesState = LoadingForever
```

This rule applies to visualization/history.

Future actual order state may require stronger transactional guarantees and is outside this migration.

---

# 28. HISTORY STATE MACHINE

- [x] **Status: Verified complete**

Evidence (2026-08-15): protocol v10 carries generation- and series-qualified `SeriesLoadState::{Empty, Resolving, Partial, Ready, Live, Failed, Superseded}` independently from `PersistenceState::{NotRequested, Pending, Durable, Degraded}`. The coordinator publishes `Resolving` before asynchronous disk/provider work, `Partial` for usable retained or derived history, `Ready` for covering history, `Live` only after handoff, and stage-specific `Failed` plus `DemandError` when no usable history remains. New consumer generations atomically supersede old work and stale completions cannot publish. A controlled regression holds provider history after `Resolving`, releases an explicit failure, and proves the same demand terminates as `Failed` with retryable `provider_history` context.

Replace ambiguous loading booleans with explicit state.

Conceptually:

```rust
enum SeriesLoadState {
    Empty,
    ResolvingMemory,
    ResolvingDisk,
    Partial,
    FetchingRemote,
    Ready,
    Live,
    Failed(...),
    Superseded,
}
```

You do not have to use these exact enum cases.

But there must be no unbounded generic Loading state.

Every request eventually becomes one of:

- usable,
- partial,
- empty,
- failed,
- superseded.

A storage failure cannot leave it pending forever.

A provider timeout cannot leave it pending forever.

A cancellation cannot leave it pending forever.

---

# 29. PROGRESSIVE PUBLICATION

- [x] **Status: Verified complete**

Evidence (2026-08-15): an exact memory hit publishes immediately. Compatible in-memory minutes and retained local/derived history install and publish a usable `Partial` covering snapshot before provider repair is queued; successful repair replaces it with the validated covering `Ready` snapshot, while repair failure leaves the partial snapshot usable. The deterministic retained-history regression observes local close `99` before provider work, retains it through provider failure, then observes repaired close `105`; compatible hot/cold derivation and repeated cache-hit tests cover the other progressive lookup levels.

The user should see useful data as soon as it exists.

Example:

Requested:

```text
Monday → Friday
```

Memory contains:

```text
Thursday → Friday
```

Disk contains:

```text
Tuesday → Wednesday
```

Provider is needed for:

```text
Monday
```

Do not wait for everything.

Instead:

```text
publish Thursday-Friday
↓
load disk
↓
extend with Tuesday-Wednesday
↓
fetch provider
↓
extend with Monday
```

The chart progressively becomes complete.

Do not hide already-valid data behind a spinner.

---

# 30. MEMORY / DISK / PROVIDER LOOKUP ORDER

- [x] **Status: Verified complete**

Evidence (2026-08-11): the implemented Coinbase demand path checks an exact `SeriesStore` hit, a compatible hot one-minute source, an encrypted retained derived segment, an encrypted retained native segment, a compatible encrypted native one-minute segment, and finally the provider. A compatible cold source is aggregated and retained as a derived segment before publication. Each earlier usable result publishes before later repair work and the desktop receives only the resulting state/snapshot events, not the lookup mechanism. Deterministic tests cover exact memory reuse, compatible hot and cold derivation, derived-before-native retained lookup, restart retention, disk-before-provider publication, and provider fallback.

A normal series query should conceptually use:

```text
1. hot in-memory series
2. compatible in-memory source series
3. local derived cache
4. persistent local history
5. provider for missing coverage
```

Not every query will use all levels.

The engine decides.

The chart does not know.

---

# 31. TIMEFRAME SWITCHING

- [x] **Status: Verified complete**

Evidence (2026-08-11): when BTC or ETH one-minute history is hot, a 5m, 15m, or 1h demand aggregates locally, publishes a partial covering snapshot, caches the result, and starts native provider repair without restarting the shared realtime session. Repeating the same coarser demand is an exact memory hit and performs no additional history fetch. The Phase 4 native switch proof and deterministic shared-session tests continue to cover provider-session stability.

Timeframe switching must not recreate the provider session merely because the interval changed.

Example:

```text
BTC 1m
↓
BTC 5m
```

If compatible source history already exists:

```text
source bars
↓
aggregate locally
↓
publish 5m
↓
cache derived result
```

The provider subscription remains alive when appropriate.

If a provider has native bar subscriptions, that is an adapter optimization, not a UI lifecycle.

---

# 32. TIMEFRAME DERIVATION RULE

- [x] **Status: Verified complete**

Evidence (2026-08-15): Coinbase hot and cold derivation accept only a supported target wider than one minute and always select the exact canonical one-minute series as their source; tick, session, week, month, one-minute, unsupported, or non-divisible targets bypass derivation and proceed to retained/provider lookup. A black-box regression first caches five-minute history, requests one-minute history, proves no generation-2 snapshot exists before provider completion, and verifies the one-minute request reaches the provider. Existing reciprocal coverage proves one-minute data derives and caches 5m/15m/1h only in the valid direction.

Never derive finer data from coarser data.

Example:

Monthly OHLC cannot reconstruct 1-minute bars.

Valid direction:

```text
1m → 5m
1m → 15m
1m → 1h
```

when session/calendar semantics permit.

Invalid:

```text
1M → 1m
```

The engine should choose the best compatible retained source.

If the requested timeframe requires data that is unavailable locally, fetch missing source history from the provider.

---

# 33. DERIVED SERIES CACHE

- [x] **Status: Verified complete**

Evidence (2026-08-11): derived Coinbase bars retain the full canonical `BarSeriesKey` identity and are bounded by the existing engine series/bar limits. They persist separately as `DataKind::Derived` under the account/entitlement/instrument/interval/source/schema/calendar/adjustment/correction dimensions, are searched before native retained bars, and remain bounded by the encrypted catalog limit. Tests prove derived data wins the retained lookup after restart and repeated timeframe demand reuses the hot derived series without another provider fetch.

Maintain a bounded derived series cache.

Conceptually:

```text
SeriesKey:
provider/account identity
instrument
interval
session/calendar semantics
source revision
adjustment/correction revision
```

Do not confuse native and derived data.

A repeated timeframe switch should become extremely cheap after the derived series has been created.

Example:

```text
BTC 1m loaded
↓
user opens 5m
↓
derive 5m
↓
cache

later:

5m requested
↓
cache hit
↓
publish immediately
```

---

# 34. ACTIVE BAR UPDATES

- [x] **Status: Verified complete**

Evidence (2026-08-18): `SeriesStore` retains completed history in an immutable shared allocation and stores the forming bar separately. Coinbase and Rithmic realtime aggregation publish one protocol `SeriesUpdate` for same-bucket revisions at the existing 16 ms presentation cadence; when a bucket rolls between flushes, one covering publication preserves the completed bar's final OHLCV and is immediately stored again as immutable history plus a separate forming tail. Deterministic engine tests verify replace-last behavior, contiguous append, stable Rithmic forming-bar timestamps, boundary-roll preservation, and completed-history pointer identity across revisions.

Do not repeatedly rebuild all historical bars when a new tick arrives.

For a current interval:

```text
incoming trade
↓
update current active bucket
↓
publish changed tail
```

Historical bars remain immutable unless corrections occur.

---

# 35. HISTORY/LIVE HANDOFF

- [x] **Status: Verified complete**

Evidence (2026-08-15): the engine establishes shared realtime demand before history completes. Coinbase and Rithmic retain generation-fenced bounded live buffers while history is unresolved; history installation establishes the exact canonical sequence/time boundary, seeds the provider-owned aggregator, applies only admissible buffered trades, publishes a covering snapshot, and then enters `Live`. Buffer overflow, a gap, conflicting history, stale generation, or invalid continuation forces covering recovery instead of applying an uncertain tail. Deterministic Coinbase reconnect and Rithmic fixed/tick handoff tests prove completed history remains immutable, the forming tail continues exactly, stale overlap is discarded, gaps fail closed, and consumers survive recovery without reconstruction.

History and realtime must be coordinated explicitly.

Conceptually:

```text
begin live subscription
↓
buffer/sequence new live events if needed

load history
↓
establish history watermark
↓
validate continuity
↓
publish covering snapshot
↓
apply only live events newer than watermark
↓
continue live
```

Exact behavior may differ per provider.

But:

history does not need to wait indefinitely for a vague `provider_streaming` boolean.

Historical and realtime state must be distinct.

---

# 36. PROVIDER CONNECTION STATE

- [x] **Status: Verified complete**

Evidence (2026-08-15): protocol v10 defines and transports generation-qualified `ProviderConnectionState::{Disconnected, Connecting, Online, Recovering, Failed}`, while `MarketEngine` independently retains the matching provider health and active session generation. Coinbase and Rithmic coordinator transitions update that authoritative state; stale generations are ignored, reconnect advances generation, and series readiness remains a separate `SeriesLoadState`. Deterministic regressions observe `Connecting → Online`, `Online → Recovering`, and a newer-generation recovery without ambiguous streaming booleans, including connection and live progress while persistence is degraded.

Use explicit provider connection state.

Conceptually:

```text
Disconnected
Connecting
Authenticating
Ready
Streaming
Recovering
Failed
```

Do not overload booleans like:

```text
still_streaming = true
streaming_generation = None
```

unless the semantics are unambiguous.

Diagnostic names must match what they actually mean.

---

# 37. ORDER BOOK OWNERSHIP

- [x] **Status: Verified complete**

Evidence (2026-08-11): the resident engine's Rithmic realtime worker is the sole depth subscriber. Canonical depth enters the single market coordinator, which owns one bounded top-20 `OrderBook` for the generation-fenced selected instrument. Protocol v9 publishes only the latest conflated fixed-point covering image to matching consumers. The desktop Rithmic runtime has no depth subscription or order-book state; it validates the authenticated engine image and performs presentation-only formatting before the bounded UI mailbox. Deterministic engine and desktop tests prove engine reconstruction, consumer/selection/provider generation identity, exact fixed-point levels, bounded publication, and presentation without a second candidate book.

Order books belong in `axiusflow_engine`.

Not in GPUI.

Not in a DOM widget.

Not inside individual charts.

Conceptually:

```text
Provider depth stream
↓
provider continuity validation
↓
canonical depth updates
↓
OrderBookRegistry
↓
one authoritative local book per relevant instrument/session
↓
consumer-specific publications
```

---

# 38. ORDER BOOK SEQUENCE SAFETY

- [x] **Status: Verified complete**

Evidence (2026-08-11): Rithmic's adapter-owned aggregate-book assembler validates venue update continuity and converts only completed covering images; a gap or unavailable book invalidates the provider session and enters bounded retry instead of publishing candidate depth. The engine-owned canonical `OrderBook` independently rejects stale generations, invalid/crossed images, and discontinuous deltas, clears invalid candidate levels, and exposes explicit awaiting-snapshot, sequence-gap, crossed-book, invalid-update, stale, or ready state through protocol v10. Existing deterministic adapter, domain, engine, IPC round-trip, and DOM projection tests prove fail-closed recovery and that the UI cannot retain a plausible silently corrupted book.

Depth events must preserve correctness.

If:

```text
expected sequence = 8122
received sequence = 8125
```

the local book is invalid.

Do NOT silently continue.

Trigger provider-specific recovery:

```text
gap
↓
mark book invalid
↓
request/rebuild covering snapshot
↓
resume contiguous deltas
```

The UI must never display a plausible but silently corrupted book.

---

# 39. DO NOT SEND EVERY MARKET EVENT TO GPUI

- [x] **Status: Verified complete**

Evidence (2026-08-18): provider events remain correctness-preserving inside the engine, while presentation receives only frame-cadenced same-bucket revisions or a covering image when a bar boundary must preserve more than one changed tail. Each consumer has one semantic series slot: a live tail merges into an unconsumed covering snapshot or replaces the prior pending tail. The desktop mailbox applies the same safe conflation and a deterministic regression proves 63 live revisions leave one queued update and one edge-triggered GPUI wake.

The engine may receive:

```text
5,000
20,000
50,000+
```

market updates per second.

GPUI does not need one render notification per event.

Internally preserve required correctness.

But presentation should use:

- batches,
- snapshots,
- conflated latest state,
- frame-oriented publication.

Example:

```text
20,000 depth updates
↓
book engine processes all required updates
↓
current correct book state
↓
publish latest presentation state at appropriate cadence
↓
GPUI
```

Do not create:

```text
20,000 GPUI notifications
```

for 20,000 feed events.

---

# 40. FOOTPRINT / ORDER FLOW FUTURE SUPPORT

- [ ] **Status: Not verified complete**

This architecture must support future or existing:

- footprint charts,
- bid/ask volume,
- delta,
- cumulative delta,
- volume profile,
- market profile,
- tape,
- DOM,
- historical trade reconstruction where provider data permits.

These belong to engine-side market computation.

The UI receives view-ready snapshots/deltas.

Do not couple footprint calculations directly to GPUI render callbacks.

---

# 41. MULTI-TAB RESOURCE POLICY

- [ ] **Status: Not verified complete**

Multiple workspace tabs must not force every hidden chart to consume full presentation resources.

Each consumer should have a visibility/resource priority.

Conceptually:

```text
FOREGROUND
visible chart in current tab

BACKGROUND
chart in another open tab

WARM
recent/pinned workspace demand

DETACHED
UI no longer needs live publication
```

Engine resource policy decides what to retain.

Example:

Foreground:

- full publication cadence,
- required realtime,
- visible history priority.

Background:

- retain series in RAM,
- lower UI publication cadence,
- possibly retain provider subscription depending shared demand.

Warm:

- retain recent data/history,
- optionally retain provider connection.

Detached:

- no UI publication,
- keep only if hot-set/resource policy requests it.

---

# 42. TAB SWITCHING

- [ ] **Status: Not verified complete**

Switching workspace tabs should primarily be a GPUI operation.

If data is already warm:

```text
click Tab B
↓
show existing GPUI workspace state
↓
attach/latest snapshots already available
↓
render
```

Do not reconnect providers merely because the selected UI tab changed.

Do not recreate market workers.

Do not reconstruct the engine.

---

# 43. MANY CHARTS MUST SHARE IMMUTABLE DATA

- [x] **Status: Verified complete**

Evidence (2026-08-18): canonical completed bars are held as `Arc<[MarketBar]>`, and consumer snapshots clone the shared allocation rather than its bars. The existing twenty-consumer regression proves all charts receive one provider generation and pointer-identical canonical history while retaining independent consumer publication state; chart-local application and Origin representations remain presentation-owned.

Avoid copying giant bar vectors per chart.

Prefer immutable shared snapshots where appropriate.

Conceptually:

```rust
Arc<SeriesSnapshot>
```

or equivalent.

Multiple charts displaying the same underlying series can share storage.

Chart-specific presentation transformations may remain local.

Do not clone 100,000 bars eight times because eight charts request them.

---

# 44. VIEWPORT DEMAND

- [ ] **Status: Not verified complete**

Charts express viewport demand.

Example:

```text
Chart 4:
visible range = ...
prefetch range = ...
```

The engine can prioritize visible missing coverage.

Panning should not trigger a complete series reload.

If already covered:

```text
memory → immediate
```

If partially covered:

```text
publish existing
+
fetch missing
```

Viewport changes do not recreate provider sessions.

---

# 45. HOT-SET MANAGER

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): `EngineState` keeps at most 32 scored, last-used provider/market/interval identities with optional stable viewport bounds. Selection touches recency, viewport changes publish immutable revisioned manifests, and complete shutdown freezes later persistent mutations before writing one final newer manifest. Only the latest two manifests survive. A deterministic equal-revision regression proves an older viewport manifest cannot replace a newer selected series on restart. Active-workspace, watchlist, pinning, memory-pressure, and measured HOT/WARM/COLD retention policy remain incomplete, so this section stays unchecked.

The engine retains a bounded working set.

Classify data roughly as:

```text
HOT
visible/current workspace

WARM
other workspace tabs
watchlist
recently used
explicitly pinned

COLD
everything else
```

The exact policy should be measured.

Do not warm the entire exchange catalog.

---

# 46. WARM ENGINE MODES

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): protocol v10 lifecycle commands cross the authenticated local boundary. `SetEngineResourceMode` now applies one coordinator-owned policy as well as acknowledging engine workspace state. The explicit per-launch `--keep-markets-live` mode is installed before desktop market workers start; after the last UI client detaches, it retains the bounded selected Coinbase/Rithmic handoffs, keeps provider sessions available, lets live bars/books advance in canonical engine state, and preserves in-flight Rithmic repair. A deterministic no-UI regression observes no Coinbase stop, advances the forming bar, reattaches from that advanced snapshot without another history fetch, then switches to ordinary warm mode and observes realtime release. An optimized Windows native run closed two successive markets-live desktops through `WM_CLOSE`; both left the engine alive, the second reused the first engine PID, and authenticated cleanup exited zero. `ShutdownEngine` marks the engine offline/suspended, freezes later persistent workspace mutation, rejects subsequent demand, closes the listener, writes a final revisioned hot-set manifest, cancels in-flight history and Coinbase realtime work, disconnects active Rithmic worker controls, drains accepted local-history requests, cancels and joins each Rithmic worker's native network/power helpers, joins the top-level market workers, and exits within one shared two-second process deadline. Default native desktop close preserves the warm engine, while per-launch `--exit-with-desktop` awaits detach before complete shutdown. Durable in-product lifetime-mode selection, optional autostart, provider-permission configuration, credentialed markets-live evidence, and end-to-end GPUI warm snapshot-to-render proof remain incomplete, so this section stays unchecked.

Support at least these product modes conceptually.

## Mode A — Exit Completely

Closing Axiusflow:

```text
desktop exits
↓
engine flushes bounded state
↓
provider connections close
↓
engine exits
```

Next launch is a cold application launch using persisted local state.

---

## Mode B — Keep Engine Warm

Closing desktop:

```text
desktop exits
↓
engine remains alive
↓
RAM hot-set remains
↓
local caches remain open
```

Provider connections may be released depending resource settings.

Reopening:

```text
desktop launches
↓
IPC attach
↓
engine already has hot state
↓
snapshots immediately available
```

---

## Mode C — Keep Markets Live

If the user explicitly enables it and provider rules permit:

```text
desktop exits
↓
engine remains alive
↓
provider sessions remain connected
↓
selected hot symbols remain current
↓
bars/books continue updating
```

Reopening the UI should effectively attach to an already-running trading terminal engine.

---

# 47. MACHINE REBOOT

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): engine startup no longer writes an unconditional Windows `HKCU` login-start entry. The obsolete development-machine entry created by the previous behavior was removed during verification. Optional user-controlled enable/disable policy, equivalent native behavior on every supported platform, persisted preference ownership, and reboot-to-warm validation are not implemented, so this section stays unchecked.

Do not claim state remains in RAM after machine power-off.

Instead support optional engine auto-start.

After user login/OS startup:

```text
OS starts axiusflow_engine
↓
load persisted hot-set metadata
↓
open local cache/catalog
↓
load recent relevant history
↓
reconnect permitted providers
↓
repair missing coverage
↓
warm engine
```

When the user later launches the desktop, the engine may already be ready.

---

# 48. PLATFORM SERVICE ABSTRACTION

- [ ] **Status: Not verified complete**

Add a narrow platform boundary.

Conceptually:

```text
crates/platform_runtime/src/background_service.rs
```

Responsibilities:

- determine whether engine is running,
- start engine,
- request full engine shutdown,
- configure optional per-user auto-start,
- remove auto-start,
- platform-specific lifecycle integration.

Do not spread Windows/macOS/Linux service APIs through the product.

---

# 49. ENGINE SUPERVISOR

- [ ] **Status: Not verified complete**

Desktop uses:

```text
apps/desktop/src/engine_supervisor.rs
```

Responsibilities:

- locate/connect to local engine,
- launch engine when required,
- wait only for IPC readiness, not provider readiness,
- reconnect after engine restart,
- honor user shutdown/warm preference.

It does NOT own engine market state.

---

# 50. DESKTOP IPC CLIENT

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): the blocking authenticated client, local framing, native installation-token access, sibling-engine discovery/start, typed workspace/market commands, resource-mode control, and complete shutdown now live in `crates/local_engine_client`. Desktop background workers depend on that narrow client support instead of linking `apps/engine` as a backend library, while the engine application retains server and market ownership. Both production Coinbase and Rithmic presentation bridges use it off GPUI; the latter installs its canonical instrument, submits series demand, polls covering series and order-book snapshots, and removes cancelled consumers without opening provider history or depth sockets. The engine application itself uses the same authenticated client for `--shutdown`. Focused tests prove protocol-v10 socket fencing, occupied-endpoint retry without spawning a competing engine, authenticated client/server exchanges, acknowledged resource-mode updates, and shutdown signaling. Reconnect with active-consumer restoration remains incomplete, so this section stays unchecked.

Use:

```text
apps/desktop/src/engine_client.rs
```

Responsibilities:

- establish local authenticated IPC,
- encode commands,
- decode engine events,
- route events to desktop presentation models,
- reconnect/recover connection state,
- never perform provider work.

It must run off the GPUI foreground thread where blocking operations are involved.

---

# 51. IPC MUST BE LOCAL

- [x] **Status: Verified complete**

Evidence (2026-08-15): desktop and the per-user engine communicate only through `interprocess` platform-local sockets at one fixed local endpoint; the engine binds one exclusive listener and exposes no TCP listener, HTTP route, REST API, or Docker-network dependency. Every frame carries strict protocol v10 and bounded protobuf framing, incompatible versions fail closed, and the first message must authenticate with the 32-byte installation credential loaded from native credential storage. Provider credentials never enter the local protocol. Mandatory manifest tests require the engine/client local transport and versioned protocol dependencies and reject public-server frameworks.

The engine is not an internet server.

Do not expose a public HTTP port.

Do not build a REST API.

Do not require Docker networking.

Use local IPC appropriate to the platform through existing transport/platform abstractions.

The protocol must be versioned.

It must authenticate the local user/process as appropriate.

It must not expose provider credentials.

---

# 52. IPC CONTROL PLANE MUST NOT BE STARVED

- [x] **Status: Verified complete**

Evidence (2026-08-15): market data is not pushed into the control channel. Each client session reads explicit commands and writes at most one requested response, while the resident coordinator drains each provider-event source under a fixed budget and then handles one bounded control command. Full history queues fail demand explicitly instead of blocking the coordinator; realtime overflow closes and advances the provider generation for covering recovery. An authenticated two-client regression leaves one market client dormant with pending state while a second client receives an acknowledged resource-mode change, registers independent demand, and receives its snapshot.

Control messages include:

- new chart demand,
- symbol switch,
- timeframe switch,
- chart closure,
- shutdown,
- provider credential/account changes.

These must never wait indefinitely behind a flood of market updates.

Separate control semantics from high-volume publication semantics.

Do not let a full market-data queue prevent:

```text
SetSeriesDemand(new selection)
```

from reaching the engine.

---

# 53. IPC PUBLICATION BACKPRESSURE

- [x] **Status: Verified complete**

Evidence (2026-08-15): the engine stores at most one latest provider state, covering series snapshot, series state, demand error, conflated authoritative order-book image, catalog search and catalog selection per consumer. Polling removes one semantic slot at a time; newer covering or presentation state replaces older pending state without an outbound push queue. Provider sequence and book reconstruction complete before the latest depth image enters that slot, and any provider/realtime queue loss forces generation recovery rather than silently dropping correctness-critical deltas. Desktop publication adds its own bounded generation-aware mailbox with explicit overflow fencing and covering resnapshot semantics.

Use bounded publication.

Different payloads require different semantics.

Covering snapshots:

```text
newer covering snapshot may replace older pending snapshot
```

Presentation-only latest values:

```text
latest wins
```

Non-conflatable correctness-critical state:

```text
never silently drop
```

Depth:

internal book reconstruction must preserve provider sequence correctness.

UI display publications may be coalesced only after the engine has built correct current state.

---

# 54. SLOW DESKTOP MUST NOT BLOCK ENGINE

- [x] **Status: Verified complete**

Evidence (2026-08-15): the engine never waits for GPUI to consume unsolicited market data because IPC publication is pull-based and retained in fixed semantic slots. Provider history, realtime, storage and coordinator workers have independent bounded channels and lifecycles. A deterministic slow-consumer regression leaves one consumer completely unread during a 32-trade live burst, proves another consumer receives the final trade and immediately changes visibility and generation, then proves the slow consumer has at most seven pending events and receives the latest covering snapshot rather than an event backlog.

If GPUI stops reading IPC temporarily:

The engine must not block provider sockets indefinitely.

The engine must not stop heartbeats.

The engine must not stop history processing.

The engine must not corrupt its book.

The client publisher should use bounded/latest-state behavior and recovery snapshots.

---

# 55. SERIES STORE

- [x] **Status: Verified complete**

Evidence (2026-08-18): `market_engine/src/series_store.rs` is the bounded owner of canonical completed history and separate forming tails, immutable snapshots, exact in-memory time-range queries, closest compatible finer fixed-time source selection, generation/continuity validation, correction invalidation, and bar/byte accounting. The coordinator now asks `SeriesStore` for compatible hot sources instead of constructing a Coinbase one-minute key itself; deterministic tests cover range boundaries, closest-source selection, immutable sharing, tail revision/rollover, bounds, and invalidation.

`market_engine/src/series_store.rs` owns hot canonical/derived series.

Responsibilities:

- insert canonical history,
- maintain active tails,
- answer in-memory range queries,
- return immutable snapshots,
- maintain bounded memory,
- identify compatible source intervals,
- invalidate corrected/revised data.

It does NOT perform provider networking.

It does NOT render charts.

It does NOT write directly to GPUI.

---

# 56. HISTORY COORDINATOR

- [ ] **Status: Not verified complete**

`market_engine/src/history.rs` owns orchestration only.

Responsibilities:

- inspect memory coverage,
- inspect local persistent coverage,
- determine missing ranges,
- request provider repairs,
- combine successful results,
- publish partial/complete state,
- coordinate generation/cancellation,
- coordinate live handoff.

Detailed provider paging/rate rules remain in `provider_history` and provider adapters.

Do not rebuild provider scheduling logic in this file.

---

# 57. LOCAL HISTORY

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-18): the crate and package identity are now `local_history` and `axiusflow_local_history`, its public error is `LocalHistoryError`, and desktop/UI manifests cannot depend on it. The resident production path still owns active local-history orchestration in `apps/engine/src/local_history.rs` rather than consuming the generic decoded-cache worker as a production dependency, so this section remains open pending consolidation or deletion of that remaining split.

Rename `desktop_history` conceptually to `local_history`.

It should not be desktop-owned.

Responsibilities:

- decoded bounded history cache,
- immutable stored series reads,
- range retrieval,
- validation,
- local history mechanics.

The engine consumes it.

GPUI does not.

---

# 58. LOCAL STORAGE

- [x] **Status: Verified complete**

Evidence (2026-08-18): the backend package and directory are now `axiusflow_local_storage` and `crates/local_storage`, and the public error is `LocalStorageError`. The resident engine is its production owner and uses it through the bounded local-history worker for encrypted immutable segments, keyed SQLite catalog metadata, retained coverage, crash-safe publication, restart recovery, and corrupt-segment quarantine. Desktop/UI manifests exclude the crate, and the full storage lifecycle suite remains the durable boundary test.

Rename `desktop_storage` conceptually to `local_storage`.

Responsibilities:

- market-history persistence,
- segment storage,
- catalog,
- crash-safe publication,
- encryption where required,
- corruption quarantine,
- local metadata.

Storage is a backend concern.

Do not expose its API to GPUI.

---

# 59. STORAGE FAILURE SEMANTICS

- [x] **Status: Verified complete**

Evidence (2026-08-15): `SeriesState` reports market readiness and persistence as separate protocol fields. Read, queue, encryption, segment, or catalog failures are reduced to bounded non-sensitive degraded detail at the engine boundary; they do not remove a validated in-memory publication or convert `Ready`/`Partial` into indefinite loading. The storage-failure regression proves `Ready/Degraded` remains immediately reusable from the engine cache without a second provider request, while encrypted-store lifecycle tests separately prove corruption quarantine and failure isolation.

Storage errors must be explicit.

Example:

```text
Market data state:
Ready

Persistence:
Failed(path/error)
```

The engine may retry or degrade.

But valid in-memory data remains usable.

Exceptions require explicit correctness justification.

---

# 60. OBSERVABILITY

- [ ] **Status: Not verified complete**

Keep forensic and production diagnostics.

Every end-to-end demand should be traceable using:

```text
ClientId
ConsumerId
GenerationId
ProviderSessionGeneration
SeriesKey
RequestId
```

Trace major boundaries:

```text
desktop.command_sent
engine.command_received
engine.memory_lookup
engine.disk_lookup
engine.provider_request_started
engine.provider_request_completed
engine.series_installed
engine.publication_created
ipc.publication_sent
desktop.publication_received
chart.snapshot_installed
gpui.frame_requested
gpui.frame_presented
```

Do not log high-cardinality provider payloads or credentials.

---

# 61. ERRORS MUST INCLUDE STAGE

- [ ] **Status: Not verified complete**

Never emit merely:

```text
install_failed
```

Emit something that identifies:

```text
consumer
generation
instrument
interval
stage
error category
error chain
elapsed time
```

Example conceptual stages:

```text
canonical_validation
memory_install
aggregation
segment_encode
encryption
filesystem_write
catalog_commit
handoff
publication
ipc_send
chart_install
```

---

# 62. NO HIDDEN INFINITE LOADING

- [x] **Status: Verified complete**

Evidence (2026-08-15): each accepted consumer generation immediately receives `Resolving` or an in-memory `Ready` publication. Bounded local/provider queues either accept work or publish a stage-specific failure; a local-history read that has not completed within two seconds degrades persistence and starts provider repair; provider transports use explicit deadlines/cancellation; usable retained history terminates as `Partial` when repair fails; successful history reaches `Ready/Live`; and a newer generation fences obsolete completion. Deterministic regressions prove a stalled local read falls back to provider repair while a late valid result remains usable, and a controlled provider failure after `Resolving` publishes `Failed` plus retryable `DemandError`. Desktop workers consume these generation-qualified engine states and do not own a competing history-loading state machine.

Every consumer demand must eventually publish a state.

Example:

```text
Resolving
Partial
Ready
Failed
Superseded
```

Desktop UI must not invent an independent loading lifecycle that disagrees with engine state.

The engine is authoritative about market-data readiness.

---

# 63. GPUI PRESENTATION MODEL

- [x] **Status: Verified complete**

Evidence (2026-08-18): the desktop retains chart selection, consumer/publication generation, viewport, load/provider state, covering replay snapshot, live tail, and bounded presentation diagnostics only. Provider and storage work crosses authenticated IPC on background workers; GPUI applies validated application updates on frame boundaries. Desktop, terminal UI, and chart integration manifests exclude provider adapters, provider history, local storage/history, `market_engine`, and SQLite, and conformance scans reject concrete history/database/provider connection handles in production presentation source.

A chart presentation model should conceptually look like:

```text
ChartViewModel
    consumer_id
    generation
    instrument
    interval
    viewport
    series_snapshot
    realtime_tail
    load_state
    provider_state
```

It must NOT contain:

```text
RithmicSession
CoinbaseClient
HistoryStore
SQLiteConnection
ProviderScheduler
TokioRuntime
OrderBookEngine
```

---

# 64. CHART INTEGRATION

- [x] **Status: Verified complete**

Evidence (2026-08-18): `crates/ui/chart_integration` is the sole Axiusflow bridge to Origin. Its bounded `ChartDataBridge` validates series/session/publication generations, rejects stale or uncorrelated recovery snapshots, merges covering snapshots and contiguous tails, applies viewport and frame updates, and converts validated application data for Origin rendering. Workspace conformance now rejects any Origin manifest dependency or production source import outside this crate.

`crates/ui/chart_integration` remains the only Axiusflow-specific bridge to Origin Charts.

Responsibilities:

- convert canonical/presentation snapshot into Origin input,
- install new series,
- update active tail,
- apply viewport,
- request redraw,
- reject stale desktop-side generation as defense in depth.

It does not request provider data directly.

---

# 65. ORIGIN CHART ENGINE

- [x] **Status: Verified complete**

Evidence (2026-08-18): Axiusflow consumes Origin only through pinned Git dependencies in `crates/ui/chart_integration`; no engine, provider, storage, protocol, application, desktop, or terminal-UI crate imports Origin directly. Origin remains a separate repository and receives already validated chart data, viewport commands, and rendering interaction through the bridge. The repository conformance test makes this dependency boundary executable.

Do not migrate provider/backend responsibilities into Origin Charts.

Origin remains:

- framework agnostic,
- independent,
- reusable,
- not dependent on Axiusflow engine,
- not dependent on Rithmic,
- not dependent on Coinbase,
- not dependent on local storage.

Axiusflow feeds it data.

Origin renders/interacts.

---

# 66. MULTI-TAB GPUI COMPONENTS

- [ ] **Status: Not verified complete**

Add or evolve terminal UI components conceptually around:

```text
crates/ui/terminal_ui/src/workspace_tabs.rs
crates/ui/terminal_ui/src/workspace_view.rs
crates/ui/terminal_ui/src/pane_grid.rs
crates/ui/terminal_ui/src/chart_panel.rs
```

Exact file names may be adapted if equivalent components already exist.

Responsibilities:

`workspace_tabs.rs`
- tab strip,
- tab creation,
- close,
- activate,
- reorder.

`workspace_view.rs`
- one workspace's pane/layout model.

`pane_grid.rs`
- multi-pane layout,
- resize/split,
- lifecycle of visible panels.

`chart_panel.rs`
- owns GPUI chart presentation entity,
- owns one `ConsumerId`,
- sends chart demand to `EngineClient`,
- consumes series publications.

None of these files may import provider adapter crates.

---

# 67. TAB STATE VS MARKET STATE

- [ ] **Status: Not verified complete**

Desktop owns:

```text
Tab A is active
Chart A is in top-left
Chart B is in bottom-right
panel size
crosshair
drawing selection
```

Engine owns:

```text
ES history
BTC subscription
NQ order book
5m derived series
provider state
```

Do not confuse these.

---

# 68. INACTIVE TAB BEHAVIOR

- [ ] **Status: Not verified complete**

When a tab becomes inactive:

Desktop may stop rendering its charts.

Engine receives visibility/resource updates.

The engine may:

- keep data hot,
- reduce publication cadence,
- retain shared subscriptions,
- release expensive depth streams when no longer needed,
- preserve history in memory.

Switching back should reuse retained state.

---

# 69. NO THREAD PER CHART

- [x] **Status: Verified complete**

Evidence (2026-08-18): `engine_market_worker::start_group` builds any number of chart endpoints and then starts exactly one desktop engine-client thread and one authenticated connection for the group. The resident engine owns one coordinator, bounded role-level provider/history/storage workers, and one `MarketEngine`; consumer registration adds bounded demand/publication state but no runtime, database worker, or provider socket. The Rithmic presentation mode likewise has one app-level engine client plus one bounded history/result helper, while provider sessions remain resident-engine workers. Twenty-consumer tests prove one provider generation, independent chart mutations, shared immutable snapshots, and final-reference cleanup; the release multi-consumer benchmark exercises the same fixed worker topology.

Opening 20 charts must not create:

- 20 runtimes,
- 20 storage workers,
- 20 provider sockets,
- 20 history managers.

Charts are consumers.

The engine is shared.

---

# 70. NO DATABASE CONNECTION PER CHART

- [x] **Status: Verified complete**

Evidence (2026-08-18): SQLite and encrypted segment ownership remain in the engine-composed storage path. `apps/desktop`, `chart_integration`, and `terminal_ui` have no storage/history/SQLite dependency, and production presentation source contains no `rusqlite`, `HistoryStore`, Rithmic history connection, or ticker connection handle. Multiple chart consumers therefore share engine-owned local data rather than opening database connections per chart.

Storage is engine-owned.

Charts never touch SQLite/local segments.

---

# 71. NO PROVIDER AUTH PER CHART

- [x] **Status: Verified complete**

Evidence (2026-08-11): Coinbase uses one process-owned public realtime worker, while Rithmic authentication is loaded only by the resident engine's bounded catalog/quote, history, and realtime workers. Desktop charts attach as authenticated local-IPC consumers and never open or authenticate a provider connection. Symbol, timeframe, viewport, and chart-consumer changes reuse these role-scoped provider sessions; deterministic multi-consumer and session-stability tests verify that chart demand does not multiply provider authentication.

Provider authentication belongs to ProviderManager.

One account/provider session should serve compatible consumers.

---

# 72. PROVIDER MANAGER

- [x] **Status: Verified complete**

Evidence (2026-08-18): `market_engine/src/provider_manager.rs` owns canonical provider account identity, capabilities, reconnect delay, session generation, and health, rejects stale connect/disconnect transitions, and authorizes history/realtime/trade/quote/depth routing before adapter work. The process shell passes the configured reconnect policy to Coinbase realtime and Rithmic catalog workers while provider-specific authentication, sockets, retry mechanics, and wire logic remain in adapters. Tests verify account/policy exposure, capability rejection, exact disconnect fencing, and strictly newer reconnect generations.

`market_engine/src/provider_manager.rs`

Responsibilities:

- own configured provider sessions,
- connect/disconnect,
- provider health,
- provider account identity,
- route provider requests,
- expose canonical provider capabilities,
- reconnect policy.

It should delegate provider-specific wire logic to adapters.

It does not understand GPUI charts.

---

# 73. SUBSCRIPTION REGISTRY

- [x] **Status: Verified complete**

Evidence (2026-08-18): `market_engine/src/subscription_registry.rs` derives one bounded shared subscription record from accepted consumer demands, keeps exact per-series consumer references, unions explicit bars/trades/quotes/depth requirements, coalesces duplicate demand, and removes only the final unused record. Coordinator cleanup now queries this registry instead of rebuilding demand from IPC event maps. Coinbase and Rithmic upstream lifetimes release on the final reference under the warm policy, and the Rithmic worker can stop and later accept a new selection without creating another runtime. Deterministic core and coordinator tests cover duplicate coalescing, atomic switches, client detach, provider isolation, first-start, and final-release behavior.

`market_engine/src/subscription_registry.rs`

Responsibilities:

- track shared upstream requirements,
- ref-count consumers,
- coalesce duplicate provider demand,
- release unused upstream streams,
- distinguish bars/trades/quotes/depth requirements.

It does not render.

---

# 74. DEMAND REGISTRY

- [x] **Status: Verified complete**

Evidence (2026-08-18): `market_engine/src/demand.rs` maps each stable `ConsumerId` to its client/workspace identity, current generation, canonical provider/instrument/entitlement/definition/interval series key, viewport, visibility priority, and explicit stream requirements. Stale or conflicting same-generation mutations fail before shared state changes; a newer symbol or interval replaces only that consumer and atomically moves its subscription reference. Tests cover independent charts, generation fencing, viewport fencing, switch churn, and client-scoped cleanup.

`market_engine/src/demand.rs`

Responsibilities:

- map `ConsumerId → current demand`,
- current generation,
- visibility/resource priority,
- requested instrument,
- requested interval,
- requested viewport/history range,
- requested stream types.

A single chart update only mutates that chart's demand.

---

# 75. PUBLICATION MANAGER

- [x] **Status: Verified complete**

Evidence (2026-08-18): `market_engine/src/publication.rs` converts authoritative snapshots and live tails into consumer/generation-qualified publications with monotonic per-consumer publication generations, invalidates publications with corrected series, and removes state on demand replacement or detach. Reasserting an unchanged current demand produces a newer covering recovery publication backed by the same immutable snapshot. The bounded process-shell publication slots conflate only equivalent covering/tail state, while bucket rollover forces a covering snapshot; deterministic tests cover recovery generation, slow-consumer conflation, incremental tails, shared immutable snapshots, invalidation, and stale-demand fencing.

`market_engine/src/publication.rs`

Responsibilities:

- convert authoritative engine state into consumer publications,
- tag publications with consumer/generation,
- provide covering snapshots,
- provide incremental updates,
- apply client backpressure semantics,
- produce recovery snapshots after publication loss.

It does not own provider sockets.

---

# 76. HOT SET

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): the engine persists a bounded list of recently selected provider/market/interval identities, recency scores, last-used times, provider/series watermarks, and stable viewport bounds rather than serializing decoded market structures. Immutable manifest revisions survive restart, retain only the latest two, quarantine corrupt candidates, migrate legacy workspace state, and now receive a final shutdown snapshot that wins over any equal-revision stale manifest. Multiple active workspaces, pins, account-scoped identity, and coverage-driven warm reconstruction remain incomplete, so this section stays unchecked.

`market_engine/src/hot_set.rs`

Persist/track:

- recently visible instruments,
- recently used intervals,
- active workspaces,
- optionally pinned symbols,
- provider/account identity needed to restore useful state.

Do not persist enormous in-memory structures blindly.

Persist enough identity/coverage information to rebuild efficiently.

---

# 77. RESOURCE POLICY

- [ ] **Status: Not verified complete**

`market_engine/src/resource_policy.rs`

Resource policy should make intentional tradeoffs.

Inputs may include:

- engine mode,
- available memory,
- number of consumers,
- visibility,
- provider limits,
- hot-set priority.

Outputs may include:

- how much history remains decoded,
- which derived intervals remain cached,
- whether hidden depth views stay subscribed,
- prefetch size,
- warm retention duration.

Do not optimize resource use before correctness.

---

# 78. MEMORY IS ALLOWED TO BUY LATENCY

- [ ] **Status: Not verified complete**

Do not treat every MB as failure.

For a professional local trading terminal, maintaining useful market history in RAM is desirable.

Prefer:

```text
more predictable memory
+
instant interactions
```

over:

```text
minimal memory
+
repeated provider requests
+
visible loading
```

Still bound memory intentionally.

---

# 79. APPLICATION STARTUP

- [ ] **Status: Not verified complete**

Desktop startup path:

```text
GPUI process starts
↓
EngineSupervisor checks engine
↓
connect if already running
or start engine
↓
IPC handshake
↓
restore desktop/workspace state
↓
send current consumer demands
↓
receive immediately available snapshots
↓
render
```

Do not wait for all provider connections before opening the window.

---

# 80. ENGINE STARTUP

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): the shipping engine process binds the fixed local endpoint, opens persisted workspace and market state, starts the market service, and enters a nonblocking accept loop independently of provider readiness. One optimized Windows cold process run reached acknowledgement of a real authenticated shutdown request `1,132.567 ms` after launch. That is lifecycle-path evidence, not a startup distribution or a measurement through first usable local snapshot, so this section stays unchecked.

Engine startup:

```text
process starts
↓
open local metadata/catalog
↓
restore resource/hot-set state
↓
initialize MarketEngine
↓
become IPC-ready
↓
begin optional provider warmup
```

IPC readiness is separate from provider readiness.

---

# 81. WARM ATTACH

- [ ] **Status: Not verified complete**

If engine is already warm:

```text
desktop starts
↓
IPC connect
↓
RegisterConsumer
↓
SeriesStore hit
↓
SeriesSnapshot
↓
render
```

No provider round trip should be required for first pixels when usable hot/local data exists.

---

# 82. COLD START

- [ ] **Status: Not verified complete**

Cold start must still be correct and reasonably fast.

The warm daemon must not become a mechanism for hiding broken cold behavior.

Test with the engine fully terminated.

Cold startup should:

- render available local data,
- connect provider asynchronously,
- fetch missing coverage,
- progressively extend chart.

---

# 83. CLOSE UI / KEEP ENGINE WARM

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): native close, custom caption close, keyboard close, and direct application quit begin each desktop market client's bounded retirement without waiting on GPUI. App quit awaits those background detach acknowledgements, and closing the final window explicitly quits the GPUI process. A release Windows native-close run with an established engine exited the desktop in `225.858 ms`, preserved the engine, and then a separate explicit cleanup command exited that engine with code zero. Warm snapshot-to-render reopen evidence and durable user policy selection remain incomplete, so this section stays unchecked.

Desktop sends `DetachClient`.

Engine:

- removes presentation publication pressure,
- preserves configured hot state,
- continues according to resource mode.

Desktop exits completely.

No GPUI process remains.

---

# 84. COMPLETE EXIT

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): `axiusflow_engine --shutdown` connects only to an existing engine, authenticates with the native installation token, requests complete shutdown, and requires acknowledgement. The engine stops accepting connections and market demand, freezes persistent workspace mutation, writes a final newer hot-set manifest on a named one-shot worker, cancels every coordinator-owned in-flight history request and the Coinbase realtime source, drops Rithmic controls so active runtimes stop, drains the bounded queue of accepted local-history operations, cancels each native network/power wait, joins all four owned Rithmic monitor helpers, and joins the coordinator plus top-level provider/history/storage workers. Flush/worker panic or expiry and stuck client sessions produce process failure under the same two-second deadline. The optimized Windows run after native-monitor ownership exposed 19 threads before shutdown, exited with code zero `8.671 ms` after acknowledgement, and left no engine process. The desktop now exposes per-launch `--exit-with-desktop`; its app-quit future awaits client detach off GPUI, then uses the same authenticated shutdown helper. A release Windows native-close run exited the desktop in `44.567 ms` and left neither desktop nor engine process. Durable in-product selection and update/uninstall integration remain incomplete, so this section stays unchecked.

Desktop requests engine shutdown.

Engine:

- stops accepting new demand,
- cancels provider work,
- closes sessions,
- performs bounded persistence,
- records hot-set state,
- shuts down IPC,
- exits.

Do not rely on graceful shutdown for correctness of already-committed data.

---

# 85. DO NOT BUILD A DISTRIBUTED SYSTEM

- [x] **Status: Verified complete**

Evidence (2026-08-15): the shipping topology remains one native desktop and one resident per-user engine joined by authenticated platform-local IPC. Workspace manifests contain no service-discovery, consensus, remote-broker, cloud-queue, remote-database, Kubernetes, RPC-server, or HTTP-server dependency; provider HTTPS/WebSocket clients terminate in provider adapters and do not expose Axiusflow services. A repository conformance test scans every workspace manifest and rejects the distributed-system dependency families prohibited by this section.

Although there are two local processes, this is not a cloud microservice architecture.

Do not add:

- service discovery,
- Kubernetes concepts,
- distributed consensus,
- remote brokers,
- Kafka,
- Redis,
- remote databases,
- cloud queues,
- HTTP microservices.

This is one local product with one local engine.

Keep IPC simple.

---

# 86. DO NOT RECREATE DOCKER INSIDE RUST

- [x] **Status: Verified complete**

Evidence (2026-08-15): `apps/` contains exactly the `desktop` and `engine` application packages. Market coordination, provider history/realtime, local history, storage, and client sessions remain bounded threads or modules owned inside the engine process rather than separately launched daemons. A conformance test enumerates application manifests and fails if another application process is added.

The goal is not to simulate ten container services.

Do not create:

```text
history daemon
cache daemon
provider daemon
book daemon
chart daemon
```

There are two user-facing processes:

```text
desktop
engine
```

Internal work uses threads/tasks/modules.

---

# 87. CODE SIZE / COMPLEXITY BUDGET

- [x] **Status: Verified complete**

Evidence (2026-08-15): the platform contains 52,520 raw source lines before terminal inline test modules across `apps/` and `crates/`. This conservative upper bound still counts test-only helper items outside those modules, comments, and blank lines while excluding dedicated tests, benchmarks, Origin, vendor material, and generated protobuf output. A repository conformance test fails above the unchanged 65,000-line soft review threshold, while the required all-target build and lint gates protect correctness from line-count-driven deletion.

Current platform-side code is too large relative to working functionality.

The migration should aggressively remove duplicate ownership and obsolete abstractions.

Target engineering envelope:

Approximately 45,000–60,000 lines of hand-written production Rust for the platform side is a reasonable complexity target after consolidation.

Soft review threshold:

Approximately 65,000 production LOC.

These figures EXCLUDE:

- Origin chart repository,
- generated protobuf Rust,
- vendor `provider_kit`,
- tests,
- benchmarks,
- fixtures,
- generated code.

This is NOT permission to code-golf.

Correctness matters more than a line count.

Do not remove necessary safety logic simply to hit a number.

The purpose of the budget is to prevent:

```text
80k → 110k
```

during a supposed simplification.

If the migration adds 20,000 new lines while leaving all old runtime code intact, the migration has failed.

---

# 88. DELETE DEAD CODE

- [x] **Status: Verified complete**

Evidence (2026-08-15): the audit deleted the desktop Rithmic wrapper's unused interval-seconds conversion, 13 redundant named interval constants, test-only interval list, and broad dead-code suppression; tests now enumerate the canonical `ChartInterval` inventory directly. The remaining dead-code suppressions are confined by conformance test to external Coinbase decode fields and generated Rithmic protobuf bindings. All-target Clippy rejects ordinary unused private code, while existing conformance checks reject retired runtime crates, duplicate authorities, placeholder modules, and obsolete backend dependencies.

Once a migrated path is proven:

DELETE the old path.

Do not leave:

```text
old_market_worker.rs
legacy_market_worker.rs
market_worker_v2.rs
new_market_worker.rs
```

Do not leave unused public APIs.

Do not leave old feature flags indefinitely.

Do not keep "maybe useful later" abstractions.

Git already provides history.

---

# 89. NO PLACEHOLDER ARCHITECTURE

- [x] **Status: Verified complete**

Evidence (2026-08-15): every current production module under `apps/` and `crates/` contains executable behavior; there are no `todo!` or `unimplemented!` macros in production Rust, and the migrated desktop/engine path is exercised through deterministic core, worker, IPC, persistence, lifecycle, and presentation tests. A workspace source scan now fails if either placeholder macro enters production code.

Do not create a new module containing mostly:

```rust
todo!()
unimplemented!()
Default::default()
```

while routing production through the old architecture.

Each migration phase must own real runtime behavior.

---

# 90. NO DUPLICATE AUTHORITY

- [x] **Status: Verified complete**

Evidence (2026-08-15): `MarketEngine`, constructed and driven only by the resident engine coordinator, is the sole market-demand state owner. The retired `desktop_market_runtime` and `desktop_provider_runtime` crates have no manifest or Rust source, are absent from the workspace, and are prohibited by conformance tests; the desktop manifest also rejects `market_engine`, provider adapters, provider history, history/storage implementations, and both retired runtime packages. Desktop market modules are bounded engine-protocol clients and presentation mailboxes only.

At no time in the final architecture may both:

```text
desktop_market_runtime
```

and:

```text
market_engine
```

believe they own market demand.

During migration a temporary bridge is acceptable.

After cutover the old owner must be removed.

---

# 91. TRAIT POLICY

- [x] **Status: Verified complete**

Evidence (2026-08-15): production has exactly ten traits. Four are real provider transport/session boundaries (`ProviderHistoryAdapter`, Coinbase and Rithmic history transports, and `ProviderSessionDriver`); two are engine history/realtime deterministic substitution seams; three isolate native credential-vault, credential-backend, and key-revocation security boundaries; and one substitutes bounded history decoding. All other ownership remains concrete. A repository conformance allowlist fails when a trait is added, removed, or moved without an explicit boundary review.

Do not add traits merely because "clean architecture uses interfaces."

Add a trait only for:

- actual provider polymorphism,
- genuine test substitution,
- platform abstraction,
- multiple real implementations.

Prefer concrete types internally where abstraction has no current consumer.

---

# 92. CRATE POLICY

- [x] **Status: Verified complete**

Evidence (2026-08-15): the pure market core remains one cohesive `axiusflow_market_engine` crate with internal demand, provider, series, and publication modules. No speculative `market_engine_core`, `market_engine_runtime`, `market_engine_services`, `market_engine_common`, or `market_engine_types` crate or directory exists. A conformance test rejects those split names in both the workspace manifest and crate tree.

Do not add a crate for every concept.

`market_engine` is deliberately cohesive.

Do not create:

```text
market_engine_core
market_engine_runtime
market_engine_services
market_engine_common
market_engine_types
```

without an actual dependency reason.

---

# 93. LOCK POLICY

- [x] **Status: Verified complete**

Evidence (2026-08-15): every production synchronous mutex was traced to its owner and acquisition path. Workspace state is serialized by `EngineState`; market worker handles are locked only to transfer shutdown ownership; desktop locks protect bounded mailboxes, coalescing state, result slots, and a polled waker; the credential vault serializes native credential access; and Rithmic locks isolate socket abort, callback, catalog, timing, and command slots. Guards are released before wakeups, joins, socket work, or another lock; no synchronous guard crosses `.await`; provider and coordinator progress never requires a desktop-held lock; and the market coordinator uses message ownership instead of shared locking.

Audit all shared locks.

For every important lock:

- who owns it,
- who acquires it,
- whether held across `.await`,
- whether another task needs it to progress,
- whether message ownership could replace it.

Never hold a synchronous mutex guard across async suspension.

Avoid nested lock graphs.

---

# 94. QUEUE POLICY

- [x] **Status: Verified complete**

Evidence (2026-08-15): `ARCHITECTURE.md` now inventories every production queue family with producer, consumer, capacity, full behavior, conflation, resynchronization, and control priority. Engine command/history/storage/catalog lanes are bounded at 64/8/16/64 items; realtime lanes are 2,048 events with a 4,096-item history handoff; Rithmic callbacks are bounded by 256 items and 8 MiB; provider environment/session commands are eight; desktop publication/command lanes are 32/32 and 32/8; chart publication is 64; history and lifecycle rendezvous are one. Data producers use nonblocking or bounded submission, loss of correctness-critical data invalidates the generation and requires covering recovery, semantic presentation slots alone conflate, and independent control/cancellation lanes prevent drain deadlock. A production-source conformance scan rejects unbounded channel constructors.

Every queue documents:

- producer,
- consumer,
- capacity,
- what happens when full,
- whether events may be conflated,
- whether loss requires resync,
- whether control traffic has priority.

No unbounded market queue unless explicitly justified by measurement.

No bounded queue may create deadlock by blocking the only task capable of draining it.

---

# 95. PROVIDER THREAD MUST NEVER BLOCK ON UI

- [x] **Status: Verified complete**

Evidence (2026-08-15): provider adapters and engine workers have no UI dependency or GPUI handle. Coinbase realtime uses bounded nonblocking event submission and terminates the generation on overflow; Rithmic callbacks use bounded byte/item queues, generation fencing and explicit recovery. Provider heartbeats and socket progress run on provider-owned workers, history and persistence use separate workers, the coordinator only writes conflated in-memory consumer slots, and desktop polling occurs through independent client-session threads. Slow-consumer, realtime-overflow, storage-failure and full-history-queue regressions prove UI inactivity and downstream pressure cannot become an indefinite provider wait.

Provider read loops may not wait for:

- GPUI,
- chart redraw,
- desktop mailbox availability indefinitely,
- disk persistence.

Provider heartbeat/protocol progress must remain independent.

---

# 96. STORAGE WRITER MUST NOT BLOCK PROVIDER

- [x] **Status: Verified complete**

Evidence (2026-08-15): the engine owns one separate bounded storage worker. The coordinator uses nonblocking `try_send` for reads and persistence, publishes valid memory state before persistence, and reports full, disconnected, failed, or two-second-stalled storage as `PersistenceState::Degraded`; provider workers never wait for the storage queue or GPUI. A stalled-read regression proves provider repair begins without waiting for the disk worker and a late valid local result remains usable. A failed-storage realtime regression observes `Ready/Degraded`, then provider `Connecting → Online`, series `Live`, and a newer forming snapshot from a live trade, proving provider progress and chart memory remain independent of disk health.

Storage is downstream.

If disk becomes slow:

- memory/chart can remain live,
- persistence becomes degraded,
- engine applies bounded buffering/backpressure,
- provider session remains healthy.

---

# 97. TEST ENGINE WITHOUT GPUI

- [x] **Status: Verified complete**

Evidence (2026-08-15): the engine manifest has no GPUI/UI dependency and its complete deterministic suite runs headlessly. E1 is covered by fixture history through canonical `MarketEngine` installation and IPC snapshot tests. E2 is covered by 1m→5m/15m/1h compatible derivation, cache reuse, and one shared realtime-session tests. E3/E4 are covered by BTC→ETH→BTC and rapid symbol/timeframe churn with stale-generation rejection. E5 is covered by failed-storage snapshots that remain reusable and live. E6 is covered by controlled provider failure reaching explicit `Failed` plus stage-specific `DemandError`. No desktop process or native window is launched by these scenarios.

The entire market engine must be testable without launching the desktop.

Required engine-level scenarios:

### E1
Coinbase BTC-USD 1m cold history.

Expected:

```text
provider response
→ canonical bars
→ SeriesStore
→ SeriesSnapshot
```

### E2
BTC 1m → 5m → 15m → 1h.

No UI.

Verify:

- no deadlock,
- generations work,
- derived cache works,
- provider session not unnecessarily recreated.

### E3
BTC → ETH → BTC.

Verify stale results are rejected.

### E4
Rapid symbol/timeframe churn.

### E5
Storage unavailable.

Valid provider bars still become usable in memory.

### E6
Provider unavailable.

Demand reaches explicit failed/degraded state.

### E7
Slow consumer.

Provider remains healthy.

### E8
Two consumers request same instrument.

Verify upstream subscription sharing.

### E9
Twenty consumers request combinations of shared instruments/intervals.

Verify no thread/provider-session explosion.

### E10
Depth sequence gap.

Verify recovery.

---

# 98. MULTI-CHART TEST

- [x] **Status: Verified complete**

Evidence (2026-08-11): `apps/engine/src/market_service.rs::twenty_chart_consumers_share_one_provider_and_remain_independent` creates one attached client representing five workspace IDs with four chart consumers each. The 20 consumers demand a repeated mix of all eight implemented BTC-USD/ETH-USD fixed-interval series. The test proves exactly eight history fetches, one realtime provider generation, bounded configured consumer/series/bar capacity, independent selection generations, continued shared-series publication after one consumer switches, and continued publication after that consumer is removed.

Create a deterministic non-GPUI engine test representing:

```text
5 workspace tabs
4 charts per tab
20 chart consumers total
```

Use a mix such as:

```text
ES 1m
ES 5m
ES 1h
NQ 1m
NQ 5m
BTC 1m
BTC 5m
ETH 1m
...
```

Verify:

- 20 consumers exist,
- shared series are reused,
- provider subscriptions are coalesced,
- memory stays bounded,
- each generation is independent,
- closing one chart does not interrupt other consumers,
- switching one chart does not cancel another chart's work.

---

# 99. GPUI MULTI-TAB TEST

- [ ] **Status: Not verified complete**

Run the real desktop.

Create several tabs.

Each tab contains multiple charts.

Verify:

- switching tabs remains responsive,
- hidden tabs do not render continuously,
- engine keeps required state warm,
- reopening a tab does not reconstruct provider sessions,
- visible charts receive correct data,
- charts with same instrument can share backend state,
- layout operations do not affect provider health.

---

# 100. WARM ENGINE TEST

- [ ] **Status: Not verified complete**

Run:

```text
launch engine
launch desktop
open BTC 1m
wait until ready
close desktop
keep engine alive
reopen desktop
```

Measure:

```text
desktop process start
→ IPC attach
→ first usable chart snapshot
→ first rendered frame
```

There should be no provider history request on the critical path when sufficient hot data exists.

---

# 101. COLD ENGINE TEST

- [ ] **Status: Not verified complete**

Fully terminate everything.

Launch desktop.

Engine starts cold.

Verify:

- local data loads first when present,
- UI remains responsive,
- provider fetch is asynchronous,
- first valid partial state renders,
- eventually current/live state is reached.

---

# 102. TIMEFRAME PERFORMANCE CONTRACT

- [x] **Status: Verified complete**

Evidence (2026-08-11): schema 2 of the ignored resident-engine release verifier primes exact BTC-USD 1m/5m and ETH-USD 1m series, then alternates generation-fenced timeframe and symbol demand on one authenticated desktop `EngineClient` until the decoded snapshot matches the requested generation and full `SeriesKey`. The provider fixture completes only while priming, outside the timed arms, so the 32 warm-ups and 128 measured samples isolate cached local coordinator, protobuf, socket, and client work. On the 24-logical-CPU Intel Windows development host, cached 1m/5m timeframe switching measured `0.0912/0.1109/0.1710 ms` and cached BTC/ETH symbol switching measured `0.0929/0.1451/0.2205 ms` p50/p95/p99. Three immediate optimized repetitions also passed the 20 ms p50 and 50 ms p95 local-interaction budgets; across all four runs, the worst observed p99 values were `0.1710 ms` for timeframe switching and `0.2205 ms` for symbol switching. The separate production aggregation runner continues to measure first derived-interval creation, so cache-hit switching is not used to claim cold derivation cost.

When compatible source data is already in memory:

Typical timeframe switching should be an in-memory/local computation path.

Target measurement for ordinary visible ranges:

```text
command received
→ usable snapshot publication
```

should generally be in low milliseconds to tens of milliseconds.

Do not claim universal exact timing across machines.

Record p50/p95/p99.

Separate network latency from local engine latency.

---

# 103. WARM ATTACH PERFORMANCE TARGET

- [x] **Status: Verified complete**

Evidence (2026-08-11): an ignored release verifier primes a 350-bar Coinbase series in the resident `MarketService`, then measures generation-fenced demand through the actual authenticated local socket and blocking desktop `EngineClient` until the decoded usable covering snapshot arrives. It uses 32 warm-ups and 128 measured samples, rejects snapshots with the wrong generation, consumer, or bar count, and fails closed above the target p50/p95 budgets. On the 24-logical-CPU Intel Windows development host, the post-refactor release run measured `0.0877 ms` p50, `0.1216 ms` p95, and `0.1477 ms` p99. Direct coordinator demand-to-snapshot measured `0.0038/0.0041/0.0054 ms` p50/p95/p99, separating local engine work from framing and IPC. The repeatable command is `cargo test --release -p axiusflow_engine release_cached_demand_ipc_and_multi_consumer_performance -- --ignored --nocapture`; results are machine-specific engineering evidence, not universal guarantees.

When engine is already running and requested series is in memory:

Target:

```text
IPC demand
→ desktop receives usable snapshot
```

in a range consistent with an effectively instant UI interaction.

Aim for:

```text
p50 < 20 ms
p95 < 50 ms
```

on representative development hardware.

These are engineering targets, not universal guarantees.

---

# 104. GPUI RESPONSIVENESS

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): schema 8 of the real windowed GPUI benchmark retains the schema-7 replay, covering-snapshot, queue, frame-registration, memory, and DWM checks and adds bounded diagnostics at the exact production interaction owners. The benchmark emits real GPUI change and Enter events through `subscribe_symbol_input`, alternates the same `select_instrument` and `select_interval` methods used by mouse and keyboard actions, and drains the resulting nonblocking commands through a capacity-four disconnected sink without an engine or provider. Four optimized Windows runs recorded 128 samples apiece per handler. Input-change, Enter-submit, instrument-selection, and interval-selection p99 ranges were `0.0003-0.0007 ms`, `0.0067-0.0810 ms`, `0.0014-0.0028 ms`, and `0.0064-0.0132 ms`; snapshot installation measured `0.2105-0.2379 ms`, replacement start to next-frame callback measured `5.3870-6.0956 ms`, frame-registration p99 measured `0.0006-0.0008 ms` across 418 samples per run, and worst update-to-frame p99 was `7.3820 ms`. Every run retained queue depth `0/1/0`, an advancing DWM timeline, and zero late, dropped, or missed-frame growth. Missing or incomplete interaction streams and mismatched command batches fail without writing evidence, and ordinary builds retain no handler sample buffers or timing calls. Cached demand latency remains verified separately by Section 102. The current terminal has no tab surface, so tab-switch duration remains open and this section stays unchecked.

GPUI foreground callbacks should not perform expensive work.

Instrument:

- input event duration,
- tab switch,
- symbol change handler,
- timeframe change handler,
- chart snapshot installation,
- frame scheduling.

Any unexpectedly long foreground operation must be investigated.

---

# 105. PROVIDER NETWORK BENCHMARKS

- [ ] **Status: Not verified complete**

Do not mix provider network latency with local engine performance.

Record separately:

```text
connection/auth
provider history
provider realtime startup
```

and:

```text
engine processing
IPC
chart install
frame
```

A provider taking 500 ms does not justify another 500 ms of local orchestration.

---

# 106. CURRENT `history.install_failed` REGRESSION

- [x] **Status: Verified complete**

Evidence (2026-08-15): the obsolete desktop Coinbase owner has been removed, and its failure invariant now has a resident-engine regression at the owning boundary. With a deterministic valid provider response and an unavailable `LocalHistoryStore`, canonical memory installation publishes the expected covering snapshot, reports `Ready/PersistenceState::Degraded`, and serves a newer consumer generation from the same in-memory series without another provider fetch. A companion realtime regression advances that degraded series to `Live`, so storage failure cannot recreate `history.install_failed` or indefinite chart loading.

Before retiring the current Coinbase path, create a regression reproducer.

The new architecture must prove:

```text
valid provider history
↓
memory install succeeds
↓
chart publication succeeds
```

If persistence fails:

```text
persistence degraded
```

but not:

```text
chart loading forever
```

---

# 107. MIGRATION MUST BE INCREMENTAL

- [ ] **Status: Not verified complete**

Do NOT perform a 100-file big-bang rewrite.

Use phases.

Each phase must:

- compile,
- pass focused tests,
- have runtime evidence,
- remove replaced code when cut over.

---

# 108. PHASE 0 — FORENSIC BASELINE

- [x] **Status: Verified complete**

Evidence (2026-08-11, baseline commit `a3c5334`, Windows 11 x86_64, Intel i7-13700K, 24 logical CPUs):

- The retained forensic sequence is `fetch_started → fetch_completed → completed_received → persist_begin → install_failed → Loading`.
- The exact current gating path is `live_market_worker/history.rs::install_repaired_snapshot → persist_history_repairs → install_merged_history`. Both persistence calls precede `install_history_snapshot` and `publish_update`; an error is converted to an unqualified string, then `fence_failed_history` clears retained bars and reports Recovering. This is why valid fetched bars can fail to reach the chart. The original external error string and candle-value print are not present in the current repository; repository search confirms the stage labels survive only in this specification, so stage identity must be restored by the new engine regression.
- Release market-data baseline (`250,000` bars): cold publish `4,184 ms`; warm open `25 ms`; warm discovery `1 ms`; first usable `178 µs`; warm read `81 ms`; decode `8 ms`; peak resident memory `31,518,720` bytes. Uncached aggregation: `5m 2,771 µs`, `15m 2,357 µs`, `1h 1,569 µs`, `4h 1,537 µs`, `1D 1,521 µs`; repeated `5m` cache hit `0 µs`. Coverage was complete with zero gaps, duplicates, or missing ranges.
- The diagnostics-overhead release verifier did not produce a valid baseline on this host: it failed with `ZeroBaseline("p99")` because the baseline quantized to zero. This verifier defect is preserved rather than misreported as a performance result.
- Cargo normal dependency graph: `1,307` rendered lines, `665` unique rendered lines, SHA-256 `ce91d7fbfb157a8f8b86c7ab2d85914dda79059936920bfb85567cede73ee6c5`. Commit `a3c5334` is the reconstructable graph source.
- Rust baseline: `70,935` lines across `142` files. Largest responsibility areas were `rithmic_protocol 13,516`, `desktop_market_runtime 12,160`, `apps/desktop 7,112`, `coinbase_market 6,183`, `desktop_provider_runtime 5,447`, `desktop_storage 4,667`, and `desktop_history 2,480` lines.
- Startup baseline: desktop constructs the Coinbase or Rithmic worker before entering GPUI; engine startup opens workspace/hot-set state, binds authenticated local IPC, and serves workspace requests without provider work. The authenticated engine handshake/restore suite passed `11/11`.
- Coinbase production public history status: release `--coinbase-live-smoke BTC-USD` passed with a covering live-provider snapshot and clean bounded shutdown; no local-cache snapshot was observed in the clean temporary root.
- Rithmic status: adapter protocol/conformance tests passed `73/73`; credentialed/entitled live runtime was not executed and remains explicitly unverified.

Before migration:

- preserve current benchmark numbers,
- preserve current forensic traces,
- locate exact `history.install_failed` error,
- locate candle printing source,
- record current Cargo dependency graph,
- record current LOC by crate,
- record current startup behavior,
- record Coinbase and Rithmic status.

Do not optimize.

Create baseline evidence.

---

# 109. PHASE 1 — ENGINE PROTOCOL

- [x] **Status: Verified complete**

Evidence (2026-08-11; crate renamed 2026-08-18): `engine_protocol` version 3 defined authenticated attach/detach, stable client/workspace/consumer identities, per-consumer series generations, viewport and visibility demand, consumer removal, explicit series and persistence states, fixed-point snapshots/updates, provider state, stage-specific demand errors, resource mode, and complete shutdown. Removed tags `8..=14` and `19..=24` remain unused. Every payload passed fragmented/coalesced round-trip, malformed/version/frame-bound tests, and strict package Clippy.

The transitional `local_engine_protocol` identity was replaced by final `engine_protocol` after all callers cut over.

Add only essential:

- handshake,
- client attach/detach,
- consumer registration,
- series demand,
- viewport demand,
- visibility,
- series state,
- series snapshot/update,
- provider state,
- errors,
- shutdown/resource mode.

Do not yet add every future feature.

Keep protocol versioned.

---

# 110. PHASE 2 — MARKET ENGINE CORE

- [x] **Status: Verified complete**

Evidence (2026-08-11): `crates/market_engine` now owns one explicit `MarketEngine` containing a bounded demand registry, provider-session/capability registry, bounded canonical series store, and latest immutable per-consumer publications. It uses ordinary Rust ownership with no GPUI, IPC serialization, sockets, storage, threads, globals, or new dependency beyond the canonical market-data domain. Deterministic tests prove per-consumer stale-generation rejection, exact provider-session fencing, bounded consumers/series/bars, disconnect cleanup, and twenty consumers sharing one `Arc<SeriesSnapshot>` while the store retains one series. Focused tests and strict package Clippy pass.

Create `market_engine`.

Move only generic ownership first:

- demand registry,
- series store,
- publications,
- provider manager contract.

No GPUI dependencies.

Write deterministic tests.

---

# 111. PHASE 3 — COINBASE FIRST END-TO-END ENGINE PATH

- [x] **Status: Verified complete**

Evidence (2026-08-11): the default desktop Coinbase startup attaches a bounded `EngineClient` to the current protocol-v5 IPC; `axiusflow_engine` owns one market coordinator, one Coinbase historical worker, one Coinbase realtime worker, the canonical `MarketEngine`/`SeriesStore`, and fixed-point per-consumer snapshots. Deterministic tests prove authenticated IPC delivery, shared engine cache use, generation fencing, disconnect cleanup, desktop precision/provenance conversion, and the history/live gate subsequently verified in section 154. Clean Windows release runs started with no resident process, spawned the sibling release engine, remained responsive with a green connection state, and rendered updating BTC-USD one-minute candles in Origin.

Move Coinbase execution into `axiusflow_engine`.

Desktop communicates only by IPC.

Required working path:

```text
GPUI
↓
EngineClient
↓
IPC
↓
MarketEngine
↓
Coinbase adapter
↓
canonical bars
↓
SeriesStore
↓
IPC snapshot
↓
GPUI
↓
Origin
```

Do not migrate Rithmic simultaneously.

Prove Coinbase first.

---

# 112. PHASE 4 — TIMEFRAME/SYMBOL SWITCHING

- [x] **Status: Verified complete**

Evidence (2026-08-11): the engine and default desktop now support BTC-USD and ETH-USD at 1m, 5m, 15m, and 1h over asynchronous protocol-v5 demand and bounded event polling. One shared Coinbase realtime session routes both products into per-series fixed-interval history/live handoffs; cached forming tails resume without rewriting their canonical sequence. Deterministic delayed-history churn drives the exact `BTC 1m to 5m to 15m to 1h to 1m to ETH 1m to BTC 1m` sequence and proves only the latest generation can publish, while a separate test proves all switches reuse one realtime start. A Windows release desktop/engine run captured all seven corresponding Origin chart states with green connection status and visible candles; both processes remained responsive, the engine retained only two established Coinbase TLS connections, and no stale overwrite, hang, or infinite loading state appeared.

Before moving Rithmic:

Prove:

```text
BTC 1m
→ 5m
→ 15m
→ 1h
→ 1m
```

and:

```text
BTC
→ ETH
→ BTC
```

including rapid churn.

No hangs.

No stale overwrite.

No infinite loading.

---

# 113. PHASE 5 — MULTI-CHART ENGINE DEMAND

- [x] **Status: Verified complete**

Evidence (2026-08-11): the desktop engine bridge now assigns one consumer ID and one bounded command/message endpoint per chart while a single coordinator thread owns the one authenticated `EngineClient`. The opt-in `--multi-chart` proof opens BTC and ETH as two real GPUI/Origin chart windows without creating a second desktop engine client, provider session, or backend runtime; endpoint shutdown removes only that chart's consumer and detaches the client only after the last endpoint closes. A Windows release run visibly rendered both charts together with green streaming state while the single desktop and single resident engine remained responsive. Closing one native chart window left the other chart rendering and responsive. The deterministic section 98 test separately proves the 20-consumer, shared-series, single-provider-generation, switch-isolation, and close-isolation invariants.

Implement multiple consumer IDs.

Test 20 consumers.

Then connect multiple real GPUI charts.

Do this before Rithmic migration if practical.

This proves the core architecture is genuinely multi-chart rather than another single-active-chart design.

---

# 114. PHASE 6 — LOCAL HISTORY / STORAGE

- [x] **Status: Verified complete**

Progress evidence (2026-08-11): one bounded resident-engine local-history worker now serves both Coinbase and Rithmic through the authenticated SQLite catalog, encrypted immutable segments, and native-vault keys under the engine data root. Hot memory is checked first; retained disk bars publish as `Partial/Durable` before provider repair, provider bars publish as `Ready/Pending` before persistence is queued, and persistence success/failure becomes independent `Durable`/`Degraded` state. The provider/account/entitlement-scoped segment format preserves Rithmic's exact nanosecond bar ordering for every one of its 15 chart cadences and still reads existing Coinbase whole-second segments. Deterministic tests prove encrypted restart reads for all Rithmic cadences, legacy segment compatibility, disk-before-provider ordering with installed Rithmic precision, retained-data survival when provider repair fails, memory-before-persistence ordering, usable provider history under total storage failure, bounded derived-series reuse, and cold finer-to-coarser derivation retained across another restart. An isolated Windows release run created encrypted Coinbase segments, then crash-restarted the same root with both processes responsive, retained all prior segments, added the refreshed range, and quarantined nothing. The obsolete desktop Coinbase storage path is deleted, and the remaining desktop Rithmic runtime has no storage access. The Phase 6 ownership and recovery requirements are verified complete.

Move storage/history behind engine.

Ensure:

- memory publication precedes persistence dependency where safe,
- progressive reads,
- derived cache,
- storage degradation state,
- warm restart.

Remove desktop access to storage.

---

# 115. PHASE 7 — RITHMIC MIGRATION

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): protocol v10 and the canonical `BarPeriod` preserve provider, instrument, entitlement revision, definition revision, the complete Rithmic chart cadence catalog, exact nanosecond bar time, bounded engine-owned order-book images, bounded exact catalog search, coarse catalog rejection, and complete selected-instrument metadata. Search and selection cross authenticated IPC as consumer- and command-generation-fenced provider-neutral messages. The resident engine owns the native-vault catalog/quote session, converts adapter catalog results at the worker boundary, installs canonical identity and precision before publishing selection success, and then coordinates its separate native-vault history and realtime sessions. It owns replay planning, cancellation, all 15 cadence collection, encrypted persistence, snapshot publication, bounded transport retry, native power/network transitions, provider-generation advancement, history/live buffering, live tick/fixed/session candle publication, the sole depth subscription, canonical top-20 order-book reconstruction, recovery state, and conflated depth IPC publication. Native environment state is retained across recovery, unavailable/suspended states fence connection startup, restored states start newer generations, and monitor failure closes a provider worker rather than continuing without lifecycle evidence. Each catalog/realtime worker retains cancellation handles and join handles for its native network and power monitors, including cleanup of partial startup. The desktop creates no Rithmic socket, loads no provider credential, starts no native provider lifecycle monitor, and performs no provider retry or live aggregation. Its app-local bounded `rithmic_engine_client` and `rithmic_engine_history` modules speak provider-neutral engine protocol directly and project engine chart/DOM snapshots; the old runtime-owned worker, shell, live chart, transition capture, series browser, and the entire `desktop_market_runtime` compatibility crate are deleted, and the desktop depends on neither `rithmic_protocol` nor `desktop_provider_runtime`. Deterministic tests cover protocol catalog round trips, catalog installation before publication, exact-time live handoff, completed-history immutability, forming-tail continuation, session-change rejection, generation monotonicity, native-state retention, native waiter cancellation, continuous snapshot conversion, engine-owned depth reconstruction/projection, all-cadence encrypted restart, provider-history isolation/cancellation, and app-local mailbox conflation. Credentialed native catalog smoke testing and exchange-calendar-owned live week/month bars remain open, so this phase remains unchecked.

Move Rithmic provider ownership into ProviderManager.

Keep:

```text
rithmic_protocol
```

as protocol adapter.

Remove:

```text
rithmic_market_worker
rithmic_shell
rithmic_live_chart
```

from generic runtime when their responsibilities have been absorbed.

Verify:

- history,
- streaming,
- timeframe changes,
- symbol changes,
- reconnect,
- order-book continuity.

---

# 116. PHASE 8 — WARM ENGINE MODE

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): the authenticated resource-mode boundary now governs coordinator retention, the complete-exit command is implemented, unconditional Windows login registration is removed, and the optimized production engine was observed writing its final bounded hot-set manifest, cancelling and joining its native environment helpers and top-level market workers, and exiting cleanly. Default native desktop close detaches bounded market clients off GPUI and preserves the resident engine; per-launch `--keep-markets-live` preserves bounded selected handoffs/provider work across final detach, and per-launch `--exit-with-desktop` requests complete exit. Deterministic no-UI reattachment proves a retained forming bar advances without another history fetch. Release Windows native-close samples left the expected topology for ordinary warm and complete-exit modes; an additional markets-live run closed two successive desktops, reused the same surviving engine PID, and ended with code-zero authenticated cleanup. Durable in-product lifetime preferences, optional cross-platform login startup, provider-permission configuration, credentialed markets-live evidence, and GPUI snapshot-to-render verification remain incomplete, so Phase 8 stays unchecked.

Only after cold behavior works correctly:

Implement:

- Keep Engine Warm,
- Keep Markets Live,
- Exit Completely,
- optional OS user-session autostart.

Warm mode must enhance a correct product.

It must not hide cold-start failures.

---

# 117. PHASE 9 — DELETE LEGACY RUNTIMES

- [x] **Status: Verified complete**

Evidence (2026-08-11; protocol crate renamed 2026-08-18): `desktop_market_runtime` and `desktop_provider_runtime` are deleted after their real owners moved to the resident engine, Rithmic adapter, observability, or the sole desktop presentation consumer. Dead Coinbase drivers/fixtures, the uncalled provider/history composition, old `application::stream_runtime`, the unconsumed desktop event/publication queue, the desktop-era Rithmic adapter module name, and the stale provider-runtime conformance script are also gone. The final duplicate-IPC audit found that the old protobuf market-stream schema and conversion adapter were used only by the deterministic disconnected desktop fixture; production publication already used the protocol now named `engine_protocol`. The fixture now drives the existing application replay model directly, its uncalled worker-thread bridge and startup variant are deleted, transport-neutral replay sequence/checksum types live with the application model, and the orphaned `protocols`, `market_protocol`, schema, build, dependency, workspace, and lockfile paths are removed. The shared bounded framing crate remains because protocol v10 actively uses it. No compatibility market protocol or dead fixture runtime remains.

After Coinbase and Rithmic use the new engine:

Delete obsolete:

- `desktop_market_runtime`,
- `desktop_provider_runtime`,
- old stream runtime logic,
- duplicate IPC market protocols,
- dead runtime bridge code.

Do not leave compatibility paths.

---

# 118. PHASE 10 — PERFORMANCE / MEMORY TUNING

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-11): the Phase 0 diagnostics verifier defect is repaired without weakening its 5% p99 or 10% p99.9 budgets. Evidence schema 2 now measures paired, alternating disabled/enabled arms around the same deterministic shipping workload: an 8,192-trade Coinbase batch is decoded, canonicalized, applied to the production fixed-point bar aggregator, and passed through the feed counters and seven local timestamp-chain intervals. Optimizer barriers make the input and accumulator observable, 256 warm-up plus 2,048 measured samples avoid sub-nanosecond quantization, and a deterministic test verifies the fixture's declared unique-trade count. A Windows x86_64 release run on the 24-logical-CPU Intel development host measured disabled/detailed p99 of `738/746 ns` (109 basis points) and p99.9 of `805/818 ns` (162 basis points), with zero gaps, overflows, or recoveries. Three preceding release repetitions also passed both budgets. This closes the invalid-baseline defect; the subsequent evidence below records the completed local demand/IPC slice and the remaining Phase 10 gaps.

Progress evidence (2026-08-11): the release-only resident-engine verifier now measures cached direct demand-to-snapshot, authenticated IPC demand-to-decoded-snapshot, cached timeframe and symbol switching, connect/authenticate/attach/restore, and one shared 350-bar series demanded by 20 independent consumers. The post-refactor schema-1 run measured direct demand at `0.0038/0.0041/0.0054 ms`, IPC demand at `0.0877/0.1216/0.1477 ms`, attach/restore at `0.1022/0.1714/0.1860 ms`, and the complete 20-consumer IPC batch at `1.9414/2.3279/2.4852 ms` p50/p95/p99 (`0.09707 ms` p50 per consumer). Schema 2 adds cached 1m/5m timeframe switching at `0.0912/0.1109/0.1710 ms` and cached BTC/ETH symbol switching at `0.0929/0.1451/0.2205 ms` p50/p95/p99. The verifier exercises the shipping coordinator, generation fences, protobuf framing, authenticated local socket, and desktop client rather than a benchmark-only transport. This closes the local demand, IPC snapshot, cached symbol/timeframe switching, and deterministic multi-consumer latency gaps and verifies the Sections 102 and 103 targets. Full desktop-process warm reopen through first rendered frame, startup, tab switching, provider-to-engine latency, foreground input, memory, queue occupancy, and reconnect measurement remain open, so Phase 10 remains unchecked.

Progress evidence (2026-08-11): schema 8 of the optimized desktop `--windowed-benchmark` samples process working set before GPUI setup and after completion, observes the real Origin replay queue immediately before and after every one-update-per-frame delta submission, installs one real 600-bar same-series covering snapshot through `OriginChartView::load_replay`, times every GPUI frame registration, and records the production input, instrument, and interval callback boundaries. Four immediate Windows runs opened a native window and rendered 256 measured replay frames; every run observed queue depth `0` before submission, `1` after submission, zero overflows, one covering-install sample, 418 frame-scheduling samples, 128 samples for each interaction handler, an advancing DWM timeline, and zero late, dropped, or missed-frame growth. Input-change, Enter-submit, instrument-selection, and interval-selection p99 ranges were `0.0003-0.0007 ms`, `0.0067-0.0810 ms`, `0.0014-0.0028 ms`, and `0.0064-0.0132 ms`; snapshot installation measured `0.2105-0.2379 ms`, replacement start to next-frame callback measured `5.3870-6.0956 ms`, frame-registration p99 measured `0.0006-0.0008 ms`, worst update-to-frame p99 was `7.3820 ms`, first-frame latency ranged from `23.1223` to `30.1152 ms`, and working-set growth ranged from `66,101,248` to `66,744,320` bytes. The verifier fails without writing evidence if queue, replacement, callback, command-batch, or foreground-timing invariants fail. It makes no resident-engine demand-latency claim, while Section 102 retains independent cached symbol/timeframe evidence. This closes the deterministic input/symbol/timeframe foreground-handler gap in addition to the single-window working-set, chart-queue, snapshot-install, and frame-scheduling gaps. Full desktop-process warm reopen through a resident-engine snapshot and rendered frame, cold startup, multi-tab switching and memory, resident-engine memory, provider-to-engine latency, and reconnect measurement remain open, so Phase 10 remains unchecked.

Progress evidence (2026-08-11): schema 3 of the release-only engine verifier adds sampled process working set to the existing production coordinator, authenticated local IPC, cached timeframe/symbol switching, attach/restore, and 20-consumer shared-series workload. The first Windows release run measured 9,940,992 bytes before engine startup, 11,128,832 bytes after startup, and 12,255,232 bytes after the complete workload: 2,314,240 bytes sampled total growth and 1,126,400 bytes sampled post-start workload growth. The same run measured IPC demand at `0.0769/0.0963/0.1129 ms`, timeframe switching at `0.0957/0.1249/0.1693 ms`, symbol switching at `0.0990/0.1317/0.1978 ms`, attach/restore at `0.1023/0.1742/0.2298 ms`, and the complete 20-consumer batch at `1.8255/2.0952/2.2435 ms` p50/p95/p99. This closes the deterministic engine-workload memory-measurement gap without inventing a budget, but it is test-process evidence rather than a standalone resident-engine endurance measurement. Full desktop-process warm reopen through a resident-engine snapshot and rendered frame, cold startup, multi-tab switching and memory, standalone resident-engine endurance memory, provider-to-engine latency, and reconnect measurement remain open, so Phase 10 remains unchecked.

Progress evidence (2026-08-11): one optimized standalone Windows engine process reached authenticated shutdown acknowledgement `1,132.567 ms` after launch, measured `10,641,408` bytes of working set immediately before the request, and exited with code zero `3.902 ms` after acknowledgement, leaving no engine process. This covers the production executable, native installation credential, fixed local endpoint, lifecycle request, and bounded exit in one cold sample. It is not startup percentile, first-usable-snapshot, warm-reopen, provider-disconnect, endurance-memory, or persistence-flush evidence, so Phase 10 remains unchecked.

Progress evidence (2026-08-11): with staged market-worker shutdown and final hot-set persistence active, the final optimized standalone Windows run retried the explicit command until IPC readiness, acknowledged at `2,616.279 ms`, measured `10,272,768` bytes immediately before the successful request, wrote `hot-set-0000000002.frame`, and exited with code zero `1.713 ms` later with no remaining engine process. A second run wrote revision 3 while retaining exactly revisions 2 and 3. Deterministic tests separately prove in-flight history cancellation plus successful joins within one second, name an intentionally uncancellable worker when a 20 ms deadline expires, and reproduce/reject stale equal-revision hot-set overwrite across restart. These remain isolated lifecycle samples and fixtures rather than startup/reopen percentiles, connected-provider teardown, or endurance evidence, so Phase 10 remains unchecked.

Progress evidence (2026-08-11): after native environment-monitor cancellation and join ownership was added, the optimized standalone engine exposed 19 process threads before shutdown, acknowledged the authenticated request at `2,706.200 ms`, measured `10,203,136` bytes of working set, and exited with code zero `8.671 ms` later with no remaining process. Deterministic platform tests prove that both Windows native waiters unblock on cancellation, while the production Rithmic catalog and realtime workers retain and join both named helpers. This is one cold complete-exit sample rather than connected-provider teardown, warm-reopen, or percentile evidence, so Phase 10 remains unchecked.

Only after correctness:

Measure:

- startup,
- warm attach,
- chart demand latency,
- timeframe switching,
- tab switching,
- provider-to-engine latency,
- engine-to-desktop latency,
- frame latency,
- memory,
- queue occupancy,
- reconnect.

Then optimize observed hot spots.

---

# 119. `ARCHITECTURE.md`

- [x] **Status: Verified complete**

Evidence (2026-08-18): `ARCHITECTURE.md` is synchronized with manifests and call paths. It explicitly assigns provider sessions, canonical market state, shared subscriptions, local persistence, lifecycle, and bounded publication to `axiusflow_engine`; assigns GPUI presentation and chart interaction to `axiusflow_desktop`; documents authenticated local IPC, persistence-independent first pixels, warm/markets-live/complete-exit behavior, independent multi-consumer charts, the current one-chart and `--multi-chart` surfaces, and the future desktop-owned tab/pane model. It also records the sole `chart_integration`/Origin boundary and the current unimplemented persistent multi-tab limitation without claiming target behavior as shipped.

Update the existing root `ARCHITECTURE.md`.

Do not create another architecture document.

The final architecture document must clearly state:

```text
axiusflow_engine owns provider sessions and market state
axiusflow_desktop owns GPUI presentation
```

Remove the old statement that the engine owns no provider market publication path.

Document:

- warm modes,
- multi-consumer architecture,
- shared subscriptions,
- persistence not gating first pixels,
- process lifetime,
- IPC semantics,
- multi-tab model.

Respect the repository rule against document sprawl.

---

# 120. `AGENTS.md`

- [x] **Status: Verified complete**

Update `AGENTS.md` with enforceable rules for future coding agents.

At minimum include:

1. GPUI cannot perform provider or persistent history work.
2. Desktop cannot import provider adapters directly.
3. Provider-specific types stop at adapters.
4. MarketEngine is the single market-demand owner.
5. Do not create per-chart provider sessions.
6. Do not create per-chart runtimes.
7. Timeframe switching does not recreate provider session merely because interval changed.
8. Valid in-memory visualization data is not blocked by persistence.
9. Stale generations never mutate current chart state.
10. No unbounded Loading state.
11. No duplicate runtime path.
12. New crates require a dependency-direction justification.
13. New traits require real polymorphism/test seam.
14. Replaced code must be deleted.
15. Compile success is not runtime success.

Evidence (2026-08-14): `AGENTS.md` now contains an enforceable market-architecture guardrail section covering GPUI blocking/provider/storage exclusions, provider-neutral desktop types, single `MarketEngine` demand ownership, shared provider runtimes, stable sessions across presentation changes, memory-before-persistence publication, generation fencing, bounded Loading/recovery, one runtime/IPC path, crate/trait justification, deletion of replaced code, and runtime verification beyond compilation.

---

# 121. CARGO DEPENDENCY RULES

- [x] **Status: Verified complete**

Final dependency direction conceptually:

```text
domain
  ↑
adapters
  ↑
provider_history
  ↑
local_history / local_storage
  ↑
market_engine
  ↑
engine_protocol
  ↑
apps/engine


engine_protocol
  ↑
apps/desktop
  ↑
ui
```

The exact Cargo graph may differ where shared types require it.

But the following must be impossible:

```text
market_engine → GPUI
provider adapter → GPUI
local_storage → GPUI
domain → GPUI
provider adapter → chart_integration
desktop UI → rithmic_protocol
desktop UI → coinbase_market
```

Evidence (2026-08-14): the remaining desktop imports of the Coinbase adapter and provider-history implementation were removed. Coinbase presentation now uses the existing provider-neutral `InstallProviderInstrument` protocol descriptor, including its exact entitlement identity, and desktop readiness evidence drives the application-owned gap/resnapshot model instead of a provider-history coordinator. Three deterministic `axiusflow_naming_check` tests inspect the authoritative manifests and reject UI dependencies from backend/domain/storage/adapters, provider adapters from chart integration, backend implementation crates from the desktop, or loss of the engine backend composition boundary.

---

# 122. DESKTOP CARGO CONSTRAINT

- [x] **Status: Verified complete**

`apps/desktop/Cargo.toml` should not directly depend on:

```text
coinbase_market
rithmic_protocol
provider_history
local_storage
local_history
market_engine
```

except temporarily during migration.

Final desktop dependencies should primarily be:

- GPUI/UI crates,
- application/presentation models,
- engine protocol/client support,
- Origin chart integration,
- platform runtime where needed.

Evidence (2026-08-14): `apps/desktop/Cargo.toml` no longer depends on `coinbase_market` or `provider_history` and has none of the forbidden Rithmic adapter, storage, history, or `market_engine` dependencies. Its shipping and diagnostics paths compile through application/domain models, local engine protocol/client support, observability, platform runtime, UI crates, GPUI, and Origin integration. The manifest constraint is covered by the workspace dependency-direction regression test.

---

# 123. ENGINE CARGO CONSTRAINT

- [x] **Status: Verified complete**

`apps/engine/Cargo.toml` owns backend composition.

It may depend on:

- market_engine,
- adapters,
- local history,
- storage,
- provider history,
- platform runtime,
- observability,
- engine protocol.

Evidence (2026-08-14): `apps/engine/Cargo.toml` is the backend composition root and directly owns both provider adapters, canonical market data, `market_engine`, encrypted desktop storage, provider history, platform runtime, local engine protocol/client support, IPC transport, randomness, and secret zeroization. It has no GPUI, chart-integration, terminal-UI, or design-system dependency. The mandatory workspace test gate includes a manifest regression that fails if required engine composition edges disappear or UI edges appear.

---

# 124. PROVIDER ADAPTER CONSTRAINT

- [x] **Status: Verified complete**

Adapters may depend on:

- canonical domain,
- provider-specific wire/network dependencies,
- provider history contracts where appropriate,
- transport/platform primitives.

They may not depend on UI.

Evidence (2026-08-14; storage crate renamed 2026-08-18): the unused Coinbase adapter production dependency on the crate now named `local_storage` was removed from its manifest and lockfile entry. Coinbase and Rithmic production dependencies are now limited to canonical market data, provider-history contracts, platform/observability primitives, and provider wire/network/security libraries. Their storage/history conformance fixtures remain dev-only, neither adapter depends on UI, and a deterministic dependency-direction test parses only each production dependency table to prevent storage or UI regression.

---

# 125. NO CROSS-LAYER CONVENIENCE IMPORTS

- [x] **Status: Verified complete**

Do not bypass architecture because importing a lower-level type is convenient.

If GPUI needs information:

add it to a proper engine publication/application model.

Do not import Rithmic structs into GPUI.

Evidence (2026-08-14): presentation manifests contain no Coinbase or Rithmic adapter dependency, chart integration contains neither adapter, and repository source inspection finds no provider-adapter crate import in the desktop or UI crates. Coinbase presentation uses the provider-neutral `InstallProviderInstrument` engine-protocol descriptor; Rithmic presentation types are app-local UI state built from provider-neutral protocol publications rather than adapter structs. Cargo therefore rejects a new cross-layer provider import unless the forbidden dependency is deliberately reintroduced, which the dependency-direction regression tests also reject.

---

# 126. FAIL FAST ON ARCHITECTURAL VIOLATIONS

- [x] **Status: Verified complete**

Evidence (2026-08-14): Cargo manifests encode the runtime dependency direction, and the mandatory workspace test gate now reads those manifests to reject UI dependencies in backend/domain/storage/provider layers, provider implementations in chart integration or desktop, storage/UI dependencies in provider adapters, and UI dependencies or missing backend composition in the resident engine. Illegal Rust imports therefore fail at compile time, while manifest-level regression tests fail before an architectural dependency can silently enter an allowed crate.

Where practical, enforce boundaries using Cargo dependencies.

A compiler error caused by illegal dependency direction is preferable to a comment saying "don't do this."

---

# 127. CONSUMER CLEANUP

- [x] **Status: Verified complete**

Evidence (2026-08-14): authenticated protocol-v10 `RemoveConsumer` reaches the resident coordinator owner, which removes only the authorized consumer's pending history waiter, conflated publication, and demand before releasing only live data no longer shared. Deterministic regressions prove an active Rithmic replay is cancelled when its final consumer is removed, twenty charts share one provider generation while a removed chart cannot poll and another continues receiving live updates, and the real local-socket client can remove Chart A while Chart B immediately submits a newer generation and receives its covering snapshot.

When a chart closes:

```text
RemoveConsumer(ChartId)
```

Engine:

- cancels that consumer's obsolete history interests,
- removes publication state,
- decrements subscriptions,
- keeps shared state used elsewhere.

Closing Chart A must not disconnect Chart B.

---

# 128. TAB CLEANUP

- [ ] **Status: Not verified complete**

Closing a workspace:

remove only consumers belonging to that workspace.

Do not globally reset MarketEngine.

---

# 129. UI RESTART

- [x] **Status: Verified complete**

Evidence (2026-08-14): every authenticated engine session records its attached client identity. Local-socket EOF and framing failures leave the receive loop through one cleanup boundary that synchronously detaches that client before the server session returns. Detach removes only that client's pending waiters, publication state, and demand registry entries, then releases unused live work according to the active resource policy. An end-to-end abrupt-drop regression receives a real engine snapshot, drops the client without `DetachClient` or `Goodbye`, waits for the server session to finish, proves the old publication is unavailable, and re-registers the same identity successfully; lower-level ownership coverage proves another client's consumers remain intact.

If desktop crashes or is killed while warm engine remains:

Engine should detect IPC disconnect.

Remove or downgrade detached client consumer demands after a bounded policy.

Do not leak consumers permanently.

---

# 130. ENGINE RESTART

- [ ] **Status: Not verified complete**

If engine restarts while desktop is open:

Desktop:

- detects disconnect,
- shows engine reconnect state,
- EngineSupervisor restarts/reconnects,
- re-registers active consumers,
- restores visible state from local persisted/chart snapshot if available,
- resumes.

Do not freeze GPUI.

---

# 131. PROVIDER RECONNECT

- [x] **Status: Verified complete**

Evidence (2026-08-15): Coinbase disconnect handling keeps both registered consumers and their covering history, publishes generation-qualified `Recovering`, repairs history, replays the bounded live suffix, and resumes a forming snapshot under the next provider generation without reconstructing desktop state. Rithmic now retains its prior engine snapshot as usable `Partial` state when a newer session starts, resets only the live handoff, requests generation-fenced local/provider covering repair, and replaces the retained snapshot only after validated history arrives. Deterministic regressions prove both paths, while adapter lifecycle tests prove reconnect uses a fresh fenced Rithmic session.

Provider reconnect must not rebuild the entire desktop state.

Engine marks affected market streams recovering.

Local historical series remains available.

Upon reconnect:

- verify continuity,
- repair gaps,
- refresh covering state,
- resume.

---

# 132. ERROR LOCALIZATION

- [x] **Status: Verified complete**

Evidence (2026-08-15): coordinator provider and provider-wide series publications are filtered by each consumer's current provider demand, so Coinbase recovery cannot mutate a Rithmic chart and Rithmic state cannot enter a Coinbase mailbox. A valid-identity Rithmic aggregation failure now marks only the affected instrument's cadence series `Partial`, retains its covering snapshots, and requests repair while unrelated instruments and the Rithmic provider remain `Online`; only malformed provider/session identity causes provider-wide recovery. Existing deterministic storage-failure regressions prove providers and live publication continue under `PersistenceState::Degraded`, and the native desktop/engine process boundary prevents chart-rendering failure from terminating market ownership.

An error in Rithmic must not break Coinbase.

An error in one instrument must not globally invalidate every chart.

A storage error must not destroy provider connectivity.

A chart rendering error must not terminate engine.

Use ownership boundaries for failure isolation.

---

# 133. MULTI-PROVIDER SERIES IDENTITY

- [x] **Status: Verified complete**

Evidence (2026-08-18): canonical `BarSeriesKey` identity contains provider, instrument, entitlement, exact period, and definition revision, and protocol conversion validates the same provenance. A new `MarketEngine` regression registers Coinbase and Rithmic, submits equal `BTC` instrument labels under distinct provider/entitlement identities, installs different data, and proves the engine retains two subscriptions and two independent stored series without cross-publication or merging.

A canonical series identity must include enough provider/account provenance that:

```text
Coinbase BTC
```

cannot accidentally merge with:

```text
another provider BTC
```

when source semantics differ.

Retain source/provenance correctness.

---

# 134. FUTURE TRADING EXECUTION

- [x] **Status: Verified complete**

Evidence (2026-08-15): the versioned local protocol exposes workspace, lifecycle, provider catalog/control, market demand, history/series, and order-book presentation messages only. It contains no place, submit, replace, cancel, flatten, position, execution-report, or other trading action, and no adapter or desktop path exposes execution. A repository conformance test rejects execution command names in the market-data IPC schema, preserving a future separate non-conflatable safety boundary instead of smuggling orders into presentation queues.

Do not implement trade execution in this migration.

But preserve a future clean boundary.

Future order actions are control-plane messages and must never be treated as conflatable market display updates.

Do not mix future order execution into chart publication queues.

---

# 135. SECURITY

- [x] **Status: Verified complete**

Evidence (2026-08-11): every credentialed shipping application market path runs inside `axiusflow_engine` and loads bounded opaque credential bytes from `NativeCredentialVault`; the desktop Rithmic bridge has no vault, credential, provider-runtime owner, provider-network import, Rithmic adapter dependency, or deleted `desktop_provider_runtime` dependency. Provider-neutral protocol v10 catalog commands/results contain only bounded identity, generation, entitlement revision, precision, and presentation metadata, never credentials. The existing installation token and encrypted-history keys remain native-vault backed, secret bytes are zeroized at the Rithmic adapter's provider-runtime boundary, and repository searches plus deterministic protocol tests confirm that credentials do not cross IPC or enter logs/persistence.

Provider credentials remain local.

Engine retrieves them through native credential mechanisms.

Desktop should not log credentials.

IPC should not expose credentials.

Do not persist credentials in plain text.

Do not put secrets in forensic logs.

---

# 136. BENCHMARKS

- [x] **Status: Verified complete**

Evidence (2026-08-11): the deterministic release suite now covers every minimum local boundary without provider-network variance. `axiusflow_market_data_performance` measures encrypted cold publication, warm catalog/read/decode, first recent segment, cold interval derivation, and repeated immutable derived-cache lookup through production storage and aggregation. Schema 8 of the desktop `--windowed-benchmark` measures real GPUI/Origin startup, direct same-series covering snapshot installation, replay-to-frame, frame-registration duration, production symbol-input/instrument/interval foreground handlers, callback cadence, native compositor progress, process working-set growth, and exact before/after chart-queue occupancy with overflow rejection. The ignored engine verifier measures cached direct demand-to-snapshot, authenticated IPC demand-to-decoded-snapshot, cached timeframe and symbol switching, connect/authenticate/attach/restore, and a complete 20-consumer shared-series batch; it also enforces the Sections 102 and 103 p50/p95 budgets. Provider connection, authentication, history, and realtime startup remain explicitly separate credentialed evidence.

Maintain deterministic local benchmarks.

At minimum measure:

```text
cold local history read
warm history read
derived interval creation
repeated derived lookup
engine demand → snapshot
IPC snapshot latency
desktop snapshot → frame
multi-consumer scaling
```

Separate network/provider benchmark results.

---

# 137. MULTI-CHART PERFORMANCE TARGET

- [ ] **Status: Not verified complete**

Test representative workstation use.

Example:

```text
5 tabs
4 charts each
20 charts
```

Requirements:

- tab switching stays responsive,
- no per-chart thread explosion,
- no per-chart provider session explosion,
- shared data is reused,
- background charts do not continuously force full-rate redraw,
- provider streams remain healthy.

Record CPU and memory rather than guessing.

---

# 138. MEMORY SHARING

- [x] **Status: Verified complete**

Evidence (2026-08-18): completed canonical history is shared through one immutable `Arc<[MarketBar]>` and the mutable forming tail is stored separately. A focused regression revises that tail repeatedly and proves the completed-history pointer remains identical, then proves a single new immutable allocation is created only when the completed tail rolls into history. Twenty simultaneous consumers continue to share the same canonical allocation.

Where appropriate:

```text
Arc<immutable series>
```

can be shared.

Do not create a full deep copy per chart.

Desktop may create chart-engine-specific vertex/geometry representations where necessary, but canonical market history should remain shared where practical.

---

# 139. UI RENDER CADENCE

- [x] **Status: Verified complete**

Evidence (2026-08-18): the desktop mailbox raises one edge-triggered wake until it is drained, and `FramePollGate` admits at most one pending GPUI next-frame poll for an active window. Between frames, superseded live tails conflate safely; at the frame boundary `ChartDataBridge` validates append-or-replace semantics and Origin receives one merged streaming update. Deterministic mailbox and chart tests cover one wake for 63 revisions plus replacement and contiguous append through one frame drain. An optimized Windows `--windowed-benchmark` run rendered 256 measured frames with chart queue depth `0/1`, zero overflows, `6.8755 ms` update-to-frame p99, `24.3698 ms` first pixel, an advancing 420-refresh DWM timeline, and zero late, dropped, or missed-frame growth.

Market data may arrive faster than screen refresh.

The UI should render based on frame demand.

Do not schedule an unlimited sequence of GPUI redraws.

Conflate presentation updates between frames when semantically safe.

---

# 140. NO GLOBAL UI BUSY LOOP

- [ ] **Status: Not verified complete**

The market stream must not keep GPUI permanently busy.

Input remains responsive during:

- history backfill,
- depth streaming,
- rapid symbol changes,
- rapid timeframe changes,
- tab changes.

---

# 141. PERFORMANCE ORDER OF OPERATIONS

- [ ] **Status: Not verified complete**

Optimize in this order:

1. remove unnecessary work,
2. simplify ownership,
3. remove duplicate copies,
4. avoid unnecessary provider calls,
5. aggregate incrementally,
6. improve cache locality,
7. reuse allocations,
8. only then consider specialized low-level optimization.

Do not begin with unsafe code or shared-memory IPC.

---

# 142. SHARED MEMORY IS NOT PHASE 1

- [x] **Status: Verified complete**

Evidence (2026-08-15): desktop/engine communication uses bounded length-prefixed protobuf messages over `interprocess` platform-local sockets. No workspace manifest or production source introduces memory mapping, shared-memory crates, shared-memory platform calls, or a shared-memory ring buffer. A conformance scan fails if those primitives appear, preserving measurement as the prerequisite for any future transport replacement.

Do not introduce shared-memory ring buffers merely because this is a trading platform.

Start with simple bounded local IPC.

Measure.

Only optimize IPC after evidence proves serialization/copying is significant.

---

# 143. DO NOT REPLACE EVERYTHING WITH ACTORS

- [ ] **Status: Not verified complete**

Message ownership is useful.

But do not create one actor per struct.

The architecture should remain understandable.

One coordinator plus provider/history/storage workers is preferable to fifty tiny actor services.

---

# 144. EXPECTED FINAL MENTAL MODEL

- [ ] **Status: Not verified complete**

A developer should be able to explain Axiusflow in one minute:

> Axiusflow Desktop is a GPUI client. It creates chart and workspace consumers and tells the local Axiusflow Engine what data they need. The Engine owns provider sessions, history, caches, order books, aggregation, subscriptions, and persistence. Multiple charts share the same engine data and provider streams. The Engine publishes versioned snapshots/deltas back to Desktop over local IPC. It can optionally remain warm after the UI closes. Provider-specific protocol types never escape their adapters.

If the system requires a ten-minute explanation involving many overlapping runtimes, simplify it.

---

# 145. EXPECTED FINAL CHART FLOW

- [x] **Status: Verified complete**

Evidence (2026-08-18): authenticated engine IPC tests drive provider-neutral series demand through the resident coordinator and receive a canonical snapshot. Engine regressions prove memory reuse, local-history-first progressive publication, provider repair, live continuation, and publication before asynchronous persistence failure handling. Desktop conversion tests preserve fixed-point provenance and incremental tails, and chart integration accepts the first real snapshot into Origin. Naming conformance now requires those boundary regressions to remain present.

For BTC 1m:

```text
ChartPanel
↓
EngineClient
↓
SetSeriesDemand
↓
IPC
↓
MarketEngine DemandRegistry
↓
SeriesStore lookup
├── hit → publish immediately
└── miss
    ↓
    local history
    ├── data → publish partial
    └── gaps
        ↓
        provider history
        ↓
        canonical bars
        ↓
        SeriesStore
        ├── publish
        └── persistence asynchronously
```

Realtime:

```text
provider socket
↓
adapter decode/validate
↓
canonical event
↓
MarketEngine
↓
update current series/book
↓
publication
↓
IPC
↓
desktop
↓
chart
```

---

# 146. EXPECTED TIMEFRAME FLOW

- [x] **Status: Verified complete**

Evidence (2026-08-18): a consumer generation change immediately replaces interval demand without recreating the shared realtime session. Deterministic engine tests prove a cached compatible one-minute series is aggregated and published as five-minute data before provider refresh, the derived result is cached, finer data is never fabricated from a coarser source, repeated demand hits canonical memory, and symbol/interval switching retains the same process-owned realtime session.

BTC 1m → 5m:

```text
Chart consumer generation +1
↓
SetSeriesDemand(BTC, 5m)
↓
MarketEngine
↓
5m cached?
├── yes → publish
└── no
    ↓
    compatible source cached?
    ├── yes
    │   ↓
    │   aggregate
    │   ↓
    │   cache
    │   ↓
    │   publish
    │
    └── no
        ↓
        fetch only missing source/provider data
        ↓
        publish progressively
```

No provider session recreation merely because interval changed.

---

# 147. EXPECTED SYMBOL FLOW

- [x] **Status: Verified complete**

Evidence (2026-08-18): newer per-consumer demand becomes authoritative immediately, cancels obsolete history interest without waiting for cleanup, and rejects late results through generation fencing. Engine tests prove cached symbol state publishes immediately, BTC-to-ETH switching reuses the shared provider session, another consumer's matching demand and canonical series remain independent, and stale history cannot overwrite the newer chart generation. Conformance requires the cancellation and shared-session regressions to remain present.

BTC → ETH:

```text
Chart generation +1
↓
new ETH demand authoritative immediately
↓
engine checks ETH warm state
↓
publish available data
↓
start missing work
```

Old BTC request may finish later.

It is rejected for that chart because generation is stale.

Other BTC charts remain unaffected.

---

# 148. EXPECTED MULTI-TAB FLOW

- [ ] **Status: Not verified complete**

Tab A contains four charts.

Tab B contains four charts.

Switch A → B:

```text
GPUI changes visible workspace
↓
visibility priorities updated
↓
existing ChartViewModels displayed
↓
engine continues shared state
```

This is NOT:

```text
destroy Tab A provider stack
↓
create Tab B provider stack
```

---

# 149. EXPECTED WARM REOPEN FLOW

- [ ] **Status: Not verified complete**

User previously viewed:

```text
ES 1m
NQ 5m
BTC 15m
```

Engine kept warm.

Desktop closed.

Later desktop opens:

```text
EngineSupervisor connects
↓
workspace consumers restored
↓
engine has ES/NQ/BTC hot series
↓
snapshots sent
↓
charts render
```

Provider network is not required before rendering those cached states.

---

# 150. DEFINITION OF MIGRATION SUCCESS

- [ ] **Status: Not verified complete**

The migration is NOT successful because:

- Cargo builds,
- tests compile,
- new crates exist,
- architecture document looks clean,
- the daemon starts,
- logs say Connected,
- candles appear in stdout.

Success requires observable runtime behavior.

At minimum:

1. Coinbase historical bars visibly render.
2. Coinbase realtime continues.
3. Rithmic historical bars visibly render where credentials/entitlements permit.
4. Rithmic realtime continues.
5. Chart never remains indefinitely Loading after terminal failure.
6. Storage failure does not hide valid in-memory chart data.
7. BTC 1m → 5m → 15m → 1h → 1m works repeatedly.
8. BTC → ETH → BTC works repeatedly.
9. Rapid switching does not hang.
10. Multiple charts work simultaneously.
11. Multiple workspace tabs work.
12. Two charts requesting the same instrument share backend state.
13. Closing one chart does not interrupt another.
14. Closing desktop in warm mode leaves only engine running.
15. Reopening desktop attaches to warm engine.
16. Complete exit terminates both processes.
17. Cold start still works.
18. Provider failure remains isolated.
19. GPUI remains responsive while market data streams.
20. No duplicate legacy market runtime remains.

---

# 151. FINAL DELIVERY REQUIREMENT

- [ ] **Status: Not verified complete**

Do not return with:

> "Implemented the new architecture."

Instead provide evidence.

For each migration phase report:

- files added,
- files removed,
- files moved,
- responsibility changes,
- Cargo dependency changes,
- runtime tests performed,
- logs/timings,
- remaining legacy path,
- next migration phase.

At final completion provide:

### Final process topology

### Final Cargo dependency graph

### Final source tree

### Final production LOC by crate

Exclude:

- generated protobuf,
- vendor files,
- tests,
- Origin chart repository.

### Deleted legacy files/crates

### Runtime verification results

### Performance measurements

### Known remaining limitations

---

# 152. MOST IMPORTANT RESTRAINT

- [ ] **Status: Not verified complete**

Do not solve complexity by adding complexity.

Do not turn:

```text
desktop_market_runtime
```

into:

```text
desktop_market_runtime
+
market_engine
+
engine_orchestrator
+
provider_supervisor
+
stream_service
```

The migration should DELETE layers.

The final system should have fewer owners than the current one.

---

# 153. FINAL ARCHITECTURAL CONTRACT

- [ ] **Status: Not verified complete**

When implementation is complete, these statements must all be true:

1. `axiusflow_desktop` owns presentation.
2. `axiusflow_engine` owns market state.
3. `MarketEngine` is the sole market-demand authority.
4. Provider adapters own provider protocols.
5. Storage is downstream of usable in-memory market state.
6. Valid chart data is never unnecessarily blocked on persistence.
7. Charts are consumers, not backend runtimes.
8. Workspace tabs are presentation containers, not backend instances.
9. Multiple charts share provider subscriptions and cached data.
10. A timeframe switch does not reconstruct the provider.
11. A symbol switch does not block on obsolete work.
12. Generations make stale work harmless.
13. Provider threads never depend on GPUI progress.
14. GPUI never performs provider/storage work.
15. The engine may live longer than the UI when the user opts in.
16. The engine may be completely terminated when the user opts out.
17. After reboot, warm state is reconstructed from persisted hot-set/local data; RAM is not magically preserved.
18. Rithmic protobuf types never leak above the Rithmic adapter.
19. Origin Charts remains independent.
20. No duplicate legacy runtime remains after migration.
21. No unbounded Loading state exists.
22. No per-chart provider session exists.
23. No per-chart runtime exists.
24. No hidden dead code remains solely because it existed before.
25. Runtime correctness, not compilation, defines success.

Build toward this architecture incrementally, prove each stage with running behavior, remove the architecture it replaces, and stop adding layers unless measured requirements actually demand them. 

# MANDATORY ADDENDUM — RESTORED ARCHITECTURAL GUARANTEES

The following requirements are part of the architecture specification and have the same authority as all previous sections.

If any earlier wording appears weaker than a rule below, follow the stricter rule below.

---

# 154. COINBASE REALTIME IS A MIGRATION GATE

- [x] **Status: Verified complete**

Evidence (2026-08-11): current protocol-v5 bounded market-event polling carries explicit provider/series state and forming-tail covering snapshots from the resident engine to the existing desktop model and Origin chart. Deterministic engine tests drive historical installation, a live active candle, deliberate disconnect, provider-generation recovery, history repair, and resumed active publication for two unchanged consumers; the unaffected consumer retains its covering history and neither desktop consumer is reconstructed. A capacity-one queue test proves overflow closes and restarts the provider generation, store tests reject completed-bar overlap rewrites while permitting only the forming tail to revise, and the Coinbase socket test makes established-session cancellation terminal through buffered TLS/WebSocket readers. Two exact-final-source Windows release captures twenty seconds apart changed 12,850 sampled chart-region pixels while the window remained responsive and healthy. The same native lifecycle observed two Coinbase TLS connections while streaming and one after desktop exit, proving the realtime WebSocket closed within three seconds while the bounded REST agent retained its idle pooled connection.

Do not interpret successful Coinbase historical candles as completion of the Coinbase migration.

Before beginning the Rithmic migration, Coinbase must work end-to-end for BOTH historical and realtime data.

Required sequence:

```text
Coinbase provider connection
→ historical request
→ canonical historical bars
→ in-memory SeriesStore
→ covering SeriesSnapshot
→ desktop/Origin visible candles
→ realtime subscription
→ verified history/live handoff
→ active candle updates
→ continued streaming
```

Required runtime proof:

```text
historical snapshot
→ realtime handoff
→ live active candle
```

Test deliberate Coinbase disconnect/reconnect.

Verify:

* historical bars remain visible during reconnect where valid,
* realtime state becomes explicitly Recovering/Disconnected,
* no fake Live state remains,
* reconnect does not reconstruct desktop state,
* reconnect does not invalidate unrelated consumers,
* missing continuity is repaired,
* active candle resumes,
* GPUI remains responsive.

DO NOT begin Rithmic migration until this works repeatedly.

This is a hard migration gate.

---

# 155. FIRST VERTICAL SLICE REMAINS EXTREMELY SMALL

- [x] **Status: Verified complete**

Evidence (2026-08-11): code scope is limited to protocol publication metadata, the existing `market_engine`, the resident engine service, one desktop engine-client bridge, and the existing Coinbase history/application/Origin boundaries. Rithmic, realtime, order book, footprint, shared memory, storage migration, and advanced warm policy were not added. Authenticated IPC and desktop conversion tests pass, and the clean Windows release run visibly rendered cold BTC-USD provider history through the spawned engine and Origin.

The first new engine path must not contain every eventual feature.

The first proving slice is:

```text
Desktop
→ local IPC
→ Engine
→ Coinbase
→ canonical BTC-USD historical bars
→ Engine in-memory state
→ IPC snapshot
→ desktop chart integration
→ Origin
→ visible GPUI candles
```

For this first slice:

NO Rithmic requirement.

NO footprint requirement.

NO order-book requirement.

NO OS background daemon requirement.

NO shared-memory IPC requirement.

NO advanced hot-set algorithm requirement.

NO massive tick optimization requirement.

Prove the process and ownership boundary first.

Then add Coinbase realtime.

Then prove switching/multiple consumers.

Then continue the migration.

---

# 156. PROVIDER CAPABILITY MODEL

- [ ] **Status: Not verified complete**

Do not assume every provider has identical semantics.

Coinbase and Rithmic may differ in:

* supported realtime stream types,
* historical capabilities,
* native bar intervals,
* depth history,
* background-session permission,
* account/session constraints,
* entitlements,
* reconnect semantics,
* subscription limits,
* historical/live independence,
* provider-specific rate limits.

Generic MarketEngine code must not contain scattered logic such as:

```rust
if provider == Provider::Rithmic {
    ...
}
```

throughout unrelated modules.

Provider capabilities must be represented at the provider boundary.

Conceptually, capabilities may include:

```text
supports_background_live
supports_historical_without_live
supports_native_interval
supports_trade_history
supports_depth_history
supports_order_book
supports_quotes
supports_server_bars
supports_local_aggregation_source
```

Do not implement fields that have no current use merely because they appear above.

But when runtime behavior genuinely differs between providers, expose that difference through a provider capability/contract rather than hardcoding provider names across MarketEngine.

`KeepMarketsLive` must respect provider licensing, entitlement, API and session rules.

---

# 157. PROVIDER NETWORK MUST BE REPLACEABLE IN TESTS

- [x] **Status: Verified complete**

Evidence (2026-08-14): the production coordinator exposes only two private provider-owner seams, `HistorySource` and `RealtimeSource`; the Rithmic adapter separately substitutes its real provider-session driver, history transport and credential-vault boundaries and uses local TLS fixtures for wire behavior. Deterministic sources drive immediate and delayed history, failure, cancellation, queue saturation, retained-history repair, realtime start/disconnect/reconnect, queue overflow and generation restart through production owners. Provider adapter, provider-history, engine, authenticated IPC, and desktop model tests additionally cover stale and duplicate data, out-of-order/gapped sequences, explicit empty history, depth gaps, and covering recovery. The mandatory workspace suite requires no live Internet, Rithmic credentials, CME entitlement, or provider timing; credentialed Rithmic checks remain separate smoke binaries.

MarketEngine correctness must not depend on live internet access.

Build deterministic provider test seams through legitimate provider boundaries.

Do not create an elaborate mocking framework.

The test seam should support controlled scenarios such as:

```text
history succeeds immediately
history succeeds after delay
history fails
history never responds until timeout
realtime begins
realtime disconnects
realtime reconnects
stale history result arrives
out-of-order sequence arrives
depth gap occurs
provider sends duplicate data
provider returns empty range
```

This is especially important for Rithmic.

CI must NOT require:

* Rithmic credentials,
* live CME entitlement,
* a particular provider being online,
* network timing behaving predictably.

Real credentialed provider tests remain separate smoke/integration evidence.

Provider smoke success does NOT prove chart success.

Required layers of verification are:

```text
provider adapter test
↓
MarketEngine test
↓
IPC integration test
↓
desktop integration test
↓
real provider smoke
```

---

# 158. NO IPC INSIDE THE ENGINE

- [x] **Status: Verified complete**

Evidence (2026-08-14): protobuf framing and local sockets terminate at the authenticated `apps/engine` server boundary. The coordinator, `MarketEngine`, history/storage/realtime workers, aggregators, and provider adapters communicate through owned values, direct calls, immutable `Arc` snapshots, and bounded standard-library channels; no internal message is serialized and there is no HTTP or local-service hop. `market_engine`, application, and domain manifests contain no engine protocol, transport, interprocess, protobuf, or async-runtime dependency, and mandatory manifest regressions reject those dependencies.

The desktop↔engine process boundary is the IPC boundary.

Inside `axiusflow_engine` and `market_engine`, use normal Rust:

* ownership,
* function calls,
* typed messages,
* bounded channels where required,
* shared immutable snapshots where appropriate.

Do NOT reproduce the old cloud architecture internally.

Forbidden unless measurement later proves otherwise:

```text
MarketEngine
→ protobuf encode
→ internal channel
→ protobuf decode
→ HistoryCoordinator
```

Do not serialize internal market messages merely because desktop↔engine IPC uses a schema.

Do not create HTTP endpoints between engine modules.

Do not create local microservices inside `axiusflow_engine`.

The process boundary already provides isolation.

---

# 159. PURE FUNCTIONS MUST REMAIN PURE

- [x] **Status: Verified complete**

Evidence (2026-08-14): interval bucketing and aggregation, fixed-point conversion, coverage planning, sequence validation, cache-key construction, page validation, canonical provider conversion, and IPC projection remain ordinary deterministic functions/modules with direct unit or conformance tests. Application, domain, and `market_engine` own no runtime, IPC, transport, thread, or async dependency, enforced by Cargo direction and mandatory manifest tests. Workers exist only around independently blocking provider, persistence, process, and UI lifecycle boundaries.

Not every operation needs:

* an actor,
* a worker,
* a state machine,
* a generation,
* a request object,
* a cancellation token,
* a channel,
* a runtime service.

Operations such as:

```text
interval bucketing
1m → 5m aggregation
price conversion
coverage calculations
sequence comparisons
cache-key construction
page validation
```

should remain ordinary deterministic functions/modules when no independent lifecycle is required.

Example:

```rust
aggregate_1m_to_5m(...)
```

should not become:

```text
AggregationService
→ AggregationCoordinator
→ AggregationWorker
→ AggregationResponseBus
```

without a measured ownership reason.

Complexity belongs around I/O and lifecycle.

Math and transformations should remain easy to test.

---

# 160. NO MAGIC GLOBAL MARKET STATE

- [x] **Status: Verified complete**

Evidence (2026-08-14): repository-wide source inspection finds no mutable static `MarketEngine`, provider manager, subscription registry, or desktop market client. Bootstrap explicitly constructs `EngineState` and one process-owned `MarketService`; its coordinator exclusively owns the `MarketEngine`, provider generations, consumer demand, canonical series, publications, live handoffs, catalog and order book. Desktop workers own their clients and presentation mailboxes through scoped handles. Remaining production statics are immutable interval metadata, a serialized native-credential access lock, and bounded DNS/HTTP utility singletons; none owns market demand, provider generations, subscriptions, series, or publications.

Do not introduce a process-global mutable singleton as a shortcut during migration.

Avoid architecture such as:

```rust
static MARKET_ENGINE: ...
static PROVIDER_MANAGER: ...
static SUBSCRIPTIONS: ...
```

where arbitrary modules can mutate global runtime state.

Engine state should have explicit ownership established during bootstrap.

Desktop client state should also be explicitly owned.

One engine process does NOT mean one uncontrolled global variable.

---

# 161. WRAPPER COLLAPSE RULE

- [x] **Status: Verified complete**

Evidence (2026-08-14): the obsolete `application::stream_runtime`, `desktop_market_runtime`, `desktop_provider_runtime`, Rithmic desktop driver, duplicate provider event bridge, and market-protocol conversion chain are deleted. The surviving layers each own a present responsibility: adapters own provider wire behavior, the resident coordinator owns lifecycle and policy, `MarketEngine` owns canonical demand/state, authenticated IPC owns the process boundary, desktop bridges validate/project engine publications, and GPUI owns presentation. Mandatory manifest regressions prevent both deleted desktop runtime crates from re-entering the workspace or either application manifest.

Every runtime layer must justify its existence by at least one real responsibility:

```text
ownership
policy
transformation
process boundary
failure isolation
dependency boundary
```

A layer whose primary behavior is:

```text
A::load()
→ B::load()
```

and B does:

```text
B::load()
→ C::load()
```

and C does:

```text
C::load()
→ D::load()
```

must be challenged.

If no semantic responsibility changes between layers, collapse the wrappers.

This rule applies especially to the existing chain involving:

```text
application::stream_runtime
desktop_market_runtime
desktop_provider_runtime
rithmic desktop_driver
rithmic session
```

Do not recreate equivalent forwarding layers under new names.

---

# 162. ERROR WRAPPER COLLAPSE RULE

- [ ] **Status: Not verified complete**

Do not create meaningless error nesting such as:

```text
ProviderError
→ RuntimeProviderError
→ MarketRuntimeError
→ ApplicationStreamError
→ ChartError
```

when each layer merely changes the enum name.

Wrap errors only when adding useful semantic context.

Retain the original source error chain.

Structured errors should allow diagnostics to answer:

```text
what failed?
where?
for which consumer?
for which generation?
for which provider?
for which series?
what was the source error?
```

Do not stringify errors prematurely.

---

# 163. MULTIPLE DESKTOP INSTANCE POLICY

- [ ] **Status: Not verified complete**

Choose this deliberately.

For the current scope, prefer one primary `axiusflow_desktop` instance per user unless there is a concrete product requirement for multiple simultaneous desktop clients.

If only one desktop is supported:

* detect an existing active primary desktop,
* activate/focus it where practical,
* do not accidentally register two primary workspace owners.

If multiple desktop clients are later supported:

* every client gets a distinct `ClientId`,
* consumer ownership remains client-scoped,
* IPC subscriptions remain reference-counted,
* closing one client cannot destroy another client's market demand.

Do NOT accidentally support multiple desktop processes through undefined behavior.

---

# 164. ENGINE MUST NOT BECOME A ZOMBIE

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): `axiusflow_engine --shutdown` is a concrete authenticated process-termination command suitable for explicit user, update, uninstall, and incompatible-engine-replacement callers. It does not start an absent engine and returns failure when no engine is reachable. The server stops accepting connections, owns bounded top-level market-worker teardown, reports named stuck workers, and the verified production process exits without a remnant. The desktop's per-launch `--exit-with-desktop` policy now calls the same non-starting helper after its clients detach; a release native-close run left no desktop or engine process. Engine PID/client/provider/resource diagnostics, durable in-product lifetime control, and actual update/uninstall/system-session callers remain incomplete, so this section stays unchecked.

Warm background mode is a user-controlled product feature.

It must not create an immortal process.

Engine status must expose enough information to determine:

```text
engine PID/status
lifetime mode
connected desktop clients
provider connection states
hot/warm resource usage
shutdown state
```

There must be a clean engine shutdown command.

The following flows must be capable of terminating the engine:

* Exit Completely,
* application uninstall,
* application update when required,
* explicit user action,
* incompatible engine replacement,
* system/user-session shutdown.

Do not leave Axiusflow engine processes orphaned after uninstall/update.

---

# 165. SHUTDOWN HAS DEADLINES

- [ ] **Status: Not verified complete**

Progress evidence (2026-08-12): authenticated shutdown marks lifecycle state offline/suspended, freezes persistent workspace mutation, stops the accept loop and publication, rejects subsequent demand, and closes the IPC listener. One shared two-second process deadline covers a named final hot-set flush, cancellation of coordinator-owned history, Coinbase realtime, Rithmic control channels and their four native environment helpers, draining already-accepted local-history work, joining every top-level market worker, and the remaining client-session wait. A panic or expired flush/worker/client stage returns process failure before termination. Deterministic tests prove both native waiters unblock on cancellation, final-manifest restart truth, successful worker cancellation/join, and the uncancellable-worker deadline path; the optimized production process exited `8.671 ms` after acknowledgement with code zero and no remnant after helper ownership was added. Desktop `ExitWithDesktop` integration now begins client retirement without waiting on GPUI, awaits the bounded detach tasks in the app-quit future, and only then requests engine shutdown. The release native-close path left neither process. Durable user selection and system-session shutdown integration remain incomplete, so this section stays unchecked.

Graceful shutdown is bounded.

Conceptually:

```text
stop accepting new client demand
→ mark engine shutting down
→ cancel obsolete provider/history work
→ stop publications
→ close subscriptions
→ close provider sessions
→ flush bounded important persistence
→ persist hot-set metadata
→ close IPC
→ terminate
```

Every shutdown stage that can wait on external I/O must have bounded behavior.

Do not hang forever because:

* Rithmic does not acknowledge something,
* Coinbase socket shutdown stalls,
* a provider task ignores cancellation,
* filesystem sync blocks unusually,
* an obsolete history request refuses to terminate.

If graceful deadline expires:

* record diagnostic,
* preserve already-committed state,
* terminate safely.

`ExitWithDesktop` must actually exit.

---

# 166. IPC VERSION / UPDATE COMPATIBILITY

- [ ] **Status: Not verified complete**

Desktop and engine are separate binaries.

During application update, they may temporarily be different versions.

The IPC handshake must detect protocol incompatibility.

Do not process incompatible messages and hope for the best.

If versions are incompatible:

```text
handshake
→ incompatible version detected
→ clear diagnostic/state
→ terminate/restart stale engine where appropriate
→ launch compatible engine
→ reconnect
```

Never turn version incompatibility into:

```text
Loading forever
```

The versioning scheme must clearly distinguish:

* compatible protocol changes,
* incompatible protocol changes.

---

# 167. SCHEMA DISCIPLINE

- [x] **Status: Verified complete**

Evidence (2026-08-11; crate renamed 2026-08-18): Axiusflow IPC prost messages remain isolated in `crates/engine_protocol`, while Rithmic vendor protobuf remains isolated in `crates/adapters/rithmic_protocol`. The bounded decoder rejects oversized/over-buffered frames, missing payloads, malformed protobuf, and any protocol-version mismatch. Removed envelope tags remain explicitly unused; the new provider-neutral instrument command and acknowledgement use new tags 41 and 42. Protocol v8 and the `axiusflow-engine-v8` local socket deliberately fence incompatible older residents. Authenticated IPC tests exercise the new schema without importing any Rithmic wire type.

If protobuf is used for Axiusflow IPC:

* keep Axiusflow IPC schemas separate from Rithmic vendor protobuf,
* never reuse removed protobuf field numbers,
* reserve removed field numbers where appropriate,
* validate message/frame size,
* version the protocol deliberately.

Do not mix:

```text
Rithmic provider schema
```

with:

```text
Axiusflow desktop-engine schema
```

They have completely different ownership.

---

# 168. WORKSPACE REVISION / RESTORE SAFETY

- [ ] **Status: Not verified complete**

Warm engine mode means the engine may retain durable workspace/hot-set intent while the desktop is absent.

Therefore workspace restore must have explicit revision semantics.

Example:

```text
engine workspace revision = 52
desktop persisted revision = 49
```

The desktop must not blindly overwrite revision 52 with revision 49 simply because it just launched.

Use a small explicit conflict/authority rule.

Do not create a distributed database.

Keep this mechanism simple.

But stale desktop workspace state must not overwrite newer engine state silently.

Workspace persistence must never block market-data publication.

Presentation-only state such as:

* hover,
* temporary selection,
* crosshair,
* transient drag state,

does not need engine persistence.

---

# 169. MEMORY LEAK / LONG-RUNNING ENGINE CONTRACT

- [ ] **Status: Not verified complete**

Because `axiusflow_engine` may remain alive for hours or days, test long-running memory behavior.

Track at minimum:

```text
engine RSS
desktop RSS
hot cache bytes
derived cache bytes
history cache bytes
order-book bytes
IPC queued bytes
storage queued bytes
active consumers
active provider subscriptions
active history operations
```

A warm engine is allowed to intentionally use meaningful memory.

The requirement is not "low memory at all times."

The requirement is:

```text
intentional
bounded
observable
evictable
stable
```

Do not allow:

```text
close/reopen charts repeatedly
→ memory grows forever
```

or:

```text
switch symbols for six hours
→ every symbol remains permanently decoded
```

Eviction should consider:

* current visibility,
* workspace membership,
* recent access,
* reconstruction cost,
* user pin/hot-set,
* memory pressure.

---

# 170. TEST FILES MUST FOLLOW THE NEW ARCHITECTURE

- [x] **Status: Verified complete**

Evidence (2026-08-18): durable provider protocol, provider-history/handoff, storage lifecycle, `MarketEngine`, protocol/IPC handshake, desktop readiness, and forensic regression suites remain in the workspace. Deleted `desktop_market_runtime` and `desktop_provider_runtime` cannot regain manifests or Rust source. Repository conformance now requires the durable test entry points and preserves the resident-engine regressions proving valid history publishes from memory and advances live state even when persistence is degraded, retaining the historical `history.install_failed` invariant at its new owner.

After migration, retain tests for durable boundaries:

```text
provider adapter conformance
provider history conformance
storage lifecycle
MarketEngine conformance
engine protocol/IPC conformance
desktop integration smoke
forensic regressions
```

Remove tests whose only purpose was exercising deleted transitional runtime architecture.

Do NOT delete valuable bug regression tests merely to reduce LOC.

The historical `history.install_failed` regression must remain represented after the old implementation disappears.

---

# 171. PROVIDER CONTROL PLANE

- [ ] **Status: Not verified complete**

The desktop may need user-facing provider controls.

These remain commands to the engine.

Conceptual control messages may include:

```text
SetProviderEnabled
RequestProviderReconnect
GetProviderState
```

Do not expose provider session objects to the desktop.

Do not let settings UI call Rithmic/Coinbase adapter code directly.

Provider controls always pass through EngineClient/IPC.

---

# 172. ENGINE CLIENT MUST NOT WAIT FOR PROVIDER READINESS

- [x] **Status: Verified complete**

Evidence (2026-08-15): engine IPC readiness is emitted immediately after authenticated protocol handshake and before client attachment or any provider demand. A deterministic local-socket regression starts the engine market owner without a realtime provider, completes `EngineReady`, attaches a desktop client, and restores workspace state successfully. Provider sessions remain idle until engine-owned demand/control starts them, and their connecting, online, history, realtime, recovery, and failure states are later generation-qualified publications; none gates IPC attachment or locally available work.

Starting/attaching the engine and connecting a market provider are different states.

Required:

```text
engine process ready
≠
Coinbase ready
≠
Rithmic ready
≠
history ready
≠
realtime live
```

Desktop must be allowed to attach as soon as engine IPC is healthy.

Provider initialization continues independently.

This distinction is especially important for multi-tab startup.

One slow provider must not prevent locally available charts from another provider from rendering.

---

# 173. PROVIDER FAILURE ISOLATION

- [x] **Status: Verified complete**

Evidence (2026-08-15): Coinbase and Rithmic have independent provider generations, health, history lanes, realtime handoffs, and provider-scoped consumer publication. A new cross-provider regression proves Coinbase recovery and `Live` transitions cannot alter a Rithmic consumer. A two-instrument Rithmic regression forces an exact fixed-point aggregation failure, observes only that instrument become `Partial` and enqueue covering repair, and proves the sibling stays `Live` while provider health remains `Online`; malformed provider/session identity remains the explicit provider-wide failure scope. Existing tests separately prove Rithmic history cancellation does not block Coinbase history, storage failure does not stop provider/live progress, and order-book gaps invalidate only the affected reconstructed book until a covering snapshot.

A Rithmic failure must not globally degrade Coinbase.

A Coinbase failure must not destroy Rithmic state.

A failure for one Rithmic instrument must not invalidate unrelated Rithmic instruments without a provider-session-level reason.

An order-book sequence failure should invalidate that affected book/session scope, not every chart.

A storage failure must not disconnect providers.

A GPUI failure must not corrupt engine market state.

Explicit failure scopes are required.

---

# 174. MULTI-TAB STARTUP RESTORE ORDER

- [ ] **Status: Not verified complete**

With several workspace tabs and many charts, do not cold-load every chart at identical priority.

On desktop attach:

```text
1. restore UI/layout metadata
2. identify active workspace
3. register visible chart consumers first
4. publish existing hot/local state immediately
5. register/warm inactive workspace consumers at lower priority
6. continue provider repairs/background warming
```

Do not make:

```text
Tab 1 visible chart
```

wait behind:

```text
Tab 5 hidden chart historical backfill
```

Resource priority should generally be:

```text
visible active-tab charts
→ visible DOM/order-flow
→ adjacent/current workspace
→ inactive open tabs
→ watchlist/pinned warm state
→ broader background prefetch
```

This is essential once Axiusflow supports many workspace tabs.

---

# 175. MULTI-CHART FAIRNESS

- [ ] **Status: Not verified complete**

One expensive chart must not starve all other charts.

Examples:

* one chart requests years of tick history,
* one footprint requests dense historical trades,
* one DOM requires high-rate depth,
* another chart only needs 500 recent bars.

History scheduling and resource policy must preserve responsiveness across consumers.

Visible/recent small demands should not sit indefinitely behind one enormous background request.

Use bounded/fair scheduling where necessary.

Do not create unlimited parallel provider requests as the solution.

---

# 176. LINKED CHARTS ARE A DESKTOP FEATURE, NOT PROVIDER DUPLICATION

- [ ] **Status: Not verified complete**

Future chart linking may allow several charts to follow the same:

* symbol,
* crosshair/time,
* interval group,
* workspace selection,

depending product design.

Implement linking as presentation/application demand coordination.

Example:

```text
Chart A symbol changes to ES
→ desktop link-group logic updates Chart B demand
→ Chart A and Chart B each retain their own ConsumerId/generation
→ engine deduplicates shared backend requirements
```

Do not implement linked charts by sharing one mutable ChartViewModel between several panes.

Do not create another provider subscription merely because two linked charts show ES.

Crosshair synchronization is primarily desktop/chart presentation state and should not be routed through provider runtime.

---

# 177. CODE SIZE GUARDRAIL CORRECTION

- [ ] **Status: Not verified complete**

Do not aggressively force the platform into 45,000–60,000 lines merely because a previous architecture prompt mentioned that range.

The platform now explicitly includes:

* separate local engine,
* desktop client,
* multiple workspace tabs,
* multi-chart consumer architecture,
* two providers,
* local history,
* storage,
* order books,
* observability,
* IPC,
* warm background engine modes,
* cross-platform lifecycle support.

A safer architectural guardrail for handwritten production Rust is approximately:

```text
50,000–70,000 LOC
```

excluding:

* Origin chart repository,
* generated protobuf Rust,
* vendor `provider_kit`,
* tests,
* fixtures,
* benchmarks,
* generated code.

This is NOT a quota.

Do not compress readable Rust.

Do not delete necessary correctness logic to hit a number.

Do not add 20,000 lines merely to perform the migration.

Use LOC to expose:

* duplication,
* dead runtime paths,
* wrapper chains,
* abandoned implementations,
* over-generalized infrastructure.

The desired direction during migration is:

```text
new ownership established
+
old ownership deleted
=
LOC stable or reduced where practical
```

not:

```text
old 80k
+
new architecture 40k
=
120k platform
```

---

# 178. ROUGH RESPONSIBILITY SIZE CHECK

- [ ] **Status: Not verified complete**

These are diagnostic guardrails only.

Conceptually:

```text
domain/shared semantics          small
apps/engine process shell        small
desktop EngineClient             small
MarketEngine                     medium
provider adapters                medium/large
history/storage                  medium
UI integration                   medium
protocol/platform/observability  small/medium
```

If:

```text
apps/engine process shell
```

becomes 10,000+ lines, investigate whether market logic leaked into the process shell.

If:

```text
EngineClient
```

becomes a market-data runtime, the architecture is wrong.

If:

```text
MarketEngine
```

becomes an unstructured dumping ground, organize it internally before inventing more crates.

---

# 179. REQUIRED RESPONSE BEFORE ARCHITECTURAL CODE CHANGES

- [x] **Status: Verified complete**

Evidence (2026-08-11): the following implementation-grounded map was completed before protocol v3 was edited.

## A. Current ownership

- Desktop entry: `apps/desktop/src/main.rs::{main, configured_market_worker, TerminalApp}`.
- Engine entry and IPC server: `apps/engine/src/main.rs::run` and `apps/engine/src/lib.rs::{bind_listener, serve_client_with_state, serve_authenticated_session}`.
- IPC client support: `crates/local_engine_client/src/lib.rs::EngineClient`; desktop Coinbase orchestration remains in `apps/desktop/src/engine_market_worker.rs`, while the transitional Rithmic history consumer lives in `crates/desktop_market_runtime/src/rithmic_history.rs`.
- Coinbase session: `coinbase_market/src/desktop_driver.rs::CoinbaseProviderDriver`; desktop orchestration is `desktop_market_runtime/live_market_worker.rs::{start_with_profile, run_worker, run_session}`.
- Rithmic catalog/live session: `rithmic_protocol/src/desktop_driver.rs::RithmicProviderDriver` with `session.rs::RithmicTickerConnection`; transitional product orchestration is `desktop_market_runtime/rithmic_market_worker.rs`. Rithmic provider history is scheduled by `apps/engine/src/market_service.rs` and collected by `apps/engine/src/rithmic_history.rs`; the desktop history task is an engine IPC consumer only.
- Shared provider lifecycle: `desktop_provider_runtime::{DesktopMarketWorker, ProviderSessionDriver}`.
- Coinbase history request and handoff: `live_market_worker.rs::{maybe_start_history, history_command_loop, fetch_history_repairs}` plus `live_market_worker/history.rs::{fetch_history_range_with_adapter_cancelled, install_repaired_snapshot, install_merged_history}` and `desktop_history::HistoryWorker`.
- Persistence: `desktop_storage::HistoryStore`, synchronously called by desktop market/history workers.
- Chart publication: `live_market_worker/publication.rs::publish_update → MarketWorkerSender → TerminalApp message drain → OriginChartView`.
- Selection generation and demand: `TerminalApp::{select_interval, select_instrument} → MarketDataWorker::try_select_coinbase`; viewport demand uses `try_set_chart_viewport`.
- Loading state: `desktop_market_runtime::ChartState::Loading` plus `TerminalApp::{chart_state, coinbase_switch}`.

## B. Current runtime spine and unique ownership

- Coinbase: GPUI `TerminalApp → resident_market_worker → desktop_market_runtime → desktop_provider_runtime → CoinbaseProviderDriver → Coinbase socket/history adapter`.
- Rithmic: GPUI `TerminalApp → resident_market_worker → desktop_market_runtime::rithmic_market_worker → RithmicProviderDriver → Rithmic session connections`.
- `application::stream_runtime` uniquely validates immutable transport-neutral publications and owns no execution.
- `desktop_market_runtime` owns desktop composition, hydration, handoff, publication, and UI mailbox.
- `desktop_provider_runtime` owns shared provider session commands and generations.
- Adapter desktop drivers own provider-specific protocol execution.

## C. Target file mapping

- KEEP domain crates, provider adapters, provider-history algorithms, pure application generation/provenance, chart integration, observability, platform runtime, and transport.
- MOVE/MERGE generic execution ownership from `desktop_market_runtime` and `desktop_provider_runtime` into `market_engine`; move history/storage ownership behind the engine.
- RENAME the former `local_engine_protocol`, `desktop_history`, and `desktop_storage` identities only after their callers cut over. This cutover completed on 2026-08-18 as `engine_protocol`, `local_history`, and `local_storage`.
- DELETE the two desktop runtime crates, Rithmic desktop product-runtime duplication, and obsolete execution bridges after the new owner replaces them.
- KEEP only GPUI presentation, bounded engine client behavior, UI-side generation defense, and Origin integration in desktop.

## D. First vertical slice

Touch only the workspace manifest, engine protocol, a small `market_engine` crate, engine service, desktop engine-client bridge, existing Coinbase history adapter, existing chart bridge, and the two architecture documents. Prove BTC-USD historical bars through `Desktop → IPC → Engine → Coinbase → canonical bars → engine memory → IPC snapshot → Origin`. Exclude Rithmic, realtime, shared memory, storage migration, and warm-daemon policy.

## E. Risks

Guard against duplicate Coinbase sessions, simultaneous legacy/new UI feeds, protocol mismatch, stale generation overwrite, persistence gating, shutdown deadlock, disconnected-client demand leaks, split ownership, and tests that accidentally prove the fixture or legacy feed.

## F. Verification

Use protocol round-trip/frame tests, headless market-engine demand/snapshot/storage-failure tests, authenticated engine IPC integration, desktop bridge regression, cold BTC-USD visible-history smoke, stale-generation and disconnect cleanup scenarios, release demand/first-snapshot measurements, then the complete repository Cargo gate.

Before making the migration, the agent must first provide an implementation-grounded migration map.

Do not begin a speculative 50-file rewrite.

Report:

## A. CURRENT OWNERSHIP

Identify exact current files/functions owning:

```text
desktop entry
engine entry
Coinbase session
Rithmic session
provider lifecycle
history request lifecycle
history/live handoff
market persistence
chart publication
selection generation
chart demand
IPC server/client
current Loading state
```

## B. CURRENT RUNTIME SPINE

Show actual call/dependency path.

Especially audit:

```text
application::stream_runtime
desktop_market_runtime
desktop_provider_runtime
rithmic desktop_driver
rithmic session
```

For every layer answer:

```text
What does this layer uniquely own?
```

## C. TARGET FILE MAPPING

For each relevant current file/crate label it:

```text
KEEP
MOVE
RENAME
MERGE
DELETE
```

and state destination and reason.

## D. FIRST VERTICAL SLICE

Name the minimum files necessary to make:

```text
Desktop
→ IPC
→ Engine
→ Coinbase history
→ canonical bars
→ Engine memory
→ IPC snapshot
→ GPUI/Origin visible candles
```

work.

## E. RISKS

Explicitly identify:

```text
duplicate provider session risk
two market runtimes active simultaneously
IPC protocol mismatch
stale generation behavior
persistence gating
shutdown deadlock
client disconnect cleanup
state ownership conflicts
legacy path secretly still feeding UI
```

## F. VERIFICATION

List exact:

```text
cargo commands
focused tests
runtime scenarios
release-mode benchmarks
```

that will be used.

Only after this map is grounded in the actual source may structural migration begin.

---

# 180. FINAL ANTI-OVERENGINEERING RULE

- [ ] **Status: Not verified complete**

The fact that Axiusflow has two processes does NOT make it a distributed system.

The fact that Axiusflow handles high-frequency market data does NOT require every operation to become an actor.

The fact that Rust supports traits does NOT require every struct to sit behind one.

The fact that IPC uses serialization does NOT mean internal engine modules should serialize to each other.

The fact that multiple charts exist does NOT mean each chart needs a backend.

The fact that multiple providers exist does NOT mean generic engine logic should duplicate once per provider.

The fact that the engine can remain alive for days does NOT mean it may leak resources indefinitely.

Prefer:

```text
one owner
clear commands
canonical data
shared state
simple functions
bounded background work
explicit failures
measured performance
```

over architectural ceremony.

The final platform should contain one understandable working path.
