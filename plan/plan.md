# Axiusflow Architecture and Authentication Migration Plan

Status: proposed for maintainer approval

Baseline: `main` at `d04ce9a` when this plan was consolidated

Scope: one ordered migration with five phases; authentication is the final phase

## Decision

Complete the resident-engine and provider-coordination cleanup before adding accounts or authentication. After the primary architecture is stable and verified, add an Axiusflow-operated Better Auth control plane on Cloudflare Workers and D1. The native applications depend only on standard OpenID Connect, while Axiusflow owns identity data, account identifiers, billing state, entitlements, and the migration path.

Phase 5 must not begin until the phase 4 exit gate passes. Earlier phases must not add dormant authentication types, temporary account state, login UI, cloud identity networking, or feature checks.

## Product outcome

The completed system has:

- one resident `MarketEngine` demand owner;
- one bounded provider-runtime path owned by `apps/engine`;
- one canonical history/live handoff and forming candle;
- a decomposed but still single-owner market coordinator;
- a provider-neutral, versioned local IPC contract;
- durable local history behind `local_history`;
- signed release delivery independent of CDN trust;
- an engine-owned user session backed by standard native OIDC;
- Axiusflow-owned account, billing, and entitlement truth; and
- no dependency by the desktop or market core on Better Auth, Stripe, Dodo, Cloudflare, or provider wire types.

## Non-negotiable invariants

- Market values remain fixed-point and retain provenance, sequence, timestamp, and generation validation.
- History/live handoff maintains contiguous completed history and exactly one canonical forming candle.
- Retired clients, selections, requests, sessions, and publications cannot mutate current state.
- Provider sessions survive symbol, timeframe, viewport, tab, and layout changes.
- Queues, retries, caches, requests, and background work remain bounded and cancellable.
- Loading resolves to data, bounded recovery, retry, or an actionable terminal error.
- The UI thread performs no network, disk, process, or shutdown work.
- Background workers publish results through the owning coordinator; they never mutate GPUI state directly.
- Credentials, tokens, raw provider payloads, webhook bodies, and payment details never enter logs.
- The desktop depends on no provider adapter, storage implementation, `market_engine`, Better Auth package, or payment SDK.
- There remain two local applications: desktop and engine. The later cloud control plane is not a third local application.
- `unsafe_code` remains forbidden workspace-wide.

## Target ownership after migration

### Desktop

The desktop renders workspaces, charts, DOM, provider selection, account views, and recovery actions. It sends bounded IPC commands and opens system-browser URLs supplied by the engine. It owns no provider session, cloud HTTP client, refresh token, payment secret, entitlement truth, or persistent identity data.

### Resident engine

The engine owns provider runtimes, canonical market state, history scheduling, persistence orchestration, client generations, publication, shutdown, and - only in phase 5 - one account session shared by all desktop windows. Network and vault work run on bounded background workers.

### `MarketEngine`

`MarketEngine` remains the provider-neutral authority for demand, provider capabilities and generations, canonical series, shared subscription reference counts, resource policy, and publications. The cleanup must not move provider sessions or wire types into this crate.

### Provider adapters

Coinbase and Rithmic adapters own wire decoding, provider-specific transports, and provider-specific runtime drivers. Wire types stop at these boundaries. The engine consumes bounded provider-neutral commands and events.

### Local history

`local_history` remains the only engine-facing persistence boundary. Provider history, storage mechanics, encryption, retention, quarantine, and recovery remain behind their existing crate boundaries.

### Cloud control plane - phase 5 only

A Cloudflare Worker hosts the authentication UI, Better Auth OIDC provider, account linking, billing adapters, webhook reconciliation, entitlement signing, and release authorization metadata. D1 stores the Better Auth schema and Axiusflow control-plane records. R2/CDN may distribute signed releases but never establishes artifact authenticity.

## Why authentication is last

`apps/engine/src/market_service.rs` currently combines process wiring, bounded channels, provider selection, history sources, realtime sources, handoff logic, storage completions, canonical publication, consumer queues, catalog routing, lifecycle handling, and a very large inline test suite. Adding account state, token refresh, browser callbacks, billing refresh, and entitlement publication before separating those responsibilities would increase the coordinator's blast radius and make rollback harder.

