# Axiusflow Architecture and Authentication Migration Plan

Status: phases 1 through 3 implemented; cross-platform release qualification and phases 4 through 5 remain incomplete

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
- signed, transactional release delivery that activates one matching desktop/engine build and removes the superseded build;
- a complete native uninstall that removes every Axiusflow-owned local artifact and credential;
- equal end-to-end product support and release qualification on Windows, macOS, and Linux;
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
- The installed desktop and resident engine always come from one verified release identity. A stale engine, autostart entry, launcher target, or executable cannot remain active after an update.
- An update is not complete until the new release passes a desktop/engine handshake and superseded binaries and staging files are removed. An uninstall is not complete while any Axiusflow-owned process, service registration, credential, market-data file, cache, setting, log, or update artifact remains.
- `unsafe_code` remains forbidden workspace-wide.

## Cross-platform support is a product requirement

Windows, macOS, and Linux are equal, first-class Axiusflow targets. The intended outcome is not
source compatibility, successful cross-compilation, or a desktop window that opens on all three
platforms. It is complete end-to-end support for the real installed product on every target:

- the packaged desktop starts the matching resident engine and authenticates over the native IPC
  transport;
- provider sessions, live publications, history, workspace restoration, and reconnect recovery
  behave identically at the application-contract boundary;
- credentials, autostart, install, update, rollback, and complete uninstall use correct native OS
  facilities and survive interruption;
- window controls, input, scaling, multi-monitor movement, rendering, and frame pacing meet the same
  acceptance standard;
- offline startup, online/offline transitions, suspend/resume, display changes, process replacement,
  desktop close, user sign-out, and operating-system shutdown resolve safely;
- bounded queues, cancellation, generation fencing, security, and data-integrity invariants remain
  intact; and
- diagnostics and actionable terminal errors are available on every target without exposing secret
  material.

A change that passes Linux CI while breaking or leaving Windows or macOS unverified is not an
acceptable development result. Neither "works on Linux" nor "compiles on all targets" is evidence
of cross-platform support. Platform-specific code, tests, packaging, and physical validation are
part of the feature itself and must land in the same completed batch. A target may be called
supported only when its required automated and native release gates pass.

### Required development policy

- Every pull request and push to `main` runs formatting, clippy, build, and deterministic workspace
  tests on Windows, macOS, and Linux. All three jobs are required and none may be represented by a
  cross-compile-only substitute.
- Any change to GPUI, Nucleus Charts, IPC, filesystem persistence, process lifecycle, credential
  storage, networking, power handling, packaging, or native window behavior must include the
  relevant target-specific tests and native release verification.
- Tests may be platform-specific where the OS contract differs, but fixtures and assertions must
  use valid native paths, executable names, error semantics, and lifecycle behavior. A Unix fixture
  running under `cfg(windows)` is not Windows coverage.
- Required native tests cannot remain permanently ignored. Credentialed or physical tests may run
  in scheduled/self-hosted lanes, but their most recent provenance-bound result must be available
  and current before a release is approved.
- A failing target blocks completion. Do not weaken, suppress, skip, or relabel the failure as a
  platform limitation unless the maintainer explicitly removes that target from product support.
- Platform parity is evaluated at the user-visible contract. Implementations should use the native
  mechanism appropriate to each OS rather than forcing one OS's mechanism onto the others.

## Cross-platform stabilization plan

### Current Windows audit - 2026-09-03

The Windows workspace compiles, and native display enumeration, DWM timing, credential-vault,
power, network, local-history, and architecture checks have substantial coverage. It is not yet a
qualified release target. The audit found these concrete blockers:

1. The local-engine client falls back from unsupported named-pipe receive timeouts to
   `PIPE_NOWAIT`, then interprets a zero-byte empty read as EOF. Windows connection, replacement,
   shutdown, and desktop supervisor tests fail deterministically with premature connection closure
   or `BrokenPipe`. This can surface as intermittent startup, reconnect, and initial-market-state
   failures in the real product.
2. Transactional lifecycle code opens and synchronizes directories using Unix filesystem
   semantics. Windows returns `StagingFailed`, causing the install, update, rollback, recovery, and
   uninstall lifecycle suite to fail before those guarantees can be established.
3. One autostart test uses an absolute Unix path even when compiled on Windows. It therefore fails
   before exercising the Windows launcher contract and does not constitute Windows coverage.
4. The required CI workflow runs only on Ubuntu. Deterministic Windows regressions can merge to
   `main` without being observed, and there is no equivalent required macOS lane.
5. Native installed-binary, provider, physical transition, physical scanout, window-control, and
   eight-hour endurance probes are not part of the required continuous gate. Tooling alone is not
   evidence that the product passed.
6. The Windows renderer uses GPUI's Direct3D 11 and DirectComposition path, while Linux uses a
   different backend. Recent GPUI and Nucleus revisions have not been qualified through a complete
   Windows physical-pacing and endurance matrix. Hybrid and virtual display adapters must be
   included because adapter selection and device-loss behavior can differ materially from a
   single-GPU machine.

The audit baseline was a clean `main` worktree at `86bf390`. `cargo check --workspace
--all-targets --all-features` passed. The Windows desktop tests passed 137 of 139, with both failures
in resident-engine reconnect/restore. `axiusflow_platform_runtime` passed 31 of 41, with the ten
failures in lifecycle/autostart coverage. `axiusflow_local_engine_client` had one deterministic IPC
failure and five ignored installed-engine probes. Local storage passed 23 of 23 tests and the
architecture checker passed 31 of 31. These results are a failure baseline, not release evidence.

### Confirmed Windows IPC reproduction - 2026-09-03

The desktop launch failure was reproduced from a clean engine process with debug and release
binaries:

```text
Axiusflow market worker could not start: ipc_receive failed: local engine connection closed
```

The resident engine starts and remains alive. The raw synchronous local-socket handshake passes,
but the real `EngineClient` framed handshake fails deterministically on Windows. The same failure
appears in `apps/engine/tests/handshake.rs` when an authenticated client connects and requests the
engine-owned workspace. This isolates the defect to the Windows local IPC session lifecycle after
transport connection and before the first authenticated command reply.

The boundary is `crates/local_engine_client` and the matching `apps/engine` `FramedConnection`:
the client uses a split stream with a background reader, while the engine uses a split stream with
a background writer. Windows named-pipe behavior is not currently equivalent to Unix socket
behavior at this boundary. A trial nonblocking-only change did not resolve it and was reverted;
retry and timeout workarounds are not considered fixes.

