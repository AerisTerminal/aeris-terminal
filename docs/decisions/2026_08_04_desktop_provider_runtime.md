# S2-19 decision: bounded desktop provider lifecycle owner

**Status:** accepted as a partial runtime and Coinbase lifecycle composition; shipping desktop integration remains open

**Evidence commands:** `tools/run_desktop_provider_runtime_conformance.sh`,
`cargo test --locked --package axiusflow_coinbase_market_adapter`, and
`tools/run_native_network_monitor_conformance.sh`

## Decision

Use `crates/desktop_provider_runtime` as the provider-neutral, single-writer
lifecycle owner around direct desktop provider adapters. The owner runs on one
declared worker thread. Credentialed adapters load opaque credentials from the
existing `CredentialVault` for every connection attempt, bound their size, lend
them only to the provider start call, and zeroize the returned vault buffer
before the attempt returns. Explicitly keyless public adapters receive an empty
borrowed credential slice and never perform a synthetic vault lookup.

Every connection attempt receives a monotonically increasing nonzero local
generation. Provider establishment, invalidation, and immutable
`MarketStreamPublication` callbacks must carry that generation. Suspend,
network loss, transport invalidation, and semantic-queue overflow fence and
request a stop for the active generation; delayed callbacks are rejected without
changing the last accepted immutable publication. If the provider cannot confirm
that stop, the runtime blocks all replacement sessions until an explicit cleanup
retry succeeds. Resume and network restoration reload vault credentials and
start a fresh generation only when a connection was already desired before the
environmental transition; an idle runtime remains idle.

The application boundary is a fixed-capacity semantic event queue. It does not
contain wire frames, provider credential material, a cloud client, or an upload
method. Overflow is explicit, retains the prior immutable publication, requests
a provider stop, and latches snapshot/reconnect recovery rather than
growing memory or silently skipping ordered state. Runtime `Debug`, metrics,
and errors expose only lifecycle state, capacities, counters, and coarse failure
classes; they omit the credential key, credential bytes, provider error text,
queued publication contents, and subscription identifiers.

`DesktopMarketWorker` composes this lifecycle owner with the existing
`HistoryWorker` on the same non-`Send` worker boundary. Each active history
handoff is registered against the streaming provider session generation before
provider callbacks can mutate handoff or cache state. Suspend, network loss,
transport invalidation, and permanent stop retire every bounded handoff after
fencing provider callbacks. A fresh provider generation can then start a new
handoff for the same identity, while delayed live and snapshot callbacks from
the retired generation fail before reaching the history worker. Local
visible-range hydration and chart publication remain available without an
active provider session or Axiusflow control-plane availability.

The first direct session driver uses the authorized keyless Coinbase public
WebSocket. It opens TLS and subscribes off the owning worker, reports actual
post-handshake establishment, delivers trades through a fixed-capacity channel,
and observes cooperative stop requests through DNS, TCP, TLS, subscription, and
stream reads in 100 ms I/O slices. One connection-wide continuity cursor
includes subscription acknowledgements, heartbeats, and market-trade messages,
and five seconds without a subscribed-channel message invalidates a silently dead connection. Queue
overflow, sequence gaps, peer closure, and transport failure terminate the
session and expose only a coarse invalidation reason. The driver owns at most
one generation thread, rejects stale stops, joins confirmed shutdown, and the
next generation atomically clears prior queued callbacks only after that join.
The composed market worker advances lifecycle state before returning a trade
from the matching streaming generation.

`NativeNetworkMonitor` supplies the same `NetworkEvent` type consumed by the
lifecycle owner. Its Linux backend installs fixed-capacity, sender/path/
interface/member-specific system-bus matches for `NetworkManager.StateChanged`
and the service's `NameOwnerChanged`, reads the initial `State` only after both
matches are active, and re-reads the current owner and property for each signal.
That reconciliation prevents queued stale signals from restoring connectivity
and maps daemon loss to unavailable. Reconciliation uses a separate system-bus
connection so synchronous property replies cannot deadlock behind the bounded
signal queue. Only global connectivity is provider-available; repeated local/
site/portal/disconnected states coalesce into one unavailable condition. The
listener blocks only on its declared background-thread API and reports bus/
property, closed-stream, and unsupported-platform failures explicitly.

