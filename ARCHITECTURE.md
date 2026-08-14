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

- `axiusflow_desktop` owns the GPUI window, terminal interaction, presentation state, chart integration, lifecycle coordination, and bounded background engine-client bridges. It no longer links the resident engine server application as a library and creates no Coinbase or Rithmic provider session, history connection, credential-vault access, native provider lifecycle monitor, or market-data storage path.
- `axiusflow_engine` is the per-user resident local process. It owns authenticated workspace/watchlist/viewport/hot-set persistence and the current production market slice: one bounded market coordinator, separate bounded Coinbase and Rithmic history workers, shared Coinbase and Rithmic realtime workers, one engine-owned Rithmic catalog/quote session, one encrypted provider-neutral local-history worker, a bounded provider-neutral selected-instrument catalog, canonical in-memory Coinbase and Rithmic bars, one authoritative bounded Rithmic order book, and per-consumer IPC snapshots. Coinbase and Rithmic bar history are engine-owned and survive restart. All Rithmic provider credentials, catalog discovery, selection installation, replay planning, cancellation, canonical collection for all 15 supported chart cadences, persistence, transport retry, native power/network transitions, live tick/fixed/session candle continuation, depth continuity/reconstruction, and publication are engine-owned. The engine accepts authenticated operational resource-mode changes and complete shutdown. `axiusflow_engine --shutdown` and the shared desktop lifecycle helper connect to an existing engine without starting another. After authentication the server freezes persistent workspace mutation, closes its listener, concurrently writes a newer revisioned hot-set manifest, cancels in-flight history and Coinbase realtime work, disconnects Rithmic control channels so active provider runtimes stop, drains already-accepted local-history requests, and joins the coordinator plus its top-level provider/history/storage workers within one shared two-second process deadline. Each Rithmic catalog/realtime worker retains cancellable native network/power wait handles and joins both named monitor helpers before it can finish. A flush, worker, or client-session failure produces a diagnostic and nonzero process exit. Hot-set publication retains only the latest two manifests, so a prior equal-revision viewport manifest cannot overwrite a newer selection on restart. Engine launch no longer registers unconditional Windows login startup. Default desktop close keeps the established engine warm and releases unused Coinbase realtime work; per-launch `--keep-markets-live` retains the bounded selected live handoffs and provider sessions after the last UI client detaches, while `--exit-with-desktop` performs complete shutdown. Durable in-product lifetime selection and optional user-controlled autostart remain unfinished.

The default shipping desktop starts background `EngineClient` support, attaches random process-lifetime client identities and independent consumer identities, and sends generation-fenced demand over protocol v10. Before those workers start, it authenticates once and applies the per-launch background resource policy to the engine coordinator. Its canonical series identity carries provider, instrument, entitlement revision, definition revision, and one exact cadence: fixed seconds, trades, session days, calendar weeks, or calendar months. Canonical bars carry both their whole exchange second for bucket arithmetic and a validated exact nanosecond ordering timestamp, so multiple Rithmic trade-count bars within one second remain distinct through engine memory and IPC. One coordinator thread multiplexes ordinary chart endpoints through a shared authenticated engine connection; the Rithmic presentation bridge uses bounded engine clients for provider-neutral catalog commands and continuous chart/DOM polling, never a provider socket. Demand acceptance is asynchronous: the engine resolves a memory hit or schedules provider history away from its coordinator, while the desktop receives only bounded polled state, error, catalog, covering-series, and covering-order-book events. Separate bounded provider-history workers prevent a slow authenticated Rithmic replay from delaying Coinbase. One process-owned Coinbase realtime worker subscribes to BTC-USD and ETH-USD and routes trades into the demanded 1m, 5m, 15m, or 1h handoffs without opening a provider session per switch. The coordinator buffers bounded live trades during history repair, seeds each canonical fixed-interval aggregator from completed history or a cached forming tail, installs only a forming-tail revision, and fences replaced selection generations. In Rithmic Test mode, exact symbol search and selection cross authenticated IPC; the engine-owned catalog session resolves provider metadata, atomically installs the canonical identity, and starts the separate engine history and realtime paths. The engine rejects stale, conflicting, invalid, or over-capacity selections, reconstructs the authoritative bounded order book, and cancels unobserved replay work except while the explicitly selected markets-live policy owns the retained repair. An authenticated `RemoveConsumer` retires only that chart's pending history interest, publication, demand, and now-unused live subscription; other consumers and their shared canonical state remain active. EOF or any framing failure on an authenticated local connection synchronously detaches that client before its server session returns, removing every owned waiter, publication, and demand so an abruptly terminated desktop cannot leak resident-engine consumers. The desktop validates engine publications into independent application models, publishes through bounded per-chart UI mailboxes, and hands each covering snapshot to its Origin chart or DOM presentation on GPUI. Native close, custom caption close, keyboard close, and direct application quit all begin market-client retirement without waiting on GPUI. The application quit future awaits those already-running bounded detach acknowledgements on the background executor, then either exits under the already-installed warm policy or authenticates and requests complete engine shutdown under `--exit-with-desktop`. In `--keep-markets-live`, live trades and depth continue updating the retained canonical handoffs with no UI consumer; reattachment can receive the advanced in-memory snapshot without another provider-history fetch. Closing the final window explicitly quits the GPUI process. The normal launch currently opens one chart; `--multi-chart` is the Phase 5 native proof surface and opens BTC and ETH charts in separate GPUI windows without another provider session or backend runtime. Persistent multi-tab/pane workspace composition remains a later UI phase.

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
- `crates/application`: generation-aware client models, provenance validation, replay snapshots, embedded deterministic replay input, transport-neutral replay checksums and sequence mechanics, and other provider-neutral pure state. It owns no worker, queue, socket, storage, or runtime lifecycle.
- `crates/market_engine`: headless engine core with one explicitly owned demand registry, provider-session registry, bounded canonical series store, and immutable per-consumer publications. `apps/engine` owns and drives it on one coordinator thread; the core itself remains free of provider adapters, storage, IPC, GPUI, threads, and globals.