The proper remediation is an explicit Windows transport contract covering framed read/write,
readiness, temporary no-data, peer closure, half-close, cancellation, and writer shutdown. It must
preserve a duplex session through `EngineReady`, authenticated attach, workspace restore, the first
market command, reconnect, replacement, and shutdown acknowledgement. Add a native Windows
regression test for that complete sequence and run the same contract on macOS and Linux before
release approval.

### Stabilization work order

1. **Make IPC correct on Windows.** Replace the `PIPE_NOWAIT`/zero-read polling behavior with a
   cancellable Windows transport strategy that distinguishes temporary lack of data from peer
   closure. Preserve bounded shutdown. Add repeated handshake, duplex command/reply, burst,
   disconnect, reconnect, incompatible-engine replacement, and shutdown-acknowledgement tests.
2. **Make lifecycle durability native.** Define the durability guarantee per platform. Use correct
   Windows file flush, atomic replacement, sharing, reparse-point, locked-file, and reboot-required
   behavior instead of attempting Unix directory `fsync`. Repair every lifecycle fixture to use
   native executable names and paths.
3. **Establish the three-OS CI matrix.** Add required Windows and macOS jobs alongside Linux for the
   complete deterministic workspace gates. Keep platform-specific failures visible and fail the
   workflow if any target is skipped unexpectedly.
4. **Qualify installed release pairs.** Build release packages on each OS; launch the packaged
   desktop; verify its executable identity and the matching engine release/generation; exercise
   authenticated IPC, workspace restoration, Coinbase, available credentialed Rithmic, clean
   shutdown, relaunch, update, rollback, and uninstall.
5. **Qualify native transitions.** On each target, capture offline startup, loss and restoration of
   network availability, suspend/resume, display disconnect/reconnect, DPI and monitor changes,
   desktop close modes, session sign-out, and OS shutdown. Require data continuity or explicit
   bounded recovery after every transition.
6. **Qualify rendering and input.** Exercise native window controls, IME, keyboard, pointer,
   drag/resize, fullscreen, multi-monitor movement, mixed DPI, and long chart/DOM interaction. On
   Windows capture external physical scanout at 60, 120, and 144 Hz, including single-GPU,
   hybrid-GPU, and virtual-display configurations. Equivalent platform-appropriate pacing evidence
   is required on macOS and Linux.
7. **Run endurance and fault campaigns.** Complete at least eight continuous hours per OS with the
   real release desktop, resident engine, and live public market path. Inject bounded IPC pressure,
   provider silence, reconnects, process replacement, disk-full/locked-file failures, and device
   loss where supported. Record memory high-water marks, queue overflow/recovery, frame pacing,
   worker shutdown, and terminal errors.
8. **Prevent recurrence.** Make all automated gates required for `main`, retain provenance-bound
   physical and credentialed results for release approval, and add architecture checks that reject
   unguarded Unix-only filesystem or process assumptions in shared platform code.

### Windows stabilization execution record - 2026-09-03

Work-order items 1 through 3 are implemented on `main` at `9ba934c`; items 4
through 8 remain open, so the cross-platform exit gate below is still closed
and phase 5 remains blocked.

- Item 1: the split-stream IPC is replaced by paired sessions. Each session
  owns a write-only command stream and a read-only event stream correlated by
  `session_nonce` and `StreamRole` in `ClientHello` (`PROTOCOL_VERSION` 16, a
  deliberate wire change). No stream is ever split, so a blocking read never
  shares a transport handle with a concurrent write; temporary no-data is
  distinguished from peer closure on every platform, handshakes are bounded,
  and a legacy single-stream resident shuts down over the raw command stream
  before the replacement starts. Regression coverage: repeated handshake,
  duplex command/reply burst, disconnect, reconnect
  (`framed_session_survives_repeated_handshake_burst_and_reconnect`),
  incompatible-engine replacement, and shutdown acknowledgement through the
  handshake, supervisor, lifecycle, and client suites. The confirmed
  reproduction above no longer occurs: the framed handshake, workspace
  restore, reconnect, and replacement paths pass deterministically on
  Windows.
- Item 2: lifecycle durability is per-platform (Unix directory `fsync`;
  Windows per-file sync plus atomic same-directory rename). Lifecycle and
  autostart fixtures use native executable names and paths.
- Item 3: `ci.yml` runs the deterministic workspace gates on Linux, Windows,
  and macOS runners. Remote CI has never passed: every push run since Aug 31
  fails all lanes at dependency fetch because `NucleusCharts/financial-charts`
  is PRIVATE and runners have no credentials (`revision 6a3ac948 not found`
  under `failed to authenticate`). Only `cargo fmt` passes remotely. The
  lanes now run `git config url."https://x-access-token:${{
  secrets.NUCLEUS_CHARTS_TOKEN }}@github.com/".insteadOf
  "https://github.com/"` after checkout (all three `ci.yml` lanes plus the
  scheduled coinbase live-gate, which fails on the same fetch); the secret
  does not exist yet, so the maintainer must create a read-only token for
  the charts repo and add it (`gh secret set NUCLEUS_CHARTS_TOKEN --repo
  Axiusflowhq/Axiusflow_GPUI`). Until then remote lanes stay red and only
  local gates count. The scheduled rithmic live-gate additionally needs a
  self-hosted `[rithmic-credentials]` runner that does not exist (it queues
  to timeout); that is maintainer infrastructure, still open.

  Correction after local proof (same day): the secret did land and the
  `insteadOf` step runs green, but all lanes still failed auth — and the
  token was exonerated by experiment. Forced-clone probes on this machine
  (cargo 1.97.1) proved cargo's built-in git client does not use
  `url.insteadOf`-embedded credentials: with a bogus token armed, the fetch
  silently succeeded through the system Git Credential Manager instead of
  failing. The rewrite only takes effect through the git CLI, proven the
  same way (`CARGO_NET_GIT_FETCH_WITH_CLI=true` + bogus token fails with
  "Invalid username or token"). The fix is therefore job-level
  `CARGO_NET_GIT_FETCH_WITH_CLI: "true"` on all three `ci.yml` lanes plus
  the scheduled coinbase gate, keeping the `insteadOf` step. Local machine
  state was restored pristine afterward (global gitconfig empty, cargo git
  db intact, experiment artifacts removed).

  Mechanism fully proven on run `33711196341` (same day): the CLI fetch is
  active on all lanes, the public zed dependency clones fine through it,
  and GitHub evaluates the rewritten credential and rejects it verbatim:
  "remote: Invalid username or token." So the workflow is now correct and
  the only remaining variable is the stored secret value itself — it is
  garbled, scope-less, expired, revoked, or minted on the wrong account
  (a valid-but-unauthorized credential would 404, not fail auth). The
  maintainer must store a working classic `repo`-scoped value; a
  validate-then-store script (API check first, `gh secret set` only on
  HTTP 200) was provided to make the next attempt foolproof. No further
  workflow change is needed; rerun the failed jobs after the secret lands.