## Conformance

The deterministic suite proves:

- missing, unavailable, and oversized vault credentials fail before a provider
  session can become active;
- the explicit keyless Coinbase driver starts while an unavailable vault stays
  untouched, whereas credentialed drivers retain mandatory vault loading;
- subscription acknowledgements, heartbeats, and market-trade messages advance
  one connection-wide sequence without hiding a gap;
- its bounded callback queue latches overflow after queued work drains, trade
  diagnostics redact payloads, stale stops cannot cancel an active generation,
  a confirmed stop permits the next generation, and undrained callbacks from
  the stopped generation cannot precede its replacement;
- the composed market worker accepts establishment before returning a Coinbase
  trade and fences both by the same provider generation;
- provider start failures expose only a coarse recovery class;
- network loss and suspend stop the active generation, while restoration and
  resume create strictly newer generations;
- delayed establishment and publication callbacks from fenced generations are
  rejected and counted;
- idle suspend/resume and network-loss/restoration cycles never initiate a
  provider session;
- direct connection calls cannot bypass an active suspend or network-loss fence;
- dropping an active owner attempts to stop its current provider generation;
- a full semantic queue rejects the candidate publication, preserves the exact
  prior shared `Arc`, requests provider shutdown, and latches recovery;
- an unconfirmed provider stop blocks replacement generations until a later
  cleanup attempt confirms shutdown, without losing power or network changes
  that arrive while cleanup is pending;
- suspend, resume, network loss, and restoration recover in either interleaving
  order without requiring a duplicate OS notification;
- the runtime is statically non-`Send`, so its declared worker ownership and
  teardown cannot migrate to another thread;
- event `Debug` output redacts the complete market publication and subscription;
- lifecycle fencing retires real history handoffs, permits the replacement
  generation to reuse their identities, and rejects delayed live and snapshot
  callbacks before history mutation;
- the composed handoff registry enforces the configured bound before starting
  additional history state;
- semantic-queue overflow retires the active generation's history handoffs
  before retry, so their identities and pins cannot strand recovery;
- composed history errors reduce decoder, storage, and continuity details to
  coarse diagnostic classes;
- authenticated local hydration succeeds while both provider and Axiusflow
  control-plane connectivity are unavailable;
- a real Coinbase history adapter page passes through the bounded scheduler and
  into the composed worker only after exact provider/account/entitlement,
  instrument, dataset, resolution, and range matching plus worker-owned
  non-reusable scheduler binding to the complete immutable segment revision and
  consumes only the scheduler's read-only validated completion; stale
  generations, unfinished pagination, missing empty-page cutover evidence,
  decoded-memory overflow, and gapped snapshots fail before publication, and
  the capacity preflight does not evict cache entries. A live gap can bind new
  recovery work without restarting or discarding the active handoff floor;
- the Linux native listener constructs the exact state and owner-change match
  rules, applies fail-closed state mapping and duplicate coalescing, and shares
  the lifecycle event type; a live host probe opened the listener and read
  global availability. Signal-driven `next_event` and daemon-restart integration
  remain explicitly unproven by this deterministic suite.

## Claim boundary

This slice does not implement Rithmic, CQG, FYERS, or a credentialed streaming
provider adapter. Coinbase now supplies bounded direct public WebSocket and
one-minute history connections; deterministic suites compose its streaming
lifecycle and validated scheduler completion with the worker. No trade-to-bar,
history-seeding, or application event loop drives those boundaries. The
lifecycle owner and native monitor remain disconnected from `apps/desktop`;
Windows and macOS network-change backends are unimplemented.
It also does not prove shipping-topology cloud absence, provider certification,
cross-platform recovery, GPUI responsiveness, or performance. `S2-19` therefore
remains partial; `S2-20` is not started and `S2-21` remains partial.
