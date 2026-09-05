# Remaining work and verification

Updated: 2026-09-05. Authentication and current market-data regressions remain
open. This plan contains unfinished work and acceptance checks, not completed
phase descriptions or execution history. Remove items only when their required
evidence passes; retain evidence in the PR. Compilation alone is not completion.

## Dedicated branches and PR acceptance

The maintainer explicitly replaces the direct-to-main workflow for this work:

1. Create a dedicated task branch before editing either repository. Both current
   checkouts are now on `fix/auth-session-continuity`.
   Continue these branches and preserve the uncommitted candidate changes.
2. The implementation agent completes the behavior and verification below.
   Use focused checks while iterating; run expensive complete gates after the
   implementation settles. Repeat them only for relevant changes or failures.
3. Commit a coherent verified batch per repository using `type(scope): outcome`,
   stage only task files, and push the dedicated branches. Do not push directly
   to main, force-push, or merge automatically.
4. Open a PR to main in each changed repository and cross-link them. Describe
   the combined native/Worker behavior and deployment order. Include exact
   commands/results, source revisions, executable identities, live observations,
   and remaining limitations. Unverified work is not ready for acceptance.
5. After local qualification, run the necessary manual self-hosted Windows/Linux
   lanes against the final branch revisions. Enable only the required workflow,
   dispatch once, wait, then disable it and shut down runners/the Linux VM.
   Do not push while lanes run. No GitHub-hosted or automatic workflows.
6. The reviewing agent independently reviews both PRs: inspect actual call paths,
   check evidence against final revisions, reproduce relevant failures/fixes,
   and verify end-to-end behavior. CI and the implementer's summary alone are
   insufficient. Return findings for correction and re-review the final changes.
7. The maintainer accepts and merges the PRs after review. Acceptance does not
   automatically lift the production installer/update hold.

Use the implementation agent for implementation and evidence gathering; reserve
the more expensive review session for completed PRs and unresolved findings.
This workflow can control cost but does not guarantee savings or correctness.

## Handoff for the interrupted authentication fix

Inspect both repositories and their AGENTS.md before continuing:

- Native: C:\Users\devraj\Downloads\axiusflow-gpui.
- Website/control plane: C:\Users\devraj\Downloads\axiusflow-website.
- Native owners: apps/desktop/src/account.rs, onboarding.rs, main.rs,
  apps/engine/src/account_service, and crates/engine_protocol/src/account.rs.
- Worker owners: workers/auth/src/index.ts, auth.ts, account.ts, login.ts,
  consent.ts, and workers/auth/migrations.

Uncommitted candidate changes require completion and verification:

- Native main.rs/onboarding.rs remove the post-login process relaunch, mount the
  terminal in the existing window, initialize market workers in the background,
  provide workspace loading/retry, and poll account presentation independently
  of market-event wakes.
- Worker package.json adds checks/tests/UI build/migration before deployment.
  account.ts separates account-load failure from signed-out UI.
  consent.ts displays the selected email and provides account switching while
  retaining the signed OAuth transaction.

Resume from these observations without treating them as end-to-end proof:

- The deployed D1 database lacked existing migrations 0005_billing_reconciliation.sql
  and 0006_subscription_interval.sql. The website query reproduced
  "no such column: billing_interval". Both migrations were applied remotely and
  the query then succeeded. Do not recreate the database or account identities.
- Aggregate remote checks found no duplicate identity subjects, orphaned account
  links, or duplicate normalized emails. Browser/native identity consistency
  still needs real proof; independent cookie and native-refresh lifetimes alone
  do not establish duplicate accounts.
- Focused desktop check, clippy, and tests passed after the native candidate edits.
  Worker tests/typecheck passed before the latest account/consent edits and must
  run again. Full workspace gates, new release binaries, live verification, and
  macOS checks remain outstanding. No candidate source change was deployed,
  committed, or pushed.
- Existing running release binaries predate the candidate changes. Identify
  their paths/PIDs again before replacement; do not use them as fix evidence.

## Current-session completion and verification

