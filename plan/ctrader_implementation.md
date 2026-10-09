# cTrader implementation

This file is the single status record and checklist for the cTrader Open API integration (batch T9
in [`longterm_plan/trading_platform.md`](longterm_plan/trading_platform.md), foundations PF10 and PF11, decisions D7–D9). It
tracks what is built and what is left.

A step is checked only when it works through the real runtime and desktop path. A passing unit test
alone does not check a step.

Last examined: 2026-10-09.

---

## 1. Summary

| Layer | Owner | Status |
| --- | --- | --- |
| AWS broker for cTrader OAuth and app credentials | `aeris-website` `infra/backend/broker_oauth/` | **Deployed**, working |
| Maintainer cTrader authorization | AWS connection table and native vault | **Done**: one connection `ready` |
| Protocol adapter: TLS, framing, auth, heartbeat, limiter | `crates/adapters/ctrader_open_api` | **Done**, live-verified on demo |
| Market data in `market_runtime` | `market_service/ctrader.rs` | **Done**; M-1 to M-9 fixed, second broker not yet run |
| Desktop market data (pick cTrader, chart, DOM) | `apps/desktop` | **Built and tested**; manual desktop check pending |
| Adapter trading messages | `crates/adapters/ctrader_open_api` | **Done**, qualified on demo |
| Live venue in `trading_runtime` (PF11) | `crates/trading_runtime` | **Built**; T-1 to T-6 fixed; brackets server-side; broker unrealized P&L shown and in loss rules |
| Session owner joining the adapter and `trading_runtime` | `market_runtime` relay (D8) | **Built**, verified on demo |
| Desktop trading (accounts, DOM, chart orders) | `apps/desktop` | **Built**, positions tab included; manual check pending |
| AWS broker hardening (Phase 9) | `aeris-website` | Uncommitted work in progress there, not by this plan's agent |
| Demo and live qualification | maintainer | **Not started** |

In short: market data works from cTrader through `market_runtime` into the desktop (Accounts,
symbol menu, chart, DOM), pending a manual check in the running app. Demo trading runs end to end:
the desktop attaches the trading owner's venue to the cTrader supervisor, which relays orders over
the shared demo session; the relay is verified on demo, and the desktop trading path awaits the
manual check and Phase 7 qualification.

## 2. Verified evidence

Recorded 2026-10-08 against the maintainer's cTrader demo account. Account ids are masked.

- **AWS stack** `AerisBrokerOAuthService` (us-east-1) is `UPDATE_COMPLETE`. `GET /health` on
  `https://app.aeristerminal.com` returns 200. The stack serves these cTrader routes: `POST start`,
  `GET callback`, `POST status`, `POST access_token`, `POST app_credentials` and `POST disconnect`.
- **App credentials** are an SSM SecureString at `/aeris/providers/ctrader/oauth_client`, set through
  `npm run configure:ctrader` with hidden input.
- **Connection table** holds one cTrader connection in phase `ready`, valid for 90 days and extended on
  every server refresh.
- **`ctrader_probe`:** application auth and account list succeeded. It found 1 demo account (830
  symbols) and 0 live accounts.
- **`ctrader_market`**, run through `MarketService` against EURUSD:

  | Check | Result |
  | --- | --- |
  | History | 1600 bars each for M1, H1 and D1, all contiguous |
  | Live forming candle | Seen |
  | Bid/ask quote | Seen, bid below ask |
  | Depth | Both sides |
  | Crossed updates withheld | 0–5 spot, 0–2 depth per run |
  | Total time | About 8.5–9.4 s |

  Rerun on the same demo broker after the M-1 to M-9 fixes: the run **exits 0**, D1 is contiguous
  across 325 session gaps, and shutdown no longer logs a failed unsubscribe. A second broker has
  not been run yet.
- **Workspace gates** were green on the examined tree: `cargo fmt`, `cargo clippy --workspace
  --all-targets --all-features -D warnings` and `cargo test --workspace --all-features`.

Rerun the evidence (close the desktop first so only one runtime holds sessions):

```text
cargo run -p aeris_market_runtime --example broker_authorization -- ctrader connect
cargo run -p aeris_market_runtime --example ctrader_probe
cargo run -p aeris_market_runtime --example ctrader_market
```

