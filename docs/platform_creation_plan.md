# Axiusflow Platform Creation Plan

**Document:** authoritative active roadmap
**Revision:** 30
**Last updated:** 2026-08-07
**Primary target:** Rithmic Test through R|Protocol WSS/Protobuf

## 0. How to use this document

This document is the single active execution backlog for Axiusflow.

Frozen history lives at
[`archive/platform_creation_plan_revision_14_2026_08_05.md`](archive/platform_creation_plan_revision_14_2026_08_05.md)
and
[`archive/platform_creation_plan_revision_7_2026_08_04.md`](archive/platform_creation_plan_revision_7_2026_08_04.md).
Those files are not backlogs.

### Hard sequencing rule (do not skip)

**Stage 0 must reach `verified` before any further product-plan execution.**

Kernel-bypass architecture (Linux AF_XDP and DPDK adapters, keep-alives, fuzz
targets, and related workspace membership) must be **completely removed from
`main`** first. Until Stage 0's gate passes:

- Do **not** start or continue Stages A, C, E, or F as active delivery work.
- Do **not** expand Stage B/D surfaces that already shipped.
- Stage 0 is not optional cleanup and is not parallelizable with A–F.

Decision:
[`decisions/2026_08_04_acceleration_retirement.md`](decisions/2026_08_04_acceleration_retirement.md).

Allowed status values:

- `ready`: specified and free of known external blockers.
- `in_progress`: implementation exists or is being changed but its gate is not met.
- `blocked_external`: dependent on material or authorization Axiusflow cannot supply.
- `deferred`: deliberately outside the current delivery path.
- `verified`: the declared gate has evidence and passes.
- `retired`: removed from active product scope.

Checkbox convention:

- `[x]` done (landed and accepted for that item)
- `[ ]` not done
- Stage header status is authoritative; checkboxes track work items under that stage.

A contract, fixture, or passing unit test does not by itself justify `verified`
for provider or product behavior. Open **§1 Active now**, finish that stage's
gate, then advance. Do not mine §12, archives, or decision history for work.
Batching, clean-code, validation, commit, and push rules live in `AGENTS.md`.
Focus on large coherent implementation batches; do not treat process as the work.
Inspect the working tree before changing it; do not overwrite concurrent edits.

## 1. Active now

| Field | Value |
|---|---|
| Current stage(s) | **Stage E** (`in_progress` — interactive controls, live candles, recovering DOM, explicit Test lifecycle state, and optional feed health are integrated). **Stage C** is `in_progress` after deterministic conformance and an authorized core-path Test run. |
| Blocked | None. The remaining Stage C work is authorized resilience evidence, not credential or agreement access. |
| Next | Capture named 60/120/144 Hz evidence and extend authorized Test evidence through heartbeat loss |
| Do not start | OMS/execution, cloud market-data features, IQFeed, CQG, R|API+, Coinbase depth/timeframes, or AF_XDP/DPDK product work |
| Decision anchors | [`2026_08_05_provider_priority_and_terminal_edge.md`](decisions/2026_08_05_provider_priority_and_terminal_edge.md), [`2026_08_04_acceleration_retirement.md`](decisions/2026_08_04_acceleration_retirement.md) |

### Progress snapshot

| Stage | Status | Progress |
|---|---|---|
| 0 — Descope cleanup (kernel bypass out) | `verified` | [x] adapters/harnesses deleted; cloud plane quarantined; workspace gate green |
| A — Stabilize Coinbase | `verified` | encrypted history retention/discovery [x]; deterministic shipping recovery [x]; BTC/ETH shipping live smoke [x]; frozen [x] |
| B — Provider-neutral runtime | `verified` | [x] complete |
| C — Rithmic read-only headless | `in_progress` | deterministic headless conformance [x]; authorized login/search/reference/trade/quote/depth/history [x]; resilience evidence [ ] |
| D — Lightweight diagnostics | `verified` | [x] complete |
| E — Main Rithmic UI | `in_progress` | pre-login shell/profile/state [x]; bounded symbol/timeframe controls [x]; authorized visible history [x]; live candles [x]; recovering read-only DOM [x]; lifecycle/feed health [x] |
| F — Readiness / endurance | `ready` | [ ] not started |

### Remaining focus (ordered)