These are the checks still owed for the interrupted session. The implementation
agent must finish them before asking for acceptance, or record an exact blocker
and leave the affected items open.

### Website and canonical identity

- [ ] Review and finish candidate changes, especially failure recovery, loading,
  startup/sign-out races, cleanup, and retired completion fencing.
- [ ] In a real signed-in browser verify /api/axiusflow/ensure returns 200 with
  the correct profile, canonical account ID, plan, and subscription. Verify
  unauthenticated 401 and origin enforcement.
- [ ] Test first-login and concurrent/repeated browser/native linking: one
  verified identity converges on one canonical account without overwriting a
  paid plan. Google and OTP for the same verified identity must agree.
- [ ] Keep account loading visible until the canonical response arrives. Inject
  timeout, network, and 500 failures: one persistent recovery state, no partial
  dashboard or false sign-out; Retry must recover without another account.
- [ ] Verify consent's actual account email, Continue, Cancel, and Use another
  account against the pinned provider implementation. Preserve signed query,
  PKCE, state, nonce, and callback. Failed session checks cannot enable approval.
- [ ] Compare website and running-desktop identity/account/plan after login and
  switching accounts, including two browser tabs and an existing browser login.
  Verify and explain session/sign-out lifetimes; do not silently switch the
  desktop account when another browser session changes.
- [ ] Exercise real Google login and real Cloudflare Email Service OTP delivery.
  Verify sender-domain configuration, wrong/expired codes, resend cooldown,
  change-email, delivery failure, and retry. Keep secrets/OTPs out of chat/logs.
- [ ] Verify deployment fails before publication if checks/migrations fail, works
  with an already-current schema, and deploys the final tested Worker/UI.
  Confirm no pending migrations and repeat browser checks on auth.axiusflow.com.

### Same desktop session and native authentication

- [ ] Build release desktop/engine; record source/lockfile identity, executable
  paths/hashes, PIDs, and engine release/install generation. Replace only the
  identified old pair through the normal lifecycle.
- [ ] From signed-out cold start prove the same desktop PID and native window
  survive login. Observe browser waiting, workspace loading, history loading,
  and streaming. No second app, UI-thread I/O, or relaunch workaround.
- [ ] Prove callback success follows code exchange, signature/issuer/audience/
  expiry/nonce checks, canonical linking, vault commit, and sanitized IPC profile.
- [ ] Exercise immediate callback, repeated clicks, browser reopen, cancel,
  browser close, timeout/retry, duplicate callback, state/nonce/PKCE mismatch,
  occupied callback port, and unknown/rotated signing keys.
- [ ] Exercise startup failure/retry, closing onboarding during startup, and
  sign-out/account changes while startup is pending. Retired work must not attach
  stale state, leak workers, block the UI, or launch another app.
- [ ] Verify account polling with silent/suspended providers and idle/minimized
  windows. Resume drains retained market state; polling is bounded and stops
  with the window.
- [ ] Verify token refresh, full engine restart restoration, account switching,
  active sign-out, sign-out during refresh, vault cleanup, revoked refresh, and
  unavailable-vault recovery. Retired completions cannot resurrect identity.
- [ ] Verify multi-window account state and lifecycle, including sign-out and
  sign-in again without restarting the application.

### Market data and recovery

- [ ] Reproduce and eliminate the need to reload/change symbol after login.
  Repeated cold starts/logins must automatically load Coinbase history and keep
  streaming through the next candle boundary without reselection.
- [ ] Verify rapid symbol/timeframe/tab/layout changes preserve provider sessions
  and reject retired publications.
- [ ] Verify order-book recovery, viewport backfill, deep history, one canonical
  forming candle, contiguous history/live handoff, persistence, and restart.
- [ ] Exercise offline startup, network loss/restoration, provider silence,
  account expiry/re-authentication, engine replacement, IPC pressure, mailbox
  overflow, and shutdown. Loading must resolve to data or actionable recovery.
