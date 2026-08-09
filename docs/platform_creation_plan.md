# Axiusflow Platform Creation Plan

**Document:** authoritative active roadmap
**Revision:** 36
**Last updated:** 2026-08-08
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
| Current stage(s) | **Stages C, E, and F** are `blocked_external`: implementation and deterministic gates pass, while completion requires real provider silence, externally instrumented physical scanout, and operator-driven physical lifecycle transitions. |
| Blocked | Provider-observed silence needs a real provider event or Rithmic-coordinated fault; named physical pacing and lifecycle captures need matching hardware/operator transitions. |
| Next | Capture the remaining provider-path, physical pacing, and physical lifecycle artifacts |
| Do not start | OMS/execution, cloud market-data features, IQFeed, CQG, R|API+, Coinbase depth/timeframes, or AF_XDP/DPDK product work |
| Decision anchors | [`2026_08_05_provider_priority_and_terminal_edge.md`](decisions/2026_08_05_provider_priority_and_terminal_edge.md), [`2026_08_04_acceleration_retirement.md`](decisions/2026_08_04_acceleration_retirement.md) |

### Progress snapshot

| Stage | Status | Progress |
|---|---|---|
| 0 — Descope cleanup (kernel bypass out) | `verified` | [x] adapters/harnesses deleted; cloud plane quarantined; workspace gate green |
| A — Stabilize Coinbase | `verified` | encrypted history retention/discovery [x]; deterministic shipping recovery [x]; BTC/ETH shipping live smoke [x]; frozen [x] |
| B — Provider-neutral runtime | `verified` | [x] complete |
| C — Rithmic read-only headless | `blocked_external` | deterministic headless conformance [x]; authorized login/search/reference/trade/quote/depth/history [x]; provider-observed resilience evidence [ ] |
| D — Lightweight diagnostics | `verified` | [x] complete |
| E — Main Rithmic UI | `blocked_external` | implementation [x]; named externally instrumented 60/120/144 Hz pacing [ ] |
| F — Readiness | `blocked_external` | deterministic failure, burst, and memory evidence [x]; provider/physical transitions [ ] |

### Remaining focus (ordered)

- [x] **Stage 0:** AF_XDP/DPDK kernel-bypass architecture removed from `main`, cloud MD plane quarantined, Linux docs updated (see §5).
- [x] **Stage A:** deterministic shipping conformance [x]; BTC/ETH shipping live smoke [x]; frozen after verification (see §6).
- [ ] **Stage C:** deterministic headless conformance [x]; authorized core-path evidence [x]; resilience/recovery evidence [ ] (see §8).
- [x] **Stage E implementation:** the main Rithmic UI vertical is integrated from the verified deterministic Stage C/D contracts; named physical pacing remains an open Stage E gate (see §10).



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
| Rithmic adapter            | `blocked_external` | Kit-backed bounded codecs, ticker/history TLS WSS lifecycles, fail-closed search/replay collectors, complete-depth-image assembly, vault-backed runtime callbacks, canonical mapping, subscriptions, silence detection, retry fencing, native environment fencing, exact covering history pages, and generation-fenced bar continuity recovery with overlap deduplication are implemented. Authorized authenticated client-local silence injection and clean recovery pass; provider-observed loss evidence remains. |
| Lightweight diagnostics    | `verified`    | Feed-health path ships through the live desktop worker and UI; deterministic tests cover cadence, bounds, redaction, counters, latency labels, and queue/memory snapshots; named disabled/enabled overhead evidence passes the p99 / p99.9 budgets. |
| Main Rithmic UI            | `blocked_external` | `--rithmic-test` opens a flush GPUI shell with integrated contract/timeframe/DOM/health/theme controls. The entitled MNQ contract hydrates automatically; bounded time or 100-trade history seeds Origin, canonical trades replace forming bars and append completed bars off-thread, and complete Rithmic depth images feed a generation-fenced read-only DOM. The history/live handoff buffers trades to a fixed bound and requests a new covering replay on overflow. Native Windows network and power events now retire the active session, history, chart, DOM, and selection before a fresh generation rediscovers and reinstalls the exact contract and timeframe. The title bar exposes coarse Test lifecycle state and the optional health panel consumes redacted immutable diagnostics. Active windows drain conflated chart/depth updates once per display frame; inactive windows stop applying UI work while bounded mailboxes retain the latest state. Externally instrumented named pacing evidence remains. |
| Readiness                  | `blocked_external` | Deterministic failure, burst, and memory evidence are implemented; provider-observed loss and operator-driven physical environment transitions remain.                                                                                              |
| Descope cleanup            | `verified`    | AF_XDP/DPDK paths are removed from `main`, the cloud market-data plane is quarantined, and the Stage 0 workspace gate passes.                                                                                                                        |


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
as optional, deferred, or parallel with Coinbase/Rithmic/UI/readiness work.

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