The correct order is therefore:

1. lock the behavior and boundaries;
2. consolidate provider runtime ownership;
3. decompose the market coordinator without splitting authority;
4. stabilize and prove the migrated architecture; then
5. add authentication, billing, and entitlements through the clean boundary.

## Phase 1 - Lock behavior and migration boundaries

### Goal

Create proof that later structural moves preserve current behavior. This phase changes no product behavior and introduces no authentication code.

### Work

- Record the actual focused and workspace verification baseline; never copy a stale warning count into the plan.
- Map every command, event, queue, worker, state owner, provider-generation check, consumer-generation check, and shutdown path in `MarketService`.
- Identify the tests that prove:
  - symbol and interval changes reuse provider sessions;
  - newer demand cancels obsolete history work;
  - viewport history belongs to the current consumer and provider generations;
  - history/live handoff is contiguous and does not duplicate forming data;
  - Rithmic reconnect and environment transitions fence retired publications;
  - Coinbase history, realtime, depth, and order flow remain bounded; and
  - multiple desktop clients remain isolated.
- Strengthen `tools/naming_check` only where a durable ownership rule is not already enforced.
- Establish a module move order so each commit compiles and replaces its previous path instead of creating compatibility layers.
- Keep the existing `HistorySource`, `RealtimeSource`, provider adapter, platform, and security traits only where they represent real boundaries or test substitution.

### Exit gate

- Focused market, history, provider, IPC, lifecycle, and persistence tests pass.
- Workspace architecture checks pass.
- The release desktop and engine are identified and can exercise the current Coinbase path; Rithmic evidence is recorded when credentials and the provider environment are available.
- Every later move has an owning module, source path, destination path, and regression test.

## Phase 2 - Consolidate provider runtime ownership

### Goal

Give the engine one explicit bounded provider-runtime registry without moving authority out of `MarketEngine` or creating one runtime per chart.

### Work

- Replace provider branching and the `HistorySources::Shared`/`Split` composition path with one bounded engine-owned registry keyed by provider identity.
- Keep `ProviderManager` inside `MarketEngine` as the provider-neutral capability, health, request-validation, and generation authority.
- Move provider construction, start, stop, reconnect, environment selection, catalog control, history dispatch, realtime dispatch, and provider-event normalization behind the engine registry.
- Preserve the existing adapter boundaries:
  - Coinbase history transport and aggregation stay in the Coinbase adapter;
  - Rithmic session drivers, wire decoding, and plant behavior stay in the Rithmic adapter;
  - engine-facing events contain canonical/domain or application types only.
- Route history, realtime, trades, quotes, and depth by declared provider capabilities rather than scattered provider-name conditionals.
- Make every provider control queue and event queue bounded, with an explicit overflow outcome.
- Make provider generation, cancellation token, worker handles, reconnect state, and terminal failure part of one registry record.
- Ensure provider teardown occurs only for explicit provider/session lifecycle events, never for presentation changes.
- Delete superseded provider construction and dispatch paths as each registry route becomes authoritative.

### Exit gate

- Exactly one session/runtime exists per configured provider account and environment.
- Symbol, timeframe, viewport, tab, and layout changes reuse that session.
- Stale generations and retired worker completions are rejected deterministically.
- Coinbase and Rithmic capability routing passes contract tests.
- Provider queues, reconnect loops, catalog results, and shutdown are bounded.
- No desktop, chart, or storage module can construct a provider runtime.

## Phase 3 - Decompose the market coordinator without splitting authority

### Goal

Turn `market_service` into a small composition and command boundary while retaining one coordinator thread and one canonical state owner.

### Work

- Convert the monolithic file into an internal `market_service/` module tree. Use the smallest useful set of modules; do not create new crates or service layers.
- Extract provider-neutral responsibilities by ownership:
  - `runtime`: process-owned channels, worker handles, startup, cancellation, and shutdown;
  - `coordinator`: the single event loop, command dispatch, generation fences, and top-level state transitions;
  - `history`: request state, retries, viewport backfill, repairs, page reconciliation, and history/live handoff;
  - `realtime`: live-series state, forming candles, Rithmic and Coinbase canonical event handling, depth, and order flow;
  - `storage`: bounded requests and completion handling through `local_history`;
  - `publication`: per-consumer bounded queues, snapshots, deltas, load states, and overflow recovery; and
  - `instrument_selection`: catalog validation, installation, and provider-neutral selection state.
