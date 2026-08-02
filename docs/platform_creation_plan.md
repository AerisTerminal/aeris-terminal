# Axiusflow Platform Creation Plan

**Status:** Working implementation and stabilization plan, revision 5 (checkbox-tracked)
**Audience:** Product engineering, platform engineering, security, compliance, data engineering, and technical leadership
**Primary language:** Rust
**Supported native operating systems:** Windows, Linux, and macOS
**Last updated:** 2026-08-01

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
- [14. Infrastructure and acceleration profiles](#14-infrastructure-and-acceleration-profiles)
- [15. Concurrency, memory, and hot-path mechanics](#15-concurrency-memory-and-hot-path-mechanics)
- [16. Performance model and release gates](#16-performance-model-and-release-gates)
- [17. Observability and operations](#17-observability-and-operations)
- [18. Security, resilience, and supply chain](#18-security-resilience-and-supply-chain)
- [19. Testing and certification](#19-testing-and-certification)

**Part III — Tracked execution work**

- [20. Delivery sequence from the current foundation](#20-delivery-sequence-from-the-current-foundation)
  - [Stage 0A: Stabilize and correct the existing foundation](#stage-0a-stabilize-and-correct-the-existing-foundation)
  - [Stage 0B: Cohesively decompose corrected monolithic crates](#stage-0b-cohesively-decompose-corrected-monolithic-crates)
  - [Stage 1: Complete the cross-platform and accelerated ingest foundation](#stage-1-complete-the-cross-platform-and-accelerated-ingest-foundation)
  - [Stage 2: Connected read-only market data](#stage-2-connected-read-only-market-data)
  - [Stage 3: Durable controlled execution](#stage-3-durable-controlled-execution)
  - [Stage 4: Professional terminal and transport](#stage-4-professional-terminal-and-transport)
  - [Stage 5: Venue-adjacent scale and global expansion](#stage-5-venue-adjacent-scale-and-global-expansion)

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

- **Structural decisions** are intended to remain stable: Rust-first domains, provider-neutral contracts, Origin Charts, GPUI boundaries, fixed-point financial values, authoritative PostgreSQL state, Redpanda durability, replay, entitlement enforcement, and reconciliation.
- **Runtime topology and acceleration profiles** evolve only through measured architecture decisions. A transport, queue, allocator, kernel interface, or deployment boundary is never permanent merely because it appeared in the first implementation.

Axiusflow supports Windows, Linux, and macOS as first-class native desktop targets. The portable path must remain fully functional and correct on all three. Linux-only acceleration can provide lower latency for compatible direct-feed, on-premises, or dedicated-host deployments, but it cannot become a requirement for using the product.

The implementation may be delivered in vertical slices, but every slice must fit these contracts. Delivery sequence is not permission to create fake external services, disposable domain models, unbounded queues, a second chart state model, or a new monolithic crate.

### 1.1 Current implementation baseline

This baseline records the audited state, not a promise about work that may be changing concurrently. Before modifying a component, the implementing agent must inspect the live Git status and diff, preserve unrelated work, and update this table only from source and validation evidence.

| Area | Current reality | Required next gate |
|---|---|---|
| `portable_network` | Real bounded UDP driver using `UdpSocket`, preallocated slots, and explicit release/drop accounting | Preserve as the portable correctness reference and complete platform/provider qualification |
| `linux_socket_network` | Real `socket2`-based tuned Linux UDP path, but without complete RSS, affinity, busy-poll, kernel/hardware timestamp, or qualified-host evidence | Complete declared tuning features or narrow the declared capability; then benchmark on a named host profile |
| `linux_af_xdp` | Exact-pinned `xsk-rs` copy-mode integration receives real veth frames in a privileged container, releases every descriptor, and rebinds the same queue across repeated lifecycles after the vendored libxdp ring-mapping defect was corrected, and survives a seeded adversarial data-path exercise over the live socket with exact release accounting; zero-copy, qualified NIC/driver, and provider evidence remain absent | Complete an independent unsafe-boundary audit, then qualify real NIC/driver hardware; report copy mode honestly and never claim zero-copy |
| `linux_dpdk` | Prerequisite probing and explicit unavailable behavior; no verified native poll-mode driver path | Implement and validate native queue/mbuf lifecycle before advancing beyond `implemented`; do not equate a probe with packet processing |
| Windows/macOS network adapters | Fixture drivers; native mode is explicitly unimplemented | Implement only behind the shared ingest contract and certify independently; never present fixtures as native adapters |
| `transport`, `realtime`, protocols, recovery, and queues | Substantive bounded contracts and implementations exist, still concentrated in large files; `crates/testing` is now decomposed into sixteen cohesive modules behind a façade | Correct current defects, preserve semantics, then split by cohesive responsibility |
| `persistence` | Transactional inbox/outbox traits with first-party staging-order and idempotent-receipt tests; no database implementation | Add real PostgreSQL transactions, migrations, outbox/inbox behavior, recovery, and integration evidence |
| `platform_runtime` | Capability ports, a standard clock, a native credential vault backed by Linux Secret Service, macOS Keychain, or Windows Credential Manager, independently signed update-manifest and streamed-artifact verification with verifier-bound provenance, one-key trust-domain confinement, monotonic anti-downgrade, and verified-predecessor rollback state, fallible native affinity and non-real-time current-thread priority hints on Linux that refuse real-time callers and keep their balanced baseline privilege-free, a real RFC 7636 PKCE secret generator that redacts both verifier and state, and a loopback redirect listener that bounds accept, every read, and each connection against one absolute deadline, verifies `state` before any error path, and survives stray traffic, stalled peers, and browser disconnection, decomposed into thirteen cohesive modules with seventy-three first-party tests, including runtime capability detection that reports only the ports this crate actually implements, a Unix crash-recoverable content-addressed update staging journal that durably preserves one signed active release and one rollback predecessor beneath a caller-provisioned root, bounded Linux logind suspend/resume notifications, a registered URI-scheme callback that verifies delivered redirects and registers a Linux XDG scheme handler, and a Linux Wayland display probe that reports per-output refresh, geometry-derived fractional scale, and the presentation feedback clock; per-OS installer activation, code-signature/notarization enforcement, Windows/macOS power and display, and Windows/macOS priority and affinity remain incomplete | Complete per-OS installer activation and code-signature/notarization enforcement, Windows/macOS power and display adapters, and safe Windows/macOS priority and affinity backends behind the existing capability ports |
| `terminal_ui` and desktop | Bounded UI infrastructure and a fixture/disconnected desktop path | Build a connected production runtime without putting network/decode work on the GPUI thread |
| Authorization service | Intentionally inactive and starts no listener | Implement the approved authentication/authorization boundary and prove startup, key, session, revocation, and failure behavior |
| `auth_service` | Not created; `crates/security` already verifies Ed25519 JWTs against revisioned JWKS snapshots | Implement the first-party Rust authentication service per Section 3.2 with no JavaScript runtime; review each exact-pinned credential dependency before adoption |
| Origin Charts | Existing major external engine integrated through an Axiusflow bridge | Treat it as a separate exact-pinned dependency and compatibility boundary; do not count its engine implementation as newly completed Axiusflow work |
| Product verticals | No complete live provider, durable data deployment, OMS/risk/ledger, broker connection, reconciliation runtime, or full production terminal | Implement through Stages 2–5 after the corrected foundation and mandatory decomposition gate |

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
| Stage 1 | Complete cross-platform and accelerated ingest foundation | `in_progress` | Contracts, fixtures, portable/tuned implementations, the privileged AF_XDP copy lifecycle, and 131 first-party tests across transport, adapters, realtime, security, authorization, persistence, and platform runtime pass; the native credential vault, signed update verification/rollback state, Unix crash-recoverable artifact staging journal with durable signing-key-rotation recovery, Linux logind suspend/resume notifications, scheduling hints, PKCE, and its loopback callback are implemented at their declared OS scope, while per-OS installer activation and code-signature/notarization enforcement, Windows/macOS power adapters, Windows/macOS display adapters, Windows/macOS priority and affinity policies, native Windows/macOS ingest, renderer-submission measurement, and PTP hardware timestamping are incomplete. |
| Stage 2 | Connected read-only market data | `not_started` | No live authorized provider path, durable branch deployment, or projections. |
| Stage 3 | Durable controlled execution | `not_started` | No OMS, risk, ledger, broker, or reconciliation runtime. |
| Stage 4 | Professional terminal and transport | `not_started` | Desktop remains a fixture/disconnected path. |
| Stage 5 | Venue-adjacent scale and global expansion | `blocked_external` | Requires qualified hardware, colocation, and authorized direct feeds. |

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

- The first-party Rust authentication service owns credentials and authentication sessions.
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

Broker, exchange, vendor, GPUI, operating-system, cloud SDK, AF_XDP, DPDK, and transport-library types stop at adapters. Internal commands and events use versioned Axiusflow contracts. The same canonical event must be produced regardless of portable socket, Windows optimization, macOS optimization, AF_XDP, or DPDK ingress.

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

References: [RFC 9106 Argon2](https://www.rfc-editor.org/info/rfc9106/), [RFC 7636 PKCE](https://www.rfc-editor.org/info/rfc7636/), [OpenID Connect Core](https://openid.net/specs/openid-connect-core-1_0.html), [W3C Web Authentication](https://www.w3.org/TR/webauthn-3/), [RFC 7519 JWT](https://www.rfc-editor.org/info/rfc7519/), and [RFC 7517 JWK](https://www.rfc-editor.org/info/rfc7517/).

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

1. `auth_service` — first-party Rust authentication service: credentials, sessions, token issuance, and JWKS publication.
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

### Stage 1: Complete the cross-platform and accelerated ingest foundation

Complete, correct, and validate the remaining work in parallel where dependencies permit:

- [x] `S1-01` explicit `platform_runtime`, `transport`, `realtime`, and `ingest_driver` ports — status: `done` (ports present; `platform_runtime` OS adapters are tracked separately as `S1-14`)
- [x] `S1-02` separate `portable_socket`, `tuned_linux_socket`, `linux_af_xdp`, and `linux_dpdk` adapter crates and build targets — status: `done` (four isolated adapter crates exist; native completeness tracked by `S0A-06`, `S0A-07`, `S0A-08`)
- [x] `S1-03` a bounded borrowed receive-batch contract with explicit buffer lifetime, release, timestamp-source, overflow, and capability semantics — status: `done` (implemented in `crates/transport` with explicit release and drop accounting)
- [ ] `S1-04` exact-pinned accelerated dependencies plus safety, license, provenance, fuzzing, and maintenance review — status: `partial` (pins and recorded safety/license/provenance/maintenance review text exist. Review found and corrected three real defects: a first-party UMEM/socket drop-order defect, the upstream libxdp 1.6.0 refcount-map descriptor leak, and the libxdp ring-mapping defect that left RX/TX/fill/completion mappings alive whenever the caller moved the ring structs, which held the kernel XSK pool and produced `EBUSY` on rebind. `tools/verify_libxdp_patch.py` verifies the patched tree hash and proves all ten reviewed hunks reconstruct the exact crates.io source. Two deterministic native-copy boundary matrices, three privileged repeated-bind regressions, and a 1,024-unit coverage-guided pre-open libFuzzer run pass. The DPDK dependency review is complete with a recorded decision: the 2026-08-02 survey of published candidates (the abandoned lemonrock family, single-release `rpkt-dpdk`, placeholder `dpdk-stdlib`, wrong-layer `teto-dpdk`, and unproven `dpdk-net`) found none meeting the exact-pin, real-RX/TX, no-stub, and maintenance bar, so `linux_dpdk` keeps `native_dependency=NotSelected` and future DPDK work will generate first-party bindgen bindings against one exact-pinned vendored DPDK release following the `third_party/libxdp-sys` pattern. A privileged seeded adversarial data-path lane now drives the live copy-mode socket through `--af-xdp-copy-fuzz` with reproducible seeds: 64 rounds transmitted 1,978 malformed, runt, VLAN, random-ethertype, and oversized frames over a 9,216-byte-MTU veth pair while asserting batch and per-frame size bounds, monotonic overflow accounting, exact release accounting, clean shutdown, and a successful post-exercise queue rebind; the lane passes in the hardened container and on the hosted privileged job, writes schema-v1 evidence naming itself `stage_1_af_xdp_copy_data_path_fuzz`, and honestly records `coverage_guided=not_claimed`. Both docker lanes now add `DAC_OVERRIDE` because the evidence bind mount is otherwise unwritable under `cap-drop ALL` on a rootful daemon. Upstream reporting or adoption of released libxdp fixes and an independent unsafe-boundary audit remain outstanding)
- [x] `S1-05` Windows, Linux, and macOS build/smoke matrix for the pinned GPUI source and portable path — status: `done_pending_commit` (in GitHub Actions run 30710769490 on `bcd7529` the `portable_windows-latest`, `portable_macos-latest`, and `portable_ubuntu-latest` jobs each pass `portable_contracts_and_real_socket_build`, the loopback packet-to-partition-fanout-Origin conformance run, evidence retention, and `pinned_gpui_desktop_compile_smoke`, and `portable_cross_platform_evidence_gate` passes over the three retained artifacts. This proves cross-platform build, portable socket, and compile-smoke parity only; it measures no renderer submission, presentation, or latency)
- [ ] `S1-06` Linux compile/conformance lanes using software-accessible AF_XDP modes and DPDK software or virtual devices where runner capabilities permit — status: `partial` (`tools/run_af_xdp_copy_conformance.sh` runs the privileged lane in a disposable network-isolated container with all capabilities dropped except BPF, IPC_LOCK, NET_ADMIN, NET_RAW, SYS_ADMIN, and SYS_RESOURCE, a private bpffs via `LIBXDP_BPFFS`, deterministic veth MAC addresses, and a bounded `AF_PACKET` replay sender that asserts its transmitted packet count. The lane now passes end to end and writes schema-v1 evidence with `renderer_submission=not_measured` and `physical_presentation=not_measured`; the three privileged repeated-bind regressions pass in the same namespace. DPDK software/virtual-device lifecycle remains unexercised, so this item stays incomplete)
- [x] `S1-07` a machine-readable, runtime-enforced readiness manifest that prevents activation or claims above each profile's proven readiness state — status: `done` (fifteen `crates/transport` unit tests assert the guard directly: fixtures cannot claim above `implemented`, AF_XDP cannot reach `production_enabled`, DPDK is capped at `contract_only`, unreviewed evidence identifiers are rejected, modes absent from reported capabilities are rejected, capability/profile mismatch is rejected, malformed manifest JSON is rejected, and the embedded manifest validates with its readiness ordering intact; `cargo test --locked --workspace --all-targets` passes and schema-v5 Linux evidence agrees with `config/ingest_readiness.json`)
- [x] `S1-08` deterministic Ethernet/IP/UDP/TCP/provider fixtures proving identical canonical events, gaps, errors, and replay across applicable profiles — status: `done` (fixture corpus and conformance harness in `crates/testing` and `tools/ingest_conformance`)
- [x] `S1-09` canonical timestamp vocabulary and latency recorder — status: `done` (recorder implemented; hardware timestamp capability tracked by `S1-10`)
- [ ] `S1-10` hardware timestamp capability reporting and validation — status: `blocked_external` (requires a PTP-capable NIC and qualified host)
- [x] `S1-11` fenced single-writer partition contract — status: `done` (fenced partitions with ownership epoch and stale-writer rejection)
- [x] `S1-12` direct-fanout and durable-tap interfaces using deterministic replay — status: `done` (bounded direct and durable queue interfaces with replay)
- [ ] `S1-13` full replay-to-GPUI frame benchmark, including model and host work — status: `partial` (`run_replay_to_gpui_host_benchmark` measures 64 iterations after 1 warmup and reports p50/p95/p99/p99.9/max for three real stages: binary decode plus client model, Origin frame construction, and GPUI scene planning through `GpuiChartRenderer::plan_frame`. Each iteration asserts immutable generations, that Origin consumed the latest sequence, and that the prepared frame produced primitives and plan operations. `OriginGpuiHostSample` sets `renderer_submission_performed=false` and `physical_presentation_measured=false`, and both the console line and schema-v5 evidence record `renderer_submission=not_measured` and `physical_presentation=not_measured`. Completion requires a windowed GPU submission and presented-frame measurement, which the current disconnected desktop path does not provide)
- [ ] `S1-14` `platform_runtime` operating-system adapters: credential vault, PKCE callback, update/rollback, power, display, and scheduling — status: `partial` (the credential vault, signed update-verification core, rollback state, native scheduling hints, and PKCE callback capabilities are implemented and tested. `NativeCredentialVault` uses exact-pinned `keyring 4.1.6` binary-secret APIs and process-wide serialized access to Linux Secret Service, macOS Keychain, or Windows Credential Manager; service and key components use independently typed base64url backend identifiers so Windows' dot-joined credential target cannot collide across vault namespaces. Missing-entry loads return `None`, deletion is idempotent, and identifiers reject empty or control-bearing values. Three backend-isolated tests validate binary round trips, collision resistance, identifier handling, and redaction without altering a host keychain. `SignedUpdateVerifier` verifies an independently signed, strict 4 KiB manifest with Ed25519 before parsing, binds the verified manifest to a domain-separated fingerprint of the authenticating key and anti-downgrade floor, enforces a monotonically increasing release sequence, and streams an exact-size artifact of at most 8 GiB through SHA-256 while rejecting cross-verifier manifest mixing, truncation, trailing bytes, and digest mismatch. `UpdateRollbackState` retains exactly one locally verified predecessor in the same signing-key trust domain, rejects impossible retained slots after a completed rollback, restores only that predecessor, never lowers its anti-downgrade sequence floor, and validates restoration of persisted active, rollback, trust-domain, and floor state. Seven verifier/state tests cover valid binary artifacts, signature/schema/tamper/downgrade rejection, verifier-provenance isolation, trust-domain confinement, artifact boundary failures, interrupted-read retry, rollback invariants, and restart hydration. `DurableUpdateActivator` is Unix-only because this safe Rust boundary cannot issue a Windows directory-metadata durability barrier; beneath an existing caller-provisioned root, it exclusively locks an 8 MiB bounded append-only journal, syncs exact rehashed artifacts into content-addressed release files before syncing each activation record and affected directory, persists the original signed manifest and signature rather than trusting reconstructed metadata, and recovers only a contiguous sequence of valid activation or one-slot rollback transitions. Recovery truncates an interrupted final append, clears stale staging files, re-verifies every manifest against the supplied independently managed signing key, requires that verifier's trusted floor to equal the latest durable floor, and rehashes every retained artifact before returning its path. Failed staging does not change committed state, a failed journal append forces reopen before another mutation, and the logical state retains one predecessor while never lowering its sequence floor. Fourteen focused tests cover activation/rollback restart recovery, repeated-activation artifact cleanup, the required pre-existing root, fixed-entry symlink rejection during initialization and recovery, interrupted-initialization artifact cleanup, partial-append cleanup, exclusive ownership, failed staging, bounded artifact validation, artifact and signing-key tampering, impossible signed transitions, signed-manifest metadata tampering, and exact floor binding. `SignedUpdateVerifier::authorize_rotation` verifies a strict statement of at most 1 KiB with the current Ed25519 key before parsing, binds the named current-key identity, rejects floor regression, weak or reused successor keys, unknown fields, and chains beyond 16 keys, and carries the root trust-domain identity into successor manifests so one in-memory predecessor can roll back across an authorized key boundary. Public identity and floor accessors provide the exact statement inputs without exposing private material. Two tests cover authorized successor verification with cross-key rollback and tamper/floor/reuse/size rejection. The Unix journal persists each release with its cumulative signed authorization chain as `KeyRotationProof` records and, on recovery, replays that chain from the independently managed root key alone through `replay_rotations`, which refuses replay onto an already-rotated verifier and never lowers the trusted floor; every persisted manifest is then re-verified by the rebuilt successor key, and a rotated activation must extend rather than diverge from its predecessor chain. Activation rejects a candidate whose proof chain does not extend the committed chain before staging or appending anything, so recovery can never reject a record this adapter committed, and an empty chain is omitted from the record so ordinary same-key stores keep emitting byte-identical schema-1 records. A record that carries authorization proofs declares activation schema 2 instead, because a predecessor binary without rotation replay cannot verify a rotated manifest at all; it therefore reports an unsupported journal schema rather than misreporting a valid journal as corrupt, and recovery requires each record to declare exactly the schema its contents imply. The committed chain is tracked as an independent high-water mark that rollback restores an older release beneath but never lowers, so a committed rotation can be neither abandoned nor replaced by a sibling branch, and recovery validates each journal transition against that same mark. Four regressions durably activate an authorized rotated release, recover it from the root key across a restart, roll back to the pre-rotation predecessor, reopen that cross-key state again, prove a root-signed sibling release is refused both immediately after rotation and after a rollback while the committed state stays recoverable, and assert the exact on-disk schema of same-key and rotated records. Windows crash-safe staging, per-OS installer switching, executable code-signature and macOS notarization enforcement, and launcher integration remain incomplete, so durable `rollback` capability is still reported unavailable. `NativePowerMonitor` uses exact-pinned `zbus 5.18.0` to install a sender/path/interface/member-specific system-bus match for systemd-logind `PrepareForSleep`, keeps at most 16 queued messages, maps the documented boolean body to `Suspending` and `Resumed`, and exposes transport, malformed-body, closed-stream, and unsupported-platform failures instead of inventing transitions. The blocking listener is Linux-only and documented for a background thread; Windows and macOS remain unavailable. Two deterministic tests cover event mapping and target capability reporting, and `busctl` on this host confirms `org.freedesktop.login1.Manager.PrepareForSleep` has the expected boolean signature. `NativeThreadScheduler` uses exact-pinned `core_affinity 0.8.3` and `thread-priority 3.1.1`: it discovers and deduplicates affinity targets per calling thread rather than caching one thread's allowed set, lets the kernel validate each requested target so a thread already narrowed to one processor can still move to another permitted target, and exposes separately fallible priority and affinity operations so partial application is never hidden as atomic. Affinity is reported available on Linux only. macOS reports affinity unavailable because `core_affinity 0.8.3` does not obtain a valid Mach thread port, and Windows reports it unavailable because that backend is not processor-group-aware on hosts above 64 logical processors. Non-real-time priority hints are reported available on Linux only and remain independently usable there even when affinity discovery is empty. Windows and macOS report priority unavailable: neither exposes a real-time-class check reachable without `unsafe`, which this workspace forbids, so raising priority there could silently produce a real-time base priority. The balanced baseline preserves the thread's existing normal priority, so it never requires privileges and never fails on a thread already started at a positive nice value, yet it still consults the real-time policy guard rather than reporting success for a real-time caller; the responsive and latency-sensitive hints request values 65 and 80, raise priority, and surface the operating-system EPERM without CAP_SYS_NICE or a raised RLIMIT_NICE instead of silently reporting success. A Linux thread already running under SCHED_FIFO or SCHED_RR is refused outright rather than having these values reinterpreted as real-time priorities, and a thread under SCHED_IDLE is refused because Linux ignores niceness for that policy, so success would not raise effective priority. The macOS backend additionally maps normal-policy values through process-wide niceness rather than a current-thread-only control. Three backend-isolated and discovery tests validate support reporting, bounded mappings, priority independence from empty affinity discovery, and OS rejection without mutating the test runner's scheduling. A direct unprivileged Linux probe on this host reported 24 discovered targets, a successful balanced hint, EPERM for the responsive hint, and a successful move to a second target after the first pin, confirming the hints report real operating-system outcomes rather than assumed success. `PkceSecret` draws 32 bytes of verifier entropy and 32 independent bytes of CSRF state from the operating-system CSPRNG through `getrandom 0.4.3`, derives the challenge as `URL_SAFE_NO_PAD(SHA-256(verifier))`, hard-codes `S256` so `plain` cannot be selected, and redacts both the verifier and the state from `Debug` because a leaked state permits a forged state-verified redirect. `LoopbackRedirectListener` binds an ephemeral `127.0.0.1` port and keeps serving redirects until one proves knowledge of `state` or the deadline elapses, enforced by a non-blocking accept loop against a single absolute `Instant` expiry. Every read is bounded by that same expiry through a deadline-aware reader, and each connection may consume at most 2 s of it; up to eight connections are served concurrently without accepting and dropping queued redirects when all worker slots are occupied. Per RFC 6749 section 4.1.2.1 it verifies `state` in constant time *before* honoring an `error` response, and reduces the reported code to at most 64 alphanumeric/`_`/`-` characters so a redirect cannot inject text into diagnostics. The browser response is best-effort, so a tab that disconnects after submitting a verified redirect cannot discard the authorization code. A forged state, missing state, foreign path, non-GET method, oversized request line, or header flood is answered `400` without aborting the pending sign-in; only a `state`-verified authorization-server error or a verified redirect missing its code aborts. Request bounds are explicit and exact: an 8,192-byte request line and up to 64 header lines of 8,192 bytes each, with exactly 64 headers accepted. Twenty callback tests pass, thirteen of them integration tests driving a real loopback socket, covering deadline enforcement, slow-drip resistance, stalled-peer starvation, saturated-worker queue preservation, stray-traffic resilience, browser disconnection, the exact header bound, and error sanitization. `RuntimeCapabilities::detect_native` derives reported capabilities from the implemented backends instead of a hand-written literal, so durable `rollback` and hardware timestamping stay `Unavailable`; Linux display timing reports the implemented Wayland probe, Linux registered URI-scheme registration reports the implemented XDG handler; Linux power notifications report the implemented logind listener, while the PKCE loopback callback, signed-update verification, and Linux priority/affinity hints report their real state. Credential-vault capability detection reports only targets with a compiled native adapter and deliberately leaves store initialization lazy, so transient Secret Service or user-session startup failures remain retryable through normal vault operations; two tests assert that no unimplemented port is ever claimed and that supported targets advertise only their compiled adapter. `RegisteredUriRedirect` validates a private redirect URI, refusing reserved `http`/`https`/`file`/`ftp` schemes that would intercept ordinary web navigation, non-RFC-3986 schemes, and a URI that already carries a query or fragment. It verifies a delivered redirect under the same rules as the loopback listener by reusing its shared percent-decoding, constant-time comparison, and error-sanitizing helpers: `state` is compared in constant time before any `error` response is honored, the redirect target must match exactly, the URI is bounded to 2 KiB, and the code is redacted from `Debug`. The registered base is additionally bounded below the 2 KiB delivered limit by a 512-byte response reserve, so a base can never be accepted that no verifiable redirect could fit, and the post-scheme remainder must be valid RFC 3986 path syntax rather than merely non-empty. `NativeUriSchemeRegistrar` is Linux-only and writes an XDG `x-scheme-handler` desktop entry beneath an existing caller-provisioned data directory through a uniquely named synced temporary file and rename, syncing both that directory and, when it creates it, its parent, and treating a concurrent directory creation as the same successful end state, so neither a crash nor a concurrent registration can leave a partial or unreferenced handler. The executable path is serialized per the Desktop Entry `Exec` rules, quoting the argument, doubling `%`, and emitting four backslashes for one literal backslash because generic string parsing precedes command-line parsing. `write_entry_in` performs only that durable write, while `register_in` also refreshes the desktop MIME database and installs the entry under `[Default Applications]` in a caller-provided `mimeapps.list`, because rebuilding the capability cache only advertises that an entry *can* handle a scheme and does not route it; that rewrite preserves every other association and section, replaces only this scheme's default, is bounded to 256 KiB, and lands through its own synced temporary file and rename. The complete read-modify-write is serialized across processes by an exclusive lock on a sibling file, so two concurrent registrations cannot lose each other's association, and every temporary name mixes the process identifier, a nanosecond timestamp, and a counter with retry so a leftover file from an earlier crash cannot block a later registration after identifier reuse. It reports an explicit unavailable-tool, refresh-failure, or unreadable-associations error instead of advertising an inactive handler as registered; Windows and macOS report the capability unavailable. Registration is one lock-guarded transaction: a single exclusive lock spans the snapshot, replacement, activation, and rollback, so concurrent registrations of the same identifier cannot snapshot the same prior entry and let a failing one undo a successful one. A failing activation step never destroys a working handler: the existing entry and the previous associations are both captured before replacement and atomically restored on failure, an entry the call created is removed instead, and the capability cache is refreshed again against the restored state while the activation error stays primary. Nine tests cover scheme and remainder validation including malformed percent escapes, the response-reserve bound, a verified code, missing/forged state, foreign targets, oversized URIs, sanitized state-verified server errors, entry content with spaced, percent-bearing, and backslashed executables, identifier/executable rejection, absence of partial entries and association files, default-handler installation that preserves unrelated associations inside private test-owned data and config homes, three concurrent association updates that all survive, and restoration of a previous handler after a failed activation. `NativeDisplayProbe` reads Linux display state from the Wayland session: `wl_output` at version 4 supplies current-mode refresh in millihertz, physical pixels, integer scale, and connector names, the XDG output manager supplies logical geometry so the effective scale is derived from the physical-to-logical ratio in thousandths, capturing fractional scales the integer event cannot express, and `wp_presentation` reports whether presented-frame feedback shares the `CLOCK_MONOTONIC` domain used by the latency recorders, with unrecognized clocks flagged so they are never compared against local records. A live probe on this host returned `DP-3` at 164,957 mHz with 2560x1440 physical pixels against 2048x1152 logical, deriving the true 1.25x scale while the integer event announced 2, and a monotonic presentation clock; per-frame feedback requires a committed surface, so it remains the UI layer's responsibility and only the clock is probed. Zero logical geometry is rejected rather than divided by, a missing compositor or Wayland library reports `NoSession` instead of invented geometry, and Windows, macOS, and X11 sessions report the port unavailable. Six deterministic tests cover scale derivation, fallback, zero-geometry rejection, refresh-interval reciprocals, and clock classification, and one session test accepts either a real probe with sane refresh and scale bounds or an explicit `NoSession` on a headless runner. The crate is decomposed into `capability`, `clock`, `credential_vault`, `display_timing`, `paths`, `composition`, `pkce`, `power_notifications`, `signed_update`, `signed_update::activation`, `thread_scheduling`, `uri_callback`, and `loopback_callback` behind a 46-line façade. Per-OS installer activation and code-signature/notarization enforcement, Windows/macOS power and display, and Windows/macOS current-thread priority and affinity adapters remain incomplete, so this item stays unchecked)
- [ ] `S1-15` native Windows and macOS ingest adapters behind the shared ingest contract — status: `not_started` (fixture drivers; `NATIVE_MODE_IMPLEMENTED = false`)
- [x] `S1-16` bounded client semantic classes and snapshot recovery — status: `done` (semantic delivery classes, snapshots, and recovery implemented)
- [x] `S1-17` first-party unit-test coverage for readiness enforcement, bounded frame decoding, and explicit accelerated unavailability — status: `done` (62 focused default-feature tests pass: 15 transport, 8 portable socket, 7 AF_XDP, 6 DPDK, 6 tuned Linux socket, 5 security, 4 authorization, 4 realtime, 4 persistence, and 3 platform runtime. Native-copy adds two deterministic AF_XDP configuration/lifecycle boundary tests, and `crates/adapters/linux_af_xdp/tests/sequential_rebind.rs` adds three privileged regressions. The descriptor-release regression passes; immediate sequential rebind remains the qualified-host/kernel blocker in `S0A-06`, not a hidden or claimed pass)
- [ ] `S1-GATE` exit criteria met — status: `not_started`

Exit criteria: the same applicable packet/replay corpus produces equivalent canonical and Origin state across profiles and operating systems; no domain, decoder, partition, or fanout contract assumes socket-owned memory; every queue and timestamp boundary is visible; AF_XDP and DPDK reach at least `fixture_validated` with unavailable hardware/provider evidence recorded honestly; the portable path is eligible to advance independently toward `production_enabled`; and Redpanda is not required to demonstrate the direct path.

### Stage 2: Connected read-only market data

Deliver:

- [ ] `S2-01` first legally authorized live provider adapters using the best currently affordable managed, sandbox, delayed, or real-time source available under its terms — status: `not_started`
- [ ] `S2-02` production ingest through `portable_socket` and, on dedicated Linux hosts, `tuned_linux_socket` — status: `not_started`
- [ ] `S2-03` AF_XDP and DPDK provider integration when a compatible authorized packet feed is available, without redesigning their Stage 1 contracts — status: `blocked_external` (requires a compatible authorized packet feed)
- [ ] `S2-04` explicit readiness and `unavailable` capability results for incompatible or unqualified profile/feed combinations, with no pretend acceleration or silent fallback — status: `partial` (AF_XDP and DPDK unavailable activation plus manifest agreement are verified; live provider/feed compatibility results remain unavailable until `S2-01`)
- [ ] `S2-05` direct latest-state fanout in production — status: `partial` (interfaces exist from `S1-12`; no live feed behind them)
- [ ] `S2-06` Redpanda durable branch and raw S3 capture — status: `not_started`
- [ ] `S2-07` binary WebSocket baseline in production — status: `partial` (adapter and runtime implemented; not deployed against a live provider)
- [ ] `S2-08` QUIC prototype behind negotiation — status: `not_started`
- [ ] `S2-09` ClickHouse projections and deterministic bars — status: `not_started`
- [ ] `S2-10` entitlement enforcement and resnapshot behavior on live streams — status: `partial` (authorization domain and snapshot recovery exist; no live stream enforcement)
- [ ] `S2-11` PostgreSQL persistence implementation behind the existing outbox/inbox traits, with migrations and recovery evidence — status: `not_started`
- [ ] `S2-12` running authentication and authorization service boundary — status: `not_started` (service intentionally exits without starting a listener)
- [ ] `S2-13` first-party Rust `auth_service` replacing the superseded Better Auth exception: password verification, OAuth/OIDC, native PKCE loopback, second factor and passkeys, PostgreSQL credential/session state, Ed25519 token issuance, and revisioned JWKS publication, with no JavaScript runtime in any deployable — status: `not_started` (Section 3.2 decision recorded; `crates/security` already verifies these tokens offline)
- [ ] `S2-14` safety, license, provenance, fuzzing, and maintenance review for each exact-pinned authentication dependency before adoption — status: `not_started` (candidates named in Section 3.2 are not integration evidence)
- [ ] `S2-GATE` exit criteria met — status: `not_started`

Exit criteria: sustained and burst workloads pass latency, loss, replay, slow-client, and semantic-equivalence gates for every profile declared `production_enabled`; the portable/tuned software and supported product features are production-complete within the current provider's declared rights, coverage, freshness, and availability without requiring a premium direct feed; delayed or sandbox data is never represented as suitable live-trading evidence; accelerated adapters remain implemented and fixture-validated but unclaimed until compatible hardware and provider evidence exists; every displayed event retains provenance.

### Stage 3: Durable controlled execution

Deliver:

- [ ] `S3-01` execution cell — status: `not_started`
- [ ] `S3-02` immutable policy and risk snapshots — status: `not_started`
- [ ] `S3-03` OMS state machine and durable order intent — status: `not_started`
- [ ] `S3-04` pre-trade risk engine and kill switches — status: `not_started`
- [ ] `S3-05` broker adapter and provider dispatch — status: `not_started`
- [ ] `S3-06` transactional outbox publication — status: `not_started`
- [ ] `S3-07` portfolio ledger input and immutable journal — status: `not_started`
- [ ] `S3-08` reconciliation runtime — status: `not_started`
- [ ] `S3-09` paper, shadow, restricted canary, then limited live trading progression — status: `not_started`
- [ ] `S3-GATE` exit criteria met: every crash, timeout, duplicate callback, and unknown-outcome scenario converges to an explainable reconciled state while meeting the regional execution budget — status: `not_started`

### Stage 4: Professional terminal and transport

Deliver:

- [ ] `S4-01` connected production desktop runtime replacing the fixture/disconnected path — status: `not_started`
- [ ] `S4-02` multi-chart and order-book workspaces — status: `not_started`
- [ ] `S4-03` scanners, advanced alerts, drawings, indicators, news, external calendars, and advanced analytics — status: `not_started`
- [ ] `S4-04` 120/144 Hz workload certification — status: `not_started`
- [ ] `S4-05` certified QUIC reliable-stream profile — status: `not_started`
- [ ] `S4-06` optional datagrams for approved semantic classes — status: `not_started`
- [ ] `S4-07` professional direct broker and FIX routes — status: `not_started`
- [ ] `S4-GATE` professional terminal and transport gates met — status: `not_started`

### Stage 5: Venue-adjacent scale and global expansion

The acceleration contracts and adapters already exist from Stage 1. This stage expands qualified production coverage rather than introducing kernel-bypass architecture for the first time.

Deliver:

- [ ] `S5-01` qualification evidence that promotes `linux_af_xdp` and `linux_dpdk` from `fixture_validated` through `hardware_validated`, `provider_certified`, and `production_enabled` without changing canonical contracts — status: `blocked_external`
- [ ] `S5-02` additional NIC, driver, firmware, and provider certification matrices for AF_XDP and DPDK — status: `blocked_external`
- [ ] `S5-03` multi-queue and multi-port scaling, hot-spare NICs, and fenced failover — status: `blocked_external`
- [ ] `S5-04` PTP/hardware timestamp deployment and continuous clock-quality alarms — status: `blocked_external`
- [ ] `S5-05` venue-adjacent execution and additional direct feeds — status: `blocked_external`
- [ ] `S5-06` multi-provider arbitration and global home-region expansion — status: `not_started`
- [ ] `S5-GATE` expansion promoted only on measured p99/p99.9 improvement without semantic, security, portability, or recovery regression — status: `not_started`

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
25. Treating a multi-thousand-line `lib.rs`, `main.rs`, `mod.rs`, `utils.rs`, or fixture file as the permanent home for unrelated runtime, protocol, state-machine, platform, and test responsibilities.
26. Performing a blind repository-wide file split without first stabilizing behavior, mapping ownership, preserving public APIs, and validating each crate incrementally.
27. Introducing any first-party JavaScript or TypeScript runtime—Node.js, Bun, or Deno—into a deployable, especially at the authentication boundary. A runtime's implementation language does not exempt it: what matters is the TypeScript source and npm dependency graph it brings inside the security perimeter.

---

## 22. Governing architecture decisions

| Area | Decision |
|---|---|
| Primary implementation | Rust only; no first-party JavaScript or TypeScript runtime |
| Authentication authority | First-party Rust `auth_service`; Node.js, Bun, and Deno are all excluded |
| Native desktop operating systems | Windows, Linux, and macOS |
| Portable client baseline | Evented Rust networking plus TLS binary WebSocket fallback |
| Preferred optional client transport | Negotiated QUIC/WebTransport after certification |
| Linux acceleration | Tuned sockets, AF_XDP, and DPDK are parallel Stage 1 implementations with explicit readiness states; portable product release is independent, and accelerated production activation remains feed- and hardware-qualified |
| Naming | `snake_case` for Axiusflow-owned identifiers, with documented language/external exceptions |
| Internal Rust module structure | `lib.rs` is a public façade; implementations are split by cohesive responsibility after current stabilization and before further major expansion |
| Refactoring sequence | Repair and validate existing behavior first, then decompose one crate at a time with API and performance preservation; all new capabilities start modular |
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
16. Current implementation claims, runtime readiness, configuration, and evidence agree; fixtures and probes are never presented as production capability.
17. Large crates expose readable `lib.rs` façades and cohesive internal modules with one discoverable owner for every decoder, state machine, queue, platform resource, and fixture family.
18. New features extend the correct module boundary rather than recreating monolithic root files.

---

## 24. Research, source, and licensing note

This revision was checked against official or primary material for [Linux AF_XDP](https://docs.kernel.org/next/networking/af_xdp.html), [Linux PTP clocks](https://docs.kernel.org/driver-api/ptp.html), [DPDK Linux deployment](https://doc.dpdk.org/guides/linux_gsg/index.html), [Windows IOCP](https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports), [Windows RIO](https://learn.microsoft.com/en-us/windows/win32/winsock/riorqueue), [Apple Network.framework](https://developer.apple.com/videos/play/wwdc2018/715/), [QUIC](https://www.rfc-editor.org/info/rfc9000/), [QUIC DATAGRAM](https://www.rfc-editor.org/info/rfc9221/), and [GPUI cross-platform product availability](https://zed.dev/blog/gpui-2-on-preview).

External capabilities are architecture inputs, not evidence that Axiusflow has implemented or benchmarked them. Exact dependency/library selection requires compatibility, security, license, maintenance, and performance evaluation at implementation time. Commercial availability, market-data rights, broker behavior, and regulatory responsibility require direct agreements and legal review.

Content was rephrased for compliance with licensing restrictions.
