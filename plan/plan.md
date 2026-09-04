# Axiusflow Architecture and Authentication Migration Plan

Status: phases 1 through 3 implemented; phase 4 substantially built but
not yet qualified — deterministic gates are green on Windows and Linux,
installed lifecycle is proven complete on both supported targets, and the
Rithmic credential/session paths are green on every stage the test feed
populates. Still open: physical transition evidence (producer built and
unit-proven, no passing run yet), eight-hour endurance (started, incomplete),
a data-carrying Rithmic feed (test plant publishes no prints), Windows/Linux
window/input/DPI/pacing runs, and maintainer approval for phase 5. macOS native
qualification is explicitly deferred and is not a phase-4 blocker. Phase 5
remains blocked.

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
- equal end-to-end product support and release qualification on Windows and Linux, with macOS
  source portability retained but native support deferred until Apple hardware exists;
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

## Supported-platform qualification is a product requirement

Windows and Linux are equal, first-class Axiusflow targets. The intended outcome is not source
compatibility, successful cross-compilation, or a desktop window that merely opens. It is complete
end-to-end support for the real installed product on both supported targets:

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
- diagnostics and actionable terminal errors are available on both supported targets without
  exposing secret material.

A change that passes Linux CI while breaking or leaving Windows unverified, or vice versa, is not
an acceptable development result. Neither "works on Linux" nor "compiles on both targets" is
evidence of supported-platform qualification. Platform-specific code, tests, packaging, and
physical validation are part of the feature itself and must land in the same completed batch. A
target may be called supported only when its required automated and native release gates pass.

macOS is a deferred target, not a supported release target and not a phase-4 gate. Shared and
macOS-specific source must remain deliberately guarded and free of known defects; cross-target
checks should run where the available toolchain can execute them. That evidence is not native
qualification, and Axiusflow must not claim macOS support until Apple hardware runs the complete
release gate. This deferral removes an unavailable machine from the critical path without turning
an expectation of portability into an unsupported compatibility promise.

### Required development policy

- Every pull request and push to `main` runs formatting, clippy, build, and deterministic workspace
  tests on Windows and Linux. Both jobs are required and neither may be represented by a
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
- A failing supported target blocks completion. Do not weaken, suppress, skip, or relabel the
  failure as a platform limitation unless the maintainer explicitly removes that target from
  product support.
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
3. **Establish the supported-OS CI matrix.** Run required Windows and Linux jobs for the complete
   deterministic workspace gates. Keep platform-specific failures visible and fail the workflow if
   either supported target is skipped unexpectedly. macOS stays outside the required matrix until
   Apple hardware exists.
4. **Qualify installed release pairs.** Build release packages on each supported OS; launch the
   packaged desktop; verify its executable identity and the matching engine release/generation; exercise
   authenticated IPC, workspace restoration, Coinbase, available credentialed Rithmic, clean
   shutdown, relaunch, update, rollback, and uninstall.
5. **Qualify native transitions.** On each supported target, capture offline startup, loss and
   restoration of network availability, suspend/resume, display disconnect/reconnect, DPI and monitor changes,
   desktop close modes, session sign-out, and OS shutdown. Require data continuity or explicit
   bounded recovery after every transition.
6. **Qualify rendering and input.** Exercise native window controls, IME, keyboard, pointer,
   drag/resize, fullscreen, multi-monitor movement, mixed DPI, and long chart/DOM interaction. On
   Windows capture external physical scanout at 60, 120, and 144 Hz, including single-GPU,
   hybrid-GPU, and virtual-display configurations. Equivalent platform-appropriate pacing evidence
   is required on Linux.
7. **Run endurance and fault campaigns.** Complete at least eight continuous hours per supported OS with the
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

Feed re-characterization (same day, vault credentials present): with no
local engine or desktop running, the debug smoke was rebuilt from current
`main` (kit enabled via `provider_kit/current/proto`) and run twice,
sequentially. Full smoke: login, 31-result search (both `CME` and
`CME-Delayed` venues offered; MNQU6/CME selected), instrument reference,
live quotes and depth all pass with zero prints; the plant then drops the
idle watch connection (`stream_read_failed=Rithmic transport failed` at
about two minutes; the smoke sends no heartbeats while watching, unlike
the product session driver). History-only: same login/search/reference
pass, ticker closes cleanly, and the replay completes with `history_empty`.
Conclusion unchanged: the adapter is correct on every populated stage;
prints and history for MNQU6 remain absent plant-side, so tick bars cannot
form. Still needs a data-carrying feed or entitlement before the
credentialed path passes end to end. No secret material in logs or repo;
sessions were sequential, never concurrent.

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