**Status:** `blocked_external`

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

**Status:** `blocked_external` (implementation is complete; external physical pacing evidence remains)

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
- [x] Forward native pointer crosshair, pane/axis dragging, wheel zoom/scroll, and axis reset into Origin.
- [x] Expose an exact GPUI header control that restores Origin's fitted time and automatic price scales.
- [x] Expose a distinct GPUI header control that returns a scrolled chart to the newest live bar without changing zoom.
- [x] Scope keyboard bar navigation, zoom, fit/reset, return-to-latest, and gesture cancellation to the focused chart surface.
- [x] Render exact crosshair, pan, time-scale, and price-scale cursors for native chart gestures.
- [x] Bound hidden-window work and retained state.

The native header now uses GPUI Component dropdown menus for exact contract and
series selection. The instrument menu includes a compact symbol query input,
remains available after an empty result, and renders only the bounded,
generation-fenced catalog result set. The current contract and series are
checked, so selection no longer relies on opaque click-to-cycle behavior. Enter
submits the query, and the Search action is visibly loading/disabled while the
single bounded provider search is in flight.

The DOM and feed-health rail now uses the GPUI Components resizable-panel
primitive rather than a fixed-width container. The chart and rail remain flush,
the rail is bounded to 240–640 px, and its width persists while switching
between DOM, health, and the closed state.

The header keeps separate DOM and Health controls with explicit selected states
instead of renaming an open panel to an ambiguous Chart action. Each selector,
chart action, theme action, and rail action exposes a concise tooltip, including
the Home and End chart shortcuts. The open rail has its own flush title and
close control while preserving the chart-to-rail seam.

Contract and series selectors now keep exact contract/venue identity, expose
their existing bounded search/selection/history status in the relevant dropdown,
and visibly gate duplicate single-flight requests. Sanitized authentication and
recovery context appears only in the existing empty/stale/error chart notice;
Ready remains overlay-free. Reconnect, stop, channel loss, and catalog-session
invalidation clear pending interaction state. Chart focus remains scoped for
keyboard navigation without drawing a container outline over the chart.

The always-mounted empty Origin entity now reports whether a real provider
snapshot is installed. Empty loading/error surfaces show a centered status;
stale or reconnecting charts retain their real candles and show only a compact
top-left notice. Ready charts have no overlay, and the redundant outer chart
border is removed so the chart remains flush against the resizable rail.

Physical disconnect and suspend retirement now preserve the exact selected
contract, venue, and series for the next authenticated generation. Retired DOM
and feed-health state is cleared immediately, retained Origin candles move
through truthful stale/reconnecting states, and interrupted initial autoload is
eligible to retry after authentication. Manual contract replacement also clears
the old depth image before the new selection is installed.

The shipping selection profile requests trades, quotes, and complete depth.
Catalog selections preserve their exact validated entitlement identity across
the adapter callback, history seed, live chart, and DOM; a delayed venue uses
its base exchange only for provider reference/subscription routing and is never
silently relabeled in canonical market metadata. Feed diagnostics now observe
all four canonical market-event classes at the generation-fenced runtime
boundary, so quote/depth counters and book readiness reflect the actual stream.
Terminal worker loss is an idempotent stopped transition with a truthful chart
error, while retained candles remain inspectable through Fit and Latest.



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

Visible time-bar replays add a bounded four-day non-trading envelope, plus 150
daily session-padding bars, and reject theoretical responses above 10,000 bars.
Provider output is sorted and only the newest 300 visible bars are resequenced
and retained. A Saturday regression proves that the one-minute request reaches
the prior session with a maximum of 6,063 bars. The ordinary native-vault smoke
uses the same weekend-safe bound.

