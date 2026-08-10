# Axiusflow Architecture

## Product

Axiusflow is a local-first professional trading platform in the category of MotiveWave and ATAS. It is a native Rust desktop application for live market data, charting, market-depth workflows, local history, and eventually safe trade execution.

The product target is best-in-class interactive and streaming performance without becoming a heavyweight workstation. Architecture is a product advantage: low latency, predictable resource use, immediate local startup, strong failure isolation, and simple ownership should distinguish Axiusflow from existing platforms.

This document describes the current architectural direction. Source code, tests, and measured runtime behavior are the implementation truth.

## Principles

1. Local first. Core viewing and workspace behavior must not depend on an Axiusflow cloud service. The resident engine owns provider sessions, cached data, and durable local state.
2. Correct before fast. Market data, sequence handling, history/live handoff, prices, quantities, and future orders must remain exact under disconnects, retries, and restarts.
3. Measure performance. Optimize observed hot paths and tail latency. Do not add speculative caches, concurrency, unsafe code, or platform-specific acceleration.
4. Keep ownership obvious. Each mutable resource has one clear owner. Communication across threads and processes uses bounded messages and explicit snapshots or deltas.
5. Stay lightweight. Prefer Rust and the standard library, operating-system facilities, and dependencies already in the workspace. New layers and dependencies require a present need.
6. Preserve the native experience. Windows, macOS, and Linux behavior should follow each operating system's conventions. Native windowing, credential storage, power, network, and display facilities belong behind narrow platform boundaries.
7. Fail safely. Stale generations, gaps, malformed provider data, unavailable credentials, and interrupted persistence must produce explicit recovery or errors, never plausible but incorrect trading state.

## Runtime topology

Axiusflow has two application processes:

- `axiusflow_desktop` owns the GPUI window, terminal interaction, presentation state, and chart integration.
- `axiusflow_engine` is a per-user resident local process. It owns provider connectivity, workspace state, hot-series state, and publication to authenticated local clients.

The desktop starts or connects to the sibling engine over a local socket. A random installation token stored in the operating system credential vault authenticates that connection. The engine retains covering state for newly attached clients and publishes bounded incremental updates to active subscribers. When no desktop client is attached, the engine can remain warm instead of tying market-data lifecycle to a window.

```text
Provider sockets
    -> provider adapters
    -> provider runtime and coordinator
    -> canonical market/history models
    -> resident engine publication
    -> authenticated local protocol
    -> desktop presentation model
    -> terminal UI and Origin chart renderer
```

## Workspace boundaries

### Applications

- `apps/desktop`: native GPUI application, window chrome, terminal composition, UI event routing, and desktop diagnostics.
- `apps/engine`: resident engine executable plus its local IPC, authentication, workspace persistence, and client-session library.

### Domain and application

- `crates/domain/instruments`: provider-neutral instrument identity.
- `crates/domain/market_data`: canonical bars, intervals, order-book state, and related market semantics.
- `crates/application`: generation-aware client models, provenance validation, replay snapshots, and stream publication behavior.

These crates must not depend on UI or a particular provider.

### Providers and coordination

- `crates/adapters/coinbase_market`: Coinbase catalog, history, streaming, and wire behavior.
- `crates/adapters/rithmic_protocol`: Rithmic protocol, network sessions, catalog, history, and market-data behavior.
- `crates/adapters/market_protocol`: conversion between canonical market models and protobuf/wire representations.
- `crates/desktop_provider_runtime`: bounded provider-session lifecycle and recovery.
- `crates/provider_history`: provider-neutral pagination, rate limiting, coverage, scheduling, and history/live handoff.
- `crates/coinbase_coordinator`: current composition layer for Coinbase and Rithmic market workers. Despite its historical name, it coordinates the desktop/engine market-data vertical and should not absorb unrelated product logic.

Provider-specific types stop at adapter boundaries. Downstream code consumes canonical identities and market models.

### Local data and protocols

- `crates/desktop_storage`: SQLite metadata, encrypted local segments, and storage lifecycle.
- `crates/desktop_history`: local history cache behavior built on storage and provider-history contracts.
- `crates/local_engine_protocol`: typed messages and framing for desktop-to-engine IPC.
- `crates/protocols`: shared protobuf-backed stream contracts and sequence semantics.
- `crates/transport`: small transport framing primitives.