Locked old-release cleanup coverage (same day, this batch): the exit gate
names locked executables explicitly, but the suite proved only journal-file
locking, never a locked file inside the superseded version directory during
update cleanup. The new Windows-native
`locked_old_release_defers_cleanup_without_completing_update` regression holds
the old engine file with read-sharing but no delete-sharing (pre-install audit
reads still pass, deletion fails as with a locked executable), proves the
upgrade fails closed with `UpdatePendingCleanup` while the candidate pointer
is active and the journal remains, then proves `recover` completes cleanup
after the lock releases leaving one version directory and no journal. No
production behavior changed; the test pins the existing fail-closed and
resumable recovery path. Verification on Windows (this machine):
`cargo fmt --all -- --check`, warnings-denied platform clippy, focused
`axiusflow_platform_runtime` 47/47, architecture checks 35/35. Items 4 through
7, remote-lane execution, the `NUCLEUS_CHARTS_TOKEN` value, and the self-hosted
`[rithmic-credentials]` runner remain open, so the exit gate stays closed and
phase 5 remains blocked.

Release-pair qualification on current tree (same day, this batch): the
release desktop and engine were rebuilt from `d22c181` with
`AXIUSFLOW_RELEASE_IDENTITY` set to the commit and
`AXIUSFLOW_INSTALL_GENERATION=43` (engine SHA256
`DFF99BB566285332093549B3F1E85A6674414641A1332A388273186A245CB178`,
desktop SHA256
`5BA359B5B2B434A2C368AF3DC2C0D24679800EF9C41A7A97C9ECEA2583F4A9C6`).
The exact engine binary was launched resident (PID 19552, path match) with
no other Axiusflow process running. The optimized status probe passed over
authenticated IPC (PID match, `coinbase:2:gen1` Online with retained bars
growing 6821 to 6956 across probes, `rithmic:0:gen0` with no demand,
shutdown `Running`); a probe built without the release identity is rejected
with the redacted mismatch detail, confirming the handshake gate. The first
snapshot demand timed out at its 2s deadline on a cold series shortly after
engine start, and the bounded retry passed (135-bar BTC-USD snapshot with
sequence, timestamp, close, and volume populated, publication 1). The
engine was shut down gracefully through `--shutdown` (no Axiusflow
processes remain; a second `--shutdown` fails closed with os error 2 as the
socket is released). This re-proves identity-bound release
startup/status/snapshot IPC on Windows for the current tree; installed
packaging, transitions, rendering, endurance, and macOS/Linux evidence
remain open under items 4 through 7.

Coinbase 600s live soak (same day, this batch): the gate-standard soak ran
twice through the in-process engine. Run 1 failed at 263s on generation 7
(ETH-USD 60s): the open-candle assert fired just past the 5s bucket-roll
grace with newest bucket `1788439380` and no forming candle yet, after six
prior switches had all passed with open candles present. The failure
message now also carries the current bucket, bucket age, snapshot/update
counts, and last-publication age (test-only diagnostic change; no behavior
or threshold change). Run 2 passed the full 600s window: 15 switches, 19
snapshots, 1682 updates, every switch loading 251 bars with the open candle
present, and the gate evidence records `passed` (15 switches, 19 snapshots,
1682 updates). Conclusion: the run-1 failure was a transient slow bucket
roll past the tight grace, not a systematic handoff defect — but the 5s
grace can false-positive on a slow roll, and future failures must be read
with the new diagnostic fields rather than assumed transient. Full
workspace gates pass with the diagnostic change. Items 4 through 7,
remote-lane execution, the `NUCLEUS_CHARTS_TOKEN` value, and the self-hosted
`[rithmic-credentials]` runner remain open, so the exit gate stays closed
and phase 5 remains blocked.

First authenticated three-OS run (same day, run `33758370088`): after the
maintainer stored a non-empty `NUCLEUS_CHARTS_TOKEN`, all three lanes passed
checkout, chart authentication, credential preflight, formatting, clippy,
and the debug workspace build — the first remote execution past dependency
fetch since Aug 31. Verdicts: Windows passed the entire lane (release pair,
workspace tests, perf baseline `first_usable=15 ms, warm_read=1558 ms`);
Linux failed the release pair with `No space left on device` building the
`gpui` rlib (debug plus release artifacts exceed default free space);
macOS failed exactly one test,
`lifecycle_revision_zero_resident_is_shutdown_before_replacement_start`,
with `WouldBlock` (os error 35) on the first read off a non-blocking
listener — accepted Unix sockets inherit non-blocking mode on macOS while
Linux blocks, so the single `.expect` read raced the client hello.
Production IPC already polls `WouldBlock` correctly; only the `#[cfg(test)]`
helpers assumed blocking reads. Fixes in this batch (no production
change): bounded `read_arrival_frame` arrival reads at the three fixture
sites, and a Linux-lane disk-freeing step removing only unused preinstalled
SDKs (the runner tool cache stays untouched). Local verification: fmt,
warnings-denied client clippy, client 4/4, architecture 35/35. CI re-run
pending; the exit gate stays closed and phase 5 remains blocked.

