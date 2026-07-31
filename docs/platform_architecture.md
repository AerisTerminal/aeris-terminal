# Axiusflow Target Architecture

**Status:** Governing target-state architecture, revision 2
**Audience:** Product engineering, platform engineering, security, compliance, data engineering, and technical leadership
**Primary language:** Rust
**Supported native operating systems:** Windows, Linux, and macOS
**Last updated:** 2026-07-31

## 1. Purpose and architectural posture

Axiusflow is a high-performance trading and financial platform. It combines a professional native terminal, broker-connected execution, portfolio and journal analytics, market-data visualization, alerts, research, and ordinary product functionality such as identity, subscriptions, workspaces, notifications, administration, and support.

The objective is not an unqualified claim of "absolute" latency. No architecture can remove exchange distance, provider latency, internet routing, operating-system scheduling, or display scanout. The objective is:

> Deliver the lowest practical and repeatable latency within each declared hardware, operating-system, network, durability, and correctness profile, with no hidden loss of financial correctness.

This document separates stable structural decisions from benchmark-driven runtime choices:

- **Structural decisions** are intended to remain stable: Rust-first domains, provider-neutral contracts, Origin Charts, GPUI boundaries, fixed-point financial values, authoritative PostgreSQL state, Redpanda durability, replay, entitlement enforcement, and reconciliation.
- **Runtime topology and acceleration profiles** evolve only through measured architecture decisions. A transport, queue, allocator, kernel interface, or deployment boundary is never permanent merely because it appeared in the first implementation.

Axiusflow supports Windows, Linux, and macOS as first-class native desktop targets. The portable path must remain fully functional and correct on all three. Linux-only acceleration can provide lower latency for compatible direct-feed, on-premises, or dedicated-host deployments, but it cannot become a requirement for using the product.

The implementation may be delivered in vertical slices, but every slice must fit these contracts. Delivery sequence is not permission to create fake external services, disposable domain models, unbounded queues, or a second chart state model.

---

## 2. Non-negotiable design principles

### 2.1 Correctness before superficial speed

A fast incorrect trading system is unusable. The order path is deterministic, idempotent, recoverable, and reconcilable. Money, quantity, price, and fee calculations use checked fixed-point or integer representations. Every external side effect has a durable intent, provider correlation identifier, timeout state, and recovery strategy.

No low-latency profile may:

- send a live order before the profile's required durable-intent boundary;
- bypass authorization, entitlement, risk, or kill-switch checks;
- infer rejection from a provider timeout;
- silently continue authoritative processing across a sequence gap;
- replace evidence with an optimistic local guess.

### 2.2 Four operational planes

The platform has four explicit planes with independent scaling and failure policy:

1. **Control plane** — identity, authorization, entitlements, users, organizations, workspaces, subscriptions, instrument reference data, configuration, administration, and support.
2. **Real-time data plane** — feed ingest, protocol decoding, sequence validation, normalization, partition-owned latest state, conflation, direct fanout, and client streaming.
3. **Execution plane** — dedicated order ingress, authorization evidence, pre-trade risk, durable order intent, OMS state transitions, provider dispatch, ledger input, and reconciliation.
4. **Durable data plane** — PostgreSQL outbox/inbox, Redpanda, raw capture, S3/Parquet, ClickHouse projections, audit archives, replay, and asynchronous workflows.

Control-plane or analytical load must not consume the bounded resources reserved for real-time data or execution.

### 2.3 Performance by bounded ownership

Performance comes from predictable work, locality, bounded memory, explicit overload policy, and isolation. Hot paths use:

- stable partitioning and one active writer per partition;
- bounded single-producer/single-consumer paths where the topology permits;
- preallocated or pooled buffers after profiling proves value;
- no database lookup per tick;
- no analytical write in the synchronous order or display path;
- no global mutable lock shared by independent partitions;
- explicit copy, allocation, serialization, and queue-depth budgets;
- batching limited by a maximum latency deadline, not throughput alone.

### 2.4 One authority and one active writer

Domain authority and runtime write ownership are distinct:

- Better Auth owns credentials and authentication sessions.
- The authorization domain owns application grants, policy evidence, and market-data entitlements.
- The OMS owns normalized order state.
- The portfolio ledger owns fills, cash movements, lots, positions, and financial journals.
- The instrument domain owns internal instrument identity and provider mappings.
- The workspace domain owns layouts, watchlists, and saved user configuration.
- PostgreSQL owns authoritative transactional records.
- ClickHouse stores rebuildable projections and owns no financial truth.
- Redpanda retains and distributes durable events but is not a transactional database replacement.

Every ordered runtime partition also has exactly one active writer identified by `partition_id`, `owner_id`, and monotonic `ownership_epoch`. Handoff is fenced: a stale owner cannot publish accepted events after a newer epoch becomes active. Assignment, lease, handoff, recovery, and stale-writer rejection are observable and tested.

### 2.5 Stable internal contracts

Broker, exchange, vendor, GPUI, Better Auth, operating-system, cloud SDK, AF_XDP, DPDK, and transport-library types stop at adapters. Internal commands and events use versioned Axiusflow contracts. The same canonical event must be produced regardless of portable socket, Windows optimization, macOS optimization, AF_XDP, or DPDK ingress.

### 2.6 Replay is an operational capability

Raw market messages, normalized market events, provider order messages, business events, policy revisions, and calculation versions are replayable. Analytics projections, search indexes, dashboards, alerts, and client snapshots are reconstructable from authoritative records and versioned events.

### 2.7 Portable correctness and foundational acceleration

The portable implementation is the correctness reference, but it is not a delivery predecessor that must be completed before accelerated work starts. From the first market-data-plane increment, portable sockets, tuned Linux sockets, AF_XDP, and DPDK are parallel implementations of one bounded ingest contract. This prevents socket-specific buffer ownership, timestamping, batching, backpressure, or scheduling assumptions from becoming architectural constraints.

Every ingest profile is compiled and exercised against the same deterministic packet and replay corpus from Stage 1. Benchmark harnesses are implemented at the same time, but a result is published only for environments that actually exist. Lack of a premium direct feed, supported zero-copy NIC, colocation host, or privileged runner does not block production-complete portable/tuned software and the product features supported by the current authorized provider; it limits the readiness state and claims of the affected accelerated profile.

Each profile reports one ordered readiness state. Advancement requires evidence, and any incompatible change to pinned dependencies, driver, firmware, hardware, provider protocol, or profile configuration revokes the affected evidence and can downgrade the state:

1. `implemented` — adapter, isolated build target, exact pins, and safety/license review exist.
2. `fixture_validated` — deterministic packet, sequence, gap, replay, lifecycle, and error conformance passes using software-accessible facilities.
3. `hardware_validated` — qualified NIC/driver/host testing proves startup, queue ownership, active mode, teardown, load, and failure behavior.
4. `provider_certified` — an authorized compatible provider feed proves framing, recovery, redundancy, provenance, and sustained/burst behavior.
5. `production_enabled` — security, operations, hardware, provider, and profile-specific performance gates all pass.

The product can ship with `portable_socket` and, where available, `tuned_linux_socket` at `production_enabled` while AF_XDP or DPDK remains `fixture_validated`. Production activation remains capability-gated: AF_XDP and DPDK require compatible feed protocols, qualified Linux hosts, supported NICs and drivers, required privileges, and verified queue configuration. A provider delivered only through TLS/TCP or a managed API does not become faster merely by selecting a kernel-bypass profile.

An acceleration profile must report its actual active mode and readiness. AF_XDP copy fallback is not reported as zero-copy. A DPDK process is not reported as active unless the expected poll-mode driver, queue, huge-page, and NIC configuration is verified. Unsupported activation fails explicitly; silent fallback is forbidden in benchmark or production profiles. Synthetic or virtual-device results prove software correctness only and cannot be presented as hardware latency, zero-copy, or provider certification.