Persistent writes use revisioned or transactional publication so a crash cannot turn a partial write into current state. Credentials and sensitive provider material must use the native credential vault or zeroizing memory, not source files, logs, or plain-text configuration.

## Local market-data execution

The implemented local path is:

```text
Coinbase or Rithmic socket
    -> provider-specific decode and continuity validation
    -> ProviderSessionDriver generation fencing
    -> canonical MarketEvent / MarketBar values
    -> provider-history scheduling and coverage repair
    -> encrypted immutable local segments plus SQLite metadata
    -> worker-owned decoded cache and history/live handoff
    -> resident-engine replay snapshot or delta
    -> bounded desktop mailbox
    -> chart bridge and Origin Charts
```

`ProviderSessionDriver` is the shared live-session boundary implemented by Coinbase and Rithmic. `ProviderHistoryAdapter` is the shared paginated-history boundary. Authentication, transport framing, provider limits, product/catalog translation, and provider-specific recovery remain inside the adapters; downstream history, storage, engine, and UI code consumes canonical identities and values.

The path is local first by construction. Provider credentials stay on the user's machine, provider traffic terminates in the resident process, durable history is stored under the user's local data root, and Axiusflow has no cloud market-data dependency. A future control plane may distribute application metadata or licensing state, but it must not become a prerequisite for local cache hydration or sit in the licensed market-data path.

## Historical data and restart behavior

`desktop_storage` stores authenticated immutable segments and a keyed SQLite catalog. Segment publication is stage, sync, link, catalog commit; interrupted or corrupt files are recovered or quarantined rather than admitted as valid history. Catalog rows record exact provider/account/entitlement, instrument, resolution, time range, and source/schema/calendar/adjustment/correction revisions.

`provider_history::CoverageSnapshot` merges complete, confirmed-empty, invalidated, and quarantined ranges, then returns only the missing or damaged repair ranges. Visible repairs outrank adjacent prefetch. Provider paging, rate limits, continuation bounds, retry attempts, cancellation interests, and total in-flight work are bounded by `HistoryScheduler`.

`desktop_history::HistoryWorker` is owned by a market-data worker thread, never GPUI. It reads and decrypts bounded segments, validates contiguous sequence, maintains a byte- and entry-bounded decoded cache, shares immutable publications across charts, and coordinates the history/live cutover. Live items are buffered during hydration; a verified covering snapshot admits only the contiguous suffix newer than its watermark.

The current storage format already supports progressive recent-first reads because retained history is segmented and range-indexed. The current Coinbase composition does not yet expose arbitrary multi-year chart paging: it installs a fixed 300-bar working set and recomputes derived intervals when selected. That limit is an application-composition constraint, not a storage or provider-history constraint, and must be removed through a paged visible-range API rather than by loading years into one `Vec`.

## Timeframes

Provider-native history is requested at the closest supported source resolution. Coinbase bars are normalized and aggregated locally into the requested canonical interval; live one-minute trades incrementally update the active interval. Rithmic maps canonical chart intervals to its provider bar specifications behind its adapter.

The durable identity includes resolution, so native or previously materialized resolutions cannot be confused. Recomputable derived intervals may be retained under the derived-data quota, but the current platform does not yet maintain a general cross-provider derived-timeframe cache. Until that exists, timeframe changes can repeat aggregation. The required design is one canonical lowest-practical source series per provider capability, page-local aggregation, immutable derived chunks keyed by source revision plus interval, and incremental tail updates. Calendar intervals must retain their explicit UTC/exchange-calendar rules instead of being approximated as fixed seconds.

## Concurrency model

- Provider sessions own their sockets and callback generations.
- The resident market worker owns provider orchestration, history scheduling, storage access, aggregation, and history/live handoff.
- Blocking history and storage work stays off GPUI and communicates through bounded channels.
- Selection generations make obsolete symbol and timeframe results stale; stale work cannot overwrite the new selection.
- Covering snapshots may replace older covering snapshots. Non-conflatable deltas, sequence gaps, and queue overflow require recovery rather than silent loss.
- Work is bounded by queue item/byte capacities, scheduler in-flight limits, cache bytes, segment bytes, chart bindings, handoffs, retries, and deadlines. No request creates an unbounded thread pool or unbounded queue.

Independent instruments should eventually be scheduled as independent bounded jobs so one slow provider request cannot head-of-line block another. The existing generation and cancellation contracts are the basis for that change; a second task system or cloud queue is not required.