- [x] **Stage 0:** AF_XDP/DPDK kernel-bypass architecture removed from `main`, cloud MD plane quarantined, Linux docs updated (see §5).
- [x] **Stage A:** deterministic shipping conformance [x]; BTC/ETH shipping live smoke [x]; frozen after verification (see §6).
- [ ] **Stage C:** deterministic headless conformance [x]; authorized core-path evidence [x]; resilience/recovery evidence [ ] (see §8).
- [ ] **Stage E:** build the main Rithmic UI vertical from the verified deterministic Stage C/D contracts (see §10).



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


| Area                       | Status        | Current reality                                                                                                                                                                                                                                     |
| -------------------------- | ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Coinbase reference + chart | `verified`    | BTC-USD / ETH-USD 1m cache-first startup, corrupt-cache refetch, reconnect fencing, redaction, history/live continuity, bounded shutdown, and public shipping smoke pass; scope is frozen.                                                           |
| Provider-neutral runtime   | `verified`    | Shared runtime passes Coinbase and deterministic Rithmic fixture conformance.                                                                                                                                                                       |
| Rithmic access             | `verified`    | The kit is installed locally under `provider_kit/`; native-vault credentials, Test login, search/reference, trades, quotes, depth, and history were exercised successfully on 2026-08-07. |
| Rithmic adapter            | `in_progress` | Kit-backed bounded codecs, ticker/history TLS WSS lifecycles, fail-closed search/replay collectors, complete-depth-image assembly, vault-backed runtime callbacks, canonical mapping, subscriptions, silence detection, retry fencing, exact covering history pages, and generation-fenced bar continuity recovery with overlap deduplication are implemented; authorized resilience/recovery evidence remains. |
| Lightweight diagnostics    | `verified`    | Feed-health path ships through the live desktop worker and UI; deterministic tests cover cadence, bounds, redaction, counters, latency labels, and queue/memory snapshots; named disabled/enabled overhead evidence passes the p99 / p99.9 budgets. |
| Main Rithmic UI            | `in_progress` | `--rithmic-test` opens a flush GPUI shell with integrated contract/timeframe/DOM/health/theme controls. The entitled MNQ contract hydrates automatically; bounded time or 100-trade history seeds Origin, canonical trades replace forming bars and append completed bars off-thread, and complete Rithmic depth images feed a generation-fenced read-only DOM. The history/live handoff buffers trades to a fixed bound and requests a new covering replay on overflow. After session invalidation the exact contract and timeframe are re-discovered and reinstalled before history, candles, and depth resume. The title bar exposes coarse Test lifecycle state and the optional health panel consumes redacted immutable diagnostics. Active windows drain conflated chart/depth updates once per display frame; inactive windows stop applying UI work while bounded mailboxes retain the latest state. Named pacing evidence remains. |
| Readiness / endurance      | `ready`       | Stage F.                                                                                                                                                                                                                                            |
| Descope cleanup            | `ready`       | **Hard first gate.** Stage 0 must remove AF_XDP/DPDK kernel bypass from `main` and quarantine the cloud MD plane before any further A–F execution.                                                                                                  |


Deferred and retired items are listed only in §12.

## 4. Guardrails



### 4.1 Product scope

- Rithmic Test through R|Protocol is the primary provider target; first milestone
is read-only market data.
- Coinbase is a correctness and regression reference only.
- Main UI follows the bounded headless Rithmic gate.
- Unsupported symbols, periods, systems, or history semantics fail explicitly.
- No raw protobuf-send escape hatch in the application API.
- Native R|API+ remains deferred; do not build a C++ R|API+ path from this kit.



### 4.2 Provider and licensed material

Rithmic licensed material is **local-only**. It must never be committed to git.

**Canonical kit location (agents must use this path):**

```text
provider_kit/current/
```

On this machine that symlink resolves to R|Protocol **0.89.0.0**:

```text
provider_kit/current/          → RProtocolAPI.0.89.0.0/0.89.0.0/
provider_kit/current/proto/    → *.proto schemas (codegen input)
provider_kit/current/doc/      → Reference_Guide.pdf
provider_kit/current/etc/
provider_kit/current/samples/
provider_kit/current/Release.Notes
```

Why `provider_kit/`: dedicated local folder for licensed provider kits, gitignored
as `/provider_kit/` in `.gitignore`. The kit is Rithmic proprietary (protos, guide,
samples). Do not place it under `crates/` or commit it. Ordinary CI and clones
build without the kit.