### 2.8 Measured claims only

A component microbenchmark cannot be marketed as end-to-end latency. Every performance result names:

- the start and end boundary;
- p50, p95, p99, p99.9, maximum, and sample count;
- warm-up and measurement duration;
- operating system and kernel/build;
- CPU, topology, power mode, memory, NIC, driver, queue layout, and GPU;
- network distance, RTT, loss, and transport;
- feed rate, burst shape, key distribution, client count, workspace, and chart count;
- overload, recovery, and drop behavior.

### 2.9 Security in normal control flow

Authentication, authorization, step-up verification, market-data entitlements, risk limits, audit evidence, and revocation are explicit stages. Acceleration cannot create a privileged side door around them.

### 2.10 Logical boundaries do not require premature microservices

Bounded contexts, schema ownership, and dependency direction are defined immediately. Process boundaries are introduced only for measured isolation, security, scaling, availability, or ownership needs. A local module call is preferred over an unnecessary network hop.

### 2.11 Project-wide snake_case

Axiusflow-owned paths, crates, modules, functions, fields, services, APIs, schemas, topics, configuration keys, metrics, and public programmable identifiers use `snake_case`.

Narrow exceptions are explicit:

- Rust type, trait, and enum declarations use language-standard `UpperCamelCase`.
- Constants and environment variables use `SCREAMING_SNAKE_CASE`.
- Official third-party product and protocol names retain their published spelling.
- Legacy CSS compatibility keys remain byte-for-byte compatible while Rust accessors use `snake_case`.

Generated public and wire names must be configured explicitly; code generation is not an exception.

---

## 3. Language, safety, and authentication policy

### 3.1 Rust-first policy

Rust owns product logic, financial logic, backend services, streaming clients, analytics workers, and native UI behavior. Domain and application crates remain independent of GPUI, provider SDKs, operating-system APIs, and transport implementations.

The workspace keeps `unsafe_code = "forbid"` for first-party core crates. A Linux acceleration dependency that internally requires unsafe FFI must be exact-pinned, isolated behind a narrow adapter, audited, fuzzed, and approved by a dedicated architecture and security decision during Stage 1 integration—not deferred until production hardening. Acceleration is not a justification for spreading unsafe code through domains or application services.

### 3.2 Better Auth exception

Official Better Auth is TypeScript. The production architecture therefore has one deliberate language exception:

- `services/auth_service` uses official Better Auth with minimal exact-pinned configuration.
- It contains no trading, portfolio, market-data, entitlement, billing, or product business logic.
- It is isolated behind versioned HTTP and JWKS contracts.
- Rust services verify short-lived Ed25519 service tokens offline against immutable revisioned key snapshots.

If zero hand-written TypeScript becomes more important than official Better Auth, that is a separate architecture decision to replace the authentication authority. An unofficial compatibility implementation is not silently substituted.

