# Architecture and Performance Audit

Status: `active`. This audit gates Stage G: it is treated as unverified until the
P0/P1 findings below are remediated and the acceptance measurements are recorded.

Scope: thread ownership, provider lifecycle, history/cache ownership, selection
generations, queues, UI invalidation, chart rendering, memory, and shutdown for
the Coinbase default path. Rithmic behavior and deferred scope are unchanged.

## Confirmed findings

### P0-1: Symbol/timeframe changes recreate the complete worker and contend for the single-owner history store

`TerminalApp::select_interval` and `select_instrument` spawn a brand-new worker
thread per selection (`apps/desktop/src/main.rs:787`, `:879` via
`MarketDataWorker::start_coinbase_product_interval`). Each new worker opens the
encrypted history store, which is single-owner
(`crates/desktop_storage/src/store.rs:1033`). The replaced worker releases the
lock only when its thread finishes. First observed failure:
`desktop market history store is already open by another worker` surfaced to the
chart and the switch silently failed.

Remediation direction: one lifetime coordinator owns the store; selection changes
are latest-wins commands processed on the owner thread. No worker recreation, no
store-lock retry, no detached shutdown chains.

### P0-2: Serialized-shutdown patch did not fix the release behavior

Commit `eb36cc6` added a 30-second previous-shutdown handoff
(`apps/desktop/src/live_market_worker.rs`, `await_previous_shutdown`). Physical
exercise still reports stuck timeframe changes and a frozen window. The handoff
treats the symptom (lock timing) instead of the cause (per-selection recreation)
and can stack waiting workers during rapid changes.

### P0-3: Rapid selection changes chain replacement workers instead of conflating

Each click spawns a worker before any previous replacement has finished opening
the store or fetching history. There is no latest-wins conflation of selection
intent; expensive paginated fetches run to completion for selections the user
has already abandoned (bounded only by page-level cancellation).

### P1-1: Active window self-sustains a continuous frame loop without new state

`schedule_market_frame` (`apps/desktop/src/main.rs:1408`) schedules
`on_next_frame`, whose callback unconditionally calls `cx.notify()`
(`apps/desktop/src/main.rs:1417`). Every notify re-renders, which re-schedules
the next frame. While the window is active this is a permanent refresh-rate
render/poll loop even with zero new market, input, or diagnostics state.
Measured symptom: the release build averaged roughly 18% CPU (271 s over
~25 min) on an i7-13700K while effectively idle.

Remediation direction: edge-triggered wake bridge; an empty-to-nonempty mailbox
transition schedules exactly one drain; the frame callback notifies only when a
drained message mutated UI state.

### P1-2: Root render reclones and refilters catalog and interval state

- `available_intervals` allocates a filtered `Vec<ChartInterval>` on every call
  (`apps/desktop/src/main.rs:752`), including per overlay row rendering.
- `instrument_entries` clones and filters the full Coinbase product catalog per
  query keystroke and per render (`apps/desktop/src/main.rs:811`).
- The root `TerminalApp::render` rebuilds the header state object, status
  strings, and overlay tree each frame (`apps/desktop/src/main.rs:2062`),
  invalidating the chart subtree for status-only changes.

Remediation direction: retained header/overlay entities, cached search results,
virtualized catalog list, static interval slices.

### P1-3: Long intervals block first paint on complete paginated history

`fetch_history_with_adapter`
(`apps/desktop/src/live_market_worker/history.rs:428`) fetches every page for
the full HISTORY_BARS range and only then installs anything. For coarse
intervals sourced from fine granularity (e.g. 1M from 1D), first useful paint
waits for the entire multi-page download. Cancellation is checked between pages
but nothing is published progressively.

Remediation direction: publish authenticated cache immediately, then the newest
provider page, then merge older pages until viewport plus overscan is covered.

## Verified-healthy subsystems (do not regress)

- Data path correctness on the live network: `--coinbase-live-smoke BTC-USD`
  passes with covering snapshot and clean shutdown (run 2026-08-09).
- Bounded queues and mailboxes: 32-item UI mailbox, bounded inbox/commands,
  generation-fenced recovery (workspace conformance tests).
- 256 MiB disk-cache quota, 64 MiB burst-growth bound, 256-level DOM bound.
- Deterministic shutdown of an inflight fetch (conformance:
  `shutdown_during_an_inflight_fetch_completes_boundedly`).
- Single-owner store lock itself works as designed; the failure is the
  contention architecture, not the lock.

## Baseline measurements

Full baseline capture (cold/warm startup, idle, streaming, rapid changes,
catalog search, pan/zoom, drawing, network recovery, shutdown at 60/120/144 Hz
on the i7-13700K / RTX 4080 / 2560×1440 UltraGear) is recorded here as each
remediation batch lands.

| Scenario | Metric | Baseline (pre-remediation) | Post-remediation |
|---|---|---|---|
| Live smoke BTC-USD | snapshot within 45 s deadline | passed (~seconds) | |
| Idle window, active | CPU | ~18% avg over 25 min | |
| Idle window, active | working set | ~97 MB | |
| Timeframe switch | outcome | lock error / stuck | |
| Theme toggle | outcome | reported stuck | |

## Acceptance gates (post-remediation)

- Pending selection feedback on the next submitted frame.
- Active update-to-frame p95 within one display interval, p99 within two, at
  60/120/144 Hz.
- Idle windows run no self-sustaining frame loop.
- At most one UI drain/render request pending per window.
- No UI-thread operation blocks on I/O, sleep, channel receive, or thread join.
- Store lock conflicts are only a cross-process startup diagnostic, never an
  in-process selection failure.
- Existing bounds (32-item mailbox, 64 MiB burst growth, 256 MiB disk cache,
  256-level DOM) remain enforced.