- Item 8 (partial): the architecture check pins the deliberate protocol
  revision and now rejects unguarded Unix/Windows filesystem and autostart
  assumptions in shared platform code
  (`platform_filesystem_assumptions_remain_explicitly_guarded`: `std::os`
  imports, ownership APIs, signals, and native autostart literals require
  explicit `cfg` guards; platform absolute paths and hardcoded Axiusflow
  `.exe` names are rejected). Provenance-bound physical or credentialed
  results are still missing.

Verification on Windows (this machine, commit `9ba934c`):

- `cargo fmt --all -- --check`, workspace clippy with warnings denied,
  workspace build, and `cargo test --workspace --all-features` all pass.
  Desktop 139 of 139 (both reconnect/restore baseline failures resolved),
  `axiusflow_platform_runtime` 41 of 41, `axiusflow_local_engine_client`
  4 of 4, handshake 17 of 17, architecture checks 31 of 31.
- The release desktop and resident engine built from the same tree were
  launched and probed over the real paired IPC: engine status reported two
  providers and 1082 retained bars, and a Coinbase BTC-USD snapshot returned
  581 bars. Rithmic, installed packaging, transitions, rendering, endurance,
  and uninstall evidence required by the exit gate are still outstanding.

Item 8 follow-up (same day, this batch): the Unix-guard architecture check
above is implemented with no production behavior change. Verification on
Windows (this machine): `cargo fmt --all -- --check`, workspace clippy with
warnings denied, workspace build, and `cargo test --workspace --all-features`
all pass; architecture checks 32 of 32, desktop 139 of 139,
`axiusflow_platform_runtime` 41 of 41, handshake 17 of 17. Items 4 through 7
and provenance-bound physical/credentialed results remain open, so the exit
gate stays closed and phase 5 remains blocked.

Item 3/8 follow-up (same day, this batch): deterministic CI evidence is now
symmetric across all three OS lanes. `workspace-windows` and
`workspace-macos` run the same market-data performance baseline and upload
OS-named 30-day artifacts (`market-data-performance-windows`,
`market-data-performance-macos`) as the Linux lane; the evidence JSON already
carries schema version, CPU/hardware, and OS/arch provenance. The new
architecture check `three_os_deterministic_gates_remain_required` pins the
three native runners, the four gates per lane, all three evidence artifacts,
and the absence of `continue-on-error`. Verification on Windows (this
machine): workspace gates all pass (architecture checks 33 of 33); the
release performance baseline passes locally (100k bars, first_usable 193 ms,
warm_read 338 ms). The new lanes still have to execute remotely; installed
packaging, transitions, rendering, endurance, and credentialed/physical
provenance in items 4 through 7 remain open, so the exit gate stays closed
and phase 5 remains blocked.

Phase 4 protocol review (same day, this batch): all 67 engine-protocol types
were audited for producers and consumers across `apps` and `crates`. Exactly
one message was obsolete: `ActivateExistingUi` (envelope tag 16) had no
sender and no handler anywhere in the workspace; second-desktop behavior
stays as implemented (isolated authenticated clients, update lock blocks new
processes). It is removed as a deliberate wire change: `PROTOCOL_VERSION`
17, tag 16 retired permanently beside the existing retired gaps, and the
architecture check now pins both the version and the tag-16 gap. The
`legacy_*` workspace/storage migrations and the legacy-engine replacement
path were reviewed and kept: they are live upgrade/replacement contracts
with regression coverage, not scaffolding. Verification on Windows (this
machine): workspace gates all pass (naming 33 of 33, protocol 7 of 7,
desktop 139 of 139, `axiusflow_platform_runtime` 41 of 41, handshake 17 of
17). Items 4 through 7 and remote-lane execution remain open, so the exit
gate stays closed and phase 5 remains blocked.

Phase sequencing enforcement (same day, this batch): the workspace was
audited for dormant phase-5 surface (Better Auth, Stripe, Dodo, Cloudflare,
passkeys, OIDC/PKCE, refresh tokens, billing, and the exact phase-5
`AccountId`/`PlanId`/`FeatureId`/`BeginLogin`/`AccountView` identifiers
across all Rust sources, manifests, and the dependency lock) and found
clean. The new architecture check
`phase_five_authentication_surface_stays_out_until_phase_four_passes` makes
that prohibition executable: any dormant authentication type, account state,
login UI, cloud identity networking, or billing dependency fails the gate
until the phase 4 exit gate passes and the check is deliberately retired.
Verification on Windows (this machine): workspace gates all pass
(architecture checks 34 of 34). Items 4 through 7 and remote-lane execution
remain open, so the exit gate stays closed and phase 5 remains blocked.

Live release verification of wire revision 17 (same day, this batch): a
stale pre-v17 resident engine (PID 32500, binary dated 6:07) was found
running and was stopped gracefully through authenticated IPC
(`--shutdown`, exit 0). The release engine was rebuilt from current `main`
and started resident. Over the real paired IPC, the release probes passed:
`native_release_status_probe` completed the v17 handshake against the
intended binary (PID match) reporting 2 providers, 5280 retained bars, and
`Running` shutdown state; `native_release_market_snapshot_probe` restored
the workspace, demanded the hot Coinbase series, and received a 5282-bar
BTC-USD snapshot with sequence, timestamp, close, and volume populated,
then removed its consumer. The engine was then shut down gracefully (exit
0); no Axiusflow processes remain. One state change to note: engine startup
reconciled autostart against the persisted workspace (which records
autostart disabled) and removed the stale `HKCU...Run\Axiusflow Engine`
entry that pointed at the dev-path `target\release` binary. That removal is
the designed reconciliation behavior, not a manual edit; re-enable autostart
from the desktop lifecycle settings if it is wanted. Rithmic credentialed
probes, installed packaging, transitions, rendering, and endurance remain
open, so the exit gate stays closed and phase 5 remains blocked.