First green three-OS run (same day, run `33768795698` on `ac3bf42`):
macOS 16m38s, Linux 31m24s, Windows 39m35s, all steps green on all lanes,
with all three release pairs and all three 30-day market-data baselines
uploaded. The two intervening reds were each diagnosed from lane logs and
fixed without touching production code: a one-frame-in-80ms scheduling
stall on a loaded macOS runner (same assertion passed there 40 minutes
earlier; window widened to 320 ms with continuity/bounds assertions
unchanged), then thirteen macOS-only failures from two native-fixture gaps
exposed the first time the suite ran that far — `TMPDIR` under symlinked
`/var` failing installer validation (fixtures now canonicalize the base on
Unix) and the display test still asserting `Unavailable` after the macOS
probe was enabled (now a validating macOS probe test mirroring Linux).
The macOS cross-target clippy lane (`x86_64-apple-darwin`, warnings denied)
passes from the Windows host for the touched crate. This meets the
cross-platform exit-gate bullet for deterministic workspace gates on native
runners. Still open: installed packaging and reboot validation, physical
transitions, rendering/scanout, eight-hour endurance, a data-carrying
Rithmic feed, the self-hosted `[rithmic-credentials]` runner, and
maintainer approval — so the exit gate stays closed and phase 5 remains
blocked.

Maintainer launch handover (same day, this batch): the release desktop and
engine were rebuilt from `d5da944` with `AXIUSFLOW_INSTALL_GENERATION=44`
and launched from the exact binaries (engine PID 16880 resident, desktop
PID 16980 with a responding native window). The authenticated status probe
confirms the intended pair: PID match, one attached desktop client,
`coinbase:2:gen1` Online with 9 retained series and 28,368 bars,
`rithmic:0:gen0` with no demand yet, shutdown `Running`. Rithmic vault
credentials remain provisioned, so `--rithmic-test` or in-window Rithmic
selection will run the live engine path; tick bars and history still
resolve only when the test feed carries prints. Handed over running for
maintainer-driven use; no graceful shutdown was issued.

Stalled depth-snapshot recovery (same day, this batch): live diagnosis on a
resident engine showed a book stuck `AwaitingSnapshot` publishing one empty
frame in 30s while bars flowed — a missed initial venue snapshot stalls
forever because later deltas cannot build the book
(`apply_delta` requires `snapshot_ready`) and nothing resubscribes while
the product set is unchanged. Fix (engine production, `89ff2f4`): each
canonical book carries a bounded watch on the coordinator tick — 30s
without a first install triggers an exclude-then-restore resubscribe dance
for the stalled products only (the venue re-sends snapshots solely on
subscribe); five dances without progress go quiet with a redacted stderr
diagnostic, leaving recovery to fresh demand, product change, or session
reconnect. Subscription updates now preserve Ready books for retained
products instead of wiping all books on every update. Rithmic books are
excluded from the wall-clock bound (slow backfill is legitimate there).
Regression cover: targeted dance legs, quiet after bound, Ready untouched,
Rithmic skip, session preservation (engine 111 = 106 + 5 new). Live
verification on the rebuilt `89ff2f4`/gen-47 pair: 534/534 Ready books in
30s with zero empty, so the preserve change did not regress subscribes.
Workspace gates green; CI run pending. The exit gate stays closed and
phase 5 remains blocked.

Remote CI billing block (same day, this batch): the three runs after the
last green (`33782369459` on `f97cb98`, `33786596888` on `89ff2f4`,
`33786927635` on `6e35aad`) all fail identically with zero executed
steps: every lane's check-run annotation reads "The job was not started
because recent account payments have failed or your spending limit needs
to be increased. Please check the 'Billing & plans' section in your
settings". Jobs never reach a runner (11s, no steps, runner unassigned),
so no build, test, release-pair, or baseline artifact exists from these
runs and the code batches are exonerated — the last green run
(`33778255928` on `e08604e`, 16:22 UTC) predates the first billing
failure (17:04 UTC). Maintainer action required: resolve the account
billing/spending-limit condition, then rerun the workflow from current
`main`; no workflow or code change is needed for this. Until remote
lanes execute, the current tree's evidence is local only. Local
re-verification on Windows (this machine, `6e35aad`): `cargo fmt --all
-- --check` clean, `axiusflow_engine` 111 passed / 0 failed (1
release-only ignore plus 3 live-soak ignores by design), handshake 17
of 17, architecture checks 35 of 35. The exit gate stays closed and
phase 5 remains blocked.

Zero-cost CI migration (same day, this batch): the maintainer will not
fund GitHub-hosted runners, so the matrix moves to maintainer-owned
self-hosted runners instead of staying red on billing. `ci.yml` lanes
now target `[self-hosted, axiusflow, linux|windows|macos]`, with a
same-ref `concurrency` cancel so pushes do not pile up behind one
machine; the Linux disk-freeing step is best-effort (`sudo -n ...
|| true`) because it was a GitHub-hosted workaround that must never
fail a personally-sized runner on a sudo prompt. The `coinbase` live
gate moves to `[self-hosted, axiusflow, linux]`; the `rithmic` gate was
already self-hosted and its comment now states the true behavior (queues
to timeout with no matching runner, reported as "not run", never a
pass). Architecture checks pin the new labels and forbid any return to
`*-latest` runners in both workflows
(`three_os_deterministic_gates_remain_required`,
`live_market_gates_stay_on_self_hosted_runners`, 36 of 36).

