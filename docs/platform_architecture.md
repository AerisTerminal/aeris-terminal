# Axiusflow Target Architecture

**Status:** Governing target-state architecture
**Audience:** Product engineering, platform engineering, security, compliance, data engineering, and technical leadership
**Primary language:** Rust
**Last updated:** 2026-07-31

## 1. Purpose

Axiusflow is a high-performance trading and financial platform. It combines a professional native trading terminal, broker-connected execution, portfolio and journal analytics, market-data visualization, alerts, research, and ordinary product functionality such as identity, subscriptions, workspaces, notifications, administration, and support.

This document defines the architecture to build from the beginning. It is not a temporary MVP architecture and it does not depend on replacing NATS with Redpanda, replacing a dashboard chart with Origin Charts, or moving from a managed identity vendor later. The permanent structural decisions are made here:

1. Rust owns product logic, financial logic, backend services, streaming clients, analytics workers, and the native UI.
2. Better Auth is self-hosted and is the authentication authority.
3. Origin Charts is the sole financial chart engine and receives a native GPUI renderer adapter.
4. Redpanda is the durable event-streaming backbone from the first production environment.
5. PostgreSQL is the authoritative transactional store for business and financial state.
6. ClickHouse is present from the beginning for analytical workloads.
7. S3 and Parquet are the long-term raw and normalized data archive.
8. Direct synchronous calls and Redpanda events have separate roles; the event bus is not misused as RPC.
9. Trading, market data, and general product workloads are isolated according to correctness and latency requirements.
10. Provider-specific objects never become the internal domain model.

The implementation may be delivered in vertical slices, but each slice must fit this final architecture. Delivery sequence is not permission to create a disposable design.

---

## 2. Non-negotiable design principles

### 2.1 Correctness before superficial speed

A fast incorrect trading system is unusable. The order path must be deterministic, idempotent, recoverable, and reconcilable. Money, quantity, price, and fee calculations use fixed-point or integer representations, not floating point. Every external side effect has a durable intent, a provider correlation identifier, and a recovery strategy.

### 2.2 Performance by isolation and bounded work

Performance comes from predictable work, data locality, backpressure, and isolation—not from placing every component in one process. Hot-path services use bounded queues, stable partitioning, preallocated buffers where profiling justifies them, and no synchronous analytical writes. General product workloads cannot interfere with execution or market-data processing.

### 2.3 One owner for every fact

Every category of state has exactly one authoritative owner:

- Better Auth owns credentials and authentication sessions.
- The authorization service owns application roles, permissions, and data entitlements.
- The order management service owns normalized order state.
- The portfolio ledger owns fills, cash movements, lots, positions, and financial journals.
- The instrument service owns internal instrument identities and symbology mappings.
- The workspace service owns layouts, watchlists, and saved user configuration.
- ClickHouse owns no authoritative financial state; it stores projections.
- Redpanda distributes and retains events but does not replace the transactional consistency boundary of PostgreSQL.

### 2.4 Stable internal contracts

Broker, exchange, vendor, GPUI, Better Auth, and cloud SDK types stop at adapter boundaries. Internal commands and events use versioned Axiusflow contracts. This is what allows providers to be added or replaced without rewriting the product.

### 2.5 Replay as an operational capability

Raw market messages, normalized market events, provider order messages, and business events must be replayable. Analytics projections, search indexes, dashboards, and alerts must be reconstructable from authoritative records and versioned events.

### 2.6 Security belongs in normal control flow

Authentication, authorization, step-up verification, market-data entitlements, risk limits, and audit records are not wrappers added at launch. They are explicit stages in request processing and event publication.

### 2.7 Project-wide snake_case

Axiusflow follows `snake_case` as the governing naming convention. All platform-owned names use lowercase words separated by underscores:

- repositories, directories, and files;
- Rust crates, modules, features, functions, variables, fields, and generated accessors;
- service and deployment identifiers;
- API path segments, parameters, request/response fields, and serialized keys;
- Protobuf fields, event names, topic segments, consumer groups, and schema subjects;
- database schemas, tables, columns, indexes, constraints, and migration names;
- configuration keys, secret identifiers, object-store prefixes, metrics, and trace attributes;
- public SDK functions and user-facing programmable APIs.

Narrow syntax exceptions are explicit:

- Rust type, trait, and enum declarations use language-standard `UpperCamelCase` where required by Rust tooling, while their modules, fields, methods, and serialized/wire names remain snake_case.
- constants and environment variables use `SCREAMING_SNAKE_CASE` where required by platform convention.
- official third-party product, protocol, and provider names are not rewritten.
- legacy CSS custom-property keys remain byte-for-byte compatible for visual parity; their Rust accessors are snake_case.

No Axiusflow-owned identifier may introduce kebab-case, camelCase, or PascalCase as a public or serialized naming style. Code generation must configure snake_case wire names explicitly.

---

## 3. Language policy and the Better Auth exception

The platform is Rust-first, but the official Better Auth project is a TypeScript authentication framework, not a Rust framework. Its official documentation describes it as a TypeScript authentication and authorization framework. Better Auth provides self-hosting, PostgreSQL-backed users and sessions, passkeys, two-factor authentication, OAuth/OIDC integrations, JWT/JWKS support, and extensible plugins.

The production architecture therefore has one deliberate language exception:

- `services/auth_service` uses official Better Auth with a minimal, pinned TypeScript configuration.
- No trading, portfolio, market-data, entitlement, billing, or product business logic is implemented in that service.
- All other first-party services and clients are Rust.
- The auth service is isolated behind a versioned HTTP contract and JWKS, so TypeScript types never leak into Rust domains.

A third-party `better-auth-rs` project exists, but its Better Auth compatibility rewrite is currently described as alpha. Authentication is too critical to base on an unofficial alpha compatibility implementation. It may be evaluated later through a security review and conformance suite, but it is not the production trust anchor defined here.

If absolute zero hand-written TypeScript becomes a harder requirement than using official Better Auth, those requirements conflict and a separate architecture decision must replace Better Auth with an audited Rust-native identity implementation. This document does not hide that trade-off.

References:

- [Better Auth introduction](https://www.better-auth.com/docs/introduction)
- [Better Auth database model](https://www.better-auth.com/docs/concepts/database)
- [Better Auth session management](https://www.better-auth.com/docs/concepts/session-management)
- [Better Auth JWT and JWKS plugin](https://www.better-auth.com/docs/plugins/jwt)

---

## 4. System context

```text
                           External systems
  ┌──────────────────────────────────────────────────────────────────┐
  │ Brokers     Market-data vendors     Exchanges     Email/SMS      │
  │ SnapTrade   Databento/dxFeed        Crypto        Billing        │
  │ Direct APIs Licensed calendars      venues        KYC partners   │
  └──────┬────────────┬────────────────────┬───────────────┬──────────┘
         │            │                    │               │
         ▼            ▼                    ▼               ▼
┌──────────────────────────── Axiusflow cloud ────────────────────────────┐
│                                                                         │
│ Cloudflare edge                                                         │
│     │                                                                   │
│     ▼                                                                   │
│ Edge gateway ───── Better Auth ───── Authentication PostgreSQL          │
│     │                                                                   │
│     ├──────── Control/product services ───── Product PostgreSQL         │
│     │                                                                   │
│     ├──────── Trading services ───────────── Trading PostgreSQL         │
│     │                                                                   │
│     └──────── Streaming gateway                                         │
│                      ▲                                                  │
│                      │                                                  │
│ Market gateways ── Redpanda ── analytics/alerts/reconciliation          │
│                      │                │                                 │
│                      ▼                ▼                                 │
│                  S3/Parquet       ClickHouse                            │
│                                                                         │
└───────────────────────────┬─────────────────────────────────────────────┘
                            │
             HTTPS / HTTP2 / binary WebSocket
                            │
          ┌─────────────────┴─────────────────┐
          ▼                                   ▼
 Native GPUI terminal                  Browser companion
 GPUI Component shell                  Rust/WASM application
 Origin GPUI adapter                   Origin WebGPU/Canvas2D
```

The cloud and client runtimes share domain schemas and behavior, not databases or mutable memory.

---

## 5. Target repository structure

```text
axiusflow_rust/
├── apps/
│   ├── desktop/                   # GPUI native terminal
│   ├── web/                       # Rust/WASM browser companion
│   └── admin/                     # Rust-based internal operations client
│
├── crates/
│   ├── domain/
│   │   ├── identity/
│   │   ├── authorization/
│   │   ├── instruments/
│   │   ├── market_data/
│   │   ├── orders/
│   │   ├── execution/
│   │   ├── risk/
│   │   ├── portfolio/
│   │   ├── ledger/
│   │   ├── analytics/
│   │   ├── entitlements/
│   │   └── audit/
│   ├── application/              # use cases; no transport/provider details
│   ├── protocols/                # generated Protobuf and public API models
│   ├── persistence/              # shared SQLx primitives, outbox/inbox
│   ├── streaming/                # Redpanda client, schema envelope, tracing
│   ├── observability/
│   ├── security/
│   ├── testing/                  # deterministic clocks, fixtures, replay tools
│   └── ui/
│       ├── terminal_ui/          # GPUI compatibility boundary
│       ├── design_system/
│       └── chart_integration/
│
├── origin_charts/                # Origin Charts workspace or pinned submodule
│   └── crates/
│       ├── origin_core/
│       ├── origin_engine/
│       ├── origin_render/
│       ├── origin_render_gpui/   # required native adapter
│       ├── origin_render_wgpu/
│       ├── origin_wasm/
│       └── origin_native/
│
├── services/
│   ├── auth_service/             # only official Better Auth/TypeScript service
│   ├── edge_gateway/
│   ├── authorization_service/
│   ├── user_service/
│   ├── workspace_service/
│   ├── instrument_service/
│   ├── market_data_gateway/
│   ├── market_data_normalizer/
│   ├── bar_service/
│   ├── streaming_gateway/
│   ├── broker_connection_service/
│   ├── order_gateway/
│   ├── risk_engine/
│   ├── order_management_service/
│   ├── portfolio_ledger_service/
│   ├── reconciliation_service/
│   ├── analytics_service/
│   ├── alert_engine/
│   ├── notification_service/
│   ├── report_service/
│   └── audit_service/
│
├── schemas/
│   ├── protobuf/
│   ├── redpanda/
│   └── public_api/
│
├── migrations/
│   ├── product/
│   ├── trading/
│   ├── identity/
│   └── clickhouse/
│
├── infra/
│   ├── terraform/
│   ├── kubernetes/
│   ├── redpanda/
│   ├── observability/
│   └── policies/
│
├── tools/
│   ├── replay/
│   ├── feed_capture/
│   ├── broker_certification/
│   └── load_test/
│
└── docs/
    ├── platform_architecture.md
    ├── platform_design_system.md
    ├── decisions/
    ├── runbooks/
    └── threat_models/
```

Logical service boundaries are defined immediately. A service may initially share a Kubernetes node pool with another service, but it does not share tables or bypass contracts. Latency-sensitive services receive dedicated deployment profiles from the beginning.

---

## 6. Client architecture

### 6.1 Native terminal

The native desktop terminal is the flagship product. It uses:

- GPUI for windows, rendering, entities, input, actions, and application lifecycle.
- GPUI Component for docking, virtualized tables, forms, tabs, menus, dialogs, themes, text, and layout primitives.
- Origin Charts for every financial chart and trading visualization.
- Tokio tasks for network, persistence, and background compute, bridged into GPUI through bounded application messages.
- OS keychain facilities for protected desktop credentials and refresh material.

GPUI is pre-1.0 and can introduce breaking changes. The repository must pin exact GPUI and GPUI Component revisions. Direct GPUI types are confined to `crates/ui/terminal_ui` and application view modules. Shared domain and application crates cannot depend on GPUI.

Reference: [GPUI README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md) and [GPUI Component](https://longbridge.github.io/gpui-component/).

### 6.2 Origin Charts is the only financial chart engine

There is no alternative chart engine in a menu and no webview-wrapped chart in the desktop application. Origin is the charting platform.

GPUI Component's basic visualization widgets may be used only for non-financial categorical displays when appropriate. Price charts, order-book charts, equity curves, P&L timelines, trading calendars with chart interactions, indicators, drawings, executions, and strategy overlays use Origin or Origin-derived primitives.

### 6.3 `origin_render_gpui`

Origin's platform-neutral `ChartEngine` and draw-list contract are preserved. The new adapter consumes the same `ChartFrame` as WebGPU, Canvas2D, screenshots, and native test renderers.

```text
Market snapshot / chart command
             │
             ▼
      origin_engine::ChartEngine
             │
             ▼
       origin_render::ChartFrame
             │
     ┌───────┴───────────┐
     ▼                   ▼
origin_render_gpui   origin_render_wgpu
native terminal      browser/WASM
```

The GPUI adapter contains:

1. `OriginChartView`: GPUI entity that owns the chart handle and user-facing state.
2. `OriginChartElement`: GPUI element responsible for layout, prepaint, clipping, and paint.
3. `GpuiDrawExecutor`: maps Origin primitives to GPUI paint operations.
4. `GpuiTextMetrics`: implements Origin's text measurement contract with GPUI's text system.
5. `GpuiInputAdapter`: normalizes pointer, wheel, touchpad, keyboard, focus, and accessibility actions.
6. `FrameScheduler`: translates Origin invalidation levels into GPUI repaint requests.
7. `ChartDataBridge`: drains bounded data commands from background tasks without blocking the UI thread.

Primitive mapping:

| Origin primitive | GPUI execution |
|---|---|
| `Rect`, `RectFrame`, `HLine`, `VLine` | pixel-aligned GPUI quads |
| `Polyline` | GPUI path/stroke or adapter tessellation |
| `AreaFill` | filled path with gradient |
| `Circle`, `RoundRect` | native GPUI geometry or path |
| `Text` | GPUI text layout and glyph paint |
| `ClipPush/ClipPop` | GPUI element clipping/scissor |

Rules:

- The adapter never owns a second price scale, time scale, crosshair, pane model, or series store.
- Chart input calls `ChartEngine`; GPUI only normalizes platform events.
- A single UI frame receives at most one merged update per chart.
- Crosshair invalidation does not rebuild static series geometry.
- Multi-chart workspaces share fonts, immutable market snapshots, and any backend-safe caches.
- No chart creates an independent GPU device merely for convenience.
- If a required GPUI primitive is unavailable, extend the adapter behind the draw-list interface; do not place chart state into GPUI.

Origin's architecture already calls for a headless engine and backend-neutral render contract: [Origin Charts architecture](https://github.com/TradeAion/Origin_charts/blob/main/docs/ARCHITECTURE.md).

### 6.4 Desktop state model

The UI process separates state into three categories:

- **Authoritative remote state:** orders, positions, balances, entitlements, and account data. The client displays versioned server snapshots and events.
- **Local durable preferences:** window placement, cache metadata, draft layouts, and device settings.
- **Ephemeral view state:** hover, selection, open dialogs, local filters, and in-progress edits.

Remote events are sequence-checked. A gap triggers a snapshot refresh rather than speculative continuation. The client never calculates authoritative buying power or order status.

### 6.5 Browser companion

The browser application uses Rust/WASM for product logic and Origin's existing WebGPU renderer with Canvas2D fallback. Minimal generated JavaScript needed to load WASM and call browser APIs is acceptable; business logic remains in Rust.

The browser and native terminal share:

- Protobuf schemas
- Domain value types
- Formatting rules
- Entitlement behavior
- Chart semantics
- Indicator formulas
- Workspace serialization

They do not have to share every presentation component. GPUI Component's browser support remains behind a capability gate until the complete browser and accessibility matrix passes.

---

## 7. Backend bounded contexts

### 7.1 Edge gateway

The Rust edge gateway is the only public entry point to internal services. It handles:

- TLS termination behind Cloudflare/AWS load balancing
- JWT verification against Better Auth JWKS
- session and device context
- request IDs, correlation IDs, and deadlines
- coarse and fine-grained rate limiting
- request size and protocol validation
- authorization calls or cached authorization decisions
- public REST and streaming protocol negotiation
- idempotency headers
- WebSocket/WebTransport lifecycle
- audit context propagation

It does not own users, orders, portfolios, or entitlements.

### 7.2 Authorization service

Authentication answers "who is this?" Authorization answers "what may this identity do?" They remain separate.

The authorization service owns:

- platform roles
- organization and team membership projections
- account access grants
- broker-account permissions
- trading permissions
- administrative capabilities
- market-data display/non-display entitlements
- professional/non-professional classification
- subscription features
- jurisdiction and product restrictions
- policy versions and evidence

Sensitive decisions are made server-side. Roles are not accepted from client input, and long-lived JWT claims are not the authority for rapidly changing entitlements.

### 7.3 User and workspace services

These services own normal product functionality:

- user profile linked to Better Auth `user_id`
- organizations and teams
- preferences, locale, timezone, and display currency
- workspaces and dock layouts
- watchlists and symbol lists
- saved chart layouts and drawing sets
- alert definitions
- subscriptions and plan metadata
- notification preferences
- export requests
- feature flags controlled by the platform

Workspace documents are versioned and use optimistic concurrency. Large exports and imports are object-store jobs rather than oversized API payloads.

### 7.4 Instrument service

The internal instrument ID is stable and provider-neutral. A ticker is not an identity.

An instrument record includes:

- internal `instrument_id`
- asset class
- listing and venue
- trading currency
- price and quantity precision
- tick and lot rules
- sessions and holidays
- lifecycle state
- corporate-action lineage
- aliases: vendor symbols, FIGI, ISIN, CUSIP where licensed, exchange identifiers, broker contract IDs

Every provider adapter maps through this service. Symbol changes do not create unrelated portfolio history.

### 7.5 Market-data gateways

Each vendor or exchange connection is isolated in a gateway process. The gateway performs only protocol-specific work:

- connect/authenticate
- decode native messages
- preserve raw sequence and timestamps
- detect transport gaps
- record feed health
- publish raw capture references and normalized candidates

A vendor gateway cannot directly construct client payloads or write portfolio data.

### 7.6 Market-data normalizer

The normalizer creates canonical events:

```text
market_event_header
- event_id
- instrument_id
- venue_id
- source_id
- source_sequence
- exchange_timestamp
- provider_receive_timestamp
- axiusflow_receive_timestamp
- publication_timestamp
- correction_flags
- quality_flags
- schema_version
```

Payloads include trade, quote, order-book delta, order-book snapshot, auction, status, halt, bar, reference-data update, and corporate action.

The pipeline preserves provenance. A consolidated display can be derived, but source-specific data is never silently relabeled as consolidated market data.

### 7.7 Bar and derived-market service

The bar service creates deterministic OHLCV and derived series for defined session calendars and timeframes. Bar definitions include source set, timezone/session policy, adjustment policy, and version. Late events and corrections create correction events rather than invisible mutations.

### 7.8 Streaming gateway

The streaming gateway translates internal event streams into client subscriptions:

- quotes and trades
- order-book snapshots and deltas
- bars
- account and order events
- portfolio updates
- alerts
- entitlement changes

It enforces data entitlements before publication. Clients receive a snapshot plus a sequence-numbered delta stream. Slow clients are conflated, downgraded, or disconnected according to subscription policy; they are never allowed to build unbounded server queues.

### 7.9 Broker connection service

This service manages external account links, OAuth/token state, broker capabilities, and connection health. Broker credentials are envelope-encrypted and are not exposed to other services. The service normalizes account identifiers and advertises provider capabilities.

Each adapter implements a versioned capability interface:

```text
broker_connector
- establish_connection
- refresh_connection
- revoke_connection
- list_accounts
- fetch_balances
- fetch_positions
- fetch_orders
- fetch_transactions
- submit_order
- replace_order
- cancel_order
- stream_order_events
- reconcile_snapshot
- capabilities
```

Unsupported behavior fails explicitly; the platform does not emulate an order type unless the semantics are exact and disclosed.

### 7.10 Order gateway

The order gateway accepts user intent and performs request-level checks:

1. authenticate identity
2. authorize broker account and operation
3. validate idempotency key
4. validate instrument and market status
5. normalize order intent
6. invoke pre-trade risk
7. submit to the order management service
8. return accepted/rejected status with the internal order ID

It does not call brokers directly.

### 7.11 Risk engine

The risk engine runs before external submission and continuously after acceptance. It consumes versioned account, position, price, and entitlement snapshots.

Checks include:

- buying power and available cash
- maximum order and position notional
- price collars and fat-finger limits
- stale or crossed market data
- duplicate intent
- order-rate limits
- restricted instruments
- session and venue state
- short-sale and options permissions where applicable
- account, strategy, user, and platform kill switches
- daily loss and exposure limits

If required state is missing or stale, risk fails closed for live trading.

### 7.12 Order management service

The OMS is the authoritative normalized order state machine. Example states include:

```text
received → validated → route_pending → submitted → acknowledged
                                      ↘ rejected
acknowledged → partially_filled → filled
acknowledged → cancel_pending → canceled
acknowledged → replace_pending → acknowledged
```

Provider events are inputs to the state machine, not direct database updates. Invalid or impossible transitions are quarantined and alert operations.

The external broker call cannot be made atomically with the local database. The design handles this explicitly:

- persist durable order intent and provider client order ID before submission
- use provider idempotency/client IDs where supported
- record the outbound request before or with dispatch state
- reconcile unknown outcomes after timeout or crash
- never translate a timeout directly into "rejected"
- preserve the raw provider response and normalized interpretation

### 7.13 Portfolio and ledger service

The portfolio ledger is the financial source of truth after normalized executions. It owns:

- fills and execution corrections
- cash journals
- fees, taxes, rebates, and commissions
- positions
- lots and cost basis methods
- realized and unrealized P&L inputs
- transfers and adjustments
- corporate-action effects
- base-currency conversion references
- daily broker snapshots
- reconciliation differences

Journal rows are immutable. Corrections append compensating or correcting entries. Monetary invariants are checked in the same PostgreSQL transaction.

### 7.14 Reconciliation service

Real-time events are not sufficient. Reconciliation independently compares:

- internal orders against broker orders
- internal fills against broker executions
- positions against broker positions
- cash and fees against statements/snapshots
- market-data sequences against expected feed sequences

Differences are classified, tracked, and resolved through explicit workflows. A successful WebSocket connection is not evidence that state is complete.

### 7.15 Analytics service

Analytics consumes ledger and market events and creates rebuildable projections in ClickHouse. It calculates:

- win rate and loss rate
- average win and average loss
- expectancy
- profit factor
- realized and unrealized P&L
- equity curves
- drawdown
- fees and slippage
- holding period
- long/short and asset-class performance
- symbol, account, broker, strategy, setup, and tag attribution
- day-of-week and time-of-day analysis
- R-multiple statistics
- calendar heatmaps
- daily, weekly, monthly, and yearly summaries

A "trade" is a derived concept, not simply one fill. Trade grouping policies—FIFO, average cost, flat-to-flat, strategy tag, or jurisdictional tax-lot policy—are versioned. Reprocessing with a new policy creates a new projection version and does not rewrite the ledger.

The user's trading calendar is generated internally from ledger-derived daily summaries. Economic, earnings, dividend, and corporate-action calendars are licensed external datasets and remain distinct.

### 7.16 Indicator, strategy, and backtest runtime

Built-in indicators are Rust crates with incremental update APIs. The dependency graph recomputes only affected outputs. Indicator results are normal Origin series and primitives.

Untrusted custom indicators or strategies run as WebAssembly components in a capability-restricted Wasmtime runtime. They receive explicit data windows and cannot access the network, file system, broker credentials, or arbitrary host functions. Live execution permissions are separate from calculation permissions.

Backtests are deterministic jobs with versioned data, corporate-action policy, fees, slippage model, calendar, strategy artifact, and random seed. Results are reproducible and stored with their complete input manifest.

### 7.17 Alerts and notifications

The alert engine evaluates server-side rules against canonical streams. Desktop-only alerts are optional conveniences, not the reliable source. Notification delivery supports in-app streams, email, push, and approved SMS channels. Every alert has deduplication, cooldown, delivery state, and audit history.

---

## 8. Redpanda event architecture

### 8.1 Why Redpanda is permanent

Redpanda is selected from the beginning for durable, partitioned event streams, Kafka API compatibility, integrated Schema Registry, transactions, replay, and tiered object storage. Its architecture provides partition-level ordering and Raft replication. This matches long-lived market-data, order-event, analytics, and audit pipelines better than introducing a smaller bus and migrating later.

References:

- [Redpanda architecture](https://docs.redpanda.com/current/get-started/architecture/)
- [Redpanda transactions](https://docs.redpanda.com/current/develop/transactions/)
- [Redpanda Schema Registry](https://docs.redpanda.com/current/manage/schema-reg/schema-reg-overview/)

Redpanda is the only general durable event backbone. NATS is not part of the target architecture.

### 8.2 What Redpanda is not

Redpanda is not used for:

- user-facing request/response RPC
- synchronous pre-trade risk calls
- direct database query replacement
- distributed locking
- ephemeral UI state
- pretending external database writes are exactly-once

Internal synchronous operations use gRPC/HTTP2 with deadlines. Redpanda carries durable facts and asynchronous work.

### 8.3 Cluster profile

Production uses Redpanda Dedicated/BYOC in the Axiusflow AWS network, distributed across three availability zones. The baseline policy is:

- replication factor 3
- minimum in-sync replicas 2
- `acks=all`
- producer idempotence enabled
- TLS in transit
- mTLS or SASL identity per workload
- topic ACLs by service account
- local NVMe for active segments
- S3 Tiered Storage for configured durable topics
- rack/AZ awareness
- no public broker endpoints
- independent development, staging, and production clusters

Market-data throughput topics and financial-event topics use separate quotas and, at sufficient volume, separate clusters. A feed burst must not delay order and ledger events.

### 8.4 Schema policy

All domain events use Protobuf registered in Redpanda Schema Registry.

Rules:

- backward-transitive compatibility by default
- field numbers are never reused
- fields are deprecated before removal
- semantic meaning cannot change under an existing field
- money and price are structured fixed-point values, never `double`
- timestamps include explicit clock meaning
- every event includes event ID, event time, publication time, producer, schema version, correlation ID, and causation ID
- payload envelopes remain small; large bodies live in S3 and are referenced by immutable URI and checksum

JSON is permitted only for external APIs, operational diagnostics, or truly schemaless user documents—not hot event paths.

### 8.5 Topic taxonomy

Topic names follow:

```text
<environment>.<domain>.<entity>.<event_family>.v<major>
```

Representative topics:

```text
prod.identity.user.lifecycle.v1
prod.authorization.entitlement.changed.v1
prod.instrument.reference.changed.v1
prod.market.trade.normalized.v1
prod.market.quote.normalized.v1
prod.market.book.delta.v1
prod.market.bar.v1
prod.market.feed.health.v1
prod.trading.order.lifecycle.v1
prod.trading.execution.lifecycle.v1
prod.trading.risk.decision.v1
prod.portfolio.ledger.entry.v1
prod.portfolio.position.changed.v1
prod.reconciliation.break.detected.v1
prod.analytics.trade.closed.v1
prod.alert.triggered.v1
prod.audit.security.event.v1
```

Do not create a topic per symbol or per user.

### 8.6 Partition keys

| Event family | Partition key | Ordering guarantee sought |
|---|---|---|
| Trades/quotes/bars | `instrument_id` | order per instrument/source stream |
| Order lifecycle | `broker_account_id` | account-level order sequencing |
| Execution lifecycle | `broker_account_id` | account-level fill sequencing |
| Ledger entries | `ledger_account_id` | ledger-account journal order |
| Positions | `portfolio_account_id` | portfolio mutation order |
| User/workspace | `user_id` or `workspace_id` | entity revision order |
| Entitlements | `principal_id` | policy-change order |
| Alerts | `alert_id` | evaluation/delivery order |

A partition key is a correctness decision. Partition-count changes and hot-key behavior are tested with production-like distributions.

### 8.7 Delivery semantics

Redpanda transactions provide atomic multi-message publication and exactly-once consume-transform-produce behavior when clients are configured correctly. They do not make PostgreSQL, ClickHouse, brokers, email providers, or payment systems part of the same transaction.

Therefore:

- PostgreSQL services use a transactional outbox.
- Consumers with external side effects use an inbox/idempotency table.
- ClickHouse sinks deduplicate by immutable event ID and projection version.
- Broker submissions use internal and provider idempotency identifiers plus reconciliation.
- Email, push, and report jobs store delivery attempts.
- Consumers commit offsets only after their defined durable effect.

### 8.8 Retention classes

| Class | Examples | Policy |
|---|---|---|
| Ephemeral high-volume | normalized top-of-book updates | short local retention; selected archive to S3 |
| Replayable market | trades, bars, feed status | longer retention with tiered storage |
| Business event | orders, fills, ledger publication | long retention plus immutable S3 archive |
| Compacted state | instrument metadata, entitlements | compaction plus bounded delete retention |
| Audit/security | security events, admin actions | long retention and S3 Object Lock |

Raw vendor packet captures go directly to compressed, partitioned S3 objects with checksums and capture manifests. Redpanda carries references and normalized events; it is not used as an unlimited packet-capture store.

---

## 9. Transactional data architecture

### 9.1 PostgreSQL clusters

Use separate AWS RDS PostgreSQL Multi-AZ clusters for failure and access isolation:

1. **Identity PostgreSQL** — Better Auth users, sessions, accounts, verification, passkeys, JWKS metadata.
2. **Product PostgreSQL** — profiles, organizations, workspaces, watchlists, alert definitions, plans, notification settings.
3. **Trading PostgreSQL** — broker accounts, order state, execution state, risk decisions, portfolio ledger, reconciliation workflows.

The OMS and ledger use separate schemas and owners within the trading cluster but can use explicit database transactions for invariants that cross their tightly coupled financial boundary. Other services do not query those schemas directly.

Every cluster has:

- automated backups and point-in-time recovery
- encryption at rest with customer-managed KMS keys
- private subnets
- TLS-required connections
- connection pooling
- migration ownership by one service/domain
- query timeouts and statement observability
- tested restore procedures
- cross-region replicas for disaster recovery

### 9.2 Financial types

Canonical values are explicit:

```text
decimal_value
- mantissa: signed 128-bit logical integer
- scale: bounded integer
- currency or unit where applicable
```

Database representation uses checked `NUMERIC` columns or integer mantissa/scale columns according to access pattern. Rust domain types prevent adding different currencies or interpreting quantity as price. Floating point is allowed for visual chart geometry and selected statistical calculations, not authoritative money, price, fee, quantity, or P&L journals.

### 9.3 Transactional outbox and inbox

Each state-changing PostgreSQL transaction inserts domain events into an outbox table. A Rust publisher reads committed rows, publishes them to Redpanda with idempotent producer settings, and records publication state. Consumers with database effects record event IDs in an inbox table in the same transaction as the effect.

Outbox lag, retry count, oldest unpublished row, inbox conflicts, and poison events are monitored as first-class service health.

### 9.4 Redis

AWS ElastiCache Redis is permitted for:

- Better Auth secondary session cache and rate limits
- edge rate-limit counters
- short-lived authorization decision cache
- distributed ephemeral presence
- short-lived stream resume metadata

Better Auth sessions remain stored in PostgreSQL, including preserved revocation history. Redis is never the only record of a user, order, fill, balance, entitlement, or workspace.

---

## 10. Analytical and historical data

### 10.1 ClickHouse from the beginning

ClickHouse is provisioned with the first production data pipeline. It stores rebuildable analytical projections:

- normalized ticks and quotes selected for query
- bars and derived market series
- execution and order analytics
- P&L time series
- journal/trade groupings
- alert evaluation history
- performance and operational aggregates

A Rust sink consumes Redpanda and writes idempotent batches. The sink owns schema conversion, event-ID deduplication, backpressure, and dead-letter handling. Direct writes from every service are forbidden.

ClickHouse tables partition by time and order by the dimensions used by queries, such as `(instrument_id, event_time)` or `(user_id, account_id, event_time)`. Raw event IDs and projection versions are retained so a projection can be rebuilt and compared.

Reference: [ClickHouse Rust integration](https://clickhouse.com/docs/integrations/rust).

### 10.2 S3 and Parquet

S3 is the permanent historical lake:

```text
s3://axiusflow_data/<environment>/<dataset>/
  source=<provider>/
  asset_class=<class>/
  date=YYYY-MM-DD/
  hour=HH/
  part_....parquet
```

Store:

- raw feed captures
- normalized market events
- bars
- broker raw messages
- immutable business-event archives
- report artifacts
- backtest inputs/results
- model and indicator artifacts
- audit exports

Objects include checksums, schema IDs, source provenance, capture versions, and encryption metadata. Object Lock is enabled for regulated audit datasets according to retention policy.

### 10.3 Data quality

Data quality checks include:

- sequence gaps
- timestamp regressions
- crossed/locked quote policy
- invalid price/quantity scales
- impossible OHLC relationships
- duplicate event IDs
- venue/session mismatches
- corporate-action discontinuities
- provider divergence

Quality flags travel with the data. Bad data is quarantined, not silently repaired without evidence.

---

## 11. Authentication and authorization architecture

### 11.1 Better Auth deployment

Better Auth runs as a private self-hosted `auth_service` behind the edge gateway. It uses:

- official Better Auth packages pinned to exact versions
- Node.js LTS runtime in a minimal locked container
- dedicated identity PostgreSQL
- database-backed sessions
- Redis as secondary cache, with sessions also preserved in PostgreSQL
- passkeys/WebAuthn
- verified email
- TOTP or passkey step-up
- OAuth/OIDC providers as product requirements dictate
- JWT/JWKS plugin for Rust service authentication
- Ed25519 keys with scheduled rotation and grace period
- secrets from AWS Secrets Manager/KMS
- strict trusted-origin and cookie configuration

The auth database is not exposed to other services. Rust services link users by immutable Better Auth subject ID.

### 11.2 Browser sessions

The browser uses secure, HTTP-only, `SameSite` cookies. Session tokens are not stored in `localStorage`. CSRF protections, origin validation, device metadata, and short freshness windows are enabled.

### 11.3 Native desktop login

The desktop app uses the system browser for login with OAuth/OIDC-style PKCE and a protected loopback or registered deep-link callback. The terminal does not embed a login webview. Long-lived refresh/session material is stored in the operating-system credential vault; access tokens remain in memory.

### 11.4 Service access tokens

Better Auth issues short-lived JWTs for Axiusflow APIs. The edge and Rust services verify:

- signature against cached JWKS
- `kid`
- issuer
- audience
- subject
- expiration and not-before
- session/device identifier where present
- token purpose

A new `kid` triggers JWKS refresh. Tokens are short-lived. Key rotation retains old public keys through the maximum token lifetime plus clock skew.

### 11.5 Sensitive action policy

JWT verification alone is insufficient for:

- enabling live trading
- changing broker connections
- withdrawals or funding changes
- creating API keys
- changing MFA/passkeys
- changing security email or password
- viewing highly sensitive exports
- administrative impersonation

These operations require a fresh Better Auth session or step-up proof plus a live authorization decision. The resulting security decision is audited.

### 11.6 Authorization is Rust-owned

Better Auth organizations or roles may assist login UX, but trading permissions and market-data entitlements are owned by the Rust authorization service. This prevents the authentication library's schema or plugin model from becoming the financial authorization model.

---

## 12. API and protocol architecture

### 12.1 Public APIs

- REST/JSON for ordinary external integration and product control operations.
- Binary WebSocket streams for broadly compatible real-time delivery.
- WebTransport may be added behind negotiation where browser support and infrastructure are proven.
- HTTP idempotency keys for state-changing requests.
- Cursor-based pagination and immutable resource versions.
- Explicit API versioning and deprecation windows.

### 12.2 Native client APIs

The native client may use gRPC/HTTP2 for typed control operations and the same binary streaming protocol used by the browser. It must not receive privileged internal service credentials.

### 12.3 Internal APIs

Synchronous service-to-service operations use `tonic` gRPC with:

- Protobuf contracts
- deadlines on every request
- bounded retries only for safe/idempotent operations
- mTLS workload identity
- propagated trace, correlation, causation, and actor context
- circuit breaking and concurrency limits

Examples of synchronous calls:

- order gateway to risk engine
- order gateway to OMS
- edge gateway to authorization service
- OMS to broker connection service for a submission

State-change notifications, projections, workflows, and analytics use Redpanda.

### 12.4 Client stream framing

Client streams include:

- stream ID
- subscription ID
- message sequence
- snapshot/delta marker
- schema version
- server timestamp
- resumable offset where supported
- entitlement version

A client that detects a gap requests a new snapshot. Resume tokens are signed and short-lived.

---

## 13. Provider architecture

Providers are adapters, not architecture. Commercial agreements, launch jurisdiction, asset classes, and redistribution rights must be confirmed before production.

### 13.1 Brokerage connectivity

**Connected external accounts:** SnapTrade provides broad account linking and supported trading through a normalized integration. It is used for breadth, while direct adapters are built for brokers that account for significant volume or require richer behavior.

- [SnapTrade brokerage API](https://snaptrade.com/brokerage-api)
- [SnapTrade connection portal](https://docs.snaptrade.com/docs/implement-connection-portal)

**Direct integrations:**

- Interactive Brokers for broad global access where its API and commercial terms fit.
- Alpaca for direct US trading and an embedded brokerage path.
- Coinbase or Coinbase Prime for applicable crypto products.
- OANDA for supported FX jurisdictions.

**Professional tier:** FIX, certified broker/FCM gateways, redundant counterparties, and venue-adjacent infrastructure are separate adapters to the same OMS contracts. A retail aggregation API is never described as low-latency institutional execution.

### 13.2 Market data

**Primary US equities/options/futures:** Databento, including its supported Rust integration and licensed live/historical data.

- [Databento live data](https://databento.com/live)
- [Databento options](https://databento.com/options)

**Broader global coverage and enterprise alternative:** dxFeed.

- [dxFeed market data](https://dxfeed.com/market-data/)

**Crypto:** direct streams from each supported venue; a composite is explicitly labeled and preserves venue provenance.

**Economic, earnings, news, and corporate actions:** select a licensed vendor through an RFP based on geography and redistribution rights. These feeds use separate canonical event families and cannot be inferred reliably from price data.

### 13.3 Cloud and platform providers

- Cloudflare: DNS, CDN, WAF, DDoS protection, bot controls, and coarse edge limits.
- AWS: VPC, EKS, EC2, RDS PostgreSQL, ElastiCache Redis, S3, KMS, Secrets Manager, SES, load balancers, PrivateLink, backup, and disaster recovery.
- Redpanda Dedicated/BYOC: event streaming in the Axiusflow AWS network.
- ClickHouse Cloud Dedicated: analytics with private connectivity.
- Grafana Cloud or a dedicated Grafana stack: OpenTelemetry metrics, traces, and logs.
- Stripe: subscription billing where applicable; billing status is projected into Axiusflow entitlements rather than checked synchronously on each request.

AWS publishes a supported Rust SDK: [AWS SDK for Rust](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/welcome.html).

---

## 14. Infrastructure topology

### 14.1 Primary region

The primary region contains:

- Cloudflare-to-AWS private/public ingress
- multi-AZ EKS cluster for edge, control, analytics workers, and general services
- dedicated EC2 Auto Scaling Groups for market-data and execution gateways
- Redpanda Dedicated/BYOC across three AZs
- three isolated PostgreSQL clusters
- ElastiCache Redis
- S3 buckets with lifecycle and Object Lock policies
- private ClickHouse connectivity
- centralized OpenTelemetry collectors

### 14.2 Kubernetes usage

Kubernetes is used for deployability and isolation, not placed in the innermost latency loop without measurement.

Node pools:

- `edge`: network-optimized, autoscaled
- `control`: general services
- `streaming`: streaming gateways with high connection counts
- `analytics`: memory/CPU workers
- `security`: auth and authorization with restrictive policies

Market feed handlers and professional execution gateways run on dedicated EC2 hosts or bare-metal/colocation hosts with pinned resources, predictable networking, and no noisy neighbors. They still use the same contracts and release pipeline.

### 14.3 Multi-region model

The platform does not use naive active-active writes for orders or ledgers. Each trading account has a home execution region. Reads and general product APIs may be globally active, but order and ledger writes are routed to their owning region.

Disaster recovery includes:

- cross-region PostgreSQL replicas and tested promotion
- Redpanda disaster-recovery replication/cluster linking according to supported deployment capabilities
- S3 cross-region replication for critical archives
- replicated container images and configuration
- ClickHouse backups and rebuildable projections
- DNS/edge failover runbooks
- broker reconnection and reconciliation procedures

RPO and RTO are assigned per domain; market dashboards and authoritative financial journals do not share one blanket target.

### 14.4 Infrastructure as code

Terraform defines cloud, Redpanda, ClickHouse networking, IAM, KMS, DNS, and databases. Kubernetes manifests are generated and reviewed from versioned configuration. Production changes require plans, policy checks, and audit records.

---

## 15. Performance architecture

### 15.1 Market-data hot path

```text
socket → protocol decoder → sequence validator → canonical struct
       → partitioned bounded queue → Redpanda producer
       → latest-state/conflation → streaming gateway
```

Principles:

- one stable shard for an instrument/source stream
- no JSON
- no database lookup per tick
- pre-resolved instrument mappings in memory
- pooled buffers where useful
- batch Redpanda publication within explicit latency limits
- capture raw source bytes asynchronously without blocking decode
- separate lossless archival flow from conflated UI flow
- sequence and quality metadata preserved

### 15.2 Order hot path

```text
client → edge → order gateway → risk → OMS durable intent
       → broker gateway → provider
```

The user request uses direct RPC, not a consume loop through Redpanda. Order and risk events are published through the transactional outbox. This preserves low latency without sacrificing durable event distribution.

### 15.3 In-process concurrency

- Tokio for network and asynchronous orchestration.
- Bounded channels only.
- Dedicated tasks or threads for ordered partitions.
- `Arc` immutable snapshots for read-heavy state.
- Avoid shared global mutexes.
- No blocking file/database calls on async executor threads.
- `spawn_blocking` or dedicated worker pools for CPU-heavy analytics.
- CPU affinity and allocator tuning only after profiling.
- `unsafe` requires a measured need, written invariant, fuzzing, and code-owner approval.

### 15.4 UI performance

- Network tasks update immutable model snapshots off the GPUI render path.
- UI drains one merged command batch per frame.
- Origin's invalidation levels determine repaint scope.
- Visible-range slicing and data conflation bound rendering work.
- Virtualized GPUI tables never materialize all rows as elements.
- Chart, table, and order-book updates have independent repaint regions.

### 15.5 Initial engineering SLO targets

These are engineering targets to validate on defined hardware and network conditions, not unqualified marketing claims:

- no loss of an acknowledged authoritative financial journal transaction
- p99 internal order acceptance under 25 ms, excluding broker network/provider time
- p99 regional normalized quote-to-stream publication under 10 ms under contracted feed load
- p99 client stream resume under 1 second when retained state permits
- 60 FPS chart pan/zoom on the defined Origin benchmark matrix
- crosshair interaction below one display-frame budget
- deterministic recovery of every in-flight order to a reconciled state
- 99.99% availability target for market-data display and read APIs
- separately defined execution availability and degraded-mode policy

Every target has a workload definition, dataset, machine profile, and percentile report.

---

## 16. Security architecture

### 16.1 Network and workload identity

- private subnets for databases, Redpanda, ClickHouse links, and internal services
- default-deny Kubernetes network policies
- workload identity through AWS IAM roles for service accounts
- mTLS for internal RPC
- unique Redpanda principal and ACL per service
- no shared production credentials
- egress allowlists for broker and vendor gateways

### 16.2 Secrets and broker credentials

- AWS KMS customer-managed keys
- envelope encryption per broker credential record
- Secrets Manager for service bootstrapping secrets
- zeroization of decrypted credential buffers where practical
- credentials never included in logs, traces, analytics, crash reports, or client payloads
- rotation and revocation workflows
- optional Nitro Enclaves/HSM-backed signing for narrowly defined high-assurance operations

### 16.3 Audit

Audit events include actor, effective principal, action, target, policy decision, device/session, source, before/after references, correlation ID, and timestamp. Security and financial audit exports are written to immutable S3 retention where required.

### 16.4 Supply chain

- exact dependency versions and reviewed lockfiles
- `cargo audit`, `cargo deny`, license policy, and vulnerability scanning
- SBOM generation
- signed containers and desktop artifacts
- provenance attestations
- protected release branches and required review
- no skipped commit or deployment hooks in production release flows
- fuzzing for protocol decoders, order state transitions, and untrusted event payloads

### 16.5 Desktop security

- signed installers and updates
- platform notarization where applicable
- update manifests signed independently of transport TLS
- OS credential vault
- no secrets in local logs
- encrypted local cache for sensitive account data
- automatic lock and remote session revocation
- anti-downgrade update policy

---

## 17. Market-data licensing and regulatory boundaries

Market-data agreements distinguish display, non-display, derived, delayed, real-time, professional, non-professional, internal distribution, and external redistribution. The entitlement service stores the contracted dimensions and enforces them before streaming, export, replay, API response, alert evaluation, or algorithmic use.

Broker data is not assumed to be redistributable. Interactive Brokers, for example, explicitly restricts dissemination of licensed market data without approval: [IBKR API market-data restrictions](https://www.interactivebrokers.com/en/index.php?f=1538&p=api).

Before live trading, legal and compliance teams must determine Axiusflow's role in each jurisdiction. Software vendor, introducing broker, broker-dealer, adviser, ATS, money-transmission, and custody obligations are not interchangeable. Contracts must allocate responsibility for KYC/CIP, AML/sanctions, options approval, suitability, best execution, books and records, statements, tax reporting, complaints, corporate actions, business continuity, and surveillance.

The architecture provides evidence, controls, entitlements, and auditability. It does not itself determine the legal classification.

---

## 18. Observability and operations

All Rust services use `tracing` and OpenTelemetry. Telemetry includes:

- request and stream latency
- Redpanda produce/consume latency and consumer lag
- partition skew and hot keys
- outbox age and retry count
- database pool saturation and slow queries
- broker session health
- provider round-trip latency
- market-data sequence gaps
- order state-transition failures
- reconciliation breaks
- entitlement denials
- stream disconnect/conflation rates
- chart frame and UI interaction timing

Trace context propagates through gRPC and event envelopes. High-cardinality trading identifiers are controlled and protected; secrets and unnecessary personal data are prohibited.

Operational dashboards are separated by domain:

- execution and risk
- broker connectivity
- market data
- financial ledger and reconciliation
- client streaming
- identity/security
- product/control plane
- data platform

Alerts point to versioned runbooks. A service is not production-ready without ownership, SLO, dashboards, alerts, rollback, and recovery procedures.

---

## 19. Testing and certification strategy

### 19.1 Domain correctness

- property tests for fixed-point arithmetic and ledger invariants
- model-based tests for order state machines
- deterministic clocks and IDs
- replay tests from captured provider events
- duplicate, reorder, correction, and gap scenarios
- broker adapter conformance suites
- entitlement-policy tests

### 19.2 Trading safety

- simulated broker environment
- shadow orders that never leave Axiusflow
- paper trading
- restricted canary accounts
- order-rate and notional caps during rollout
- kill-switch drills
- unknown-outcome and reconnect tests
- daily reconciliation before expanding access

### 19.3 Market data

- packet/message capture replay
- sequence-gap injection
- provider divergence tests
- bar reconstruction comparisons
- corporate-action correction tests
- entitlement leakage tests

### 19.4 Origin and GPUI

The same versioned chart fixture runs through:

- Origin WebGPU
- Origin Canvas2D
- Origin native renderer
- `origin_render_gpui`

Tests compare geometry, interactions, scales, visible ranges, screenshots where deterministic, and performance. GPUI adapter work is complete only when it consumes the shared `ChartFrame`; visual similarity from a separately implemented chart is not acceptance.

### 19.5 Resilience

- Redpanda broker/AZ failure
- PostgreSQL failover and restore
- Redis loss
- Better Auth restart and key rotation
- broker disconnect/reconnect
- delayed and duplicated callbacks
- ClickHouse unavailability
- S3 throttling
- region evacuation exercises

Analytical or notification failures must not block order processing. Risk or authoritative persistence failures must fail trading safely.

---

## 20. Delivery sequence within the final architecture

This sequence controls risk; it does not introduce temporary components.

### Stage 1: Foundation and contracts

Deliver:

- monorepo and crate boundaries
- Protobuf conventions and Schema Registry policy
- AWS networking and environments
- Redpanda, PostgreSQL, ClickHouse, Redis, S3, and observability
- Better Auth service and Rust JWT verification
- authorization service
- outbox/inbox libraries
- instrument identity model
- GPUI shell and `origin_render_gpui` contract proof

Exit criteria: one authenticated user can open the GPUI application, load a versioned instrument, receive a replayed market stream through Redpanda, and render the same Origin frame through GPUI and the existing backend.

### Stage 2: Read-only connected trading data

Deliver:

- SnapTrade and first direct broker adapter
- account, positions, balances, transactions, and orders
- portfolio ledger import
- independent reconciliation
- Databento market data
- bars and chart history
- ClickHouse projections
- dashboard metrics, win rate, equity curve, and trading calendar

Exit criteria: broker state reconciles reproducibly and every dashboard metric traces back to ledger inputs.

### Stage 3: Controlled execution

Deliver:

- order ticket
- authorization and step-up policy
- risk engine
- OMS state machine
- submit/cancel/replace
- provider event normalization
- kill switches
- paper trading, then limited live accounts

Exit criteria: crash/restart, timeout, duplicate callback, and reconciliation tests recover every test order to an explainable final state.

### Stage 4: Professional terminal capability

Deliver:

- multi-chart workspaces
- order books
- advanced alerts
- drawings and indicators
- saved templates
- scanners
- news and external calendars
- advanced analytics and journal tagging
- deterministic backtests and sandboxed WASM plugins

### Stage 5: Global and institutional expansion

Deliver as products require:

- additional regions and home-region routing
- additional carrying brokers and direct feeds
- FIX/FCM gateways
- venue-adjacent execution hosts
- advanced derivatives risk
- surveillance and regulatory reporting
- multi-provider market-data arbitration

No stage replaces Better Auth, Origin, Redpanda, PostgreSQL, ClickHouse, or the domain contracts established in Stage 1.

---

## 21. Explicit rejected designs

The following are not part of this architecture:

1. A second financial chart engine exposed beside Origin.
2. Embedding the browser chart in a GPUI webview.
3. Reimplementing price/time scales in the GPUI adapter.
4. NATS as an interim bus.
5. Redis as an order, session, entitlement, or portfolio source of truth.
6. ClickHouse as the authoritative order or ledger database.
7. Redpanda request/reply for synchronous risk and order submission.
8. JSON market-data events on internal hot paths.
9. Floating-point money or quantity journals.
10. Provider SDK models shared across service boundaries.
11. One database schema writable by every service.
12. Naive global active-active order writes.
13. Broker callbacks directly updating order rows.
14. Trusting broker WebSocket continuity instead of reconciliation.
15. Storing desktop bearer tokens in plaintext files or browser tokens in `localStorage`.
16. Using unofficial alpha authentication compatibility code as the production trust anchor.
17. Running untrusted user strategies as native dynamic libraries.
18. Claiming exchange-data rights that are absent from contracts.

---

## 22. Governing architecture decisions

| Area | Decision |
|---|---|
| Primary implementation language | Rust |
| Naming convention | `snake_case` for every Axiusflow-owned path, service, module, function, field, API/wire name, schema object, topic segment, configuration key, and public programmable identifier; only documented language/external compatibility exceptions apply |
| Design system | `docs/platform_design_system.md`; exact legacy token compatibility with exactly three concrete radii: `radius_sm`, `radius_default`, and `radius_full` |
| Authentication | Official Better Auth, self-hosted; isolated TypeScript exception |
| Authorization | Rust service with explicit policies and entitlements |
| Native UI | GPUI + GPUI Component |
| Financial charting | Origin Charts only |
| Native chart backend | `origin_render_gpui` consuming shared `ChartFrame` |
| Browser chart backend | Origin Rust/WASM WebGPU with Canvas2D fallback |
| Durable event streaming | Redpanda from inception |
| Event schemas | Protobuf + Redpanda Schema Registry |
| Synchronous internal RPC | gRPC/HTTP2 with deadlines and mTLS |
| Transactional storage | Isolated PostgreSQL clusters/schemas |
| Analytical storage | ClickHouse from inception |
| Historical/raw storage | S3 + Parquet |
| Ephemeral cache | Redis, never authoritative |
| Initial serious market data | Databento; dxFeed for broader coverage |
| Existing broker aggregation | SnapTrade plus direct high-value adapters |
| Cloud | AWS with Cloudflare edge |
| Runtime orchestration | EKS for general services; dedicated hosts for latency paths |
| Observability | OpenTelemetry with Grafana-compatible backend |
| Custom strategy isolation | WebAssembly/Wasmtime capability sandbox |
| Financial numeric model | checked fixed-point/integer domain types |
| Global trading writes | account home-region ownership |

---

## 23. Definition of architectural success

The architecture is functioning as intended when:

1. The same Origin chart state and frame render through GPUI, WebGPU, Canvas2D, and native tests without duplicate chart models.
2. A broker or market-data provider can be added without changing order, portfolio, chart, or analytics domain types.
3. Every accepted order has a durable intent, deterministic state history, raw provider evidence, and reconciliation result.
4. Every displayed portfolio metric traces to immutable ledger inputs and a versioned calculation policy.
5. Redpanda events can rebuild ClickHouse projections and downstream state without rewriting authoritative financial records.
6. Better Auth can be restarted, sessions revoked, keys rotated, and JWT verification continued safely.
7. Market-data entitlements are enforced at every publication and export boundary.
8. A slow client, analytics outage, or ClickHouse failure cannot degrade order correctness.
9. A broker disconnect or unknown submission result converges through reconciliation rather than guesswork.
10. Performance claims are backed by repeatable headless and end-to-end benchmarks on named hardware.
11. Disaster-recovery exercises restore authoritative state and explain all in-flight orders.
12. Nearly all first-party product and infrastructure logic remains Rust, with the Better Auth service as a small, explicit, isolated exception.
13. Automated naming checks reject non-snake_case Axiusflow-owned paths, services, APIs, fields, schemas, topics, configuration keys, and generated public identifiers.

---

## 24. Source and licensing note

External product capabilities referenced in this document were checked against official Better Auth, Redpanda, Origin Charts, GPUI, GPUI Component, AWS, ClickHouse, SnapTrade, Databento, dxFeed, and Interactive Brokers materials. Their content has been summarized and rephrased rather than reproduced. Commercial availability, pricing, jurisdiction coverage, exchange-data rights, and regulatory responsibilities require direct contracts and legal review before implementation or launch.
