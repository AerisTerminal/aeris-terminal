# S2-25 decision: worker-owned startup and history correctness

**Status:** accepted for deterministic correctness; performance and release certification remain open

**Evidence command:** `tools/run_startup_history_correctness.sh`

## Decision

Use `crates/desktop_history` as the desktop-local composition boundary between
the encrypted immutable segment store, provider-history handoff, and chart
consumers. A dedicated worker owns blocking storage access, payload decoding,
and provider snapshot/live transitions. The GPUI thread receives only shared
immutable `Arc<HistoryPublication<T>>` generations from a bounded cache.

The worker is given the GPUI thread identity at construction, rejects creation
on that thread, and rejects later calls from any thread other than its owner.
Axiusflow control-plane availability is an observed input only: it cannot alter
local cache reads, direct-provider recovery, or offline behavior.

## Correctness matrix

The deterministic integration suite uses the real encrypted
`HistoryStore` and covers:

- declared cold and warm OS-cache cases with empty and populated application
  caches, while making no OS-cache latency claim;
- warm in-memory reuse without a second storage read or decode;
- warm entries and direct chart/publication access revalidating their exact
  segment key and retention deadline, rejecting a wrong key and expiring the
  publication before reuse with the same expiration-first ordering as storage,
  while removing the expired catalog record and encrypted backing file;
- authenticated local hydration while the provider is offline and the control
  plane is unavailable;
- an empty offline cache returning explicit provider-refetch unavailability;
- an unavailable control plane not preventing a direct-provider history
  request;
- a live-only missing-history policy remaining live-only even while the provider
  connection is online;
- a changed schema revision missing the old exact segment rather than restoring
  stale data;
- a damaged encrypted segment being quarantined and returning its explicit
  recovery path;
- authenticated bytes that decode to a gapped sequence failing closed before
  any chart publication;
- a segment above the configured read bound being rejected from cataloged
  payload metadata before its deliberately damaged file is opened or decoded;
- an expanded encrypted file whose cataloged payload still fits being rejected
  from file metadata against the caller's payload bound plus exact codec
  overhead before allocation or reading.

## Shared publication and handoff

The worker opens and recovers storage only after rejecting GPUI-thread
construction. Every local or provider-only identity passes the same bounded
field and revision validation before handoff state is retained. The cache has
explicit entry, decoded-byte, chart-binding, and generation bounds.
The worker is non-`Send`, so its storage owner and destructor cannot migrate to
the GPUI thread after construction.
Multiple charts bound to the same history identity receive the same `Arc`, rather
than independent decoded copies. Replaced or evicted generations retained by
consumers remain charged until their final `Arc` is released, with released
generations pruned before current memory usage is reported. Active provider
handoffs pin their current publication against eviction, and the worker bounds
the number of handoffs and requires explicit retirement before reusing capacity.
New handoffs inherit a cached provider generation and watermark as their minimum
snapshot floor, so a provider snapshot cannot visibly regress already-published
provider history. Local segment generations are worker-local, so they contribute
only their sequence watermark during the first provider cutover, and expired
local state is removed before it can seed any floor. Handoff state is moved out
of the worker map for transactional mutation
rather than deep-cloning buffered owning payloads.
A provider snapshot and all buffered contiguous live items publish as one
immutable generation. Duplicates do not publish, gaps keep the last valid
generation visible, and subsequent live values publish by replacement rather
than partial mutation.
Snapshot values move into publications without duplication. Live replacement
proves capacity for both the current and candidate generation before cloning.
Snapshot overlap is charged only for the live suffix that survives cutover,
including at the maximum sequence watermark without saturating arithmetic.
The eviction planner proves the complete unbound victim set before removing any
entry, so a rejected publication cannot partially evict unrelated history.
The decoder first computes its exact retained charge without retaining decoded
values. The cache then transactionally reserves that capacity and removes only
the necessary unbound victims before decoded allocation begins, so old and new
history never transiently exceed the configured bound.
Worker-local temporary references are released before replacement accounting,
and oversized snapshots are rejected from their reported decoded charge before
their values are cloned into a publication.
The decoder receives its reserved worker-wide decoded-memory allowance before
allocation. Cached generations, externally retained generations, and buffered
live values across every handoff share the same global decoded-byte budget.

Cache replacement and handoff advancement are transactional at this boundary.
The coordinator applies provider transitions to a bounded candidate state and
commits a new visible value only after the replacement publication fits. The
suite forces a decoded-memory overflow, proves that the visible generation
remains unchanged, and requires a newer snapshot covering the unretained live
sequence rather than relying on an impossible lower-cost retry.
Live item-count overflow, live decoded-byte overflow, and sequence gaps latch a
snapshot-required watermark covering the observed but unretained sequence. The
floor advances for every later observation that also cannot be retained. The
suite proves that a snapshot below that floor is rejected and a covering newer
snapshot recovers without silently skipping any observation.

## Claim boundary

This evidence proves deterministic local correctness for the named matrix and
hard configured memory/read bounds. It does not measure cold or warm OS-cache
latency, GPUI presentation, peak RSS, allocation count, physical disk I/O,
provider request latency, cross-platform packaging, huge workspaces, hidden-tab
policy, indicator readiness, or pan/zoom and symbol/timeframe interaction. It
does not implement a provider network adapter or connected desktop runtime.
Those remain under `S2-19`, `S2-20`, `S4-01`, `S4-09`, and `S4-10`.
