# S2-19 decision: bounded desktop provider lifecycle owner

**Status:** accepted as a partial runtime foundation; direct provider and desktop integration remain open

**Evidence command:** `tools/run_desktop_provider_runtime_conformance.sh`

## Decision

Use `crates/desktop_provider_runtime` as the provider-neutral, single-writer
lifecycle owner around a future direct desktop provider adapter. The owner runs
on one declared worker thread, loads opaque credentials from the existing
`CredentialVault` for every connection attempt, bounds their size, lends them
only to the provider start call, and zeroizes the returned vault buffer before
the attempt returns.

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

## Conformance

The deterministic suite proves:

- missing, unavailable, and oversized vault credentials fail before a provider
  session can become active;
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
- event `Debug` output redacts the complete market publication and subscription.
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
  control-plane connectivity are unavailable.

## Claim boundary

This slice does not implement Rithmic, CQG, FYERS, or another direct provider
adapter. It does not add a native OS network-change monitor, connect the
lifecycle owner to `apps/desktop`, or connect a provider history fetch adapter.
The composed worker proves provider-generation ownership of history callbacks,
but no native provider SDK or application event loop drives that boundary yet.
It also does not prove shipping-topology cloud absence, provider certification,
cross-platform recovery, GPUI responsiveness, or performance. `S2-19` therefore
remains partial; `S2-20` and `S2-21` remain not started.