Evidence files are written to `.cache/evidence/`, which Git ignores.

## 3. What is built

### 3.1 AWS broker (`aeris-website`)

- [x] **OAuth sign-in:** the start route makes a desktop proof challenge (SHA-256 of a 32-byte proof).
  The pending state lasts 10 minutes, and the callback state is consumed once.
- [x] **Code exchange:** the code is exchanged at `openapi.ctrader.com/apps/token`. Tokens are stored
  with KMS envelope encryption, using a new data key per encryption and the connection id as AAD.
- [x] **Server-side refresh:** inside a 7-day window, the server refreshes under a conditional lock.
  `force_refresh` handles a token the desktop has seen invalidated. The 90-day connection lifetime
  restarts on each refresh.
- [x] **Rate limits:**
  - 50 starts per hour for cTrader, counted separately from tastytrade.
  - Two shared work leases.
  - API stage throttle of 2 per second, burst 5.
- [x] **`app_credentials`** returns the client id and secret only to a caller holding a `ready`
  connection and its proof. This is the implemented form of D7 (see section 5).
- [x] **Logging:** responses carry `no-store`, errors return fixed strings, and nothing logs secrets.
  API access logs are disabled on purpose.
- [x] **Tests:** `ctrader.test.mjs` covers exchange, refresh rotation, the lock policy, validation,
  provider isolation and leak checks.

### 3.2 Protocol adapter (`crates/adapters/ctrader_open_api`)

- [x] **Pinned protos:** Spotware `openapi-proto-messages` at `3fd8bdd`, in
  `third_party/ctrader_open_api/SOURCE`, compiled by `build.rs`.
- [x] **Transport:** TLS through rustls with webpki roots, on port 5035 at the `demo.` and `live.`
  `ctraderapi.com` hosts.
  - Frames are a 4-byte length and a `ProtoMessage`. Inbound frames are capped at 4 MiB, outbound
    requests at 256 KiB.
  - Proto2 required fields are validated on the wire.
- [x] **Timeouts:** connect, TLS, write and read are all bounded. The session is dead after 30 s of
  silence. A heartbeat goes out after 8 s with no writes.
- [x] **Queues:** outbound holds 256, inbound 16, and there are at most 256 pending requests.
- [x] **Request limiter:** about 41 general requests per second (documented limit 50) and about 3.6
  historical per second (documented limit 5). Historical requests also count against the general limit.
- [x] **Authentication:** application auth, account list by access token, and lazy account auth with
  checks on the ctid echo and the live/demo flag.
- [x] **Credential recovery:**
  - Application credentials are fetched again once on `CH_CLIENT_AUTH_FAILURE`.
  - On token invalidation, the session force-refreshes once and re-authorizes every account.
  - After an account disconnect, the account is re-authorized once.
- [x] **Error mapping:** rate-limit errors carry their `retryAfter`, connection limit and maintenance
  carry their own waits, and token errors become `NeedsReconnect`.
- [x] **Hosted client:** talks to the AWS broker. Only `connection_id` and `proof` go in the native
  vault (`aeris.provider.ctrader` / `broker_connection`).
  - The access token is cached and refreshed 24 h before expiry.
  - App credentials zeroize on drop and are redacted in `Debug`.
- [x] **Market messages:**
  - Symbols list and symbol by id
  - Spot subscribe and unsubscribe
  - Live trendbar subscribe and unsubscribe
  - Depth subscribe and unsubscribe
  - Spot and depth events
  - Trendbar history
  - Tick data (implemented; the runtime does not use it)
- [x] **Fixed-point prices:** wire 1/100000 values are rescaled exactly to the symbol's `digits`.
  Values that cannot be represented exactly are rejected, never rounded.
- [x] **Quantity scales:** depth sizes are in cents (scale 2), and trendbar volume is a tick count
  (scale 0). Spot quotes have no size; the quote quantity is `None`, not a placeholder value.
- [x] **Demo guard:** `DemoAccount` only accepts an account observed on the demo host with `!is_live`.
- [x] **Tests:** codec, session (loopback TLS server), market decoders and the hosted cache, with
  sanitized fixtures (placeholder ctid).

### 3.3 Market data (`crates/market_runtime/src/market_service/ctrader.rs`)

