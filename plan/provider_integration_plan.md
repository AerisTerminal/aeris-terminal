# Provider integration and cleanup plan

Tracked plan for the tastytrade integration, the provider abstraction, and the cleanup that
follows. Any session or agent can continue from here.

**How to use this file**

- `[x]` done and committed · `[~]` in progress / uncommitted · `[ ]` not started
- Read `AGENTS.md` first. Its five principles, architecture invariants, verification rules and
  rule 7 (edit with native file tools) are acceptance criteria for every item. `tools/naming_check`
  is authoritative where stricter. Read `CSS.md` before UI work.
- Update the checkboxes in the same commit as the work they describe. Never tick an item that
  is not committed and verified; mark maintainer-only items as such instead.
- One local commit per phase per repo, `type(scope): outcome`. Do not push; the reviewer
  reviews each commit first.
- Never weaken a test or a `naming_check` rule to go green. Credentials, tokens, capability bytes
  and private payloads never enter source, logs, fixtures, commits or reports.

Repositories:

- Native: `C:\Users\devraj\Downloads\Softwares\aeris-terminal`
- Website + AWS broker: `C:\Users\devraj\Downloads\Softwares\aeris-website`
  (`infra/backend/tastytrade/`, `infra/lib/broker_stack.ts`)

Last status review: 2026-10-01, native `main` at `7071595` plus uncommitted Phase 2 work.

## Decisions

- Phase order: 1 (finish) → 2 → 3 → 4 → 5 → 6.
- tastytrade: futures first; keep existing equity search working.
- Older tastytrade history: accept DXLink's documented `fromTime`-only limit. When no older
  candles exist, mark backwards history exhausted (bounded, no retry loop, no error state).
- Futures currency: never derive or default it; leave it unset when tastytrade omits it.
- Pre-change timing baseline is withdrawn; use the reference timings below.
- Pruning unused `third_party/gpui_base` components is approved.
- `naming_check`: brittle assertions may be replaced with equal-or-stricter checks; no rule is
  deleted, removal candidates are listed for the maintainer.

## Provider facts (verified)

- tastytrade API data is Level 1 only; no `Order`/depth through the API. tastytrade may offer paid
  depth and longer-history tiers later.
- tastytrade trusted third-party authorization is approved, so any tastytrade customer can
  authorize Aeris. Still confirm once with a second account.
- tastytrade confirmed `TimeAndSale` carries aggressor side.
- Measured live, `/ESZ26:XCME`, US cash hours (reviewer probe, release build):
  - `TimeAndSale`: every print, 65–80/s.
  - `Candle` (1m) live updates: about 2/s, interval p50 ~0.5 s, p99 up to 1.7 s.
  - `Quote`: about 2/s, p50 ~0.5 s, p99 up to 1.7 s.
  - Requesting `Quote`/`Candle` on `contract: STREAM` does not change those rates; the throttling
    is on the provider's side for the API entitlement.
  - Exchange to local receive: ~0.2 s typical (the local clock was ~0.5 s off), with occasional
    provider-side spikes.

Reference timings (reviewer, commit `e063ba5`, debug build, market closed): cold first search
9.2 s, cached futures search 340 ms, remote equity search 348 ms, cold selection → first candles
6.9 s, timeframe switch 1.7 s.

---

## Phase 1 — tastytrade performance and correctness

Commits: native `d5e20e7`, `7071595`; website `de3dfb1` (deployed and pushed).

### Done

- [x] Broker off the hot path: `access_token` route; instrument and quote-token proxy routes removed
  (deployed to AWS and pushed).
- [x] Desktop calls `api.tastyworks.com` directly with the required `User-Agent`.
- [x] Access token refreshed lazily (only when a REST call needs it and it is expired or within 60 s
  of expiry); no periodic refresh timer.
- [x] 3 s client-side request throttle removed.
- [x] Runtime-owned futures catalog served locally (instant futures search and default listing).
- [x] Warm DXLink session with the quote token cached until shortly before its expiry.
- [x] Candles first: history completes on the candle snapshot; tick-tape backfill is separate and only
  runs when trades are demanded.
- [x] History concurrency raised from 2 to 4.
- [x] `TimeAndSale` on a dedicated `contract: STREAM` channel.
- [x] 8-minute live replay removed (live subscriptions start at connect time).
- [x] Futures contract metadata populated (point value and dates); currency left unset when omitted.
- [x] Forming vs closed last candle decided from the instrument's market session.
- [x] Older history exhaustion when DXLink has no older candles.
- [x] Per-channel failures isolated (`ChannelFailure`) instead of tearing down the session.
- [x] Bounded decode-failure budget instead of failing on one malformed row.
- [x] Quote side times recorded as provider time, never as exchange time.

### Remaining