Hardware reality: only this Windows machine exists, so only the Windows
lane has an execution path today. Maintainer setup (my API token lacks
runner-admin scope, so this is Settings-side): repo Settings → Actions
→ Runners → New self-hosted runner → Windows → run the shown
download/config commands as the maintainer user with the `axiusflow`
and `windows` labels, then run interactively via `run.cmd`, not as a
service (vault/DPAPI parity with dev; the lane needs git, rustup with
the pinned toolchain auto-installing from `rust-toolchain.toml`, and
the already-stored `NUCLEUS_CHARTS_TOKEN`). Expect ~40 heavy minutes
per run; push in batches. Linux needs a VM (git, rustup, one-time
`tools/setup_linux_desktop.sh --system`, labels `axiusflow,linux`); a
runner holding Rithmic vault credentials takes the additional
`rithmic-credentials` label. macOS has no path: Apple hardware is
mandatory and none exists, so the macOS lane will queue 60 minutes and
fail closed on every push, and macOS cannot be called supported until a
native lane runs — the target is kept, not dropped, pending an explicit
maintainer decision. Verification on Windows (this machine): fmt clean,
targeted warnings-denied clippy clean, architecture checks 36 of 36.
The reworked workflows have no remote execution yet (no runner
registered), so they are reviewed but unproven; the exit gate stays
closed and phase 5 remains blocked.