References: [Better Auth introduction](https://www.better-auth.com/docs/introduction), [database model](https://www.better-auth.com/docs/concepts/database), [session management](https://www.better-auth.com/docs/concepts/session-management), and [JWT/JWKS plugin](https://www.better-auth.com/docs/plugins/jwt).

---

## 4. System topology

```text
                    external providers and venues
        market feeds         brokers/FIX         product providers
             │                    │                     │
             ▼                    ▼                     ▼
┌──────────────────────────── Axiusflow ────────────────────────────┐
│                                                                  │
│  real-time data plane              execution plane               │
│                                                                  │
│  portable / linux ingest           regional execution ingress    │
│            │                                  │                   │
│            ▼                                  ▼                   │
│  decoder → sequence validator       authorization/risk snapshot  │
│            │                                  │                   │
│            ▼                                  ▼                   │
│  fenced partition owner             durable OMS intent           │
│            │                                  │                   │
│       ┌────┴─────────┐                        ▼                   │
│       │              │                venue/broker gateway        │
│       ▼              ▼                        │                   │
│  latest state    durable tap                  ▼                   │
│       │              │                    provider               │
│       ▼              ▼                                            │
│  direct fanout   Redpanda/S3  ← outbox ← PostgreSQL              │
│       │              │                                            │
│       ▼              └────→ ClickHouse/replay/workers             │
│  regional streaming gateway                                    │
│       │                                                          │
│  control plane: auth, authorization, instruments, workspaces     │
│                                                                  │
└───────┬──────────────────────────────────────────────────────────┘
        │ HTTPS + binary WebSocket, or negotiated QUIC/WebTransport
        ▼
┌──────────────────────────────────────────────────────────────────┐
│ native terminal: Windows | Linux | macOS                         │
│ GPUI shell → Origin engine → shared ChartFrame → GPUI renderer   │
│ browser companion: Rust/WASM → Origin WebGPU/Canvas2D            │
└──────────────────────────────────────────────────────────────────┘
```

Redpanda is parallel to the fastest market-data fanout path, not an inline acknowledgement barrier. It remains the only general durable event backbone.

Cloud and clients share schemas and behavior, not databases or mutable memory.

---

## 5. Logical contexts and deployment model

### 5.1 Logical bounded contexts

The logical architecture retains separate ownership for:

- identity and authentication;
- authorization and entitlements;
- users, organizations, subscriptions, and workspaces;
- instruments, calendars, symbology, and corporate-action lineage;
- market ingest, normalization, bars, latest state, and streaming;
- broker connections, orders, execution, and risk;
- portfolio ledger and reconciliation;
- analytics, alerts, notifications, reports, and audit;
- protocol, persistence, replay, observability, security, and platform adapters.

A logical context may be a crate, module, database schema owner, or extracted service. It is not automatically a process.

### 5.2 Initial production deployables

The initial serious production topology uses a small number of deployables:

1. `auth_service` — isolated official Better Auth service.
2. `control_plane` — authorization, users, workspaces, instruments, subscriptions, and ordinary product operations, with internal module boundaries.
3. `edge_stream_gateway` — public control ingress, transport negotiation, regional streaming, entitlement enforcement, and connection lifecycle.
4. `market_data_plane` — provider gateway, normalization, fenced partition ownership, latest state, direct fanout, bars, and durable tap.
5. `execution_plane` — order ingress, risk, OMS, broker connectivity, ledger input, and reconciliation modules arranged as execution cells.
6. `data_worker` — outbox publication, ClickHouse projections, S3 jobs, analytics, alerts, reports, and asynchronous workflows.

A context is extracted when evidence shows at least one of:

- independent security or credential boundary;
- different scaling curve or hardware profile;
- failure isolation required by an SLO;
- a network hop already exists for an external boundary;
- release ownership requires independent deployment;
- profiling shows colocated work causes measurable interference.

Extraction cannot introduce shared-table writes or bypass a domain contract.

### 5.3 Repository direction

```text
apps/
  desktop/                  # Windows, Linux, and macOS GPUI terminal
  web/                      # Rust/WASM companion
  admin/                    # internal operations client
crates/
  domain/                   # framework-free business and financial domains
  application/              # use cases and ports; no GPUI/provider/OS types
  protocols/                # versioned Protobuf and public framing models
  realtime/                 # partition ownership, latest state, fanout contracts
  transport/                # portable and negotiated client/internal transports
  persistence/              # SQL, outbox/inbox, migration primitives
  streaming/                # Redpanda envelopes and durable consumers
  platform_runtime/         # OS capability and secure-storage abstractions
  observability/
  security/
  testing/
  adapters/
    market_protocol/
    portable_network/
    windows_network/
    macos_network/
    linux_socket_network/
    linux_af_xdp/
    linux_dpdk/
  ui/
    terminal_ui/            # GPUI compatibility boundary
    design_system/
    chart_integration/
services/
  auth_service/
  control_plane/
  edge_stream_gateway/
  market_data_plane/
  execution_plane/
  data_worker/
schemas/
  protobuf/
  redpanda/
  public_api/
infra/
  terraform/
  kubernetes/
  redpanda/
  performance_profiles/
tools/
  replay/
  feed_capture/
  broker_certification/
  latency_probe/
  load_test/
docs/
  decisions/
  runbooks/
  threat_models/
```

This is a target direction, not a claim that unimplemented directories or services already exist.

---

## 6. Cross-platform native client architecture

### 6.1 Supported operating-system contract

Windows, Linux, and macOS are first-class release targets. No product feature may depend on Linux-only behavior unless it is explicitly labeled as an optional acceleration capability.

The release matrix covers the supported architecture and operating-system combinations defined by release policy. At minimum, every release candidate must prove:

- application startup, update, and rollback;
- secure credential storage;
- system-browser authentication callback;
- keyboard, pointer, touchpad, focus, accessibility, and window behavior;
- transport fallback and reconnect;
- Origin geometry and interaction parity;
- GPU rendering and software/degraded fallback policy;
- suspend, resume, network migration, display/DPI changes, and multi-monitor behavior;
- deterministic market-stream and order-event semantics.

Zed distributes its GPUI-based product on macOS, Windows, and Linux, but Axiusflow still owns its exact pinned-revision certification matrix: [GPUI cross-platform product evidence](https://zed.dev/blog/gpui-2-on-preview).

### 6.2 Platform runtime boundary

`platform_runtime` exposes capability-oriented ports for:

- credential vault;
- PKCE loopback or registered URI callback;
- monotonic and wall clocks;
- file and cache locations;
- signed update and rollback integration;
- power and sleep notifications;
- display refresh, scale factor, and presentation timing where available;
- thread-priority and affinity hints;
- network capability reporting.

Adapters map these ports to Windows, Linux, and macOS facilities. Domain and application crates never branch on an operating-system name.

Credential material uses Windows protected credentials, macOS Keychain, or a supported Linux Secret Service implementation. Long-lived refresh material never lives in plaintext files; access tokens remain memory-resident and short-lived.

### 6.3 GPUI and Origin ownership

The native terminal uses:

- GPUI for windows, scene submission, input, actions, entities, and lifecycle;
- GPUI Component for docking, virtualized tables, forms, menus, dialogs, themes, and layout primitives;
- Origin Charts for every financial chart and trading visualization;
- a background network/model runtime bridged into GPUI through bounded messages.

GPUI types remain confined to `crates/ui/terminal_ui`, `crates/ui/chart_integration`, and application view modules. Domain and application crates remain GPUI-free.

Origin remains the sole owner of chart series, panes, scales, crosshair, drawings, indicators, layout, and `ChartFrame`. The GPUI adapter normalizes platform input and executes Origin primitives; it never creates a second chart state model.

```text
versioned market generation
           │
           ▼
origin_engine::ChartEngine
           │
           ▼
origin_render::ChartFrame
      ┌────┴─────────┐
      ▼              ▼
origin_render_gpui   origin_render_wgpu
native terminal      browser companion
```

### 6.4 Desktop market-data path

```text
network receive
  → bounded frame decoder
  → sequence and schema validation
  → partitioned client model writer
  → immutable generation publication
  → one merged UI command per frame
  → Origin incremental update
  → ChartFrame build
  → GPUI submission
  → physical presentation
```

Rules:

- Network I/O, decode, decompression, and model updates never run on the GPUI thread.
- One writer owns each client model partition; readers consume immutable generation handles.
- Large snapshots use bounded chunking and an atomic generation swap rather than piecemeal visible mutation.
- Intermediate replaceable quotes may be conflated before the frame boundary.
- Ordered book deltas are never conflated across a gap; a gap requests a snapshot.
- Account, order, fill, entitlement, and risk events use reliable delivery and cannot be discarded as display noise.
- Multiple charts subscribe to shared market generations instead of duplicating feed state.
- A chart receives at most one merged authoritative update per display frame.
- Crosshair and pointer invalidation do not rebuild static series geometry.

### 6.5 Client operating modes

| Mode | Availability | Behavior |
|---|---|---|
| `balanced` | Windows, Linux, macOS | Event-driven networking, normal power use, adaptive refresh. |
| `low_latency` | Windows, Linux, macOS | Dedicated decode/model workers, tighter batching deadlines, higher refresh, increased power use. |
| `linux_direct` | Qualified Linux enterprise systems only | Optional direct-feed adapter with compatible NIC and privileges; never required for ordinary cloud streaming. |

A user selecting `low_latency` receives a warning about power and thermal cost. Busy polling is never silently enabled on a consumer laptop.

### 6.6 Browser companion

The browser uses Rust/WASM for product logic and Origin WebGPU with Canvas2D fallback. It shares Protobuf contracts, domain value types, formatting, entitlement behavior, chart semantics, indicator formulas, and workspace serialization. It does not need to share every native presentation component.

---

## 7. Real-time market-data plane

### 7.1 Ingest profiles

All profiles terminate at the same provider framing and canonical decoder contracts. They are parallel foundational implementations, not a maturity ladder.

| Profile | Intended deployment | Characteristics |
|---|---|---|
| `portable_socket` | Development, cloud, ordinary servers | Tokio/evented sockets, kernel networking, portable operational model. |
| `tuned_linux_socket` | Dedicated Linux feed hosts | Pinned resources, RSS/queue steering, socket tuning, optional busy polling after measurement. |
| `linux_af_xdp` | Qualified direct-feed hosts | XDP redirect into UMEM rings; zero-copy only when NIC/driver support is verified. |
| `linux_dpdk` | Colocation or dedicated appliance hosts | Poll-mode userspace NIC access, isolated cores/NICs, huge-page and driver configuration. |

Stage 1 defines an `ingest_driver` port with explicit operations for capability discovery, queue binding, bounded borrowed receive batches, timestamp provenance, overflow reporting, release, and health. The port exposes neither socket handles, UMEM descriptors, nor DPDK `mbuf` objects. Provider framing and decoding complete while receive memory is valid; no borrowed NIC or driver pointer crosses a task, queue, or canonical boundary. Accepted canonical events are materialized into partition-owned bounded storage.

Each profile has its own adapter crate and build target. Portable binaries do not link AF_XDP or DPDK dependencies. Linux accelerated binaries exact-pin their bindings and native dependencies, record driver/firmware compatibility, and run the same decoder, sequence, gap, replay, and error fixtures as `portable_socket`.

Linux CI performs compile and unprivileged conformance checks. Where runner capabilities permit, it also exercises AF_XDP generic/copy behavior and DPDK software or virtual devices for bounded receive/release lifecycle, startup, shutdown, and fault handling. These lanes can establish `fixture_validated`; they do not establish zero-copy, physical-NIC latency, or provider behavior. Qualified hardware runners and compatible authorized feeds advance the same unchanged adapter through `hardware_validated`, `provider_certified`, and `production_enabled` when those resources become available.

AF_XDP is optimized Linux packet processing with RX/TX and UMEM ownership rings. Zero-copy is conditional on driver support; generic or copy mode remains a copy path: [Linux AF_XDP documentation](https://docs.kernel.org/next/networking/af_xdp.html).

DPDK is implemented from the foundation for compatible direct-feed and dedicated-host deployments, while remaining operationally restricted to infrastructure where poll-mode CPU consumption and NIC ownership are acceptable. It is not installed into ordinary desktops or general Kubernetes workloads: [DPDK Linux guide](https://doc.dpdk.org/guides/linux_gsg/index.html).

`io_uring` may reduce Linux syscall and asynchronous I/O overhead for suitable workloads, but it is not described as kernel bypass. It is evaluated as part of `tuned_linux_socket`, separately from AF_XDP and DPDK.

### 7.2 Canonical ingest path

```text
provider socket/NIC queue
  → protocol decoder
  → source-sequence validator
  → pre-resolved instrument mapping
  → canonical fixed-layout event
  → fenced single-writer partition
```

The canonical event header includes:

```text
market_event_header
- event_id
- instrument_id
- venue_id
- source_id
- source_sequence
- partition_id
- ownership_epoch
- exchange_timestamp
- provider_receive_timestamp
- nic_receive_timestamp_if_available
- axiusflow_receive_timestamp
- normalized_timestamp
- correction_flags
- quality_flags
- schema_version
```

Provider-specific objects do not cross this boundary. A ticker is never treated as instrument identity.

### 7.3 Direct fanout and durable tap

One accepted canonical event feeds two independently bounded branches:

```text
                           ┌→ latest state/conflation → direct fanout
canonical partition owner ┤
                           └→ durable tap → Redpanda/raw archive
```

The direct branch does not wait for a Redpanda acknowledgement. The durable branch does not silently disappear: its queue depth, oldest item age, produce latency, capture status, and gaps are first-class health signals.

Both branches preserve the same event ID, source sequence, ownership epoch, timestamps, provenance, and quality flags.

### 7.4 Message semantic classes

| Class | Examples | Delivery behavior |
|---|---|---|
| `state_replace` | top-of-book, indicative quote, feed health | Newer state supersedes older state; intermediate values may be conflated. |
| `ordered_delta` | order-book delta, incremental bar correction | Sequence required; any gap blocks continuation until a snapshot. |
| `reliable_event` | trade, halt, reference correction, entitlement change | Reliable stream and durable publication according to dataset policy. |
| `authoritative_event` | order, fill, ledger, risk decision | Reliable only; transactional durability and idempotency required. |
| `snapshot` | book, account, workspace, instrument state | Reliable, bounded, versioned, checksum-capable, atomically installed. |

QUIC datagrams may carry only data whose application semantics tolerate loss and define recovery. They never carry orders, fills, ledger entries, policy changes, or the sole copy of an ordered delta.

### 7.5 Latest state and client recovery

A latest-state service is a memory-resident, partition-owned projection, not a new authority. It provides bounded snapshots for new subscriptions and resynchronization. Snapshot generation is versioned and tied to the partition sequence/epoch.

If a client is slow:

- `state_replace` updates are conflated to the newest entitled value;
- independent subscription classes receive independent budgets;
- ordered streams trigger resnapshot rather than unbounded buffering;
- the server may downgrade depth, pause a subscription, or disconnect;
- authoritative account events are never silently dropped.

### 7.6 Raw capture and data quality

Raw source capture runs on an independently bounded path to local segments and S3 manifests. Failure to capture is observable and creates an explicit evidence gap. It cannot block the decoder indefinitely or be silently represented as complete history.

Quality checks include sequence gaps, timestamp regression, invalid scales, crossed/locked policy, impossible OHLC relationships, duplicate event IDs, venue/session mismatch, corporate-action discontinuity, and provider divergence.

### 7.7 Deterministic bars and derived data

Bars and derived market series are deterministic functions of versioned source sets, session calendars, adjustment policy, and correction rules. Late data creates correction events; it does not invisibly rewrite history.

---

## 8. Client and internal transport architecture

### 8.1 Named transport profiles

| Profile | Use | Required availability |
|---|---|---|
| `control_https` | REST/JSON and typed control operations over HTTPS/HTTP/2 | All clients |
| `stream_websocket` | TLS binary WebSocket snapshots and deltas | All native clients and browsers |
| `stream_quic` | QUIC reliable streams plus negotiated datagrams | Native clients after certification |
| `stream_webtransport` | Browser QUIC/WebTransport capability | Optional negotiated browser path |
| `internal_rpc` | Protobuf gRPC over HTTP/2 with mTLS | Internal synchronous operations |
| `internal_realtime` | Private low-latency framed link between partition owners and gateways | Dedicated data-plane deployments |

Binary WebSocket remains the universal fallback. QUIC is preferred only when the route, implementation, firewall behavior, loss handling, and operating-system matrix pass the benchmark and resilience gates.

QUIC provides secure multiplexed streams, low-latency establishment, and path migration: [RFC 9000](https://www.rfc-editor.org/info/rfc9000/). QUIC DATAGRAM adds negotiated unreliable datagrams that are congestion controlled and not retransmitted: [RFC 9221](https://www.rfc-editor.org/info/rfc9221/).

### 8.2 QUIC application profile

A single authenticated connection can carry:

- reliable control stream for subscription lifecycle and snapshots;
- reliable account stream for orders, fills, positions, balances, alerts, and entitlement changes;
- reliable ordered market streams where loss is not semantically acceptable;
- datagram flows for self-identifying, replaceable market updates.

Datagram payloads include a flow identifier, subscription identifier, event sequence, schema version, and semantic class. They fit the discovered path MTU; application fragmentation is not assumed.

Zero-RTT is forbidden for orders, cancels, replacements, funding actions, policy changes, or any non-replay-safe command. It may be evaluated for idempotent subscription restoration with an application replay defense.

### 8.3 Portable operating-system networking

The baseline Rust transport uses portable evented networking and the same protocol on all supported systems. Optional OS adapters are profiling decisions:

- **Windows:** IOCP provides efficient queued asynchronous completion; RIO adds registered buffers and request/completion queues for qualified high-throughput paths. Neither is described as general desktop kernel bypass. References: [Windows IOCP](https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports) and [Winsock RIO request queues](https://learn.microsoft.com/en-us/windows/win32/winsock/riorqueue).
- **macOS:** the portable path remains valid; Network.framework may be evaluated behind the transport port for connection establishment and mobility behavior. Reference: [Apple Network.framework](https://developer.apple.com/videos/play/wwdc2018/715/).
- **Linux:** evented sockets are the default. Tuned sockets, AF_XDP, and DPDK are separate server/direct-feed profiles with explicit capability checks.

No OS-specific adapter changes application framing or domain semantics.

### 8.4 Internal RPC

Synchronous internal operations use Protobuf gRPC over HTTP/2 with deadlines, mTLS workload identity, bounded safe retries, circuit breaking, concurrency limits, and propagated actor/correlation/causation context.

Redpanda is not used for request/reply, pre-trade risk RPC, or client subscription control.

### 8.5 Encoding and copy budget

- No JSON in market-data, execution, or internal event hot paths.
- Protobuf remains the durable and control contract.
- A compact versioned binary client frame may wrap or specialize generated payloads after compatibility tests.
- A gateway encodes each equivalent fanout payload once per entitlement/transport class, not once per subscriber.
- Scatter/gather and registered buffers are used only when they reduce measured copies without extending lifetimes unsafely.
- Every benchmark reports allocations and bytes copied per message stage.

---

## 9. Execution plane

### 9.1 Execution cell

An execution cell owns a stable set of broker accounts in one home region. It colocates hot-path modules while preserving logical contracts:

```text
client command
  → dedicated regional execution ingress
  → token and policy evidence validation
  → idempotency and instrument validation
  → pre-trade risk using immutable versioned snapshots
  → OMS durable intent transaction
  → broker/venue gateway dispatch
  → provider acknowledgement or unknown outcome
  → normalized execution event
  → ledger and reconciliation
```

The general control plane is not synchronously queried per order. Authorization policy, account permissions, instrument rules, positions, buying power, limits, and market-health inputs are preloaded as bounded versioned snapshots. Missing, expired, or stale required state fails closed.

### 9.2 Durable acceptance definition

`accepted` means the normalized intent and provider client order ID are durably committed to the authoritative trading PostgreSQL boundary and can be recovered after process loss. It does not mean the provider or venue accepted the order.

States distinguish at least:

```text
received → validated → risk_approved → route_pending → submitted
         ↘ rejected                    ↘ unknown_outcome
submitted → acknowledged → partially_filled → filled
submitted → cancel_pending → canceled
submitted → replace_pending → acknowledged
```

A timeout never becomes `rejected` without provider evidence.

### 9.3 Low-latency execution without correctness loss

- Hot-path modules run in one execution deployable or on the same dedicated host until evidence requires extraction.
- Prepared statements, warm connections, bounded pools, and prevalidated immutable snapshots remove avoidable work.
- Provider dispatch begins immediately after the required durable commit.
- Transactional outbox publication is asynchronous to response and provider dispatch.
- Venue-adjacent gateways are deployed for professional routes where commercial access permits.
- Retail aggregation adapters are never marketed as institutional low-latency execution.
- A future alternative durable journal requires a separate decision proving replication, fencing, recovery, PostgreSQL convergence, and audit semantics. Send-before-durable is not an optimization option.

### 9.4 OMS, ledger, and reconciliation

Provider messages are inputs to the OMS state machine, not direct row mutations. Invalid transitions are quarantined. The immutable portfolio ledger owns fills, cash, fees, taxes, lots, positions, corrections, and reconciliation evidence. Corrections append compensating entries.

Reconciliation independently compares internal orders, fills, positions, cash, fees, and provider snapshots. A live stream is never treated as proof of completeness.

### 9.5 Execution tiers

- **Regional durable execution:** cloud-region cell, correctness-first, intended for professional terminal order flow.
- **Venue-adjacent execution:** dedicated Linux host near provider/venue infrastructure, pinned resources, direct certified adapter, and separately measured latency.

Axiusflow does not claim that an internet-connected desktop is an HFT colocation engine. The same order contract can route to a venue-adjacent cell without changing the client domain.

---

## 10. Control plane, identity, and authorization

### 10.1 Edge and control responsibilities

The public edge handles TLS, request limits, JWT verification, session/device context, request IDs, deadlines, protocol negotiation, idempotency headers, rate limits, and coarse routing. Professional streaming and execution may use dedicated regional ingress after the same identity and policy evidence has been established.

The control plane owns users, organizations, teams, workspaces, watchlists, subscriptions, instrument reference data, feature policy, exports, and administration. Large imports/exports are object-store jobs.

### 10.2 Authorization

Authentication answers who the principal is. Authorization answers what that principal may do. Authorization owns grants, account permissions, trading permissions, administrative capabilities, data entitlements, classification, jurisdiction restrictions, policy versions, and evidence.

Sensitive `trade` and `administer` policy matches require additional assurance stages. A policy match alone is not final permission to trade.

Hot paths consume immutable signed or authenticated policy snapshots with revision and expiry/freshness metadata. Revocation distribution is measured. Sensitive actions can require a live step-up decision.

### 10.3 Instrument identity

An internal instrument ID is stable and provider-neutral. Records include venue/listing, asset class, currency, price/quantity precision, tick/lot rules, sessions, lifecycle, corporate-action lineage, and licensed aliases. Provider adapters resolve mappings before hot-path processing.

### 10.4 Native login

The native terminal uses the system browser with PKCE and a protected loopback or registered callback. It does not embed a login webview. Platform adapters store refresh material in the operating-system vault.

---

## 11. Durable data plane

### 11.1 Redpanda role

Redpanda is the only general durable event backbone. It provides partitioned retention, schema-governed events, replay, asynchronous workflows, and projection input. It is not:

- the direct tick-to-client barrier;
- request/reply transport;
- synchronous risk RPC;
- a distributed lock;
- a PostgreSQL consistency replacement;
- an unlimited raw packet store.

Production policy includes replication, minimum in-sync replicas, `acks=all` for durable classes, idempotent producers, TLS, workload identity, ACLs, local NVMe, tiered storage, AZ awareness, and separate quotas or clusters for market throughput versus financial events.

References: [Redpanda architecture](https://docs.redpanda.com/current/get-started/architecture/), [transactions](https://docs.redpanda.com/current/develop/transactions/), and [Schema Registry](https://docs.redpanda.com/current/manage/schema-reg/schema-reg-overview/).

### 11.2 Schema policy

Durable domain events use Protobuf with backward-transitive compatibility by default. Field numbers are never reused. Semantic meaning does not change under an existing field. Fixed-point values are structured, not `double`. Envelopes include event ID, schema version, producer, timestamps with clock meaning, correlation, causation, partition, and ownership epoch.

Topic names follow:

```text
<environment>.<domain>.<entity>.<event_family>.v<major>
```

Topics are not created per symbol or user.

### 11.3 Partition and delivery semantics

Partition keys reflect correctness:

| Event family | Partition key |
|---|---|
| market source stream | `instrument_id` plus `source_id` |
| order and execution lifecycle | `broker_account_id` |
| ledger entries | `ledger_account_id` |
| positions | `portfolio_account_id` |
| workspace | `workspace_id` |
| entitlement | `principal_id` |

PostgreSQL state changes use a transactional outbox. Consumers with database effects use an inbox/idempotency record in the effect transaction. Redpanda transactions do not make PostgreSQL, ClickHouse, brokers, email, or object storage part of one transaction.

### 11.4 PostgreSQL

Separate identity, product, and trading PostgreSQL ownership boundaries provide access and failure isolation. The OMS and ledger may use explicitly owned schemas in the trading cluster and shared transactions only for documented financial invariants.

Clusters require encryption, private networking, TLS, bounded pooling, query deadlines, backups, point-in-time recovery, tested restore, and cross-region disaster-recovery policy.

### 11.5 ClickHouse

ClickHouse stores rebuildable analytical projections: selected market history, bars, order/execution analytics, P&L series, trade groupings, alerts, and operational aggregates. One Rust sink consumes durable events and writes idempotent batches with event ID and projection version. Services do not directly write arbitrary analytical tables.

Reference: [ClickHouse Rust integration](https://clickhouse.com/docs/integrations/rust).

### 11.6 S3 and Parquet

S3 stores raw feed captures, normalized archives, bars, broker evidence, immutable business-event archives, reports, backtest inputs/results, artifacts, and audit exports. Objects include checksum, schema, provenance, capture version, and encryption metadata. Object Lock is used where retention policy requires it.

### 11.7 Redis

Redis may hold rate limits, secondary session cache, short-lived authorization cache, presence, and stream-resume metadata. It is never the only record of a user, order, fill, balance, entitlement, workspace, or session revocation history.

---

## 12. Analytics, strategies, alerts, and product workflows

Analytics consumes ledger and market events and creates rebuildable versioned projections. A "trade" is derived by a declared grouping policy; it is not assumed to equal one fill. Reprocessing creates a new projection version and does not rewrite the ledger.

Built-in indicators are incremental Rust components. Untrusted custom indicators and strategies run as capability-restricted WebAssembly components with bounded data windows and no ambient file, network, or credential access. Calculation permission and live execution permission are separate.

Backtests record data version, corporate-action policy, fees, slippage, calendar, strategy artifact, and random seed.

Alerts run server-side against canonical streams with deduplication, cooldown, delivery state, and audit history. A desktop-only alert is a convenience, not the reliable source.

---

## 13. Provider and licensing architecture

Providers are adapters, not architecture. A provider-specific type cannot become an order, instrument, market, chart, or portfolio domain type.

Broker breadth may begin with an aggregation provider, while direct adapters are built for high-value volume, richer semantics, and professional execution. FIX, certified broker/FCM gateways, redundant counterparties, and venue-adjacent deployments use the same OMS contract.

Market sources may include Databento for serious US market coverage, dxFeed for broader enterprise coverage, and direct venue streams where licensed. News, economic, earnings, dividend, and corporate-action datasets require separately licensed providers.

Data agreements distinguish display, non-display, derived, delayed, real-time, professional, non-professional, internal distribution, and redistribution. Entitlements are enforced before streaming, export, replay, API response, alert evaluation, or algorithmic use.

Broker data is not assumed redistributable. Commercial rights and jurisdiction obligations are launch gates, not implementation details. Reference: [IBKR API market-data restrictions](https://www.interactivebrokers.com/en/index.php?f=1538&p=api).

---

## 14. Infrastructure and acceleration profiles

### 14.1 General topology

- EKS runs control, edge, data workers, and ordinary services.
- Streaming gateways use network-optimized pools or dedicated hosts based on measured connection load.
- Feed handlers and professional execution cells use dedicated EC2, bare-metal, or colocation hosts.
- Redpanda, PostgreSQL, Redis, S3, ClickHouse, KMS, Secrets Manager, and telemetry use private connectivity where supported.
- Kubernetes is not placed in the innermost latency loop merely for consistency.

### 14.2 Linux dedicated-host profiles

A dedicated latency host defines and records:

- isolated CPU set and NUMA placement;
- NIC queue/RSS steering and interrupt placement;
- CPU governor, C-state, turbo, and thermal policy;
- huge-page policy when required;
- memory locking and page-fault policy;
- socket, AF_XDP, or DPDK mode;
- driver and firmware versions;
- PTP/hardware timestamp capability;
- bounded poll/sleep strategy;
- watchdog and failover behavior.

DPDK or AF_XDP is never enabled in a mixed-tenant general-service node.

### 14.3 Clock synchronization

Dedicated feed and execution hosts use PTP-capable infrastructure where available, with hardware timestamping and measured synchronization error. Linux exposes PTP hardware clocks and standardized timestamp integration: [Linux PTP clock documentation](https://docs.kernel.org/driver-api/ptp.html).

Cross-host latency is not calculated by subtracting unsynchronized wall clocks. Reports include clock source, synchronization method, maximum observed offset, and uncertainty.

### 14.4 Multi-region ownership

Each trading account has one home execution region and one active execution owner. Reads and product APIs may be globally active; order and ledger writes are routed to the owner. Disaster recovery promotes ownership through a fenced epoch and requires provider reconnection plus reconciliation before unrestricted trading resumes.

### 14.5 Infrastructure as code

Terraform and reviewed versioned configuration define networking, IAM, KMS, databases, Redpanda, ClickHouse connectivity, host profiles, DNS, and disaster recovery. Production profile changes require a plan, policy checks, benchmark comparison, rollback, and audit evidence.

---

## 15. Concurrency, memory, and hot-path mechanics

### 15.1 Partition execution model

A hot partition is owned by one task or thread. Input arrives through a bounded queue; mutation occurs without a shared global mutex; output is published as immutable generations or bounded messages.

Cross-partition work is asynchronous unless a documented financial invariant requires a transaction. Repartitioning is an explicit drain, fence, snapshot, epoch increment, and resume procedure.

### 15.2 Queue policy

Every queue declares:

- producer and consumer;
- item and byte capacity;
- semantic class;
- overflow action;
- maximum residence-time SLO;
- recovery mechanism;
- metrics and alert threshold.

Allowed overflow actions are semantic: conflate replaceable state, request snapshot after ordered loss, reject before acceptance, shed optional work, or disconnect a slow consumer. "Grow the queue" is not a production overload policy.

### 15.3 Allocation and layout

- Steady-state hot loops target zero heap allocation after warm-up where measured.
- Reusable slabs/pools are bounded and ownership is explicit.
- Compact IDs replace repeated strings in hot structs.
- Cache-line contention and false sharing are measured.
- Structure-of-arrays layouts are considered for scanning workloads; array-of-structures remains valid for event-oriented paths.
- NUMA-local ownership is maintained on dedicated hosts.
- Payloads are decoded once and transformed only when contract boundaries require it.

### 15.4 Scheduling

Tokio is used for portable network orchestration and ordinary asynchronous work. Dedicated ordered partitions may use pinned threads when executor jitter violates the profile. CPU-heavy analytics use separate pools. Blocking file/database work never runs on async network workers.

Busy polling is profile-specific, bounded, observable, and disabled in balanced desktop mode.

### 15.5 Observability overhead

Hot loops use preallocated histograms, per-partition counters, and sampled traces. A trace span or formatted log is not created for every market tick. Financial and security audit records remain complete through their durable path.

---

## 16. Performance model and release gates

### 16.1 Latency vocabulary

- **Feed receive:** first timestamp at NIC or socket boundary under the declared clock.
- **Canonical event:** validated provider-neutral event accepted by the active partition owner.
- **Fanout enqueue:** event/latest generation accepted by the direct fanout branch.
- **Gateway send:** final application payload handed to the transport.
- **Client receive:** complete frame/datagram available to the client decoder.
- **Model apply:** client partition generation containing the event becomes readable.
- **Frame submit:** Origin frame has been submitted through GPUI.
- **Presented pixel:** the relevant physical display update is externally or platform-instrumented as visible.
- **Durable order acceptance:** authoritative intent transaction committed.
- **Provider dispatch:** request handed to the provider transport after durable acceptance.

### 16.2 Required timestamp chain

```text
exchange_timestamp
provider_receive_timestamp
nic_receive_timestamp
axiusflow_receive_timestamp
normalized_timestamp
fanout_enqueue_timestamp
gateway_send_timestamp
client_receive_timestamp
model_apply_timestamp
frame_submit_timestamp
present_timestamp_if_measurable
```

Order measurements additionally include command creation, regional ingress, authorization evidence validation, risk completion, durable commit, provider dispatch, provider acknowledgement, normalized execution, and client presentation.

### 16.3 Challenge targets

These are initial architecture challenge targets, not production claims. Missing workload definitions invalidate the result.

| Boundary | `portable_socket` p99 | dedicated/tuned p99 | accelerated Linux p99 |
|---|---:|---:|---:|
| feed receive → canonical event | ≤ 250 µs | ≤ 100 µs | ≤ 50 µs |
| canonical event → direct fanout enqueue | ≤ 100 µs | ≤ 75 µs | ≤ 50 µs |
| canonical event → regional gateway send | ≤ 1 ms | ≤ 500 µs | ≤ 250 µs |
| client receive → validated model apply | ≤ 500 µs | ≤ 300 µs | profile-specific |
| model apply → GPUI frame submit | ≤ 2 ms | ≤ 2 ms | same client gate |
| client receive → GPUI frame submit | ≤ 3 ms | ≤ 2.5 ms | same client gate |
| input event → GPUI frame submit | ≤ 3 ms | ≤ 2.5 ms | same client gate |

Desktop presentation gates:

- 120 Hz is the minimum professional interaction benchmark where hardware supports it.
- 144 Hz is the flagship benchmark target.
- Client receive to presented pixel targets one available refresh interval at p99 and two at p99.9 on the named display profile.
- A 60 Hz fallback remains supported but is not the flagship performance claim.
- Multi-chart, order-book, scanner, and active-order workloads are tested together, not only as isolated charts.

Execution challenge targets:

| Boundary | Regional cell p99 | Dedicated professional cell p99 |
|---|---:|---:|
| execution ingress → completed risk decision | ≤ 1 ms | ≤ 500 µs |
| execution ingress → durable order acceptance | ≤ 5 ms | ≤ 2 ms |
| durable acceptance → provider dispatch | ≤ 1 ms | ≤ 500 µs |

These order targets exclude client WAN and provider response but include all required internal correctness work. If PostgreSQL durability cannot satisfy a target on the declared topology, the target fails; correctness is not weakened to make the number pass.

### 16.4 WAN and physical limits

No universal quote-to-pixel or click-to-venue number is published across arbitrary internet paths. End-to-end reports separate:

```text
server processing + network transit + client processing + display scheduling
```

Regional reports name client location, gateway region, route, RTT, loss, and display refresh. Venue reports name colocation/provider boundaries and exclude nothing without labeling it.

### 16.5 Tail and overload gates

A release gate includes p99.9 and maximum behavior under:

- contracted sustained feed load;
- defined microbursts;
- hot-symbol concentration;
- subscription churn;
- slow clients;
- packet loss and reordering;
- snapshot storms;
- one partition-owner failover;
- durable-tap slowdown;
- telemetry enabled at production settings.

There is no pass if an authoritative event is silently lost, a queue becomes unbounded, a stale writer publishes accepted data, or recovery requires an unexplained state guess.

### 16.6 Benchmark profiles

Every result records OS/kernel, CPU and NUMA topology, power policy, memory, NIC/firmware/driver, queue setup, GPU/driver, display resolution/refresh/DPI, transport, RTT/loss, build flags, feed dataset, burst distribution, instrument distribution, subscriptions, charts, sample count, and percentile method.

The portable baseline is gated separately on Windows, Linux, and macOS. Linux acceleration is an additional gate, never a substitute for the portable matrix.

---

## 17. Observability and operations

Operational telemetry includes:

- every defined latency boundary and queue residence histogram;
- active network/acceleration mode and fallback reason;
- ownership epoch, handoff duration, and stale-writer rejection;
- market gaps, quality flags, conflation, resnapshot, downgrade, and disconnect rates;
- Redpanda produce latency, consumer lag, partition skew, and hot keys;
- raw-capture continuity and manifest gaps;
- outbox age, retry count, and oldest unpublished record;
- database pool saturation, commit latency, and slow queries;
- provider round-trip, unknown outcomes, invalid OMS transitions, and reconciliation breaks;
- entitlement denials and revocation propagation;
- client decode, model, Origin build, GPUI submission, and presentation timing;
- dropped frames, frame pacing, GPU pressure, memory, allocation, copy, and thermal mode.

Dashboards are separated for control, real-time data, execution, durable data, client performance, security, and provider health. A production deployable requires ownership, SLOs, alerts, runbooks, rollback, recovery, and capacity limits.

---

## 18. Security, resilience, and supply chain

- Private networking and default-deny policy protect databases, durable infrastructure, and internal RPC.
- Workloads use unique identities and least-privilege Redpanda/database/cloud permissions.
- Broker credentials use envelope encryption and never enter logs, traces, analytics, crash reports, or clients.
- Desktop installers and updates are signed; macOS is notarized where required; update manifests are independently signed and anti-downgrade protected.
- Dependencies are exact-pinned with reviewed lockfiles, SBOMs, provenance, vulnerability review, and license policy.
- Protocol decoders, state transitions, accelerated adapters, and untrusted payloads are fuzzed.
- Redpanda, PostgreSQL, Redis, identity, provider, ClickHouse, S3, partition-owner, and region-failure exercises are required.
- Analytics, reports, or notifications cannot block execution.
- Missing risk or authoritative persistence fails live trading safely.

Disaster recovery restores authoritative state and explains every in-flight order. It does not claim zero RPO/RTO without measured proof.

---

## 19. Testing and certification

### 19.1 Semantic equivalence

The same versioned capture is processed through portable and accelerated ingest adapters. Canonical event bytes/values, sequence decisions, quality flags, snapshots, and errors must match. Performance cannot excuse semantic divergence.

### 19.2 Cross-platform client matrix

Windows, Linux, and macOS run the same:

- protocol fixtures and malformed-input corpus;
- reconnect, network-change, suspend/resume, and snapshot recovery scenarios;
- Origin geometry, scale, input, and screenshot tests where deterministic;
- multi-monitor/DPI and display-refresh tests;
- secure vault and authentication callback tests;
- tick-to-frame and input-to-frame workloads;
- long-running memory, allocation, and queue-bound checks.

### 19.3 Market-data certification

- packet/message capture replay;
- duplicate, reorder, correction, wrap, and gap injection;
- burst and hot-key distributions;
- partition handoff and stale-owner injection;
- direct-fanout versus durable-replay equivalence;
- slow-client and snapshot-storm behavior;
- entitlement leakage checks;
- AF_XDP copy/zero-copy capability verification;
- DPDK NIC/queue isolation and failover.

### 19.4 Trading certification

- model-based OMS tests;
- fixed-point and ledger properties;
- broker-adapter conformance;
- paper and shadow order paths;
- duplicate intent and callback handling;
- timeout/unknown-outcome recovery;
- crash between durable commit and provider dispatch;
- kill-switch and stale-risk-snapshot drills;
- daily reconciliation before broader live access.

### 19.5 Physical performance evidence

Renderer submission timing is necessary but not sufficient. Tick-to-pixel and input-to-pixel certification includes full host preparation and presentation. Where platform presentation timestamps are insufficient, an external high-speed camera or photodiode-style harness is used for the flagship claim.

---

## 20. Delivery sequence from the current foundation

The repository currently contains foundational domains, strict security/authorization primitives, Protobuf contracts, bounded replay, one GPUI source, and an Origin GPUI bridge. It does not yet contain a live feed, Redpanda path, production transport, OMS/risk/ledger runtime, provider connection, or infrastructure deployment.

### Stage 1: Cross-platform and accelerated ingest foundation

Deliver in parallel:

- explicit `platform_runtime`, `transport`, `realtime`, and `ingest_driver` ports;
- separate `portable_socket`, `tuned_linux_socket`, `linux_af_xdp`, and `linux_dpdk` adapter crates and build targets;
- a bounded borrowed receive-batch contract with explicit buffer lifetime, release, timestamp-source, overflow, and capability semantics;
- exact-pinned accelerated dependencies plus Stage 1 safety, license, provenance, fuzzing, and maintenance review;
- Windows, Linux, and macOS build/smoke matrix for the pinned GPUI source and portable path;
- Linux compile/conformance lanes using software-accessible AF_XDP modes and DPDK software or virtual devices where runner capabilities permit;
- a machine-readable, runtime-enforced readiness manifest that prevents activation or claims above each profile's proven readiness state;
- deterministic Ethernet/IP/UDP/TCP/provider fixtures proving identical canonical events, gaps, errors, and replay across applicable profiles;
- canonical timestamp vocabulary and latency recorder, including hardware timestamp capability;
- fenced single-writer partition contract;
- direct-fanout and durable-tap interfaces using deterministic replay;
- full replay-to-GPUI frame benchmark, including model and host work;
- bounded client semantic classes and snapshot recovery.

Exit criteria: the same applicable packet/replay corpus produces equivalent canonical and Origin state across profiles and operating systems; no domain, decoder, partition, or fanout contract assumes socket-owned memory; every queue and timestamp boundary is visible; AF_XDP and DPDK reach at least `fixture_validated` with unavailable hardware/provider evidence recorded honestly; the portable path is eligible to advance independently toward `production_enabled`; and Redpanda is not required to demonstrate the direct path.

### Stage 2: Connected read-only market data

Deliver:

- first legally authorized live provider adapters using the best currently affordable managed, sandbox, delayed, or real-time source available under its terms;
- production ingest through `portable_socket` and, on dedicated Linux hosts, `tuned_linux_socket`;
- AF_XDP and DPDK provider integration when a compatible authorized packet feed is available, without redesigning their Stage 1 contracts;
- explicit readiness and `unavailable` capability results for incompatible or unqualified profile/feed combinations, never pretend acceleration or silent fallback;
- direct latest-state fanout;
- Redpanda durable branch and raw S3 capture;
- binary WebSocket baseline;
- QUIC prototype behind negotiation;
- ClickHouse projections and deterministic bars;
- entitlement enforcement and resnapshot behavior.

Exit criteria: sustained and burst workloads pass latency, loss, replay, slow-client, and semantic-equivalence gates for every profile declared `production_enabled`; the portable/tuned software and supported product features are production-complete within the current provider's declared rights, coverage, freshness, and availability without requiring a premium direct feed; delayed or sandbox data is never represented as suitable live-trading evidence; accelerated adapters remain implemented and fixture-validated but unclaimed until compatible hardware and provider evidence exists; every displayed event retains provenance.

### Stage 3: Durable controlled execution

Deliver:

- execution cell;
- immutable policy/risk snapshots;
- OMS, risk, broker adapter, durable intent, outbox, ledger input, reconciliation, and kill switches;
- paper, shadow, restricted canary, then limited live trading.

Exit criteria: every crash, timeout, duplicate callback, and unknown-outcome scenario converges to an explainable reconciled state while meeting the regional execution budget.

### Stage 4: Professional terminal and transport

Deliver:

- multi-chart and order-book workspaces;
- scanners, advanced alerts, drawings, indicators, news, external calendars, and advanced analytics;
- 120/144 Hz workload certification;
- certified QUIC reliable-stream profile;
- optional datagrams for approved semantic classes;
- professional direct broker/FIX routes.

### Stage 5: Venue-adjacent scale and global expansion

The acceleration contracts and adapters already exist from Stage 1. This stage expands qualified production coverage rather than introducing kernel-bypass architecture for the first time.

Deliver:

- qualification evidence that promotes `linux_af_xdp` and `linux_dpdk` from `fixture_validated` through `hardware_validated`, `provider_certified`, and `production_enabled` without changing canonical contracts;
- additional NIC, driver, firmware, and provider certification matrices for AF_XDP and DPDK;
- multi-queue and multi-port scaling, hot-spare NICs, and fenced failover;
- PTP/hardware timestamp deployment and continuous clock-quality alarms;
- venue-adjacent execution and additional direct feeds;
- multi-provider arbitration and global home-region expansion.

Expansion is promoted only when it materially improves p99/p99.9 without semantic, security, portability, or recovery regression.

---

## 21. Explicit rejected designs

1. Linux-only desktop functionality.
2. Calling `io_uring` kernel bypass.
3. Claiming AF_XDP zero-copy without verified driver support.
4. Installing DPDK into ordinary consumer desktops or mixed general-service nodes.
5. A serial `normalizer → Redpanda acknowledgement → client fanout` display path.
6. Using Redpanda as request/reply, synchronous risk, or a database transaction replacement.
7. Using QUIC datagrams for orders, fills, ledger entries, snapshots, or the sole copy of ordered deltas.
8. Using QUIC zero-RTT for non-replay-safe trading or security commands.
9. Unbounded server, client, chart, or telemetry queues.
10. A second financial chart engine or GPUI-owned duplicate chart model.
11. Embedding the browser chart in a GPUI webview.
12. GPUI, provider, cloud, or OS types in domain/application crates.
13. Floating-point authoritative money, price, fee, quantity, or journal values.
14. Redis or ClickHouse as authoritative financial state.
15. Provider callbacks directly mutating order rows.
16. Send-before-durable order routing to win a benchmark.
17. Naive global active-active order or ledger writes.
18. A process per bounded context from the first deployment.
19. Custom unsafe hot-path code without measured need, audited invariants, fuzzing, and approval.
20. Marketing component microbenchmarks as tick-to-pixel or click-to-venue results.
21. Silent transport or acceleration fallback in a claimed benchmark profile.
22. Claiming data rights, availability, or low-latency provider behavior absent from contracts and evidence.
23. Deferring AF_XDP/DPDK buffer ownership, timestamping, and backpressure contracts until after a socket-specific data plane has hardened.
24. Blocking a production-complete portable/tuned release on unavailable paid direct feeds, colocation, or acceleration hardware—or claiming synthetic evidence as their substitute.

---

## 22. Governing architecture decisions

| Area | Decision |
|---|---|
| Primary implementation | Rust, with isolated official Better Auth TypeScript exception |
| Native desktop operating systems | Windows, Linux, and macOS |
| Portable client baseline | Evented Rust networking plus TLS binary WebSocket fallback |
| Preferred optional client transport | Negotiated QUIC/WebTransport after certification |
| Linux acceleration | Tuned sockets, AF_XDP, and DPDK are parallel Stage 1 implementations with explicit readiness states; portable product release is independent, and accelerated production activation remains feed- and hardware-qualified |
| Naming | `snake_case` for Axiusflow-owned identifiers, with documented language/external exceptions |
| Native UI | One exact-pinned GPUI source plus GPUI Component |
| Financial charting | Origin Charts only; shared engine and `ChartFrame` |
| Domain dependency direction | Domain/application remain GPUI-, provider-, transport-, cloud-, and OS-free |
| Operational planes | Control, real-time data, execution, and durable data |
| Market display path | Direct bounded fanout parallel to durable publication |
| Runtime ordering | Fenced single writer per ordered partition |
| Initial deployment model | Six cohesive deployables; extract by evidence |
| Durable event backbone | Redpanda, never RPC or inline display barrier |
| Authoritative transactions | PostgreSQL with outbox/inbox |
| Analytics | ClickHouse rebuildable projections |
| Historical/raw data | S3 and Parquet |
| Ephemeral cache | Redis, never authoritative |
| Client delivery semantics | Reliable snapshots/events plus optional loss-tolerant datagrams by semantic class |
| Execution | Durable intent before provider dispatch; home-region execution cell |
| Financial numerics | Checked fixed-point/integer domain types |
| Strategy isolation | Capability-restricted WebAssembly |
| Performance evidence | Hardware/profile-scoped p99, p99.9, max, overload, and recovery gates |
| First-party core safety | `unsafe_code = "forbid"`; accelerated dependency boundaries require separate review |

---

## 23. Definition of architectural success

The architecture is functioning as intended when:

1. Windows, Linux, and macOS pass the same native correctness, security, reconnect, and chart-semantic suite.
2. Portable sockets, tuned Linux sockets, AF_XDP, and DPDK share foundational contracts and produce equivalent canonical events under applicable fixtures; the portable/tuned product can reach production independently, while an accelerated profile cannot advance beyond its proven readiness or make hardware/provider claims without evidence.
3. A normalized event reaches direct fanout without waiting for Redpanda while the durable branch remains observable and replayable.
4. Every ordered partition has one fenced writer and stale epochs are rejected.
5. Slow clients and durable/analytical outages cannot create unbounded memory or corrupt execution.
6. Every accepted order has durable intent, deterministic state history, raw provider evidence, and reconciliation result.
7. Every portfolio metric traces to immutable ledger inputs and a versioned policy.
8. Every displayed market value retains source, sequence, quality, entitlement, and correction provenance.
9. Origin owns chart state once and the same frame contract renders through supported backends.
10. Provider adapters can be replaced without changing order, portfolio, chart, or analytics domains.
11. Performance claims include full named boundaries, hardware, network, OS, load, p99.9, overload, and recovery evidence.
12. Tick-to-pixel evidence includes model, GPUI host work, GPU submission, and physical presentation—not renderer submission alone.
13. A region, process, partition owner, provider, database, or durable consumer failure converges through documented recovery rather than guessing.
14. Market-data entitlements are enforced at stream, export, replay, API, alert, and algorithmic boundaries.
15. The initial platform remains operationally understandable; extraction follows evidence instead of fashion.

---

## 24. Research, source, and licensing note

This revision was checked against official or primary material for [Linux AF_XDP](https://docs.kernel.org/next/networking/af_xdp.html), [Linux PTP clocks](https://docs.kernel.org/driver-api/ptp.html), [DPDK Linux deployment](https://doc.dpdk.org/guides/linux_gsg/index.html), [Windows IOCP](https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports), [Windows RIO](https://learn.microsoft.com/en-us/windows/win32/winsock/riorqueue), [Apple Network.framework](https://developer.apple.com/videos/play/wwdc2018/715/), [QUIC](https://www.rfc-editor.org/info/rfc9000/), [QUIC DATAGRAM](https://www.rfc-editor.org/info/rfc9221/), and [GPUI cross-platform product availability](https://zed.dev/blog/gpui-2-on-preview).

External capabilities are architecture inputs, not evidence that Axiusflow has implemented or benchmarked them. Exact dependency/library selection requires compatibility, security, license, maintenance, and performance evaluation at implementation time. Commercial availability, market-data rights, broker behavior, and regulatory responsibility require direct agreements and legal review.

Content was rephrased for compliance with licensing restrictions.