- Move code according to the state it owns, not merely by function size.
- Keep one `Coordinator` owner. Extracted modules operate through narrow methods or owned sub-state; they do not maintain mirrors of canonical series, provider generation, selection, or consumer state.
- Move inline unit tests beside their owning modules and retain end-to-end coordinator tests for cross-module behavior.
- Replace obsolete helpers and duplicate state immediately; do not retain forwarding wrappers after callers move.
- Keep `apps/engine/src/lib.rs` focused on local IPC sessions, workspace state, background-service lifecycle, and routing into `MarketService`.

### Exit gate

- The top-level coordinator and process-wiring modules are readable composition boundaries rather than repositories of provider algorithms.
- No canonical market state or generation authority is duplicated.
- History/live, reconnect, stale-result, persistence, depth, order-flow, resource-pressure, and multi-client tests remain green.
- The architecture checker enforces the resulting ownership boundaries.
- The release application demonstrates continuous market data through symbol, timeframe, tab, layout, reconnect, offline-start, and shutdown transitions.

## Phase 4 - Stabilize integration, recovery, and signed delivery

### Goal

Finish and prove the primary architecture migration before any authentication work begins.

### Work

- Review the engine protocol after decomposition and remove obsolete messages or duplicate state. Version any necessary wire change explicitly.
- Keep IPC frames, client outboxes, decode limits, request IDs, and publication queues bounded.
- Prove workspace and hot-set restoration through the decomposed coordinator.
- Prove local-history startup, corruption quarantine, refetch, retention, and key-revocation behavior through the real engine path.
- Complete orderly cancellation and join behavior for provider, history, storage, IPC, and platform lifecycle workers.
- Verify that loading always reaches data, bounded retry/recovery, or an actionable terminal error.
- Remove migration-only aliases, forwarding functions, duplicate fixtures, and old module paths.
- Keep release authenticity independent of authentication:
  - build immutable platform/architecture artifacts;
  - generate a versioned manifest with version, channel, minimum version, URL, size, hash, and rollout metadata;
  - sign the canonical manifest with a key unavailable to R2/CDN;
  - verify signature, size, hash, platform, and downgrade policy before installation; and
  - preserve the current installation after interrupted, corrupt, disk-full, or rejected updates.