A later native-vault-only repeat on 2026-08-07 again passed both authenticated
ticker sessions, the 16-result search, reference, trades, quotes, depth,
reconnect identity, and clean close, and returned 276 one-minute bars plus seven
100-trade bars for `MNQU6` on `CME`. The smoke now also sends an explicit
protocol heartbeat on each authenticated ticker session and requires both
responses to be accepted before reporting success.

Windows frame evidence on 2026-08-07: the native display probe identified
`LG ULTRAGEAR` (`\\.\DISPLAY1`) at 2560x1440, 1.25x scale, and 165,000 mHz.
A 256-frame debug-profile Origin replay run after 32 warmup frames reported a
13.33 ms frame-callback interval p50 (about 75 Hz). The report now labels GPUI
`on_next_frame` as post-render callback cadence rather than physical scanout.
This does not satisfy the named 60/120/144 release-profile matrix, so the gate
remains open.

The schema-3 Windows harness now samples the DWM primary-output compositor QPC
timeline around the real GPUI replay. It records `DwmFlush` boundaries,
refresh/displayed/completed counter advances, compositor cadence percentiles,
and late/dropped/missed deltas. It explicitly records
`physical_presentation_measured=false` and
`external_scanout_instrumented=false`; DWM evidence is compositor evidence only.
No named 60/120/144 physical run has been captured, so the gate remains open.

`tools/verify_physical_pacing_matrix.ps1` is the fail-closed acceptance boundary
for that gate. It requires distinct externally instrumented 60/120/144 Hz
captures tied to one full source revision and measured-binary hash, validates
each capture hash and percentile/counter shape, and explicitly rejects DWM,
GPUI callback, or compositor-only reports.

The operator workflow prepares one clean, locked release build and freezes its
executable plus `Cargo.lock`, emits exact external-capture templates, runs the
same binary once at each required display mode, and retains schema-3 DWM
diagnostics only as supporting mode evidence. Finalization pins every external
and supporting artifact by SHA-256 before invoking the matrix verifier. External
scanout instrumentation is still required; these tools do not synthesize it.

The capture bundle for clean revision `f003ebb` was prepared on 2026-08-08 with
release executable SHA-256
`5ACB3485508FF602C1BB8B3C94C819ACA39AA0CB71FA56716B8E097C44F6E980`.
Supporting schema-3 GPUI/DWM profiles passed on the named `LG ULTRAGEAR` at
59.999 Hz (1920x1080), 119.999 Hz (2560x1440), and 144.000 Hz (2560x1440).
Their frame-callback p50 values were 16.6649 ms, 8.3364 ms, and 6.9598 ms;
their SHA-256 hashes are respectively `8F8D2CE79975160390C97214478CF690970E2B812D5A106A428AAFFADDDFE76B`,
`D36053AD8C8FBE378FAA95A07E067BA157DDE81363D6D60A984A2564D74A9B8C`,
and `6B72E30D4989CE2477559C15F384320147DF8A1906410FD4A1293AF06681B830`.
The original 165 Hz display mode was restored after every capture. No external
scanout instrument was installed or connected, so these supporting compositor
profiles do not satisfy the physical-presentation gate.

- [x] No GPUI-thread network, storage, protobuf, or aggregation work.
- [x] Correct DOM gap recovery.
- [x] Responsive symbol/timeframe replacement.
- [ ] Measured 60/120/144 Hz frame pacing on named hardware.
- [x] Test and live states are visually explicit.



## 11. Stage F — Readiness

**Status:** `blocked_external` (deterministic readiness evidence passes;
provider-observed loss and physical lifecycle evidence require external events)

### Remaining work

Capture evidence for:

- [x] Slow consumers and publication overflow.
- [ ] Provider-observed heartbeat and message-silence loss.
- [x] Trade, history, and depth gaps.
- [x] Disconnect, bounded reconnect, and terminal failures.
- [ ] Suspend/resume and offline startup.
- [x] Burst traffic and frame-aligned conflation.
- [x] Cache corruption and covering resnapshot.
- [x] Current and high-water memory.

