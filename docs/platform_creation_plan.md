# Axiusflow Platform Creation Plan

**Status:** Working implementation and stabilization plan, revision 7 (checkbox-tracked)
**Audience:** Product engineering, platform engineering, security, compliance, data engineering, and technical leadership
**Primary language:** Rust
**Supported native operating systems:** Windows, Linux, and macOS
**Last updated:** 2026-08-04

## Contents

**Part I — How this plan is executed**

- [1. Purpose and document status](#1-purpose-and-document-status)
  - [1.1 Current implementation baseline](#11-current-implementation-baseline)
  - [1.2 Implementation truth rules](#12-implementation-truth-rules)
  - [1.3 Mandatory repair-before-expansion gate](#13-mandatory-repair-before-expansion-gate)
  - [1.4 Mandatory cohesive Rust module architecture](#14-mandatory-cohesive-rust-module-architecture)
  - [1.5 Priority decomposition map](#15-priority-decomposition-map)
  - [1.6 Decomposition completion criteria](#16-decomposition-completion-criteria)
  - [1.7 Progress tracking protocol](#17-progress-tracking-protocol)
  - [1.8 Stage status summary](#18-stage-status-summary)

**Part II — Standing rules and target design**

- [2. Non-negotiable design principles](#2-non-negotiable-design-principles)
- [3. Language, safety, and authentication policy](#3-language-safety-and-authentication-policy)
- [4. System topology](#4-system-topology)
- [5. Logical contexts and deployment model](#5-logical-contexts-and-deployment-model)
- [6. Cross-platform native client architecture](#6-cross-platform-native-client-architecture)
- [7. Real-time market-data plane](#7-real-time-market-data-plane)
- [8. Client and internal transport architecture](#8-client-and-internal-transport-architecture)
- [9. Execution plane](#9-execution-plane)
- [10. Control plane, identity, and authorization](#10-control-plane-identity-and-authorization)
- [11. Durable data plane](#11-durable-data-plane)
- [12. Analytics, strategies, alerts, and product workflows](#12-analytics-strategies-alerts-and-product-workflows)
- [13. Provider and licensing architecture](#13-provider-and-licensing-architecture)
- [14. Current infrastructure and future cloud profiles](#14-current-infrastructure-and-future-cloud-profiles)
- [15. Concurrency, memory, and hot-path mechanics](#15-concurrency-memory-and-hot-path-mechanics)
- [16. Performance model and release gates](#16-performance-model-and-release-gates)
- [17. Observability and operations](#17-observability-and-operations)
- [18. Security, resilience, and supply chain](#18-security-resilience-and-supply-chain)
- [19. Testing and certification](#19-testing-and-certification)

**Part III — Tracked execution work**

- [20. Delivery sequence from the current foundation](#20-delivery-sequence-from-the-current-foundation)
  - [Stage 0A: Stabilize and correct the existing foundation](#stage-0a-stabilize-and-correct-the-existing-foundation)
  - [Stage 0B: Cohesively decompose corrected monolithic crates](#stage-0b-cohesively-decompose-corrected-monolithic-crates)
  - [Stage 1: Complete the cross-platform provider foundation](#stage-1-complete-the-cross-platform-provider-foundation)
  - [Stage 2: Connected read-only market data](#stage-2-connected-read-only-market-data)
  - [Stage 3: Durable controlled execution](#stage-3-durable-controlled-execution)
  - [Stage 4: Professional terminal and transport](#stage-4-professional-terminal-and-transport)
  - [Stage 5A: Optional cloud data and venue-adjacent scale](#stage-5a-optional-cloud-data-and-venue-adjacent-scale)
  - [Stage 5B: Optional managed execution](#stage-5b-optional-managed-execution)

**Part IV — Constraints, decisions, and definition of success**

- [21. Explicit rejected designs](#21-explicit-rejected-designs)
- [22. Governing architecture decisions](#22-governing-architecture-decisions)
- [23. Definition of architectural success](#23-definition-of-architectural-success)
- [24. Research, source, and licensing note](#24-research-source-and-licensing-note)

Part I governs how work proceeds. Part II describes standing rules and the intended target design and contains no progress claims. Part III holds the tracked checkbox work items. Part IV records hard constraints and completion meaning.

---

## 1. Purpose and document status

Axiusflow is a high-performance trading and financial platform. It combines a professional native terminal, broker-connected execution, portfolio and journal analytics, market-data visualization, alerts, research, and ordinary product functionality such as identity, subscriptions, workspaces, notifications, administration, and support.

This is a **platform creation plan**, not a declaration that the target architecture already exists. It governs the sequence used to turn the current foundation into the intended product. A separate final architecture document is created only after the platform is stable, its principal vertical paths are implemented, and measured evidence confirms the deployed topology.

The immediate order of work is mandatory:

1. establish the real current state and protect concurrent work;
2. correct incomplete, contradictory, failing, or overstated current implementations;
3. validate the corrected behavior at its honest readiness level;
4. decompose stable monolithic crates into cohesive internal modules without changing behavior;
5. continue product implementation using those module boundaries from the first commit of every new capability;
6. promote readiness and performance claims only from recorded evidence.

This order does not mean completing the whole product inside monolithic files and splitting it afterward. It means repairing the **existing** foundation first, decomposing that corrected foundation, and then refusing to add further unrelated implementation to oversized `lib.rs` files.

The objective is not an unqualified claim of "absolute" latency. No architecture can remove exchange distance, provider latency, internet routing, operating-system scheduling, or display scanout. The objective is:

> Deliver the lowest practical and repeatable latency within each declared hardware, operating-system, network, durability, and correctness profile, with no hidden loss of financial correctness.

This plan separates stable structural decisions from benchmark-driven runtime choices:

- **Structural decisions** are intended to remain stable: Rust-first domains, provider-neutral contracts, Origin Charts, GPUI boundaries, fixed-point financial values, local bounded provider sessions, explicit replay rights, authoritative transactional state where Axiusflow owns it, entitlement enforcement, and reconciliation.
- **Runtime topology and acceleration profiles** evolve only through measured architecture decisions. A transport, queue, allocator, kernel interface, or deployment boundary is never permanent merely because it appeared in the first implementation.

Axiusflow supports Windows, Linux, and macOS as first-class native desktop targets. In the current product phase, each desktop connects directly to the user's entitled Rithmic, CQG, or other approved provider endpoint; Axiusflow does not receive, relay, persist, or redistribute that market data through its cloud. A future Axiusflow cloud market-data service remains a planned option, not a present topology or an implementation assumption. It requires separate provider rights, compliance approval, economics, security design, and measured evidence before activation.

Rithmic- and CQG-specific adapter, conformance, certification, and live-provider work begins only after the applicable partnership or commercial access supplies the required dev kit, protocol materials, credentials, rights, and test environment. That external dependency blocks only those provider-specific work items; it does not pause the rest of the platform. Provider-neutral desktop runtime, canonical models, charting, order flow, local caching, startup/hydration, replay, OMS/risk foundations, UI, and performance instrumentation continue in parallel and are tested with deterministic fixtures and legally authorized crypto data such as the existing Coinbase reference feed. Passing crypto or replay tests proves the shared architecture and performance profile, not Rithmic/CQG certification or semantic equivalence.

The near-term product is a local-first professional terminal in the same broad category as Sierra Chart, ATAS, and MotiveWave: the user's machine owns provider connectivity, chart state, order-flow processing, and direct broker order routing. That category describes the deployment model, not a claim about another product's internal implementation or relative performance. Axiusflow differentiates through fast measured startup, progressive history hydration, shared local state, smooth frame pacing, correct recovery, and reproducible performance evidence. Funding or infrastructure growth is not a prerequisite for this topology.

The implementation may be delivered in vertical slices, but every slice must fit these contracts. Delivery sequence is not permission to create fake external services, disposable domain models, unbounded queues, a second chart state model, or a new monolithic crate.

### 1.1 Current implementation baseline

This baseline records the audited state, not a promise about work that may be changing concurrently. Before modifying a component, the implementing agent must inspect the live Git status and diff, preserve unrelated work, and update this table only from source and validation evidence.

| Area | Current reality | Required next gate |
|---|---|---|
| `portable_network` | Real bounded UDP test driver with preallocated slots and explicit release/drop accounting; Rithmic/CQG managed APIs do not use this transport | Preserve its reusable ownership/conformance contracts, but do not treat UDP loopback as the direct-provider production baseline |
| `linux_socket_network` | Historical tuned Linux UDP experiment; it does not accelerate managed WSS/TLS provider sessions | Remove it from active provider selection with the raw-packet profiles and reintroduce only a measured provider-supported socket optimization |
| `linux_af_xdp` | Historical experimental work proves an exact-pinned copy-mode veth lifecycle and explicit buffer ownership; it has no role beneath the current managed provider sessions | Remove it from active product scope and release surfaces with DPDK while preserving reusable contracts and historical evidence |
| `linux_dpdk` | Historical experimental work proves prerequisite probing and a version-locked EAL/`net_ring` lifecycle only; the current direct-to-user managed-provider topology has no DPDK use case | Remove it from active product scope and default validation/deployment surfaces while preserving reusable transport contracts and historical evidence; reconsider only after a separately approved raw/multicast cloud-feed program exists |
| Windows/macOS network adapters | Historical fixture drivers for the packet-ingest contract; native mode is explicitly unimplemented | Replace active product selection with the cross-platform provider WebSocket adapter and optional certified provider SDK boundaries |
| `stream_websocket` | Bounded binary WebSocket framing/session and background-runtime contracts exist, but no Rithmic/CQG protocol adapter is connected | Reuse its applicable lifecycle/bounds while implementing provider-specific WSS, Protobuf, heartbeat, authentication, recovery, and certification |
| `transport`, `realtime`, protocols, recovery, and queues | Substantive bounded contracts and implementations exist, still concentrated in large files; `crates/testing` is now decomposed into sixteen cohesive modules behind a façade | Correct current defects, preserve semantics, then split by cohesive responsibility |
| `persistence` | Transactional inbox/outbox traits with first-party staging-order and idempotent-receipt tests, a checksummed ordered migration runner with drift rejection, and a real PostgreSQL outbox/inbox implementation proven against a digest-pinned server including reconnect recovery | Exercise TLS, role isolation, and backup/PITR drills in the production deployment |
| Desktop-local storage | The `S2-22` measured decision selects exact-pinned `rusqlite 0.40.1` with bundled SQLite 3.53.2 in WAL/FULL mode. `crates/desktop_storage` implements the blocking worker-side bounded keyed manifest plus encrypted immutable history files, and `crates/desktop_history` composes exact visible-range reads into bounded immutable publications. `crates/desktop_provider_runtime::DesktopMarketWorker` now composes that history owner, provider lifecycle fencing, the direct Coinbase session driver, bounded one-minute aggregation, and current-history seeding on one non-`Send` worker, but no UI drives it | Integrate the composed worker with the UI during `S2-19`, then obtain Windows/macOS, destructive-crash, backup-exclusion, key-lifecycle, and production performance evidence |
| Provider history | `crates/provider_history` implements the provider-neutral bars/ticks/depth capability matrix, bounded deduplicating/cancellable scheduler, explicit expired-work and terminal-fetch-failure reporting, visible/adjacent priority, monotonic rate/concurrency gates, page validation, immutable validated completions, and contiguous snapshot/backfill/live handoff. `crates/desktop_history` composes that handoff with local storage for the deterministic `S2-25` matrix, and `DesktopMarketWorker` fences its callbacks by provider session generation. `crates/adapters/coinbase_market` implements the first matching direct-provider fetch adapter and reusable bar aggregator: bounded BTC-USD/ETH-USD public one-minute candles over HTTPS, exact fixed-point decoding, identity-bound canonical payloads, and bounded deterministic live trade-to-bar conversion. A deterministic composition passes a Coinbase page through the scheduler, installs it only after exact segment identity, terminal-pagination, generation, decode-size, empty-cutover, and continuity checks, seeds its latest fetchable completed minute into the aggregator, and opens the following generation-fenced live minute without duplication; the shipping desktop event loop, other Coinbase precision profiles, ticks/depth, and every Rithmic/CQG history profile remain unavailable | Drive the proven Coinbase completion and aggregation boundary from the shipping desktop during `S2-19`; then add each commercial-provider profile, fixtures, rights checks, and certification after partnership access |
| `platform_runtime` | Capability ports, a standard clock, a native credential vault backed by Linux Secret Service, macOS Keychain, or Windows Credential Manager, independently signed update-manifest and streamed-artifact verification with verifier-bound provenance, one-key trust-domain confinement, monotonic anti-downgrade, and verified-predecessor rollback state, fallible native affinity and non-real-time current-thread priority hints on Linux that refuse real-time callers and keep their balanced baseline privilege-free, a real RFC 7636 PKCE secret generator that redacts both verifier and state, and a loopback redirect listener that bounds accept, every read, and each connection against one absolute deadline, verifies `state` before any error path, and survives stray traffic, stalled peers, and browser disconnection, decomposed into fourteen cohesive modules with seventy-seven first-party tests, including runtime capability detection that reports only the ports this crate actually implements, a Unix crash-recoverable content-addressed update staging journal that durably preserves one signed active release and one rollback predecessor beneath a caller-provisioned root, bounded Linux logind suspend/resume notifications, a bounded Linux NetworkManager availability listener, a registered URI-scheme callback that verifies delivered redirects and registers a Linux XDG scheme handler, and a Linux Wayland display probe that reports per-output refresh, geometry-derived fractional scale, and the presentation feedback clock; per-OS installer activation, code-signature/notarization enforcement, Windows/macOS power, network, and display, and Windows/macOS priority and affinity remain incomplete | Complete per-OS installer activation and code-signature/notarization enforcement, Windows/macOS power, network, and display adapters, and safe Windows/macOS priority and affinity backends behind the existing capability ports |
| `terminal_ui` and desktop | Bounded UI infrastructure, a fixture/disconnected desktop path, a provider-neutral vault/generation/fencing lifecycle owner with a direct Coinbase driver not yet wired into the application, and a windowed replay-to-presented-frame benchmark proven on a named 165 Hz Wayland display profile | Connect live aggregation and the local history owner without putting network/decode work on the GPUI thread, then certify each supported provider |
| Authorization service | Runs a bounded typed HTTP boundary verifying Ed25519 service-access tokens against a pinned JWKS with allow/deny decisions, monotonic policy replacement with revocation, and proven startup/key/revocation/failure behavior; sessions and gRPC remain unimplemented | Implement session issuance (`S2-13`) and the gRPC workload-identity transport |
| `auth_service` | Runs the credential core: argon2 signup/sign-in with uniform failure timing, PostgreSQL sessions, first-party Ed25519 issuance, and revisioned JWKS whose tokens verify through `crates/security`; OAuth/OIDC, PKCE, second factors, and passkeys remain unimplemented | Implement OAuth/OIDC, native PKCE loopback, second factor and passkeys, TLS, and production key management per Section 3.2 |
| Origin Charts | Existing major external engine integrated through an Axiusflow bridge | Treat it as a separate exact-pinned dependency and compatibility boundary; do not count its engine implementation as newly completed Axiusflow work |
| Product verticals | A centralized Coinbase reference vertical proves TLS ingest, canonical latest state, bounded fanout, and optional durability, but it is not the current production delivery topology; direct user-device Rithmic/CQG adapters, OMS/risk/ledger, reconciliation, and the full production terminal remain | Reuse the proven contracts inside a desktop-local provider runtime, add direct Rithmic/CQG certification, and keep cloud market-data routing disabled until its separate future gate is approved |

Machine-readable readiness manifests must agree with actual behavior. If code only detects prerequisites or returns `unavailable`, its manifest cannot imply verified native operation. If current source and the manifest disagree, either complete the implementation and evidence or downgrade the readiness claim in the same change.

### 1.2 Implementation truth rules

Progress is recorded by working behavior and evidence, not file count or line count:

- A trait, port, type, constant, or capability enum is a contract, not an implementation.
- A deterministic fixture proves specified software semantics, not live provider, physical NIC, operating-system, or production behavior.
- A startup probe is not an active native data path.
- AF_XDP copy mode is not zero-copy; a virtual device is not qualified hardware.
- An inactive listener, explicit `unavailable` result, or disconnected desktop is not a running product service.
- A benchmark harness is not a benchmark result.
- Existing Origin engine capability is not newly implemented Axiusflow capability.
- A target described later in this plan remains unimplemented until source, tests, integration evidence, and operational behavior prove it.

Every component change must classify itself as one of `contract_only`, `fixture_validated`, `hardware_validated`, `provider_certified`, or `production_enabled`, using the narrower profile-specific readiness model where applicable. Documentation, runtime reporting, configuration, and evidence must use the same classification.

### 1.3 Mandatory repair-before-expansion gate

Before broad Stage 2 work or another large feature is added, finish the current stabilization pass:

1. inspect and preserve all concurrent and uncommitted work;
2. resolve failing workflows and dependency/path inconsistencies without hiding failures;
3. finish or honestly downgrade partial AF_XDP and DPDK claims;
4. reconcile readiness configuration with runtime behavior;
5. remove accidental fixture delegation from production paths;
6. verify bounded ownership, release, overflow, error, shutdown, and recovery behavior;
7. run targeted tests first, then the affected crate/workspace checks and the applicable CI-equivalent commands;
8. record what cannot be validated because privileged hardware, provider access, or commercial data is unavailable.

No agent may overwrite another agent's active work, discard a diff, bypass hooks, weaken a test, or silently substitute a fixture merely to make this gate pass.

### 1.4 Mandatory cohesive Rust module architecture

Crate boundaries may remain, but a large `src/lib.rs` is not the final internal architecture. For every nontrivial crate:

- `lib.rs` is the public façade: crate documentation, `mod` declarations, deliberate `pub use` exports, feature gates, and only minimal composition glue.
- Runtime loops, protocol decoders, state machines, queue algorithms, provider logic, persistence code, large fixtures, and platform-specific implementations belong in named modules.
- New unrelated behavior must not be appended to an already monolithic `lib.rs`.
- Split by responsibility and invariant, not by arbitrary line ranges and not into one file per type.
- Keep implementation details private by default. Preserve the established public API through explicit re-exports unless an approved breaking change is necessary.
- Avoid generic dumping grounds such as `utils.rs`, `common.rs`, `helpers.rs`, or a new oversized `mod.rs`. Name modules after owned concepts.
- Keep dependency direction acyclic. Domain modules do not import adapters, GPUI, operating-system APIs, cloud SDKs, or provider SDKs.
- Place integration tests under `tests/`; keep narrowly scoped unit tests beside their module; put shared fixtures/corpora in clearly owned testing modules or fixture directories.
- One module owns each state machine or invariant. Parallel modules must communicate through explicit types rather than reaching into each other's mutable internals.

A crate requires decomposition before receiving another major capability when any of these are true:

- `lib.rs` contains multiple independent responsibilities;
- reviewers cannot locate an invariant or ownership boundary without scanning unrelated code;
- fixtures and production runtime behavior are interleaved;
- platform/provider-specific code leaks into shared contracts;
- merge conflicts repeatedly occur in the same root file;
- `lib.rs` exceeds roughly 500 lines of implementation rather than façade/API material.

The 500-line value is a review trigger, not a reason to create meaningless fragments. After decomposition, a typical façade should remain well below that size. Cohesion and ownership are the deciding criteria.

### 1.5 Priority decomposition map

After current behavior is corrected and validated, decompose the largest crates in this order while preserving behavior:

| Crate/area | Required cohesive module direction |
|---|---|
| `crates/testing` | corpus/fixtures, ingest conformance, protocol assertions, replay/recovery scenarios, readiness/evidence, latency assertions, platform-specific harnesses |
| `adapters/market_protocol` | framing, decoder, canonical mapping, sequence/gap policy, timestamps/provenance, validation, errors |
| `adapters/stream_websocket` | configuration, framing, connection/session lifecycle, subscription/snapshot flow, backpressure, reconnect, metrics, errors |
| `ui/chart_integration` | Origin bridge, model generations, subscriptions, frame scheduling, input translation, metrics/errors |
| `realtime` | partition identity, fencing/ownership, bounded queues, latest state, fanout, snapshots, recovery |
| `application` | ports, commands/queries, use-case services, immutable snapshots, error model, feature-owned orchestration |
| `transport` | ingest driver contract, receive batches, capabilities/readiness, semantic delivery classes, lifecycle, errors |
| authorization/security | policy model, evidence, decision pipeline, entitlements, revocation/freshness, cryptographic boundary, audit types |
| each OS/accelerated adapter | configuration, capability probe, native resource ownership, receive/release lifecycle, statistics, shutdown, errors |

These names are architectural directions, not permission for blind file movement. Before moving a symbol, identify its responsibility, callers, invariants, visibility, and tests. Use language-aware moves/renames where possible, keep each change reviewable, and run targeted validation after each crate rather than attempting one repository-wide blind split.

### 1.6 Decomposition completion criteria

A crate's split is complete only when:

1. behavior and public contracts remain unchanged unless the change is explicitly approved;
2. `lib.rs` acts as a readable façade rather than the implementation body;
3. production code and fixtures have obvious, separate ownership;
4. each state machine, queue, decoder, and platform resource has one discoverable home;
5. module visibility is minimal and no cyclic dependency was introduced;
6. formatting, lint/type/build checks, targeted tests, and applicable conformance checks pass;
7. performance-sensitive movement does not add allocation, copying, locking, async hops, or dynamic dispatch without measurement;
8. the plan's baseline and readiness statements are updated if evidence changed.

No functionality credit is awarded for moving lines alone. Decomposition is a maintainability gate that enables safe continued implementation.

### 1.7 Progress tracking protocol

Section 20 is the single source of truth for tracked work. Every work item there is a checkbox with a stable identifier and an explicit status token. Do not duplicate progress claims elsewhere in this document.

Checkbox meaning:

- `- [x]` — the item is complete at its declared scope and its evidence is named in the status note.
- `- [ ]` — the item is not complete, regardless of how much code exists.

Status tokens qualify unchecked items and record how a checked item was proven:

| Token | Meaning |
|---|---|
| `done` | Implemented and validated at the declared scope; evidence named. |
| `done_pending_commit` | Implemented and validated locally, but not yet committed or pushed. |
| `partial` | Code or configuration exists, but the deliverable or its validation is incomplete. |
| `in_progress` | Actively being implemented right now. |
| `not_started` | No implementation exists. |
| `blocked_external` | Cannot progress without privileged hardware, paid provider access, commercial data, or a user decision. Name the blocker. |
| `superseded` | Replaced by another item; name the replacement identifier. |

Identifier format is `<stage>-<number>`, for example `S0A-03`. Identifiers are stable and never reused. New work appends a new number rather than renumbering existing items.

Rules for updating this file:

1. Change a checkbox only from evidence: source, passing commands, integration behavior, or recorded artifacts. Never from intention or from lines written.
2. Name the evidence in the status note, such as the command run, the fixture, or the artifact produced.
3. A contract, trait, fixture, probe, or harness alone never justifies `- [x]` for a behavior item. Apply Section 1.2.
4. If validation is impossible in the current environment, use `blocked_external` and say what is missing. Do not invent a pass.
5. If evidence is revoked by a dependency, driver, firmware, provider, or configuration change, uncheck the item and downgrade its status in the same change.
6. Update Section 1.1 and Section 1.8 whenever a status change alters the real baseline.
7. Keep status notes short. Detailed results belong in evidence artifacts, not in this plan.

### 1.8 Stage status summary

Each gate is `not_started`, `in_progress`, `blocked_external`, or `complete`. A gate becomes `complete` only when every item in its stage is checked and its exit criteria are satisfied.

| Stage | Gate | Status | Note |
|---|---|---|---|
| Stage 0A | Stabilize and correct the existing foundation | `complete` | Origin migration, workflow repair, DPDK downgrade, readiness reconciliation, vendored-patch provenance verification, privileged CI classification, and the privileged AF_XDP copy lifecycle including repeated queue rebinding are validated locally and on hosted runners. GitHub Actions run 30710769490 on `bcd7529` reports all eight checks green: `workspace_quality`, `linux_profile_compile_and_unprivileged_conformance`, `linux_af_xdp_generic_copy_veth_conformance`, `portable_windows-latest`, `portable_macos-latest`, `portable_ubuntu-latest`, `portable_cross_platform_evidence_gate`, and the automated review status. |
| Stage 0B | Cohesively decompose corrected monolithic crates | `complete` | All priority crates have cohesive private modules and façade-only roots. The portable, tuned Linux, AF_XDP, and DPDK adapters are 23-, 29-, 31-, and 24-line façades; the already-small macOS and Windows capability boundaries remain 26 lines. Public behavior is preserved and all local workspace validation is green. |
| Stage 1 | Complete cross-platform provider foundation | `in_progress` | Reusable bounded transport, canonical, recovery, credential-vault, platform, and UI contracts pass substantial tests. Historical AF_XDP/DPDK evidence remains recorded but is de-scoped from the product. Direct managed-provider adapters, per-OS installer activation, code-signature/notarization enforcement, and remaining platform ports are incomplete. |
| Stage 2 | Connected read-only market data | `in_progress` | The centralized Coinbase/Redpanda/S3/ClickHouse lanes prove reusable reference contracts but are not the chosen production topology. SQLite WAL, encrypted immutable history, bounded provider scheduling/handoff, deterministic worker-owned startup correctness, the direct Coinbase session driver, and bounded history-seeded live bar aggregation now compose behind one worker-local boundary, but no GPUI path drives it yet. Production requires direct desktop Rithmic/CQG sessions, provider certification, local rights enforcement, completion of `S2-19` integration, and the Section 16 evidence suite with no Axiusflow cloud market-data hop. |
| Stage 3 | Durable controlled execution | `not_started` | No OMS, risk, ledger, broker, or reconciliation runtime. |
| Stage 4 | Professional terminal and transport | `not_started` | Desktop remains a fixture/disconnected path. |
| Stage 5A | Optional cloud data and venue-adjacent scale | `blocked_external` | Starts only after commercial, provider-data-rights, compliance, funding, security, and measured-benefit decisions authorize the exact data capability; current releases do not depend on it. |
| Stage 5B | Optional managed execution | `blocked_external` | Starts independently only after broker/provider authorization, regulatory/custody, funding, security, recovery, and measured-benefit decisions authorize the exact execution capability; current releases do not depend on it. |

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

### 2.2 Four logical operational planes

The platform keeps four logical planes with independent ownership and failure policy, but a logical plane is not automatically a cloud service:

1. **Control plane** — Axiusflow identity, application authorization, users, organizations, workspaces, subscriptions, configuration, administration, and support.
2. **Local real-time data plane** — direct provider connection on the user's device, protocol decoding, sequence validation, normalization, partition-owned latest state, conflation, and local chart/workspace fanout.
3. **Execution plane** — current desktop-local order ingress, authorization evidence, pre-trade risk, crash-safe local intent, OMS state transitions, direct provider dispatch, ledger input, and reconciliation; any future managed execution is a separately gated topology.
4. **Optional durable/cloud data plane** — user-owned local replay plus, only in a separately approved future topology, PostgreSQL outbox/inbox, Redpanda, raw capture, S3/Parquet, ClickHouse projections, audit archives, and asynchronous workflows.

In the current phase, raw and normalized market data remain on the user's device unless an explicit provider agreement permits a narrowly defined upload. Control-plane or analytical work must not consume the bounded resources reserved for local real-time data or execution.

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

- The first-party Rust authentication service owns credentials and authentication sessions.
- The authorization domain owns application grants, policy evidence, and market-data entitlements.
- The OMS owns normalized order state.
- The portfolio ledger owns fills, cash movements, lots, positions, and financial journals.
- The instrument domain owns internal instrument identity and provider mappings.
- The workspace domain owns layouts, watchlists, and saved user configuration.
- The local provider session owns its connection, source sequence, and recoverable market snapshot.
- The local embedded transactional boundary owns workspace/catalog state and crash-safe desktop OMS/order intent in the current topology.
- PostgreSQL owns only the authoritative transactional records Axiusflow is approved to host.
- ClickHouse stores only approved rebuildable projections and owns no financial truth.
- Redpanda is a future/cloud durability option and is never required for the direct desktop display path.

Every ordered runtime partition also has exactly one active writer identified by `partition_id`, `owner_id`, and monotonic `ownership_epoch`. Handoff is fenced: a stale owner cannot publish accepted events after a newer epoch becomes active. Assignment, lease, handoff, recovery, and stale-writer rejection are observable and tested.

### 2.5 Stable internal contracts

Broker, exchange, vendor, GPUI, operating-system, cloud SDK, native provider SDK, and transport-library types stop at adapters. Internal commands and events use versioned Axiusflow contracts. The same canonical event must be produced whether it arrives through Rithmic/CQG WebSocket-Protobuf, a certified native provider SDK, deterministic replay, or a future approved Axiusflow cloud stream.

### 2.6 Replay is an operational capability

Provider order messages, business events, policy revisions, and calculation versions are replayable according to their authority and retention policy. Raw and normalized market data are recorded locally only when the provider agreement, exchange entitlement, user consent, retention limit, encryption policy, and deletion behavior explicitly permit it. A provider snapshot plus subsequent valid deltas reconstructs local display state; absence of recording rights is never treated as a recovery defect.

### 2.7 Direct-provider correctness and optimization order

The production correctness reference is a direct user-device provider session over the transport the provider certifies. For Rithmic Protocol and CQG Web API this is TLS WebSocket plus Protobuf; Rithmic R|API+ is a separate native-SDK adapter candidate. The desktop never substitutes a lower-level packet backend beneath a provider-controlled managed protocol.

Optimization proceeds in this order, with one measured change at a time:

1. choose the provider interface, endpoint, subscription level, and recovery semantics appropriate to the product;
2. keep network I/O, TLS, Protobuf decode, canonicalization, and model mutation off the GPUI thread;
3. bound every queue and assign semantic overflow behavior;
4. reuse receive/decode storage and reduce allocations or copies only where profiles show they matter;
5. partition model ownership and share immutable generations across charts;
6. align replaceable display work to the next frame while preserving every ordered or authoritative event;
7. tune worker count, affinity hints, batching deadline, allocator, socket options, and power mode independently on each operating system;
8. adopt a provider native SDK only when its correctness, packaging, safety, certification, and end-to-end measurements beat the portable protocol adapter.

DPDK and AF_XDP are not consumer acceleration profiles for managed TLS/WebSocket or provider-SDK sessions. They are removed from the active product roadmap. Historical adapter code and evidence may be retained temporarily for extraction or deletion, but portable release gates do not build, install, advertise, or depend on them. A future cloud data service may reconsider raw-packet acceleration only after Axiusflow has a contracted raw/multicast feed, dedicated qualified infrastructure, explicit redistribution rights, and evidence that tuned kernel sockets are the bottleneck.

Each active provider profile reports one ordered readiness state:

1. `implemented` — adapter, isolated build target, exact pins, and safety/license review exist.
2. `fixture_validated` — deterministic protocol, sequence, gap, replay, lifecycle, and error conformance passes.
3. `provider_certified` — the provider's required conformance suite and approved environment pass.
4. `live_validated` — authorized live sessions prove reconnect, recovery, sustained/burst behavior, and exact provenance without overstating transport timestamps.
5. `production_enabled` — security, operations, cross-platform packaging, provider, and performance gates all pass.

Unsupported activation fails explicitly. Silent fallback is forbidden in a claimed benchmark profile, while ordinary product fallback is permitted only when it is visible, semantically equivalent, and reported in the evidence artifact.

### 2.8 Measured claims only

A component microbenchmark cannot be marketed as end-to-end latency. A provider timestamp is not a local receive timestamp, and subtracting clocks with unknown offset produces an age observation rather than a latency measurement. Every performance result names:

- the start and end boundary;
- p50, p95, p99, p99.9, maximum, and sample count;
- warm-up and measurement duration;
- operating system and kernel/build;
- CPU, topology, power mode, memory, NIC, driver, queue layout, and GPU;
- network distance, RTT, loss, and transport;
- feed rate, burst shape, key distribution, client count, workspace, and chart count;
- overload, recovery, gap, conflation, and drop behavior;
- clock source, clock-domain relationship, synchronization uncertainty, and whether physical presentation was measured;
- commit, exact dependency lock, compiler flags, artifact schema, percentile method, and raw-result checksum;
- repeated-run variance and the baseline used for comparison.

Claims use boundary-qualified names such as `socket_receive_to_model_apply`, `model_apply_to_present`, or `input_to_provider_write`. Words such as “fastest,” “zero latency,” “exchange latency,” “tick-to-pixel,” or “click-to-trade” are prohibited unless the complete named boundary is actually instrumented and the comparison population is defined. A local optimization is promoted only when it improves the target tail under the production workload without increasing gaps, incorrect state, memory bounds, CPU/thermal instability, reconnect time, or frame loss.

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

### 3.2 No JavaScript or TypeScript runtime

Axiusflow has no first-party JavaScript or TypeScript runtime. Authentication is a first-party Rust service, so no Node.js, Bun, or Deno process exists in any deployable.

This supersedes the earlier Better Auth exception. Better Auth is TypeScript and requires a JavaScript runtime, a `package.json`, a JavaScript lockfile, and a transitive npm dependency tree inside the security perimeter. Authentication is the most security-sensitive boundary in the platform, so it does not receive the platform's only foreign runtime and only npm supply chain.

**Bun was evaluated as a substitute and rejected.** The rejection does not depend on Bun's implementation language, which has changed over time: Bun's repository is now predominantly Rust, while the 1.3.14 binary verified on the development host was still built from Zig sources. The decisive property is that Bun executes TypeScript on an embedded JavaScript engine. Replacing Node.js with Bun would retain TypeScript, the `package.json` manifest, a JavaScript lockfile, and the transitive npm dependency graph inside the authentication boundary. It removes no supply-chain surface, so it is not an improvement over Node.js for this purpose. Evaluate any future runtime proposal on its supply chain and audit surface, not on the language it happens to be written in.

The authentication service therefore owns credentials, sessions, and token issuance in Rust:

- password verification uses a memory-hard key-derivation function;
- OAuth/OIDC and native PKCE loopback flows use exact-pinned Rust clients;
- second-factor and passkey support use exact-pinned Rust implementations;
- credential, session, and refresh state is authoritative in PostgreSQL rather than a runtime-specific store;
- it issues short-lived Ed25519 tokens and publishes JWKS with an explicit key revision;
- Rust services continue verifying those tokens offline against immutable revisioned key snapshots through `crates/security`, which this decision leaves unchanged.

Candidate exact-pinned dependencies require the same safety, license, provenance, fuzzing, and maintenance review as any accelerated dependency in Section 3.1 before adoption: `argon2`, `oauth2`, `openidconnect`, `webauthn-rs`, and `totp-rs`. Naming a candidate is not integration evidence.

#### 3.2.1 `better-auth-rs` evaluation and fork strategy

`better-auth-rs` is the leading candidate to supply the authentication service. It satisfies this section's intent directly — Rust, no JavaScript runtime, no npm dependency graph — and its published surface already covers email/password, sessions, password and email-verification flows, OAuth, TOTP two-factor, passkeys, organizations with RBAC, admin, API keys, and device authorization, with Axum integration, SQLx PostgreSQL support, and MIT/Apache-2.0 dual licensing compatible with this proprietary workspace.

It is an independent reimplementation, not an official port, and it must never be described as one. Measured on 2026-08-01: the repository `better-auth-rs/better-auth-rs` describes itself as a "Rust backend implementation of @better-auth" under an organization distinct from official Better Auth; the crate `better-auth` is at `0.10.0` published 2026-04-12 with 5,619 total downloads; the last `master` commit was 2026-06-06 and was documentation only; and there are seven contributors, of whom one human holds 109 commits while two are automation accounts and four contributed one or two commits each.

The compatibility gap is the decisive adoption risk and must be re-measured before any commitment. The project claims compatibility with `better-auth@1.4.19`, while official Better Auth released `v1.6.25` on 2026-07-23 — roughly two minor versions and three and a half months of upstream change, including any security fixes in that window. Combined with the download count, this means Axiusflow would likely become the primary serious downstream consumer of a single-maintainer credential implementation and would inherit its maintenance burden rather than share it.

The approved strategy is to fork rather than depend directly, because a fork makes the upstream-divergence risk auditable instead of invisible:

1. Fork at an exact commit and pin by revision, matching the Origin Charts dependency pattern in Section 22.
2. Record the upstream Better Auth version the fork claims compatibility with, currently `better-auth@1.4.19`.
3. Perform a differential review of official Better Auth commits after that version. Security-relevant upstream fixes — session fixation, token replay, timing-sensitive comparisons, verification-link reuse, OAuth state and nonce handling, passkey challenge handling — are candidate defects in the Rust reimplementation until proven absent. Reproduce each as a failing test before fixing.
4. Treat this differential as continuing maintenance work, not one-time onboarding. Upstream security fixes land after adoption too, so the review repeats on a defined cadence and is a release gate for the authentication service.
5. Contribute non-differentiating fixes and benchmarks upstream where the license permits. Publishing improvements reduces long-term fork divergence; it is a maintenance strategy, not a product deliverable, and it never gates an Axiusflow release.

Two boundaries are non-negotiable regardless of adoption. First, the service owns credentials, authentication sessions, and token issuance only; application grants, RBAC, and market-data entitlements remain owned by the authorization domain per Section 2.4, so the organization and admin plugins must not become a second authorization authority. Second, credential, session, and refresh state stays authoritative in PostgreSQL under the outbox/inbox contracts in Section 11, never in a library-specific store.

Approval still requires the full `S2-14` review — provenance and maintenance signals, exact pin and checksum, license verification, and cryptographic review of password hashing, session invalidation, token issuance, and JWKS rotation — plus evidence that issued tokens verify unchanged through `crates/security`. That last check is the cheapest decisive test and is performed first. Until this evidence exists, composed exact-pinned primitives remain the fallback path, and no benchmark or comparison claim is published without the measured evidence Section 16 requires.

This is more implementation work than configuring Better Auth. That cost is accepted deliberately to keep one language, one supply chain, and one audit surface at the authentication boundary. Reintroducing a foreign runtime requires a new architecture and security decision recorded in this section, never a silent dependency addition.

One external boundary remains and is not first-party Node.js: Origin Charts publishes its browser package with npm tooling. The native desktop consumes only Origin's Rust crates and needs no JavaScript runtime. If a browser surface ships later, Origin's package build remains an external dependency's toolchain behind the exact-pinned dependency boundary and does not create a first-party JavaScript service.

#### 3.2.2 `better-auth-rs` review outcome (2026-08-03)

The full `S2-14` review of the leading candidate is complete and negative: `docs/decisions/2026_08_03_better_auth_rs_review.md` records the re-measured signals, the verified 0.10.0 checksum, and the decisive static finding that the crate issues HS256 JWTs and opaque session tokens with no EdDSA or JWKS path, so its tokens can never verify through `crates/security`. Adoption is rejected and the fork strategy in Section 3.2.1 is closed without forking. The authentication service proceeds with composed exact-pinned primitives, each reviewed before adoption per `S2-14`, and Ed25519 issuance with revisioned JWKS is first-party work on the existing `crates/security` contract.

References: [RFC 9106 Argon2](https://www.rfc-editor.org/info/rfc9106/), [RFC 7636 PKCE](https://www.rfc-editor.org/info/rfc7636/), [OpenID Connect Core](https://openid.net/specs/openid-connect-core-1_0.html), [W3C Web Authentication](https://www.w3.org/TR/webauthn-3/), [RFC 7519 JWT](https://www.rfc-editor.org/info/rfc7519/), and [RFC 7517 JWK](https://www.rfc-editor.org/info/rfc7517/).

---

## 4. System topology

```text
 Rithmic / CQG / approved provider or broker
      ▲ user-entitled market data, history, orders, and execution reports
      │ over certified WSS+Protobuf or native SDK
      ▼
┌──────────────── Axiusflow desktop: Windows | Linux | macOS ────────────────┐
│ OS credential vault → provider session → bounded frame/protobuf decode     │
│                                      ↓                                    │
│ sequence/gap recovery → canonical local partition → immutable latest state │
│                                                        ↓                   │
│ shared model generations → Origin ChartFrame → GPUI → physical display     │
│        ↕ local workspace/catalog + rights-gated history segment cache      │
│ input → local risk/OMS → crash-safe intent → direct provider order route   │
└──────────────────────────────────────┬─────────────────────────────────────┘
                                       │ asynchronous app control/sync only;
                                       │ no market-data or order-routing hop
                                       ▼
┌──────────────────── Current Axiusflow cloud ───────────────────────────────┐
│ auth, application licensing, signed updates, workspace/configuration       │
│ Raw quotes, DOM, provider credentials, and market-data replay are excluded.│
└────────────────────────────────────────────────────────────────────────────┘

Future optional topology — disabled until separately approved:

provider redistribution feed → Axiusflow cloud data plane → entitled clients
                             ↘ approved durable/replay/analytics systems
```

The current quote-to-pixel and input-to-provider-dispatch paths contain no Axiusflow cloud hop. Cloud and clients share versioned contracts and behavior, not market-data buffers, provider credentials, order/account payloads, local databases, or mutable memory. Existing centralized market-data, Redpanda, S3, ClickHouse, and streaming-gateway work remains reference evidence for the future optional topology; none is a dependency of the direct desktop release.

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

The current production topology deliberately minimizes custody of provider data:

1. `desktop` — owns the user's provider session, local normalization, latest state, chart fanout, rights-permitted history cache, local risk/OMS and crash-safe order intent, direct provider order routing, and reconciliation.
2. `auth_service` — first-party Rust authentication for Axiusflow accounts, application sessions, token issuance, and JWKS publication; it never receives provider credentials.
3. `control_plane` — Axiusflow licensing, users, asynchronous workspace/configuration sync, instruments, subscriptions, signed-update metadata, administration, and support; it never proxies market data or order traffic and is not on the application startup critical path.

`edge_stream_gateway`, `market_data_plane`, market-data Redpanda/S3/ClickHouse paths, and cloud `data_worker` are future optional deployables. Existing implementations are retained as conformance/reference work, not activated production dependencies. A future cloud data service must pass a new architecture decision covering provider redistribution rights, end-user entitlement enforcement, data residency, retention/deletion, operational cost, and a measured advantage over direct provider delivery.

The desktop opens from a locally durable workspace/configuration snapshot. Cloud sync and licensing refresh run asynchronously under a documented offline grace/expiry policy; a slow or unavailable control plane cannot produce a blank terminal, delay local chart hydration, or interrupt an already authorized direct provider session before that policy expires.

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
  streaming/                # optional future-cloud envelopes and consumers
  platform_runtime/         # OS capability and secure-storage abstractions
  observability/
  security/
  testing/
  adapters/
    market_protocol/
    portable_network/
    windows_network/
    macos_network/
    rithmic_protocol/       # direct WSS + Protobuf provider adapter
    cqg_web/                # direct WSS + Protobuf provider adapter
    rithmic_rapi/           # optional certified native-SDK boundary
    linux_socket_network/   # historical/dedicated-host experiments, not desktop runtime
  ui/
    terminal_ui/            # GPUI compatibility boundary
    design_system/
    chart_integration/
services/
  auth_service/
  control_plane/
  edge_stream_gateway/      # future cloud-data topology only
  market_data_plane/        # current reference vertical; future cloud topology only
  execution_plane/
  data_worker/              # future cloud-data topology only
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
direct provider WSS/native callback
  → bounded frame decoder
  → Protobuf/provider message decode
  → sequence and schema validation
  → provider-neutral canonical mapping
  → partitioned local model writer
  → immutable generation publication
  → one merged UI command per frame
  → Origin incremental update
  → ChartFrame build
  → GPUI submission
  → physical presentation
```

Rules:

- Network I/O, decode, decompression, and model updates never run on the GPUI thread.
- Provider credentials remain in the operating-system credential vault and are presented only to the provider endpoint from the user's device.
- Raw provider messages, DOM, account data, and orders never enter Axiusflow telemetry or cloud services.
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
| `balanced` | Windows, Linux, macOS | Direct provider session, event-driven networking, normal power use, adaptive refresh. |
| `low_latency` | Windows, Linux, macOS | Direct provider session, dedicated decode/model workers, tighter batching deadlines, higher refresh, and increased power use. |
| `provider_native` | Provider- and OS-specific | Certified native SDK behind the same canonical contract; selected only when packaging, stability, and end-to-end evidence beat the protocol adapter. |

A user selecting `low_latency` receives a warning about power and thermal cost. Busy polling, real-time scheduling, NIC rebinding, AF_XDP, and DPDK are never enabled by the consumer product.

### 6.6 Browser companion

The browser uses Rust/WASM for product logic and Origin WebGPU with Canvas2D fallback. It shares Protobuf contracts, domain value types, formatting, entitlement behavior, chart semantics, indicator formulas, and workspace serialization. It does not need to share every native presentation component.

### 6.7 Local-first startup, history, and cache architecture

Startup is a staged local pipeline, not one blocking initialization transaction:

```text
process launch
  ├→ validate local schema/manifest → paint shell and last workspace
  ├→ open OS vault → connect/authenticate provider → restore subscriptions
  ├→ hydrate visible chart range from local cache or provider history
  ├→ prepare visible indicators and render generations
  └→ refresh license/config and sync workspace asynchronously

cached history → provider snapshot/backfill → contiguous live sequence
```

The shell, docking layout, watchlists, last symbols, and locally cached visible chart ranges load without waiting for Axiusflow cloud, provider authentication, every indicator, every historical range, or every workspace tab. Provider connection, cache validation, visible-range hydration, and non-visible workspace preparation run concurrently on bounded background workers. The GPUI thread performs no filesystem, network, decompression, decode, database, or indicator computation.

`crates/provider_history` supplies the worker-side capability and scheduling boundary for this pipeline. It validates separate bars/ticks/depth support, lookback, span, page size, pagination, rate, and concurrency before dispatch; deduplicates exact work across consumers; prioritizes visible then adjacent ranges; uses monotonic time for quota windows; reports expired queued work and bounded terminal fetch failures; and fails closed on malformed pages or non-contiguous live handoff. Deterministic fixtures exercise the shared contract today. The Coinbase profile remains fail-closed until its desktop fetch adapter exists, while the separately authorized reference feed continues supporting broader platform tests. Rithmic/CQG profiles and provider calls are added only after partnership materials and rights arrive; their absence does not stop the local runtime, cache, charting, replay, startup, UI, OMS/risk, or performance-evidence work.

The desktop storage hierarchy is:

1. **Transactional local catalog** — workspace metadata, schema and migration state, cache keys/manifests, provider recovery checkpoints, and local OMS/order-intent state use the exact-pinned SQLite WAL/FULL decision from `S2-22`. `crates/desktop_storage` implements the history-manifest slice; workspace, checkpoint, and OMS repositories remain separate later integrations rather than implied completed behavior.
2. **Immutable history segments** — `crates/desktop_storage` now publishes checksummed, XChaCha20-Poly1305-encrypted, versioned files only where provider rights permit retention. A keyed segment identity includes provider, account/entitlement class, instrument, data kind, resolution, time range, session calendar, adjustment policy, and source/schema/correction revisions. The opaque market payload codec remains independently selectable. File sync and immutable linking precede the SQLite manifest commit, so a partial file is unreachable and an orphan is removed during recovery.
3. **Bounded memory cache** — current books/bars, visible and adjacent chart ranges, decoded shared model generations, and render-ready level-of-detail data under explicit item and byte budgets. Multiple charts reuse these generations instead of decoding or calculating the same history independently.

Market history does not become transactional metadata rows merely for convenience, and the catalog does not become an unbounded tick store. Hot/visible segments may remain uncompressed when measurement favors page-cache locality; colder eligible segments may use independently decompressible blocks after measuring load time, size, CPU, and memory. The exact segment codec and index are chosen by a recorded spike, not by fashion.

Hydration is useful-work-first:

- paint validated cached pixels or data for the visible time range first, then adjacent ranges, wider history, non-visible tabs, and expensive indicators;
- request only missing or invalid provider ranges through a bounded, deduplicating scheduler that respects provider pagination, concurrency, and rate limits;
- build coarse-to-fine or level-of-detail render data so pan, zoom, symbol, and timeframe changes produce a useful frame before full-resolution background work finishes;
- incrementally update indicators from versioned inputs and cache only rights-permitted derived results keyed by formula/engine version and parameters;
- atomically hand off cached history to provider snapshot/backfill and then live sequence with no gap, duplicate, flicker, timestamp regression, or partial visible mutation;
- never restore stale DOM, account, position, or live quote state as current. Cached market data is visibly marked with source and freshness until verified handoff; order entry remains fail-closed until provider account/risk/order reconciliation completes.

Cache invalidation is explicit. Entitlement loss, account change, schema migration, corporate action, session-calendar or adjustment-policy revision, provider correction/source change, checksum failure, or secure-deletion request invalidates the affected scope. Corruption quarantines only affected segments and falls back to provider re-fetch or live-only behavior. If retention is forbidden, the same runtime operates with an ephemeral memory cache and provider history; it never hides persistence or uploads data to compensate.

Redis, PostgreSQL, ClickHouse, S3, Redpanda, and every intermediary Axiusflow-managed or unrelated backend are forbidden in desktop startup, workspace restoration, chart hydration, live display, and the order data path. Direct order routing has two required external data-path dependencies: the certified provider/broker endpoint is the destination, and the independently user-controlled recovery target certified under Section 9.2 supplies the declared durability acknowledgement. Where the provider cannot enforce an exclusive trading session, the opaque Axiusflow lease service defined in Section 9.1 is a permitted conditional authorization dependency: a fresh lease plus provider-enforced account-wide risk may enable trading, but the service never receives or forwards order, account, position, fill, or market-data payloads and is never queried per order. None of these dependencies is an Axiusflow market-data/order gateway, and neither the recovery target nor lease service is a market-data dependency.

---

## 7. Real-time market-data plane

### 7.1 Ingest profiles

All active profiles terminate at the same provider framing and canonical decoder contracts. They differ by provider-supported interface, not by attempts to replace the provider's transport underneath it.

| Profile | Intended deployment | Characteristics |
|---|---|---|
| `provider_websocket` | Windows, Linux, macOS desktop | Provider-certified WSS/TLS, Protobuf, event-driven OS networking, bounded decode and reconnect state. |
| `provider_native` | Provider-supported desktop targets | R|API+ or an equivalent native SDK behind an isolated C ABI/process boundary and the same canonical contracts. |
| `deterministic_replay` | Tests and rights-permitted local diagnostics | Versioned recorded/synthetic messages with controlled cadence, bursts, faults, and expected canonical output. |
| `cloud_stream` | Future optional product only | Axiusflow streaming protocol; unavailable until the separate cloud-data gate passes. |

The active receive port exposes complete provider frames/messages, receive time, bounded ownership, overflow, reconnect state, and health. Provider-specific callbacks and Protobuf objects are consumed inside the adapter. Accepted canonical events are materialized into partition-owned bounded storage; no SDK pointer, borrowed frame, TLS object, or provider-generated type crosses a task, queue, or canonical boundary.

Every provider adapter runs the same canonical value, sequence/gap, snapshot, reconnect, malformed-input, overload, and chart-state corpus. Platform-specific optimization is permitted behind the adapter—IOCP on Windows, evented sockets on Linux, and supported Apple networking facilities on macOS—but the provider's certified protocol and TLS requirements remain unchanged.

DPDK and AF_XDP are explicitly inactive for the current topology. They cannot accelerate Rithmic Protocol or CQG Web API beneath managed WSS/TLS and would impose consumer NIC, privilege, power, compatibility, and support risk. `io_uring` is not called kernel bypass and is evaluated only if a measured Linux WebSocket/TLS workload exposes a syscall bottleneck that the selected Rust transport can actually address.

### 7.2 Canonical ingest path

```text
provider WSS frame or native SDK callback
  → protocol decoder
  → source-sequence validator
  → pre-resolved instrument mapping
  → canonical fixed-layout event
  → desktop-local single-writer partition
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
- ownership_epoch (desktop-local session generation in the current topology)
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

### 7.3 Local fanout and rights-gated replay

One accepted canonical event advances local latest state before any optional secondary work:

```text
                           ┌→ latest state/conflation → charts/books/workspaces
local canonical partition ┤
                           └→ optional encrypted local replay, only if licensed
```

The display branch never waits for disk or cloud acknowledgement. Optional local recording has an independent bounded queue, explicit retention and deletion policy, encryption, completeness status, and a provider-rights gate. If recording is unavailable or prohibited, live display continues and the product does not imply replay availability.

Both permitted branches preserve the same event ID, source sequence, ownership epoch, timestamps, provenance, and quality flags. Uploading either branch to Axiusflow infrastructure is forbidden in the current topology.

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

If the local model or UI is slow:

- `state_replace` updates are conflated to the newest entitled value;
- independent subscription classes receive independent budgets;
- ordered streams trigger resnapshot rather than unbounded buffering;
- the provider adapter may reduce non-authoritative display work, request a new snapshot, or reconnect according to the certified protocol;
- authoritative account events are never silently dropped.

### 7.6 Local capture rights and data quality

Raw source capture is disabled by default. When the provider agreement and user policy permit it, capture runs on an independently bounded path to encrypted local segments with retention and secure deletion. Failure to capture is observable and creates an explicit local evidence gap. It cannot block the decoder indefinitely or be silently represented as complete history. Cloud upload and S3 capture are future-cloud capabilities and remain disabled.

Each provider profile declares separate cache rights and capabilities for bars, ticks, market depth, derived data, order/account evidence, maximum lookback, pagination, correction behavior, rate limits, retention, export, and secure deletion. Cache eligibility is checked before every persistent dataset class, not inferred from the ability to display it. Account and entitlement scopes cannot share segments unless the provider contract explicitly permits it. Data whose storage right is revoked, forbidden, or retention-expired follows the provider's required secure-deletion policy and is never quarantined. Quarantine is reserved for corrupt data whose retention rights remain valid. The runtime re-fetches permitted provider history or runs live-only rather than silently retaining prohibited data.

Quality checks include sequence gaps, timestamp regression, invalid scales, crossed/locked policy, impossible OHLC relationships, duplicate event IDs, venue/session mismatch, corporate-action discontinuity, and provider divergence.

### 7.7 Deterministic bars and derived data

Bars and derived market series are deterministic functions of versioned source sets, session calendars, adjustment policy, and correction rules. Late data creates correction events; it does not invisibly rewrite history.

---

## 8. Client and internal transport architecture

### 8.1 Named transport profiles

| Profile | Use | Required availability |
|---|---|---|
| `control_https` | REST/JSON and typed control operations over HTTPS/HTTP/2 | All clients |
| `provider_websocket` | Provider-mandated TLS WebSocket carrying provider Protobuf messages directly to the user's device | Every OS supported by that provider profile |
| `provider_native` | Certified native provider SDK callback transport | Optional where supported and proven |
| `stream_websocket` | Future Axiusflow cloud snapshots and deltas | Disabled in the current market-data topology |
| `stream_quic` | Future Axiusflow QUIC reliable streams plus negotiated datagrams | Disabled until cloud-data certification |
| `stream_webtransport` | Future browser cloud-stream capability | Disabled until cloud-data certification |
| `internal_rpc` | Protobuf gRPC over HTTP/2 with mTLS | Internal synchronous operations |
| `internal_realtime` | Future private link between cloud partition owners and gateways | Future dedicated data-plane deployments only |

The provider-certified interface is authoritative for direct sessions. Axiusflow does not negotiate QUIC in place of a provider's WSS/TLS protocol. The existing binary WebSocket and QUIC work applies only to a future Axiusflow cloud stream and can be activated only when the route, rights, implementation, firewall behavior, loss handling, and operating-system matrix pass their gates.

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
- **Linux:** evented sockets are the default. `epoll`/runtime configuration, buffer sizing, TLS behavior, batching deadlines, worker affinity hints, and `io_uring` are evaluated separately and only from measured provider-session evidence. AF_XDP and DPDK are excluded from the consumer runtime.

No OS-specific adapter changes application framing or domain semantics.

### 8.4 Internal RPC

Synchronous internal operations use Protobuf gRPC over HTTP/2 with deadlines, mTLS workload identity, bounded safe retries, circuit breaking, concurrency limits, and propagated actor/correlation/causation context.

Redpanda is not used for request/reply, pre-trade risk RPC, or client subscription control.

### 8.5 Encoding and copy budget

- No JSON in market-data, execution, or internal event hot paths.
- Protobuf remains the durable and control contract.
- A compact versioned binary client frame may wrap or specialize generated payloads after compatibility tests.
- The provider adapter decodes once into canonical local state; multiple charts consume shared immutable generations rather than repeating provider decode.
- Scatter/gather and registered buffers are used only when they reduce measured copies without extending lifetimes unsafely.
- Every benchmark reports allocations and bytes copied per message stage.

---

## 9. Execution plane

### 9.1 Current desktop execution cell

In the current product phase, one desktop-local execution cell owns each active provider account session. It colocates hot-path modules while preserving logical contracts and routes directly to the provider or broker API:

```text
desktop input or strategy command
  → local session, entitlement, and policy validation
  → idempotency and instrument validation
  → local pre-trade risk using reconciled immutable snapshots
  → crash-safe local OMS intent transaction with stable client order ID
  → direct Rithmic/CQG/broker session dispatch
  → provider acknowledgement or unknown outcome
  → normalized local OMS/ledger event
  → provider order, fill, position, balance, and risk reconciliation
```

The Axiusflow control plane is not synchronously queried per order and never receives the order, account, position, or fill payload in this topology. Provider credentials stay in the OS vault. Authorization policy, account permissions, instrument rules, positions, buying power, limits, and market-health inputs are preloaded as bounded versioned snapshots and reconciled against the provider before trading is enabled. Missing, expired, stale, or divergent required state fails closed.

One account has one active Axiusflow trading session generation only when an external authority can enforce that claim. The preferred authority is a provider-enforced exclusive trading session. If the provider permits concurrent sessions, an Axiusflow control service may issue a short-lived opaque cross-device lease containing only a pseudonymous account binding, device/session identity, epoch, and expiry—never provider credentials, orders, positions, fills, or market data. That lease coordinates Axiusflow instances only; it does not pretend to fence unrelated provider applications. Live ordering in this concurrent-session profile additionally requires provider/broker-enforced account-wide hard risk limits that cover every writer and remain authoritative when local snapshots race external activity. Without provider exclusivity or that provider-enforced risk boundary, the account remains market-data/read-only even when an Axiusflow lease is fresh. A control outage never blocks shell or chart startup, but may block new orders or lease renewal where the provider supplies no equivalent fence.

Reconnect, restart, suspend/resume, or a second device cannot blindly replay a command: stable client order IDs, provider correlation IDs, authoritative session generations, explicit retry classes, unknown-outcome states, and provider reconciliation determine whether dispatch is safe. Orders entered through other provider applications are external authoritative activity and must be incorporated by reconciliation rather than rejected as impossible. Server-side provider risk and order types remain authoritative where contracted; local controls add protection but never claim to replace them.

### 9.2 Durable acceptance definition

`accepted_locally` means the normalized intent and provider client order ID are committed to the crash-safe local transactional boundary and can be recovered after process loss. In a live profile it is not reported until the independent recovery commit below is also acknowledged. It does not mean the provider or venue accepted the order. The user-facing state distinguishes local acceptance, dispatch, provider acknowledgement, execution, and reconciliation without collapsing them into one optimistic status.

Live trading also requires a declared device-loss recovery profile. Every journal record has a stable ID and explicit `prepared` and `committed` phases. For a new intent, the runtime commits a local prepared record, appends and syncs the encrypted prepared record to the independent user-controlled target, commits the local `accepted_locally` transition, then appends and syncs the independent commit marker. Only after that final acknowledgement may the UI report local acceptance or the runtime dispatch to the provider. A crash after independent prepare but before local acceptance leaves a remote prepare that restore treats as never accepted and never dispatches. A crash after local acceptance but before the independent commit marker cannot have dispatched; same-device recovery may idempotently finish or abort the marker, while total-device-loss recovery treats the uncommitted remote prepare as not accepted. Every decision is resolved by stable record/client-order IDs plus provider reconciliation, never by guessing from timing.

The encrypted record contains the intent, policy/risk revision, OMS transition, provider correlation, and ledger input. During normal operation and while certified degraded capacity remains, every subsequent authoritative acknowledgement, rejection, fill, cancel/replace transition, provider correction, reconciliation result, and ledger input uses the same ordered prepared/committed journal semantics in both boundaries. Provider-authoritative observations always update the live in-memory OMS, ledger, position, and risk views immediately, including while recovery acknowledgement is pending, so safety decisions never use deliberately stale exposure. Until the independent commit marker is acknowledged, the transition is marked `recovery_pending` and is not represented as independently recoverable or published into downstream stores that claim recovery durability. The UI may show that explicit durability degradation but cannot mislabel the observation as durably recovered state.

The target may be a separately mounted user-owned volume, NAS, or customer-controlled storage integration only after its atomicity, ordering, idempotency, sync, availability, credential, and restore semantics pass certification; a second file on the same failure domain is not sufficient. Encryption uses a user-held recovery secret that survives loss of the device vault. Axiusflow services never receive the plaintext, ciphertext, storage credential, or object location.

The `local_only` recovery profile is limited to paper/shadow operation because total device loss can erase intent and policy evidence that provider history cannot reconstruct. If the independent recovery target becomes unavailable, new live orders and exposure-increasing replaces fail closed. The recovery-degraded safety path permits only provider-enforced atomic reduce-only/close-position actions and cancellation of specifically identified exposure-increasing orders under provider semantics; locally inferred reductions, blanket cancellation that may remove protective orders, and ordinary liquidation orders fail closed. Permitted actions and existing provider activity are recorded in the local journal and bounded ordered recovery backlog while the UI declares recovery degradation. At a certified high-water mark, the runtime invokes only a provider-certified kill/flatten control whose documented semantics cannot increase exposure or silently remove required protection, rejects every other command, and consumes a preallocated emergency journal reserve sized and tested against the maximum outstanding activity allowed by the provider risk boundary. If the provider lacks those atomic safety semantics, the degraded profile disables all local order submission and relies on the provider's independent risk controls and supported external emergency channel. If the reserve nevertheless exhausts or an unbounded provider stream makes such sizing impossible, the runtime continues applying provider truth to live risk, records an explicit evidence-discontinuity marker if durable local space remains, disables local order submission, and requires provider-history reconciliation before trading resumes; it does not claim a complete local transition history. A provider profile that cannot bound outstanding activity or supply authoritative recovery history cannot certify recovery-degraded live trading. Total device loss during any acknowledged recovery outage creates an explicit non-zero-RPO evidence gap and is outside the certified zero-loss profile.

Restore verifies the encrypted journal and rebuilds local OMS/ledger state from committed records only. Orphan prepares are retained as evidence but never become accepted orders or dispatch commands; provider history determines whether a provider-authoritative lifecycle event must be reconstructed. Restore then reconciles every order, fill, position, balance, and provider-side risk control before live trading resumes. Backup/restore rights, retention, secure deletion, key-loss behavior, and jurisdiction are explicit; no zero-RPO claim exists until pre-dispatch intent, continuous lifecycle replication, every prepared/committed crash boundary, recovery-target outage handling, and destructive restore drills prove it.

States distinguish at least:

```text
received → validated → risk_approved → route_pending → submitted
         ↘ rejected                    ↘ unknown_outcome
submitted → acknowledged → partially_filled → filled
submitted → cancel_pending → canceled
submitted → replace_pending → acknowledged
```

A timeout never becomes `rejected` without provider evidence.

### 9.3 Low-latency direct execution without correctness loss

- Hot-path modules run inside the desktop execution runtime; no Axiusflow network hop is added before provider dispatch.
- Prepared statements, a warm provider session, preallocated command storage, and prevalidated immutable snapshots remove avoidable work.
- Provider dispatch begins immediately after the required durable commit.
- Analytics, cloud sync, notification, and optional audit export are asynchronous to provider dispatch.
- Provider-native server-side brackets, OCOs, trailing stops, and risk controls are preferred when their documented semantics protect the user across desktop disconnects.
- Retail aggregation adapters are never marketed as institutional low-latency execution.
- The local transactional store and journal mode are selected only after crash, corruption, disk-full, fsync, recovery, and latency evidence. Send-before-durable is not an optimization option.

### 9.4 OMS, ledger, and reconciliation

Provider messages are inputs to the OMS state machine, not direct row mutations. Invalid transitions are quarantined. The immutable portfolio ledger owns fills, cash, fees, taxes, lots, positions, corrections, and reconciliation evidence. Corrections append compensating entries.

Reconciliation independently compares internal orders, fills, positions, cash, fees, and provider snapshots. A live stream is never treated as proof of completeness.

### 9.5 Execution tiers

- **Direct desktop execution:** current product; the user's device routes through the certified provider/broker API with local durable intent and reconciliation.
- **Provider-hosted controls:** provider-side risk and server-side order features used under their documented contract; no Axiusflow infrastructure required.
- **Future managed or venue-adjacent execution:** optional dedicated infrastructure considered only after broker/provider authorization, regulatory and custody review, sustainable funding, security design, disaster recovery, and measured user benefit.

Axiusflow does not claim that an internet-connected desktop is an HFT colocation engine. The provider and WAN dominate external latency, while Axiusflow owns and measures local input-to-durable-intent, durable-intent-to-provider-write, callback-to-model, and presentation boundaries. A future execution topology may reuse the same OMS contract but cannot become a dependency of the direct desktop product without a separate governing decision.

---

## 10. Control plane, identity, and authorization

### 10.1 Edge and control responsibilities

The public edge handles TLS, request limits, JWT verification, session/device context, request IDs, deadlines, protocol negotiation, idempotency headers, rate limits, and coarse routing. The current direct-provider desktop has no Axiusflow streaming or execution ingress. Dedicated regional ingress for professional streaming or managed execution may be introduced only after the applicable future Stage 5 product gate passes and the same identity and policy evidence has been established.

The control plane owns users, organizations, teams, subscriptions, instrument reference data, feature policy, exports, administration, the synchronized copy of workspaces/watchlists/configuration, and—only where a provider lacks exclusive sessions—opaque short-lived Axiusflow trading-session leases. A lease carries no provider credential or trading payload and is acquired outside the per-order path. The desktop owns the locally durable startup copy and can restore it without a control-plane round trip. Conflict-aware cloud sync is asynchronous and bounded. Large imports/exports are object-store jobs.

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

This section specifies the future optional cloud-data topology and Axiusflow-owned transactional events for separately approved future capabilities. It is not part of the current direct desktop data or order paths. Market-data topics, capture, and projections remain disabled until Stage 5A explicitly authorizes them; managed order, execution, ledger, and position events remain disabled until Stage 5B explicitly authorizes them. Approval of either event family does not enable the other.

### 11.1 Redpanda role

When an optional Stage 5 capability is approved, Redpanda may be its durable event backbone only for the event families authorized by that capability's gate. Stage 5A may enable licensed cloud market-data families; Stage 5B may independently enable managed order, execution, ledger, and position families. Topics, credentials, ACLs, retention, quotas, and preferably clusters are isolated by capability so one approval cannot leak payloads into the other. The backbone provides partitioned retention, schema-governed events, replay, asynchronous workflows, and projection input. It is not:

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

These cloud partition contracts apply only to event families whose owning future product gate has passed. Market-source partitions require approval of the optional cloud-data product; order, execution, ledger, and position partitions require approval of future managed execution. The current direct-provider desktop does not publish those payloads to Axiusflow cloud. Workspace and entitlement events remain limited to the asynchronous product/control plane.

When enabled, partition keys reflect correctness:

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

Identity and product PostgreSQL ownership boundaries support the current asynchronous control plane. A trading PostgreSQL cluster does not exist in the direct-provider desktop topology and may be introduced only after the future managed-execution gate passes. In that separately approved topology, identity, product, and trading ownership boundaries provide access and failure isolation; the managed OMS and ledger may use explicitly owned schemas in the trading cluster and shared transactions only for documented financial invariants.

Clusters require encryption, private networking, TLS, bounded pooling, query deadlines, backups, point-in-time recovery, tested restore, and cross-region disaster-recovery policy.

### 11.5 ClickHouse

ClickHouse stores rebuildable analytical projections: selected market history, bars, order/execution analytics, P&L series, trade groupings, alerts, and operational aggregates. One Rust sink consumes durable events and writes idempotent batches with event ID and projection version. Services do not directly write arbitrary analytical tables.

Reference: [ClickHouse Rust integration](https://clickhouse.com/docs/integrations/rust).

### 11.6 S3 and Parquet

Where separately licensed and approved, S3 may store cloud raw-feed captures, normalized archives, and bars. It may also store Axiusflow-owned broker evidence, immutable business-event archives, reports, backtest inputs/results, artifacts, and audit exports. Objects include checksum, schema, provenance, capture version, rights classification, retention policy, and encryption metadata. Object Lock is used where retention policy requires it.

### 11.7 Redis

Redis may hold rate limits, secondary session cache, short-lived authorization cache, presence, and stream-resume metadata. It is never the only record of a user, order, fill, balance, entitlement, workspace, or session revocation history.

---

## 12. Analytics, strategies, alerts, and product workflows

Analytics consumes ledger and market events and creates rebuildable versioned projections. A "trade" is derived by a declared grouping policy; it is not assumed to equal one fill. Reprocessing creates a new projection version and does not rewrite the ledger.

Built-in indicators are incremental Rust components. Untrusted custom indicators and strategies run as capability-restricted WebAssembly components with bounded data windows and no ambient file, network, or credential access. Calculation permission and live execution permission are separate.

Backtests record data version, corporate-action policy, fees, slippage, calendar, strategy artifact, and random seed.

During the direct-provider phase, market alerts run locally and are explicitly labeled device-dependent: they cannot fire while the desktop or provider session is offline. Reliable server-side market alerts require the future cloud-data rights and topology gate. Both forms use deduplication, cooldown, delivery state, and declared audit/retention behavior.

---

## 13. Provider and licensing architecture

Providers are adapters, not architecture. A provider-specific type cannot become an order, instrument, market, chart, or portfolio domain type.

The current product uses user-entitled direct sessions. Rithmic Protocol API and CQG Web API are WSS/TLS plus Protobuf candidates; Rithmic R|API+ is a native-SDK candidate behind a separately reviewed adapter. Provider credentials are created or authorized by the user, stored in the operating-system credential vault, and sent only from that device to the provider endpoint. Axiusflow cloud services neither terminate the provider session nor receive those credentials.

Broker breadth may begin with an aggregation provider, while certified native adapters are evaluated for richer semantics or lower measured local overhead. FIX, certified broker/FCM gateways, and future venue-adjacent deployments use the same OMS contract without forcing their topology onto the desktop market-data path.

Market sources may include Databento for serious US market coverage, dxFeed for broader enterprise coverage, and direct venue streams where licensed. News, economic, earnings, dividend, and corporate-action datasets require separately licensed providers.

Data agreements distinguish display, non-display, derived, delayed, real-time, professional, non-professional, local storage, internal distribution, and redistribution. Provider-side user entitlements are not replaced by an Axiusflow subscription. Rights are checked before local recording, export, replay, alert evaluation, algorithmic use, or any future cloud upload.

Broker data is not assumed redistributable, uploadable, or retainable. Direct-to-user delivery reduces Axiusflow's data-routing scope but does not remove provider certification, exchange agreements, display rules, professional/non-professional classification, or jurisdiction obligations. Commercial rights and legal review remain launch gates. References: [Rithmic API interfaces](https://www.rithmic.com/apis), [CQG Web API](https://partners.cqg.com/api-resources/web-api), and [IBKR API market-data restrictions](https://www.interactivebrokers.com/en/index.php?f=1538&p=api).

---

## 14. Current infrastructure and future cloud profiles

### 14.1 General topology

- The user's desktop and provider endpoint form the current market-data path; no Axiusflow cloud host is in that loop.
- The user's desktop and provider/broker endpoint form the current order path; no Axiusflow cloud host is in that loop.
- Axiusflow cloud infrastructure runs only approved control-plane and ordinary product services during this phase.
- Provider credentials, raw/normalized market data, orders, account state, positions, and fills are excluded from cloud logs, traces, crash reports, object storage, queues, and analytics unless a later separately authorized capability explicitly changes one payload class.
- Future streaming gateways, feed handlers, market-data Redpanda families, S3, ClickHouse, and associated dedicated hosts remain disabled until the Stage 5A cloud-data gate passes. Managed-execution Redpanda families and execution hosts remain independently disabled until the Stage 5B execution gate passes.
- A future cloud topology may use managed compute, dedicated hosts, or colocation only after rights and measurement select them; Kubernetes is never placed in an innermost latency loop merely for consistency.

### 14.2 Consumer performance profiles

A named desktop performance profile records:

- operating system/build, CPU, logical-core topology, memory, GPU, display, NIC/Wi-Fi, and power source;
- provider interface, endpoint region if disclosed, transport, TLS, compression, subscriptions, depth, and message/burst rate;
- worker count, affinity/priority hints, queue bounds, batching deadline, allocator, and runtime configuration;
- CPU governor/power mode, thermal state, background-load profile, and whether the device is charging;
- receive, decode, canonical apply, Origin build, GPUI submit, presentation, reconnect, and gap-recovery measurements.

The default profile remains event-driven and battery-safe. A low-latency profile may dedicate ordinary worker threads and spend more power after measurement, but never binds a NIC away from the OS, requires huge pages, installs VFIO/UIO, or silently busy-spins.

### 14.3 Clock synchronization

Local stage measurements use one process monotonic clock. Provider/exchange timestamps remain separate clock domains unless the provider documents their semantics and the measurement records bounded synchronization uncertainty. Future dedicated feed and execution hosts may use PTP-capable infrastructure where available: [Linux PTP clock documentation](https://docs.kernel.org/driver-api/ptp.html).

Cross-host latency is not calculated by subtracting unsynchronized wall clocks. Reports include clock source, synchronization method, maximum observed offset, and uncertainty.

### 14.4 Future optional multi-region ownership

The current desktop topology has no Axiusflow home execution region. If managed execution is separately authorized later, each trading account has one home execution region and one active execution owner. Reads and product APIs may be globally active; order and ledger writes are routed to the owner. Disaster recovery promotes ownership through a fenced epoch and requires provider reconnection plus reconciliation before unrestricted trading resumes.

### 14.5 Infrastructure as code

Terraform and reviewed versioned configuration define the current control-plane infrastructure. Networking, IAM, KMS, databases, Redpanda, ClickHouse connectivity, host profiles, DNS, and disaster recovery for optional future data or execution services are added only after their governing gate passes. Production profile changes require a plan, policy checks, benchmark comparison, rollback, and audit evidence.

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
- Locality is measured before affinity is introduced; consumer builds do not assume one NUMA topology.
- Payloads are decoded once and transformed only when contract boundaries require it.

### 15.4 Scheduling

The selected portable evented runtime owns connection orchestration; the provider adapter does not introduce a second runtime without evidence. Dedicated decode/model workers and affinity hints may be used when measured scheduler jitter violates the low-latency profile. CPU-heavy analytics use separate pools. Blocking file/database work never runs on network or ordered-model workers.

Busy polling and real-time scheduling are disabled in consumer builds. The low-latency mode may shorten bounded batching/poll deadlines and dedicate ordinary threads, but it must remain revocable on thermal, suspend, battery, or foreground-state changes.

### 15.5 Observability overhead

Hot loops use preallocated histograms, per-partition counters, and sampled traces. A trace span or formatted log is not created for every market tick. Financial and security audit records remain complete through their durable path.

### 15.6 Optimization promotion ladder

Each optimization lands separately with before/after Section 16 artifacts and a rollback path. The preferred sequence is:

1. eliminate duplicate subscriptions, decoding, canonicalization, and per-chart state;
2. remove GPUI-thread network/decode work and reduce UI invalidations to one merged update per frame;
3. pre-resolve symbols and scales, reuse receive/Protobuf scratch storage where supported, and remove proven allocation/copy hot spots;
4. tune bounded channel topology, worker count, batching deadline, socket buffers, TLS/session reuse, and provider-supported compression independently;
5. evaluate release LTO, code-generation units, PGO, allocator choice, CPU feature dispatch, affinity, and priority hints on the full OS matrix;
6. compare a provider native SDK with the portable protocol path at identical semantics and workloads.

An optimization is rejected if it improves an isolated median while worsening p99.9, correctness, gap recovery, memory, CPU/power, thermal stability, startup, reconnect, or portability. Provider limits and WAN distance are never “optimized” by relabeling a narrower local boundary.

---

## 16. Performance model and release gates

### 16.1 Latency vocabulary

- **Provider/exchange timestamp:** a value supplied by the provider in its clock domain; provenance and apparent-age input, not automatically a local latency boundary.
- **Local receive complete:** the complete WSS message or native SDK callback is available to Axiusflow and sampled with the process monotonic clock.
- **Decode complete:** framing, decompression if any, and Protobuf/provider decoding have completed.
- **Canonical accept:** the validated provider-neutral event is accepted by the desktop-local partition.
- **Model publish:** an immutable model generation containing the event becomes readable.
- **Frame submit:** Origin frame has been submitted through GPUI.
- **Presented pixel:** the relevant physical display update is externally or platform-instrumented as visible.
- **Durable order acceptance:** authoritative intent transaction committed.
- **Provider dispatch:** request handed to the provider transport after durable acceptance.

### 16.2 Required timestamp chain

```text
exchange_timestamp
provider_receive_timestamp
local_receive_complete_monotonic
decode_complete_monotonic
canonical_accept_monotonic
model_publish_monotonic
ui_invalidation_monotonic
origin_frame_ready_monotonic
frame_submit_timestamp
present_timestamp_if_measurable
```

Only timestamps in the same proven clock domain are subtracted. Provider/exchange timestamps are reported separately with their documented meaning, clock source, precision, and uncertainty. Order measurements additionally include input capture, command creation, local authorization/risk, durable boundary if applicable, provider write/callback, provider acknowledgement, normalized execution, and client presentation.

### 16.3 Real-time challenge targets

These are local architecture challenges, not production claims. They exclude unknown provider and internet transit and are invalid without the named workload and hardware profile.

| Boundary | Initial challenge |
|---|---:|
| local receive complete → canonical accept | p99 ≤ 500 µs and p99.9 ≤ 1 ms |
| canonical accept → model publish | p99 ≤ 250 µs and p99.9 ≤ 500 µs |
| local receive complete → model publish | p99 ≤ 750 µs and p99.9 ≤ 1.5 ms |
| model publish → frame submit | p99 ≤ 2 ms |
| local receive complete → presented pixel | p99 within one available refresh interval and p99.9 within two on the named display profile |

Failure to meet a challenge starts profiling; it does not authorize correctness loss, semantic conflation of ordered data, unsafe code, busy spinning, or an unsupported transport. A faster native SDK is selected only when the complete `local receive → model publish/present` distributions beat the WSS baseline under equivalent provider semantics.

Desktop presentation gates:

- 120 Hz is the minimum professional interaction benchmark where hardware supports it.
- 144 Hz is the flagship benchmark target.
- Local receive complete to presented pixel targets one available refresh interval at p99 and two at p99.9 on the named display profile.
- A 60 Hz fallback remains supported but is not the flagship performance claim.
- Multi-chart, order-book, scanner, and active-order workloads are tested together, not only as isolated charts.

Direct desktop execution challenge targets:

| Boundary | Balanced p99 | Low-latency p99 |
|---|---:|---:|
| local command ready → completed local risk decision | ≤ 1 ms | ≤ 500 µs |
| local command ready → crash-safe local acceptance | ≤ 5 ms | ≤ 2 ms |
| local acceptance → provider transport write | ≤ 1 ms | ≤ 500 µs |

These order targets exclude provider/WAN response but include all required local correctness work, including the independent user-controlled recovery acknowledgement in a live profile. If the selected embedded transactional and recovery boundaries cannot satisfy a target on the declared hardware/profile, the target fails; correctness is not weakened to make the number pass.

### 16.4 Startup, hydration, and interaction challenges

Startup performance is a first-class release gate. These are challenge targets, not claims, and must be reported separately for cold OS-cache, warm OS-cache, empty application cache, populated application cache, cache migration, one quarantined/corrupt segment, offline provider, slow provider, and unavailable Axiusflow control plane.

| Boundary | Initial challenge on named reference hardware |
|---|---:|
| process launch → first painted shell | warm p95 ≤ 250 ms; cold p95 ≤ 750 ms |
| process launch → restored workspace interactive | warm p95 ≤ 500 ms; cold p95 ≤ 1.5 s |
| process launch → cached visible chart useful frame | warm p95 ≤ 750 ms; cold p95 ≤ 2 s |
| symbol/timeframe switch → useful frame from cache | p95 ≤ 100 ms |
| pan/zoom input → stable useful frame | p99 within one available refresh interval |
| provider authentication start → authenticated | measured separately; no universal target without provider evidence |
| visible-range history request → useful frame | measured by provider/cache profile and requested range |
| cached/backfill state → verified live handoff | zero gaps, duplicates, regressions, flicker, or partial visible mutation |

The shell challenge excludes operating-system process launch outside Axiusflow control only when that exclusion is stated. A useful frame contains correctly scaled, source/freshness-labelled data for the visible range; an empty canvas, spinner, skeleton, or stale DOM is not a pass. “Faster than” another product is publishable only for the same named device, OS, product version, workspace, dataset/provider state, network condition, boundary, and repeatable procedure.

Startup reports include p50/p95/p99, five-run variance, CPU time, peak RSS, allocations, bytes read and decompressed, disk I/O, provider requests/bytes, cache hit ratios, migration/recovery work, indicator work, frame misses, and time spent on the GPUI thread. The release fails if startup requires the control cloud, blocks the GPUI thread on I/O/decode, scans all history before painting, loads every hidden tab eagerly, uses unbounded memory, displays unlabelled stale data, or enables order entry before provider reconciliation.

### 16.5 WAN, provider, and physical limits

No universal quote-to-pixel or click-to-venue number is published across arbitrary internet paths. End-to-end reports separate:

```text
exchange/provider processing + internet transit + local processing + display scheduling
```

The first two terms cannot be isolated from an ordinary managed API unless the provider supplies documented synchronized timestamps. Live reports therefore publish provider timestamp age, local processing, presentation, provider round-trip where measurable, endpoint/region, client location, network type, RTT/loss observations, and display refresh as separate values. No subtraction of unsynchronized wall clocks is presented as one-way latency.

### 16.6 Tail and overload gates

A release gate includes p99.9 and maximum behavior under:

- the highest provider-documented subscription and depth Axiusflow supports;
- deterministic replay at recorded and synthetic sustained rates;
- defined microbursts measured from scheduled arrival time to avoid coordinated omission;
- hot-symbol concentration;
- subscription churn;
- a deliberately slow UI consumer;
- disconnect, partial frame, malformed message, provider gap, and reordered replay input;
- snapshot storms;
- provider reconnect and resubscription;
- permitted local-recorder slowdown and disk-full behavior;
- telemetry enabled at production settings;
- cold/warm startup, empty/populated/corrupt caches, schema migration, huge workspaces, rapid symbol/timeframe switching, and aggressive pan/zoom;
- provider-history throttling, partial ranges, control-cloud outage, and cache-disabled operation where retention rights forbid persistence.

There is no pass if an ordered or authoritative event is silently lost, a queue becomes unbounded, canonical state diverges, memory grows without a declared bound, the GPUI thread performs network/decode work, or recovery requires an unexplained state guess. Replaceable quote conflation is counted and reported rather than described as packet loss.

### 16.7 Benchmark profiles

Every result records OS/kernel, CPU topology, power and thermal policy, memory, NIC/Wi-Fi and driver, GPU/driver, display resolution/refresh/DPI, provider interface, endpoint/region if known, transport/TLS/compression, RTT/loss observations, build flags, exact commit and lockfile digest, dataset/provenance, burst distribution, instrument/depth distribution, subscriptions, charts, sample count, histogram/percentile method, queue high-water marks, allocations, copied bytes, CPU time, memory high-water mark, gaps, conflation, reconnects, and frame misses.

The evidence suite has five distinct lanes:

1. **Deterministic local replay** — repeatable correctness and controlled-load comparison with scheduled-arrival timestamps.
2. **Authorized live observation** — real provider behavior without pretending the arrival rate or WAN is controlled.
3. **Windowed presentation** — real GPUI submission and platform presentation timing; flagship claims use external camera/photodiode evidence when platform callbacks are insufficient.
4. **Endurance and recovery** — at least one full session including reconnect, suspend/resume, network change, thermal equilibrium, and bounded-memory evidence.
5. **Startup and local hydration** — cold/warm/empty/corrupt/offline matrices, visible-range-first history, live handoff, workspace restoration, symbol/timeframe changes, pan/zoom, and indicator readiness on named low-, mid-, and flagship hardware profiles.

A publishable p99.9 requires at least 100,000 relevant samples; smaller runs label the percentile exploratory. Release comparisons use at least five measured runs after a declared warm-up, retain per-run and aggregate results, and report variance rather than selecting the best run. The benchmark definition and regression threshold are committed before collecting the candidate result. Unless a stricter profile overrides it, a change fails performance review when the confidence-supported regression exceeds 5% at p99 or 10% at p99.9, or when correctness, gaps, memory, CPU/power, thermal stability, reconnect time, or presentation loss worsens materially.

The portable provider profile is gated separately on Windows, Linux, and macOS. A native provider SDK is an additional measured profile, never a substitute for the portable matrix and never presumed faster from vendor marketing.

---

## 17. Observability and operations

Operational telemetry includes:

- every defined latency boundary and queue residence histogram;
- active provider interface, endpoint class, local mode, and fallback reason;
- local session generation, reconnect duration, and stale-session rejection;
- market gaps, quality flags, conflation, resnapshot, downgrade, and disconnect rates;
- local rights-gated capture continuity, retention state, and evidence gaps;
- local catalog/cache hit ratio, segment quarantine, migration, bytes read/decompressed, and visible-range hydration timing;
- local order-intent commit latency, recovery, unknown-outcome, and reconciliation state;
- future-cloud outbox age, database saturation, and slow-query metrics only when those approved deployables are active;
- provider round-trip, unknown outcomes, invalid OMS transitions, and reconciliation breaks;
- entitlement denials and revocation propagation;
- client decode, model, Origin build, GPUI submission, and presentation timing;
- dropped frames, frame pacing, GPU pressure, memory, allocation, copy, and thermal mode.

Raw quotes, DOM, account values, orders, credentials, provider tokens, instrument subscription lists, and payload bodies never enter Axiusflow telemetry. Diagnostics use bounded counters, timings, redacted provider error classes, and explicit user-exported local evidence bundles. Dashboards are separated for control, execution, client performance, security, and provider health; future cloud-data dashboards do not exist until that topology is approved. A production deployable requires ownership, SLOs, alerts, runbooks, rollback, recovery, and capacity limits.

---

## 18. Security, resilience, and supply chain

- Private networking and default-deny policy protect databases, durable infrastructure, and internal RPC.
- Workloads use unique identities and least-privilege database/cloud permissions.
- Provider credentials remain on the user's device in the native credential vault and never enter Axiusflow services, logs, traces, analytics, or crash reports.
- Desktop installers and updates are signed; macOS is notarized where required; update manifests are independently signed and anti-downgrade protected.
- Dependencies are exact-pinned with reviewed lockfiles, SBOMs, provenance, vulnerability review, and license policy.
- Protocol decoders, state transitions, native-SDK boundaries, and untrusted payloads are fuzzed.
- Identity, provider, local partition, credential-vault, reconnect, network-change, and desktop resource-exhaustion exercises are required now. Stage 5A activates Redpanda, ClickHouse, S3, and cloud partition/region exercises for its data infrastructure; Stage 5B independently activates Redpanda, transactional-database, outbox/inbox, execution-owner fencing, region-failover, and reconciliation exercises for its managed-execution infrastructure.
- Analytics, reports, or notifications cannot block execution.
- Missing risk or authoritative persistence fails live trading safely.

Disaster recovery restores authoritative state and explains every in-flight order within its declared profile. Live desktop trading requires the independently controlled encrypted recovery journal from Section 9.2; a same-device `local_only` store is paper/shadow only. No zero RPO/RTO claim exists without measured pre-dispatch durability and destructive restore proof.

---

## 19. Testing and certification

### 19.1 Semantic equivalence

The same versioned provider fixture is processed through the portable protocol adapter and every optional native SDK adapter. Canonical event bytes/values, sequence decisions, quality flags, snapshots, and errors must match. Performance cannot excuse semantic divergence.

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
- reconnect/session-generation and stale-callback injection;
- live state versus rights-permitted local-replay equivalence;
- slow-model/UI and snapshot-storm behavior;
- entitlement leakage checks;
- provider conformance and certification suites;
- WSS/native-SDK canonical equivalence where both interfaces are licensed.

### 19.4 Trading certification

- model-based OMS tests;
- fixed-point and ledger properties;
- broker-adapter conformance;
- paper and shadow order paths;
- duplicate intent and callback handling;
- timeout/unknown-outcome recovery;
- crash between durable commit and provider dispatch;
- process or total-device loss at every local/independent prepared/committed boundary and after acknowledgement/fill/cancel/replace/correction transitions, continuous independent-journal ordering, recovery-target outage, bounded backlog, and destructive restore;
- kill-switch and stale-risk-snapshot drills;
- daily reconciliation before broader live access.

### 19.5 Physical performance evidence

Renderer submission timing is necessary but not sufficient. Tick-to-pixel and input-to-pixel certification includes full host preparation and presentation. Where platform presentation timestamps are insufficient, an external high-speed camera or photodiode-style harness is used for the flagship claim.

---

## 20. Delivery sequence from the current foundation

Section 1.1 is the governing current-state baseline. Later target descriptions do not override it and must not be read as implementation claims. Work proceeds through explicit gates; an agent does not skip repair or decomposition because a later-stage feature appears more visible.

This section is the tracked work ledger. Every item is a checkbox governed by Section 1.7: check an item only from named evidence, keep its status token accurate, and update Section 1.8 when a gate's overall status changes. Statuses below were seeded from a read-only audit, so the implementing agent must confirm each one against live source and validation output before relying on it.

### Stage 0A: Stabilize and correct the existing foundation

Complete this gate before broad new product implementation:

- [ ] `S0A-01` inspect the live worktree and active-agent changes; preserve and integrate rather than overwrite them — status: `in_progress` (must be re-verified at the start of every session)
- [x] `S0A-02` restore green targeted and CI-equivalent workflows by fixing root causes, not by suppressing checks — status: `done_pending_commit` (GitHub Actions run 30710769490 on `bcd7529` is fully green across all eight checks, and the same gates pass locally: `cargo fmt --all -- --check`, workspace build/check/Clippy/tests, `axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, ingest conformance, native-copy compilation, the privileged AF_XDP copy lane, and the three privileged repeated-bind regressions. Four root causes were fixed rather than suppressed: the libxdp ring-mapping defect that held the kernel XSK pool, the provenance verifier hashing the gitignored generated `third_party/libxdp-sys/Cargo.lock`, the Linux lanes missing `libfontconfig-dev` and `libfreetype-dev` for the pinned GPUI desktop build, and the privileged rebind step invoking `cargo` through `sudo` where it is absent from root's PATH)
- [x] `S0A-03` replace the Origin Charts submodule/path integration with an exact-pinned external Git dependency — status: `done` (gitlink and `.gitmodules` removed, `origin_charts` ignored, three workspace deps pinned to `rev = 8753b071aed2f01ae6c61ad9fbc2a4d4a0506531`; `cargo metadata --locked` and `cargo check --locked -p axiusflow_chart_integration` pass)
- [x] `S0A-04` commit and push the Origin migration so the submodule stops appearing in the remote repository — status: `done` (`ad5637a` is present on synchronized `main...origin/main`; the remote tree has no Origin gitlink or `.gitmodules`)
- [x] `S0A-05` reconcile readiness manifests, runtime capability reports, and evidence artifacts with actual behavior — status: `done` (`cargo run --locked --package axiusflow_ingest_conformance -- --evidence-report .cache/evidence/stage_1_evidence_Linux.json` reports portable/tuned `fixture_validated`, AF_XDP `implemented` but unavailable, DPDK `contract_only` and unavailable; manifest now records Ubuntu 26.04 tuned-loopback evidence and the same limitations)
- [x] `S0A-06` finish the current AF_XDP copy-mode path to its honest evidence level, including bounded ownership, release, startup/shutdown, error, and fallback behavior — status: `done_pending_commit` (seven default adapter tests plus two native-copy boundary tests assert explicit unavailability, bounded configuration, lifecycle misuse rejection, no advertised fallback mode, and unverified zero-copy prerequisites. `tools/run_af_xdp_copy_conformance.sh` now reports `af_xdp_copy_conformance=passed active_mode=af_xdp_copy generic_skb=true forced_copy=true zero_copy=false`, covering the abandoned-batch, released-batch, and market-bar lifecycles with `generic_corpus_outcomes=6`, `generic_corpus_accepted_events=2`, `market_bar_corpus_outcomes=7`, and `market_bar_corpus_accepted_events=3`, and all three privileged regressions in `crates/adapters/linux_af_xdp/tests/sequential_rebind.rs` pass. The `EBUSY` blocker was a dependency defect, not a host limitation: `strace -e trace=bind,close,bpf,mmap,munmap` showed `munmap(0x2ffffffed8, 1344)` and `munmap(0x18c00000040, 832)` failing with `EINVAL` during `xsk_socket__delete`, because `xsk-rs 0.8.0` moves the fill/completion ring structs out of the boxes whose pointers libxdp retains as `ctx->fill`/`ctx->comp`. The vendored `libxdp-sys 0.2.4+1.6.0` source now records each RX, TX, fill, and completion mmap base at creation and unmaps those bases at deletion, so every ring mapping is released; `tools/verify_libxdp_patch.py` proves the ten reviewed hunks reconstruct the registry source byte for byte. `AfXdpCopyDriver::start` additionally retries only a native `EBUSY` at 10 ms intervals under a 1 s deadline, because the kernel releases the XSK pool asynchronously after the socket closes; every other error still fails immediately. Independent unsafe-boundary audit, privileged native data-path fuzzing, qualified NIC/driver evidence, and an authorized packet feed remain outstanding, and zero-copy is not claimed)
- [x] `S0A-07` either implement the claimed DPDK native behavior or explicitly downgrade it as unavailable without implying poll-mode verification — status: `done` (manifest maximum is `contract_only`; conformance reports `active_mode=unavailable`, native dependency `not_selected`, poll-mode lifecycle not exercised, and no hardware/provider claim)
- [x] `S0A-08` confirm portable and tuned Linux paths do not delegate production behavior to fixtures — status: `done` (Linux conformance passes native `UdpSocket` and `socket2` loopback, bounded lifecycle/overflow, packet-to-Origin, and partition/fanout equivalence; both remain only `fixture_validated` with tuning/provider limitations explicit)
- [x] `S0A-09` audit every `implemented`, `fixture_validated`, native-mode, zero-copy, and production-ready claim against actual source and evidence — status: `done` (repository claim search plus schema-v5 Linux evidence confirms actual active modes; connected-live, zero-copy, hardware, provider, and production claims are all `not_claimed`)
- [x] `S0A-10` document unavailable hardware/provider validation as an explicit evidence limitation rather than inventing a pass — status: `done` (manifest limitations and schema-v5 evidence name unexercised AF_XDP/DPDK lifecycle, unmeasured presentation, and absent hardware/provider evidence)
- [x] `S0A-11` leave the repository with no unexplained workflow failure or contradictory readiness declaration in the corrected scope — status: `done_pending_commit` (every lane in GitHub Actions run 30710769490 on `bcd7529` passes, the privileged lane's `af_xdp_sequential_queue_rebind_regression` step reports `3 passed; 0 failed`, and no fallback path was taken: `verify_af_xdp_unavailable_evidence_claim_boundary` and `unavailable_evidence_scope` are skipped while `AF_XDP_COPY_EXERCISED=true`. Readiness declarations remain portable/tuned `fixture_validated`, AF_XDP `implemented`, and DPDK `contract_only`, with zero-copy, hardware, provider, and production all `not_claimed`)
- [x] `S0A-GATE` exit criteria met: current code, configuration, runtime reporting, workflow behavior, and evidence agree; all corrected behavior passes its targeted checks; unsupported capability fails explicitly; and unresolved external hardware/provider gates are named without overstating readiness — status: `done_pending_commit` (every Stage 0A deliverable `S0A-02` through `S0A-11` is checked; `S0A-01` is a standing per-session worktree audit that is re-verified at the start of every session and is therefore never permanently checked. GitHub Actions run 30710769490 on `bcd7529` is green end to end; AF_XDP and DPDK activation still fail explicitly when unavailable; and the remaining external gates — qualified NIC/driver hardware, PTP timestamping, authorized provider feeds, and an independent unsafe-boundary audit — are named in `S0A-06`, `S1-04`, `S1-10`, and Stage 5 without any hardware, provider, zero-copy, or production claim; the privileged native data-path fuzzing named here at the time has since landed as the seeded adversarial lane recorded in `S1-04`)

### Stage 0B: Cohesively decompose corrected monolithic crates

After Stage 0A stabilizes behavior, apply Sections 1.4–1.6 crate by crate. Each crate is checked only when it satisfies all eight completion criteria in Section 1.6.

Method, applied per crate:

- [x] `S0B-01` map responsibilities, invariants, public exports, callers, and tests before moving any code — status: `done` (all priority crates and adapter callers were mapped before movement; the final adapter map separated configuration, native ownership, prerequisites/review, fixtures, errors, and tests)
- [x] `S0B-02` move one cohesive responsibility set at a time and preserve behavior — status: `done` (all priority crates are decomposed; targeted adapter tests and the full workspace suite pass without behavioral changes)
- [x] `S0B-03` turn each `lib.rs` into a documented façade with private modules and deliberate re-exports — status: `done` (all priority roots are façades; the four substantive network adapters are 23–31 lines and preserve their established public names through re-exports)
- [x] `S0B-05` run formatting and the narrowest meaningful validation after each crate, followed by affected workspace checks — status: `done` (targeted adapter check, Clippy, and tests pass; workspace format, build, check, Clippy, tests, naming, provenance, conformance, and native-copy compilation pass)
- [x] `S0B-04` separate production paths, fixtures, evidence generation, and platform-specific implementations — status: `done` (production state machines and platform resource owners now live separately from fixture and test modules throughout every priority crate)
- [x] `S0B-06` stop and fix any semantic, allocation/copy, queueing, visibility, or dependency regression before continuing — status: `done` (acyclic module graphs and unchanged hot-path operations were verified; no decomposition added allocation, copying, locking, async hops, or dynamic dispatch)
- [x] `S0B-07` require every subsequent feature to start in the correct cohesive module rather than rebuilding a monolith — status: `done` (Sections 1.4–1.6 are the mandatory architecture gate and every priority root now enforces the façade pattern)

Crate order, following Section 1.5:

- [x] `S0B-10` `crates/testing` (~5,688 lines in `lib.rs`) — status: `done` (all eight Section 1.6 criteria are satisfied. (1) Public contracts are unchanged: the exported surface is byte-identical to the previous commit at 37 items with none added or removed. (2) `lib.rs` is a 52-line façade of module declarations and re-exports. (3) Fixtures and support code are owned by `harness_error`, `packet_corpus`, `binary_fixture`, `canonical_market_bar`, and `loopback_fixture`, separate from the twelve scenario modules. (4) Each decoder, corpus, queue-policy, partition, and scenario group has one discoverable home. (5) Visibility is minimal with 31 `pub(crate)` items and the module graph is acyclic after the `loopback_fixture` fix recorded in `S0B-06`. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Sections 1.1 and 1.8 are updated in the same change)
- [x] `S0B-11` `crates/adapters/market_protocol` (~2,276) — status: `done` (six cohesive modules extracted: `errors`, `convention`, `wire_codec`, `canonical_mapping`, `envelope_encoding`, and `stream_decoder`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 27 items with none added or removed. (2) `lib.rs` is a 37-line façade of module declarations and re-exports. (3) Wire DTO codec, canonical mapping, and envelope encoding are separated from the stream decoder state machine. (4) Each error model, codec, convention, and decoder has one discoverable home. (5) Visibility is minimal with two `pub(crate)` items and the module graph is acyclic: `errors` is a leaf, `convention`/`wire_codec`/`canonical_mapping` depend only on it, and `envelope_encoding` and `stream_decoder` sit above them. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-12` `crates/adapters/stream_websocket` (~1,796) — status: `done` (four cohesive modules extracted: `session`, `endpoint`, `plain_loopback_owner`, and `background_runtime`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 27 items with none added or removed. (2) `lib.rs` is a 28-line façade of module declarations and re-exports. (3) Message-semantics session state is separated from endpoint validation, single-connection ownership, and the background thread bridge. (4) Each state machine — session, owner, and runtime worker — has one discoverable home. (5) Visibility is minimal with one `pub(crate)` item and the module graph is acyclic: `session` and `endpoint` are leaves, `plain_loopback_owner` depends on both, and `background_runtime` sits above them. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-13` `crates/ui/chart_integration` (~1,793) — status: `done` (seven cohesive modules extracted: `bridge`, `coordinator`, `provenance`, `origin_bridge`, `host_benchmark`, `recovery_conformance`, and `view`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 15 items with none added or removed. (2) `lib.rs` is a 24-line façade of module declarations and re-exports. (3) The Origin engine bridge, bounded provenance retention, and headless conformance evidence are separated from the GPUI view entity. (4) The data bridge, stream coordinator, provenance store, and view each have one discoverable home. (5) Visibility is minimal with 21 `pub(crate)` items and the module graph is acyclic: `bridge` and `provenance` are leaves, `coordinator`/`origin_bridge` depend on `bridge`, and `host_benchmark`/`recovery_conformance`/`view` sit above them. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, nine `stage_2_*=passed` results, and `replay_to_gpui_host_benchmark=passed samples=64`. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-14` `crates/realtime` (~1,612) — status: `done` (seven cohesive modules extracted plus a test-only fixture: `canonical_event`, `partition`, `bounded_queue`, `fanout`, `snapshot`, `latest_state`, `errors`, and `test_fixture`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 27 items with none added or removed. (2) `lib.rs` is a 27-line façade of module declarations and re-exports. (3) The test-only canonical event and queue-policy fixtures live in a `#[cfg(test)]` module, separate from the production contracts. (4) Each state machine — canonical event validation, fenced partition ownership, bounded queue overflow, direct/durable fanout, snapshot checksums, and latest-state projection — has one discoverable home, and the four unit tests now sit beside the modules they exercise per Section 1.4. (5) Visibility is minimal with 15 `pub(crate)` items and the module graph is acyclic: `errors` is a leaf, `canonical_event` depends only on it, and `snapshot`/`partition`/`bounded_queue`/`fanout`/`latest_state` layer upward. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-15` `crates/application` (~1,370) — status: `done` (seven cohesive modules extracted: `use_case`, `provenance`, `replay_snapshot`, `embedded_source`, `generation`, `stream_runtime`, and `errors`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 25 items with none added or removed, including the re-exported `axiusflow_protocols` types. (2) `lib.rs` is a 30-line façade of module declarations and re-exports. (3) The deterministic embedded replay source is separated from the transport-neutral snapshot, generation, and stream-runtime contracts. (4) Each of the use-case port, provenance validation, replay session, single-writer client model, and stream command/event boundary has one discoverable home. (5) Visibility is minimal with three `pub(crate)` items and the module graph is acyclic: `errors` and `use_case` are leaves, `provenance` depends on `errors`, `replay_snapshot` on both, and `embedded_source`/`generation`/`stream_runtime` layer upward. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-16` `crates/transport` (~808) — status: `done` (seven cohesive modules extracted: `profile`, `receive_batch`, `binary_frame`, `driver`, `readiness`, `errors`, and `fixture_driver`. All eight Section 1.6 criteria hold. (1) The exported surface is byte-identical to the previous commit at 27 items with none added or removed. (2) `lib.rs` is a 26-line façade of module declarations and re-exports. (3) The deterministic fixture driver is isolated in `fixture_driver`, separate from the production ingest contracts. (4) The profile vocabulary, borrowed receive batch, readiness guard, and bounded frame decoder each have one discoverable home, and the fifteen unit tests now sit beside the readiness and binary-frame modules they exercise per Section 1.4. (5) Visibility is minimal with one `pub(crate)` item and the module graph is acyclic after the `profile` fix recorded in `S0B-06`: `profile`, `receive_batch`, and `binary_frame` are leaves. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed with all 15 transport tests accounted for, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-17` authorization and security crates — status: `done` (`crates/domain/authorization` is split into six cohesive modules plus a test fixture — `identifiers`, `request`, `policy`, `decision`, `evaluator`, `errors`, and `test_fixture` — and `crates/security` into four plus a test fixture — `identity`, `key_set`, `verifier`, `errors`, and `test_fixture`. All eight Section 1.6 criteria hold for both. (1) Both exported surfaces are byte-identical to the previous commit at 16 and 15 items respectively, none added or removed. (2) Their `lib.rs` files are 24-line and 19-line façades. (3) Test-only principals, grants, signing keys, JWKS documents, and tokens live in `#[cfg(test)]` fixture modules, separate from the production policy and cryptographic boundary. (4) Identifier validation, grant snapshots, decision evidence, evaluation, JWKS parsing, and token verification each have one discoverable home, and the nine unit tests now sit beside the modules they exercise per Section 1.4. (5) Visibility is minimal and both production module graphs are acyclic: `errors` is the leaf in each, and `crates/security` is strictly layered `errors` → `identity` → `key_set` → `verifier`. (6) `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets` at 62 passed and 0 failed, `cargo run --locked --package axiusflow_naming_check`, `python3 tools/verify_libxdp_patch.py`, and `cargo run --locked --package axiusflow_ingest_conformance` all pass, the last reproducing `stage_1_contract_smoke=passed`, `corpus_outcomes=6 canonical_events=2`, and nine `stage_2_*=passed` results. (7) The changes are pure moves that add no allocation, copying, locking, async hop, or dynamic dispatch. (8) This ledger and Section 1.8 are updated in the same change)
- [x] `S0B-18` each operating-system and accelerated network adapter — status: `done` (portable socket, tuned Linux socket, AF_XDP, and DPDK are split by configuration, native/unavailable ownership, prerequisite/review evidence, fixtures, errors, and tests behind 23-, 29-, 31-, and 24-line façades; the already-cohesive macOS and Windows capability boundaries remain 26 lines; targeted and workspace validation pass)
- [x] `S0B-GATE` exit criteria met: the priority crates have discoverable ownership boundaries; their `lib.rs` files are façades rather than implementation bodies; existing public behavior remains valid; and no broad new feature continues the one-file-per-crate implementation pattern — status: `done` (all priority façades and module ownership were audited; full local CI-equivalent validation passes)

### Stage 1: Complete the cross-platform provider foundation

Complete, correct, and validate the remaining work in parallel where dependencies permit:

- [x] `S1-01` explicit `platform_runtime`, `transport`, `realtime`, and `ingest_driver` ports — status: `done` (ports present; `platform_runtime` OS adapters are tracked separately as `S1-14`)
- [x] `S1-02` separate `portable_socket`, `tuned_linux_socket`, `linux_af_xdp`, and `linux_dpdk` adapter crates and build targets — status: `done` (four isolated adapter crates exist; native completeness tracked by `S0A-06`, `S0A-07`, `S0A-08`)
- [x] `S1-03` a bounded borrowed receive-batch contract with explicit buffer lifetime, release, timestamp-source, overflow, and capability semantics — status: `done` (implemented in `crates/transport` with explicit release and drop accounting)
- [x] `S1-04` exact-pinned accelerated dependencies plus safety, license, provenance, fuzzing, and maintenance review — status: `retired` (the historical experimental adapters reached the recorded safety, provenance, virtual-device, fuzz, and privileged lifecycle evidence described by `S0A-06` and `S1-06`, but the current managed-provider topology has no valid use for either dependency. `S1-18` removes both adapters and their native dependency graph from active product and release scope. The isolated source, vendored patch, fuzz target, harnesses, and prior evidence remain for audit; no unfinished acceleration review blocks the consumer product, and reintroduction requires a new decision under `S5-05`)
- [x] `S1-05` Windows, Linux, and macOS build/smoke matrix for the pinned GPUI source and portable path — status: `done_pending_commit` (in GitHub Actions run 30710769490 on `bcd7529` the `portable_windows-latest`, `portable_macos-latest`, and `portable_ubuntu-latest` jobs each pass `portable_contracts_and_real_socket_build`, the loopback packet-to-partition-fanout-Origin conformance run, evidence retention, and `pinned_gpui_desktop_compile_smoke`, and `portable_cross_platform_evidence_gate` passes over the three retained artifacts. This proves cross-platform build, portable socket, and compile-smoke parity only; it measures no renderer submission, presentation, or latency)
- [x] `S1-06` Linux compile/conformance lanes using software-accessible AF_XDP modes and DPDK software or virtual devices where runner capabilities permit — status: `done` (`tools/run_af_xdp_copy_conformance.sh` runs the privileged lane in a disposable network-isolated container with all capabilities dropped except BPF, IPC_LOCK, NET_ADMIN, NET_RAW, SYS_ADMIN, and SYS_RESOURCE, a private bpffs via `LIBXDP_BPFFS`, deterministic veth MAC addresses, and a bounded `AF_PACKET` replay sender that asserts its transmitted packet count. The lane now passes end to end and writes schema-v1 evidence with `renderer_submission=not_measured` and `physical_presentation=not_measured`; the three privileged repeated-bind regressions pass in the same namespace. A DPDK virtual-device lane now passes too: `tools/run_dpdk_vdev_lifecycle.sh` builds the adapter's `native` feature against the exact-pinned Ubuntu 25.11-2 DPDK packages — the build refuses any other `libdpdk` modversion and the runtime refuses any `rte_version` that does not contain 25.11.0 — and exercises EAL init, `net_ring` virtual-device enumeration (one port), and clean EAL teardown without huge pages, PCI, or privileges, with every unsafe FFI call isolated in `native_sys` behind one serialized EAL guard; the lane passes on the host and in the hardened container and writes schema-v1 evidence at honest `contract_only` readiness with `poll_mode_receive=not_exercised`. The queue and mbuf lifecycle is now exercised too: the same lane creates a 1,024-entry pktmbuf pool, configures the `net_ring` port with one receive and one transmit queue, transmits eight frames through the `rte_eth_tx_burst` fast path, receives all eight back through `rte_eth_rx_burst` with byte-exact payloads, accounts for every pool entry at teardown (1,024 before and after), and stops, closes, and frees cleanly. bindgen 0.72.1 generates the exported DPDK surface from the exact reviewed headers while DPDK's header-inline fast path is reached through the checked-in one-line forwarders in `native/shim.c`, so no Rust code replicates header internals; every unsafe call stays isolated in `native_sys` behind one serialized EAL guard, and the lane passes on the host and in the hardened container with schema-v1 evidence at honest `contract_only` readiness. A real-PMD queue, hardware NIC/driver qualification, and DPDK fuzzing remain outstanding and are tracked by `S1-04`, `S5-01`, and `S5-02`. GitHub Actions run 30758575835 on `433c858` reports all eight checks green, including `linux_af_xdp_generic_copy_veth_conformance` with its data-path fuzz step and the `linux_dpdk_virtual_device_eal_lifecycle` job, which builds the exact-pinned 25.11.0 packages inside a self-contained ubuntu:26.04 image, runs the queue/mbuf loopback, and verifies the evidence claim boundary)
- [x] `S1-07` a machine-readable, runtime-enforced readiness manifest that prevents activation or claims above each profile's proven readiness state — status: `done` (the embedded manifest advertises only portable and tuned Linux sockets; targeted tests prove retired AF_XDP and DPDK profiles fail with `MissingProfile`, while unreviewed evidence identifiers, unavailable modes, capability/profile mismatches, and malformed manifests remain rejected)
- [x] `S1-08` deterministic Ethernet/IP/UDP/TCP/provider fixtures proving identical canonical events, gaps, errors, and replay across applicable profiles — status: `done` (fixture corpus and conformance harness in `crates/testing` and `tools/ingest_conformance`)
- [x] `S1-09` canonical timestamp vocabulary and latency recorder — status: `done` (recorder implemented; hardware timestamp capability tracked by `S1-10`)
- [ ] `S1-10` hardware timestamp capability reporting and validation — status: `blocked_external` (requires a PTP-capable NIC and qualified host)
- [x] `S1-11` fenced single-writer partition contract — status: `done` (fenced partitions with ownership epoch and stale-writer rejection)
- [x] `S1-12` direct-fanout and durable-tap interfaces using deterministic replay — status: `done` (bounded direct and durable queue interfaces with replay)
- [x] `S1-13` full replay-to-GPUI frame benchmark, including model and host work — status: `done` (`run_replay_to_gpui_host_benchmark` measures 64 iterations after 1 warmup and reports p50/p95/p99/p99.9/max for three real stages: binary decode plus client model, Origin frame construction, and GPUI scene planning through `GpuiChartRenderer::plan_frame`, asserting immutable generations, that Origin consumed the latest sequence, and that the prepared frame produced primitives and plan operations. The windowed completion path is `axiusflow_desktop --windowed-benchmark`: it drives deterministic replay deltas through the real GPUI Wayland window one update per frame, performs genuine renderer submission, and measures update-to-presented latency and presented-frame cadence through GPUI's compositor-driven `on_next_frame` callbacks against the display profile reported by `NativeDisplayProbe`. On this host's named profile (DP-3, 164,957 mHz, derived 1.25x scale, monotonic presentation clock) 256 measured frames after 32 warmup reported update-to-presented p50/p95/p99/p99.9/max of 13.0/14.3/15.1/18.3/18.3 ms and present-interval cadence p50 of 13.2 ms — roughly 75 Hz pacing on the 165 Hz profile, recorded as measured rather than normalized. The schema-v1 artifact names its method `gpui_on_next_frame_compositor_frame_callbacks`, records `renderer_submission_performed=true` and `physical_presentation_measured=true`, and honestly limits itself: presentation is measured through compositor frame callbacks rather than external scanout instrumentation, the window's output is attributed by compositor placement, the data source is the disconnected fixture, and the profile is a single named display)
- [ ] `S1-14` `platform_runtime` operating-system adapters: credential vault, PKCE callback, update/rollback, power, display, and scheduling — status: `partial` (the credential vault, signed update-verification core, rollback state, native scheduling hints, and PKCE callback capabilities are implemented and tested. `NativeCredentialVault` uses exact-pinned `keyring 4.1.6` binary-secret APIs and process-wide serialized access to Linux Secret Service, macOS Keychain, or Windows Credential Manager; service and key components use independently typed base64url backend identifiers so Windows' dot-joined credential target cannot collide across vault namespaces. Missing-entry loads return `None`, deletion is idempotent, and identifiers reject empty or control-bearing values. Three backend-isolated tests validate binary round trips, collision resistance, identifier handling, and redaction without altering a host keychain. `SignedUpdateVerifier` verifies an independently signed, strict 4 KiB manifest with Ed25519 before parsing, binds the verified manifest to a domain-separated fingerprint of the authenticating key and anti-downgrade floor, enforces a monotonically increasing release sequence, and streams an exact-size artifact of at most 8 GiB through SHA-256 while rejecting cross-verifier manifest mixing, truncation, trailing bytes, and digest mismatch. `UpdateRollbackState` retains exactly one locally verified predecessor in the same signing-key trust domain, rejects impossible retained slots after a completed rollback, restores only that predecessor, never lowers its anti-downgrade sequence floor, and validates restoration of persisted active, rollback, trust-domain, and floor state. Seven verifier/state tests cover valid binary artifacts, signature/schema/tamper/downgrade rejection, verifier-provenance isolation, trust-domain confinement, artifact boundary failures, interrupted-read retry, rollback invariants, and restart hydration. `DurableUpdateActivator` is Unix-only because this safe Rust boundary cannot issue a Windows directory-metadata durability barrier; beneath an existing caller-provisioned root, it exclusively locks an 8 MiB bounded append-only journal, syncs exact rehashed artifacts into content-addressed release files before syncing each activation record and affected directory, persists the original signed manifest and signature rather than trusting reconstructed metadata, and recovers only a contiguous sequence of valid activation or one-slot rollback transitions. Recovery truncates an interrupted final append, clears stale staging files, re-verifies every manifest against the supplied independently managed signing key, requires that verifier's trusted floor to equal the latest durable floor, and rehashes every retained artifact before returning its path. Failed staging does not change committed state, a failed journal append forces reopen before another mutation, and the logical state retains one predecessor while never lowering its sequence floor. Fourteen focused tests cover activation/rollback restart recovery, repeated-activation artifact cleanup, the required pre-existing root, fixed-entry symlink rejection during initialization and recovery, interrupted-initialization artifact cleanup, partial-append cleanup, exclusive ownership, failed staging, bounded artifact validation, artifact and signing-key tampering, impossible signed transitions, signed-manifest metadata tampering, and exact floor binding. `SignedUpdateVerifier::authorize_rotation` verifies a strict statement of at most 1 KiB with the current Ed25519 key before parsing, binds the named current-key identity, rejects floor regression, weak or reused successor keys, unknown fields, and chains beyond 16 keys, and carries the root trust-domain identity into successor manifests so one in-memory predecessor can roll back across an authorized key boundary. Public identity and floor accessors provide the exact statement inputs without exposing private material. Two tests cover authorized successor verification with cross-key rollback and tamper/floor/reuse/size rejection. The Unix journal persists each release with its cumulative signed authorization chain as `KeyRotationProof` records and, on recovery, replays that chain from the independently managed root key alone through `replay_rotations`, which refuses replay onto an already-rotated verifier and never lowers the trusted floor; every persisted manifest is then re-verified by the rebuilt successor key, and a rotated activation must extend rather than diverge from its predecessor chain. Activation rejects a candidate whose proof chain does not extend the committed chain before staging or appending anything, so recovery can never reject a record this adapter committed, and an empty chain is omitted from the record so ordinary same-key stores keep emitting byte-identical schema-1 records. A record that carries authorization proofs declares activation schema 2 instead, because a predecessor binary without rotation replay cannot verify a rotated manifest at all; it therefore reports an unsupported journal schema rather than misreporting a valid journal as corrupt, and recovery requires each record to declare exactly the schema its contents imply. The committed chain is tracked as an independent high-water mark that rollback restores an older release beneath but never lowers, so a committed rotation can be neither abandoned nor replaced by a sibling branch, and recovery validates each journal transition against that same mark. Four regressions durably activate an authorized rotated release, recover it from the root key across a restart, roll back to the pre-rotation predecessor, reopen that cross-key state again, prove a root-signed sibling release is refused both immediately after rotation and after a rollback while the committed state stays recoverable, and assert the exact on-disk schema of same-key and rotated records. Windows crash-safe staging, per-OS installer switching, executable code-signature and macOS notarization enforcement, and launcher integration remain incomplete, so durable `rollback` capability is still reported unavailable. `NativePowerMonitor` uses exact-pinned `zbus 5.18.0` to install a sender/path/interface/member-specific system-bus match for systemd-logind `PrepareForSleep`, keeps at most 16 queued messages, maps the documented boolean body to `Suspending` and `Resumed`, and exposes transport, malformed-body, closed-stream, and unsupported-platform failures instead of inventing transitions. The blocking listener is Linux-only and documented for a background thread; Windows and macOS remain unavailable. Two deterministic tests cover event mapping and target capability reporting, and `busctl` on this host confirms `org.freedesktop.login1.Manager.PrepareForSleep` has the expected boolean signature. `NativeThreadScheduler` uses exact-pinned `core_affinity 0.8.3` and `thread-priority 3.1.1`: it discovers and deduplicates affinity targets per calling thread rather than caching one thread's allowed set, lets the kernel validate each requested target so a thread already narrowed to one processor can still move to another permitted target, and exposes separately fallible priority and affinity operations so partial application is never hidden as atomic. Affinity is reported available on Linux only. macOS reports affinity unavailable because `core_affinity 0.8.3` does not obtain a valid Mach thread port, and Windows reports it unavailable because that backend is not processor-group-aware on hosts above 64 logical processors. Non-real-time priority hints are reported available on Linux only and remain independently usable there even when affinity discovery is empty. Windows and macOS report priority unavailable: neither exposes a real-time-class check reachable without `unsafe`, which this workspace forbids, so raising priority there could silently produce a real-time base priority. The balanced baseline preserves the thread's existing normal priority, so it never requires privileges and never fails on a thread already started at a positive nice value, yet it still consults the real-time policy guard rather than reporting success for a real-time caller; the responsive and latency-sensitive hints request values 65 and 80, raise priority, and surface the operating-system EPERM without CAP_SYS_NICE or a raised RLIMIT_NICE instead of silently reporting success. A Linux thread already running under SCHED_FIFO or SCHED_RR is refused outright rather than having these values reinterpreted as real-time priorities, and a thread under SCHED_IDLE is refused because Linux ignores niceness for that policy, so success would not raise effective priority. The macOS backend additionally maps normal-policy values through process-wide niceness rather than a current-thread-only control. Three backend-isolated and discovery tests validate support reporting, bounded mappings, priority independence from empty affinity discovery, and OS rejection without mutating the test runner's scheduling. A direct unprivileged Linux probe on this host reported 24 discovered targets, a successful balanced hint, EPERM for the responsive hint, and a successful move to a second target after the first pin, confirming the hints report real operating-system outcomes rather than assumed success. `PkceSecret` draws 32 bytes of verifier entropy and 32 independent bytes of CSRF state from the operating-system CSPRNG through `getrandom 0.4.3`, derives the challenge as `URL_SAFE_NO_PAD(SHA-256(verifier))`, hard-codes `S256` so `plain` cannot be selected, and redacts both the verifier and the state from `Debug` because a leaked state permits a forged state-verified redirect. `LoopbackRedirectListener` binds an ephemeral `127.0.0.1` port and keeps serving redirects until one proves knowledge of `state` or the deadline elapses, enforced by a non-blocking accept loop against a single absolute `Instant` expiry. Every read is bounded by that same expiry through a deadline-aware reader, and each connection may consume at most 2 s of it; up to eight connections are served concurrently without accepting and dropping queued redirects when all worker slots are occupied. Per RFC 6749 section 4.1.2.1 it verifies `state` in constant time *before* honoring an `error` response, and reduces the reported code to at most 64 alphanumeric/`_`/`-` characters so a redirect cannot inject text into diagnostics. The browser response is best-effort, so a tab that disconnects after submitting a verified redirect cannot discard the authorization code. A forged state, missing state, foreign path, non-GET method, oversized request line, or header flood is answered `400` without aborting the pending sign-in; only a `state`-verified authorization-server error or a verified redirect missing its code aborts. Request bounds are explicit and exact: an 8,192-byte request line and up to 64 header lines of 8,192 bytes each, with exactly 64 headers accepted. Twenty callback tests pass, thirteen of them integration tests driving a real loopback socket, covering deadline enforcement, slow-drip resistance, stalled-peer starvation, saturated-worker queue preservation, stray-traffic resilience, browser disconnection, the exact header bound, and error sanitization. `RuntimeCapabilities::detect_native` derives reported capabilities from the implemented backends instead of a hand-written literal, so durable `rollback` and hardware timestamping stay `Unavailable`; Linux display timing reports the implemented Wayland probe, Linux registered URI-scheme registration reports the implemented XDG handler; Linux power notifications report the implemented logind listener, while the PKCE loopback callback, signed-update verification, and Linux priority/affinity hints report their real state. Credential-vault capability detection reports only targets with a compiled native adapter and deliberately leaves store initialization lazy, so transient Secret Service or user-session startup failures remain retryable through normal vault operations; two tests assert that no unimplemented port is ever claimed and that supported targets advertise only their compiled adapter. `RegisteredUriRedirect` validates a private redirect URI, refusing reserved `http`/`https`/`file`/`ftp` schemes that would intercept ordinary web navigation, non-RFC-3986 schemes, and a URI that already carries a query or fragment. It verifies a delivered redirect under the same rules as the loopback listener by reusing its shared percent-decoding, constant-time comparison, and error-sanitizing helpers: `state` is compared in constant time before any `error` response is honored, the redirect target must match exactly, the URI is bounded to 2 KiB, and the code is redacted from `Debug`. The registered base is additionally bounded below the 2 KiB delivered limit by a 512-byte response reserve, so a base can never be accepted that no verifiable redirect could fit, and the post-scheme remainder must be valid RFC 3986 path syntax rather than merely non-empty. `NativeUriSchemeRegistrar` is Linux-only and writes an XDG `x-scheme-handler` desktop entry beneath an existing caller-provisioned data directory through a uniquely named synced temporary file and rename, syncing both that directory and, when it creates it, its parent, and treating a concurrent directory creation as the same successful end state, so neither a crash nor a concurrent registration can leave a partial or unreferenced handler. The executable path is serialized per the Desktop Entry `Exec` rules, quoting the argument, doubling `%`, and emitting four backslashes for one literal backslash because generic string parsing precedes command-line parsing. `write_entry_in` performs only that durable write, while `register_in` also refreshes the desktop MIME database and installs the entry under `[Default Applications]` in a caller-provided `mimeapps.list`, because rebuilding the capability cache only advertises that an entry *can* handle a scheme and does not route it; that rewrite preserves every other association and section, replaces only this scheme's default, is bounded to 256 KiB, and lands through its own synced temporary file and rename. The complete read-modify-write is serialized across processes by an exclusive lock on a sibling file, so two concurrent registrations cannot lose each other's association, and every temporary name mixes the process identifier, a nanosecond timestamp, and a counter with retry so a leftover file from an earlier crash cannot block a later registration after identifier reuse. It reports an explicit unavailable-tool, refresh-failure, or unreadable-associations error instead of advertising an inactive handler as registered; Windows and macOS report the capability unavailable. Registration is one lock-guarded transaction: a single exclusive lock spans the snapshot, replacement, activation, and rollback, so concurrent registrations of the same identifier cannot snapshot the same prior entry and let a failing one undo a successful one. A failing activation step never destroys a working handler: the existing entry and the previous associations are both captured before replacement and atomically restored on failure, an entry the call created is removed instead, and the capability cache is refreshed again against the restored state while the activation error stays primary. Nine tests cover scheme and remainder validation including malformed percent escapes, the response-reserve bound, a verified code, missing/forged state, foreign targets, oversized URIs, sanitized state-verified server errors, entry content with spaced, percent-bearing, and backslashed executables, identifier/executable rejection, absence of partial entries and association files, default-handler installation that preserves unrelated associations inside private test-owned data and config homes, three concurrent association updates that all survive, and restoration of a previous handler after a failed activation. `NativeDisplayProbe` reads Linux display state from the Wayland session: `wl_output` at version 4 supplies current-mode refresh in millihertz, physical pixels, integer scale, and connector names, the XDG output manager supplies logical geometry so the effective scale is derived from the physical-to-logical ratio in thousandths, capturing fractional scales the integer event cannot express, and `wp_presentation` reports whether presented-frame feedback shares the `CLOCK_MONOTONIC` domain used by the latency recorders, with unrecognized clocks flagged so they are never compared against local records. A live probe on this host returned `DP-3` at 164,957 mHz with 2560x1440 physical pixels against 2048x1152 logical, deriving the true 1.25x scale while the integer event announced 2, and a monotonic presentation clock; per-frame feedback requires a committed surface, so it remains the UI layer's responsibility and only the clock is probed. Zero logical geometry is rejected rather than divided by, a missing compositor or Wayland library reports `NoSession` instead of invented geometry, and Windows, macOS, and X11 sessions report the port unavailable. Six deterministic tests cover scale derivation, fallback, zero-geometry rejection, refresh-interval reciprocals, and clock classification, and one session test accepts either a real probe with sane refresh and scale bounds or an explicit `NoSession` on a headless runner. The crate is decomposed into `capability`, `clock`, `credential_vault`, `display_timing`, `paths`, `composition`, `pkce`, `power_notifications`, `signed_update`, `signed_update::activation`, `thread_scheduling`, `uri_callback`, and `loopback_callback` behind a 46-line façade. Per-OS installer activation and code-signature/notarization enforcement, Windows/macOS power and display, and Windows/macOS current-thread priority and affinity adapters remain incomplete, so this item stays unchecked)
- [x] `S1-14a` Linux native network-change adapter — status: `done` (`platform_runtime::NativeNetworkMonitor` adds the fourteenth cohesive platform module and updates the façade to 48 lines. It installs sender/path/interface/member-specific system-bus matches for `NetworkManager.StateChanged` and the service's `NameOwnerChanged`, bounds the signal queue at 16 messages, reads initial state only after subscribing, and reconciles every signal through a separate query connection against the current owner/property so stale queued transitions cannot restore connectivity, daemon loss maps unavailable, and property replies cannot deadlock behind signal backpressure. It treats only global connectivity as provider-available, coalesces repeated non-global states, and emits the exact `NetworkEvent` consumed by `DesktopProviderRuntime`. `RuntimeCapabilities::detect_native` reports the compiled backend. `tools/run_native_network_monitor_conformance.sh` passes four deterministic cases and, on this host, a live listener/property probe reporting `Available`; signal-driven and daemon-restart integration remain explicitly unproven. This entry supersedes the older module/test inventory embedded in `S1-14`; Windows and macOS network-change adapters remain incomplete.)
- [ ] `S1-15` native Windows and macOS ingest adapters behind the shared ingest contract — status: `not_started` (fixture drivers; `NATIVE_MODE_IMPLEMENTED = false`)
- [x] `S1-16` bounded client semantic classes and snapshot recovery — status: `done` (semantic delivery classes, snapshots, and recovery implemented)
- [x] `S1-17` first-party unit-test coverage for readiness enforcement, bounded frame decoding, and explicit accelerated unavailability — status: `done` (62 focused default-feature tests pass: 15 transport, 8 portable socket, 7 AF_XDP, 6 DPDK, 6 tuned Linux socket, 5 security, 4 authorization, 4 realtime, 4 persistence, and 3 platform runtime. Native-copy adds two deterministic AF_XDP configuration/lifecycle boundary tests, and `crates/adapters/linux_af_xdp/tests/sequential_rebind.rs` adds three privileged regressions. The descriptor-release regression passes; immediate sequential rebind remains the qualified-host/kernel blocker in `S0A-06`, not a hidden or claimed pass)
- [x] `S1-18` retire AF_XDP and DPDK from active product scope: remove them from desktop/runtime selection, default workspace validation, installers, readiness advertising, and active CI; preserve provider-neutral bounded ownership, sequence, timestamp, and backpressure contracts plus historical evidence — status: `done` (the two adapter crates are excluded from root workspace membership and the active dependency graph; ingest conformance, readiness schema v6, the live feed matrix, Linux installer, Linux development guide, and Stage 1 workflow expose only portable and tuned socket profiles. `tools/run_acceleration_retirement_conformance.sh` deterministically verifies workspace, readiness, installer, workflow, and runtime rejection. `docs/decisions/2026_08_04_acceleration_retirement.md` records the preserved bounded ownership, release, sequence, timestamp, overflow, partition, fanout, and replay contracts plus the isolated historical source and evidence boundary)
- [ ] `S1-GATE` exit criteria met — status: `not_started`

Exit criteria: the same applicable provider-message/replay corpus produces equivalent canonical and Origin state across operating systems and optional native SDKs; no domain, decoder, partition, or fanout contract assumes provider-library-owned memory; every queue and timestamp boundary is visible; AF_XDP and DPDK are absent from the active consumer product and release gates; direct WSS/native provider profiles fail explicitly above their evidence; and neither Redpanda nor an Axiusflow cloud hop is required to demonstrate the production path.

### Stage 2: Connected read-only market data

Deliver:

Revision 7 defines the target production topology. Completed centralized items below remain valid implementation evidence, but labels such as “in production,” “client,” “durable branch,” or “gateway” describe the exercised reference vertical, not an authorization to route production user market data through Axiusflow cloud services.

Stage 2 does not wait as a whole for a Rithmic or CQG agreement. `S2-16` through `S2-18` remain externally blocked until the applicable provider materials and rights arrive; provider-neutral items continue against deterministic replay and the authorized Coinbase crypto feed. After access is granted, the provider adapters must reuse these contracts and pass their own fixtures, rights review, certification, and direct-user-device evidence before being called supported.

- [x] `S2-01` first legally authorized live provider adapters using the best currently affordable managed, sandbox, delayed, or real-time source available under its terms — status: `done` (the first authorized provider is Coinbase Advanced Trade public market data — free, keyless for public channels, US-safe — recorded as entitlement class `crypto_public_realtime` with venue provenance on every event. `crates/adapters/coinbase_market` subscribes to `heartbeats` and `market_trades` over TLS with bounded configuration, validates `sequence_num` continuity with an explicit gap outcome instead of silent continuation, deduplicates the snapshot/update trade overlap in a bounded window, and decodes prices and sizes to exact mantissa+scale fixed point with no float round-trip, covered by ten first-party tests including the documented protocol messages and timestamp parsing. The live lane `--coinbase-live` connected to the real endpoint on 2026-08-03 and collected 247 BTC-USD/ETH-USD trades in 20 s with 20 heartbeats, zero sequence gaps, zero malformed messages, and a byte-exact sample trade, writing schema-v1 evidence at honest scope (single venue, public rate limits, no Level 2, no backfill). The vertical is now proven end to end: `services/market_data_plane` backfills 300 one-minute candles per product over HTTPS (chunked-transfer aware), aggregates live trades into deterministic minute bars — merging the still-open backfilled minute instead of duplicating it — and serves clients a bounded snapshot plus live delta bars in the exact binary market-bar protocol over one-product-per-connection WebSockets with overload disconnects instead of unbounded buffering. `tools/run_live_data_plane.sh` proves the full path: 298–299 snapshot bars at sequences 1–N followed by a live aggregated delta bar at N+1 with valid OHLC, increasing timestamps, and `coinbase` provenance on every event. The now-retired `AXIUSFLOW_LIVE_ENDPOINT` reference mode rendered that stream in the desktop for a sustained 20-second window; `S2-21` removes it from the shipping binary while retaining the reference service and conformance lane. Two real defects were found and fixed in the vertical: a seconds-vs-minutes unit mismatch that silently classified every trade as late, and a duplicate current-minute bar that broke timestamp monotonicity. Entitlement class `crypto_public_realtime` and single-venue, public-rate-limit, plaintext-client-edge limitations are recorded in schema-v1 evidence; production deployment remains under `S2-02`)
- [ ] `S2-02` production direct-desktop provider runtime using WSS/TLS or a certified native SDK, with credentials confined to the OS vault and no Axiusflow cloud market-data hop — status: `partial` (the authorized keyless Coinbase public adapter now exposes a direct TLS/WebSocket connection and `CoinbaseProviderDriver` runs it behind the desktop lifecycle owner with one generation-fenced session thread, a fixed-capacity redacted callback queue, explicit overflow/sequence-gap/peer-close/transport invalidation, cooperative DNS/TCP/TLS/subscription/read stop polling in 100 ms I/O slices, and replacement-generation callback cleanup only after confirmed join. Credentialed providers still load only through `CredentialVault`; the explicit public policy bypasses vault access rather than storing a fake secret. Deterministic tests compose establishment, authorized current-history seeding, bounded trade-to-bar aggregation, and lifecycle-fenced delivery with `DesktopMarketWorker`, but `apps/desktop`, native network-monitor delivery, shipping-topology inspection, cross-platform evidence, and provider certification remain incomplete, so this item stays unchecked.)
- [x] `S2-03` remove AF_XDP/DPDK product integration and replace their active feed matrix with `provider_websocket`, optional `provider_native`, deterministic replay, and disabled `cloud_stream` profiles — status: `done` (`ProviderFeedProfile` is separate from the historical foundational transport enum and exposes exactly the Section 7.1 vocabulary. The schema-v2 active matrix reports the live-verified Coinbase TLS/WebSocket feed class as `applicable` to `provider_websocket`, deterministic replay as `test_only`, and unqualified native plus disabled cloud paths as explicit `unavailable`; no AF_XDP or DPDK row remains. The focused transport and conformance-tool tests plus a generated Coinbase matrix artifact pass.)
- [x] `S2-04` explicit readiness and `unavailable` capability results for incompatible or unqualified profile/feed combinations, with no pretend acceleration or silent fallback — status: `done` (the provider-oriented feed matrix reports unqualified native and disabled cloud profiles as `unavailable`, deterministic replay as `test_only`, and the TLS/WebSocket profile as transport-compatible without claiming direct-desktop certification; its artifact continues declaring silent fallback forbidden)
- [x] `S2-05` centralized reference latest-state fanout — status: `done` (`services/market_data_plane` now materializes every product's completed backfill as a checksum-verified canonical snapshot under a unique fenced partition, installs it into `CanonicalLatestState`, and sends every subsequent completed live bar through `DirectDurableFanout`. Both client snapshots and live deltas are projected from that canonical state, so the accepted direct branch advances the partition-owned latest state before its exact identity, epoch, sequence, timestamps, and provenance enter bounded client queues. Event IDs include instrument, bar time, and sequence rather than colliding across products. A configured Redpanda tap requires a caller-owned `--ownership-state` file whose locked, synced reservation advances the writer epoch before every process start; its independent bounded worker retains and retries each dequeued event until delivery evidence arrives, exposes retry/delivery health, and never puts a durable acknowledgement in the client path. Missing taps and full/stopped worker queues fail explicitly with counters instead of silent fallback. Five first-party tests prove durable epoch advancement, distinct cross-product identities, epoch propagation, identical branch identity, latest-state advancement, topic/key/envelope metadata, duplicate rejection, and gap latching. `tools/run_live_data_plane.sh` passed against the authorized Coinbase feed on 2026-08-04 with a 299-bar canonical snapshot followed by contiguous live sequence 300; a live digest-pinned Redpanda integration also consumed the expected `instrument:coinbase:btc:usd:coinbase` partition key from the service's configured worker. This is reusable conformance evidence for local latest-state work and a future cloud topology, not the current production delivery route.)
- [x] `S2-06` Redpanda durable branch and raw S3 capture — status: `done` (`crates/streaming` now owns the Section 11.2 envelope contract — protobuf `EventMetadata` with event ID, schema version, producer, timestamps, correlation/causation, partition, and ownership epoch over a truncation-rejecting length-framed wire form — and the Section 11.2/11.3 topic and partition-key vocabulary with four first-party tests. The durable branch publishes through exact-pinned `rdkafka 0.39.0` with vendored librdkafka 2.12.1 as an idempotent `acks=all` producer behind bounded publish/flush calls, with delivery evidence on a bounded channel and unsafe FFI isolated in one private producer module; `rskafka 0.6.0` was rejected for lacking the idempotent-producer semantics Section 11.1 requires. The raw capture path is a first-party synchronous `SigV4` PUT/GET client — no listing, no multipart, bounded sizes — whose signing chain matches the AWS documented known-answer example and whose civil-date and URI-encoding rules are unit-tested. `tools/run_redpanda_durable_branch.sh` publishes 64 envelopes to a digest-pinned single dev Redpanda broker and reads the partition back byte-exact with per-key ordering intact and exact delivery accounting; `tools/run_s3_raw_capture.sh` writes four 64 KiB immutable raw batches to a digest-pinned plaintext `MinIO` and reads them back byte-exact with SHA-256 integrity. Both lanes pass locally, the `stage_2_durable_plane` workflow runs them on hosted runners with claim-boundary verification, and schema-v1 evidence honestly records TLS, ACLs, replication, IAM policy, schema governance, managed-store, and multi-broker drills as `not_exercised`. Hosted execution evidence is pending: run 30761677940 could not start any job because the GitHub account hit its billing/spending limit (external blocker, 2026-08-02))
- [x] `S2-07` binary WebSocket baseline in production — status: `done` (the binary market-bar WebSocket baseline is exercised against the live authorized provider: `services/market_data_plane` serves the exact binary protocol (protobuf `MarketBarStreamEnvelope` frames — snapshot chunks then deltas) that the historical reference desktop consumed from the live Coinbase feed, with one product per connection, bounded per-client queues with overload disconnects instead of unbounded buffering, and entitlement enforcement at the handshake from `S2-10`. The `live_data_plane` and `entitlement_enforcement` lanes retain that reference evidence; `S2-21` removes the cloud route from the shipping desktop. TLS at the public client edge remains a Stage 2 gate concern, not a baseline gap)
- [x] `S2-08` QUIC prototype behind negotiation — status: `done` (the prototype runs exact-pinned `quinn 0.11.11` (MIT OR Apache-2.0, quinn-rs) on a current-thread `tokio 1.53.1` runtime with `rcgen 0.14.8` self-signed certificates inside the conformance tool's `quic` feature; the workspace's synchronous library boundaries stay async-free because the runtime lives only in the prototype lane. One loopback `stream_quic` connection carries the Section 8.2 profile: a reliable control stream where the client offers `stream_quic` and the server accepts it, while an unsupported offer is explicitly rejected with no silent fallback; a reliable market stream delivers the ordered snapshot-plus-eight-deltas payload byte-exact; four self-identifying datagrams (flow, subscription, sequence, schema, semantic class) all arrive; the client pins the server's exact certificate and a client pinning a foreign certificate fails; zero-RTT stays disabled. Thirty-two sequential handshakes measure p50/p95/max (1.66 ms p50 on this host). Schema-v1 evidence is explicitly `prototype_not_certified` with load, loss, migration, and the operating-system matrix unexercised)
- [x] `S2-09` ClickHouse projections and deterministic bars — status: `done` (`crates/streaming` now owns the Section 11.5 projection sink: a bounded synchronous `ClickHouse` HTTP client writing `JSONEachRow` batches of fixed-point bar rows carrying event ID and projection version into a `ReplacingMergeTree(projection_version)` keyed on the stable bar identity, so a repeated insert never duplicates and a rebuilt batch supersedes under `FINAL`. `tools/run_clickhouse_projections.sh` runs against a digest-pinned ClickHouse 26.7.1.1315 server with basic authentication: it projects the 64-bar deterministic embedded replay snapshot, reinserts the identical batch with no duplication (64 rows under `FINAL`), rebuilds at version 2 with byte-identical fixed-point values, and reads the deterministic ordering back exactly. Schema-v1 evidence records TLS and cluster deployment as `not_exercised`)
- [x] `S2-10` entitlement enforcement and resnapshot behavior on live streams — status: `done` (the reference market data plane enforces entitlements on every live connection: the handshake requires a service-access Ed25519 token verified offline against the revisioned JWKS (`crates/security`), and the authorization domain's exact-grant snapshot decides `stream:<product>` access with HTTP 403 on denial — no silent access without a matching grant. A supervisor resnapshots keys and policy on a bounded interval, requiring monotonic policy versions, and re-evaluates every connected principal against the new snapshot, disconnecting revoked streams instead of silently retaining them. `tools/run_entitlement_enforcement.sh` proves the full behavior on the live Coinbase feed: a valid token opens the stream, missing, foreign-key, and ungranted tokens are refused at the handshake, and removing the grant in policy version 2 disconnects the live connection within the resnapshot interval. The historical desktop reference mode passed its token through `AXIUSFLOW_AUTH_TOKEN`; `S2-21` removes that route from the shipping binary. Schema-v1 evidence records TLS and production policy distribution as `not_exercised`)
- [x] `S2-11` PostgreSQL persistence implementation behind the existing outbox/inbox traits, with migrations and recovery evidence — status: `done` (`crates/persistence` now implements the contracts against the exact-pinned synchronous pure-Rust `postgres 0.19.14` client (MIT OR Apache-2.0; sqlx and tokio-postgres rejected to keep an async runtime out of the persistence boundary), decomposed into `errors`, `migrations`, `postgres_transactions`, and `review` behind the existing façade with all four first-party contract tests intact. `migrate` applies embedded checksummed migrations in strict order inside one transaction each and re-verifies recorded SHA-256 checksums on every run, so a mutated migration fails as drift. `PostgresTransaction` stages outbox records inside the caller's transaction with bounded identifiers and payloads, deduplicates inbox receipts via `ON CONFLICT DO NOTHING`, and the unpublished-outbox query redelivers every committed-but-unpublished record in commit order. `tools/run_postgres_persistence.sh` proves the lifecycle against a digest-pinned PostgreSQL 17.10: first migration applies 1 and the re-run 0, a committed record is visible, a duplicate receipt stages nothing, a rolled-back record is invisible, a dropped-and-reconnected client still recovers the record, publish marking clears it, and a tampered checksum is rejected as drift. Schema-v1 evidence records TLS, role isolation, and backup drills as `not_exercised`)
- [x] `S2-12` running authentication and authorization service boundary — status: `done` (`services/authorization_service` now runs a bounded typed HTTP boundary: Ed25519 service-access JWTs are verified against a pinned JWKS snapshot through `crates/security` (offline, revisioned), decisions are evaluated through the authorization domain's exact-grant snapshots, and policy distribution is a monotonically increasing versioned snapshot swap so removing a grant revokes it immediately. Requests are bounded (8 KiB line, 32 headers, 4 KiB body, 2 s deadline per connection) and oversized bodies are drained before the 413 response so a client never sees a reset. `tools/run_authorization_boundary.sh` spawns the real service against a lane-generated JWKS and proves startup with health reporting, allow and deny decisions, expired-token and foreign-key rejection, monotonic swap with revocation, and malformed/oversized rejection; startup without valid JWKS or policy still exits fail-closed. Schema-v1 evidence records TLS, sessions, workload identity, and the gRPC transport as `not_exercised`)
- [ ] `S2-13` first-party Rust `auth_service` replacing the superseded Better Auth exception: password verification, OAuth/OIDC, native PKCE loopback, second factor and passkeys, PostgreSQL credential/session state, Ed25519 token issuance, and revisioned JWKS publication, with no JavaScript runtime in any deployable — status: `partial` (`services/auth_service` now runs the credential core: signup stores argon2 PHC credentials (exact-pinned `argon2 0.5.3`, RustCrypto, MIT OR Apache-2.0, 42M downloads, reviewed in `docs/decisions/2026_08_03_better_auth_rs_review.md`), sign-in verifies with a dummy-hash path so unknown principals share the same argon2 timing, sessions are SHA-256-digested secrets in PostgreSQL with durable revocation, and Ed25519 service-access tokens are issued first-party against the `crates/security` contract with revisioned JWKS published at `/jwks`. `tools/run_auth_service.sh` proves the vertical against a digest-pinned PostgreSQL: signup, duplicate rejection, wrong-password and unknown-principal 401s, and the decisive Section 3.2 check — the issued token verifies offline through `crates/security` against the service's own JWKS with subject and session intact. OAuth/OIDC, native PKCE loopback, second factor and passkeys, TLS, and production key management remain unimplemented, so this item stays unchecked)
- [ ] `S2-14` safety, license, provenance, fuzzing, and maintenance review for each exact-pinned authentication dependency before adoption — status: `partial` (the leading-candidate review is complete with a rejection: `docs/decisions/2026_08_03_better_auth_rs_review.md` records the 2026-08-03 re-measurement (crate 0.10.0 checksum verified, 5,670 downloads, single-maintainer, master stale since 2026-06-06), the decisive static finding that the crate issues HS256 JWTs and opaque session tokens with no EdDSA or JWKS path — so its tokens can never verify through `crates/security` — and the security-relevant upstream gap to `better-auth@1.4.19` (magic-link/email-OTP Origin validation, Google One Tap sign-up bypass, Apple PKCE, and the 1.7 identity rewrite). The composed exact-pinned primitives path in Section 3.2 is confirmed; per-primitive reviews for `argon2`, `oauth2`, `openidconnect`, `webauthn-rs`, and `totp-rs` are still required at adoption time, so this item stays unchecked)
- [ ] `S2-15` second authorized live provider adapter: Indian equities via user-credentialed broker API (first candidate FYERS, self-serve OAuth app, free real-time WebSocket ticks; alternates Upstox, Dhan) — entitlement class `broker_byok_realtime`: every user connects with their own broker account and OAuth consent (public-client PKCE loopback, no client secret in any deployable, tokens in the platform credential vault), data is display-and-local-analytics only with no server-side receipt, storage, or retransmission, and every event retains broker/venue provenance — status: `not_started`
- [ ] `S2-16` direct Rithmic Protocol API adapter: WSS/TLS plus generated Protobuf, bounded heartbeat/session lifecycle, snapshots/deltas, sequence and gap recovery, provider timestamps with documented clock meaning, order/execution message separation, and Rithmic conformance evidence — status: `blocked_external` (requires the dev kit, protocol files, test credentials, certification cases, and a usable market-data test environment)
- [ ] `S2-17` direct CQG Web API adapter: WSS/TLS plus generated Protobuf, bounded logon/ping/subscription lifecycle, snapshot/delta/DOM recovery, collapsing detection, provider timestamps, and CQG conformance evidence — status: `blocked_external` (requires commercial access, protocol package, test credentials, certification cases, and confirmed rights)
- [ ] `S2-18` optional Rithmic R|API+ feasibility lane behind an isolated C ABI shim or process boundary, including supported compiler/runtime packaging, callback lifetime safety, reconnect semantics, canonical equivalence with Protocol API, and measured end-to-end comparison before selection — status: `blocked_external` (requires the C++ dev kit and redistribution/runtime terms)
- [ ] `S2-19` desktop-local provider runtime: OS-vault credentials, provider session generation/fencing, bounded semantic queues, shared immutable model state, rights-gated encrypted local history/replay, visible-range-first hydration, deterministic cache/backfill/live handoff, redacted diagnostics, suspend/resume and network-change recovery, and no provider payload or credential upload — status: `partial` (`crates/desktop_provider_runtime` provides the non-`Send` single-worker lifecycle core: credentialed attempts reload bounded opaque bytes through `CredentialVault` and zeroize the returned buffer, while explicitly keyless public providers bypass vault access instead of manufacturing a secret; session callbacks are fenced by strictly increasing local generations, semantic output is fixed-capacity with payload-redacted `Debug`, accepted market state is shared as immutable `Arc<MarketStreamPublication>`, queue overflow preserves the prior publication while fencing the session, drop attempts provider cleanup, an unconfirmed stop blocks replacement until cleanup succeeds without losing interleaved power/network state, diagnostics expose only coarse classes/counters, and every connection path respects suspend/network fences while environmental restoration forces a fresh generation only when a connection was already desired. The direct `CoinbaseProviderDriver` now opens the authorized public TLS/WebSocket session off-worker, reports post-handshake establishment, uses a fixed-capacity redacted callback queue, tracks one provider-wide sequence across subscription acknowledgements, heartbeats, and trades, fences five seconds of provider silence, terminates on overflow/sequence gap/peer close/transport failure, observes cooperative stop through DNS/TCP/TLS/subscription/read work in 100 ms I/O slices, owns at most one generation thread, rejects stale stops, joins confirmed shutdown, and clears stopped-generation callbacks before replacement. `DesktopMarketWorker` composes those callbacks with the real encrypted history/hydration/handoff owner on the same worker: it advances lifecycle state before returning a generation-matched trade, bounds active handoffs, registers them against the streaming provider generation, retires them on every lifecycle fence including semantic-queue overflow, rejects delayed live/snapshot callbacks before history mutation, coarsens nested history diagnostics, and keeps authenticated local hydration independent of provider and control-plane availability. Its scheduler-completion boundary accepts an immutable scheduler-validated Coinbase page only after exact provider/account/entitlement, instrument, dataset, resolution, and range matching plus worker-owned non-reusable scheduler binding to the complete immutable segment revision, rejects unfinished pagination and stale generations before decoding, requires explicit empty-page cutover evidence, preflights decoded bytes without mutating cache state, validates contiguous history before atomic publication, and can issue fresh recovery work after a live gap without discarding the active handoff floor. The shared Coinbase adapter now also owns exact-scale one-minute aggregation with a hard 4,096-bar per-product retention ceiling; the worker caps registered products, accepts only the matching active handoff and latest fetchable completed-minute publication for seeding, opens the following generation-fenced live minute without duplicating history, resets aggregate state on every lifecycle fence, and invalidates malformed live input instead of continuing a potentially corrupt stream. `platform_runtime::NativeNetworkMonitor` supplies the same lifecycle event type from a bounded Linux `NetworkManager` listener. The Coinbase adapter supplies direct BTC-USD/ETH-USD public one-minute history with strict public identity scope, minute-aligned 350-item request bounds and correct inclusive-provider/half-open-internal translation, direct rustls HTTPS with explicit crypto-provider selection, one capacity-one resolver worker, one absolute DNS/connect/TLS/HTTP deadline, bounded multi-address fallback, exact fixed-point bars, stable minute-derived sequences, and payload/metadata identity binding. `tools/run_desktop_provider_runtime_conformance.sh` passes twenty-nine deterministic cases including driver lifecycle, callback bounds/redaction, real adapter→scheduler→worker completion, history-seeded live aggregation, and malformed-input invalidation; `tools/run_native_network_monitor_conformance.sh` passes four listener cases plus a live host probe, `tools/run_provider_history_conformance.sh` passes eight Coinbase adapter cases, and the live Coinbase lane requires non-empty direct HTTPS history before WebSocket trades. Native-network event delivery, Windows/macOS network-change backends, and `apps/desktop` wiring remain absent; cloud-data absence remains a separate `S2-21` gate, so this item stays unchecked.)
- [ ] `S2-20` verifiable performance evidence suite from Section 16: deterministic scheduled-arrival replay, authorized live observation, windowed presentation, startup/local hydration, endurance/recovery, raw-result checksums, five-run variance, 100,000-sample publishable p99.9, and cross-platform regression gates — status: `not_started`
- [ ] `S2-21` prove cloud-data absence in the shipping topology: network integration tests and release inspection show that market data and provider credentials flow only between the user device and provider endpoints; cloud telemetry and services receive neither payload class — status: `partial` (the shipping desktop no longer accepts the historical `AXIUSFLOW_LIVE_ENDPOINT`/`AXIUSFLOW_AUTH_TOKEN` route to the centralized reference service and no longer links a direct WebSocket client. `tools/run_desktop_cloud_absence.sh` inspects the locked desktop dependency closure, shipping source, and release binary and fails if an Axiusflow market-data service, cloud data-plane dependency, retired endpoint knob, or credential-in-query path is present. The current release remains fixture-only, so direct device-to-provider traffic and credential-flow network integration evidence remain pending `S2-02` and `S2-19`; this item stays unchecked.)
- [x] `S2-22` embedded desktop storage spike and architecture decision: compare exact-pinned SQLite WAL with at least one pure-Rust transactional candidate; test workspace/catalog and local OMS workloads, crash recovery, corruption, disk full, migration, fsync latency, packaging, security, and maintenance; select the transactional catalog separately from the immutable history-segment codec — status: `done` (commit `1fbb43d`; `tools/run_embedded_store_spike.sh` and the recorded decision select SQLite only for relational desktop metadata while retaining explicit power-loss, physical-disk-full, encryption, Windows, and macOS gates.)
- [x] `S2-23` rights-aware local history store: atomic checksummed segment publication, provider/account/entitlement isolation, bounded catalog, encryption/retention/secure deletion, targeted quarantine, source/schema/calendar/adjustment/correction invalidation, and provider re-fetch or live-only fallback — status: `done` (`crates/desktop_storage` implements the blocking worker-side boundary; `tools/run_local_history_store.sh` passes ten lifecycle tests covering every named behavior, quarantined expiry/refetch, and shared-key deletion safety. The decision records physical-erasure, power-cut, provider-rights, cross-platform, vault integration, and production-performance limitations.)
- [x] `S2-24` provider history capability adapters and bounded request scheduler: bars/ticks/depth matrix, lookback, pagination, rate limits, request deduplication/cancellation, visible-range priority, adjacent prefetch, and deterministic snapshot/backfill/live sequence handoff — status: `done` (`crates/provider_history` implements the bounded provider-neutral worker contract, including monotonic quota windows, bounded retry/terminal fetch-failure reporting, explicit empty-snapshot cutover watermarks, and explicit eviction/reporting when queued work exceeds lookback. `crates/adapters/coinbase_market` now implements `ProviderHistoryAdapter` for BTC-USD/ETH-USD public one-minute candles only: request ranges are minute-aligned, translate the internal exclusive end to Coinbase's final inclusive minute, and are capped at the provider's documented 350-item maximum; local scheduling is capped at one request and one in-flight page per second; TLS uses an explicit rustls crypto provider, attempts every bounded resolved address through one capacity-one resolver worker, and applies one 15-second absolute deadline across DNS, connect, TLS, and HTTP; responses are bounded to 1 MiB; every decimal converts exactly to the reviewed instrument scale; and the versioned payload binds sequence, event time, and OHLCV before desktop decoding. Other Coinbase precision profiles, ticks, and depth remain explicitly unavailable. `tools/run_provider_history_conformance.sh` passes eight scheduler cases, three handoff cases, and eight Coinbase adapter cases including cancellation-bound socket I/O and immediate fallback from definitive connection failures; the real Coinbase lane additionally fetches and decodes a non-empty bounded page for each configured product directly over provider HTTPS before its WebSocket observation. The decision records that desktop/startup composition, performance, provider certification, and Rithmic/CQG profiles remain under `S2-19`, `S2-20`, `S2-25`, and the partnership-gated `S2-16` through `S2-18`.)
- [x] `S2-25` startup and history correctness suite: cold/warm/empty/populated/corrupt/migrated/offline/control-cloud-down matrices, shared multi-chart cache, zero gap/duplicate/flicker handoff, bounded memory/I/O, and no GPUI-thread storage/network/decode work — status: `done` (`crates/desktop_history` opens and recovers the real encrypted segment store only after its dedicated-thread guard, composes provider handoff there, then exposes only bounded shared immutable chart generations. `tools/run_startup_history_correctness.sh` passes the full deterministic matrix, cached key/retention revalidation with expired backing-file cleanup, live-only miss policy, bounded provider-only identity validation, exact shared-`Arc` multi-chart reuse, chart-rebind eviction release, retained-generation charging, active-handoff pinning, cached generation/watermark floors with separate local/provider generation namespaces, bounded local cache rotation with exact preallocation reservation, ownership-safe bounded handoff mutation and retirement, corrupt-segment quarantine, schema-revision miss, bounded decoder expansion, gapped decoded-sequence rejection, explicit offline/control-outage behavior, storage-limit configuration rejection, cataloged payload and encrypted-file pre-I/O bounds, pre-clone decoded snapshot/live rejection with move-only snapshot publication, worker-wide bounded buffered-live bytes/items, exact snapshot-overlap charging, duplicate/gap rejection, covering-snapshot recovery after every observed loss, and transactional publication/eviction failures. The decision explicitly makes no startup-latency, GPUI presentation, cross-platform, provider-network, huge-workspace, hidden-tab, indicator, or interaction-performance claim; those remain under `S2-19`, `S2-20`, `S4-01`, `S4-09`, and `S4-10`.)
- [ ] `S2-GATE` exit criteria met — status: `not_started`

Exit criteria: direct Rithmic and/or CQG user-device sessions pass provider certification plus sustained, burst, gap, reconnect, slow-model, presentation, startup/hydration, endurance, and semantic-equivalence gates on every supported operating system; credentials stay in the OS vault; raw and normalized provider data do not traverse Axiusflow cloud; local caching/recording is rights-gated; cold/warm/corrupt/offline histories recover without stale-truth ambiguity; cached/backfill/live handoff has no gap, duplicate, or flicker; delayed or sandbox data is never represented as live-trading evidence; every displayed event retains provenance; and every public performance statement is reproducible from a checksummed Section 16 artifact.

### Stage 3: Durable controlled execution

Deliver:

- [ ] `S3-01` desktop-local execution cell with one authoritatively fenced Axiusflow provider account session and no Axiusflow order-routing hop; require provider exclusivity, or both a short-lived opaque cross-device lease and provider-enforced account-wide hard risk across every writer, otherwise remain read-only — status: `not_started`
- [ ] `S3-02` immutable policy and risk snapshots — status: `not_started`
- [ ] `S3-03` local OMS state machine, stable client order IDs, and crash-safe transactional order intent — status: `not_started`
- [ ] `S3-04` pre-trade risk engine and kill switches — status: `not_started`
- [ ] `S3-05` direct Rithmic/CQG/broker order adapter and provider dispatch — status: `not_started`
- [ ] `S3-07` rights-permitted local portfolio ledger input and immutable journal — status: `not_started`
- [ ] `S3-08` restart/reconnect provider reconciliation for orders, fills, positions, balances, and risk state — status: `not_started`
- [ ] `S3-09` paper, shadow, restricted canary, then limited live trading progression — status: `not_started`
- [ ] `S3-10` encrypted device-loss recovery profile: independently controlled durability target, user-held recovery key, two-boundary prepared/committed protocol with pre-dispatch intent acknowledgement, ordered acknowledged replication of every authoritative execution/reconciliation/ledger transition, orphan-prepare rules, bounded outage backlog with a calculated emergency reserve and explicit evidence-discontinuity behavior, corruption/key-loss/retention/secure-deletion rules, and destructive restore plus provider-reconciliation drills; `local_only` remains paper/shadow — status: `not_started`
- [ ] `S3-11` explicit retry classes, unknown-outcome recovery, and server-side order-feature integration — status: `not_started`
- [ ] `S3-GATE` exit criteria met: provider-exclusive and provider-risk-bounded Axiusflow-leased multi-device cases, external provider-application activity, process and total-device loss, recovery-target outage through emergency-reserve exhaustion, destructive restore, every timeout, duplicate callback, stale session, lease expiry, and unknown outcome converge to an explainable provider-reconciled state while meeting the direct desktop execution budget and sending no order/account payload through Axiusflow cloud — status: `not_started`

Retired identifier, preserved for history and excluded from Stage 3 deliverables and gate completion: `S3-06` transactional outbox publication — status: `superseded` by `S5-08` because the current direct desktop execution path publishes no order/account payload to Axiusflow cloud.

### Stage 4: Professional terminal and transport

Deliver:

- [ ] `S4-01` connected production desktop runtime replacing the fixture/disconnected path — status: `not_started`
- [ ] `S4-02` multi-chart and order-book workspaces — status: `not_started`
- [ ] `S4-03` scanners, advanced alerts, news, external calendars, and advanced analytics — status: `not_started` (drawings and indicators ship in the Origin Charts engine; remaining work is desktop wiring, tracked under `S4-01`)
- [ ] `S4-04` 120/144 Hz workload certification — status: `not_started`
- [ ] `S4-05` certified direct-provider WSS/native profile across the release OS matrix — status: `not_started`
- [ ] `S4-06` balanced and low-latency local modes with measured power, thermal, memory, frame-pacing, and recovery behavior — status: `not_started`
- [ ] `S4-07` professional direct broker and FIX routes — status: `not_started`
- [ ] `S4-08` instant local shell/workspace restoration with asynchronous control sync and documented offline grace/expiry behavior — status: `not_started`
- [ ] `S4-09` progressive visible-range-first chart/indicator hydration, shared history generations, render level-of-detail, adjacent prefetch, and responsive symbol/timeframe/pan/zoom behavior — status: `not_started`
- [ ] `S4-10` Section 16.4 startup and interaction certification on named low-, mid-, and flagship hardware across Windows, Linux, and macOS — status: `not_started`
- [ ] `S4-GATE` professional terminal and transport gates met, including useful local UI before external readiness, certified startup/hydration/interaction targets, smooth flagship workloads, bounded resource use, and fail-closed trading state — status: `not_started`

### Stage 5A: Optional cloud data and venue-adjacent scale

This stage is intentionally unavailable to the initial product. A cloud-data capability begins only after Axiusflow has a signed provider agreement permitting server-side receipt and redistribution, a compliance and data-residency decision, viable economics, and an approved architecture record. Direct desktop data delivery remains supported even if this option later ships.

Deliver:

- [ ] `S5-01` commercial and legal gate: explicit server receipt, redistribution, storage, derived-data, professional/non-professional, geography, audit, deletion, and end-user entitlement rights — status: `blocked_external`
- [ ] `S5-02` isolated cloud-data architecture decision with threat model, cost model, data-flow inventory, retention, credential model, tenant isolation, incident response, and direct-desktop fallback — status: `blocked_external`
- [ ] `S5-03` provider-certified cloud ingest and entitlement enforcement using ordinary tuned sockets first, with fenced ownership, bounded latest state, direct fanout, and rights-scoped durability — status: `blocked_external`
- [ ] `S5-04` benchmark cloud delivery against direct provider delivery on named user locations and workloads; publish provider/WAN, server processing, client processing, presentation, gaps, and availability separately — status: `blocked_external`
- [ ] `S5-05` decide from profiles whether any raw/multicast feed is bottlenecked by tuned kernel sockets; only then create a new decision for AF_XDP, DPDK, Onload, or provider-native acceleration — status: `blocked_external`
- [ ] `S5-06` optional certified QUIC/WebTransport client delivery, multi-provider arbitration, and regional expansion without removing the direct-provider fallback — status: `blocked_external`
- [ ] `S5-GATE` optional cloud data ships only when its rights, compliance, security, funding/cost, correctness, availability, recovery, and measured user benefit all pass without weakening the direct desktop data path — status: `blocked_external`

### Stage 5B: Optional managed execution

This independent stage is intentionally unavailable to the initial product. It begins only after broker/provider authorization, a regulatory and custody decision, viable funding, approved security and recovery designs, and measured evidence that an Axiusflow order hop benefits the user. Direct desktop order routing remains supported even if this option later ships.

Deliver:

- [ ] `S5-07` separate managed-execution decision for any Axiusflow order/account hop: broker/provider authorization, regulatory/custody analysis, threat model, durable authority, tenant isolation, disaster recovery, reconciliation, cost/funding, and measured benefit over direct desktop routing — status: `blocked_external`
- [ ] `S5-08` transactional outbox publication for managed order/account events, implemented only after `S5-07` authorizes the exact payload classes and topology — status: `blocked_external`
- [ ] `S5-EXEC-GATE` optional managed execution ships only when its authorization, compliance/custody, security, funding/cost, correctness, availability, recovery, and measured user benefit all pass without weakening direct desktop order routing — status: `blocked_external`

No date or current-product dependency is attached to either Stage 5 capability. “Planned” means the contracts avoid preventing it; it does not mean Axiusflow currently receives or may legally redistribute provider data or route user orders.

---

## 21. Explicit rejected designs

1. Linux-only desktop functionality.
2. Calling `io_uring` kernel bypass.
3. Claiming AF_XDP zero-copy without verified driver support.
4. Shipping AF_XDP or DPDK in the consumer product, rebinding a user's NIC, requiring huge pages/VFIO, or spending a polling core for a managed provider API.
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
23. Routing, storing, logging, or retransmitting user-entitled provider market data through Axiusflow cloud before the future cloud-data rights and architecture gate passes.
24. Blocking a production-complete portable/tuned release on unavailable paid direct feeds, colocation, or acceleration hardware—or claiming synthetic evidence as their substitute.
25. Treating a multi-thousand-line `lib.rs`, `main.rs`, `mod.rs`, `utils.rs`, or fixture file as the permanent home for unrelated runtime, protocol, state-machine, platform, and test responsibilities.
26. Performing a blind repository-wide file split without first stabilizing behavior, mapping ownership, preserving public APIs, and validating each crate incrementally.
27. Introducing any first-party JavaScript or TypeScript runtime—Node.js, Bun, or Deno—into a deployable, especially at the authentication boundary. A runtime's implementation language does not exempt it: what matters is the TypeScript source and npm dependency graph it brings inside the security perimeter.
28. Waiting for control-cloud availability, provider login, complete history, every indicator, or every hidden workspace tab before painting an interactive local shell.
29. Treating cached quotes, DOM, account state, or positions as live truth after restart, or enabling order entry before provider reconciliation.
30. Scanning or rebuilding an entire local history store for every launch, chart, symbol change, timeframe change, pan, or zoom.
31. Putting Redis, PostgreSQL, ClickHouse, S3, Redpanda, or another network dependency in desktop startup, chart hydration, live display, or direct order routing.
32. Routing, storing, logging, or retransmitting user orders, account state, positions, or fills through Axiusflow cloud before a separate execution/custody/compliance architecture gate permits the exact payload class.
33. Claiming safe live trading across devices or applications without provider-enforced exclusivity, or without both a fresh opaque Axiusflow cross-device lease and provider-enforced account-wide hard risk covering every writer; local process generations alone do not provide distributed fencing.
34. Enabling live order dispatch with the only intent/OMS/ledger evidence on one device; same-device durability is not total-device-loss recovery.

---

## 22. Governing architecture decisions

| Area | Decision |
|---|---|
| Primary implementation | Rust only; no first-party JavaScript or TypeScript runtime |
| Authentication authority | First-party Rust `auth_service`; Node.js, Bun, and Deno are all excluded |
| Native desktop operating systems | Windows, Linux, and macOS |
| Current market-data topology | User device connects directly to the user's entitled provider; Axiusflow cloud is not in the market-data path |
| Current order topology | User device commits crash-safe local intent and routes directly through the certified provider/broker session; Axiusflow cloud is not in the order path |
| Portable client baseline | Provider-certified WSS/TLS plus Protobuf over evented OS networking |
| Preferred optional provider transport | Certified native SDK only after canonical equivalence and end-to-end measurement |
| Consumer acceleration | Bounded queues, off-UI decode/model workers, storage reuse, shared immutable generations, frame-aligned conflation, and measured OS/runtime tuning; no AF_XDP or DPDK |
| Future cloud data | Planned but disabled; requires separate provider rights, compliance, economics, security, and measured-benefit gate |
| Naming | `snake_case` for Axiusflow-owned identifiers, with documented language/external exceptions |
| Internal Rust module structure | `lib.rs` is a public façade; implementations are split by cohesive responsibility after current stabilization and before further major expansion |
| Refactoring sequence | Repair and validate existing behavior first, then decompose one crate at a time with API and performance preservation; all new capabilities start modular |
| Native UI | One exact-pinned GPUI source plus GPUI Component |
| Financial charting | Origin Charts only; shared engine and `ChartFrame` |
| Domain dependency direction | Domain/application remain GPUI-, provider-, transport-, cloud-, and OS-free |
| Operational planes | Control, real-time data, execution, and durable data |
| Market display path | Direct provider session → desktop-local canonical state → bounded local fanout; optional licensed local recording is never inline |
| Startup path | Local validated shell/workspace first; provider connection, visible history, indicators, hidden tabs, licensing refresh, and cloud sync proceed concurrently behind bounded workers |
| Desktop transactional store | Exact-pinned SQLite WAL/FULL selected by crash/corruption/disk-full/migration and latency evidence; history payloads remain separately encrypted immutable files |
| Live order recovery | Pre-dispatch intent requires independent acknowledgement before dispatch; during normal/zero-loss operation every later authoritative lifecycle/ledger transition is encrypted, ordered, synced, and independently acknowledged, while a declared target outage uses the bounded local backlog, emergency reserve, and explicit evidence-gap behavior in Section 9.2 without blocking safety actions; no order/account plaintext, ciphertext, credential, or location enters Axiusflow services; `local_only` is paper/shadow only |
| Desktop market cache | Rights-aware immutable checksummed history segments plus a bounded in-memory visible-range/shared-generation cache; stale live state never becomes truth |
| Runtime ordering | Fenced single writer per ordered partition within an enforceable authority scope; live cross-device/concurrent-session trading requires provider exclusivity, or both an opaque Axiusflow lease and provider-enforced account-wide hard risk covering every writer |
| Initial deployment model | Local-first desktop plus minimal asynchronous auth/control services; no cloud market-data or order gateway |
| Durable event backbone | Redpanda only for approved future cloud/Axiusflow-owned events, never RPC or an inline display barrier |
| Cloud authoritative transactions | PostgreSQL with outbox/inbox for approved cloud-owned state only |
| Future cloud analytics | ClickHouse rebuildable projections only after the cloud-data gate |
| Future cloud historical/raw data | S3 and Parquet only after the cloud-data gate |
| Future cloud ephemeral cache | Redis, never authoritative and never a desktop dependency |
| Future cloud client delivery | Reliable snapshots/events plus optional loss-tolerant datagrams by semantic class; disabled for the direct-provider product |
| Execution | Crash-safe local intent before direct provider dispatch; future managed/venue-adjacent execution requires a separate gate |
| Financial numerics | Checked fixed-point/integer domain types |
| Strategy isolation | Capability-restricted WebAssembly |
| Performance evidence | Boundary- and clock-qualified, checksummed, five-run hardware/profile evidence with publishable p99.9 only at ≥100,000 samples plus overload and recovery gates |
| First-party core safety | `unsafe_code = "forbid"`; accelerated dependency boundaries require separate review |

---

## 23. Definition of architectural success

The architecture is functioning as intended when:

1. Windows, Linux, and macOS pass the same native correctness, security, reconnect, and chart-semantic suite.
2. Rithmic/CQG provider messages travel directly to the user's device, and WSS/native adapters produce equivalent canonical events under applicable fixtures and certification suites.
3. A normalized event reaches desktop-local latest state and chart fanout without any Axiusflow cloud or disk acknowledgement; optional licensed local recording remains observable and independently bounded.
4. Every local ordered partition has one writer and stale session generations/epochs are rejected.
5. Slow UI consumers, local-recorder failure, and control-cloud outages cannot create unbounded memory or corrupt local market state.
6. Every accepted live order has crash-safe local intent, pre-dispatch independently recoverable encrypted evidence, provider-exclusive or provider-risk-bounded leased session evidence, provider correlation, explicit recovery-durability/gap status for every observed transition, and a reconciliation result without an Axiusflow cloud routing or storage hop.
7. Every portfolio metric traces to immutable ledger inputs and a versioned policy.
8. Every displayed market value retains source, sequence, quality, entitlement, and correction provenance.
9. Origin owns chart state once and the same frame contract renders through supported backends.
10. Provider adapters can be replaced without changing order, portfolio, chart, or analytics domains.
11. Performance claims include full named boundaries, hardware, network, OS, load, p99.9, overload, and recovery evidence.
12. Tick-to-pixel evidence includes model, GPUI host work, GPU submission, and physical presentation—not renderer submission alone.
13. A process, local partition owner, provider session, network, database, or optional durable consumer failure converges through documented recovery rather than guessing.
14. Provider and Axiusflow entitlements/rights are enforced at connection, local storage, export, replay, alert, and algorithmic boundaries; cloud receipt remains impossible until separately approved.
15. The initial platform remains operationally understandable; extraction follows evidence instead of fashion.
16. Every performance statement identifies its exact boundaries and clock domains, retains a reproducible checksummed artifact, reports tails and variance, and never converts provider timestamp age or component timing into a false end-to-end claim.
17. Current implementation claims, runtime readiness, configuration, and evidence agree; fixtures and probes are never presented as production capability.
18. Large crates expose readable `lib.rs` façades and cohesive internal modules with one discoverable owner for every decoder, state machine, queue, platform resource, and fixture family.
19. New features extend the correct module boundary rather than recreating monolithic root files.
20. The terminal paints a validated local shell and workspace without waiting for control cloud, provider login, complete history, indicators, or hidden tabs; visible data hydrates first and hands off to live state without gaps, duplicates, flicker, or stale-truth ambiguity.
21. Cold/warm startup, workspace restoration, visible-chart readiness, symbol/timeframe changes, pan/zoom, cache recovery, and offline/control-outage behavior pass named cross-platform evidence gates with bounded CPU, memory, I/O, and provider requests.

---

## 24. Research, source, and licensing note

This revision was checked against official or primary material for [Rithmic API interfaces](https://www.rithmic.com/apis), [CQG Web API](https://partners.cqg.com/api-resources/web-api), [SQLite WAL](https://www.sqlite.org/wal.html), [Linux AF_XDP](https://docs.kernel.org/next/networking/af_xdp.html), [Linux PTP clocks](https://docs.kernel.org/driver-api/ptp.html), historical [DPDK Linux deployment](https://doc.dpdk.org/guides/linux_gsg/index.html), [Windows IOCP](https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports), [Windows RIO](https://learn.microsoft.com/en-us/windows/win32/winsock/riorqueue), [Apple Network.framework](https://developer.apple.com/videos/play/wwdc2018/715/), [QUIC](https://www.rfc-editor.org/info/rfc9000/), [QUIC DATAGRAM](https://www.rfc-editor.org/info/rfc9221/), and [GPUI cross-platform product availability](https://zed.dev/blog/gpui-2-on-preview).

External capabilities are architecture inputs, not evidence that Axiusflow has implemented or benchmarked them. Exact dependency/library selection requires compatibility, security, license, maintenance, and performance evaluation at implementation time. Commercial availability, market-data rights, broker behavior, and regulatory responsibility require direct agreements and legal review.

Content was rephrased for compliance with licensing restrictions.