- [x] **Descriptor:**
  - Connection kind is hosted broker, with entitlement `ctrader-authorized`.
  - Depth is available. There is no trade tape (`trades_available: false`), so footprint, CVD and
    time and sales are declared unavailable instead of being invented from quotes.
- [x] **One supervisor thread:** at most one authenticated session per host. Changing demand
  (symbol, timeframe or chart) diffs the subscriptions and never tears down a healthy session. An idle
  host closes after 30 s.
- [x] **Per-account catalog:** search covers up to 16 accounts and returns up to 100 results. Symbol
  lists are cached per account for 30 minutes. Selection is validated against the current search.
- [x] **Instrument ids:** `ctrader:{demo|live}:{ctid}:{symbolId}`.
- [x] **On-demand trendbar history:** at most 8 pages, with an older probe for exhaustion, the
  forming bar split out, and one history task at a time.
- [x] **Live forming candle:** driven by the live trendbars inside spot events.
- [x] **Depth book:** quotes are added, replaced and deleted by quote id, up to 1024 per book. The
  book resubscribes on lost continuity, and crossed books are withheld and counted.
- [x] **Generation fencing:** a shared generation and epoch, so a retired generation's history or
  events cannot change current state.
- [x] **Authorization changes:** close hosts, cancel history, clear caches and pause the provider.
- [x] **Shared quote model:** `QuoteLevel` with an optional quantity, so a provider without quote size
  is represented honestly. The DOM shows an empty size, not a fake one.
- [x] **Tests:** `ctrader_tests.rs` covers the descriptor, demand, ids, period mapping, history
  paging, fencing, session reuse, idle close, catalogs, live candles and bounds.

### 3.4 Trading skeleton (`crates/trading_runtime`, `crates/domain/trading`)

- [x] **Account shapes:** `TradingAccount` accepts `ctrader` with `Demo` or `Live` and a
  `broker_ref`. `BrokerPosition` models a hedged position with SL/TP, swap and commission.
- [x] **Store schema v18:** columns `venue_id` and `broker_ref`, plus tables `broker_orders`,
  `broker_deals`, `broker_positions` and `broker_account_state`.
- [x] **Routing:** `VenueRoute` sends `ctrader/Demo` to `CtraderDemo`. `ctrader/Live` is refused
  ("data-only; trading is disabled") on every command.
- [x] **Venue boundary:**
  - Outbound requests (Place, Modify, Cancel) go through a queue of 64.
  - Events come back through `VenueInbox`, which holds 1024 and blocks the reader instead of dropping
    events. After 20 s stalled it asks for a reconnect and reconcile.
  - Client order ids are idempotent, up to 50 characters.
- [x] **Order state machine:** handles Accepted, Replaced, Cancelled, CancelRejected, Expired and
  Rejected. Events from a stale generation are dropped.
- [x] **Tests:** `venue/scripted.rs` and `tests/foundation.rs`.

## 4. Known defects in existing code

Fix these before the code paths they affect are reachable. The trading defects cannot be reached
from the shipped desktop today, because no cTrader account is registered and no venue is attached.

### Market data

All fixed in `7bf8f1c0` with regression tests in `ctrader_tests.rs`. The desktop maps chart `1D`
to `BarPeriod::Session{days:1}` for cTrader (M-1). M-5 needed a second fix: the transport's
`receive` reported `Closed` (shown as `Reconnect`) when a shutdown cancelled an in-flight
unsubscribe; it now reports `Cancelled`, covered by
`cancellation_during_a_request_reports_cancelled_not_a_broken_session`.

- [x] **M-1 D1 bar time.**
  - **Cause:** cTrader daily bars open at 17:00 New York time, which is 21:00 UTC in summer and
    22:00 UTC in winter. The runtime maps chart `1D` (`Time{86400}`) to D1 with a manual override in
    `trendbar_period`, so bars that are 23 h or 25 h apart are labelled as a fixed 24 h period. The
    comment "anchored at 21:00 UTC" is only true in US daylight time.
  - **Effects:** the forming-bar check uses a nominal 24 h, and study alignment uses `open + 86400`.
  - **Fix:** model cTrader D1 as `BarPeriod::Session{days:1}` end to end, or have the check accept the
    17:00 New York open. Add a fixture test that crosses a daylight-saving change, and make the
    `ctrader_market` check pass.