- Run focused checks while iterating, then all workspace gates:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo build --workspace --all-targets --all-features`
  - `cargo test --workspace --all-features`
- Build and run the release desktop and resident engine and verify that they are the intended binaries.

### Exit gate - hard prerequisite for phase 5

- All workspace gates pass without suppressing warnings.
- Coinbase and available Rithmic release paths pass live verification.
- Streaming, history, persistence, IPC, recovery, resource pressure, multi-window behavior, and shutdown pass their acceptance evidence.
- No provider session is recreated by presentation changes.
- No obsolete migration path or duplicate authority remains.
- Signed-update negative cases fail safely if update delivery is included in the release.
- The maintainer explicitly approves starting the account platform.

## Phase 5 - Add Better Auth, accounts, billing, and entitlements

### Goal

Add identity and subscriptions through the now-stable engine boundary without modifying the market coordinator, provider registry, history/live logic, or chart integration.

### Identity decision

- Operate Better Auth on Cloudflare Workers with D1.
- Use Better Auth's OAuth 2.1/OpenID Connect provider surface.
- Register the desktop as a public native client with no embedded secret.
- Use the system browser, Authorization Code flow, S256 PKCE, a nonce and state, and an ephemeral callback bound to literal `127.0.0.1`.
- Start with email OTP and one social OIDC provider. Do not enable passwords or passkeys until recovery, export, reset, and support policies are approved.
- Pin exact Better Auth and OAuth Provider package revisions.
- Keep the Rust engine dependent only on OIDC discovery, authorization, token, refresh, revocation, logout, UserInfo, and JWKS standards.
- Keep Better Auth IDs and schema types out of the desktop, engine protocol, account domain, billing ledger, and entitlement claims.

### Data ownership

D1 contains two deliberately separate groups of data:

- the Better Auth schema for identities, sessions, verification state, OAuth links, and any later credential factors; and
- Axiusflow tables for `AccountId`, `identity_links`, billing customers, subscriptions, webhook inbox, entitlement revisions, devices, and release authorization metadata.

Axiusflow's `AccountId` is canonical. A Better Auth user ID, email address, Stripe customer ID, Dodo customer ID, Rithmic account ID, and provider `entitlement_id` are all external links with distinct meanings.

### Native account flow

1. The desktop sends `BeginLogin` to the resident engine over authenticated bounded IPC.
2. The engine creates state, nonce, PKCE verifier/challenge, a request generation, timeout, cancellation token, and loopback listener.
3. The desktop opens the Axiusflow authentication URL in the system browser.
4. Better Auth authenticates the user and returns one authorization code to the loopback callback.
5. The engine validates the transaction, exchanges the code, validates issuer/audience/signature/time claims, and links the OIDC subject to `AccountId` through the control plane.
6. Access tokens remain in memory. Refresh material and the newest valid entitlement lease use separate account-specific keys in the native credential vault.
7. The desktop receives only sanitized `AccountView` state and actionable errors.

The listener accepts one pending transaction, rejects duplicates and mismatched state, times out, closes after completion, and never binds to all interfaces.

### Engine-owned account states

- `SignedOut`
- `Authorizing`
- `Active`
- `OfflineLease`
- `ReauthenticationRequired`
- `LeaseExpired`
- `TerminalError`

Each transition is generation-fenced. Retired login, refresh, checkout, logout, webhook reconciliation, and lease results cannot mutate current state.

### Code boundaries

- `crates/domain/account`: `AccountId`, `PlanId`, `FeatureId`, `FeatureSet`, lease claims, and pure validation; no HTTP, GPUI, provider, Better Auth, Stripe, or Dodo types.
- `crates/engine_protocol/src/account.rs`: bounded versioned commands/events and sanitized views; no tokens or vendor payloads.
- `apps/engine/src/account_service/`: account state machine, PKCE transaction, vault integration, control-plane client, refresh, cancellation, and lease cache; no market-coordinator logic.
- Desktop account components: IPC actions, browser opening, account rendering, and recovery UX; no cloud HTTP or credentials.
- Cloudflare Worker: Better Auth, login UI, D1 identity/account records, billing adapters, webhook inbox, entitlement signing, and checkout/portal creation; no provider sessions or native state.

### Billing

- Prefer Stripe Billing and hosted Checkout if Axiusflow's merchant account is approved.
- Keep Dodo behind the same cloud billing adapter as a fallback.
- Do not implement Stripe Connect unless Axiusflow will onboard and pay third-party sellers.
- Map vendor product and price IDs to internal `PlanId`; never expose them through native IPC.
- Verify webhook signatures against the unmodified raw body before parsing.
- Store each event under unique `(provider, event_id)` identity, acknowledge after durable receipt, reconcile asynchronously, and tolerate duplicate and out-of-order delivery.
- Update canonical subscription state and entitlement revision consistently.
- Use bounded retry with a terminal diagnostic state.

### Entitlement lease

The control plane signs a compact lease containing schema version, `AccountId`, device ID, `PlanId`, `FeatureSet`, entitlement revision, issued-at, not-before, expiry, audience, and signing-key ID. It contains no email, name, vendor customer ID, or payment details.

The engine validates schema, audience, signature, time window, account/device binding, monotonic revision, known key, and encoded size. It never replaces a valid cached lease with an older revision.

Proposed defaults requiring explicit maintainer approval:

- online refresh every six hours with jitter and earlier near expiry;
- 72-hour offline lease validity;
- seven-day recoverable billing grace;
- immediate revocation only for explicit security, fraud, or administrator action; and
- a device limit and reset policy chosen before enforcement.

Enforcement progresses within phase 5 as ordered work, not additional phases:

1. define and test account/lease contracts;
2. run the Better Auth Worker/D1 sandbox and native PKCE flow;
3. integrate hosted billing and webhook reconciliation;
4. observe signed entitlements in shadow mode for at least one stable release;
5. ship warnings and recovery actions; and
6. enforce only the explicitly approved feature boundary with a tested server-side rollback.

Payment changes never abruptly destroy provider sessions or corrupt persisted market state. Enforcement blocks new gated demand or enters an explicitly designed degraded/read-only state.

### Authentication operations owned by Axiusflow

- database schema generation and forward/rollback migrations;
- encrypted D1 export and clean-environment restore drills;
- OIDC and entitlement-signing key rotation with overlapping verification keys;
- Better Auth security-advisory monitoring and emergency upgrades;
- rate limits, bot protection, abuse detection, CSP, cookie policy, and redirect allow-lists;
- authentication-domain DNS/TLS and sender-domain alignment;
- transactional email delivery, bounce/failure handling, and recovery UX;
- privacy, deletion, retention, audit, and incident-response procedures; and
- capacity and cost monitoring for Workers, D1, email, SMS if ever enabled, and logs.

Better Auth is selected for control and portability, not because authentication becomes free to operate. The framework introduces no MAU or custom-domain rent, but infrastructure and security operations remain real costs.

### Phase 5 exit gate

- Success, cancellation, browser close, callback timeout, duplicate callback, and port collision pass in release binaries.
- State, nonce, PKCE, issuer, audience, expiry, and unknown-key failures fail closed.
- Restart, refresh, logout, vault separation, and secret deletion pass native inspection.
- D1 backup, encrypted export, schema migration, rollback, and clean restore preserve `AccountId` links.
- OIDC and entitlement signing-key rotation pass with supported old clients.
- Email OTP delivery failures and abuse limits resolve to actionable states.
- Purchase, renewal, failure, recovery, cancellation, refund, duplicates, and out-of-order billing events reconcile correctly.
- Offline lease, expiry, warning, enforcement, rollback, and multi-window publication pass.
- Account and plan refresh never recreate a provider session.
- Logs contain no credentials, tokens, raw identity payloads, webhook bodies, or payment details.
- All workspace gates and live market paths still pass.

## Delivery discipline

- Work directly on `main` unless the maintainer requests otherwise.
- Complete and verify one coherent batch before the next.
- Use the existing `type(scope): outcome` commit style.
- Replace obsolete paths instead of retaining compatibility layers.
- Add no crate, runtime, framework, feature flag, service layer, or queue unless a phase requirement genuinely needs it.
- Run focused checks during implementation and all workspace gates before each phase is declared complete.
- Compilation is not proof for streaming, persistence, IPC, lifecycle, rendering, browser callbacks, billing reconciliation, or offline entitlement behavior.
- Record independently reproducible baseline failures precisely; never suppress or hide them.

## Authoritative references

- [RFC 8252 - OAuth 2.0 for Native Apps](https://www.rfc-editor.org/rfc/rfc8252)
- [Better Auth OAuth 2.1 Provider](https://better-auth.com/docs/plugins/oauth-provider)
- [Better Auth database documentation](https://better-auth.com/docs/concepts/database)
- [Better Auth Cloudflare D1 support](https://better-auth.com/blog/1-5)
- [Better Auth MIT-licensed repository](https://github.com/better-auth/better-auth)
- [Cloudflare Workers and D1 pricing](https://developers.cloudflare.com/workers/platform/pricing/)
- [Stripe subscription Checkout](https://docs.stripe.com/payments/checkout/build-subscriptions)
- [Stripe webhook handling](https://docs.stripe.com/webhooks)
- [Stripe customer portal](https://docs.stripe.com/customer-management)
- [Dodo Payments documentation](https://docs.dodopayments.com/)
- [Cloudflare R2 consistency](https://developers.cloudflare.com/r2/reference/consistency/)
- [Cloudflare Workers secrets](https://developers.cloudflare.com/workers/configuration/secrets/)

## Final approval statement

Approve phases 1-4 as the primary architecture migration. Authentication work is explicitly deferred. After phase 4 passes every exit gate and receives maintainer approval, execute phase 5 using Axiusflow-operated Better Auth, provider-neutral OIDC on the native side, replaceable billing adapters, signed offline entitlements, and gradual enforcement.
