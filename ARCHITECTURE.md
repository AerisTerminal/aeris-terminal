# Axiusflow Architecture

## Product

Axiusflow is a local-first professional trading platform in the category of MotiveWave and ATAS. It is a native Rust desktop application for live market data, charting, market-depth workflows, local history, and eventually safe trade execution.

The product target is best-in-class interactive and streaming performance without becoming a heavyweight workstation. Architecture is a product advantage: low latency, predictable resource use, immediate local startup, strong failure isolation, and simple ownership should distinguish Axiusflow from existing platforms.

This document describes the current architectural direction. Source code, tests, and measured runtime behavior are the implementation truth.

## Principles

1. Local only. Core viewing, history, provider connectivity, and workspace behavior run on the user's machine and have no remote Axiusflow service dependency.
2. Correct before fast. Market data, sequence handling, history/live handoff, prices, quantities, and future orders must remain exact under disconnects, retries, and restarts.
3. Measure performance. Optimize observed hot paths and tail latency. Do not add speculative caches, concurrency, unsafe code, or platform-specific acceleration.
4. Keep ownership obvious. Each mutable resource has one clear owner. Communication across threads and processes uses bounded messages and explicit snapshots or deltas.
5. Stay lightweight. Prefer Rust and the standard library, operating-system facilities, and dependencies already in the workspace. New layers and dependencies require a present need.
6. Preserve the native experience. Windows, macOS, and Linux behavior should follow each operating system's conventions. Native windowing, credential storage, power, network, and display facilities belong behind narrow platform boundaries.
7. Fail safely. Stale generations, gaps, malformed provider data, unavailable credentials, and interrupted persistence must produce explicit recovery or errors, never plausible but incorrect trading state.

## Runtime topology

Axiusflow ships two local application processes:

- `axiusflow_desktop` owns the GPUI window, terminal interaction, presentation state, chart integration, and bounded background engine-client bridges. It no longer links the resident engine server application as a library and creates no Coinbase or Rithmic provider session, history connection, credential-vault access, native provider lifecycle monitor, or market-data storage path.
- `axiusflow_engine` is the per-user resident local process. It owns authenticated workspace/watchlist/viewport/hot-set persistence and the current production market slice: one bounded market coordinator, separate bounded Coinbase and Rithmic history workers, shared Coinbase and Rithmic realtime workers, one engine-owned Rithmic catalog/quote session, one encrypted provider-neutral local-history worker, a bounded provider-neutral selected-instrument catalog, canonical in-memory Coinbase and Rithmic bars, one authoritative bounded Rithmic order book, and per-consumer IPC snapshots. Coinbase and Rithmic bar history are engine-owned and survive restart. All Rithmic provider credentials, catalog discovery, selection installation, replay planning, cancellation, canonical collection for all 15 supported chart cadences, persistence, transport retry, native power/network transitions, live tick/fixed/session candle continuation, depth continuity/reconstruction, and publication are engine-owned. Complete warm lifecycle policy is still unfinished.

The default shipping desktop starts background `EngineClient` support, attaches random process-lifetime client identities and independent consumer identities, and sends generation-fenced demand over protocol v10. Its canonical series identity carries provider, instrument, entitlement revision, definition revision, and one exact cadence: fixed seconds, trades, session days, calendar weeks, or calendar months. Canonical bars carry both their whole exchange second for bucket arithmetic and a validated exact nanosecond ordering timestamp, so multiple Rithmic trade-count bars within one second remain distinct through engine memory and IPC. One coordinator thread multiplexes ordinary chart endpoints through a shared authenticated engine connection; the Rithmic presentation bridge uses bounded engine clients for provider-neutral catalog commands and continuous chart/DOM polling, never a provider socket. Demand acceptance is asynchronous: the engine resolves a memory hit or schedules provider history away from its coordinator, while the desktop receives only bounded polled state, error, catalog, covering-series, and covering-order-book events. Separate bounded provider-history workers prevent a slow authenticated Rithmic replay from delaying Coinbase. One process-owned Coinbase realtime worker subscribes to BTC-USD and ETH-USD and routes trades into the demanded 1m, 5m, 15m, or 1h handoffs without opening a provider session per switch. The coordinator buffers bounded live trades during history repair, seeds each canonical fixed-interval aggregator from completed history or a cached forming tail, installs only a forming-tail revision, and fences replaced selection generations. In Rithmic Test mode, exact symbol search and selection cross authenticated IPC; the engine-owned catalog session resolves provider metadata, atomically installs the canonical identity, and starts the separate engine history and realtime paths. The engine rejects stale, conflicting, invalid, or over-capacity selections, reconstructs the authoritative bounded order book, and cancels unobserved replay work. The desktop validates engine publications into independent application models, publishes through bounded per-chart UI mailboxes, and hands each covering snapshot to its Origin chart or DOM presentation on GPUI. The normal launch currently opens one chart; `--multi-chart` is the Phase 5 native proof surface and opens BTC and ETH charts in separate GPUI windows without another provider session or backend runtime. Persistent multi-tab/pane workspace composition remains a later UI phase.

