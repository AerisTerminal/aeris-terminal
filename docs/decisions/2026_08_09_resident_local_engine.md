# Resident local engine and cached-first startup

**Date:** 2026-08-09  
**Status:** accepted  
**Roadmap owner:** Stage H in `platform_creation_plan.md`

## Decision

Axiusflow will use one resident, per-user local data-engine process and one or
more presentation-only UI clients. The engine is the sole owner of provider
connections, history storage, workspace state, coverage repair, aggregation,
decoded caches, and background scheduling. The GPUI desktop process owns only
interaction, window state, and rendering.

The engine is local-first, not cloud-dependent. A cached workspace must open
without network access. When legally retainable history is already complete and
valid locally, Axiusflow must not download or recompute it again unless an
explicit invalidation dimension changes.

This decision retains the existing encrypted immutable segment store,
generation fencing, bounded queues, corruption quarantine, rights enforcement,
and progressive publications. It replaces GUI-process ownership and the Stage G
disposable-cache-only policy; it is not a rewrite of the validated data path.

## Process topology

```text
axiusflow_desktop (GPUI)
    |
    | authenticated, versioned local IPC
    v
axiusflow_engine (resident per-user process)
    |-- workspace and hot-set state
    |-- coverage and repair coordinator
    |-- provider-neutral scheduler
    |-- Coinbase and Rithmic coordinators
    |-- encrypted immutable segment store
    `-- bounded decoded and derived-series caches
