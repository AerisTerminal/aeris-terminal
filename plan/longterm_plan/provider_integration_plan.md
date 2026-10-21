# Provider integration and cleanup plan

Tracked plan for the provider abstraction and the cleanup that follows. Any session or agent can continue from here.

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
- Website: `C:\Users\devraj\Downloads\Softwares\aeris-website`

Last status review: 2026-10-02, native `main` at `2b19077` with a clean worktree; the remaining
unchecked items require live-market hours or maintainer credentials.

## Decisions

- Phase order: 2 → 3 → 4 → 5 → 6.
- Pruning unused `third_party/gpui_base` components is approved.
- `naming_check`: brittle assertions may be replaced with equal-or-stricter checks; no rule is
  deleted, removal candidates are listed for the maintainer.

## Phase 1 — Retired

Phase 1 covered performance and correctness work for a broker integration that has since been
removed from Aeris. Its items are retired; the neutral live-bar and Level 1 top-of-book work it
motivated lives on in Phase 2 and Phase 3.

---

## Phase 2 — Provider abstraction in `market_runtime` (behavior-preserving)

Status: complete.

### Done

- [x] Neutral worker contract: `ProviderControl`, `ProviderDemand`, provider events (`provider_event.rs`).
- [x] One live map `SeriesLive` with `CandleGapPolicy` (Hyperliquid `Contiguous`; session-gapped
  providers `SessionGapsAllowed`).
- [x] `ProviderDescriptor` registry for every registered provider.
- [x] Fixed wake arrays and magic drain lanes removed; sized from the registry.
- [x] Named per-provider coordinator fields removed.
- [x] Provider-name branches removed from `coordinator.rs`, `realtime.rs`, `publication.rs`,
  `instrument_selection.rs` and `mod.rs`.

### Remaining

- [x] Remove the remaining provider-name branches in `market_service/runtime.rs` (history-source
  checks, reconnect-delay and catalog-publisher wiring) and `market_service/history.rs`; anything
  provider-specific belongs in the descriptor or the provider's own module.
- [x] Move coordinator logic still living in a provider's `market_service` module behind the
  neutral contract; that module keeps only the provider worker, history source and descriptor.
- [x] Move `merge_live_candle` and its tests from the Hyperliquid adapter into `market_runtime`,
  if not already done.
- [x] Test proving a fourth descriptor registers with no coordinator changes.
- [x] Existing coordinator/realtime/history tests pass, ported without weakening.
- [x] Update `naming_check` assertions that named removed functions/types to equal-or-stricter
  structural checks (e.g. one live map; no `provider_id ==` literals outside descriptor and
  provider modules).
- [x] Gates: check/test/clippy (`--all-targets --all-features -- -D warnings`) for
  `aeris_market_runtime`, `aeris_hyperliquid_market_adapter`, `aeris_rithmic_protocol_adapter`,
  the provider adapters of that time, `aeris_platform_runtime`, `aeris_desktop` and `naming_check`.
- [x] Ignored live Hyperliquid tests that need no credentials.
- [x] Commit.

Constraints: every bound, overflow/cancellation behavior, generation fence, stop fence, idle-stop
overlap recovery, catalog overflow rejection and history retry policy behaves exactly as before.
`MarketService`'s public API and `MarketRuntimeEvent` stay unchanged unless unavoidable.

Maintainer-only: Rithmic test smoke (needs credentials).

---

## Phase 3 — Provider-neutral desktop

- [x] Header **Accounts** panel (left of the time zone, secondary surface) replaces the
  command-palette broker commands and the order-ticket practice-account dialogs: broker
  connect/disconnect with runtime status (`MarketService::provider_connected`), Hyperliquid
  public feed, practice account create/select/delete. The order ticket only shows the active
  account and opens the panel. When descriptors land, list providers from them instead of
  hardcoding provider names in `components/accounts_panel.rs`.
- [x] `market_runtime` publishes provider presentation descriptors through a provider-neutral
  contract: id, display name, chart intervals, default/empty-query listing, search hint, logo key,
  catalog-symbol source, depth capability, connection kind (credentials, public).
- [x] Replace `TerminalProvider` branches in `apps/desktop/src/desktop.rs`,
  `desktop/workspace_surface.rs`, `components/{symbol_menu,terminal_view,watchlist_panel,time_sales_panel}.rs`,
  `engine_market_worker*.rs`, `market_worker.rs` and `desktop/local_state.rs` with descriptors.
  - [~] Desktop display names and default listing queries now read from the runtime descriptor registry;
    search hints and chart interval tables do as well; symbol-menu and watchlist provider marks now
    consume descriptor metadata. Connection messages, connection-kind prompts and depth capability
    now also consume descriptor metadata; worker status labels do as well. Interval lookup is keyed
    by descriptor ID instead of positional provider matches. Provider-specific selection payloads and
    product-policy branches remain provider-owned; generic engine lifecycle and depth checks now use
    descriptor presence/capabilities instead of enumerating provider variants; viewport persistence
    and connection-kind prompt labels follow the same metadata. Startup catalog matching now reads
    the descriptor's catalog-symbol source and selection entitlement instead of naming a provider;
    generic menu selection carries only its result index instead of provider-tagged variants;
    startup metadata refresh is descriptor-declared and the time-and-sales history header is
    provider-neutral; ready-market labels and Day 3 cadence/replay support now use descriptor metadata.
    Remaining provider names are limited to provider-owned selection payloads, wire identity and
    entitlement validation, worker identities, and the Rithmic default-selection product policy.