```text
GPUI demand
    -> bounded desktop EngineClient worker
    -> authenticated protocol-v10 local IPC
    -> resident MarketEngine coordinator
    -> Coinbase public-history worker + Coinbase realtime worker
    -> canonical fixed-point bars and bounded SeriesStore
    -> bounded per-consumer IPC state/snapshot publication
    -> desktop application model and bounded UI mailbox
    -> terminal UI and Origin chart renderer
```

## Workspace boundaries

### Applications

- `apps/desktop`: native GPUI application, window chrome, terminal composition, UI event routing, desktop diagnostics, and its app-local bounded market-presentation mailbox plus deterministic disconnected fixture.
- `apps/engine`: resident engine executable plus authenticated local IPC, activation, workspace persistence, recovery, migration, and corrupt-file quarantine.

### Domain and application

- `crates/domain/instruments`: provider-neutral instrument identity.
- `crates/domain/market_data`: canonical bars, intervals, order-book state, and related market semantics.
- `crates/application`: generation-aware client models, provenance validation, replay snapshots, and stream publication behavior.
- `crates/market_engine`: headless engine core with one explicitly owned demand registry, provider-session registry, bounded canonical series store, and immutable per-consumer publications. `apps/engine` owns and drives it on one coordinator thread; the core itself remains free of provider adapters, storage, IPC, GPUI, threads, and globals.

These crates must not depend on UI or a particular provider.

### Providers and coordination

- `crates/adapters/coinbase_market`: Coinbase catalog, history, streaming, and wire behavior.
- `crates/adapters/rithmic_protocol`: Rithmic protocol, network sessions, catalog, history, and market-data behavior. It now owns canonical collection and exact timestamp conversion for all 15 supported chart cadences, including daily-session aggregation into calendar weeks and months.
- `crates/adapters/market_protocol`: conversion between canonical market models and protobuf/wire representations.
- `crates/desktop_provider_runtime`: transitional Rithmic-only provider-session lifecycle, generation fencing, and feed diagnostics. Its unused provider/history composition has been deleted; the remaining lifecycle still runs only inside the resident engine and is scheduled for migration into the Rithmic adapter or engine provider manager.
- `crates/provider_history`: provider-neutral pagination, rate limiting, coverage, scheduling, and history/live handoff.

Provider-specific types stop at adapter boundaries. Downstream code consumes canonical identities and market models.

### Local data and protocols

- `crates/desktop_storage`: SQLite metadata, encrypted local segments, and storage lifecycle.
- `crates/desktop_history`: local history cache behavior built on storage and provider-history contracts.
- `crates/local_engine_client`: blocking authenticated local-IPC client, native installation-token access, sibling-engine discovery/startup, framing, and typed engine commands. It contains no provider, market-state, storage, or GPUI behavior. Desktop calls its blocking connection/start APIs only from background workers; the resident engine reuses its installation-token access during process startup but not its client connection or process-start behavior.
- `crates/local_engine_protocol`: versioned authentication, workspace, lifecycle, engine market-demand, readiness, provider-state, provider-neutral catalog search/selection, fixed-point series publication, and conflated order-book framing. Protocol version 10 is active; it makes series-demand and catalog success asynchronous and carries consumer generation, provider session generation, publication generation, decimal precision, canonical bars with exact nanosecond exchange ordering, forming-tail state, bounded market-event polling, entitlement revision, definition revision, exact fixed-time, trade-count, session-day, calendar-week, or calendar-month cadence identity, generation-fenced canonical catalog results, and selected-instrument metadata. The stable `v8` endpoint generation plus strict protocol negotiation prevents an incompatible older resident engine from being mistaken for a compatible endpoint or a second engine from starting against the same local state.
- `crates/protocols`: shared protobuf-backed stream contracts and sequence semantics.
- `crates/transport`: small transport framing primitives.