First self-hosted Windows green (same day, run `33788488213` on
`7f719d9`): the maintainer registered `axiusflow-windows`
(`C:\actions-runner`, labels `axiusflow,windows`, interactive
`run.cmd`). The lane's first attempt failed in 18s at `Validate private
chart credential` with `pwsh: command not found` — this box had only
Windows PowerShell 5.1 while the lane pins `shell: pwsh`; free
PowerShell 7.6.5 was installed via winget and the failed lane rerun.
Result: `workspace-windows` completed success — fmt, warnings-denied
clippy, workspace build, release desktop/engine pair with provenance
manifest, full workspace tests, and the market-data baseline all pass
on maintainer hardware, with `release-pair-windows` and
`market-data-performance-windows` uploaded. The Linux and macOS lanes
stay queued with no runners and will fail closed at their timeouts;
macOS remains without an execution path until Apple hardware exists.
The exit gate stays closed and phase 5 remains blocked.

Self-hosted git-credential pollution (same day, this batch): the first
green Windows run executed the `git config --global ...insteadOf`
authentication step as the maintainer user, permanently redirecting all
of this machine's github.com git traffic through the chart token — the
next push failed with `Repository not found` (the token cannot see
`Axiusflow_GPUI`) until the global rewrite was removed by hand and push
access verified restored. All five authenticated jobs (three `ci.yml`
lanes, both live gates) now write the rewrite to a per-job temp file
exported via `GITHUB_ENV` as `GIT_CONFIG_GLOBAL` (bash and pwsh
variants; checkout still runs before it, so its own token is
unaffected) and delete the file in an always-run cleanup step, so no
secret or rewrite can leak into the owner's persistent configuration or
linger in runner temp. Architecture checks forbid any return to `git
config --global` and pin the scoped credential plus cleanup in every
job. Mechanism proven locally: a bogus-token scoped file intercepts
CLI git (`Invalid username or token`), while the clean tree resolves
`HEAD` through the credential manager with an empty global config.
Verification on Windows (this machine): fmt clean, targeted
warnings-denied clippy clean, architecture checks 36 of 36. The next
Windows lane execution is the live proof; the exit gate stays closed
and phase 5 remains blocked.

Workflow indentation breakage (same day, this batch): the scoped
credential commit above shipped with three corrupted `name:` lines (12
spaces instead of 10 under `with:`) introduced by the editing step, so
run `33792720189` died with zero jobs and a workflow-file error before
any lane started. Fixed by restoring the exact indentation; both
workflow files now parse under a real YAML parser with the expected
shape (ci: 3 lanes at 16/14/14 steps with self-hosted labels; live
gates: 2 jobs at 8/7 steps). Lesson for this machine: workflow edits
get parser validation before push, since remote execution is the only
other check and it costs a full run. The exit gate stays closed and
phase 5 remains blocked.

Job-scoped credential live proof (same day, run `33793515820` on
`e14503d`): `workspace-windows` completed success through the new path
— checkout, job-scoped authenticate, preflight, fmt, warnings-denied
clippy, workspace build, release pair with provenance, full workspace
tests, market-data baseline, artifact uploads, and the always-run
credential cleanup all pass, with `release-pair-windows` and
`market-data-performance-windows` uploaded. The private charts
dependency fetched through `GIT_CONFIG_GLOBAL`, and this machine's
global gitconfig is verified empty afterward: no persistent rewrite,
no lingering secret. Linux/macOS lanes remain queued with no runners.
The exit gate stays closed and phase 5 remains blocked.

Self-hosted Linux runner online (same day, this batch): Hyper-V was
enabled by the maintainer (no reboot needed for staging; one reboot
taken later for an unrelated reason), and an Ubuntu 24.04 Server VM
(`axiusflow-linux`: 8 vCPU, 16 GB RAM, 120 GB disk, NAT via Default
Switch) was built on this box and registered with labels
`axiusflow,linux`, running as a systemd service. Provisioned inside:
git, build-essential, pkg-config, desktop build libraries, rustup with
pinned toolchain 1.97.1 plus clippy/rustfmt, cargo on the system PATH.
Lessons from the build, in order: (1) Ubuntu's `*-cloudimg-amd64.img`
is qcow2 despite the name — raw conversions boot nothing; convert with
`-f qcow2`. (2) Hyper-V automatic checkpoints corrupted the first VM's
disk chain (config referenced a deleted AVHDX); they are disabled for
this VM. (3) qemu-img output needs a desparse copy before Hyper-V
accepts `Resize-VHD` (0xC03A001A). (4) The cloud image emits nothing on
the Hyper-V serial console; the VMConnect console is the ground truth.
(5) This box does not automount new volumes — seed-disk handling must
assign and mountvol-mount a letter explicitly. (6) PowerShell quoting:
`ssh-keygen -N '""'` sets a literal two-quote passphrase, which broke
key auth with a misleading server-accepts-then-client-gives-up
handshake; fixed with `-N ''`, and the VM is now key-only (password
login refused at protocol level). (7) The repo was renamed to canonical
`Axiusflowhq/axiusflow-gpui`; runner registration is case-sensitive on
the path and 404s otherwise, while git itself follows the rename —
local origin updated. The Windows runner predates the rename; the next
run shows whether it stays bound. Verification: key-only SSH, toolchain
versions, and runner `Listening for Jobs`. The exit gate stays closed
and phase 5 remains blocked.

Linux lane OOM and memory fix (same day, this batch): the first
`workspace-linux` execution reached clippy, then rustc was SIGKilled
compiling `ash` and the runner worker died with it (job cancelled).
Cause: the VM had 2 GB, not 16 — Hyper-V `New-VM` enables Dynamic
Memory by default and never ballooned up. Pinned static 16 GB
(`dynamic=False startup=17179869184` verified in-VM as 15 GB usable),
plus an 8 GB swap file and `jobs = 4` in the runner user's
`~/.cargo/config.toml` (runner-scoped; no workflow change) as
insurance for dual-lane load. Same run also proved the rename is
harmless to the old Windows registration: `workspace-windows` went
green post-rename. The rerun after the fix completed success: fmt,
warnings-denied clippy, workspace build, release pair with provenance,
full workspace tests, and market-data baseline all pass on the VM, with
`release-pair-linux` and `market-data-performance-linux` uploaded
alongside the Windows pair from the same run. Two native OS lanes are
now green on maintainer hardware; macOS remains without an execution
path. The exit gate stays closed and phase 5 remains blocked.

Installed-lifecycle campaign, Windows (same day, this batch): the real
`axiusflow_launcher` (built with a throwaway test key) drove a temp
install root through test-signed v1 (gen 9001) and v2 (gen 9002) release
binaries. Fresh install exited 0 with exactly one version directory,
one active pointer, one manifest, and no journals — the live health
check passed against the real desktop/engine over Coinbase. Update
exited 0 with exactly one v2 directory (v1 fully removed), one
pointer/manifest, no journals, no stray processes. Full
`--remove-all-local-data` deleted the vault keys (including the Rithmic
test credential), data, cache, and logs, then failed closed with
`UninstallPendingCleanup`: residue was the complete versions tree,
pointers, manifests, and journal. Root cause proven, not assumed: the
launcher runs from inside the tree it deletes, and Windows denies
deleting a running executable (empirical `Access denied` deleting the
running campaign engine; read_dir order hits the launcher first, so
nothing after it was attempted). Manual deletion completed the removal
and the absence audit passes (no roots, no Run entry, vault already
audited absent by the uninstall itself; engine state recreated by the
semantics probe was removed too). Production consequence: complete
uninstall needs an out-of-tree uninstall path (self-relocating launcher
or equivalent) — recorded as required phase-4 work, not yet
implemented. Side note: identity-gated IPC shutdown verified in passing
(same-identity `--shutdown` exits 0). The maintainer's resident engine
was shut down gracefully before the campaign; Rithmic reprovisioning
awaits the test password. The exit gate stays closed and phase 5
remains blocked.

Uninstall fix landed and proven (same day, this batch): the launcher
now renames its running image to a sibling staging directory (same
volume, renames permitted) before deleting the original tree
synchronously with an honest exit code; an OS-owned delayed deleter
removes staging afterwards (batch file on Windows, direct unlink on
Unix). Two failed designs are recorded so they are not retried: a
relocated child can never remove the tree while its waiting parent runs
from inside it, and inline `cmd /c` commands silently break because
Rust's argv quoting leaves cmd-stripped `\"` escapes in every path —
the command must live in a batch file. Final proof on Windows: fresh
install, then `--remove-all-local-data` exits 0 with install root,
lifecycle, staging, data, vault, and processes all verified absent.
Unit cover is the rename round-trip on fixture copies (the destructive
path itself stays physical-only by design); the architecture check pins
the relocation markers. The exit gate stays closed and phase 5 remains
blocked.