- [x] Per-provider error strings become templates using the display name; interval tables come
  from descriptors.
- [x] Investigate `RITHMIC_ENTITLEMENT_ID = "crypto_public_realtime"` in `desktop.rs`: it is the
  Rithmic Test entitlement expected by the current adapter path, so rename it to
  `RITHMIC_TEST_ENTITLEMENT_ID`; adapter selection entitlement preservation remains covered by
  `delayed_catalog_selection_preserves_entitlement_while_using_the_base_market_venue`.
- [x] Providers without depth publish an explicit Level 1 top-of-book frame; such a provider becomes
  Ready on the first current BBO, shows best bid/ask and traded volumes, and retains genuine
  connection failure banners while Rithmic and Hyperliquid retain full depth.
- [x] Session-gapped provider timeframe and series recovery stays scoped to the affected series,
  bounds late candle subscriptions from the current wall clock, and publishes a terminal Failed
  state after automatic retries are exhausted instead of leaving the desktop in a reconnecting state.
- [x] Durable local state keeps loading existing provider selections; sanitizer regression coverage
  exercises every provider identity and entitlement route.
- [x] Gates and commit.

Maintainer-only: visual check of the symbol menu and depth-unavailable state.

## Phase 4 — Split large files (mechanical, no logic changes)

- [x] `crates/market_runtime/src/study.rs` (7.7k lines) — moved its test module to
  `crates/market_runtime/src/study/tests.rs`; production code remains in the public module.
- [x] `apps/desktop/src/desktop/workspace_surface.rs` (6k) — moved its test module to
  `apps/desktop/src/desktop/workspace_surface_tests.rs`.
- [x] `crates/account_runtime/src/account_service/mod.rs` (5.7k) — moved its test module to
  `crates/account_runtime/src/account_service/tests.rs`.
- [x] `apps/desktop/src/desktop.rs` (5k) — moved the remaining inline HTTP wiring tests to
  `apps/desktop/src/http_wiring_tests.rs`; the main test module was already external.
- [x] `crates/trading_runtime/src/lib.rs` (4.6k) — its test module was already external in
  `crates/trading_runtime/src/tests.rs`.
- [x] Oversized provider modules under `market_service` and anything still oversized in
  `market_service/{coordinator,realtime}.rs` after Phase 2 — moved provider tests to sibling
  `*_tests.rs` files; coordinator/realtime remain runtime-owned cohesive files.
- [x] Update path-based `naming_check` assertions to the new locations with equal strictness;
  extracted test boundaries are now asserted explicitly and the production ownership checks remain
  on their original owners.
- [x] Gates and commit.

Pure moves plus visibility changes; tests move with their code.

## Phase 5 — Prune `third_party/gpui_base`

- [x] Map the transitive internal dependencies of what the desktop uses: `gpui_base::init`,
  `Theme`/`ColorTokens`/`RadiusTokens`/`ThemeAppearance`, `Button`, `Switch`/`SwitchThumb`/`SwitchTrack`,
  `input::{Input, InputState, InputEvent, InputEditorStyle}`.
- [x] Remove every unused module (dock, calendar, date_picker, color_picker, table, tree, sheet, …)
  and unused dependencies. The retained set is the button/switch/input behavior, theme tokens,
  scrollbar support, global state, and their bounded interaction helpers.
- [x] Keep `LICENSE-APACHE` and the `upstream-revision` metadata; document the retained set in the
  crate's `lib.rs` doc.
- [x] Desktop builds and tests pass; line counts after pruning are recorded in the delivery report.
- [x] Commit.

Maintainer-only: visual check that inputs, buttons and switches behave identically.

## Phase 6 — `naming_check` review

- [x] Keep every ownership, dependency-direction, boundedness and security rule; the naming gate
  passes 57 tests after the module-pruning review.
- [x] Replace brittle function-name/string-presence assertions with equal-or-stricter structural or
  behavioral checks for the provider-runtime ownership test; remaining source checks protect active
  lifecycle boundaries and current security contracts rather than removed names.
- [x] List removal candidates (rules whose original reason appears gone) with evidence; delete nothing.
  - [x] No rule met the removal threshold: provider ownership, bounded queues, credential scope,
    process topology, and release lifecycle checks all still have live code paths and regression
    evidence.
- [x] Commit.

---

## Final gates (after Phase 6)

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [x] `cargo test --workspace --all-features --locked`
- [x] Website `infra` tests (if the website changed): backend `npm test` and CDK infra
  `npm run typecheck` both pass in `aeris-website`.

Report pre-existing failures precisely; fix every failure the work caused.

## Report expected at each phase end

Commit hash per repo, what changed, tests added, gates and results, deployments, blocked and
maintainer-only items, measured timings with method, every `naming_check` change (old → new and
why it is not weaker), removal candidates, line counts before/after for `market_runtime`,
`apps/desktop` and `third_party/gpui_base`, and any `AGENTS.md` principle not fully satisfied.

## Product follow-ups (not part of this plan's phases)

- [ ] Next task: define the provider-neutral, `trading_runtime`-owned read-only account
  observation contract for positions, live PnL, journal entries and portfolio balances. Keep
  refresh work bounded and generation-fenced, use the existing vault `read` scope as the only
  credential boundary, and add contract/restart tests before wiring a provider adapter or UI.
- Rithmic production: production endpoints, system/gateway picker from Rithmic's system list,
  replacing the hardcoded Rithmic Test endpoint; confirm per-system enablement with Rithmic.