Persistent writes use revisioned or transactional publication so a crash cannot turn a partial write into current state. Credentials and sensitive provider material must use the native credential vault or zeroizing memory, not source files, logs, or plain-text configuration.

## Local market-data execution

The default implemented Coinbase path is:

```text
GPUI demand
    -> desktop EngineClient worker
    -> authenticated local IPC
    -> engine MarketEngine coordinator
    -> Coinbase history worker + realtime WebSocket worker
    -> canonical completed bars + active-candle aggregation
    -> bounded shared SeriesStore
    -> bounded fixed-point IPC snapshots and explicit provider state
    -> desktop validation and bounded mailbox
    -> chart bridge and Origin Charts
```

This production slice supports BTC-USD and ETH-USD history plus realtime at 1m, 5m, 15m, and 1h. Coinbase history is requested at the demanded native granularity, then locally resequenced into one contiguous canonical series; realtime trades update the matching UTC-aligned fixed-interval handoff. A bounded engine storage worker owns the existing authenticated SQLite catalog and encrypted immutable segment store under the engine data root, with keys held in the native credential vault. Demand checks the exact hot series, compatible hot one-minute history, retained derived history, retained native history, compatible retained native one-minute history, and finally the provider. Compatible one-minute history immediately derives 5m, 15m, or 1h bars in the valid finer-to-coarser direction whether its source is hot or retained after restart; the bounded engine store and encrypted derived segment cache make repeated switches immediate while native provider coverage refreshes asynchronously. Rithmic uses the same provider/account/entitlement-scoped store for all 15 native chart cadences. Its provider-neutral segment version preserves exact nanosecond bar time, while the reader remains compatible with existing whole-second Coinbase segments. An engine-owned catalog/quote session performs bounded exact symbol search and selection with native-vault credentials and native lifecycle fencing. Successful selection installs provider-neutral descriptor, precision, entitlement, session, and selection metadata in the coordinator before publication over protocol v10. A separate engine realtime worker begins from that generation-fenced installed instrument and is the sole trade and depth subscriber. Tick, fixed-time, and one-/three-session-day handoffs buffer trades until completed history is installed, revise only a forming tail, advance the engine provider generation on transport retry, and force covering history before resuming. The coordinator applies canonical depth to one bounded top-20 order book, fails closed on invalid continuity, and publishes only the latest conflated fixed-point image over protocol v10. Desktop validates that image and performs display formatting without reconstructing another candidate book. Week/month charts remain history-driven rather than inventing live calendar bars without an exchange calendar. The desktop IPC consumer remains attached after initial history and conflates subsequent engine snapshots. Retained bars publish immediately as usable `Partial/Durable` state while provider repair continues. Refreshed provider history installs and publishes in memory as `Ready/Pending` before an asynchronous persistence request is queued; a disk or persistence failure reports `Degraded` independently and cannot erase valid chart data. On first demand, completed history is installed before the buffered live suffix; only the current forming candle may revise an installed series tail. Returning to a cached forming snapshot seeds that same canonical tail before live updates resume. A provider generation change exposes Recovering state, retains the prior covering chart, marks the prior book stale, refetches completed history for the new generation, replays the bounded live buffer, and resumes active-candle and covering-book publication without rebuilding desktop consumers. The shared Coinbase realtime WebSocket is released when the last consumer detaches. A completed warm-engine product remains a later slice; no shipping desktop market path owns a provider connection.

`ProviderSessionDriver` is currently a transitional Rithmic lifecycle boundary used only by the resident engine. Coinbase no longer implements that desktop-era contract: its production history and realtime sessions are owned directly by the engine workers through the Coinbase adapter's venue-specific transports. `ProviderHistoryAdapter` remains the shared paginated-history boundary. Authentication, transport framing, provider limits, product/catalog translation, and provider-specific recovery remain inside the adapters; downstream history, storage, engine, and UI code consumes canonical identities and values.

