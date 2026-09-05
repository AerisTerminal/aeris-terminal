# Trading-platform correctness and qualification plan

Updated: 2026-09-06. This is the remaining-work plan, not a completion report.
The immediate objective is trustworthy market data and reliable account/market
lifecycle behavior through the running terminal. Feature expansion must not
outrun qualification of the paths it depends on. Production installer/update
publication remains on hold until explicit maintainer acceptance.

## Evidence baseline and status rules

The examination used native revision `9c9fb8a`; this plan refresh also inspected
website revision `eb24514` in `C:/Users/devraj/Downloads/axiusflow-website`.
Recheck both working trees and revisions before implementation. Existing branch
names, running processes, deployment state, and previous evidence are not proof
that a candidate is current.

The native examination counted 125,898 physical lines of tracked Rust, including
comments and blanks. Approximately 39,000 lines are tests/diagnostic conformance
and 87,000 are other code. This is not solely Coinbase streaming: desktop,
resident engine, Rithmic, storage, IPC, platform support and Nucleus host code
account for most of it. These approximate counts are context, not a deletion
quota or a measure of maturity.

During that examination, formatting, workspace clippy, workspace build and
workspace tests passed: 875 passed, 9 ignored. A temporary additional test of
live stream -> OfflineSuspended -> Warm failed to restart the provider for
existing chart demands. It was removed after diagnosis; permanent regression
coverage is still owed. No release desktop/browser reproduction or production
Rithmic qualification was performed. A green existing suite does not close this
failure or certify the current terminal.

Use these distinctions for every item below:

- **Reproduced:** a failing behavior was exercised; record the exact scope.
- **Code-confirmed:** the control path is present; production impact may still
  need reproduction.
- **Risk/reported:** a plausible defect or maintainer report needing isolation;
  do not promote it to a proven root cause.
- **Implemented, unqualified:** source exists but required behavior evidence is
  incomplete. Keep its acceptance checkbox open.
- **Accepted:** the final revision passes the item's deterministic and applicable
  native/live checks, independent review is resolved, and the maintainer accepts.
- **Externally blocked:** record the unavailable prerequisite, supported partial
  evidence, exact remaining check, and who can supply it. Never count it as pass.

Do not remove an unresolved issue because another test passed. Close an item
only with linked evidence for its acceptance criteria; keep remaining limitations
visible in the release/PR record. This register is not an exhaustive defect audit:
new findings enter it with severity, owner, reproduction and acceptance criteria.

## Trading-data acceptance standard

An unexplained wrong price/volume, missing covered bar, duplicate contribution,
retired-state mutation or stale feed presented as live blocks acceptance of the
affected capability. A later repair does not erase an earlier incorrect live
presentation; its provisional status, correction and time to recovery must be
observable and tested. Provider revisions, sparse sessions and rounding rules
must be explicit in expected results, never blanket tolerances for mismatches.
Availability and accuracy are separate: neither a moving chart nor a responsive
login establishes data correctness. Every injected failure needs a measured,
bounded recovery or actionable terminal outcome, including when no events arrive.

## Priority and defect register

P0 blocks acceptance of affected market-data or account behavior. P1 blocks
broader production qualification. Source paths are relative to the native root
unless prefixed with `website:`.