**Codegen / adapter rules:**

- When `current/proto` is present, generate Rust protobuf bindings into `OUT_DIR`
(prost / build.rs). Do not check generated bindings into git.
- When the kit is absent, build an explicit `RithmicKitUnavailable` backend so
workspace validation stays green.
- Ignore ZIPs, guides, `.proto` files, generated bindings, credentials, captures,
licensed inventories, and payload fixtures in version control unless a license
review explicitly permits tracking them.
- Manual agreement acceptance occurs through R|Trader or R|Trader Pro on
**Rithmic Test** before API login can succeed.



### 4.3 Credential handling

- Provision Rithmic Test credentials through a TTY prompt into
`NativeCredentialVault`. Never write them into this plan, source, env files,
or git.
- The fixed provisioner is
  `cargo run -p axiusflow_rithmic_protocol_adapter --bin provision_rithmic_test`;
  it accepts no credential arguments or environment inputs.
- Never accept passwords in argv, environment, config, logs, fixtures, debug
output, or panic messages.
- Load credential bytes only for the connection attempt, bound size, and zeroize
temporary buffers immediately afterward.
- Diagnostics expose coarse classes only—never provider text that may echo
secrets or account information.
- Credentials are valid for **Rithmic Test only**, not Rithmic Paper Trading,
Rithmic 01, or other systems. Fence `system_name` accordingly and fail closed.



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



## 5. Stage 0 — Descope cleanup (hard first gate)

**Status:** `verified`

**Mandatory before the rest of this plan.** Kernel-bypass architecture must be
completely removed from `main` before executing Stages A–F. Do not treat Stage 0
as optional, deferred, or parallel with Coinbase/Rithmic/UI/endurance work.

This stage deletes the retired AF_XDP and DPDK paths entirely and quarantines
the deferred cloud market-data plane so the active product path is a normal
userspace terminal stack only.

Decision:
[`decisions/2026_08_04_acceleration_retirement.md`](decisions/2026_08_04_acceleration_retirement.md).

New agents: if Stage 0 is not `verified`, do Stage 0 only.

### Done

- [x] Retirement decision, store decision, provider-priority Stage 0 note, and archive banners (rev 7 / rev 14) landed with revision 15.

### Completed work

- [x] Created annotated tag `retired/af_xdp_dpdk_5c13262`; deleted the AF_XDP/DPDK adapters, vendored dependency, fuzz target, privileged harnesses, and related workspace exclusions.
- [x] Quarantined `services/market_data_plane` via `workspace.exclude` without deleting `crates/domain/market_data`.
- [x] Updated `docs/linux_development.md` to point historical investigation at the tag and git history.
- [x] Added and passed `tools/run_acceleration_retirement_conformance.sh` checks for retired paths, workspace membership, readiness, installer, and CI absence.

### Gate

- [x] Stage 0 deletions and quarantine pass the green workspace build.
- [x] Workspace builds do not compile AF_XDP/DPDK adapters or the cloud MD plane.
- [x] Kernel-bypass architecture is fully gone from `main` (no product keep-alive).
- [x] §12 lists every deferred/retired disposition; none appear as Work in A–F.
- [x] `docs/linux_development.md` matches the delete-from-main policy.
- [x] Stage 0 is `verified`; Stages A/C (and later E/F) may resume in roadmap order.



## 6. Stage A — Stabilize and freeze Coinbase

**Status:** `verified`

Stage 0 and the Stage C deterministic gate are verified. Stage A passed its
deterministic shipping gate and public BTC/ETH smoke and is now frozen.

### Done