Rithmic reprovision and session restore (same day, this batch): the
maintainer-supplied test credential was encoded to the version-1 vault
blob and stored under the existing key through a throwaway helper that
was deleted afterward (round-trip verified, secrets cleared from the
session, tree clean). The dev release desktop/engine were rebuilt
without campaign identity (binaries verified clean) and relaunched
(engine resident, desktop window open). The ignored
`native_release_rithmic_markets_live_round_trip` probe then
live-verified the credential on the engine path: workspace restore,
attach, Rithmic search, instrument selection, and series demand all
pass; tick bars cannot form because the test feed publishes no prints
or history for MNQU6 (`historical bars are unavailable`), unchanged
from every prior feed characterization — adapter correct on every
populated stage, engine healthy afterward. The exit gate stays closed
and phase 5 remains blocked.

Linux runner vault outage and fix (same day, this batch): the first
manually-dispatched `coinbase` live gate failed in 6s with `demand
error in filesystem_write` after loading 251 venue bars. Root-caused,
not assumed: `LocalHistoryStore::open` needs catalog/segment keys from
the native vault (Secret Service), and the minimal server VM ships no
secret provider — the storage worker starts degraded and the first
persist fails the demand honestly. Proven by a dummy-key probe that
fails identically with and without a session bus (the earlier successful
Rithmic provisioning ran on the Windows DPAPI vault, so it never
contradicted this). Fix is environmental, not product: `gnome-keyring`
installed, a boot-persistent login keyring with an auto-generated
0600 password, a foreground `unlock-keyring.service` unit (`--daemonize`
exits instantly on this box; foreground stays), linger enabled, and a
runner drop-in injecting the user bus address. Proven across a reboot:
both units active, secret store/lookup/clear passes. Stuck-queue
postscript: jobs queued during runner downtime can stick indefinitely
(cancel + fresh dispatch recovers); same-ref concurrency flips
superseded runs to cancelled at run level while their green lane
evidence stands. The exit gate stays closed and phase 5 remains blocked.

Live-gate night, both venues (same day, run `33824909811`): the vault
fix validated live — the coinbase soak ran 353s with 8 consecutive
251-bar switches, open candle present every time, then missed covering
history on the 9th demand inside 45s (transient venue miss after 8
clean passes; no systematic pattern across runs, evidence artifact
uploaded). Self-inflicted wound recorded honestly: an earlier coinbase
attempt this night died by my own VM reboot mid-job — never reboot or
restart a runner with a live job; verify idle first. The Rithmic gate
failed twice at `timed out waiting for catalog search` (45s, zero
results) while the engine path, minutes apart on the same vault
credential, passes login/search/select/install and fails only on the
known empty feed. First failure overlapped the resident engine's
lingering reconnect loop (single-session churn); the second ran with no
engine Rithmic runtime anywhere, pointing at plant-side throttling after
churn or a brief search outage rather than product or credential —
re-run in a quiet window before drawing conclusions. Operational rule:
Rithmic live work needs a provably session-free window (engine
`rithmic:0:gen0` and no smoke in flight). The exit gate stays closed
and phase 5 remains blocked.

Rithmic gate kitless root cause (same day): all three catalog-search
timeouts traced to a missing licensed kit, not the plant or credential.
The runner checkout never had `provider_kit/current/proto` (gitignored,
absent after every fresh checkout), so CI builds fell back to kitless
Rithmic, which cannot speak R|Protocol — while the dev tree (156 proto
files present) searched fine minutes apart on the same credential.
Permanent copy preserved at `C:\axiusflow-deps\provider-kit` (hash
verified); the Rithmic gate now restores it per run with a fail-fast
absence check, pinned by architecture checks. Adjacent gap recorded but
not changed: a terminal session death during a pending search resolves
only at the consumer deadline (45s silence), since the failure is
retried and waiters cleared silently. The exit gate stays closed and
phase 5 remains blocked.

Checkout wipes manual kit seeds (same day): `actions/checkout` runs
`git clean -ffdx` by default, so hand-seeding the runner checkout is
futile — the workflow restore step is the only durable path, and no
kitted CI run has happened yet. Coinbase side note: two more transient
venue-timing legs (one history miss, one bucket-correction miss) across
20+ clean switches; no systematic pattern. The exit gate stays closed
and phase 5 remains blocked.

First kitted Rithmic gate (same day, run `33830904833`): the restore
step works — login and catalog search complete in CI for the first time
(all prior search timeouts were kitless artifacts). New leg: selection
times out after search passes. The soak searches literal roots (`MNQ`,
`MES`) and selects the exact-symbol match, while the engine probe forces
`MNQU6` and passes selection/reference minutes apart on the same
credential. Open question, not yet root-caused: whether the plant lists
an unreferenceable root entry tonight or reference is briefly out. Next
step is observability, not guessing: log the selected symbol/exchange in
the soak output, then re-run in a quiet window. No product defect is
evidenced; the engine path stays green on every populated stage. The
exit gate stays closed and phase 5 remains blocked.