These crates must not depend on UI or a particular provider.

### Providers and coordination

- `crates/adapters/coinbase_market`: Coinbase catalog, history, streaming, and wire behavior.
- `crates/adapters/rithmic_protocol`: Rithmic protocol, network sessions, provider lifecycle and generation fencing, session contracts, catalog, history, and market-data behavior. It owns canonical collection and exact timestamp conversion for all 15 supported chart cadences, including daily-session aggregation into calendar weeks and months. Its lifecycle writes directly into the bounded feed accumulator owned by `observability`; there is no intermediate diagnostics wrapper.
- `crates/provider_history`: provider-neutral pagination, rate limiting, coverage, scheduling, and history/live handoff.

Provider-specific types stop at adapter boundaries. Downstream code consumes canonical identities and market models.

### Local data and protocols

- `crates/desktop_storage`: SQLite metadata, encrypted local segments, and storage lifecycle.
- `crates/desktop_history`: local history cache behavior built on storage and provider-history contracts.
- `crates/local_engine_client`: blocking authenticated local-IPC client, native installation-token access, sibling-engine discovery/startup, framing, typed workspace/market commands, operational resource-mode control, and complete engine shutdown. It contains no provider, market-state, storage, or GPUI behavior. Desktop calls its blocking connection/start APIs only from background workers; the resident engine reuses its installation-token access for process startup and its authenticated client for the explicit `--shutdown` lifecycle action.
- `crates/local_engine_protocol`: versioned authentication, workspace, lifecycle, engine market-demand, readiness, provider-state, provider-neutral catalog search/selection, fixed-point series publication, and conflated order-book framing. Protocol version 10 is active; it makes series-demand and catalog success asynchronous and carries consumer generation, provider session generation, publication generation, decimal precision, canonical bars with exact nanosecond exchange ordering, forming-tail state, bounded market-event polling, entitlement revision, definition revision, exact fixed-time, trade-count, session-day, calendar-week, or calendar-month cadence identity, generation-fenced canonical catalog results, and selected-instrument metadata. The stable `v8` endpoint generation plus strict protocol negotiation prevents an incompatible older resident engine from being mistaken for a compatible endpoint or a second engine from starting against the same local state.
- `crates/transport`: shared bounded length-prefixed framing used by the local engine protocol.

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