- [x] Replace the synthetic placeholder bar with explicit chart states: `Loading`, `Ready`, `Stale`, `Recovering`, and `Error`.
- [x] Split the oversized desktop live worker by composition, history, lifecycle/recovery, publication/provenance, and tests.
- [x] Make one coordinator fence provider recovery and chart resnapshot, reject stale generations/callbacks/recovery IDs, and wait for a new covering snapshot.
- [x] Replace 10 ms polling with one bounded event-driven inbox covering provider, environment, UI recovery, and shutdown; drain a bounded batch per wakeup.
- [x] Replace front-removal vectors with fixed-capacity `VecDeque` storage.
- [x] Coalesce state and forming-bar UI updates where safe.
- [x] Deliver completed bars in order or fence and request one recovery snapshot.
- [x] Encode validated Coinbase pages into bounded versioned segments, retain them through the worker-owned encrypted store, and discover the newest exact series revision without exposing raw catalog dimensions.
- [x] Hydrate and publish the newest authenticated retained segment before connecting, normalize it into the live aggregator's bounded sequence space, reject corrupt segments, and replace cached ownership with a fresh live covering snapshot.
- [x] Prove bounded launch and shutdown, reconnect, corrupt-cache provider refetch, diagnostics redaction, and deterministic history-to-live continuity through the shipping worker.
- [x] Cover BTC-USD and ETH-USD one-minute handoffs without gaps or duplicates.
- [x] Repeat the launch, covering-snapshot, and clean-shutdown path through the shipping desktop worker against the public Coinbase feed for BTC-USD and ETH-USD.



### Gate

- [x] Coinbase shipping desktop path passes deterministic and live smoke coverage with explicit state, bounded event-driven behavior, one recovery owner, nonblocking publication, and no unreviewed warnings.
- [x] After this gate, Coinbase receives correctness fixes only—no symbols, timeframes, depth, analytics, or cloud routes.

Component evidence command:

- `tools/run_coinbase_desktop_conformance.sh`

Live shipping-worker evidence command:

- `tools/run_coinbase_desktop_live_smoke.sh`



## 7. Stage B — Provider-neutral contracts and runtime

**Status:** `verified`

### Done

- [x] Contracts live in `crates/domain/market_data` and `crates/desktop_provider_runtime`.
- [x] Coinbase adapter and deterministic Rithmic fixture adapter pass the same lifecycle, generation-fencing, bar, book, recovery, and publication conformance suites without provider-specific types in shared runtime modules.
- [x] Shared runtime conformance gate passed (Coinbase + deterministic Rithmic fixture).



### Remaining work

None — gate met.

## 8. Stage C — Rithmic read-only headless core

**Status:** `in_progress`

Stage 0 and Stage C deterministic conformance are verified. Authorized Test
core-path evidence is complete; authorized resilience/recovery evidence remains.

Rithmic has unlocked the R|Protocol path (kit download + Rithmic Test credentials).
The local kit is installed. Implementation of encoding/decoding and the read-only
session is unblocked after Stage 0. The R|Trader Test agreement screen was checked
on 2026-08-07 and showed nothing requiring signature; credentials must still be
loaded only through the native vault before live evidence.

### Local kit (codegen input)

Agents building Stage C **must** read schemas from:

```text
provider_kit/current/proto/
```


| Fact              | Value                                                                     |
| ----------------- | ------------------------------------------------------------------------- |
| Protocol product  | R|Protocol API (WSS + Protobuf) — **not** native R|API+                   |
| Installed version | `0.89.0.0`                                                                |
| Schema dir        | `provider_kit/current/proto/` (~155 `.proto` files)                       |
| Reference guide   | `provider_kit/current/doc/Reference_Guide.pdf`                            |
| Binding output    | Generate into `OUT_DIR` via build.rs / prost; never commit generated code |
| Missing kit       | Emit `RithmicKitUnavailable` and keep default workspace builds green      |


Key proto families for the first read-only milestone include system info, login,
heartbeat, symbol search / instrument metadata, last trade, best bid/offer,
order book / depth, and market-data subscribe updates. Use the Reference Guide
for template IDs. **Outbound allowlist must exclude all order and execution
templates** (cancel, modify, bracket, etc. remain decode-only or unused).

### Authorized Test endpoint


| Fact                 | Value                                                             |
| -------------------- | ----------------------------------------------------------------- |
| WebSocket URL        | `wss://rituz00100.rithmic.com:443` (SSL / `wss` only)             |
| `system_name`        | `Rithmic Test` only                                               |
| Out of scope systems | Rithmic Paper Trading, Rithmic 01, and any other non-Test system  |
| Credentials          | OS vault via TTY — never store in repo, plan, or env files        |
| Agreements           | R|Trader Test screen checked 2026-08-07; no agreement was shown   |




### Prerequisites

- [x] R|Protocol kit access issued by Rithmic.
- [x] Rithmic Test credentials issued.
- [x] Local kit installed at `provider_kit/current/` (`0.89.0.0`).
- [x] R|Trader / R|Trader Pro Test agreement screen checked; no agreement was presented.
- [x] Credentials loaded only through vault (TTY → `NativeCredentialVault`) for live evidence.