- [x] **Urgent: drive the tastytrade live candle from trades.** The chart's forming candle currently
  advances only on provider `Candle` events (~2/s), so it trails the tape by 0.5–1.7 s.
  - [x] Each accepted trade (NEW, valid tick, not a spread leg) updates close/high/low/volume of
    the forming bar immediately and rolls to a new bar at the period boundary by exchange time.
  - [x] Provider `Candle` events become authoritative reconciliation: completed bars are replaced
    by the provider candle; a lagging `Candle` event never moves the forming bar backwards
    relative to trades already applied. Reconcile deterministically (candle `count`, trade
    identity/timestamps); corrections and cancels still reach the bar. Document the rule in code
    and cover it with tests.
  - [x] Session gaps stay allowed; no double counting across the history/live handoff boundary.
  - [x] Reuse the trade-built handoff (the Rithmic model) through the Phase 2 neutral live-bar
    model; do not add another copy.
- [x] Top of book from trades: update best bid/ask prices from each `TimeAndSale`'s
  `bidPrice`/`askPrice`; take sizes from the latest `Quote`; never present a stale size as fresh.
- [x] Equity search: runtime debounce, newest-wins cancellation of superseded searches, and a
  bounded cache of recent results.
- [x] Decode-failure budget: either make it truly per connection or rename it; it currently resets
  on a 60 s rolling window but is labelled per connection.
- [~] Release-build timings for: menu open → futures results, equity search, cold and warm
  selection → first candles, timeframe switch, reconnect after idle stop, chart close lag behind
  the tape, quote price update rate (p50/p90/p99 and measurement method), against the references above.
  - [~] Release probe (2026-10-02, `cargo run -p aeris_market_runtime --release --locked --example tastytrade_market -- /ES`): cold search 7545 ms, cached futures search 0 ms, equity search 1513 ms, selection to first candles 4579 ms, timeframe switch 839 ms. The session reached live state but the market was closed (`updates=0`, `ticks=0`), so reconnect, tape-lag and quote-rate percentiles remain unmeasured.
- [x] Tests: deterministic coverage for the trade-driven candle, reconciliation, top-of-book from
  trades, search cancellation and caching (tastytrade currently has 25 tests).

### Maintainer-only

- [ ] Visual check during US cash hours: tastytrade chart, tape and footprint keep up with the market.
- [ ] Confirm third-party authorization works by connecting a second tastytrade account.

---

## Phase 2 — Provider abstraction in `market_runtime` (behavior-preserving, three providers)

Status: complete.

### Done

- [x] Neutral worker contract: `ProviderControl`, `ProviderDemand`, provider events (`provider_event.rs`).
- [x] One live map `SeriesLive` with `CandleGapPolicy` (Hyperliquid `Contiguous`, tastytrade
  `SessionGapsAllowed`).
- [x] `ProviderDescriptor` registry for Rithmic, Hyperliquid and tastytrade.
- [x] Fixed wake arrays and magic drain lanes removed; sized from the registry.
- [x] Named per-provider coordinator fields removed.
- [x] Provider-name branches removed from `coordinator.rs`, `realtime.rs`, `publication.rs`,
  `instrument_selection.rs` and `mod.rs`.

### Remaining

- [x] Remove the remaining provider-name branches in `market_service/runtime.rs` (history-source
  checks, reconnect-delay and catalog-publisher wiring) and `market_service/history.rs`; anything
  provider-specific belongs in the descriptor or the provider's own module.
- [x] Move coordinator logic still living in `market_service/tastytrade.rs` behind the neutral
  contract; that module keeps only the tastytrade worker, history source and descriptor.
- [x] Move `merge_live_candle` and its tests from the Hyperliquid adapter into `market_runtime`,
  if not already done.
- [x] Test proving a fourth descriptor registers with no coordinator changes.
- [x] Existing coordinator/realtime/history tests pass, ported without weakening.
- [x] Update `naming_check` assertions that named removed functions/types to equal-or-stricter
  structural checks (e.g. one live map; no `provider_id ==` literals outside descriptor and
  provider modules).
- [x] Gates: check/test/clippy (`--all-targets --all-features -- -D warnings`) for
  `aeris_market_runtime`, `aeris_hyperliquid_market_adapter`, `aeris_rithmic_protocol_adapter`,
  `aeris_tastytrade_market_adapter`, `aeris_platform_runtime`, `aeris_desktop` and `naming_check`.
- [x] Ignored live Hyperliquid tests that need no credentials.
- [x] Commit.

Constraints: every bound, overflow/cancellation behavior, generation fence, stop fence, idle-stop
overlap recovery, catalog overflow rejection and history retry policy behaves exactly as before.
`MarketService`'s public API and `MarketRuntimeEvent` stay unchanged unless unavoidable.

Maintainer-only: Rithmic test smoke (needs credentials).

---

## Phase 3 — Provider-neutral desktop