Rithmic credentialed verification (same day, this batch): the maintainer
supplied R|Protocol API 0.89.0.0 from Downloads; `proto/` (156 entries,
template 5.42) was copied to gitignored `provider_kit/current/proto` with
the Downloads copy unchanged, and the release engine was rebuilt with the
`rithmic_kit` cfg verified on (kit-gated session tests compile in). Test
credentials were provisioned to the native vault
(`com.axiusflow.terminal`/`provider-rithmic-test-default-v1`) through a
throwaway helper that has since been deleted; no secret material is in the
repo, logs, or binaries. The phase-1 baseline failure was reproduced live
and root-caused: the test plant emits schema-valid `LastTrade` session/clear
markers with presence/clear bits set but no price or size, and the adapter
required both unconditionally, so `MissingField("trade_price")` tore down
every session before engine-registry routing. The fix skips content-free
marker frames at decode and read time (mirroring the existing quote
`Cleared`/`Unchanged` and `Ok(None)` conversion patterns): no price is ever
fabricated, partial trade content still fails closed, and a leftover Sep-1
per-trade stderr probe was removed. Regression cover: marker skip,
partial-absence failure, anonymous-marker rejection, and a session-level
marker-then-trade fixture. Live results on the release path: ticker login,
31-result search, instrument reference, heartbeat, quotes, and depth all
pass; a 5-minute tolerant observation saw zero prints for front-month
MNQU6/CME, and a 4-day minute-bar replay completes with zero bars
(`history_empty`), so the engine tick round trip still cannot form bars.
That absence is test-feed/account-entitlement state, not a protocol defect:
every stage that has data verifies end to end. Still open: a feed (or
entitlement) that actually carries prints/history, plus installed
packaging, transitions, rendering, endurance, and remote lanes. Builds now
silently fall back to kitless Rithmic when `provider_kit/current` is
absent; keep the kit restored before concluding Rithmic is unavailable. The
provisioned password transited chat to reach the vault; rotate it at will.

Both-paths rerun (same day, test credentials confirmed no-rotation-needed):
the engine markets-live round trip and the full smoke were each run again
against the fixed release engine. Engine path: login/search/select succeed,
then the tick-history leg fails bounded with provider `Recovering` and an
actionable "historical bars are unavailable" terminal detail — no stream
death, no poison, engine alive, lifecycle restored. Smoke path: login,
31-result search with MNQU6/CME selected, reference, heartbeat, live quotes
and depth all pass; a 5-minute tolerant watch then sees zero prints, and the
plant drops the long-idle connection (honest transport error; the smoke
sends no heartbeats while watching, unlike the product session driver).
Conclusion unchanged: the adapter is correct on every stage the test feed
populates; prints and history for MNQU6 are simply absent on this feed, so
tick bars cannot form. Still needs a data-carrying feed or entitlement
before the credentialed path can pass end to end.

Full-platform launch with Rithmic (same day): release desktop and the
kit-enabled v17 engine were rebuilt from current `main` and launched (the
running engine was stopped gracefully first, since Windows cannot replace a
running binary). Desktop PID 27524 opened a responding native window and
attached as the engine's single authenticated client (engine PID 32852,
2 providers, 5341 retained Coinbase bars and growing, `Running`). Rithmic
vault credentials are provisioned, so selecting provider Rithmic and
instrument MNQU6/CME in the UI will run login, search, reference,
subscription, and live quotes/depth through the real engine path; tick bars
and history resolve only when the test feed carries prints. Handed over
with the window open for maintainer-driven selection.

Rithmic selection diagnosis (same day): the maintainer reported the desktop
shows normal data, not Rithmic. Engine status confirms it: `coinbase:2:gen3`
(Online) but `rithmic:0:gen0` — no Rithmic session has ever started in this
engine lifetime, so no Rithmic demand has reached the engine; only Coinbase
is demanded. The desktop is fully wired for Rithmic selection
(`symbol_menu`, supervisor search/select/install, dedicated Rithmic client
modules), and the status probe now prints per-provider state to make this
visible. Next step is maintainer-driven selection in the open window while
watching the engine state change.

Rithmic entry point found (same day): Rithmic has no in-window switcher in
this build; it is a separate desktop mode launched with
`axiusflow_desktop --rithmic-test` (the instrument dialog's Rithmic
branches render only for Rithmic surfaces). A second desktop was launched
with that flag alongside the Coinbase window: single engine PID 32852,
`clients=2` with multi-client isolation holding, and the Rithmic provider
session establishing (`rithmic:1:gen6`, reconnecting at probe time). The
Rithmic window is open for maintainer-driven search and selection.

Maintainer-reported Rithmic chart error (same day): "Rithmic visible
history could not be loaded" is the desktop's honest terminal state for the
empty feed (main.rs `apply_rithmic_history` failure path keeps the previous
chart and restates demand). At probe time both desktops were closed
(`clients=0`) with providers mid-recovery (`coinbase:1:gen4`,
`rithmic:0:gen7` reconnecting). One caution for future live runs: Rithmic
test users allow a single concurrent login, so adapter-level smoke logins
can force-logout the engine's own session and churn its generations; run
either the smoke or the engine path at a time, never both at once.

Why the test feed carries no data (same day): the chain to the plant is
proven (login/search/reference/subscribe/heartbeat/quotes/depth), so the
absence is plant-side. Ranked causes: (1) the Rithmic test system publishes
no prints or history for these symbols right now (5+ quiet minutes,
empty 4-day replay); (2) the paper account's CME market-data entitlement
lapsed or never covered streaming (login/authentication is not
entitlement); (3) schedule effects on the test plant. Notably, an Aug 7
commit ("Enable entitled Rithmic CME data") shows search results then
carried `-Delayed` venue suffixes that had to be stripped for entitled
streaming, so a change in what the catalog offers is itself a signal: the
smoke now lists every search hit (symbol/exchange/expiration) for feed
characterization. Live Rithmic logins also stay single-session, so all
further live probing is maintainer-driven while the desktop holds the
session.

Actionable Rithmic terminal error (same day): phase 4 requires loading to
resolve to data, bounded recovery, or an actionable terminal error, and the
maintainer-hit "Rithmic visible history could not be loaded" was terminal
but not actionable (cause buried, no corrective action). The desktop now
maps the known empty-feed marker (`history_failure_messages` in
`rithmic_engine_history.rs`, marker produced by
`apps/engine/.../history.rs`): that case renders cause (feed published no
prints/history, no bars can form), action (check market-data entitlement,
reselect to retry), and standing state (previous chart stays live). All
other failures keep byte-identical wording; success paths untouched.
Regression cover: empty-feed message content and exact legacy wording.
Verification on Windows (this machine): workspace gates all pass (desktop
141 of 141, naming 34 of 34, zero failures). No live Rithmic login was used;
the maintainer holds the single test-plant session.