### Remaining work



#### Build / encode path

- [x] Detect kit at `provider_kit/current/proto/`.
- [x] Generate Rust types from the installed `.proto` set into `OUT_DIR`.
- [x] Implement documented R|Protocol v2 raw-protobuf-per-binary-WebSocket-message encode/decode using generated private types (no extra length prefix and no raw public send escape hatch).
- [x] Keep a `RithmicKitUnavailable` backend for kit-less CI.



#### Protocol lifecycle (Rithmic-specified)

- [x] Open validated WSS to `wss://rituz00100.rithmic.com:443`.
- [x] Send `RequestRithmicSystemInfo`; parse and bound the available system names.
- [x] Close that discovery connection.
- [x] Open a **new** validated WSS connection to the same URL.
- [x] Send `RequestLogin` with `system_name = Rithmic Test` and vault credentials.
- [x] Establish heartbeat, instruments, and **read-only** market-data subscriptions.



#### Required behavior

- [x] Bound TLS, handshake, frame, protobuf, repeated-field, symbol, depth, queue, and deadline sizes.
- [x] Map provider instruments to stable internal identities with metadata.
- [x] Decode trades, quotes, complete depth snapshots (template 156 exposes no provider delta sequence), and supported history.
- [x] Detect heartbeat/message silence and stop cleanly with generation fencing.
- [x] Retry transient failures from 250 ms to 8 seconds with bounded backoff.
- [x] Do not retry rejected login, unsupported systems, or schema/template mismatches; unsigned agreements surface through the terminal login-rejection path.
- [x] On malformed/missing depth chunks, discard candidate book state and require a new snapshot; never synthesize provider deltas or sequences absent from template 156.
- [x] Recover trade/history continuity with bounded overlap, deduplication, and a covering snapshot.
- [x] Fail closed when the installed protocol cannot recover continuity.
- [x] Enforce an outbound-template allowlist containing read-only templates only.
- [x] Fail tests if any order or execution template can be emitted.



### Gate

- [x] `tools/run_rithmic_protocol_conformance.sh` proves discovery, login, trades, quotes, depth, heartbeat, disconnect/reconnect, recovery, clean stop, bounds, allowlisting, redaction, and the kit-unavailable build.
- [ ] Authorized Rithmic Test traffic repeats heartbeat-loss evidence before provider behavior is marked `verified`; login, search/reference, trades, quotes, depth, time/tick history, a fresh disconnect/reconnect cycle, recovery, and clean close already pass.



## 9. Stage D — Lightweight diagnostics

**Status:** `verified`

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

### Done

- [x] Feed-health path ships through the live desktop worker and UI.
- [x] Deterministic tests cover cadence, bounds, redaction, counters, latency labels, and queue/memory snapshots.
- [x] Named disabled/enabled overhead evidence passes the p99 / p99.9 budgets.
- [x] Evidence commands:
  - `cargo test -p axiusflow_observability --all-targets`
  - `cargo test -p axiusflow_desktop_provider_runtime --all-targets`
  - `cargo test -p axiusflow_desktop --all-targets`
  - `bash tools/run_diagnostics_overhead_benchmark.sh`
- [x] Evidence artifact: `.cache/evidence/stage_d_diagnostics_overhead_Linux.json` on the measured host.



### Remaining work

None — gate met.

### Gate

- [x] Snapshot cadence, memory bounds, redaction, counter accuracy, latency labels, and disabled/enabled overhead pass deterministic tests and a named benchmark.



## 10. Stage E — Main Rithmic UI vertical

**Status:** `in_progress` (interactive controls, live candles, and read-only DOM landed)

Begin only after Stage 0 is `verified` and Stages C and D pass their
deterministic headless gates.

### Prerequisites

- [x] Stage D deterministic headless gate met.
- [x] Stage C deterministic headless gate met.



### Remaining work