The obsolete `desktop_market_runtime` workspace crate is deleted. Its only remaining behavior—the bounded presentation mailbox, application-model handoff, and deterministic disconnected fixture—now lives in the `axiusflow_desktop` package alongside its sole consumer and retains the same bounded conflation and recovery tests. The app-local `rithmic_engine_client` and `rithmic_engine_history` modules are engine-protocol presentation clients only: they forward bounded provider-neutral catalog and series demand, continuously poll canonical chart and order-book snapshots, convert them into application/presentation models, and cancel obsolete demand by removing their engine consumer. `apps/desktop` depends on neither `rithmic_protocol` nor `desktop_provider_runtime`. The app-local Rithmic shell and series browser contain presentation state only. No desktop layer loads credentials or creates provider sessions. `apps/engine/src/rithmic_history.rs` owns native-vault history credential loading, bounded replay planning, the authenticated history connection, all 15 cadence collection, and exact-time publication; `apps/engine/src/rithmic_realtime.rs` owns both the authenticated catalog/quote lifecycle and the separate trade/depth lifecycle, including bounded transport retry and native power/network transitions. Provider adapters stop at venue authentication, sockets, wire parsing, catalog translation, venue continuity, rate limits, paging, and provider-specific canonical conversion. The chart and DOM bridges retain consumer-side stale and identity rejection as defense in depth, not as second market-state owners.

The obsolete Coinbase desktop provider driver, event bridge, deterministic session fixture, and feature flag are deleted. The unused `desktop_provider_runtime::DesktopMarketWorker` provider/history composition is also deleted with its history and storage dependencies; Rithmic now owns the callback-state errors it formerly borrowed from that dead layer.

The path is local by construction. Provider credentials stay on the user's machine, provider traffic terminates in a local worker, and durable history is stored under the user's local data root. No remote Axiusflow service, licensing gateway, or network chart service exists in the product architecture.

## Historical data and restart behavior

`desktop_storage` stores authenticated immutable segments and a keyed SQLite catalog. Segment publication is stage, sync, link, catalog commit; interrupted or corrupt files are recovered or quarantined rather than admitted as valid history. Catalog rows record exact provider/account/entitlement, instrument, resolution, time range, and source/schema/calendar/adjustment/correction revisions.

`provider_history::CoverageSnapshot` merges complete, confirmed-empty, invalidated, and quarantined ranges, then returns only the missing or damaged repair ranges. Visible repairs outrank adjacent prefetch. Provider paging, rate limits, continuation bounds, retry attempts, cancellation interests, and total in-flight work are bounded by `HistoryScheduler`.

`desktop_history::HistoryWorker` is owned by a market-data worker thread, never GPUI. It reads and decrypts bounded segments, validates contiguous sequence, maintains a byte- and entry-bounded decoded cache, shares immutable publications across charts, and coordinates the history/live cutover. Live items are buffered during hydration; a verified covering snapshot admits only the contiguous suffix newer than its watermark.

The current storage format supports progressive recent-first reads because retained history is segmented and range-indexed. Coinbase chart viewport changes are first-class desktop-worker demand: the active selection generation is checked, the visible time range is aligned to the source interval, one visible window is prefetched behind the viewport, duplicate ranges are deduplicated, local coverage is installed directly, and only missing ranges are sent to the provider. The retained working set is bounded to the adapter-supported chart window rather than the old 300-bar slice.

## Timeframes

Provider-native history is requested at the closest supported source resolution. The current engine Coinbase slice requests its exact supported native 1m, 5m, 15m, or 1h granularity and uses the same fixed interval for incremental live-trade aggregation. Rithmic maps canonical chart intervals to its provider bar specifications behind its adapter.

The durable identity includes resolution and source revision, so native and derived data cannot be confused. Coinbase warm switches first look for immutable derived chunks keyed by source revision and interval; when absent, retained canonical one-minute chunks are aggregated once, published immediately, and stored under the bounded derived-data quota before provider reconciliation. Rithmic retains its provider-native series mapping. Calendar intervals keep explicit UTC/exchange-calendar bucket rules instead of being approximated as fixed seconds.

