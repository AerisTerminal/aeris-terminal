# Axiusflow Platform Creation Plan

**Document:** authoritative active roadmap
**Revision:** 13
**Last updated:** 2026-08-05
**Primary target:** Rithmic Test through R|Protocol WSS/Protobuf

## 1. Document authority

This document is the single active execution roadmap for Axiusflow.

The frozen revision 7 plan is retained at
[`archive/platform_creation_plan_revision_7_2026_08_04.md`](archive/platform_creation_plan_revision_7_2026_08_04.md).
It is historical evidence, not an active backlog. Historical CI narratives and
retired AF_XDP/DPDK work remain there and are intentionally absent here.

Allowed status values are:

- `ready`: specified and free of known external blockers.
- `in_progress`: implementation exists or is being changed but its gate is not met.
- `blocked_external`: dependent on material or authorization Axiusflow cannot supply.
- `deferred`: deliberately outside the current delivery path.
- `verified`: the declared gate has evidence and passes.
- `retired`: removed from active product scope.

No other status token is valid in this plan. A contract, fixture, or passing unit
test does not by itself justify `verified` for provider or product behavior.

## 2. Product thesis

Axiusflow is a lightweight, local-first professional market terminal. The first
bounded product path is a read-only Rithmic Test terminal with charts, standard
timeframes, tick charts, and a DOM/order book.

Its product edge is transparent speed and correctness:

- connect directly from the user's device to the authorized provider;
- keep network, decoding, aggregation, and storage work off the UI thread;
- display latency boundaries, queue pressure, gaps, recovery, and provenance;
- bound memory, queues, payloads, deadlines, and retries;
- recover deterministically instead of concealing loss;
- reproduce behavior from authorized deterministic fixtures.

Axiusflow does not claim unmeasured superiority over ATAS, Sierra Chart, TOS, or
another terminal. Performance claims require named hardware, operating system,
display, provider/environment, workload, sample window, and percentile evidence.

## 3. Current truth

| Area | Status | Current reality |
|---|---|---|
| Coinbase reference feed | `in_progress` | Direct public history and live aggregation exist for BTC-USD and ETH-USD one-minute bars. The desktop integration remains under stabilization and lacks the full desktop-live gate. |
| Coinbase chart UI | `in_progress` | The application has an emerging live worker path, but no complete desktop-live integration gate. Loading and recovery behavior are not final. |
| Coinbase depth and timeframes | `deferred` | Coinbase is frozen at one-minute bars. It has no depth UI and no timeframe selector. |
| Provider-neutral runtime | `verified` | Shared runtime modules are provider-neutral. Coinbase and deterministic Rithmic semantic fixtures pass the same bounded lifecycle, generation, bar, book-gap recovery, covering-snapshot, invalidation, and immutable publication conformance gate. |
| Rithmic access | `blocked_external` | Access has been offered, but the accepted package, agreements, Test login, protocol semantics, entitlements, and certification have not been verified. |
| Rithmic adapter | `ready` | A kit-optional, read-only implementation can begin without committing proprietary material. Live validation remains externally blocked. |
| Lightweight diagnostics | `in_progress` | The bounded feed-health accumulator is wired to the provider-neutral headless event boundary and exercised by both Coinbase and Rithmic semantic fixtures. UI frame-path wiring and named overhead evidence remain. |
| Main Rithmic UI | `ready` | Work begins after the deterministic headless and diagnostics gates pass. |
| IQFeed | `deferred` | Access is preliminary and it is not on the current delivery path. |
| CQG | `deferred` | No implementation or certification work is active. |
| R|API+ | `deferred` | Native R|API+ is distinct from R|Protocol and outside this roadmap. |
| Orders and execution | `deferred` | Orders, OMS, risk, positions, accounts, and execution are excluded. |
| Cloud market data | `deferred` | Provider data remains direct-to-device; Axiusflow does not relay it through its cloud. |
| AF_XDP and DPDK | `retired` | Historical experiments are archived and are not product dependencies. |

The current working tree must be inspected before every change. Existing edits
belong to their author until understood. No stage may overwrite or discard them.

## 4. Guardrails

### 4.1 Product scope

- Rithmic Test through R|Protocol is the primary provider target; the first
  milestone is read-only market data.
- Coinbase is a correctness and regression reference only.
- Main UI work follows the bounded headless Rithmic gate; complete provider
  template coverage is not required first.
- Unsupported symbols, periods, systems, or history semantics fail explicitly.
- No raw protobuf-send escape hatch is part of the application API.

### 4.2 Provider and licensed material

- Store an accepted local kit only under
  `.cache/provider_kits/rithmic/current/`.
- Ignore ZIPs, guides, `.proto` files, generated bindings, credentials, captures,
  licensed inventories, and payload fixtures unless a license review explicitly
  permits tracking them.