- [x] Launch a reliable local shell before login or history completion.
- [x] Show provider profile, Test environment, and connection state.
- [x] Search and select discovered symbols.
- [x] Select 100-trade, 1m, 5m, 15m, 1h, and daily series.
- [x] Define and implement truthful tick-series aggregation and continuity.
- [x] Keep the Origin chart mounted before provider data arrives; never substitute fixture candles.
- [x] Run a native-vault-only headless Test smoke for login, discovery, selection, history, and streaming.
- [x] Hydrate visible-range-first history into the Origin chart.
- [x] Replace forming candles and append completed candles deterministically.
- [x] Build a read-only DOM from complete provider images plus ordered canonical deltas.
- [x] Display loading, offline, reconnecting, stale, delayed, test, and live states.
- [x] Add an optional collapsible feed-health and latency panel.
- [x] Conflate chart and depth updates on frame boundaries.
- [x] Fence symbol/timeframe replacement by selection generation.
- [x] Bound hidden-window work and retained state.

The native header now uses GPUI Component dropdown menus for exact contract and
series selection. The instrument menu includes a compact symbol query input,
remains available after an empty result, and renders only the bounded,
generation-fenced catalog result set. The current contract and series are
checked, so selection no longer relies on opaque click-to-cycle behavior.



### Gate

Provider evidence on 2026-08-07: Test login, a 16-result `MNQ` search, CME
instrument reference, trades, quotes, complete depth, and a one-minute history
replay all passed headlessly from the native vault. The successful combined run
received all three live market classes, 264 historical one-minute bars, and six
100-trade historical tick bars for `MNQU6` on `CME`. A second fresh discovery,
login, reference, and combined subscription cycle also passed. Rithmic depth images may omit `ssboe`; the adapter preserves that absence
and uses the qualified local receive time instead of rejecting the frame. Catalog venues ending
in `-Delayed` must use the base entitled exchange for reference, subscription,
and history requests.

Windows frame evidence on 2026-08-07: the native display probe identified
`LG ULTRAGEAR` (`\\.\DISPLAY1`) at 2560x1440, 1.25x scale, and 165,000 mHz.
A 256-frame debug-profile Origin replay run after 32 warmup frames reported a
13.33 ms frame-callback interval p50 (about 75 Hz). The report now labels GPUI
`on_next_frame` as post-render callback cadence rather than physical scanout.
This does not satisfy the named 60/120/144 release-profile matrix, so the gate
remains open.

- [x] No GPUI-thread network, storage, protobuf, or aggregation work.
- [x] Correct DOM gap recovery.
- [x] Responsive symbol/timeframe replacement.
- [ ] Measured 60/120/144 Hz frame pacing on named hardware.
- [x] Test and live states are visually explicit.



## 11. Stage F — Readiness and endurance

**Status:** `ready` (after the upstream product gates)

### Remaining work

Capture evidence for:

- [x] Slow consumers and publication overflow.
- [ ] Heartbeat and message-silence loss.
- [x] Trade, history, and depth gaps.
- [x] Disconnect, bounded reconnect, and terminal failures.
- [ ] Suspend/resume and offline startup.
- [x] Burst traffic and frame-aligned conflation.
- [x] Cache corruption and covering resnapshot.
- [x] Current and high-water memory.
- [ ] Eight-hour headless and desktop endurance.

Deterministic evidence on 2026-08-07: the cross-platform ingest conformance now
emits named Stage F results rather than an opaque aggregate bitmask. A Windows
run passed slow-consumer publication overflow with atomic rollback, lifecycle
event overflow with last-generation retention, bounded reconnect exhaustion and
fresh-budget recovery, ordered-gap recovery, and corrupt-snapshot rejection.
The desktop suites separately pass authenticated cache-corruption fallback,
covering resnapshot after publication overflow, and terminal Rithmic failure
redaction. The message-silence row remains open until the already-passing
deterministic silent-peer timeout is repeated as authorized Rithmic heartbeat
loss.

The schema-2 desktop readiness artifact now exercises the production live-chart,
history-handoff, and read-only DOM state machines together. It rejects a repeated
trade sequence without mutating the chart, latches history snapshot-required on
a sequence discontinuity and accepts only a newer covering generation, and
clears depth on a sequence gap until a covering complete image restores the
book. All five named gap/recovery outcomes passed on Windows on 2026-08-07.

Desktop burst evidence on 2026-08-07: the headless
`--desktop-readiness` command published 10,000 successively newer Rithmic chart
snapshots without draining the UI mailbox. The fixed-capacity 32-item mailbox
retained exactly one item at series generation 10,000, and 10,000 attempted
frame-drain schedules admitted exactly one callback until completion. The
command writes schema-2 JSON and opens no window.

