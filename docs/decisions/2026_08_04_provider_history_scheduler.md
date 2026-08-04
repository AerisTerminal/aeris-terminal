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

`crates/adapters/coinbase_market` implements the first direct fetch profile for
Coinbase BTC-USD and ETH-USD public one-minute candles, whose reviewed price and
quantity scales match the canonical profile. Requests use only the public account and
`crypto_public_realtime` entitlement scope, a canonical Coinbase instrument
identity, minute-aligned half-open ranges, and at most the provider's documented
350 candles. The local profile intentionally permits one request and one
in-flight page per second. Ticks and depth remain unsupported.

The direct adapter calls Coinbase's public Advanced Trade candle endpoint over
rustls with an explicit AWS-LC provider, system-independent WebPKI roots, no
credential header, a single capacity-one resolver worker, a 15-second absolute
DNS/connect/TLS/HTTP deadline, and a 1 MiB response bound.
Coinbase's inclusive REST `end` is translated from the internal half-open range
to its final included minute before dispatch, preventing the provider's newest-
first limit from displacing the oldest requested candle. Provider candles are
then filtered to the exact requested range, ordered oldest first, and decoded
without a floating-point round trip. Their stable sequence
is derived from the UTC minute, so an absent provider bucket remains an
observable sequence gap instead of being renumbered away. The versioned binary
payload repeats and binds sequence, event time, and fixed-point OHLCV; the
desktop decoder rejects any mismatch between payload and `HistoryItem`
metadata.

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

The evidence command runs eight scheduler contract tests, three handoff tests,
and six Coinbase adapter tests. They cover unsupported
capabilities and invalid bounds, expired-request eviction/reporting, priority,
exact deduplication before and during dispatch, atomic adjacent prefetch, queue
and interest bounds, rate and concurrency gates, shared and final-owner
cancellation, bounded fetch failure, monotonic quota timing across wall-clock
correction, page validation, cursor continuation, overlap removal, explicit
empty-snapshot watermarks for global sequences, contiguous cutover, gaps, and
buffer overflow. The Coinbase cases additionally prove public-only scope,
minute/range/page bounds before transport, exact provider path construction,
out-of-order response normalization, exact fixed-point conversion, duplicate
and over-precision rejection, and payload/metadata identity binding. The Stage
2 live Coinbase lane fetches and decodes a non-empty bounded HTTPS page for each
configured product before its WebSocket observation, while retaining
`production_deployment=not_exercised`.

The deterministic suite does not prove provider quota behavior. The live lane
proves only the authorized public Coinbase endpoint and one bounded page per
BTC-USD and ETH-USD product; it does not prove another Coinbase precision
profile, rights for another provider, Rithmic/CQG
compatibility, desktop startup, storage integration, cache retention, UI
smoothness, cross-platform behavior, latency, throughput, or production
performance. `S2-19` owns desktop provider-runtime composition, `S2-25` owns
startup/history correctness, and `S2-20` owns reproducible performance
evidence.