Provenance-bound gate evidence, Windows (same day, item 8): the deterministic
gates above ran against tree `d2788b3` with `Cargo.lock` SHA256
`638A13B743A23BBEC72EB8E382DADF9315BE440F1FE9CEA1961BF9D217F6B1AE` on
Microsoft Windows 11 Pro 10.0.26200, Intel64 Family 6 Model 183. Tallies:
desktop 141, naming 34, `platform_runtime` 41, handshake 17, protocol 7,
adapter 111 + smoke-bin 11, engine 106, market_data 23, observability 18,
local_history 12, storage lifecycle 22, provider_history conformance 11,
handoff conformance 3, market_engine 30; clippy workspace `-D warnings`
clean, `cargo fmt --check` clean, zero failures. The resident release pair
currently running (desktop PID 32216, engine PID 10852) was built from tree
`162e924`: engine SHA256
`B217BC973468A4F3AF0A7BFEC3AB6F2D757323BBDE77A60935786CB416669864`,
desktop SHA256
`92581A9AA1180E0DE91F103DF740C7EC735ED7D9DA46D3945250ECDE5D9A3BE7`.
Production-code drift between the build tree and `d2788b3` is exactly the
desktop message mapping (`main.rs`, `rithmic_engine_history.rs`); engine
production code is identical and the `local_engine_client` drift is inside
`#[cfg(test)]` probe code only. A same-tree release rebuild was attempted
and correctly refused by the OS: the running release desktop holds a lock
on its own executable (access denied replacing
`target/release/axiusflow_desktop.exe`), which is the expected Windows
behavior behind the no-in-place-overwrite update rule. Display make/model,
scanout rates, and physical/credentialed runs remain open under items 4
through 7.

Live-gate fetch parity (same day, this batch): the scheduled `rithmic`
live-market gate lacked the private-charts fetch contract that `ci.yml`
and the `coinbase` live gate already carry (no
`CARGO_NET_GIT_FETCH_WITH_CLI`, no `Authenticate private chart
dependency` step), so the self-hosted credentialed lane would fail at
dependency fetch even after the runner exists. It now carries the same
job-level CLI-fetch env and authentication step. The new architecture
check `live_market_gates_fetch_private_charts_through_cli` pins both
live gates to that contract so the asymmetry cannot recur.
Verification on Windows (this machine): `cargo fmt --all -- --check`,
workspace clippy with warnings denied, workspace build, and
`cargo test --workspace --all-features` all pass with zero failures
(architecture checks 35 of 35, desktop 141 of 141,
`axiusflow_platform_runtime` 41 of 41, handshake 17 of 17, protocol 7
of 7, engine 106 with 1 ignored). Items 4 through 7, remote-lane
execution, the `NUCLEUS_CHARTS_TOKEN` value, and the self-hosted
`[rithmic-credentials]` runner remain open, so the exit gate stays
closed and phase 5 remains blocked.

Update-transaction coverage (same day, this batch): the phase 4 exit
gate names multi-version upgrades and failed-candidate deletion
explicitly, but the lifecycle suite proved only a single 1-to-2 upgrade
and asserted only the restored generation after a failed health check.
The new `successive_upgrades_leave_one_matching_release_and_no_staging`
regression installs 1, upgrades through 2 to 3, and asserts exactly one
version directory, active generation 3, and no residual journal; the
rollback regression now also asserts the failed candidate directory is
deleted and the journal is gone. No production behavior changed: the
strengthened assertions pass against the existing rollback path, which
already removes the candidate pointer, manifest, directory, and journal
before reporting `HealthCheckFailed`. Verification on Windows (this
machine): `cargo fmt --all -- --check`, workspace clippy with warnings
denied, workspace build, and `cargo test --workspace --all-features`
all pass with zero failures (architecture checks 35 of 35, desktop 141
of 141, `axiusflow_platform_runtime` 42 of 42, handshake 17 of 17,
protocol 7 of 7, engine 106 with 1 ignored). Items 4 through 7,
remote-lane execution, the `NUCLEUS_CHARTS_TOKEN` value, and the
self-hosted `[rithmic-credentials]` runner remain open, so the exit
gate stays closed and phase 5 remains blocked.

Interrupted-download coverage (same day, this batch): the exit gate
requires interrupted downloads to fail safely, but the suite proved
only tampered bytes, never a partial bundle. The new
`interrupted_bundle_fails_closed_without_activation` regression proves
both shapes: a missing bundle file fails with `StagingFailed` and a
truncated file fails with `VerificationFailed`, each with no active
release and no residual journal. No production behavior changed; the
test pins the existing per-file copy-then-verify staging order. The
live-gate architecture check now also rejects `continue-on-error` so
venue failures stay visible. Verification on Windows (this machine):
`cargo fmt --all -- --check`, workspace clippy with warnings denied,
workspace build, and `cargo test --workspace --all-features` all pass
with zero failures (architecture checks 35 of 35, desktop 141 of 141,
`axiusflow_platform_runtime` 43 of 43, handshake 17 of 17, protocol 7
of 7, engine 106 with 1 ignored). Items 4 through 7, remote-lane
execution, the `NUCLEUS_CHARTS_TOKEN` value, and the self-hosted
`[rithmic-credentials]` runner remain open, so the exit gate stays
closed and phase 5 remains blocked.

Lifecycle journal durability follow-up (same day, this batch): update
transactions now persist each state transition in a dedicated journal slot
(`update-0-preparing.json` through `update-5-cleanup.json`) before removing
older slots. Recovery scans newest-to-oldest and validates the embedded state,
so an interrupted Windows write cannot silently make a later transaction look
earlier. A regression test proves newest-state selection and fail-closed
behavior when the newest slot is corrupt. The focused platform-runtime suite
passes 44/44 on Windows; native installed update interruption and reboot
validation remain open under exit-gate items 4 and 7.

Three-OS CI rerun evidence (same day, run `33714658670`): the corrected
workflow reached the private-chart authentication step on Linux, Windows,
and macOS, and formatting passed on all three runners. Each lane then failed
at the first Cargo dependency fetch with GitHub's explicit `Invalid username
or token` response for `NucleusCharts/financial-charts`; no platform build or
test step ran. This confirms the workflow mechanism is now symmetric and the
remaining blocker is solely the repository secret value/scope. The phase-4
exit gate therefore remains closed until a valid read-only token is stored and
the complete three-OS run reaches build and test.

macOS display-probe parity (same day, this batch): the platform runtime no
longer reports display timing as unimplemented on macOS. The existing pinned
CoreGraphics-backed `display-info` adapter is now enabled for macOS and feeds
the same validated output model used by Windows (pixel dimensions, refresh
rate, scale, and descriptive identity; invalid native values are discarded).
The Windows platform-runtime test and clippy gates remain green (44/44 tests,
warnings denied). This establishes the native code path but is not physical
macOS rendering or scanout evidence; those release qualifications remain open
under exit-gate items 4 through 7.

Cross-target compile follow-up (same day, this batch): `cargo clippy -p
axiusflow_platform_runtime --target x86_64-apple-darwin --all-targets
--all-features -- -D warnings` passes from the Windows host, confirming the
new macOS code path is warning-clean even though it cannot provide native
runtime or physical-display evidence here. The Windows test suite remains
44/44 green.

Workspace lint follow-up (same day, this batch): the complete Windows host
workspace gate now passes with `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, including the macOS-targeted platform code
when compiled in the workspace dependency graph. No warning suppression or
platform-specific skip was added.

