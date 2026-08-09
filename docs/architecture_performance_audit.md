# Architecture and Performance Audit

Status: `active`. This audit gates Stage G: it is treated as unverified until the
P0/P1 findings below are remediated and the acceptance measurements are recorded.

Scope: thread ownership, provider lifecycle, history/cache ownership, selection
generations, queues, UI invalidation, chart rendering, memory, and shutdown for
the Coinbase default path. Rithmic behavior and deferred scope are unchanged.

## Confirmed findings

### P0-1: Symbol/timeframe changes recreate the complete worker and contend for the single-owner history store

Remediated (commit `ab0dc4e`). One lifetime coordinator owns the history thread,
environment monitors, and the store session for the application lifetime;
selection changes are latest-wins commands fenced by a monotonically increasing
sequence and rebuild the provider session in place on the owner thread. No
worker recreation, no store-lock retry, no detached shutdown chains.

Original finding: `TerminalApp::select_interval` and `select_instrument` spawned
a brand-new worker thread per selection. Each new worker opened the encrypted
history store, which is single-owner
(`crates/desktop_storage/src/store.rs:1033`). The replaced worker released the
lock only when its thread finished. First observed failure:
`desktop market history store is already open by another worker` surfaced to the
chart and the switch silently failed.

### P0-2: Serialized-shutdown patch did not fix the release behavior

Remediated (commit `ab0dc4e`). The 30-second previous-shutdown handoff added by
commit `eb36cc6` is removed together with the per-selection recreation it
patched around; the store lock is now always released before any reacquisition
because one coordinator owns the session.

### P0-3: Rapid selection changes chain replacement workers instead of conflating

Remediated (commit `ab0dc4e`). Selection intent conflates latest-wins inside the
coordinator; stale sequences are fenced on publish and abandoned fetches cancel
between pages (conformance: `stale_selections_are_fenced_to_the_newest_requested_sequence`,
`reselection_ends_the_session_cleanly_without_store_contention`).

### P1-1: Active window self-sustains a continuous frame loop without new state

Remediated (2026-08-09). The UI mailbox is now edge-triggered: an
empty-to-nonempty transition fires one registered wake
(`MarketWorkerMailbox::fire_mailbox_wake`), which schedules exactly one drain on
the foreground executor through a std-only waker flag (`UiWake`). The drain
notifies only when messages were applied; an idle window schedules no frames.
Window reactivation schedules a single gated drain for backlog accumulated
while inactive. The final sender drop fires the wake so disconnect handling
stays timely. Conformance: `mailbox_wake_fires_once_per_edge_until_drained`,
`mailbox_wake_fires_for_queued_messages_on_registration`,
`mailbox_wake_fires_when_last_sender_drops`,
`mailbox_wake_stops_after_receiver_drops`.

Original finding: `schedule_market_frame` scheduled `on_next_frame`, whose
callback unconditionally called `cx.notify()`; every notify re-rendered and
re-scheduled the next frame. Measured symptom: the release build averaged
roughly 18% CPU (271 s over ~25 min) on an i7-13700K while effectively idle.

### P1-2: Root render reclones and refilters catalog and interval state

Partially remediated (2026-08-09). `available_intervals` now returns static
slices (`COINBASE_INTERVALS` / `ChartInterval::ALL`); the timeframe overlay
consumes `&'static [ChartInterval]` with no per-call allocation.

Deferred, with rationale: retained header/overlay entities, cached search
results, and the virtualized catalog list. With P1-1 fixed, renders happen only
on state change, so the catalog filter/clone runs per keystroke or overlay open
instead of per frame; the remaining cost is bounded (~hundreds of small
allocations per interaction, not per refresh tick). The entity split is a
larger render-tree restructure that needs a visual acceptance pass; it is
scheduled only if post-remediation measurements still show a problem.

### P1-3: Long intervals block first paint on complete paginated history

Remediated (commit `ab0dc4e`). History delivery is progressive: a recent
one-page phase publishes the newest bars immediately, then the full range
installs (conformance:
`recent_phase_publishes_the_newest_page_before_the_full_range`). A latent
aggregation bug exposed by the new phase (bucket-aligned bars emitted before
the requested range start were rejected by strict page validation) is fixed.

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
| Live smoke BTC-USD | snapshot within 45 s deadline | passed (~seconds) | passed 2026-08-09 (post P1-1) |
| Idle window, active | CPU | ~18% avg over 25 min | pending physical exercise |
| Idle window, active | working set | ~97 MB | pending physical exercise |
| Timeframe switch | outcome | lock error / stuck | pending physical exercise |
| Theme toggle | outcome | reported stuck | pending physical exercise |

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
