# Aeris Terminal — Architectural Refactoring Plan

Tracked roadmap for converting Aeris Terminal from provider-coupled runtime monoliths into a clean, adapter-driven, multi-connection, maintainable, and scalable architecture.

## How to use this file

- `[x]` done, verified, and committed · `[~]` in progress · `[ ]` not started
- Every phase must satisfy all five core principles in `AGENTS.md` (Proper architecture, Clean code, Scalable, Maintainable, Built for the long run) and pass `cargo fmt --check`, `aeris_naming_check`, `clippy --all-targets --all-features -D warnings`, and `cargo test --workspace --all-features --locked`.
- Preserve durable state compatibility across released versions, exact generation fencing, bounded queues, and single-owner runtime invariants.
- Refactor incrementally in shippable, behavior-preserving batches—never a big-bang rewrite. Update checkboxes in the same commit as the work they complete.

---

## Baseline Audit (2026-11-20)

Concrete architectural bottlenecks measured on `main` (`dcb694f4`):

1. **Provider coupling inside `crates/market_runtime`:**
   - `crates/market_runtime/Cargo.toml` directly depends on concrete adapter crates (`aeris_hyperliquid_market_adapter`, `aeris_rithmic_protocol_adapter`) and `aeris_trading`.
   - Concrete provider runtime workers live inside `crates/market_runtime/src/` instead of in their adapter crates: `hyperliquid_{realtime,history,display_depth,market_scan,market_screen}.rs`, `rithmic_{realtime,history}.rs` (3,983 lines), `rithmic_login.rs`, and `rithmic_venue/`.
   - Hardcoded provider references permeate `market_service/` (`coordinator.rs`: 441 matches, `rithmic_login.rs`: 157, `runtime.rs`: 156, `mod.rs`: 124, `provider_session.rs`: 104, `realtime.rs`: 102).
   - `ProviderSession` carries provider-specific escape hatches (`adapter_retries_transport`, `adapter_labels_sessions`, `rebind`), and `provider_event.rs` directly matches `aeris_rithmic_protocol_adapter::ProviderInvalidationReason`.
2. **Hardcoded broker singleton & 1-login-per-provider:**
   - `market_service/broker_venue.rs` hardcodes `self.runtime.rithmic_login`, spawns `crate::rithmic_venue::run` directly, and enforces a single `venue_attached: AtomicBool`.
   - `market_runtime` owns broker venue lifecycle even though `trading_runtime` is the authoritative broker/order owner.
   - `RithmicLogin` assumes one saved login (`RITHMIC_LOGIN_VAULT_KEY`) per app, blocking simultaneous **Aeris-managed CME data** + **user-supplied prop-firm Rithmic logins** + additional brokers (Tradovate, CQG, IBKR, etc.).
3. **Oversized monolith files:**
   - `crates/market_runtime/src/market_service/coordinator.rs`: 10,792 lines
   - `apps/desktop/src/desktop/workspace_surface.rs`: 8,356 lines
   - `apps/desktop/src/desktop.rs`: 6,469 lines
   - `crates/adapters/rithmic_protocol/src/provider_session.rs`: 4,850 lines
   - `crates/release_lifecycle/src/lifecycle.rs`: 4,566 lines
   - `crates/ui/terminal_ui/src/order_book_view.rs`: 4,320 lines
   - `crates/market_runtime/src/study.rs`: 4,261 lines
   - `apps/desktop/src/desktop/workspace_tabs.rs`: 4,247 lines
   - `crates/market_runtime/src/rithmic_realtime.rs`: 3,983 lines
   - `apps/desktop/src/desktop/engine_market_worker.rs`: 3,813 lines
4. **Stringly-typed internal boundaries & legacy IPC translation glue:**
   - `Reply<T> = SyncSender<Result<T, String>>` across runtime boundaries hinders typed fault recovery and localization.
   - `apps/desktop/src/desktop/engine_market_worker.rs` still translates in-process calls through legacy IPC-shaped protobuf `aeris_contracts` wrappers.

---

## Target Dependency Architecture

```text
apps/desktop (Composition Root — registers concrete adapters into MarketService & TradingService)
   ├── crates/adapters/hyperliquid_market  (implements MarketProviderPlugin + optional BrokerVenuePlugin)
   ├── crates/adapters/rithmic_protocol    (implements MarketProviderPlugin + ProviderAuthPlugin + BrokerVenuePlugin)
   ├── crates/adapters/<future_provider>   (plugs in with zero edits to market_runtime or trading_runtime)
   ├── crates/market_runtime               (provider-neutral coordinator, leases, history merge, recording, alerts, studies)
   │      └── crates/provider_spi          (shared neutral traits: MarketProviderPlugin, ProviderAuthPlugin, ProviderFault, CatalogDispatch, HistoryFetcher, RealtimeSession)
   ├── crates/trading_runtime              (single owner of accounts, orders, fills, risk + multi-venue BrokerVenueRegistry)
   │      └── crates/domain/trading        (VenueCapabilities, VenueEvent, BrokerVenuePlugin contract)
   └── crates/account_runtime              (Aeris OIDC identity & managed entitlement credentials)
```