The same Windows run sampled the process working set throughout the burst:
12,763,136 bytes at baseline, 14,462,976 bytes current/high-water after the
burst, and 1,699,840 bytes sampled growth against a declared 67,108,864-byte
limit. This is bounded burst evidence; the eight-hour row remains open for a
continuous plateau measurement.

The headless `--desktop-endurance <report> <seconds>` runner is now available
for the continuous gate. It paces frame cycles at 16 ms, injects a 1,000-update
burst every 60 cycles, verifies the exact newest generation after every drain,
tracks mailbox and working-set high water, and writes a clean-stop JSON result.
A two-second Windows qualification run completed 124 frame cycles and 2,122
updates with one retained mailbox item, zero stale/gapped publications, and a
14,442,496-byte sampled working-set high water. This qualifies the runner but
does not replace the required eight-hour execution.



### Gate

- [ ] All failure cases recover or fail closed as specified.
- [ ] Memory and queues remain within declared bounds.
- [ ] No stale generation reaches the model or UI.
- [ ] Eight-hour run records no unexplained gap, deadlock, secret exposure, or unbounded growth.



## 12. Deferred / retired

Reopening any row requires a new accepted decision. Do not reopen via archive
Stage 3 / 5B checklists.


| Item                          | Status     | Disposition                       | Pointer                                   |
| ----------------------------- | ---------- | --------------------------------- | ----------------------------------------- |
| Coinbase depth and timeframes | `deferred` | (c) freeze — no work              | Stage A gate; provider_priority decision  |
| IQFeed                        | `deferred` | (c) no work                       | provider_priority decision                |
| CQG                           | `deferred` | (c) no work                       | provider_priority decision                |
| R|API+                        | `deferred` | (c) no work                       | provider_priority decision                |
| Orders / OMS / execution      | `deferred` | (c) no work                       | provider_priority; amended store decision |
| Cloud market data             | `deferred` | (b) quarantine crate in Stage 0   | provider_priority; Stage 0                |
| AF_XDP and DPDK               | `retired`  | (a) delete from `main` in Stage 0 | acceleration_retirement; Stage 0          |


Disposition checklist:

- [x] **(c)** defer-with-no-work accepted for Coinbase depth/timeframes, IQFeed, CQG, RAPI+, OMS/execution.
- [x] **(b)** cloud MD plane (`services/market_data_plane`) quarantined via `workspace.exclude`.
- [x] **(a)** AF_XDP/DPDK deleted from `main`; history retained by the annotated retirement tag.



## 13. Performance targets

Targets guide measurement; they are not current claims.


| Boundary                      | Target                                             |
| ----------------------------- | -------------------------------------------------- |
| Diagnostics publication       | At most 4 Hz                                       |
| Transient reconnect backoff   | 250 ms minimum, 8 s maximum                        |
| UI update scheduling          | Frame-aligned, never one render per wire delta     |
| Queue and history storage     | Fixed capacity with visible current/high-water use |
| Detailed diagnostics overhead | p99 <= 5%; p99.9 <= 10% regression                 |
| Frame pacing                  | Measured at 60, 120, and 144 Hz                    |
| Endurance                     | Eight continuous hours before readiness claims     |


Latency reporting must separate provider clock-relative timestamp age from local
socket-to-present processing. Every percentile report includes p50, p95, p99,
p99.9, maximum, sample count, warm-up, and loss/recovery counters.

## 14. Completion definition

This roadmap is complete when a user can launch the native shell, securely
authenticate to Rithmic Test, discover and switch supported instruments, view
tick and supported time-based charts, inspect a recovering read-only DOM, and
understand feed health without exposing credentials or licensed data.

### Roadmap completion checklist

- [ ] Launch native shell and authenticate securely to Rithmic Test.
- [ ] Discover and switch supported instruments.
- [ ] View tick and supported time-based charts.
- [ ] Inspect a recovering read-only DOM.
- [ ] Understand feed health without exposing credentials or licensed data.
- [ ] Deterministic replay evidence.
- [ ] Authorized Test evidence.
- [ ] Bounded queues and memory.
- [ ] Generation-fenced recovery.
- [ ] Clean shutdown.
- [ ] Responsive frame pacing.
- [ ] Endurance gate.
- [ ] Finding-free local review and pushed `main` commit.

Production trading is not part of this completion definition.