## Performance evidence and instrumentation

Always-on feed diagnostics count trades, quotes, depth updates, publications, gaps, duplicates, malformed messages, stale callbacks, overflows, UI conflation, queue occupancy, memory, and lifecycle state. Opt-in fixed histograms cover socket-to-decode, decode-to-canonical, canonical-to-model, model-to-UI, UI-to-frame, and frame-to-present boundaries without unbounded label cardinality.

`HistoryWorker::metrics` additionally records local storage operations and bytes, memory-cache hits and misses, decode work, provider snapshots, live items, duplicates, and cumulative storage-write, storage-read, decode, snapshot-install, and live-publication nanoseconds. These metrics contain no provider payloads, credentials, account text, or instrument text.

`axiusflow_market_data_performance` is the deterministic offline regression runner. It uses the production Coinbase segment codec, encrypted `HistoryStore`, catalog coverage planner, and interval aggregator. Its JSON evidence records cold publication, warm catalog open and discovery, recent-first time to first usable segment, full warm read and decode, repeated timeframe-switch latency, payload/stored bytes, coverage gaps, resident memory, and sampled process CPU. CI runs the release binary on 100,000 bars, enforces broad anti-regression budgets, and retains the JSON artifact for build-to-build comparison. Provider-network benchmarks remain separate because provider latency, entitlements, and credentials are not deterministic CI inputs.

The first recorded local release run on 250,000 one-minute bars produced 715 encrypted segments (15.5 MB payload): cold publication 1.37 seconds, warm catalog open 22 ms, coverage discovery 1 ms, full warm read 78 ms, decode 8 ms, and first recent segment in under 1 ms. Removing unconditional sort and duplicate-buffer copies reduced sorted-source timeframe aggregation from 7.2-8.4 ms to 1.3-3.1 ms across 5-minute through daily intervals on the same run. Machine-specific JSON is transient evidence under `.cache`, not a portable product guarantee.

## Ranked local-data roadmap

1. Replace the Coinbase 300-bar composition limit with a paged visible-range contract and recent-first publication. This has the largest user impact and moderate implementation risk because storage, coverage, and handoff already support the required pieces.
2. Add immutable derived-timeframe chunks keyed by source revision and update only the active tail. This removes repeated aggregation during timeframe switching while keeping memory bounded.
3. Split history scheduling into fair per-instrument lanes under one global bound, with generation cancellation before decode and aggregation. This prevents slow or obsolete work from delaying the active selection.
4. Extend deterministic performance scenarios to symbol churn, interrupted publication recovery, concurrent live plus history, and multi-million tick streams. Use synthetic transports in CI and credentialed provider runs only as local evidence.
5. Add chart-preparation and first-render timestamps to the existing latency chain so time to first pixels is measured across the process boundary rather than inferred from data readiness.
6. Evaluate memory mapping only after paged reads and derived chunks are measured. Encryption and authenticated recovery currently require bounded read/decrypt buffers, so mapping ciphertext alone is not automatically a win.

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

Every streamed view has an identity and generation. A selection change invalidates older work. Covering snapshots establish a known state; deltas are accepted only when their sequence and generation continue that state. A detected gap, stale response, or provider reconnect requires recovery from a covering snapshot.

History and live data meet at one explicit handoff boundary:

1. Determine local coverage.
2. Fetch only missing provider history within provider limits.
3. Validate and publish a covering ordered snapshot.
4. Buffer or sequence live events during hydration.
5. Admit only live events newer than the accepted history boundary.

Prices and quantities use fixed-point or provider-exact representations. Floating-point conversion is a presentation concern and must not become the source of stored or transmitted truth.

## Concurrency and backpressure

Provider sessions, the resident engine, and GPUI have distinct owners. Do not let the UI thread perform blocking network, disk, process, or shutdown work. Do not let background workers mutate GPUI state directly.

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

The only Markdown files allowed in the Axiusflow repository are root-level `ARCHITECTURE.md` and `AGENTS.md`. Do not create plans, reports, reviews, roadmaps, temporary Markdown, package READMEs, or duplicate architecture documents. Put durable architecture here, executable behavior in code and tests, and transient work notes outside the repository.

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

Focused conformance and release-mode performance checks supplement this gate for provider, persistence, IPC, UI, and latency changes. A passing compile is not proof of runtime correctness.