## Concurrency model

- The resident market coordinator is the single mutable owner of default Coinbase demand, provider generation, cached series, and consumer publications.
- Each market command is authorized against the authenticated session's attached client identity; one desktop client cannot mutate another client's consumers.
- One bounded engine history worker owns the Coinbase history adapter; provider I/O never blocks GPUI or the market coordinator.
- One bounded engine realtime worker owns the Coinbase WebSocket. Its generation-fenced events enter a bounded queue; queue overflow, sequence invalidation, or disconnect forces explicit recovery and a covering history repair.
- The coordinator stores at most the latest provider state, snapshot, and series state per consumer. Covering snapshots may conflate; provider deltas are not silently discarded.
- The bounded desktop engine-client worker interleaves commands with 16 ms market-event polling and application-model conversion, never provider execution or canonical market state.
- The engine owns separate bounded Rithmic catalog/quote, history, and realtime workers plus the authoritative bounded order book. Provider workers apply native power/network transitions before provider retry; the desktop Rithmic bridge owns only IPC polling and chart/DOM presentation projection.
- Blocking history and storage work stays off GPUI and communicates through bounded channels.
- Selection generations make obsolete symbol and timeframe results stale; stale work cannot overwrite the new selection.
- The desktop command boundary coalesces selection changes to the newest state until the bounded provider-worker mailbox accepts it; queue pressure must never silently discard the active selection.
- Chart viewport changes are also retained as demand on the active series; the Coinbase worker may not drop them silently or apply them to an older symbol/timeframe generation.
- Covering snapshots may replace older covering snapshots. Non-conflatable deltas, sequence gaps, and queue overflow require recovery rather than silent loss.
- Work is bounded by queue item/byte capacities, scheduler in-flight limits, cache bytes, segment bytes, chart bindings, handoffs, retries, and deadlines. No request creates an unbounded thread pool or unbounded queue.

Visible history work rotates across per-instrument lanes under the existing global in-flight bound. Selection changes cancel obsolete work before aggregation, Coinbase requests have a 30-second task deadline and five-second network-operation bound, and shared scheduler interests remain alive until their final consumer cancels.

## Performance evidence and instrumentation

Always-on feed diagnostics count trades, quotes, depth updates, publications, gaps, duplicates, malformed messages, stale callbacks, overflows, UI conflation, queue occupancy, memory, and lifecycle state. Opt-in fixed histograms cover socket-to-decode, decode-to-canonical, canonical-to-model, model-to-UI, UI-to-frame, and frame-to-present boundaries without unbounded label cardinality.

`HistoryWorker::metrics` additionally records local storage operations and bytes, memory-cache hits and misses, decode work, provider snapshots, live items, duplicates, and cumulative storage-write, storage-read, decode, snapshot-install, and live-publication nanoseconds. These metrics contain no provider payloads, credentials, account text, or instrument text.

`axiusflow_market_data_performance` is the deterministic storage regression runner. It uses the production Coinbase segment codec, encrypted `HistoryStore`, catalog coverage planner, interval aggregator, and a repeated immutable derived-timeframe lookup. The desktop's `--windowed-benchmark` path complements it by opening a real GPUI/Origin window and measuring startup-to-first-frame, steady replay-to-frame latency, a five-minute covering timeframe replacement, callback cadence, and native compositor timing. Provider-network benchmarks remain separate because provider latency, entitlements, and credentials are not deterministic CI inputs.

The first recorded local release run on 250,000 one-minute bars produced 715 encrypted segments (15.5 MB payload): cold publication 1.37 seconds, warm catalog open 22 ms, coverage discovery 1 ms, full warm read 78 ms, decode 8 ms, and first recent segment in under 1 ms. Removing unconditional sort and duplicate-buffer copies reduced sorted-source timeframe aggregation from 7.2-8.4 ms to 1.3-3.1 ms across 5-minute through daily intervals on the same run. Machine-specific JSON is transient evidence under `.cache`, not a portable product guarantee.