| ID | Priority / evidence | Finding and correction owner | Required closure |
| --- | --- | --- | --- |
| MD-01 | P0 / reproduced in engine fixture | Sign-out clears live handoffs; normal post-login Warm mode does not reconstruct existing chart streams. Owners: `apps/engine/src/market_service/coordinator.rs`, `realtime.rs`, `apps/engine/src/main.rs`. | Existing authorized demands resume automatically, publish fresh covering data and advance across a candle boundary without chart reselection. |
| LC-01 | P0 / code-confirmed | Account market gate marks an authentication transition handled even if applying resource mode fails. Owner: `apps/engine/src/main.rs::start_account_market_gate`. | Applied state is acknowledged, failures remain recoverable with bounded retry, and a retired retry cannot override newer account/lifecycle state. |
| MD-02 | P0 / code-confirmed risk | Coinbase history arrival time is used as a trade-coverage cutoff; buffered trades before arrival can be absent from the history response yet discarded. Owners: `market_service/runtime.rs`, `history.rs`. | Handoff establishes actual coverage without missing or double-counting trades; response latency and clock skew cannot establish false completeness. |
| MD-03 | P0 / reported, cause unresolved | Incorrect prints, large gaps and recurring streaming freezes. The referenced image has not been visually reproduced in this examination. Owners: engine canonical publication, desktop client model, `crates/ui/chart_integration`. | Isolate each symptom at provider, history, engine, IPC, client and rendered stages; prove corrected OHLCV and visible recovery against independent evidence. |
| RT-01 | P1 / code-confirmed | Initial Rithmic desktop connection failure reports Recovering then waits only for shutdown. Owner: `apps/desktop/src/rithmic_engine_client.rs`. | Real bounded reconnect or an actionable terminal state; initial failure/retry resumes the requested catalog/series without restarting the app. |
| QA-01 | P0 / code-confirmed coverage gap | Account-switch fixtures omit market resumption; live soak bypasses production account gate/desktop; continuity checks do not independently verify OHLCV. Owners: engine/desktop tests and existing qualification tools. | Permanent cross-boundary regression and independent data-value checks fail on the old behavior and pass on the correction. |
| QA-02 | P1 / code-confirmed evidence weakness | `readiness_conformance.rs::recorded_gate` searches for outcome text; reports lack source/binary binding and the reader does not establish freshness. | Structured, revision-bound evidence; stale, malformed, interrupted or mismatched reports cannot qualify the candidate. |
| CX-01 | P1 / design finding | Coordinator live/history/retry/publication maps and desktop recovery paths require separate updates across transitions. | Simplify ownership in the affected path while preserving bounds, provenance and recovery; deletion is justified by replaced behavior and tests, not LOC targets. |

## Execution order and correction work

### Gate A: Restore a complete account-to-market lifecycle

Owners: engine account market gate and market coordinator; desktop account,
market workers and supervisor; Worker identity routes where behavior intersects.
Resolve MD-01, LC-01 and the corresponding QA-01 regression first.

- [ ] Add a permanent regression reproducing the already-observed sequence:
  establish a live stream, suspend, confirm stop, restore normal lifecycle mode,
  require the same existing demands to receive fresh data. It must fail before
  the fix; include it with the correction in the verified batch. Do not substitute
  disconnect/reconnect for sign-out suspension.
- [ ] Define and implement one reconciliation path from current authorized demand
  and lifecycle policy to required provider/history work. Resume must rebuild
  cleared live handoffs, arrange covering history and fence retired generations.
  Preserve user selection, viewport, pane identity and bounded retained history.
- [ ] Reconcile pending history, cancellation handles, generation state, retry
  deadlines, depth/order-flow state and queued publications together. Late
  callbacks and pre-sign-out events must not make a new account appear live.
- [ ] Distinguish desired resource mode from successfully applied mode using
  existing ownership. Retry failed application with bounded backoff/cancellation;
  do not publish successful operational state before application succeeds.
- [ ] Test account transitions interleaved with network/power changes, shutdown,
  failed/full coordinator commands and engine restart. Include a transition
  occurring between gate observations, slow acknowledgements, and rapid reversal.
  Newer sign-out must win over an older in-flight resume.
- [ ] Keep account and market readiness separate in IPC/UI. Signing in alone is
  not proof of fresh market data. Retained charts must visibly identify stale,
  loading, recovering or unavailable data until current-generation readiness.
- [ ] Through the production account gate and real IPC, verify fresh history and
  continuing updates after sign-out/sign-in, account expiry/re-authentication,
  engine replacement and account switching. Cover Warm, permitted MarketsLive,
  foreground/background/minimized windows, multiple panes and multiple windows.
- [ ] Run at least 20 consecutive same-window sign-out/sign-in cycles on the
  qualified candidate, including delayed completion and cancellation cases.
  Record cycle count and failures; no restart, manual symbol change or hidden
  retry reset may be used as recovery evidence. Zero stranded chart demands.

Exit: deterministic transition coverage plus observed running-desktop recovery
through at least the next candle boundary. User intent survives; obsolete account
and provider work cannot mutate current state. No credentials enter test fixtures.