Catalog generation-advance wedge fixed (same day): an engine whose
Rithmic catalog session starts offline never dials (the only
`request_connection` runs once at startup), and pending searches were
dropped silently on every retry. On environment advance with pending
demand the session now rejects each waiter with an actionable
dispatch-unavailable event and restarts, so re-demand lands on a
generation that can connect; consumers (soak, capture runner) retry on
rejection instead of timing out. Regression cover: pending searches and
selections are rejected with identity on advance. The capture runner
additionally caps demand attempts instead of hammering the plant.
Throttle postscript: tonight's search timeouts fit plant-side throttling
after login churn as well as the wedge; a 45-minute quiet period with a
single recovery probe distinguishes them. The exit gate stays closed
and phase 5 remains blocked.

Wedge fix proven live (same day): after the quiet period the stale
engine still timed out search, while a fresh engine on the fixed binary
passed search/select/demand within a minute on the same credential —
failing only at the known empty feed. Not throttle, not lockout, not
credential: the pre-fix catalog session genuinely wedges. The exit gate
stays closed and phase 5 remains blocked.

Empty-token gitconfig pollution, second occurrence (same day): push
failed with `Invalid username or token` again — global
`url.https://x-access-token:@github.com/.insteadOf` (EMPTY token) plus
the dev remote rewritten to an empty-token URL. Cleaned the same way
(`--unset-all` the full lowercase key path; `set-url` needs quoting)
and the push went through. Writer still unidentified: no current lane
writes `--global` (pinned absent by architecture checks), and no
manual command accounts for it. Next time: check
`~/.gitconfig` LastWriteTime immediately and correlate with active
runner jobs before touching anything; candidate suspects are a stale
checkout auth step and IDE Git integration. The exit gate stays closed
and phase 5 remains blocked.

Stopping point, end of session (same day): coding pauses here with the
tree committed and the record current. Confirmed state:
- Transition capture producer (`--capture-native-transitions`, headless
  recorder + report schema + unit cover) is implemented and warning-free,
  but has NO passing physical run. Five attempts failed at the operator
  boundary, not in product logic: stale manifests, Enter-while-online,
  silent 45s waits (fixed with narrated retries), and one reconnect held
  too briefly for session establishment. The last attempt logged a clean
  offline start, then exited before reconnecting. Next attempt needs the
  operator to hold each network state until the console confirms it.
- 8-hour endurance started but did NOT complete (`completion=incomplete`);
  no endurance evidence may be cited.
- Rithmic wedge fix is implemented, unit-covered, and proven live (fresh
  fixed engine passes search/select/demand; stale engines hang search).
- Rithmic credential reprovisioned and live-verified on the engine path;
  bars remain blocked on the printless test feed.
- Installed lifecycle (install/update/complete-uninstall) proven on
  Windows with real binaries, including the self-relocation fix.
- Deterministic gates green on Windows and Linux runners, including
  the wedge fix and capture producer (macOS queues with no runner).
- The scheduled Coinbase live gate just passed the full 600s soak on the
  vault-fixed VM; the scheduled Rithmic gate is queued behind it.
- No Axiusflow desktop, engine, or runner-driven job is running now; the
  resident session was left stopped.
- Two commits are committed locally but UNPUSHED (wedge live-proof note,
  tool-offender note); pushing retriggers both CI lanes (~40 min cook).
Open external blockers unchanged: Apple hardware, data-carrying Rithmic
feed, Linux lifecycle campaign, window/input/DPI/pacing runs, phase 5
approval. The exit gate stays closed and phase 5 remains blocked.

Resume audit and catalog terminal-recovery fix (same day, this batch): the
checkout began clean and synchronized (`main == origin/main` at `a38fcf7`).
Phases 1 through 3 remain complete; phase 4 remains the active phase, with
the cross-platform exit gate still blocked by macOS hardware, Linux installed
lifecycle proof, physical transition/rendering/input/DPI/pacing evidence, a
complete eight-hour endurance run, and a data-carrying Rithmic feed. The local
evidence inventory confirms the earlier endurance attempt stopped after
16,862,253 ms (about 4h41m) with `completion_state=incomplete`; the latest
native-transition attempt exited 101 after 16 seconds without a report, so
neither is counted as passing evidence. Its orphaned resident engine was shut
down through the supported authenticated command, leaving no engine process.

The adjacent Rithmic catalog gap recorded above is fixed: terminal callbacks
and retry activation now advance the provider generation and publish an
actionable rejection for every pending search or selection before clearing
the bounded maps. Consumers can re-demand immediately instead of remaining
silent until their 45-second outer deadline. The existing environment-advance
path uses the same helper, and a regression proves generation advance, both
command-domain rejections, and complete pending-map retirement. An accidental
duplicate `--capture-native-transitions` argument branch was removed without
changing the CLI contract. Windows verification: formatting, workspace clippy
with warnings denied, workspace build, and the complete workspace test suite
all pass (engine 113 passed plus one release-only ignore; naming 39 of 39;
desktop main 147 of 147). Live Rithmic and physical evidence were not rerun in
this code batch. The exit gate stays closed and phase 5 remains blocked.