- [ ] Exercise available Rithmic engine behavior using the external Provider Kit
  (restore from C:\axiusflow-deps\provider-kit if necessary), native-vault test
  credentials, and exactly one session. The Test feed's absent prints/history
  remains an external limitation, not passing bar/streaming evidence.

### Automated and platform qualification

- [ ] Add meaningful regression coverage for reproduced failures and changed
  lifecycle/recovery behavior; source-string assertions alone are insufficient.
- [ ] In workers/auth run npm run typecheck, npm test, and npm run build:ui.
  Syntax-check and exercise emitted account/consent scripts. Run applicable
  marketing-site typecheck/build gates.
- [ ] Run cargo fmt --all -- --check.
- [ ] Run cargo clippy --workspace --all-targets --all-features -- -D warnings.
- [ ] Run cargo build --workspace --all-targets --all-features.
- [ ] Run cargo test --workspace --all-features and architecture/naming checks.
  Fix task regressions and report independently reproducible baseline failures.
- [ ] Complete relevant native Windows/Linux release and final manual self-hosted
  qualification. Preserve prior accepted platform evidence while re-verifying
  behavior changed by this batch.
- [ ] Audit guarded macOS browser launch, Keychain, loopback/Unix IPC, LaunchAgent,
  lifecycle, filesystem, and shutdown paths. Run available Apple-target checks,
  including warning-denied axiusflow_platform_runtime checks where possible;
  record concrete SDK/toolchain blockers. No Apple hardware exists: native
  rendering, login, restart, install, and power-transition qualification remain
  deferred. Keep source free of known defects without claiming tested support
  or creating an unusable required CI lane.
- [ ] Attach final evidence to both PRs, resolve independent review findings,
  and leave acceptance/merge to the maintainer.

## Remaining production qualification beyond this fix

- [ ] Billing: Dodo test-mode checkout, portal, plan changes, purchase, renewal,
  failure/recovery, cancellation, refund, and grace. Test duplicate, delayed,
  interrupted, and out-of-order webhooks. Website, desktop, subscription, and
  signed lease must agree.
- [ ] Entitlements: account/device binding, monotonic revisions, offline validity,
  expiry, refresh, warning/recovery, key rotation, multi-window propagation, and
  rollback. Confirm device/reset policy and the precise feature boundary before
  further enforcement; plan changes must preserve provider sessions and history.
- [ ] Operations: encrypted D1 export/clean restore, migration recovery, identity
  preservation, OIDC/lease key overlap for old clients, abuse/rate limits,
  cookie/CSP/redirect policy, email failures, privacy/deletion/retention,
  incident response, and capacity/cost monitoring.
- [ ] After the completed candidate stabilizes, run uninterrupted eight-hour
  Windows/Linux endurance and fault campaigns with provenance, bounded resource
  growth, continuity, recovery, frame pacing, worker cleanup, and shutdown.

## Production installer and update hold

Do not publish/activate production installers, automatic updates, signed release
discovery, or rollouts until the maintainer explicitly lifts the hold following
authentication and wider platform validation. The fixed manual early-access ZIP
is not production installer/update evidence.

After that approval:

- [ ] Package immutable matching desktop/engine/assets with signed manifests,
  inventory, hashes, platform/architecture, and downgrade policy. Finish release
  metadata, discovery, download, and stable-launcher activation.
- [ ] Verify interrupted downloads, invalid signatures, corruption, disk full,
  transaction-boundary crashes, locked executables, stale sockets, mismatched
  releases, readiness failure, rollback, and reboot-required cases.
- [ ] Verify update locking, bounded shutdown, atomic activation, matching IPC
  identity, workspace restore, and provider readiness. Success leaves one active
  release, correct autostart, and no superseded binaries/staging/cleanup journals.
- [ ] Verify fresh install, multi-version upgrade, interrupted/repeated uninstall,
  and absence audits through installed binaries. Remove only validated owned
  paths, processes, registrations, IPC artifacts, and vault entries; native
  sign-out/uninstall must not delete the remote account.
- [ ] Obtain maintainer acceptance of supported-target release evidence before
  production publication.