### Gate B: Establish data-value correctness and honest gap handling

Owners: Coinbase adapter, engine history/realtime/publication, provider_history,
local_history, client model and chart integration. Resolve MD-02 and isolate MD-03.

- [ ] Capture a reproducible scenario for each wrong-print/gap/freeze report:
  provider/instrument, precision, cadence, selected range, expected/actual values,
  event timing, connection/account state and source/binary revision. Use existing
  sanitized diagnostics; retain normalized non-secret evidence, not raw payloads.
- [ ] Trace the same bar identity through decoded provider data, fetched history,
  canonical engine state, persisted history, IPC, desktop model and rendered
  chart. Determine the first incorrect stage before changing downstream code.
- [ ] Replace the local-response-time handoff assumption with a documented
  coverage rule supported by the actual provider API. Verify the pinned adapter
  and provider contract before choosing sequence/watermark or overlap handling.
  Where an exact forming cutoff is unavailable, expose provisional/incomplete
  state and perform bounded authoritative reconciliation; do not claim exactness
  or invent a watermark. No credential, sequence or timestamp validation bypass.
- [ ] Add deterministic overlap tests with independently computed expected OHLCV:
  response built before a buffered trade, delayed response, multi-page history,
  bucket roll during fetch, clock skew, duplicate/late trades, history failure,
  reconnect, stale cache, and selection/sign-out during handoff. Check every
  expected trade contributes exactly once where exact reconstruction is claimed.
- [ ] Verify price/quantity scales, timestamps, volume, rounding rules and
  provenance across Coinbase and available Rithmic data classes. Compare closed
  candles with an independent authoritative dataset/provider history after its
  publication settles; do not use the same aggregator as both implementation
  and oracle. Record expected correction windows and any provider discrepancy.
- [ ] Distinguish confirmed no-trade intervals, venue/session closures, missing
  coverage and disconnected feeds. Carry-forward candles are permitted only
  under an explicit supported convention with appropriate provenance. Never
  manufacture continuity to hide lost data or render stale prices as fresh.
- [ ] Verify exactly one forming candle, stable identity through append/revision
  and resnapshot, contiguous covered history for continuous-market intervals,
  and legitimate sparse/session-aware history for other instruments. Preserve
  viewport position and avoid duplicate/gap bars during covering replacements.
- [ ] Exercise live-edge and viewport repair timeout, exhausted retry budget,
  queue pressure and storage failure. Unresolved coverage must stay observable
  and have an actionable bounded recovery path; a silent retry exhaustion is not
  successful repair. Check the current live-edge retry path explicitly.
- [ ] Confirm persistence/restart preserves corrected values and coverage; older
  asynchronous writes cannot replace newer canonical history. Include corruption,
  interrupted writes, disk-full and unavailable-store recovery.
- [ ] Run independent OHLCV comparison and visual checks across supported fixed
  and calendar intervals, symbol switches, deep backfill and quiet/busy periods.
  Separate data gaps from viewport/axis/rendering artifacts. Keep Nucleus changes
  in its repository; host chrome, bridge and pointer behavior remain here.

Exit: reproduced symptoms have identified causes and verified corrections, or
remain explicitly open. Sequence continuity alone is insufficient. Do not claim
forming or historical accuracy beyond the evidence available from the provider.

### Gate C: Make recovery and ownership smaller and consistent

Owners: coordinator, market_engine, desktop engine workers/supervisor and Rithmic
client. Resolve RT-01 and CX-01 incrementally within proven behavior boundaries.

- [ ] Inventory authoritative state and derived state for demand, account access,
  provider generation, history coverage, forming bar and publication. Document
  ownership beside the implementation; remove redundant state only after reading
  every mutation, cleanup, restart and failure path.
- [ ] Consolidate repeated transition/recovery logic around existing owners.
  Do not introduce another service, framework, compatibility path, feature flag
  or generic state machine merely to rearrange files. Preserve public contracts
  unless replacing the obsolete path completely is necessary.
- [ ] Fix initial Rithmic client startup failure: bounded retry and cancellation,
  or a terminal error with functioning Retry. Test engine absent, delayed startup,
  IPC failure, successful recovery and shutdown while waiting. Recovery labels
  must correspond to recovery work that can actually progress.