The first direct-path Windows release run after removing the engine subscription from first pixels measured 22.9 ms from window setup to the first GPUI frame and 5.9 ms from a five-minute covering snapshot submission to the following frame callback on a 165 Hz display. The same run measured 6.0 ms p50 update-to-frame latency with advancing DWM refresh evidence. A separate 100,000-bar storage run measured the first recent segment in 155 microseconds, a complete warm read in 30 ms, a cold five-minute derivation in 966 microseconds, and its repeated immutable-cache lookup below the microsecond timer resolution. These numbers are machine-specific regression evidence, not physical panel scanout guarantees.

## Ranked local-data roadmap

1. Extend the visible-range runtime from Coinbase bars to provider trade and depth history so footprint/order-flow views receive the same demand-driven backfill behavior.
2. Extend immutable derived chunks and incremental active-tail updates from Coinbase bars to Rithmic and future provider capabilities without weakening calendar rules.
3. Extend deterministic performance scenarios to symbol churn, interrupted publication recovery, concurrent live plus history, viewport panning, and multi-million tick streams. Use synthetic transports in CI and credentialed provider runs only as local evidence.
4. Evaluate memory mapping only after paged reads and derived chunks are measured. Encryption and authenticated recovery currently require bounded read/decrypt buffers, so mapping ciphertext alone is not automatically a win.

### UI

- `crates/ui/design_system`: Axiusflow theme tokens.
- `crates/ui/terminal_ui`: reusable terminal and DOM presentation components.
- `crates/ui/chart_integration`: the boundary between Axiusflow models/GPUI and Origin Charts.

`Origin_charts/` is a separate repository and is not governed by this document. The main workspace consumes pinned Origin crates from its Git repository. Changes to Origin documentation or implementation must be made in that repository deliberately, never as collateral work in Axiusflow.

### Platform and observability

- `crates/platform_runtime`: native credential, display timing, network, power, and I/O cancellation facilities.
- `crates/observability`: bounded feed diagnostics and latency evidence.
- `tools`: conformance, naming, and overhead measurement utilities; tools are not runtime architecture.
- `schemas/protobuf`: versioned shared wire schemas.
- `provider_kit`: vendor reference material, not application source or a design template.

## Data correctness

Every streamed view has a local series identity, session generation, publication generation, and source sequence. A selection change invalidates older work. Covering snapshots establish a known state; deltas are accepted only when their sequence and session continue that state. A new session requires a covering snapshot, and older sessions or publication generations are rejected. A detected gap, stale response, or provider reconnect requires recovery from a covering snapshot.

The transient market-stream schema is version 2. Removed distributed partition fields are reserved in Protobuf and cannot be reused; session generation lives once on the market-event header, while publication generation identifies covering snapshot revisions. Persisted workspace and encrypted history use separate versioned schemas and are unaffected by this transient contract.

History and live data meet at one explicit handoff boundary:

1. Determine local coverage.
2. Fetch only missing provider history within provider limits.
3. Validate and publish a covering ordered snapshot.
4. Buffer or sequence live events during hydration.
5. Admit only live events newer than the accepted history boundary.

Prices and quantities use fixed-point or provider-exact representations. Floating-point conversion is a presentation concern and must not become the source of stored or transmitted truth.

## Concurrency and backpressure

Provider sessions, the desktop market worker, the resident engine, and GPUI have distinct owners. Do not let the UI thread perform blocking network, disk, process, or shutdown work. Do not let background workers mutate GPUI state directly.

Queues must be bounded and have semantics appropriate to their payload:

- covering state may replace older covering state;
- transient deltas may be conflated only when the resulting state is provably equivalent;
- gaps and dropped non-conflatable events trigger recovery;
- control commands and future order actions are never silently dropped.

The render loop is demand-driven. Streaming data may request presentation, but it must not keep the UI permanently busy, block input dispatch, or recreate expensive workers for ordinary selection changes.

## Performance contract

Performance work is accepted with evidence from release builds on representative hardware. Track at least startup time, time to first usable local state, history-to-live readiness, UI input latency while streaming, frame time, memory growth, reconnect time, queue depth, and p50/p95/p99 provider-to-presentation latency.

Prefer improvements in this order:

1. Remove unnecessary work and copies.
2. Fix ownership, batching, wakeups, and algorithms.
3. Reuse allocations and compact hot data.
4. Use an existing platform or dependency primitive.
5. Add platform-specific optimization only after portable code misses a measured target.