`ProviderSessionDriver` is an adapter-local Rithmic lifecycle and deterministic-test boundary. Coinbase does not implement it: its production history and realtime sessions are owned directly by engine workers through the Coinbase adapter's venue-specific transports. `ProviderHistoryAdapter` remains the shared paginated-history boundary. Authentication, transport framing, provider limits, product/catalog translation, and provider-specific recovery remain inside the adapters; downstream history, storage, engine, and UI code consumes canonical identities and values. Provider adapter production manifests depend only on canonical market data, provider-history contracts, platform/observability primitives, and venue wire/network libraries; storage/history fixtures remain dev-only and neither adapter links UI.

The obsolete `desktop_market_runtime` workspace crate is deleted. Its only remaining behavior—the bounded presentation mailbox, application-model handoff, and deterministic disconnected fixture—now lives in the `axiusflow_desktop` package alongside its sole consumer and retains the same bounded conflation and recovery tests. The app-local `rithmic_engine_client` and `rithmic_engine_history` modules are engine-protocol presentation clients only: they forward bounded provider-neutral catalog and series demand, continuously poll canonical chart and order-book snapshots, convert them into application/presentation models, and cancel obsolete demand by removing their engine consumer. `apps/desktop` depends on no provider adapter, provider-history implementation, storage implementation, or `market_engine`; its Coinbase presentation catalog uses the same provider-neutral installed-instrument protocol descriptor as other engine publications. The app-local Rithmic shell and series browser contain presentation state only. No desktop layer loads credentials or creates provider sessions. `apps/engine/src/rithmic_history.rs` owns native-vault history credential loading, bounded replay planning, the authenticated history connection, all 15 cadence collection, and exact-time publication; `apps/engine/src/rithmic_realtime.rs` owns both the authenticated catalog/quote lifecycle and the separate trade/depth lifecycle, including bounded transport retry and native power/network transitions. Provider adapters stop at venue authentication, sockets, wire parsing, catalog translation, venue continuity, rate limits, paging, and provider-specific canonical conversion. The chart and DOM bridges retain consumer-side stale and identity rejection as defense in depth, not as second market-state owners.

The obsolete Coinbase desktop provider driver, event bridge, deterministic session fixture, feature flag, and the entire `desktop_provider_runtime` crate are deleted. The surviving Rithmic lifecycle, generation fence, vault boundary, and session contract live with the Rithmic adapter and report state directly to the engine-owned caller. The adapter retains only bounded provider callback queues that are actually consumed, while `observability::FeedDiagnostics` owns the metrics accumulator directly. The old `application::stream_runtime` immutable-publication wrapper and unconsumed desktop event/publication queue are also deleted. The unused protobuf market-stream schema, generated-protocol crate, and conversion adapter are deleted; production desktop-to-engine publication has exactly one wire contract in `local_engine_protocol`. The deterministic disconnected benchmark fixture now submits replay snapshots and deltas directly to the same application model used by engine publications and owns no thread or wire protocol.

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

`axiusflow_market_data_performance` is the deterministic storage regression runner. It uses the production Coinbase segment codec, encrypted `HistoryStore`, catalog coverage planner, interval aggregator, and a repeated immutable derived-timeframe lookup. The desktop's `--windowed-benchmark` path complements it by opening a real GPUI/Origin window and measuring startup-to-first-frame, steady replay-to-frame latency, one direct same-series covering-snapshot replacement, snapshot-install and GPUI frame-registration foreground durations, callback cadence, native compositor timing, process working set from before GPUI setup through completion, chart replay-queue occupancy at the before/after delta-submission boundaries, and the production symbol-input, instrument-selection, and interval-selection callbacks. Schema 8 emits real GPUI change and Enter events through the shipping symbol-input subscription, alternates the existing instrument and timeframe handlers, and drains their nonblocking commands through a capacity-four disconnected sink; it does not start an engine or provider. It fails without writing evidence if one submitted delta remains queued at the next frame callback, any queue overflow occurs, the covering replacement is rejected, an interaction batch does not emit exactly its symbol and timeframe commands, or any foreground timing stream is absent or incomplete; callback/report failures propagate to the process result. All handler timing and chart timestamp state exists only under the diagnostics feature, so ordinary builds retain neither sample buffers nor timing calls. Memory remains measured evidence rather than inheriting the unrelated headless-endurance target. Provider-network benchmarks remain separate because provider latency, entitlements, and credentials are not deterministic CI inputs.