- Generate bindings into `OUT_DIR` when the kit is present; otherwise build an
  explicit `RithmicKitUnavailable` backend.
- Keep ordinary validation green without proprietary files; add a private
  kit-enabled lane only when secure CI material is available.
- Manual agreement acceptance occurs through R|Trader or R|Trader Pro.

### 4.3 Credential handling

- Provision credentials through a TTY prompt into `NativeCredentialVault`.
- Never accept passwords in command-line arguments, environment variables,
  configuration files, logs, fixtures, debug output, or panic messages.
- Load credential bytes only for the connection attempt, bound their size, and
  zeroize temporary buffers immediately afterward.
- Diagnostics and errors expose coarse classes, never provider text that may
  echo secrets or account information.

### 4.4 Runtime and UI boundaries

- One provider-neutral coordinator owns session and chart-recovery fencing.
- Provider adapters own transport and wire decoding.
- Provider-neutral workers own lifecycle, bars, books, recovery, diagnostics,
  and immutable publication.
- GPUI owns presentation only; it performs no network, storage, protobuf, or
  aggregation work.
- All queues are bounded and all overflow behavior is explicit.
- State and forming updates may coalesce; ordered completed bars and book deltas
  may not silently drop.
- Old generations, callbacks, recovery identifiers, and UI selections can never
  mutate the current workspace.

### 4.5 Validation and delivery

Work continuously through a very large coherent batch from the current roadmap
stage, including its regression coverage. Do not stop for full-workspace
validation, review, commits, or pushes after individual edits or small
checkpoints. Use focused builds and tests during implementation only when they
provide useful feedback.

After the large work unit is complete, run one commit-boundary gate in order:

1. `cargo fmt --all -- --check`.
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
3. `cargo build --workspace --all-targets --all-features`.
4. Relevant tests and provider conformance suites.
5. Run the configured `cx/gpt-5.6-sol` low-reasoning review with
   `codex exec review --uncommitted --ephemeral -m cx/gpt-5.6-sol -c
   'model_reasoning_effort="low"'` until it reports no findings.
6. Commit to `main`, then `git push origin main` without force.

If validation or review finds problems, fix them as one batch, use focused
checks while iterating, and repeat the complete gate only when the batch is
again ready to commit.

## 5. Stage A — Stabilize and freeze Coinbase

**Status:** `in_progress`

### Work

- Replace the synthetic placeholder bar with explicit chart states:
  `Loading`, `Ready`, `Stale`, `Recovering`, and `Error`.
- Split the oversized desktop live worker by composition, history,
  lifecycle/recovery, publication/provenance, and tests.
- Make one coordinator fence provider recovery and chart resnapshot, reject stale
  generations/callbacks/recovery IDs, and wait for a new covering snapshot.
- Replace 10 ms polling with one bounded event-driven inbox covering provider,
  environment, UI recovery, and shutdown; drain a bounded batch per wakeup.
- Replace front-removal vectors with fixed-capacity `VecDeque` storage.
- Coalesce state and forming-bar UI updates where safe.
- Deliver completed bars in order or fence and request one recovery snapshot.
- Prove bounded launch and shutdown, offline startup, reconnect, corrupt-cache
  recovery, diagnostics redaction, and history-to-live continuity.
- Cover BTC-USD and ETH-USD one-minute handoffs without gaps or duplicates.

### Gate

Coinbase is frozen when the shipping desktop path passes deterministic and live
smoke coverage with explicit state, bounded event-driven behavior, one recovery
owner, nonblocking publication, and no unreviewed warnings. After this gate,
Coinbase receives correctness fixes only—no symbols, timeframes, depth,
analytics, or cloud routes.

## 6. Stage B — Provider-neutral contracts and runtime

**Status:** `verified`

### Canonical contracts

- Fixed-point `MarketTrade` and `TopOfBookQuote`.
- `BookSide`, `DepthLevel`, `DepthSnapshot`, and `DepthDelta`.
- `BarSeriesKey` and `BarUpdate::{Forming, Completed}`.
- `OrderBookPublication` with revision, source watermark, bounded top-N levels,
  and stale/recovery state.
- `MarketEvent::{Trade, Quote, DepthSnapshot, DepthDelta}`.
- Every event carries provider, instrument, entitlement, source sequence,
  session generation, and qualified timestamps.

### Session boundary

- `ProviderSessionEvent` exposes discovery, authentication, instruments, market
  events, heartbeat, invalidation, and stop.
- Sealed read-only commands are connect, replace subscriptions, request
  recovery, disconnect, and shutdown.
- Remove Coinbase types, aggregation maps, and Coinbase-specific methods from
  `desktop_provider_runtime`.