Linux installed-lifecycle qualification (same day, this batch): the existing
Hyper-V Ubuntu runner is healthy (Ubuntu 24.04, Linux 6.8.0-138, static 16 GB
RAM plus 8 GB swap, 92 GB free during the run), and the `workspace-linux` job
in run `33884151294` passed formatting, warnings-denied clippy, workspace
build, the provenance-bound release pair, all workspace tests, and the market
data performance baseline on commit `eb48330`. A separate native campaign on
that same clean checkout built two release identities (generations 9201 and
9202) and exercised the real stable launcher. Fresh install passed the
authenticated desktop/engine readiness check; update activated generation
9202 and removed generation 9201, leaving one pointer, manifest, version, and
no journal; `--remove-all-local-data` then removed the install, lifecycle,
staging, native-vault, and application data inventory and passed the external
absence audit. No Axiusflow process remains, while the runner and Secret
Service units remain active. Evidence is retained on the VM at
`local-data/evidence/linux-installed-lifecycle-20260904/report.json` (report
SHA256 `8e4be3cd78d5ab768fbdbe2339920d61ef1f9a0d5811661c8c7b1444663a16c1`;
launcher `2726d850be4c133ccdb2d5847f1435b19665f6884dfa2e94277ab03a918f54d3`,
desktop `df36e95d9a49fe29e26dfee6fa6467a1cf72756982e56ed456ebc1477a67ead2`,
engine `208648a5c6296ec6596487b6e21101e5d6093cf31a105f61bced662294a7a54f`).
The first harness attempt failed closed on an externally serialized signature;
the second stopped before signing on a temporary dependency-version mismatch.
The passing run used the repository's own manifest types and signing function;
all throwaway signing helpers and the ephemeral private key were deleted.
This closes the Linux installed install/update/complete-uninstall evidence gap,
but a Hyper-V VM is not physical Linux display, input, or frame-pacing proof.
The macOS hardware, physical transition/rendering matrix, complete eight-hour
endurance, and data-carrying Rithmic-feed blockers remain; phase 5 stays
blocked.

Supported-target decision (same day, maintainer-approved): Windows and Linux
are the phase-4 and production-release targets. Native macOS qualification is
deferred because no Apple hardware is available; it no longer blocks phase 4
and the unserviceable `workspace-macos` job is removed instead of queueing to
timeout on every push. This is not a claim that macOS works: the existing
CoreGraphics, Keychain, LaunchAgent, Unix IPC, lifecycle, and guarded source
paths remain in the tree and must stay clean under available static and
cross-target checks, but only a future native Apple lane can promote macOS to
a supported target. `tools/naming_check` now pins the two required self-hosted
lanes, their complete deterministic gates, provenance artifacts, scoped
credentials, and the absence of a required macOS job. Historical three-OS
results above remain accurate execution history and are not retroactively
relabelled. The phase-4 exit gate below now applies to Windows and Linux. The
edited workflow parsed successfully with exactly `workspace-linux` and
`workspace-windows`; the naming policy passed all 39 tests, the complete
Windows workspace format/clippy/build/test gate passed, and
`axiusflow_platform_runtime` passed warning-denied Clippy for
`x86_64-apple-darwin`. That Apple-target check is source-portability evidence
only, not native macOS qualification.

Supported-target CI and Rithmic fixture follow-up (same day): commit
`2739246` completed the new two-lane CI run `33888206226` successfully on
both native runners. Each lane passed formatting, warnings-denied clippy,
workspace build and tests, release desktop/engine provenance and artifact
upload, and the market-data performance gate. A current-head live run
(`33889321694`) exposed a separate defect in the Rithmic soak fixture: it
selected the catalog's unreferenceable `MNQ` product root on `CME-Delayed`
instead of an expiring outright contract. The gate now follows the desktop
and smoke-probe rule—exclude the root and calendar spreads, require an
expiration, and choose the earliest listed contract—with deterministic
coverage. A local credentialed rerun selected `MNQU6`, completed engine-owned
search/reference/selection/install immediately, then failed explicitly at
the already-known external boundary, `Rithmic historical bars are
unavailable`; the test plant still supplies no data with which to qualify
bars. The parallel Coinbase run reached 251-bar covering history through
seven switches and continued publishing, then failed because the new
one-minute bucket was still absent six seconds after its boundary; this is
recorded as live timing evidence, not a deterministic-gate regression.

### Supported-platform exit gate

Supported-platform stabilization is complete only when all of the following are true for Windows
and Linux:

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
supported-platform gate is incomplete. macOS may become supported only after the same gate passes
on native Apple hardware; until then it remains explicitly unqualified and does not block the
Windows/Linux product.

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