`axiusflow_diagnostics_overhead` measures opt-in histogram cost against a deterministic provider workload rather than against diagnostics calls in isolation. Each release sample decodes and canonicalizes one bounded Coinbase trade batch, applies the trades to the production fixed-point bar aggregator, and records the same feed counters and seven local timestamp-chain intervals with detailed histograms disabled and enabled. The paired arms alternate execution order, retain 256 warm-up and 2,048 measured samples, use optimizer barriers around external input and accumulated state, and reject semantic divergence, zero baselines, or p99 regressions above 5% and p99.9 regressions above 10%. Evidence schema 2 distinguishes this workload from the obsolete sub-nanosecond microbenchmark.

`axiusflow_engine` includes an ignored release-only verifier for the resident cached-demand boundary. It primes one production `MarketService` with deterministic 350-bar Coinbase history and exact BTC-USD 1m/5m plus ETH-USD 1m series, then measures direct coordinator demand-to-snapshot, authenticated local-socket demand through the blocking desktop `EngineClient` until a decoded covering snapshot arrives, cached timeframe and symbol switching, connect/authenticate/attach/restore, and a complete 20-consumer shared-series IPC batch. The same generation, full series identity, consumer identity, protobuf framing, authentication, conflation, and coordinator code used by the shipping desktop is exercised; there is no benchmark-only transport or public fixture API. Schema 3 uses 32 warm-ups plus 128 measured single-consumer samples, 8 warm-ups plus 32 measured multi-consumer batches, records p50/p95/p99, and samples the benchmark process working set before engine startup and after engine startup, direct demand, authenticated IPC/multi-consumer demand, and attach/restore. It fails when cached IPC demand or switching exceeds the 20 ms p50 or 50 ms p95 engineering targets. Memory remains measured evidence without an invented budget, and the test-process result does not replace a standalone long-running resident-engine measurement. Provider priming remains outside timed switch arms, and first derived-interval creation remains separately measured by `axiusflow_market_data_performance`. Run it with `cargo test --release -p axiusflow_engine release_cached_demand_ipc_and_multi_consumer_performance -- --ignored --nocapture`.

The first recorded local release run on 250,000 one-minute bars produced 715 encrypted segments (15.5 MB payload): cold publication 1.37 seconds, warm catalog open 22 ms, coverage discovery 1 ms, full warm read 78 ms, decode 8 ms, and first recent segment in under 1 ms. Removing unconditional sort and duplicate-buffer copies reduced sorted-source timeframe aggregation from 7.2-8.4 ms to 1.3-3.1 ms across 5-minute through daily intervals on the same run. Machine-specific JSON is transient evidence under `.cache`, not a portable product guarantee.

The first direct-path Windows release run after removing the engine subscription from first pixels measured 22.9 ms from window setup to the first GPUI frame and 6.0 ms p50 update-to-frame latency with advancing DWM refresh evidence on a 165 Hz display. A separate 100,000-bar storage run measured the first recent segment in 155 microseconds, a complete warm read in 30 ms, a cold five-minute derivation in 966 microseconds, and its repeated immutable-cache lookup below the microsecond timer resolution. These numbers are machine-specific regression evidence, not physical panel scanout guarantees.

Four immediate schema-8 Windows release runs on that display measured 23.1223-30.1152 ms from window setup to first frame, 0.2105-0.2379 ms to install a real 600-bar same-series covering snapshot through `OriginChartView::load_replay`, and 5.3870-6.0956 ms from replacement start to the following frame callback. Each run then recorded 128 samples apiece through the real symbol-input change callback, Enter-submit callback, instrument-selection handler, and interval-selection handler. Their p99 ranges were 0.0003-0.0007 ms, 0.0067-0.0810 ms, 0.0014-0.0028 ms, and 0.0064-0.0132 ms respectively; GPUI frame registration measured 0.0006-0.0008 ms p99 across 418 samples per run, and worst update-to-frame p99 was 7.3820 ms. Every run observed zero queued updates before each delta submission, exactly one after, zero overflows across 256 measured replay frames, an advancing DWM timeline, and zero late, dropped, or missed-frame growth. Desktop process working-set growth ranged from 66,101,248 to 66,744,320 bytes. Schema 8 deliberately makes no resident-engine demand-latency claim: cached symbol/timeframe demand remains covered by the resident-engine verifier. The current desktop has no tab surface, so tab-switch timing remains open rather than being simulated.