**Core Rule:** Once refactored, `crates/market_runtime` must have **zero** Cargo dependencies on any `crates/adapters/*` crate and **zero** provider-specific branches inside `market_service/`.

---

## Execution Roadmap (Recommended To-Do Order)

### Phase 1 — Define Neutral Provider & Auth SPI (`provider_spi` / Adapter Contracts)
*Goal: Establish clean, typed, provider-neutral contracts before moving concrete workers out of `market_runtime`.*

- [x] **1.1 Canonical `ProviderFault` & fault-recovery classification:** Replace direct `aeris_rithmic_protocol_adapter::ProviderInvalidationReason` matching in `market_service/provider_event.rs` with a provider-neutral `ProviderFault { kind: ProviderFaultKind, auto_recovers: bool, detail: &'static str }` produced by each adapter.
- [x] **1.2 Clean `ProviderSession` & `ProviderCatalog` trait contract:** Eliminate provider-specific quirks leaking through `market_service/runtime.rs` (`dispatch_rithmic_catalog`, `dispatch_hyperliquid_catalog`, `LiveRithmicHistory`, `LiveHyperliquidHistory`) behind cohesive `ProviderRuntimeFactory`, `HistorySource`, and `CatalogDispatcher` traits.
- [x] **1.3 Provider-neutral `ProviderAuthAdapter` & `CredentialProvider` trait:** Abstract `discover_servers`, `saved_login`, `save_login`, `clear_login`, and credential-revision watching out of `market_service/rithmic_login.rs` into a pluggable auth adapter registered per provider (designing cleanly for both `UserVault` and future `ManagedAeris` credentials).
- [x] **1.4 Typed internal runtime errors:** Introduce typed `MarketServiceError` / `ProviderAuthError` / `VenueAttachError` enums at internal seams to replace ad-hoc `Result<_, String>` replies without breaking existing public APIs prematurely.
- [x] **1.5 `naming_check` guardrail:** Enforce that `market_service/{coordinator,realtime,history,publication,provider_event,provider_session,runtime}.rs` cannot import concrete `aeris_*_adapter` types directly.

### Phase 2 — Extract Concrete Providers Out of `market_runtime` Into Adapter Crates
*Goal: Make `crates/market_runtime` genuinely provider-neutral with zero adapter crate dependencies.*

- [ ] **2.1 Move Hyperliquid runtime workers into `crates/adapters/hyperliquid_market`:** Relocate `hyperliquid_realtime.rs`, `hyperliquid_history.rs`, `hyperliquid_display_depth.rs`, `hyperliquid_market_scan.rs`, `hyperliquid_market_screen.rs`, and Hyperliquid presentation/descriptor builders behind `MarketProviderPlugin`.
- [ ] **2.2 Move Rithmic runtime workers & login into `crates/adapters/rithmic_protocol`:** Relocate `rithmic_realtime.rs`, `rithmic_history.rs`, `rithmic_login.rs`, and Rithmic presentation/descriptor builders behind `MarketProviderPlugin` + `ProviderAuthPlugin`.
- [ ] **2.3 Move `rithmic_venue/` out of `market_runtime` into `crates/adapters/rithmic_protocol`:** Remove `aeris_trading` broker-venue coupling from `crates/market_runtime`.
- [ ] **2.4 Composition-root registration:** Register built-in adapter plugins (`HyperliquidPlugin`, `RithmicPlugin`, `RecordingReplayPlugin`) explicitly at the application composition root (`DesktopServices` / `MarketService::builder().register_provider(...)`).
- [ ] **2.5 Remove adapter dependencies from `crates/market_runtime/Cargo.toml`:** Delete `aeris_hyperliquid_market_adapter` and `aeris_rithmic_protocol_adapter` from `market_runtime` dependencies, and add a `naming_check` rule ensuring `market_runtime` never re-acquires a concrete adapter dependency.
- [ ] **2.6 Pluggable provider regression test:** Add an integration test registering a mock 3rd provider + mock broker venue solely through the plugin traits with zero edits to `market_runtime`.