- [x] Header **Accounts** panel (left of the time zone, secondary surface) replaces the
  command-palette broker commands and the order-ticket practice-account dialogs: tastytrade
  connect/disconnect with runtime status (`MarketService::provider_connected`), Hyperliquid
  public feed, practice account create/select/delete. The order ticket only shows the active
  account and opens the panel. When descriptors land, list providers from them instead of
  hardcoding tastytrade and Hyperliquid in `components/accounts_panel.rs`.
- [x] `market_runtime` publishes provider presentation descriptors through a provider-neutral
  contract: id, display name, chart intervals, default/empty-query listing, search hint, logo key,
  depth capability, connection kind (credentials, hosted broker, public).
- [ ] Replace `TerminalProvider` branches in `apps/desktop/src/desktop.rs`,
  `desktop/workspace_surface.rs`, `components/{symbol_menu,terminal_view,watchlist_panel,time_sales_panel}.rs`,
  `engine_market_worker*.rs`, `market_worker.rs` and `desktop/local_state.rs` with descriptors.
- [ ] Per-provider error strings become templates using the display name; interval tables come
  from descriptors.
- [ ] Investigate `RITHMIC_ENTITLEMENT_ID = "crypto_public_realtime"` in `desktop.rs`: trace it into
  the Rithmic adapter; fix with a regression test if stale, rename if meaningful.
- [ ] Providers without depth show an explicit "depth not available from this provider" state
  instead of an empty ladder (`CSS.md` tokens; text over depth uses `--book-*-text`).
- [ ] Durable local state keeps loading existing Rithmic, Hyperliquid and tastytrade selections.
- [ ] tastytrade logo and attribution ("Market data provided by tastytrade") per tastytrade's
  brand guidelines, once the assets are received.
- [ ] Gates and commit.

Maintainer-only: visual check of the symbol menu, depth-unavailable state and attribution.

## Phase 4 — Split large files (mechanical, no logic changes)

- [ ] `crates/market_runtime/src/study.rs` (7.7k lines)
- [ ] `apps/desktop/src/desktop/workspace_surface.rs` (6k)
- [ ] `crates/account_runtime/src/account_service/mod.rs` (5.7k)
- [ ] `apps/desktop/src/desktop.rs` (5k)
- [ ] `crates/trading_runtime/src/lib.rs` (4.6k)
- [ ] `crates/market_runtime/src/market_service/tastytrade.rs` and anything still oversized in
  `market_service/{coordinator,realtime}.rs` after Phase 2
- [ ] Update path-based `naming_check` assertions to the new locations with equal strictness.
- [ ] Gates and commit.

Pure moves plus visibility changes; tests move with their code.

## Phase 5 — Prune `third_party/gpui_base`

- [ ] Map the transitive internal dependencies of what the desktop uses: `gpui_base::init`,
  `Theme`/`ColorTokens`/`RadiusTokens`/`ThemeAppearance`, `Button`, `Switch`/`SwitchThumb`/`SwitchTrack`,
  `input::{Input, InputState, InputEvent, InputEditorStyle}`.
- [ ] Remove every unused module (dock, calendar, date_picker, color_picker, table, tree, sheet, …)
  and unused dependencies.
- [ ] Keep `LICENSE-APACHE` and the `upstream-revision` metadata; document the retained set in the
  crate's `lib.rs` doc.
- [ ] Desktop builds and tests pass; report line counts before/after.
- [ ] Commit.

Maintainer-only: visual check that inputs, buttons and switches behave identically.

## Phase 6 — `naming_check` review

- [ ] Keep every ownership, dependency-direction, boundedness and security rule.
- [ ] Replace brittle function-name/string-presence assertions with equal-or-stricter structural or
  behavioral checks.
- [ ] List removal candidates (rules whose original reason appears gone) with evidence; delete nothing.
- [ ] Commit.

---

## Final gates (after Phase 6)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
- [ ] Website `infra` tests (if the website changed)

Report pre-existing failures precisely; fix every failure the work caused.

## Report expected at each phase end

Commit hash per repo, what changed, tests added, gates and results, deployments, blocked and
maintainer-only items, measured timings with method, every `naming_check` change (old → new and
why it is not weaker), removal candidates, line counts before/after for `market_runtime`,
`apps/desktop` and `third_party/gpui_base`, and any `AGENTS.md` principle not fully satisfied.

## Product follow-ups (not part of this plan's phases)

- Rithmic production: production endpoints, system/gateway picker from Rithmic's system list,
  replacing the hardcoded Rithmic Test endpoint; confirm per-system enablement with Rithmic.
- Read-only tastytrade account features (positions, live PnL, journal, portfolio) with the
  existing `read` scope, owned by `trading_runtime`.
- tastytrade order execution only after traction: `trade` scope, sandbox testing, legal review.