Deterministic evidence on 2026-08-07: the cross-platform ingest conformance now
emits named Stage F results rather than an opaque aggregate bitmask. A Windows
run passed slow-consumer publication overflow with atomic rollback, lifecycle
event overflow with last-generation retention, bounded reconnect exhaustion and
fresh-budget recovery, ordered-gap recovery, and corrupt-snapshot rejection.
The desktop suites separately pass authenticated cache-corruption fallback,
covering resnapshot after publication overflow, and terminal Rithmic failure
redaction. A native-vault-only authorized Test run on 2026-08-08 authenticated
three fresh generations and proved generation-scoped client-local inbound
suppression yields `MessageSilence` for generation 1 and `HeartbeatSilence` for
generation 2, followed by a confirmed stop of healthy generation 3 and a clean
protocol close. The run also exercised the production covering-history state
machine deterministically. This is authorized client-local fault injection, not
provider-observed heartbeat loss (`provider_observed_loss=false`), so the
heartbeat/message-silence row remains open.

A schema-3 passive provider-path recorder now runs the unmodified production
receive loop from an immutable, clean-worktree binary, loads credentials only
from the native vault, and writes a no-overwrite artifact bound to the full
source revision, executable hash, and `Cargo.lock` hash. It requires an exact
raw invalidation, transient retry, confirmed generation stop, strictly newer
authenticated recovery, and clean protocol close. It never claims provider
causation: a client-side observation cannot distinguish Rithmic from a network
middlebox. The artifact also records the exact observation generation's
negotiated heartbeat interval together with the shipping response and
message-silence timeouts. Qualification recomputes the production detector
ordering with checked arithmetic: heartbeat silence is feasible only when
`heartbeat + response < message`, while message silence is feasible when
`message <= heartbeat + response`; missing, stale-generation, altered, or
short-window timing fails closed. No real provider-path silence artifact has
been captured, so the row remains open.

A passive 180-second run from clean revision `645885f` on 2026-08-08 loaded the
native-vault credentials, authenticated generation 1, and observed the
unmodified production receive loop without local suppression or fault
injection. The session remained healthy until the observation deadline, so the
schema-3 artifact failed closed with `observation_timeout`, no observed
invalidation, and `qualified=false`. The negotiated heartbeat was 60 seconds,
with the shipping five-second response deadline and two-minute message-silence
deadline. This was a real attempted run, not qualifying loss evidence.

The passive heartbeat observation uses the shipping two-minute message-silence
window and a five-second heartbeat-response deadline. An earlier observed
Rithmic Test ten-second negotiated heartbeat can therefore reach
`HeartbeatSilence` before the broader message timeout; a regression locks that
ordering without changing production behavior or claiming provider causation.
With that exact Test timing,
passive `MessageSilence` cannot win the production timeout race; its evidence
remains externally unavailable unless a provider session negotiates a feasible
heartbeat interval and the corresponding raw invalidation is actually observed.

Windows native environment support landed on 2026-08-08. The platform runtime
registers bounded connectivity-hint and suspend/resume callbacks, and actual
registration plus initial-network probes pass on Windows. The shipping Rithmic
worker applies initial offline state before connection, cancels in-flight history
and clears chart/DOM/selection state on network loss or suspend, discards retired
callbacks, and reconnects only through a fresh session generation after
restoration. Deterministic shipping-path tests cover offline startup, network
recovery, suspend/resume, stale-history rejection, and retry/callback fencing.
Both native monitors are now mandatory before the provider can connect on the
shipping path. Registration or monitor-stream failure stops the active session,
clears live state, inhibits further reconnect in that worker, and surfaces only
a bounded coarse terminal status instead of continuing without lifecycle
fencing.
The Win32 callback ingress is nonblocking and loss-aware: connectivity bursts
atomically retain the latest provider-relevant state, while suspend/resume
bursts always publish a pending suspend before a coalesced resume and let a
newer final suspend cancel that resume. The history worker likewise conflates
undrained results to the latest generation and disconnects its command sender
before joining, so a full command queue cannot deadlock shutdown. Saturation and
full-queue regressions cover these exact boundaries.
The Stage F row remains open until physical offline and suspend/resume transitions
are captured as named evidence.

