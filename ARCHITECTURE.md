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