- [x] **M-2 Account id in logs.** cTrader instrument ids embed the trading account id, and generic
  `diagnostic!` lines in `market_service/history.rs`, `realtime.rs` and `ctrader.rs` print the raw id
  to stderr, as do the `parse_instrument_id` error messages. Redact the id in those lines without
  changing the id format, which routing depends on.
- [x] **M-3 Permanent pause after transient faults.** The recovery `failures` counter resets only on
  an authorization change. Five transient faults across the whole process lifetime pause cTrader until
  the user reconnects. Reset the counter after a healthy session.
- [x] **M-4 Wait hints ignored.** `ConnectionLimit` (300 s) and `Maintenance` carry server waits, but
  the runtime treats them as generic host failures. Honor them.
- [x] **M-5 Misleading shutdown log.** Shutdown logs "unsubscribe failed; closing the session:
  Reconnect". Skip the unsubscribe when stopping, or report it as cancelled.
- [x] **M-6 One account fails the whole search.** A single failing account aborts the search across
  all accounts. The catalog also opens the demo host even when every account is live. Isolate
  failures per account.
- [x] **M-7 Price step is a placeholder.** `price_increment: Some(1)` is the smallest representable
  step, not the symbol's tick. Derive the tick from `pipPosition` and `digits` before trading uses it.
- [x] **M-8 Unused adapter backoff.** `ReconnectBackoff` in the adapter is used only by tests, because
  the runtime has its own policy. Delete it or use it; there should be one policy.
- [x] **M-9 Long catalog fetches stall events.** Symbol-list fetches run on the same thread that polls
  events, so live updates stall while a large catalog loads. Bound or interleave the fetches.

### Trading

All six are fixed in the Phase 4 venue batch, with tests in `venue/scripted.rs` and
`tests/foundation.rs`.

- [x] **T-1 Risk checks skipped.** `place_broker_order` returns before `evaluate_order_risk`, so a
  demo order skips every M3.2 check: lock, plan, discipline, limits, currency and increment.
- [x] **T-2 Global commands blocked.** `preflight_accounts` errors whenever a demo account is in
  scope, so one registered demo account breaks the global kill switch, cancel-all and flatten-all for
  **every** account, simulated ones included.
- [x] **T-3 Simulator fills broker orders.** `market_fill_candidates` and
  `cancel_unfilled_immediate_orders` do not check the route, so the simulator can fill or cancel
  cTrader orders.
- [x] **T-4 Wrong order matched.** Venue events match orders by client order id without checking that
  the order belongs to a cTrader account.
- [x] **T-5 Pending state overridden.** `Accepted` and `Cancelled` override `PendingModify` and
  `PendingCancel`. After a reconnect, a `Replaced` event keeps the old prices, because the pending
  modification is held only in memory.
- [x] **T-6 Generation not persisted.** The venue generation resets to 0 on start, and orders left in a
  pending state survive on disk with nothing to reconcile them.

### Desktop

- [x] **D-1 Duplicate Rithmic row.** Unknown provider ids no longer fall back to Rithmic:
  `known_terminal_provider` is the exact mapping, `terminal_provider_from_id` is removed, and a
  restored chart from an unmapped provider fails with an explicit message. Tests in
  `symbol_menu.rs` and `desktop/tests.rs` cover it.

## 5. Decisions

- [ ] **D7: client secret.** The implemented answer is that the AWS broker hands `{client_id,
  client_secret}` to an authorized desktop through `app_credentials`. The desktop sends it in
  `ProtoOAApplicationAuthReq` and keeps it only in zeroizing memory, never on disk or in logs. Two
  things remain:
  - Confirm with Spotware in writing that this is acceptable for a distributed desktop application.
  - Record the decision and the rotation path in the roadmap. Today a rotation needs a fresh
    `configure:ctrader`, and every desktop picks up the new value on its next
    `CH_CLIENT_AUTH_FAILURE` refetch. Test that path.
- [x] **D8: owner of the cTrader trading session.** Decided 2026-10-08 by the maintainer: one
  connection per host carries both market data and trading.
  - The `market_runtime` cTrader supervisor keeps sole ownership of the socket.
  - It relays trading through the bounded `VenueRequest` / `VenueInbox` boundary, using a shared
    venue contract so `market_runtime` does not depend on `trading_runtime`.
  - Order, fill, position and account state stays only in `trading_runtime`.

  Recorded in `AGENTS.md`; the roadmap entry is still to update.
