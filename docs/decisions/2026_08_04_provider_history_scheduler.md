# S2-24 decision: bounded provider-history scheduling and live handoff

**Date:** 2026-08-04
**Status:** implemented
**Evidence command:** `tools/run_provider_history_conformance.sh`

## Decision

Use `crates/provider_history` as the provider-neutral, worker-side contract for
history capabilities, bounded request scheduling, response validation, and
snapshot/backfill/live handoff. It contains no provider SDK, network runtime,
filesystem access, GPUI work, or cloud dependency. A direct desktop provider
runtime owns those integrations and implements `ProviderHistoryAdapter` after
the provider supplies the applicable protocol, rights, credentials, and test
environment.

The exact request identity includes provider, account, entitlement revision,
instrument, data class, resolution, time range, page bound, and continuation.
This prevents work or results from crossing account and entitlement scopes.

## Capability contract

Each provider profile declares bars, ticks, and depth independently. A
supported class supplies its implemented resolutions, maximum lookback,
maximum request span, maximum page size, pagination style, fixed-window request
rate, and maximum concurrency. Unsupported classes fail explicitly with a
reason. Profiles describe only implemented Axiusflow behavior; they are not
inferred from another provider or represented as provider-certified limits.

`crates/adapters/coinbase_market` exposes a fail-closed profile: bars, ticks,
and depth are all unsupported until a desktop-local fetch adapter implements
the shared request and completion contract. The existing centralized Coinbase
REST backfill remains reference evidence; it is not advertised as a capability
of this unconnected desktop worker boundary.

Rithmic and CQG capability profiles, fetch adapters, fixtures, and certification
remain intentionally absent. That work starts only after partnership access
provides the authoritative materials and rights. This does not block shared
platform work or testing with deterministic fixtures and authorized Coinbase
crypto data.

## Scheduler and handoff contract

- Every queue, in-flight set, and request fan-out has an explicit finite bound.
- Exact queued or in-flight requests deduplicate across interested consumers;
  cancellation removes one consumer without aborting shared work and reports
  provider aborts only when no consumer remains. An aborting dispatch continues
  occupying its concurrency slot until the worker acknowledges the abort or a
  valid completion arrives.
- Visible requests precede adjacent prefetch and background work. Previous and
  next adjacent ranges are derived only when valid, and submission preflights
  the complete bounded set before mutating scheduler state.
- Dataset concurrency and monotonic fixed request-rate windows gate dispatch;
  Unix time is used separately for provider lookback validation. Provider
  requests that age beyond their declared lookback are removed, reported with
  their interested consumers, and do not block later valid work. Provider
  continuations must match the declared pagination style and range, progress
  before a following page is scheduled, and remain within a bounded per-request
  continuation history.
- Mismatched, oversized, out-of-range, unordered, or invalidly paginated pages
  are rejected while the original in-flight request remains available for
  retry or explicit cancellation.
- Provider fetch failures finalize their dispatch immediately. They requeue
  only within the configured attempt and queue bounds; terminal failures release
  scheduler capacity and report every affected consumer.
- Live events buffer behind a verified contiguous snapshot. Snapshot overlap is
  discarded, empty snapshots require an explicit provider cutover watermark,
  only the contiguous live suffix is released, and gaps, overflow, generation
  regression, or watermark regression latch `SnapshotRequired` instead of
  silently continuing.

## Evidence and limitations

The evidence command runs eight scheduler contract tests, three handoff tests, and
the Coinbase fail-closed capability-profile test. They cover unsupported
capabilities and invalid bounds, expired-request eviction/reporting, priority,
exact deduplication before and during dispatch, atomic adjacent prefetch, queue
and interest bounds, rate and concurrency gates, shared and final-owner
cancellation, bounded fetch failure, monotonic quota timing across wall-clock
correction, page validation, cursor continuation, overlap removal, explicit
empty-snapshot watermarks for global sequences, contiguous cutover, gaps, and
buffer overflow.

This is deterministic contract evidence on the current host. It is not a live
provider request, provider quota, provider-rights, Rithmic/CQG compatibility,
desktop startup, storage integration, UI smoothness, cross-platform, latency,
throughput, or production-performance claim. `S2-19` owns desktop provider
runtime integration, `S2-25` owns startup/history correctness, and `S2-20` owns
reproducible performance evidence.