- [ ] Review every Loading/Recovering state for its owner, next event, deadline,
  retry budget, overflow policy, cancellation and terminal/retry outcome. Exercise
  provider silence and no-event paths; UI progress cannot depend on a market wake.
- [ ] Verify symbol/timeframe/tab/layout changes do not recreate provider sessions;
  one demand owner serves consumers and one retained consumer does not starve
  another. Recovery remains provider-neutral at the desktop/IPC boundary.
- [ ] Recheck all bounds under slow UI, history pressure and reconnect storms:
  queues, retries, buffers, caches, snapshots and worker counts. Overload must
  trigger explicit recovery with enough capacity to deliver that recovery.
- [ ] Use scenario tests to protect each simplification. Report state/path removal
  and release measurements when claiming improvement; do not optimize for test
  count, shrink validation, or treat moving a large file as correctness work.

Exit: corrected paths have a single accountable lifecycle owner and tested
terminal/recovery outcomes. No broad rewrite is authorized by a LOC concern.

### Gate D: Complete authentication, account and entitlement qualification

Inspect both repositories and their AGENTS.md for every auth/billing change.
Native owners: `apps/engine/src/account_service`, `apps/desktop/src/account.rs`,
`onboarding.rs`, `main.rs`, `crates/engine_protocol/src/account.rs`.
Worker owners: `workers/auth/src/{index,auth,account,login,consent,identity,lease,
billing,webhooks}.ts`, UI and migrations.

The old plan's uncommitted-candidate snapshot and two-second polling/waterfall
instructions are superseded. Native `9c9fb8a` has 250 ms active account polling,
background lease warmup and transport reuse changes; website `4e312e2` adds
single-request bootstrap and `eb24514` adds sanitized server timings. Verify
these implementations rather than repeat them. Their existence does not close
latency, security, market readiness or deployed end-to-end acceptance.

- [ ] Verify canonical bootstrap/ensure/link and origin enforcement for signed-in,
  signed-out, expired, first-login and failing requests. Concurrent/repeated
  Google/OTP/browser/native linking must converge on one canonical account
  without overwriting paid plans or inventing identities.
- [ ] Verify truthful loading/error/retry, consent email, Continue/Cancel/Use
  another account, two browser tabs and existing browser sessions. Preserve the
  signed OAuth transaction, PKCE S256, state, nonce and literal 127.0.0.1 callback.
  Browser account changes must not silently replace the native account.
- [ ] Through real Google and Cloudflare Email Service OTP journeys verify sender
  domain and actual delivery, invalid/expired codes, resend cooldown, change-email,
  delivery failure and retry. No Resend or replacement mail provider; no OTPs,
  provider secrets, tokens or cookies in chat, fixtures, logs or evidence.
- [ ] Prove one desktop PID/window survives signed-out startup through callback,
  code exchange, issuer/audience/signature/expiry/nonce validation, account link,
  native-vault commit, sanitized IPC profile, workspace history and live data.
- [ ] Exercise immediate/duplicate callback, repeated clicks, browser reopen/close,
  cancel, timeout/retry, occupied loopback port, bad state/nonce/PKCE, unknown and
  rotated keys, revoked refresh, unavailable vault, full engine restart, and
  sign-out/account switching during every delayed startup/refresh/lease stage.
  Retired callbacks cannot resurrect identity, entitlement or market activity.
- [ ] Verify independent account progress with silent/suspended providers,
  multi-window/minimized UI, startup cancellation and worker cleanup. No UI-thread
  network/disk/process/shutdown work, extra app launch or stale-account flash.
- [ ] Reverify remote schema and migration/deployment order. The earlier session
  reported applying migrations 0005 and 0006 and successful aggregate identity
  checks; confirm present state without recreating D1 or existing identities.
  Test migration failure and already-current schema before Worker publication.
- [ ] Measure actual browser/native stage timings: user/mail time separately from
  callback, exchange/JWKS, link, vault, lease, IPC observation, workspace/history
  load and first fresh market update. Compare baseline/candidate under the same
  conditions with sample counts, median and defensible p95; fresh connections
  and reused connections remain separate. No unsanitized HAR/OAuth query capture.