### Phase 3 — Multi-Venue Broker Registry & `ConnectionId` Separation
*Goal: Unlock simultaneous Aeris-managed CME data + user prop-firm Rithmic logins + multiple broker venues (Tradovate, IBKR, Hyperliquid execution, etc.).*

- [ ] **3.1 Move broker venue ownership to `trading_runtime` (`BrokerVenueRegistry`):** Replace `MarketService::attach_broker_venue` (single `venue_attached: AtomicBool`) with a bounded multi-venue registry on `TradingService` keyed by `VenueId` / `ConnectionId`.
- [ ] **3.2 Decouple `ConnectionId` from `ProviderId`:** Introduce `ConnectionId` + `CredentialSource` (`Public`, `UserVault { key }`, `ManagedAeris { entitlement_id }`) so one provider adapter (e.g. Rithmic) can host multiple independent connections (e.g. Aeris-managed CME data feed alongside one or more user prop-firm accounts).
- [ ] **3.3 In-app R|Protocol agreement hook (`ProviderAuthPlugin`):** Expose a clean adapter-neutral `PendingAgreement` / `accept_agreement` flow so managed CME subscribers can sign CME Market Data Agreements inside Aeris Terminal without installing R|Trader Pro.
- [ ] **3.4 Migration & compatibility tests:** Verify existing persisted workspaces and legacy single-login Rithmic vault entries migrate transparently to the default `ConnectionId`.

### Phase 4 — Decompose `market_runtime` Monoliths (`coordinator.rs` & `study.rs`)
*Goal: Break the 10,792-line `coordinator.rs` into small, testable, single-responsibility domain owners.*

- [ ] **4.1 Extract `SeriesBookkeeper` (series history & live-bar merging):** Isolate canonical bar history, forming-candle handoff, viewport fetch scheduling, and gap recovery from the coordinator loop.
- [ ] **4.2 Extract `DepthAndTapeBookkeeper` (L2 book, DBO queues, tape, sweeps):** Isolate order book state, DBO order levels, recent trade retention, and sweep detection behind a clean unit-testable module.
- [ ] **4.3 Extract `CatalogAndScanCoordinator`:** Isolate symbol search, catalog caching, market screener, and market scanner dispatch/fanout.
- [ ] **4.4 Extract `RecordingAndReplayRouter`:** Isolate live recording taps and recording-replay session lifecycle from the core coordinator loop.
- [ ] **4.5 Decompose `study.rs` (4,261 lines) and `rithmic_protocol/src/provider_session.rs` (4,850 lines):** Split study scheduling/state storage and Rithmic plant handlers into focused submodules (< 800 lines each).

### Phase 5 — Simplify Desktop Bridge & Decompose UI Monoliths
*Goal: Eliminate legacy IPC-era translation layers and make `apps/desktop` easy to navigate and extend.*

- [ ] **5.1 Slim down `apps/desktop/src/desktop/engine_market_worker.rs` (3,813 lines):** Remove redundant IPC-era protobuf round-trips for in-process calls where direct domain types already exist; keep `aeris_contracts` strictly for wire/persistence boundaries.
- [ ] **5.2 Unify instrument metadata projection:** Consolidate repeated conversions between `InstallProviderInstrument`, `InstrumentDescriptor`, `ProviderCatalogSymbol`, and `InstrumentTerms` into one authoritative domain projection.
- [ ] **5.3 Decompose `workspace_surface.rs` (8,356 lines), `desktop.rs` (6,469 lines), and `workspace_tabs.rs` (4,247 lines):** Split into cohesive controllers (chart pane controller, order-entry/trading controller, side-panel dock controller, layout/sync controller, keyboard command router).
- [ ] **5.4 Decompose `order_book_view.rs` (4,320 lines):** Separate price-grid layout math, volume/delta/imbalance analytics, and GPUI cell rendering into distinct modules.

### Phase 6 — End-to-End Architectural Guardrails & Cleanup
*Goal: Lock in the new boundaries permanently so coupling cannot creep back.*

- [ ] **6.1 Strengthen `tools/naming_check` architectural rules:**
  - `market_runtime` cannot depend on `crates/adapters/*` or `aeris_trading::venue`.
  - No `provider_id ==` or concrete adapter imports inside `market_service/` core modules.
  - Ratchet file-size / `Result<_, String>` counts downward after each completed phase.
- [ ] **6.2 Update `AGENTS.md` architecture invariants:** Document the new `provider_spi`, `BrokerVenueRegistry`, and `ConnectionId` invariants once landed.
- [ ] **6.3 Full workspace verification:** Format, `aeris_naming_check`, full workspace `clippy --all-targets --all-features -D warnings`, full workspace `cargo test --workspace --all-features --locked`, and release smoke launch.