The first valid schema-2 diagnostics run on the same 24-logical-CPU Windows development host measured a 738 ns disabled p99 and 746 ns detailed p99 per trade (1.09% regression), plus an 805 ns disabled p99.9 and 818 ns detailed p99.9 (1.62% regression), with zero gaps, overflows, or recoveries across 2,048 measured 8,192-trade samples. The JSON remains transient machine-specific evidence under `.cache`; the repeatable release command and fail-closed thresholds are the repository gate.

The first post-refactor resident-engine release run on that host measured cached direct demand-to-snapshot at 0.0038/0.0041/0.0054 ms, authenticated IPC demand-to-decoded-snapshot at 0.0877/0.1216/0.1477 ms, and connect/authenticate/attach/restore at 0.1022/0.1714/0.1860 ms p50/p95/p99. A complete 20-consumer shared-series IPC batch measured 1.9414/2.3279/2.4852 ms, or 0.09707 ms per consumer at the p50 batch rate. These are machine-specific regression measurements; the repeatable release verifier and its fail-closed warm-demand budgets are the durable gate.

The first schema-2 run with provider priming fully fenced on that host measured cached authenticated 1m/5m timeframe switching at 0.0912/0.1109/0.1710 ms and cached BTC/ETH symbol switching at 0.0929/0.1451/0.2205 ms p50/p95/p99. Three immediate optimized repetitions passed the same fail-closed local-interaction budgets; across all four runs, the worst observed p99 values were 0.1710 ms for timeframe switching and 0.2205 ms for symbol switching. These numbers exclude provider priming and therefore describe the resident cached-switch boundary rather than network or cold derivation latency.

The first schema-3 Windows release run sampled the process hosting that production coordinator and authenticated IPC boundary at 9,940,992 bytes before engine startup, 11,128,832 bytes after startup, and 12,255,232 bytes after the complete direct, switching, multi-consumer, and attach workload. Sampled total growth was 2,314,240 bytes and sampled post-start workload growth was 1,126,400 bytes. The same run kept IPC, switching, attach, and 20-consumer latency within the existing fail-closed budgets. This is bounded-workload test-process evidence, not a standalone resident-engine endurance or warm-lifecycle measurement.

An optimized standalone Windows engine launch followed by the real authenticated `axiusflow_engine --shutdown` action initially acknowledged shutdown 1,132.567 ms after process launch, measured 10,641,408 bytes of working set immediately before the successful request, and exited with code zero 3.902 ms after acknowledgement. With staged market-worker shutdown and final hot-set persistence active, the next optimized run retried the command until IPC readiness, acknowledged at 2,616.279 ms, measured 10,272,768 bytes immediately before the successful request, wrote `hot-set-0000000002.frame`, and exited with code zero 1.713 ms after acknowledgement, leaving no engine process. A second lifecycle run wrote revision 3, retained exactly revisions 2 and 3, and again left no process. After native network/power monitor cancellation and join ownership was added, the optimized engine exposed 19 process threads immediately before shutdown, acknowledged at 2,706.200 ms, measured 10,203,136 bytes of working set, and exited with code zero 8.671 ms later with no remnant. The measurements verify the production binary, native installation credential, fixed local endpoint, nonblocking accept loop, lifecycle command, native monitor and market-worker joins, bounded manifest retention, and bounded process exit. They are individual cold lifecycle samples, not warm-reopen, long-running memory, connected-provider shutdown, or percentile evidence.

A release Windows native-close run with an established engine exited the default desktop in 225.858 ms and preserved the engine until an explicit code-zero cleanup command. The same native close under `--exit-with-desktop` exited the desktop in 44.567 ms and left neither process. A deterministic coordinator regression for `--keep-markets-live` detaches the final client, observes no provider stop, advances the canonical forming bar while no consumer exists, reattaches from that advanced snapshot without another history fetch, and then proves that switching back to ordinary warm mode releases Coinbase realtime. An optimized Windows markets-live process run created its first native window in 140.024 ms, closed it through `WM_CLOSE`, created a second desktop window in 119.009 ms against the same surviving engine PID, closed it through the same native path, and then removed the engine with a code-zero authenticated shutdown. These are policy-path samples and deterministic boundary evidence rather than close-latency percentiles, end-to-end GPUI snapshot-to-render timing, credentialed-provider evidence, or connected-provider shutdown evidence.

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
- `provider_kit`: vendor reference material, not application source or a design template.