Native release-pair CI preparation (same day, this batch): every Linux,
Windows, and macOS deterministic lane now builds the release desktop and
engine together after the workspace build, embeds the same commit SHA as the
release identity and workflow run number as the install generation, and
uploads an OS-labelled pair. Uploads fail when either expected binary is
absent, and the architecture gate pins the three release commands, embedded
identity inputs, artifact names, and fail-closed upload policy.
This supplies provenance-bound binaries for the installed-pair qualification
once CI authentication is repaired; it does not itself constitute packaging,
launch, update, rollback, or uninstall evidence. Verification on Windows:
formatting passes and architecture checks pass 35/35.

Release-pair CI execution evidence (same day, run `33715438347`): all three
native jobs reached checkout, private-dependency authentication setup, and
formatting, then failed during workspace clippy while fetching the pinned
private Nucleus revision with `Invalid username or token`. Consequently the
new release build and upload steps were not reached. This is the same
credential-only failure observed in the prior run, now confirmed after the
release-pair workflow change; no release artifact exists from CI yet.

Clean-host workspace verification (same day, this batch): after stopping the
stale resident release engine that held a Windows executable lock, the full
`cargo test --workspace --all-features` suite completed with zero failures.
The run covers desktop 141, engine 106 plus one intentional release-only
ignore, handshake 17, protocol 7, naming 35, platform runtime 44, Rithmic
adapter 111, and all remaining workspace crates and doc tests. The initial
attempt exposed only generated-target contamination from a cross-target check;
the clean rebuild removed that false failure. Remote three-OS execution is
still blocked before compilation by the invalid private-chart credential.

Local release-pair qualification (same day, this batch): the Windows release
desktop and engine were built with `AXIUSFLOW_RELEASE_IDENTITY` set to the
current commit and `AXIUSFLOW_INSTALL_GENERATION=42`. The exact engine binary
was launched resident, and the optimized `native_release_status_probe`
completed over authenticated IPC: PID 16504, two provider slots, 6,767
retained bars, and a running shutdown state. The engine was then shut down via
its supported authenticated command and no Axiusflow engine process remained.
This proves identity-bound release startup/status IPC on Windows; it is not
installed packaging, update/rollback, rendering, transition, endurance, or
macOS/Linux runtime evidence.

Locked-file lifecycle coverage (same day, this batch): Windows now removes a
failed journal replacement's temporary `.next` file before returning the
staging error. A native `share_mode(0)` regression fixture holds the existing
journal open, proves the replacement fails closed, verifies the original
record remains readable after the lock is released, and verifies no temporary
artifact remains. Windows platform-runtime verification is 45/45 tests with
warnings-denied clippy green; installed update/reboot behavior remains open.

Release-pair provenance manifest (same day, this batch): each native CI lane
now writes `release-pair.provenance` containing the workflow commit, install
generation, and SHA-256 of both release binaries, then uploads it with the
pair. The naming gate pins all three manifests and the fail-closed artifact
paths. Local Windows release startup/status qualification remains valid; CI
cannot produce the manifest until the private dependency credential is fixed.

macOS cross-target audit result (same day, this batch): a Windows-hosted
`cargo check --workspace --all-targets --all-features --target
x86_64-apple-darwin` reached the native macOS dependency graph and then stopped
in `ring`/`aws-lc-sys` because no C compiler is installed for the Apple target
(`cc` not found). This is an environment limitation of cross-compiling
C-backed dependencies, not evidence of source compatibility; the authoritative
macOS build remains the native `macos-latest` lane once its private dependency
credential is repaired.

Private-dependency preflight (same day, this batch): deterministic and live
market workflows now validate `NUCLEUS_CHARTS_TOKEN` against the GitHub API
before Cargo runs. Linux/macOS use `curl` and Windows uses PowerShell; both
paths keep the token in an environment variable and emit only an empty/HTTP
failure category. The naming gate pins all five preflight steps. This removes
several minutes of opaque Cargo retries and makes the remaining CI blocker
actionable without weakening the required three-OS gate.

Preflight execution result (same day, run `33745898713`): the new validation
ran on all native runners and showed `NUCLEUS_CHARTS_TOKEN` is empty in the
job environment on Linux, Windows, and macOS. The repository secret name is
listed in GitHub metadata, but no value is injected; Cargo therefore never
starts. This narrows the maintainer action from “debug Cargo authentication”
to storing a non-empty read-only token value with access to the private charts
repository, then rerunning the workflow.

Focused gate refresh (same day, this batch): the current Windows checkout
passes the platform-runtime suite at 45/45 and the standalone architecture
naming suite at 35/35, with no ignored tests in either suite. These checks
confirm the local lifecycle and boundary assertions remain green after the
credential-preflight changes; they do not substitute for the still-blocked
native Linux/macOS CI, packaging, physical-transition, provider-feed, or
endurance evidence.

Native data-root selection correction (same day, this batch): lifecycle
inventory resolution now selects the path policy by compiled target before
reading environment variables. Windows uses `LOCALAPPDATA`, macOS uses
`~/Library/Application Support` and `~/Library/Caches`, and Linux uses the
XDG locations with the documented home-directory fallbacks. A stray
`LOCALAPPDATA` variable can no longer redirect a macOS or Linux uninstall into
a Windows-shaped root. Formatting, warnings-denied platform clippy, and the
45-test platform suite pass after this change; native-host verification on
macOS/Linux remains a CI responsibility.

Atomic autostart configuration (same day, this batch): Linux desktop-entry
and macOS LaunchAgent writes now use a same-directory synced temporary file
followed by an atomic rename. Failed writes remove the temporary artifact and
leave the prior registration intact, so a crash cannot expose a truncated
startup definition. The Windows registry path remains unchanged. Platform
runtime tests (46/46, including a staging-cleanup regression), warnings-denied
clippy, formatting, and architecture checks (35/35) pass after this change.

Native CI rerun confirmation (same day, run `33746804477`): all three native
jobs reached the credential preflight and failed because the injected
`NUCLEUS_CHARTS_TOKEN` value is empty. The target-root and atomic-autostart
changes therefore have local Windows evidence only; no claim of native
Linux/macOS build or runtime qualification is made.

