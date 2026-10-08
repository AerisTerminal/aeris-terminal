# cTrader implementation

This file is the single status record and checklist for the cTrader Open API integration (batch T9
in [`longterm_plan/trading_platform.md`](longterm_plan/trading_platform.md), foundations PF10 and PF11, decisions D7–D9). It
tracks what is built and what is left.

A step is checked only when it works through the real runtime and desktop path. A passing unit test
alone does not check a step.

Last examined: 2026-10-08.

---

## 1. Summary

| Layer | Owner | Status |
| --- | --- | --- |
| AWS broker for cTrader OAuth and app credentials | `aeris-website` `infra/backend/broker_oauth/` | **Deployed**, working |
| Maintainer cTrader authorization | AWS connection table and native vault | **Done**: one connection `ready` |
| Protocol adapter: TLS, framing, auth, heartbeat, limiter | `crates/adapters/ctrader_open_api` | **Done**, live-verified on demo |
| Market data in `market_runtime` | `market_service/ctrader.rs` | **Done**; M-1 to M-9 fixed, second broker not yet run |
| Desktop market data (pick cTrader, chart, DOM) | `apps/desktop` | **Built and tested**; manual desktop check pending |
| Adapter trading messages | `crates/adapters/ctrader_open_api` | **Not started** |
| Live venue in `trading_runtime` (PF11) | `crates/trading_runtime` | **Skeleton only**; safety defects in section 4 |
| Session owner joining the adapter and `trading_runtime` | to decide (D8) | **Not started** |
| Desktop trading (accounts, DOM, chart orders) | `apps/desktop` | **Not started** |
| Demo and live qualification | maintainer | **Not started** |

In short: market data works from cTrader through `market_runtime` into the desktop (Accounts,
symbol menu, chart, DOM), pending a manual check in the running app. Trading has a routing and
safety skeleton but cannot send an order.

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

- [ ] **T-1 Risk checks skipped.** `place_broker_order` returns before `evaluate_order_risk`, so a
  demo order skips every M3.2 check: lock, plan, discipline, limits, currency and increment.
- [ ] **T-2 Global commands blocked.** `preflight_accounts` errors whenever a demo account is in
  scope, so one registered demo account breaks the global kill switch, cancel-all and flatten-all for
  **every** account, simulated ones included.
- [ ] **T-3 Simulator fills broker orders.** `market_fill_candidates` and
  `cancel_unfilled_immediate_orders` do not check the route, so the simulator can fill or cancel
  cTrader orders.
- [ ] **T-4 Wrong order matched.** Venue events match orders by client order id without checking that
  the order belongs to a cTrader account.
- [ ] **T-5 Pending state overridden.** `Accepted` and `Cancelled` override `PendingModify` and
  `PendingCancel`. After a reconnect, a `Replaced` event keeps the old prices, because the pending
  modification is held only in memory.
- [ ] **T-6 Generation not persisted.** The venue generation resets to 0 on start, and orders left in a
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
- [ ] **D8: owner of the cTrader trading session.** cTrader limits connections, so one connection per
  host should carry both market data and trading. The recommendation:
  - The `market_runtime` cTrader supervisor keeps sole ownership of the socket.
  - It serves trading through the existing bounded `VenueRequest` / `VenueInbox` boundary.
  - Trading state stays only in `trading_runtime`.

  Decide this before step 6.1 (attaching the venue), update `AGENTS.md` and `tools/naming_check` if
  the ownership rules change, and record it here and in the roadmap.
- [ ] **D9: hedging and netting.** cTrader accounts can be hedged or netted. Decide how
  `trading_runtime` presents each (positions per id versus net), and which account types are allowed
  for the first release.

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

- [ ] **Order requests:**
  - `ProtoOANewOrderReq` (2106): market, limit and stop orders; volume in 0.01 units; `clientOrderId`
    up to 50 characters; relative and absolute SL/TP.
  - `ProtoOAAmendOrderReq` (2109)
  - `ProtoOACancelOrderReq` (2108)
  - `ProtoOAClosePositionReq` (2111)
  - `ProtoOAAmendPositionSLTPReq` (2110)
- [ ] **Events:** `ProtoOAExecutionEvent` (2126), `ProtoOAOrderErrorEvent` (2132) and
  `ProtoOATrailingSLChangedEvent` (2107).