## Data correctness

Every streamed view has a local series identity, session generation, publication generation, and source sequence. A selection change invalidates older work. Covering snapshots establish a known state; deltas are accepted only when their sequence and session continue that state. A new session requires a covering snapshot, and older sessions or publication generations are rejected. A detected gap, stale response, or provider reconnect requires recovery from a covering snapshot.

Application replay snapshots retain session generation, publication generation, source sequence, provenance, and checksum evidence as pure in-process values; they are not a second IPC protocol. Desktop-to-engine traffic uses only versioned `local_engine_protocol`, while Rithmic vendor protobuf remains isolated inside its adapter. Persisted workspace and encrypted history use separate versioned schemas.

History and live data meet at one explicit handoff boundary:

1. Determine local coverage.
2. Fetch only missing provider history within provider limits.
3. Validate and publish a covering ordered snapshot.
4. Buffer or sequence live events during hydration.
5. Admit only live events newer than the accepted history boundary.

Prices and quantities use fixed-point or provider-exact representations. Floating-point conversion is a presentation concern and must not become the source of stored or transmitted truth.

## Concurrency and backpressure

Provider sessions, resident-engine workers, and GPUI have distinct owners. Do not let the UI thread perform blocking network, disk, process, or shutdown work. Do not let background workers mutate GPUI state directly. Desktop window close moves each worker's bounded detach acknowledgement to the GPUI background executor; app quit awaits those tasks before any complete-engine request. Complete engine shutdown is authenticated and bounded by one two-second process deadline. It freezes persistent workspace mutation, writes the final hot-set manifest on a named one-shot worker, stops new IPC acceptance and market publication, cancels in-flight history and Coinbase realtime work, closes Rithmic worker controls, drains accepted local-history requests, joins the top-level market workers, and then waits for client sessions with any remaining time. Each Rithmic worker owns cancellation handles and join handles for its blocking native network/power helpers; dropping the worker's environment receiver first releases bounded sends, native cancellation unblocks OS waits, and helper joins complete before the Rithmic worker exits. A stuck or panicked flush/market worker or an expired client-session wait causes process failure before unconditional process termination.

Desktop IPC is authenticated platform-local socket transport only; neither the engine, client, nor protocol manifest admits a public-server framework. Each accepted client has an independent bounded server session, while market publications remain pull-based: the engine retains one latest slot per semantic publication class and writes only a requested response to a client socket. Realtime queues are drained under a fixed budget before the coordinator handles one control command, provider writers use bounded nonblocking submission with explicit recovery on overflow, and a non-reading client neither fills a push queue nor blocks another client's control path. A deterministic 32-trade slow-consumer regression proves live processing reaches the final covering state, the dormant consumer retains at most the seven semantic slots, and another consumer can change visibility and series generation; a separate authenticated two-client regression proves a dormant market socket cannot starve resource-mode or demand control.

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

The `axiusflow_naming_check` test target also enforces Cargo dependency direction: backend/domain/storage/adapters cannot depend on UI, chart integration cannot depend on provider adapters, the desktop cannot depend on backend implementation crates, and the engine manifest remains the backend composition root. It also prevents the pure application/domain/`market_engine` core from acquiring IPC, serialization, transport, or async-runtime dependencies and prevents the deleted desktop market/provider runtime wrappers from re-entering the workspace. Provider-network substitution stays at legitimate ownership boundaries: the coordinator has private `HistorySource` and `RealtimeSource` seams, while the Rithmic adapter uses its provider-session driver, history transport, credential-vault boundary, and local TLS fixtures. Deterministic fixtures exercise history success, delay, failure, cancellation and queue saturation plus realtime start, disconnect, reconnect, overflow, stale generation, duplicate/gap rejection, and depth recovery without credentials or Internet access. Internal engine work remains typed Rust calls and bounded channels: protobuf framing exists only at the authenticated desktop-process boundary. Runtime state is explicitly constructed under `EngineState`, `MarketService`, and its coordinator-owned `MarketEngine`; there is no mutable process-global market owner. Pure bucketing, aggregation, fixed-point conversion, coverage, sequence, cache-key, and page-validation code remains ordinary deterministic functions with direct tests, while the deleted `application::stream_runtime`, `desktop_market_runtime`, `desktop_provider_runtime`, and Rithmic desktop-driver forwarding chain has not been recreated.