Never trade determinism, recoverability, security, or cross-platform correctness for a benchmark headline.

## Change rules

- Add a crate only for a durable boundary with multiple real consumers or a necessary dependency direction.
- Add a trait only when there are multiple current implementations, a genuine test seam, or a platform/provider boundary that requires substitution.
- Add configuration only when users or deployments need to choose the value now.
- Keep provider, domain, application, IPC, and UI conversions at their boundaries; do not leak wire types across layers.
- Keep architectural claims in this file concise and current. Historical plans and decision-log sprawl are not architecture.
- Update this document in the same change when runtime topology, ownership, persistence, security boundaries, or supported platforms change.

## Repository documentation rule

The only Markdown files allowed in the Axiusflow repository are these three root-level files:

- `ARCHITECTURE.md`: authoritative current implemented architecture.
- `AGENTS.md`: repository engineering and delivery instructions.
- `AXIUSFLOW LOCAL ENGINE ARCHITECTURE MIGRATION SPECIFICATION.md`: approved target-state contract for migrating market-data ownership into the resident engine.

The migration specification may intentionally differ from this document while work is incomplete. As each migration slice ships, update this document to describe the new current behavior. A change to the approved target must update both architecture documents in the same commit and clearly preserve the distinction between current and target state.

Do not create any other plans, reports, reviews, roadmaps, temporary Markdown, package READMEs, or duplicate architecture documents. Put current durable architecture here, executable behavior in code and tests, approved migration requirements in the migration specification, and transient work notes outside the repository.

`Origin_charts/` is a separate repository with its own root-level `ARCHITECTURE.md` and `AGENTS.md`. Its Markdown inventory is not part of Axiusflow, and its files must not be changed from this repository unless coordinated Origin work is explicitly requested.

Architecture is never a historical snapshot. Any code change that alters process topology, crate ownership, dependencies, data flow, persistence, security boundaries, provider/UI boundaries, supported platforms, or verification gates must update this file in the same commit. Validate it against manifests, public exports, entry points, and real call paths before delivery.

## Verification

The workspace gate is:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets --all-features
cargo test --workspace --all-features
```

The first migration preparation slice was protocol-only and created no second provider runtime.

The second preparation slice added the headless `market_engine` state owner. Its bounded store shares one immutable bar snapshot across matching consumers, consumer generations fence stale presentation, provider generations fence stale sessions, and client detach removes only that client's demand.

The Phase 5 proof connected BTC-USD and ETH-USD historical and realtime demand at 1m, 5m, 15m, and 1h through authenticated protocol-v5 IPC; protocol v10 preserves that behavior while adding exact entitlement, cadence, nanosecond bar identity, engine-owned catalog search/selection, canonical instrument installation, and conflated engine-owned order-book publication required for Rithmic migration. An engine-owned coordinator plus bounded catalog, history, and realtime workers populate `MarketEngine`; the desktop bridge validates precision and provenance and reuses the existing bounded application/UI publication boundary. Deterministic tests cover protocol catalog round trips, selection installation before publication, service cache sharing, authenticated IPC delivery, fixed-point conversion, cached forming-tail continuation, stale generations, exact rapid switch churn with delayed history, one realtime session across symbol/interval switches, deliberate disconnect/reconnect, two-consumer isolation, retained history, resumed active candles, queue-overflow recovery, provider-instrument generation fencing, engine-owned Rithmic depth/DOM projection, last-consumer teardown, and the app-local bounded presentation mailbox. Clean Windows release runs rendered visible Origin candles with a healthy connection state, remained responsive, showed 12,850 sampled chart-region pixels change over twenty seconds, and then completed the visible BTC 1m to 5m to 15m to 1h to 1m and BTC to ETH to BTC switch sequences without stale overwrite or loading hangs. The live WebSocket closed within three seconds of desktop exit. Credentialed engine-owned Rithmic catalog smoke testing, exchange-calendar-owned live week/month bars, and complete warm-mode policy remain later gates.

Focused conformance and release-mode performance checks supplement this gate for provider, persistence, IPC, UI, and latency changes. A passing compile is not proof of runtime correctness.