Credential status remains unchanged (same day, run `33746889051`): the next
native workflow after this documentation update again completed with all three
OS lanes failing at the same empty-token preflight. No Cargo compilation,
release artifact, or runtime evidence was produced.

Latest native run confirmation (same day, run `33746889051`): the completed
logs show the empty value on Linux, Windows, and macOS explicitly; the
workflow remains correctly fail-closed before any private dependency or
release artifact is downloaded.

Full local workspace gate refresh (same day, this batch): after the recent
platform changes, `cargo test --workspace --all-features` completed with zero
failures on Windows. The run covered all workspace unit, integration, and doc
tests; only the explicitly release-only performance test and three live-market
soak tests remained ignored by design because they require an optimized
resident process, live feeds, or native credentials. This strengthens local
regression evidence but does not replace the native three-OS release gate.

Workspace clippy gate refresh (same day, this batch):
`cargo clippy --workspace --all-targets --all-features -- -D warnings`
completed successfully on Windows in 48 seconds after rebuilding the full
dependency graph. No warning suppression or platform skip was introduced; the
native Linux/macOS and live-provider gates remain separate evidence
requirements.

Atomic lifecycle-record replacement (same day, this batch): `write_json_atomic`
previously deleted an existing active pointer or manifest before renaming the
new record into place. That created a real absence window on Unix and macOS,
which could make a crash or concurrent launch observe no active release. The
replacement now uses the POSIX atomic rename-over-destination path; Windows
retains its required remove-then-rename fallback under the lifecycle lock,
with the platform difference documented at the helper boundary. The focused
`axiusflow_platform_runtime` suite passes 43/43. This improves the durable
POSIX activation guarantee but does not close the native Windows atomic
replacement, physical-transition, remote-lane, credentialed-feed, packaging,
or endurance evidence gates; phase 5 remains blocked.

Cross-platform journal durability follow-up (same day, this batch): update
recovery no longer overwrites a single `update.json`. Each transaction state is
written to its own bounded journal record, and the new record is durable before
older state records are removed; recovery selects the newest valid state and
cleanup removes every slot. Interrupted uninstall also preserves an existing
journal rather than rewriting it. This removes the Windows delete-before-rename
gap from update state transitions while keeping the lifecycle lock and
fail-closed recovery semantics. The focused platform suite remains 43/43.

### Cross-platform exit gate

Cross-platform stabilization is complete only when all of the following are true for Windows,
macOS, and Linux:

- the complete deterministic workspace gates pass on native runners;
- no required platform or installed-binary test is ignored or represented by another OS;
- packaged desktop/engine identity, authenticated IPC, reconnect, replacement, and shutdown pass;
- install, update interruption, rollback, stale-binary prevention, and complete uninstall pass;
- Coinbase and every available credentialed provider pass the real engine path;
- offline, network, power, display, session, and process lifecycle transitions recover correctly;
- window controls, input, mixed-DPI behavior, and physical frame pacing meet their acceptance
  thresholds;
- the eight-hour release endurance run completes without unbounded growth, silent data loss,
  stuck loading, leaked workers, or an unexplained process exit; and
- the evidence names the exact source revision, dependency lock, package, executable hashes,
  hardware/display configuration, OS version, and test result.

Phase 4 cannot pass, phase 5 cannot begin, and a production release cannot be approved while this
cross-platform gate is incomplete.

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

### Platform lifecycle

`platform_runtime` owns the native install/update/uninstall boundary. It maintains one versioned inventory of Axiusflow-owned install paths, state roots, service registrations, shortcuts, IPC names, lock files, update artifacts, and native-vault identifiers. Desktop, engine, storage, and future account code register ownership through this boundary rather than scattering new paths or credential names.

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

### Phase 1 execution record - 2026-08-31

The behavior lock and ownership map were established against the current `main` implementation:

- `MarketService::start_composed` is the process-composition boundary. Its only production
  provider inputs are the Coinbase public account and Rithmic Test environment records installed
  in `ProviderRuntimeRegistry`.
- The bounded `Command` queue remains the only coordinator command/completion lane. Service
  commands are `RestoreHotSet`, `SetResourceMode`, `Status`, `Attach`, and `Detach`; consumer
  commands are `Register`, `Remove`, `Viewport`, `ResourceClass`, `Demand`, provider search and
  selection, instrument installation, and `Poll`; history and storage workers return only the
  declared completion variants. `Coordinator` owns every dispatch and state transition.
- Each registry record owns one provider history sender and worker, its provider-specific live and
  catalog controls/events, cancellation state, observed worker generation/reconnect/terminal
  state, and worker handles. The registry rejects a duplicate provider identity before a second
  runtime can become active.
- Provider request queues remain capped at 8 history requests per provider; Coinbase and Rithmic
  live event queues at 2,048; Coinbase live controls at 1; Rithmic live controls at 2; and catalog
  and coordinator command lanes at 64. Full history and catalog queues return explicit capacity
  errors. Live event overflow latches recovery and retires the affected generation. Consumer series
  queues remain capped at 1,024 and recover with a covering snapshot.
- `MarketEngine` remains the sole owner of demand, provider capabilities and accepted generations,
  canonical series, shared subscription reference counts, resource policy, and publications.
  `Coordinator` owns in-flight request bookkeeping, adapter-neutral catalog installation,
  history/live seam state, consumer outboxes, storage orchestration, and presentation recovery; the
  provider registry owns concrete worker lifecycle only.
- Provider generations are verified by `MarketEngine` before dispatch and again on completion.
  Consumer generation and client ownership are checked before viewport, demand, poll, and
  publication. Catalog authorization/session/selection generations fence replaced searches and
  selections. Shutdown first cancels in-flight requests and provider controls, then the registry
  disconnects and joins its worker handles while `MarketRuntime` enforces the public deadline.
- The behavior lock is covered by the existing engine regressions for symbol/timeframe session
  reuse, rapid-demand cancellation, viewport/provider fencing, contiguous history/live handoff,
  Coinbase reconnect and overflow, Rithmic reconnect/environment/catalog generations, independent
  provider history workers, bounded depth/order flow, shutdown deadlines, and multi-client IPC
  isolation. Phase 2 added a duplicate-runtime regression and an architecture assertion for the
  registry boundary.

The phase 3 move order is source `apps/engine/src/market_service.rs` into its internal module tree,
with no forwarding layer: process channels/start/stop to `runtime`; catalog authorization and
installation to `instrument_selection`; local-history dispatch/completions to `storage`; request,
retry, viewport repair, and handoff state to `history`; live aggregation/depth/order flow to
`realtime`; consumer outboxes/snapshots/load states to `publication`; and the remaining single event
loop and generation transitions to `coordinator`. Each move carries the matching regressions named
above, while cross-module session-reuse, handoff, recovery, shutdown, and multi-client tests remain
at the coordinator boundary.