- [x] **D9: hedging and netting.** Decided 2026-10-08 by the maintainer: both account types are
  allowed in the first release, each in its native shape. Hedged accounts show one position per
  broker `positionId`; netted accounts show one net position per symbol.

## 6. Remaining checklist

The phases are in dependency order. Run the focused checks for each step as you go, and the full
gates (section 7) at the end of each phase.

### Phase 1: close out market data

- [x] Fix M-1 through M-9.
- [x] Add a regression test for each fix.
- [ ] Make `ctrader_market` exit 0, rerun it on two brokers, and record the evidence summary here.
  Exits 0 on the maintainer's demo broker (section 2); a second broker needs a second demo
  account authorized by the maintainer.
- [x] Commit the market-data batch.

### Phase 2: cTrader in the desktop (data)

- [x] **Provider enum:** `TerminalProvider::Ctrader` with `TerminalProvider::ALL`. Restoring a chart
  from a provider the build does not expose shows an explicit error instead of another provider.
- [x] **Generalize the hosted-broker wiring:** `HostedBroker`, `HostedBrokerConnections` and one
  `run_broker_operation` path serve tastytrade and cTrader (connect, disconnect, refresh on
  overlay open, and the symbol-menu connect prompt for the menu's provider).
- [x] **Accounts panel:** one `broker_card` renders the tastytrade and cTrader cards.
- [x] **Market worker:** `series_key` accepts `ctrader` with entitlement `ctrader-authorized` and
  the `ctrader:` prefix. `snapshot_instrument` parses `ctrader:{demo|live}:{ctid}:{symbolId}` as
  foreign exchange; the quote currency comes from the installed instrument's contract terms
  (`ProtoOAAssetListReq`, cached per account like the symbol list).
- [x] **Startup:** `catalog_refresh_on_startup: true`, so restored charts re-read digits, tick and
  quote currency from the account's current catalog.
- [x] **Chart and DOM:** depth reaches the DOM through the shared `QuoteLevel` path. Providers
  without `trades_available` get no Footprint chart type or tape studies (CVD, delta, big trades)
  in menus, `set_chart_type` refuses Footprint with a message, a restored or retained chart is
  shown as candles without tape panes, and time and sales says the provider publishes no trades.
- [ ] **Logo:** the generic provider mark is shown, as for tastytrade, until Spotware's attribution
  rules are confirmed (Phase 10).
- [x] **Architecture check:** `every_built_in_provider_maps_to_a_desktop_provider_or_is_hidden` in
  `desktop/tests.rs` enumerates the runtime registry (`HIDDEN_RUNTIME_PROVIDERS` is empty), and
  `tools/naming_check` requires that test and forbids a fallback provider mapping.
- [x] **Desktop tests:** provider mapping and labels, the menu listing and connect prompt, broker
  operation results, worker series keys and identity parsing, and restoring a cTrader chart and an
  unknown-provider chart through the real surface constructor.
- [ ] **Manual check:** in the running desktop, connect from Accounts, search EURUSD, chart M1, H1 and
  D1, watch live updates, switch symbols and timeframes without a reconnect, restart and restore.

### Phase 3: trading protocol in the adapter

Check every field against the pinned `.proto` files and a demo response before code depends on it.

The encoders and decoders live in `crates/adapters/ctrader_open_api/src/trading/`. They are checked
against the pinned protos and were exercised on the maintainer's demo account on 2026-10-08 with
`cargo run -p aeris_market_runtime --example ctrader_trading_capture` (minimum-volume EURUSD
orders, all cancelled or closed by the run; reconcile is empty before and after).

- [x] **Order requests** (`TradingRequest`): `new_order` (2106: market, limit and stop; volume in
  cents; `clientOrderId` 1–50 printable ASCII bytes; absolute or relative SL/TP, relative only on
  market orders), `amend_order` (2109), `cancel_order` (2108), `close_position` (2111) and
  `amend_position_protection` (2110). Prices go out as the exact decimal double of the
  fixed-point value.
- [x] **Events:** `decode_execution_event` (2126), `decode_order_error_event` (2132) and
  `decode_trailing_stop` (2107). Required fields are checked at every nesting depth. Quoted prices
  must be exact at the symbol scale; VWAP prices (position price, close entry price, order
  execution price) are kept at the finest exact scale up to 10 digits, never rounded.
- [x] **State recovery:** `reconcile` (2124/2125), `order_list` (2175/2176), `deal_list`
  (2133/2134), `order_details` (2181/2182) and `position_deals` (2179/2180), with bounded pages
  and `hasMore`.
- [x] **Account:** `trader` (2121/2122); `decode_trader` also reads `ProtoOATraderUpdatedEvent`
  (2123). `ProtoOAMarginChangedEvent` (2141) is left for later.
- [x] **Reference data:** `ProtoOAAssetListReq` (2112), used for the quote currency.
- [x] **Session changes for trading:**
  - An order request is answered by 2126 or by 2132; a rejection no longer faults the session.
  - Later 2126/2132 frames for an answered request are queued as events instead of dropped.
  - A 2164 for another account, or an uncorrelated `ProtoOAErrorRes` naming another account, is
    queued for the event loop instead of failing the in-flight request. The request's account is
    read from field 2 of its payload.
- [x] **Safety:** every encoder that changes an order or position takes a `DemoAccount`; reads
  take any observed account id.
- [x] **Fixtures:** test fixtures carry the field shapes the demo server sent. Observed on demo:
  - Limit and stop orders: accepted, replaced on amend, cancelled; order details answer 2182.
  - A market order with time in force IOC is accepted and filled under the request's id.
  - Protection on a market order arrives as a separate server-created `STOP_LOSS_TAKE_PROFIT`
    closing order, sent without the request's id (`isServerEvent`), carrying the parent's client
    order id. Closing the position cancels it under the close request's id.
  - A close-position order takes the request's `clientMsgId` as its `clientOrderId`.
  - `ProtoOAPosition.price` is `0` before a fill and after a close; it decodes as no price.
  - `moneyDigits` is present on positions, deals, close details and the trader (2).
  - Amending only the stop loss removes the take profit.
  - A volume below the minimum is answered by 2132 `TRADING_BAD_VOLUME` under the request's id.
  - `position_deals` rejects a future end time with `INCORRECT_BOUNDARIES`.
  - Not yet observed: a partial fill (EURUSD minimum volume fills whole) and a trailing stop.

### Phase 4: provider-neutral live venue (PF11) in `trading_runtime`

The provider-neutral contract is `aeris_trading::venue` (D8): `trading_runtime` owns the state and
the bounded inbox, and a relay translates the contract to one broker's protocol.

- [x] **Extend `VenueRequest`:** `Place` (broker account, order type, SL/TP as price or distance,
  position id), `Amend`, `Cancel` by broker order id, `ClosePosition`, `AmendPositionProtection`
  and `Reconcile`, each validated before it leaves the owner.
- [x] **Extend `VenueUpdate`:** `Order` reports with the broker order id and filled quantity,
  `Refused`, `Fill` (deal id, price, quantity, position id, realized close), `Position`,
  `PositionClosed`, `Balance` and `Snapshot`.
- [x] **Persist state** (schema v19): the broker order id is bound in `broker_orders` on first
  report; deals are written once by deal id to `fills` and `broker_deals`; `broker_positions` and
  `broker_account_state` follow reports. A closing deal for an order Aeris never placed (server
  SL/TP, close request) creates a mirror order `ct-{broker id}`.
- [x] **Fix T-1 to T-6:**
  - `evaluate_order_risk` and the quantity-increment check run on the broker path. The practice
    rule that contract currency equal account currency is not applied: the broker converts P&L
    into the deposit currency.
  - Flatten, reverse, close position and amend protection send real requests.
  - The kill switch locks first, so it locks even with the venue disconnected, and reports broker
    orders it could not cancel.
  - No demo or live account blocks global kill, cancel or flatten for the others; flatten reports
    what it could not request in `FlattenOutcome::incomplete`.
  - The touch simulator and its IOC sweep skip broker orders.
- [x] **Recovery:**
  - The venue generation is persisted, and so are broker order id bindings.
  - Every attach queues a `Reconcile` for each demo broker account.
  - A snapshot replaces the account's positions, re-confirms orders still open (settling unanswered
    modify or cancel requests to the broker's state), and settles orders it no longer holds:
    filled from deals, cancelled when bound or partly filled, rejected when never received.
  - Replayed deals are recorded once; tests cover duplicate, lost and offline-filled orders.
- [x] **Brackets:** on a broker account a bracket is one entry order carrying server-side SL/TP
  distances from the template's ticks (`BracketPlacement::BrokerProtected`); nothing is managed
  locally. Stop risk is checked first. Scale-out targets, trailing stops and break-even are refused
  for broker accounts with their reasons.
- [ ] **Live risk:** risk checks run on broker orders, and the maximum-contract rule counts open
  broker positions (netted per instrument). Broker positions carry cTrader's own unrealized P&L
  (`ProtoOAGetPositionUnrealizedPnLReq` 2187, already in the deposit currency, so no conversion
  on our side; verified on demo 2026-10-09: gross -0.01, net -0.06 EUR on 1,000 EURUSD). The
  owner requests it every second while a connected account holds broker positions; a broker
  account's summary shows that open P&L and equity = balance + net unrealized, and neither
  while the account is unreachable. Decided 2026-10-09 by the maintainer: loss rules include it.
  A broker account's session P&L is the realized P&L of its deals since the profile's session
  start (baseline must be zero) plus cTrader's net unrealized; rules re-check on every refresh
  and realized deal. While the account is unreachable, orders are refused, the live check does
  not lock on a guess, and the risk meter is omitted. Session plans still read simulated
  positions only.
- [ ] **Round trips:** trade-history round trips cover simulated accounts only; design broker trade
  history (hedged positions per broker id) with Phase 6.

### Phase 5: venue worker and account registration

- [x] **D8 relay** (`market_service/ctrader_venue.rs`): the desktop attaches
  `TradingService::attach_demo_venue()` (the owner assigns and persists the generation) to
  `MarketService::attach_ctrader_venue`. The cTrader supervisor relays requests over the demo
  session's rate-limited request path and pushes translated events into the owner's inbox. It
  keeps the demo session open while a venue is attached, routes unsolicited execution events for
  served accounts, and after a session drop reconciles each served account with deals replayed
  from just before the drop. When the demo session cannot be opened (for example, cTrader is not
  connected) trading waits on its own backoff and refuses queued requests with the reason, without
  charging market-data recovery. Verified on demo with
  `cargo run -p aeris_market_runtime --example ctrader_venue_probe`: announce, reconcile snapshot,
  far limit accepted at 0.80000, cancel confirmed.
- [x] **Register accounts:** the relay announces each demo account (`AccountObserved`: name, deposit
  currency from `ProtoOATraderRes` and the asset list, money scale) and its balance; the owner
  registers it as `ctrader-demo-{ctid}` with `venue_id = "ctrader"` and `broker_ref` = the ctid,
  and reconciles a new account at once. Live accounts are not announced (data-only).
- [x] **Unobserved accounts:** the owner tracks which broker accounts the current venue generation
  announced (cleared on every attach, in memory because it is a per-session fact) and the snapshot
  exposes it; the Accounts panel shows other demo accounts as "not connected". The unused
  `connection_state` column stays unused.
- [x] **Register instruments:** charting a cTrader symbol already registers its `TradingInstrument`
  from the install (price scale = digits, quantity scale 2, tick from M-7, step volume and quote
  currency from Phase 2).
- [x] **Bounds:** requests per relay turn (16), the outbound queue (64), the blocking inbox (1024,
  reattach after a 20 s stall), the spec cache (1024), and at most 16 deal-list requests per
  replay: a window the broker reports as truncated is split in halves until each part fits.
- [x] **Day orders:** decided 2026-10-08 by the maintainer: broker pending orders requested as
  `Day` rest good-till-cancelled, and market orders carry immediate-or-cancel; the owner stores the
  time in force the broker holds. The relay still refuses `Day` at the contract level.

### Phase 6: desktop trading

- [x] **Accounts panel:** broker accounts are listed under "Broker accounts", apart from practice
  accounts, with Simulated, Demo and Live badges in token colours (`indigo` for demo, `warning`
  for live). They get no delete button, live accounts cannot be selected, and the order ticket
  shows the selected account's badge. Visual check pending.
- [ ] **Account selection:** the selected account feeds the DOM, chart trading and the order ticket.
  Simulated, demo and live accounts are always visually distinct.
- [ ] **Order entry:** DOM and chart market, limit and stop orders; modify by drag; cancel; brackets
  as server SL/TP; close position; amend SL/TP; flatten; reverse.
- [x] **Feedback:** flatten, reverse, flatten-all and chart close report fills, cTrader close
  requests sent (never as fills) and anything incomplete, which makes the feedback an error.
- [x] **Broker positions in the desktop:** the desktop holds `broker_positions` from the snapshot;
  a broker account's net exposure enables flatten and reverse, allows the chart close, and draws
  the DOM marker when one position makes it up and its entry is exact at the book scale. The
  marker carries no point value, so no unrealized P&L is shown that the broker has not reported.
- [ ] **Broker positions panel:** a POSITIONS tab in the bottom panel (maintainer's choice,
  2026-10-09) lists each broker position (hedged positions separately, newest first) with
  entry, SL, TP and cTrader's open P&L, filtered by the shared account filter; Close sends the
  close off the UI thread and is disabled while the account is unreachable. Visual check
  pending; SL/TP editing from the row is not built yet.
- [ ] **Kill switch and risk:** profiles apply to cTrader demo accounts exactly as to simulated ones.

### Phase 7: demo qualification

Qualify on two cTrader brokers, for example IC Markets and Pepperstone.

- [ ] DOM and chart trading, brackets, flatten, the kill switch, partial fills and rejections.
- [ ] Restart the desktop with working orders and open positions. After reconcile, state matches the
  broker, with no duplicate or lost orders.
- [ ] Drop the network mid-order. Reconnect, reconcile, and confirm no duplicate or lost orders.
- [ ] Token invalidation and refresh during a session.
- [ ] Rate limits under fast order entry: no 429 storms, and backpressure is visible.
- [ ] Record sanitized evidence here.

### Phase 8: live trading

- [ ] Get explicit maintainer approval to lift `LIVE_TRADING_DISABLED`, and add a per-account opt-in
  confirmation.
- [ ] Allow live-host encoders only for accounts observed `is_live` on the live host.
- [ ] Qualify live on a small maintainer account.

### Phase 9: AWS broker hardening

- [ ] **Alarms:** Lambda errors, throttles and duration; API 4xx, 5xx and 429; DynamoDB throttling;
  KMS and SSM failures.
- [ ] **Request logs:** one structured line per request with route, status, latency and provider, but
  no query string or body.
- [ ] **Health:** `/health` reports both providers and checks SSM, DynamoDB and KMS.
- [ ] **Disconnect:** revoke the token at cTrader if Spotware provides a revoke endpoint. Otherwise
  record that disconnect only deletes the row.
- [ ] **Missing tests:** cTrader disconnect, two concurrent refreshes competing for a rotating refresh
  token, and a refresh whose save fails after cTrader has rotated the token.
- [ ] **Avoid the double decrypt:** the lease-free `access_token` read currently decrypts twice.
- [ ] **Stale wording:** the KMS key description and the `infra` README still say tastytrade-only and
  30 days.
- [ ] **WAF:** decide on a WAF for the HTTP API before public release.
- [ ] **Secret migration:** move provider secrets to Secrets Manager with a rotation runbook (README
  Phase 2).

### Phase 10: release rules

- [ ] Follow Spotware's name, logo and attribution rules in the app and on the website.
- [ ] No forex marketing to Indian residents, and no US users (cTrader has no US brokers).
- [ ] Confirm the Open API application status and terms in the portal: commercial use, white-label
  builds, and whether brokers must enable the application separately.

## 7. Gates

Each phase ends with the full gates passing:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Changes to the AWS broker run its `npm test` in `aeris-website/infra/backend/broker_oauth` and
`infra` tests, then deploy through CDK from a clean, pushed `main`.

## 8. Acceptance

From the roadmap:

- A user authorizes a cTrader account from the Accounts panel, charts it, and trades a demo account
  from the DOM and the chart with the M3.2 checks enforced.
- Reconnects and restarts never duplicate or lose orders.
- The client secret is handled exactly as D7 records, and never appears in source, logs or fixtures.