- Keep wire frames and raw protobuf outside public runtime contracts.
- Define tick, 1m, 5m, 15m, 1h, and daily series semantics.
- Enable a period only after its trade semantics and exchange-session calendar
  are defined.

### Gate

Both the Coinbase adapter and deterministic Rithmic fixture adapter pass the
same lifecycle, generation-fencing, bar, book, recovery, and publication
conformance suites without provider-specific types in shared runtime modules.

### Current implementation evidence

- Canonical fixed-point trade, top-of-book quote, depth snapshot/delta, standard
  bar-period, bar-series key, bar-update, and market-event contracts are present.
- The bounded provider-neutral order book publishes immutable top-N snapshots,
  ignores stale generations and sequences, and discards candidate state on gaps
  or crossed books until a covering snapshot arrives.
- Provider session discovery, authentication, instrument, market, heartbeat,
  invalidation, and stop events have explicit validation and memory bounds.
- The application command surface is closed to connect, subscription
  replacement, recovery, disconnect, and shutdown; it has no raw payload or
  provider-template command.
- Coinbase decoded trades project into the canonical fixed-point trade contract
  with exact configured scales and qualified timestamps.
- Coinbase session driving, callback validation, live aggregation, and history
  seeding now live behind the Coinbase adapter boundary; shared runtime modules
  contain no Coinbase driver, aggregation map, type, or method.
- A deterministic Rithmic Test semantic fixture emits bounded discovery,
  authentication, instrument, trade, depth, heartbeat, forming-bar, and
  completed-bar evidence. Coinbase owns an equivalent deterministic semantic
  fixture that retains the reviewed public trade decode and projection path.
- The shared adapter harness runs both fixtures through the same steady-state
  book publications, sequence-gap fail-closed transition, rejected non-covering
  snapshot, covering recovery snapshot, post-recovery delta, terminal
  invalidation, and immutable application publication/recovery checks.
- Authorized Rithmic WSS/Protobuf decoding and provider-behavior evidence remain
  externally blocked and belong to the Stage C gate after protocol-kit access.

## 7. Stage C — Rithmic read-only headless core

**Status:** `blocked_external`

Implementation that does not require the kit is `ready`; authorized Test and
schema validation remain `blocked_external` until package and account access are
verified.

### Protocol lifecycle

1. Open validated WSS for system discovery.
2. Request and bound system information.
3. Validate and retain available systems, then close that connection.
4. Open a fresh validated WSS connection.
5. Authenticate specifically to the Test system.
6. Establish heartbeat, instruments, and read-only subscriptions.

### Required behavior

- Bound TLS, handshake, frame, protobuf, repeated-field, symbol, depth, queue,
  and deadline sizes.
- Map provider instruments to stable internal identities with metadata.
- Decode trades, quotes, depth snapshots/deltas, and supported history.
- Detect heartbeat/message silence and stop cleanly with generation fencing.
- Retry transient failures from 250 ms to 8 seconds with bounded backoff.
- Do not retry rejected credentials, unsigned agreements, unsupported systems,
  or schema/template mismatches.
- On a depth gap, discard candidate book state and require a new snapshot.
- Recover trade/history continuity with bounded overlap, deduplication, and a
  covering snapshot.
- Fail closed when the installed protocol cannot recover continuity.
- Enforce an outbound-template allowlist containing read-only templates only.
- Fail tests if any order or execution template can be emitted.

### Gate

Deterministic fixtures prove discovery, login, trades, quotes, depth, heartbeat,
disconnect/reconnect, recovery, clean stop, bounds, allowlisting, and redaction.
Authorized Rithmic Test traffic must repeat the gate before provider behavior is
marked `verified`.

## 8. Stage D — Lightweight diagnostics

**Status:** `in_progress`

Always-on instrumentation consists of allocation-free counters and current or
high-water values. Bounded detailed histograms are opt-in. Publish an immutable
snapshot no faster than 4 Hz containing:

- provider, system, environment, and connection state;
- session generation, uptime, reconnect count, and recovery reason;
- heartbeat age and last-message age;
- trade, quote, depth, and publication rates;
- gaps, duplicates, malformed messages, stale callbacks, overflows, and
  coalesced UI updates;
- current and high-water queue occupancy;
- history/handoff and order-book recovery state;
- local socket-read, decode, canonical-accept, model-publish, UI-enqueue, frame
  submit, and presentation latency where measurable;
- provider timestamp age labelled as clock-relative age, never network latency;
- approximate bounded runtime memory.

Diagnostics exclude credentials, raw payloads, account data, and licensed
subscription inventories. Detailed diagnostics must not regress p99 by more
than 5% or p99.9 by more than 10% under the same measured workload.

### Current implementation evidence