- [ ] **State recovery:**
  - `ProtoOAReconcileReq/Res` (2124/2125)
  - `ProtoOAOrderListReq` (2175) and `ProtoOADealListReq` (2133)
  - `ProtoOAOrderDetailsReq` (2181) and `ProtoOADealListByPositionIdReq` (2179)
- [ ] **Account:** `ProtoOATraderReq/Res` (2121/2122) and `ProtoOATraderUpdatedEvent` (2123) for
  balance and account type. `ProtoOAMarginChangedEvent` (2141) is optional for the first release.
- [ ] **Reference data:** `ProtoOAAssetListReq` (2112) for deposit and quote currency conversion.
- [ ] **Session changes for trading:**
  - One request can produce several responses (order accepted, then filled, under one
    `clientMsgId`).
  - Unsolicited execution events must be routed by account instead of being dropped.
  - Do not charge an unsolicited `ProtoOAErrorRes` or account-disconnect event to an unrelated
    in-flight request.
- [ ] **Safety:** encoders accept only a `DemoAccount` until the live gate is lifted (Phase 8).
- [ ] **Fixtures:** sanitized fixtures from real demo responses for each message, including partial
  fills, rejections and SL/TP changes.

### Phase 4: provider-neutral live venue (PF11) in `trading_runtime`

- [ ] **Extend `VenueRequest`:** carry the account's `broker_ref`, SL/TP and order type. Add
  `ClosePosition` and `AmendPositionSltp`.
- [ ] **Extend `VenueUpdate`:**
  - the broker order id;
  - `PartiallyFilled` and `Filled`, with deal id, price, quantity and position id;
  - position opened, updated and closed;
  - balance updates;
  - a `Reconciled` snapshot of open orders and positions.
- [ ] **Persist state:**
  - the client-to-broker order mapping in `broker_orders`;
  - fills, written idempotently by deal id to `fills` and `broker_deals`;
  - `broker_positions` and `broker_account_state`.
- [ ] **Fix T-1 to T-6:**
  - Run `evaluate_order_risk` and the increment and currency checks on the broker path.
  - Replace the unconditional `DEMO_VENUE_UNAVAILABLE` in flatten, reverse, close and amend with real
    broker requests when a venue is attached.
  - The kill switch locks the account even with the venue disconnected.
  - A demo account never blocks global kill, cancel or flatten for other accounts.
  - Exclude broker orders from simulated fills.
- [ ] **Recovery:**
  - Persist the venue generation.
  - Reconcile at startup and after every attach.
  - Resolve every order left `Pending`, `PendingModify` or `PendingCancel` against the broker's
    snapshot.
  - Prove that no order is duplicated or lost.
- [ ] **Brackets:** use server-side SL/TP on cTrader orders and positions instead of local managed
  brackets, and label any part that stays local.
- [ ] **Live risk:** decide whether live risk evaluation (daily loss, trailing drawdown) uses broker
  fills and broker PnL for cTrader accounts.

### Phase 5: venue worker and account registration

- [ ] Implement D8: the cTrader supervisor attaches the demo venue
  (`TradingService::attach_demo_venue`) for each session generation. It sends outbound requests
  through the rate limiter and pushes decoded events into `demo_venue_inbox()`.
- [ ] **Register accounts:** after authorization, map each observed account to a `TradingAccount`:
  - `venue_id = "ctrader"`;
  - `broker_ref` = the ctid;
  - the environment from `is_live`;
  - currency and money scale from `ProtoOATraderRes`.

  Mark accounts that are no longer observed as disconnected instead of deleting them.
- [ ] **Register instruments:** register traded symbols as `TradingInstrument`s with exact price and
  volume scales, tick size (M-7), and lot, minimum and step volume.
- [ ] **Bounds:** the outbound queue, the inbox and reconcile work stay bounded, and an overload
  forces a reconnect and reconcile rather than silent loss.

### Phase 6: desktop trading

- [ ] **Accounts panel:** list broker accounts separately from practice accounts, with Simulated,
  Demo and Live badges in token colours. Broker accounts get no delete button, and the text is
  neutral instead of "Practice …".
- [ ] **Account selection:** the selected account feeds the DOM, chart trading and the order ticket.
  Simulated, demo and live accounts are always visually distinct.
- [ ] **Order entry:** DOM and chart market, limit and stop orders; modify by drag; cancel; brackets
  as server SL/TP; close position; amend SL/TP; flatten; reverse.
- [ ] **Feedback:** show rejections and `pending_close_requests` in flatten feedback. Never show a
  fill before the broker confirms it.
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