Verification recorded for this execution:

- `cargo fmt --all -- --check`: passed.
- Phase-owned clippy (`axiusflow_engine`, all targets/features): passed with warnings denied.
- `cargo build --workspace --all-targets --all-features`: passed.
- `cargo test --workspace --all-features`: passed.
- Architecture checks: 29 passed.
- Release desktop and engine: built and launched; `/proc` executable inodes matched the newly built
  release files, and closing the desktop stopped the matching resident engine.
- Coinbase release soak: passed against the final registry implementation for 60 seconds with one
  timeframe switch, three covering snapshots, and 246 live updates.
- Rithmic release smoke: credentials and provider environment were available; ticker login, symbol
  search, and instrument reference passed, after which the adapter stopped with the redacted
  baseline failure `stream_read_failed=Rithmic protocol validation failed` before engine-registry
  routing.
- Workspace clippy remains blocked only by unchanged desktop baseline findings in
  `apps/desktop/src`; the modified engine and architecture targets are clean.

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
  - build one immutable release bundle containing the matching desktop, engine, and required runtime assets;
  - generate a versioned manifest with release identity, install generation, channel, minimum version, platform, architecture, file inventory, URL, size, hash, and rollout metadata;
  - sign the canonical manifest with a key unavailable to R2/CDN;
  - verify signature, size, hash, file inventory, platform, architecture, permissions, and downgrade policy before activation; and
  - preserve the current installation after interrupted, corrupt, disk-full, or rejected updates.

### Transactional update and stale-binary prevention

- Use a small platform-native launcher/updater supplied by packaging that is not one of the binaries being replaced. It is not a resident service, a third application-state owner, or a new workspace application. Do not overwrite a running desktop or engine in place.
- Install each candidate into a new versioned directory on the same filesystem as the active installation. Write and sync every file, validate the signed inventory, and never expose a partially staged directory as current.
- Hold one machine/user-scoped update lock so two desktops, an autostart event, or two updater processes cannot race activation.
- Before switching versions, mark the installation as updating so no new Axiusflow process can start, ask the resident engine to stop through authenticated IPC, close the desktop, and wait for bounded worker shutdown.
- Confirm the old desktop and engine process identities have exited and released the IPC endpoint. Fail safely or enter an explicit reboot-required state if an owned process or file lock cannot be cleared.
- Atomically switch one active-release pointer or launcher manifest only after the old processes have stopped. All shortcuts and autostart registrations target the stable launcher, never a version-specific desktop or engine path.
- Extend the local handshake with release identity and install generation. The launcher starts the active desktop, the desktop resolves the engine from that same active release, and readiness succeeds only when both report the manifest's exact release identity and generation.
- If an old engine still owns the socket, reports a different release identity, or runs outside the active version directory, the desktop must not attach to it. The updater shuts it down by verified process identity before starting the active engine; an unverified process is never killed by name alone.
- After activation, require a bounded health check covering desktop launch, authenticated IPC, engine readiness, workspace restore, and one provider-neutral market-service readiness probe. On failure, atomically restore the previous pointer and delete the failed candidate.
- Keep the previous version only inside the in-progress transaction. After the new release passes its health check, remove the superseded version and all download/staging files, then verify they are absent. If removal is blocked, report `UpdatePendingCleanup` or `RebootRequired`; never report the update as complete.
- At every normal launch, reconcile the active manifest against the process path, autostart target, installed file inventory, and unfinished update journal. Repair or surface an actionable terminal error instead of silently running an old binary.

### Complete local uninstall

- Ship an uninstaller through the same platform-lifecycle boundary and expose one clearly confirmed action: **Remove Axiusflow and all local data**. Ordinary sign-out remains separate and does not imply account deletion.
- The uninstaller runs outside the binaries it removes, takes the install/update lock, blocks relaunch, disables every autostart/service registration first, requests authenticated engine shutdown, closes remaining Axiusflow windows, and confirms all verified Axiusflow process identities have exited.
- Revoke and delete every Axiusflow native-vault entry before deleting encrypted data, including the local IPC token, market-history catalog and segment keys, provider credentials, and - after phase 5 - refresh tokens, device keys, and cached entitlement material.
- Delete every path in the ownership inventory, including:
  - desktop and engine binaries, runtime assets, launchers, shortcuts, uninstall metadata, and version directories;
  - engine workspace and hot-set frames, corrupt/quarantine files, local market history, catalogs, segments, staging files, provider caches, and retained market data;
  - desktop preferences, layouts, drawing state, window state, caches, and temporary files;
  - application logs, diagnostics, crash reports owned by Axiusflow, update downloads, journals, rollback files, lock files, and stale IPC filesystem entries; and
  - Windows Run entries, Linux autostart desktop files, macOS launch agents, and any later OS integration owned by Axiusflow.
- Resolve and validate every deletion target from the signed install/data inventory. Never follow symlinks or reparse points outside an Axiusflow-owned root and never delete by a broad home, profile, application-data, or temporary-directory prefix.
- Make deletion idempotent and resumable. A crash or reboot resumes the uninstall journal before any Axiusflow launch, while missing files and already-revoked keys count as successful cleanup.
- Finish with an absence audit covering process list, service/autostart registrations, active and superseded install roots, every registered data/cache/log root, IPC artifacts, and every registered vault key. If any item remains, show the exact redacted category and remediation and do not report success.
- The guarantee is complete logical removal of Axiusflow-owned local state. Filesystem snapshots, external backups, and operating-system audit records outside Axiusflow's ownership cannot be erased by the application; destroying the vault keys before removing encrypted market data makes any residual storage blocks cryptographically unreadable.
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
- Interrupted download, invalid signature, corrupt file, disk-full, crash at every transaction boundary, locked executable, stale socket owner, mismatched desktop/engine release, failed health check, rollback, and reboot-required update cases fail safely.
- A successful update leaves exactly one active release, one matching desktop/engine generation, no superseded binary or staging artifact, and an autostart entry resolving through the stable launcher to that release.
- Fresh-install, multi-version-upgrade, interrupted-update, interrupted-uninstall, and repeated-uninstall tests leave no Axiusflow-owned local data, market history, credentials, processes, service entries, or update artifacts after uninstall reports success.
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
- The install/data inventory includes every phase-5 account, device, entitlement, and billing-cache artifact, and complete uninstall removes all of them without changing the user's remote account unless separately requested.
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