- The observability owner retains always-on counters and current/high-water
  queue and approximate-memory gauges in fixed-size storage. Impossible
  occupancy, changing capacity, and memory-bound violations fail explicitly.
- Immutable snapshots are suppressed inside a 250 ms window and calculate
  deterministic trade, quote, depth, and publication rates from the prior
  accepted snapshot.
- Session generations, reconnects, uptime, heartbeat age, last-message age,
  history state, order-book state, and coarse recovery reasons are explicit.
- Provider timestamp age is signed and labelled `ProviderClockRelativeAge`; it
  is never presented as network or local processing latency.
- Detailed local latency histograms are opt-in, fixed-size, bounded by a
  maximum accepted sample, and expose p50, p95, p99, p99.9, maximum, accepted,
  and rejected sample evidence.
- The overhead evidence contract requires named hardware, operating system,
  workload, warm-up, sample count, ordered percentiles, and identical
  gap/overflow/recovery outcomes before enforcing the 5% and 10% budgets.
- The provider-neutral headless diagnostics boundary consumes validated
  discovery, authentication, instrument, market, heartbeat, invalidation, and
  stop events; fences stale generations and unfenced invalidations; maps coarse
  recovery reasons; and records canonical publication evidence.
- Coinbase and deterministic Rithmic sessions pass the same diagnostics
  conformance checks for identity, generation, market/depth/publication counts,
  heartbeat age, last-message age, recovery, and immutable snapshot output.
- Remaining work is wiring UI enqueue, frame-submit, and measurable presentation
  timestamps, then capturing the named disabled/enabled benchmark.

### Gate

Snapshot cadence, memory bounds, redaction, counter accuracy, latency labels,
and disabled/enabled overhead pass deterministic tests and a named benchmark.

## 9. Stage E — Main Rithmic UI vertical

**Status:** `ready`

Begin only after Stages C and D pass their deterministic headless gates.

### Work

- Launch a reliable local shell before login or history completion.
- Show provider profile, Test environment, and connection state.
- Search and select discovered symbols.
- Select tick, 1m, 5m, 15m, 1h, and daily series.
- Hydrate visible-range-first history into the Origin chart.
- Replace forming candles and append completed candles deterministically.
- Build a read-only DOM from one snapshot plus ordered deltas.
- Display loading, offline, reconnecting, stale, delayed, test, and live states.
- Add an optional collapsible feed-health and latency panel.
- Conflate chart and depth updates on frame boundaries.
- Fence symbol/timeframe replacement by selection generation.
- Bound hidden-window work and retained state.

### Gate

No GPUI-thread network, storage, protobuf, or aggregation work; correct DOM gap
recovery; responsive symbol/timeframe replacement; and measured 60/120/144 Hz
frame pacing on named hardware. Test and live states must be visually explicit.

## 10. Stage F — Readiness and endurance

**Status:** `ready`

Before readiness or performance claims, capture evidence for:

- slow consumers and publication overflow;
- heartbeat and message-silence loss;
- trade, history, and depth gaps;
- disconnect, bounded reconnect, and terminal failures;
- suspend/resume and offline startup;
- burst traffic and frame-aligned conflation;
- cache corruption and covering resnapshot;
- current and high-water memory;
- eight-hour headless and desktop endurance.

### Gate

All failure cases recover or fail closed as specified; memory and queues remain
within declared bounds; no stale generation reaches the model or UI; and the
eight-hour run records no unexplained gap, deadlock, secret exposure, or
unbounded growth.

## 11. Performance targets

Targets guide measurement; they are not current claims.

| Boundary | Target |
|---|---|
| Diagnostics publication | At most 4 Hz |
| Transient reconnect backoff | 250 ms minimum, 8 s maximum |
| UI update scheduling | Frame-aligned, never one render per wire delta |
| Queue and history storage | Fixed capacity with visible current/high-water use |
| Detailed diagnostics overhead | p99 <= 5%; p99.9 <= 10% regression |
| Frame pacing | Measured at 60, 120, and 144 Hz |
| Endurance | Eight continuous hours before readiness claims |

Latency reporting must separate provider clock-relative timestamp age from local
socket-to-present processing. Every percentile report includes p50, p95, p99,
p99.9, maximum, sample count, warm-up, and loss/recovery counters.

## 12. Completion definition

This roadmap is complete when a user can launch the native shell, securely
authenticate to Rithmic Test, discover and switch supported instruments, view
tick and supported time-based charts, inspect a recovering read-only DOM, and
understand feed health without exposing credentials or licensed data.

Completion additionally requires deterministic replay, authorized Test evidence,
bounded queues and memory, generation-fenced recovery, clean shutdown, responsive
frame pacing, the endurance gate, a finding-free local review, and a pushed
`main` commit. Production trading is not part of this completion definition.