The shipping Rithmic retry ticket and feed-health diagnostics now preserve the
exact coarse invalidation reason, including `HeartbeatSilence` and
`MessageSilence`, rather than collapsing every transient failure into generic
transport recovery. A provenance-bound native-transition capture mode observes
the ordinary Rithmic Test worker and never triggers network or power changes.
Its fail-closed report requires ready state before each physical loss, native
callback ordering, successful synchronous generation retirement, cleared
chart/history/DOM/selection state, strictly newer native-vault authentication and
full rehydration after both offline recovery and suspend/resume, zero observer
overflow, and a final clean worker stop. The committed launcher rejects existing
artifacts, revalidates source/binary/lockfile provenance after the operator run,
and invokes a strict artifact verifier. Deterministic recorder and verifier tests
pass; no physical transition capture has been performed, so the row remains
open.

The schema-2 transition artifact also requires the initial-offline restoration
to authenticate and rehydrate before a separate online network loss, followed
by recovery before suspend/resume. Global callback ordinals, timestamps, and
retired/fresh generations must form one continuous sequence, and any native
monitor failure permanently disqualifies the capture.

Qualification also requires the application to start from an actual native
`Unavailable` network result. The operator then restores connectivity, waits for
full readiness, performs a separate online-ready loss/recovery cycle, and finally
performs suspend/resume. The initial state is one-shot: a later loss cannot be
relabelled as offline startup.

The schema-3 desktop readiness artifact now exercises the production live-chart,
history-handoff, and read-only DOM state machines together. It rejects a repeated
trade sequence without mutating the chart, latches history snapshot-required on
a sequence discontinuity and accepts only a newer covering generation, and
clears depth on a sequence gap until a covering complete image restores the
book. It also proves stale chart trades cannot mutate current state, equal or
retired history snapshots remain rejected until a newer covering generation
arrives, and retired DOM session/selection events cannot replace the current
selection. All named gap, recovery, and generation-fence outcomes pass on
Windows.

Desktop burst evidence on 2026-08-07: the headless
`--desktop-readiness` command published 10,000 successively newer Rithmic chart
snapshots without draining the UI mailbox. The fixed-capacity 32-item mailbox
retained exactly one item at series generation 10,000, and 10,000 attempted
frame-drain schedules admitted exactly one callback until completion. The
command writes schema-3 JSON and opens no window.

The same Windows run sampled the process working set throughout the burst:
12,763,136 bytes at baseline, 14,462,976 bytes current/high-water after the
burst, and 1,699,840 bytes sampled growth against a declared 67,108,864-byte
limit. This is bounded burst evidence.

A 2026-08-08 repeat after the catalog dispatch-fencing fix again retained one
item in the 32-item mailbox after 10,000 publications, admitted one pending
frame callback, and measured 1,413,120 bytes of working-set growth against the
same 67,108,864-byte limit.



### Gate

- [ ] All failure cases recover or fail closed as specified.
- [x] Memory and queues remain within declared bounds.
- [x] No stale generation reaches the model or UI.



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


Latency reporting must separate provider clock-relative timestamp age from local
socket-to-present processing. Every percentile report includes p50, p95, p99,
p99.9, maximum, sample count, warm-up, and loss/recovery counters.

## 14. Completion definition

This roadmap is complete when a user can launch the native shell, securely
authenticate to Rithmic Test, discover and switch supported instruments, view
tick and supported time-based charts, inspect a recovering read-only DOM, and
understand feed health without exposing credentials or licensed data.

### Roadmap completion checklist

- [x] Launch native shell and authenticate securely to Rithmic Test.
- [x] Discover and switch supported instruments.
- [x] View tick and supported time-based charts.
- [x] Inspect a recovering read-only DOM.
- [x] Understand feed health without exposing credentials or licensed data.
- [x] Deterministic replay evidence.
- [x] Authorized Test core-path evidence.
- [x] Bounded queues and memory.
- [x] Generation-fenced recovery.
- [x] Clean shutdown.
- [ ] Responsive frame pacing.
- [x] Finding-free local review and pushed `main` commit.

Production trading is not part of this completion definition.