- [ ] Retain provisional targets: visible feedback <=100 ms; engine final state
  to desktop display <=250 ms p95; warm dashboard bootstrap <=1 s p95; native
  approval response to usable account <=2 s p95 on the measured healthy network.
  These are unproven optimization targets, not deadlines permitting weaker
  checks. Report market-readiness time separately and all unmet targets.
- [ ] Verify Dodo test-mode checkout/portal, purchase, plan changes, renewal,
  payment failure/recovery, cancellation, refund and grace. Duplicate, delayed,
  interrupted and out-of-order webhooks must leave website plan, desktop plan,
  subscription status and signed lease consistent.
- [ ] Verify entitlement account/device binding, monotonic revisions, offline
  validity/expiry, refresh, warning/recovery, rotation, multi-window propagation
  and rollback. Confirm device/reset policy and exact feature-access policy with
  the maintainer before changing enforcement. Lease transport failure must not
  silently grant access or contradict the browser's reported account outcome.

Exit: both final native and deployed Worker revisions have matching real account,
profile, plan and access evidence, including subsequent market recovery. Resolve
policy ambiguity explicitly; do not optimize authentication by weakening it.

### Gate E: Qualify providers, tests and the actual terminal

Resolve QA-01 and QA-02 before using evidence to accept production behavior.

- [ ] Keep deterministic component tests for fixed-point/provenance/generation,
  queue, parser, crypto, storage and concurrency rules. Add real IPC integration
  through the production account gate for Gates A-D. Source-string architecture
  checks and scripted profile tests are not substitutes for behavior tests.
- [ ] Exercise release desktop and engine together. Match normalized bars and
  freshness state from engine publication through the real desktop client and
  Nucleus host to visible output. Include overflow/resnapshot, slow UI, restart,
  account transitions, multiple panes and visible loading/error/recovery states.
- [ ] Extend existing evidence producers/readers with structured validation,
  source/lockfile and executable hashes, platform/configuration, scenario,
  start/end time, duration, expected/observed values and outcome. Keep secrets
  and raw payloads out. A missing, stale, interrupted, malformed or mismatched
  run is unqualified. Historical passing files cannot approve a newer revision.
- [ ] Retain existing Coinbase live soak, but supplement its sequence/liveness
  assertions with independent values and the real account/IPC/desktop path.
  Explicitly invoke ignored applicable live tests; ordinary cargo test does not.
- [ ] Qualify Rithmic by capability and environment: discovery/catalog, precision,
  depth, historical time/calendar/tick bars, forming trades, reconnect and
  persistence. Restore missing `provider_kit/current/proto` from the unchanged
  canonical `C:/axiusflow-deps/provider-kit` before building or judging capability.
- [ ] Use native-vault test/paper credentials provisioned only through the
  interactive terminal prompter. Exactly one Rithmic Test session: engine or
  designated probe, never both. Production credentials never enter scheduled
  jobs, CI, chat, environment provisioning or the repository.
- [ ] Record supplied Test capabilities and absent prints/history separately.
  No prints is not an adapter defect or passing tick-bar evidence. Record the
  outstanding production provisioning/capabilities required from Rithmic and
  maintainer-controlled qualification once available; no production claim from
  simulated tests or kitless builds. Testable recovery must still be corrected.
- [ ] After fixes settle, run uninterrupted eight-hour Windows and Linux native
  endurance sessions with canonical-value samples, continuity, fresh-data age,
  bounded memory/queues/workers, frame pacing, cancellation and shutdown checks.
  Run separate fault campaigns for network/power loss, engine crash/replacement,
  IPC/backpressure, history/storage failure and account expiry/switching. Record
  recovery deadlines and resource budgets before each run; restart resets are
  failures, not successful endurance. Never weaken budgets after seeing results.

Exit: scenario coverage and evidence scope are explicit. No ignored live gate,
synthetic endurance run or incomplete Rithmic environment is reported as full
production qualification. Existing passing cases must survive every correction.

## Review, local gates and supported-target qualification

