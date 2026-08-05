# Axiusflow Platform Creation Plan

**Document:** authoritative active roadmap
**Revision:** 15
**Last updated:** 2026-08-06
**Primary target:** Rithmic Test through R|Protocol WSS/Protobuf

## 0. How to use this document

This document is the single active execution backlog for Axiusflow.

Frozen history lives at
[`archive/platform_creation_plan_revision_14_2026_08_05.md`](archive/platform_creation_plan_revision_14_2026_08_05.md)
and
[`archive/platform_creation_plan_revision_7_2026_08_04.md`](archive/platform_creation_plan_revision_7_2026_08_04.md).
Those files are not backlogs.

Allowed status values:

- `ready`: specified and free of known external blockers.
- `in_progress`: implementation exists or is being changed but its gate is not met.
- `blocked_external`: dependent on material or authorization Axiusflow cannot supply.
- `deferred`: deliberately outside the current delivery path.
- `verified`: the declared gate has evidence and passes.
- `retired`: removed from active product scope.

A contract, fixture, or passing unit test does not by itself justify `verified`
for provider or product behavior. Open **§1 Active now**, finish that stage's
gate, then advance. Do not mine §12, archives, or decision history for work.
Batching, validation, review, commit, and push rules live in `AGENTS.md`.
Inspect the working tree before changing it; do not overwrite concurrent edits.

## 1. Active now

| Field | Value |
|---|---|
| Current stage(s) | Stage 0 (`ready`), Stage A (`in_progress`), Stage D (`in_progress`) |
| Blocked | Stage C — Rithmic package, agreements, Test login, schema, entitlements |
| Next after current | Stage E (after C+D deterministic gates), then Stage F |
| Do not start | OMS/execution, cloud market-data features, IQFeed, CQG, R\|API+, Coinbase depth/timeframes, AF_XDP/DPDK product work |
| Decision anchors | [`2026_08_05_provider_priority_and_terminal_edge.md`](decisions/2026_08_05_provider_priority_and_terminal_edge.md), [`2026_08_04_acceleration_retirement.md`](decisions/2026_08_04_acceleration_retirement.md) |

Remaining work for current `in_progress` / cleanup stages:

- **Stage 0:** delete AF_XDP/DPDK from `main`, quarantine cloud MD plane, align companion decisions (see §5).
- **Stage A:** explicit chart states, worker split, one recovery coordinator, event-driven inbox, bounded queues, ordered completed bars, BTC/ETH 1m continuity, then freeze.
- **Stage D:** capture and validate the named disabled/enabled diagnostics overhead benchmark against the p99 / p99.9 budgets.

## 2. Product thesis

Axiusflow is a lightweight, local-first professional market terminal. The first
bounded product path is a read-only Rithmic Test terminal with charts, standard
timeframes, tick charts, and a DOM/order book.

Product edge: direct-to-device provider connectivity, off-UI-thread processing,
visible latency/queue/gap/recovery/provenance, bounded resources, deterministic
recovery, and authorized fixture replay. Performance claims require named
hardware, OS, display, provider/environment, workload, sample window, and
percentile evidence—never unmeasured superiority over other terminals.

Full scope policy:
[`decisions/2026_08_05_provider_priority_and_terminal_edge.md`](decisions/2026_08_05_provider_priority_and_terminal_edge.md).

## 3. Active truth

| Area | Status | Current reality |
|---|---|---|
| Coinbase reference + chart | `in_progress` | BTC-USD / ETH-USD 1m history and live aggregation exist; desktop-live gate incomplete. |
| Provider-neutral runtime | `verified` | Shared runtime passes Coinbase and deterministic Rithmic fixture conformance. |
| Rithmic access | `blocked_external` | Package, agreements, Test login, schema, entitlements, and certification unverified. |
| Rithmic adapter (kit-optional) | `ready` | Read-only kit-optional work can proceed; live validation stays blocked. |
| Lightweight diagnostics | `in_progress` | Feed-health path ships; named disabled/enabled overhead evidence remains. |
| Main Rithmic UI | `ready` | Starts after Stages C and D deterministic headless gates. |
| Readiness / endurance | `ready` | Stage F. |
| Descope cleanup | `ready` | Stage 0 — remove retired keep-alives and quarantine deferred build surfaces. |

Deferred and retired items are listed only in §12.

## 4. Guardrails

### 4.1 Product scope

- Rithmic Test through R|Protocol is the primary provider target; first milestone
  is read-only market data.
- Coinbase is a correctness and regression reference only.
- Main UI follows the bounded headless Rithmic gate.
- Unsupported symbols, periods, systems, or history semantics fail explicitly.
- No raw protobuf-send escape hatch in the application API.

### 4.2 Provider and licensed material

- Accepted local kit only under `.cache/provider_kits/rithmic/current/`.
- Ignore ZIPs, guides, `.proto` files, generated bindings, credentials, captures,
  licensed inventories, and payload fixtures unless license review permits.
- Generate bindings into `OUT_DIR` when the kit is present; otherwise build an
  explicit `RithmicKitUnavailable` backend.
- Ordinary validation stays green without proprietary files.
- Manual agreement acceptance occurs through R|Trader or R|Trader Pro.

### 4.3 Credential handling

- Provision credentials through a TTY prompt into `NativeCredentialVault`.
- Never accept passwords in argv, environment, config, logs, fixtures, debug
  output, or panic messages.