```

Windows uses an installation-scoped named pipe with a current-user ACL. Unix
platforms use an installation-scoped Unix-domain socket. The UI connects to the
existing engine or starts it without a visible console and retries for a bounded
deadline. One installation lock prevents two engine owners. A second UI either
attaches as another client or activates the existing primary UI according to
the explicit client policy.

The engine may remain resident after the last UI closes. `Interactive`, `Warm`,
`Constrained`, and `OfflineSuspended` resource modes govern provider sessions,
prefetch, diagnostics cadence, and decoded-cache retention. Provider terms,
credentials, operating-system power state, or user policy may require a
connection to close even while the engine remains resident.

## Ownership boundaries

### Engine owns

- provider credentials for the bounded duration of a connection attempt;
- provider connections, reconnects, generations, and silence recovery;
- the only open `HistoryStore` and its cross-process lock;
- workspace revisions, selections, watchlists, viewports, and hot-set scores;
- catalog discovery and normalized instrument metadata;
- history coverage, missing-range repair, and confirmed-empty ranges;
- raw-to-normalized decoding and incremental bar/derived-series aggregation;
- immutable snapshot and delta publication through bounded IPC mailboxes;
- resource-mode transitions and maintenance.

### UI owns

- windows, panels, focus, keyboard and pointer behavior;
- design-system resolution and GPUI/Origin rendering;
- local presentation state that has no data-engine meaning;
- subscription interest for visible chart, DOM, catalog, and diagnostics views.

The engine and provider coordinators must not depend on GPUI, Origin Charts,
`chart_integration`, or `terminal_ui`. IPC messages carry provider-neutral
domain snapshots; the UI adapts those snapshots to Origin and GPUI.

## Cached-first startup contract

Startup is ordered by visible-user value:

1. connect to or start the local engine;
2. authenticate and negotiate the protocol version;
3. restore the small versioned workspace and hot-set manifest;
4. validate and decode the most recent cached visible chart blocks;
5. publish the cached chart and stable viewport to the UI;
6. establish or confirm the live provider connection in parallel;
7. compare local coverage and provider watermark;
8. fetch and verify only missing or invalidated ranges;
9. merge the new tail without resetting zoom, drawings, selection, or crosshair;
10. prefetch adjacent and recently used ranges only after visible work is idle.

Network availability must never gate a valid cached chart. Disk writes,
provider I/O, decoding, aggregation, and expensive analytics never execute on
the GPUI thread.

## Persistent workspace and hot set

The engine persists a bounded, checksummed, atomically replaced state containing
at least:

- schema and cache-manifest revisions;
- active provider, market, and interval;
- watchlist and recently used series;
- last stable viewport per recent series;
- last accepted provider and series watermarks;
- hot-set score and last-use time;
- resource-mode and warm-mode policy;
- references to eligible derived-series checkpoints.

Corrupt workspace state is quarantined independently from market history. The
engine falls back to a default workspace without deleting valid history.

## Coverage and integrity model

Every retained series range is classified as `Complete`, `Partial`,
`ConfirmedEmpty`, `Missing`, `Invalidated`, or `Quarantined`. Segment metadata
includes range, record count, first/last sequence where available, plaintext and
ciphertext checksums, source/schema/calendar/adjustment/correction revisions,
retention rights, and sealing state.

The coordinator computes the difference between requested coverage and valid
local coverage, schedules only missing ranges, verifies the result, and updates
the manifest transactionally. Opening older ranges progressively improves the
local store. A lack of records is never interpreted as complete coverage unless
the provider response proves a confirmed-empty range.

Old eligible segments become sealed and immutable. The active tail remains
bounded and mutable until its time/size boundary, then seals atomically. Direct
memory mapping of encrypted payloads is not a zero-copy path; mappings are
limited to eligible indexes or authenticated decoded block caches.

## Retention and provider rights

“Never download twice” applies only to data Axiusflow is legally and
contractually permitted to retain. Coinbase public data may use durable local
retention under the accepted product policy. Rithmic and future providers use
explicit entitlement-scoped policies: `Durable`, `Expiring`, `MemoryOnly`, or
`LiveOnly`. Expiry, entitlement change, logout, schema change, correction, and
key revocation invalidate the exact affected scope.

The former blanket “disposable 256 MiB LRU” decision is retired. Disk and RAM
remain bounded through configurable quotas and deterministic eviction, but a
quota evicts only eligible cold acceleration/derived data according to policy;
it must not silently violate a completeness promise.

## Priority scheduler

All engine work is scheduled under one explicit priority class:

1. `Critical`: visible cached snapshot, live continuity, and gap recovery;
2. `Interactive`: symbol/timeframe change and user-requested pan into history;
3. `Prefetch`: neighboring ranges and likely next views;
4. `Background`: recently used series and derived checkpoints;
5. `Maintenance`: sealing, expiry, compaction, and quota enforcement.

Priority is combined with workspace/selection/provider generations and
cancellation. Newer interactive intent supersedes stale work. Background work
must yield CPU, disk, and network capacity to critical and interactive work.

## IPC and failure behavior

The protocol is length-bounded, append-only, versioned, and authenticated with
an installation token stored in the OS credential vault. Every chart/DOM delta
is fenced by engine epoch, workspace revision, selection generation, and
provider generation. Slow clients receive a bounded covering snapshot or an
explicit backpressure fault; ordered data is never silently dropped.

On engine crash, the UI keeps the last immutable frame visible, marks it stale,
starts or reconnects to one engine, restores subscriptions, and accepts only a
covering newer generation. On UI crash, the engine remains healthy and releases
that client's subscriptions. Protocol incompatibility produces a controlled
restart/update diagnostic, never undefined decoding.

## Derived data

Normalized bars and eligible analytical series are persisted separately from
raw/provider capture. Incremental indicators persist the minimum resumable
checkpoint needed to continue from the sealed prefix. A revision of formula,
input series, session calendar, adjustment, or parameters invalidates only the
affected derived identity. No indicator is recomputed over complete immutable
history merely because a new tail arrived.

## Performance and correctness gates

Initial targets on named reference hardware are:

- application shell visible: p95 <= 300 ms;
- restored cached workspace state delivered: p95 <= 400 ms;
- cached chart first visible frame: p95 <= 500 ms;
- local cached-range query: p95 <= 10 ms for the declared block size;
- visible update-to-frame: p95 <= one display interval, p99 <= two;
- UI-thread network, storage, blocking receive, sleep, or join: zero;
- idle UI self-sustaining frames: zero;
- lost confirmed market events: zero;
- undetected historical gaps: zero;
- stale generation accepted after selection/restart: zero.

Cold/warm startup, offline startup, corrupt state, rapid selections, engine/UI
crash, provider gap, suspend/resume, quota pressure, and 60/120/144 Hz rendering
must have deterministic tests plus named physical evidence. Targets are revised
only with recorded measurements, never to excuse a regression.

## Rejected alternatives

- **GUI-owned provider/storage workers:** cannot remain warm after UI exit and
  recreates ownership/lifecycle coupling.
- **Download everything before first paint:** violates perceived startup and
  offline use.
- **Keep all history decoded in RAM:** creates unbounded memory and startup work.
- **Cloud backend as the primary cache:** violates the local-first product edge.
- **Rewrite the validated store/coordinators:** discards stronger correctness
  guarantees without solving process ownership.
- **Memory-map encrypted segment files directly:** does not remove
  authentication/decryption and creates a false zero-copy claim.

## Completion condition

This decision is complete only when the desktop has no in-process provider or
history-store ownership, the resident engine is the shipping default, cached
offline startup and gap repair pass, warm-mode lifecycle is packaged and
observable, the old path is deleted, and every performance/correctness gate has
recorded evidence.