Follow current maintainer instructions and AGENTS.md for branch/commit authority;
this plan does not override them. At this refresh both checkouts are on
`fix/auth-session-continuity`. Preserve unrelated work and never switch, merge
or overwrite branches based only on this recorded snapshot. The earlier plan
records linked PR review and maintainer acceptance; retain that acceptance gate.

- [ ] Implement a coherent correction batch in dependency order above. Capture
  failing regression evidence before the fix and final evidence afterward.
  Stage only task-owned files; use `type(scope): outcome` commits. No force-push,
  destructive Git operation, automatic merge or intermediate push as a CI probe.
- [ ] Run focused checks while changing behavior, then the complete local gates
  after the batch settles: `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
  `cargo build --workspace --all-targets --all-features`;
  `cargo test --workspace --all-features`, including architecture/naming checks.
  Fix introduced failures; report reproducible baseline failures precisely.
- [ ] For affected Worker code run typecheck, tests and UI build, syntax-check and
  exercise emitted scripts, and run applicable marketing-site build/type checks.
  A plan-only edit requires documentation/diff validation, not a new live session
  or deployment. Previous code gates do not become new behavioral evidence.
- [ ] Build release binaries for behavior changes; record paths/hashes/PIDs and
  release/install generation. Replace only the identified old pair through normal
  lifecycle. Verify both actual running binaries before any live/visual claim.
- [ ] Produce linked native/Worker PRs where applicable with deployment order,
  revisions, exact commands/results, before/after scenario evidence, limitations
  and open IDs. Independently review real call paths and reproduce relevant
  results; green CI or the implementer's summary alone is insufficient.
- [ ] Batch the final qualified commits and push once within authorized workflow.
  Then enable and dispatch only the necessary manual self-hosted Windows/Linux
  workflows. Validate changed workflow YAML with a real parser. No GitHub-hosted,
  automatic push/PR/scheduled runs, global Git credentials or pushes during lanes.
- [ ] Keep the Linux VM shut down during development; start it only for deliberate
  qualification and shut it down gracefully immediately afterward. Windows
  runner is interactive for vault parity. After runs, disable workflows again
  and shut down runners. Follow AGENTS.md for runner paths, labels and SSH setup.
- [ ] Audit guarded macOS browser/Keychain/loopback/IPC/LaunchAgent/filesystem and
  shutdown paths; run warning-denied cross-target checks where tooling permits.
  Record SDK/toolchain blockers. Without Apple hardware, native rendering,
  authentication, install, restart and power behavior remain explicitly deferred;
  do not add an unusable required lane or claim native qualification.
- [ ] Resolve independent review findings against final revisions. The maintainer
  accepts the batch; acceptance does not automatically lift the release hold.

## Operations and production release hold

- [ ] Verify encrypted D1 export and clean restore, migration recovery, identity
  preservation, OIDC/lease key overlap for older clients, abuse/rate limits,
  cookie/CSP/redirect policy, mail failure, privacy/deletion/retention, incident
  response, and capacity/cost monitoring before wider production acceptance.
- [ ] Obtain explicit maintainer resolution of all applicable P0/P1 findings and
  external capability limitations. Do not imply this data/auth qualification
  certifies order routing, execution or risk controls outside the tested scope.

Do not publish or activate production installers, automatic updates, signed
release discovery or rollouts until the maintainer explicitly lifts the hold
following authentication and wider platform validation. The fixed manual
early-access ZIP is not production installer/update evidence.

After that approval:

- [ ] Package immutable matching desktop/engine/assets with signed manifests,
  hashes/inventory, platform/architecture and downgrade policy; finish release
  metadata, discovery, download and stable-launcher activation.
- [ ] Verify interruption, invalid signatures, corruption, disk full, activation
  crashes, locked executables, stale sockets, mismatched releases, readiness
  failure, rollback and reboot-required cases through installed binaries.
- [ ] Verify update locking, bounded shutdown, atomic activation, matching IPC,
  restored workspace and provider readiness. One release remains active with
  correct autostart and no superseded staging/cleanup journals or processes.
- [ ] Verify fresh install, multi-version upgrade, repeated/interrupted uninstall
  and absence audits. Remove only validated owned paths, processes, registrations,
  IPC artifacts and vault entries. Native sign-out/uninstall must not delete the
  remote account. Obtain supported-target acceptance before publication.