- Load credential bytes only for the connection attempt, bound size, and zeroize
  temporary buffers immediately afterward.
- Diagnostics expose coarse classes only—never provider text that may echo
  secrets or account information.

### 4.4 Runtime and UI boundaries

- One provider-neutral coordinator owns session and chart-recovery fencing.
- Adapters own transport and wire decoding; provider-neutral workers own
  lifecycle, bars, books, recovery, diagnostics, and immutable publication.
- GPUI is presentation-only: no network, storage, protobuf, or aggregation.
- All queues are bounded with explicit overflow behavior.
- State and forming updates may coalesce; ordered completed bars and book deltas
  must not silently drop.
- Old generations, callbacks, recovery IDs, and UI selections never mutate the
  current workspace.

## 5. Stage 0 — Descope cleanup

**Status:** `ready`

Prefer completing Stage 0 before expanding Stages E/F. It may run in parallel
with Stages A and D. New agents pick Stage 0 when it is not `verified`.

### Remaining work

1. Create tag `retired/af_xdp_dpdk_<shortsha>`; delete
   `crates/adapters/linux_af_xdp`, `crates/adapters/linux_dpdk`, and related fuzz
   / exclude entries; confirm `cargo build --workspace` stays green.
2. Quarantine `services/market_data_plane` via `workspace.exclude` (do not delete
   `crates/domain/market_data`).
3. Update `docs/linux_development.md` after the deletions so it no longer claims
   source is retained in-tree.

Retirement decision, store decision, provider-priority Stage 0 note, and archive
banners (rev 7 / rev 14) already landed with revision 15.

### Gate

- Stage 0 deletions and quarantine land with a green workspace build.
- Workspace builds do not compile AF_XDP/DPDK adapters or the cloud MD plane.
- §12 lists every deferred/retired disposition; none appear as Work in A–F.
- `docs/linux_development.md` matches the delete-from-main policy.

## 6. Stage A — Stabilize and freeze Coinbase

**Status:** `in_progress`

### Remaining work

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

## 7. Stage B — Provider-neutral contracts and runtime

**Status:** `verified`

### Remaining work

None — gate met. Contracts live in `crates/domain/market_data` and
`crates/desktop_provider_runtime`.

### Gate

Coinbase adapter and deterministic Rithmic fixture adapter pass the same
lifecycle, generation-fencing, bar, book, recovery, and publication conformance
suites without provider-specific types in shared runtime modules.

## 8. Stage C — Rithmic read-only headless core

**Status:** `blocked_external`

Kit-optional implementation is `ready`; authorized Test and schema validation
remain `blocked_external` until package and account access are verified.

### Remaining work

#### Protocol lifecycle

1. Open validated WSS for system discovery.
2. Request and bound system information.
3. Validate and retain available systems, then close that connection.
4. Open a fresh validated WSS connection.
5. Authenticate specifically to the Test system.
6. Establish heartbeat, instruments, and read-only subscriptions.

#### Required behavior

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

## 9. Stage D — Lightweight diagnostics

**Status:** `in_progress`

Always-on instrumentation: allocation-free counters and current/high-water
values. Bounded detailed histograms are opt-in. Publish an immutable snapshot no
faster than 4 Hz containing:

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

### Remaining work

- Capture and validate the named disabled/enabled overhead benchmark against the
  p99 and p99.9 budgets.

### Gate

Snapshot cadence, memory bounds, redaction, counter accuracy, latency labels,
and disabled/enabled overhead pass deterministic tests and a named benchmark.

## 10. Stage E — Main Rithmic UI vertical

**Status:** `ready`

Begin only after Stages C and D pass their deterministic headless gates.

### Remaining work

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

## 11. Stage F — Readiness and endurance

**Status:** `ready`

### Remaining work

Capture evidence for:

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

## 12. Deferred / retired

Reopening any row requires a new accepted decision. Do not reopen via archive
Stage 3 / 5B checklists.

| Item | Status | Disposition | Pointer |
|---|---|---|---|
| Coinbase depth and timeframes | `deferred` | (c) freeze — no work | Stage A gate; provider_priority decision |
| IQFeed | `deferred` | (c) no work | provider_priority decision |
| CQG | `deferred` | (c) no work | provider_priority decision |
| R\|API+ | `deferred` | (c) no work | provider_priority decision |
| Orders / OMS / execution | `deferred` | (c) no work | provider_priority; amended store decision |
| Cloud market data | `deferred` | (b) quarantine crate in Stage 0 | provider_priority; Stage 0 |
| AF_XDP and DPDK | `retired` | (a) delete from `main` in Stage 0 | acceleration_retirement; Stage 0 |

Disposition classes: **(a)** delete from `main`, **(b)** quarantine + pointer,
**(c)** defer-with-no-work.

## 13. Performance targets

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

## 14. Completion definition

This roadmap is complete when a user can launch the native shell, securely
authenticate to Rithmic Test, discover and switch supported instruments, view
tick and supported time-based charts, inspect a recovering read-only DOM, and
understand feed health without exposing credentials or licensed data.

Completion additionally requires deterministic replay, authorized Test evidence,
bounded queues and memory, generation-fenced recovery, clean shutdown, responsive
frame pacing, the endurance gate, a finding-free local review, and a pushed
`main` commit. Production trading is not part of this completion definition.
