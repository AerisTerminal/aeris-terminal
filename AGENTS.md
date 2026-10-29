# AGENTS.md

Instructions for coding agents working in the Aeris native repository.

Aeris is a local-first Rust/GPUI trading terminal. The desktop is the single application process.
In-process `market_runtime` and `account_runtime` own provider sessions, canonical market/account state,
and bounded background work; GPUI owns presentation scheduling only. A secondary local market process must not return.

Aeris is in production. Version 1.0.0 shipped publicly for Windows x64 and macOS Apple silicon on
2026-10-25 through `https://aeristerminal.com/download/`, and real users run it with their own data.
Treat every change as a change to a shipped product: durable state written by any released version
must keep opening and migrating, and a user-visible capability ships only when it works on the
installed app. What the website (`https://aeristerminal.com/`, sibling repository) says Aeris can do
must match what the current release does; when a change adds, removes or limits a user-visible
capability, update the website copy in the same coordinated task.

## Non-negotiable core principles

Every change must satisfy all five principles below. They are mandatory acceptance criteria for
features, fixes, refactors, dependency upgrades, tests, tooling, and releases. Never compromise them
for speed, convenience, a temporary workaround, or a passing check.

1. **Proper architecture:** Keep behavior and state at their authoritative ownership boundary.
   Preserve dependency direction, single ownership, and every architecture invariant in this file
   and `tools/naming_check`. Never bypass the owner with duplicate state or parallel implementations.
2. **Clean code:** Use clear names, cohesive responsibilities, explicit errors, and the smallest
   complete implementation. Remove superseded code when replacing it; do not leave dead paths,
   speculative abstractions, or lint suppressions that hide defects.
3. **Scalable:** Keep work, memory, queues, caches, and retries bounded. Share provider sessions and
   canonical state, handle overload and cancellation explicitly, and verify relevant workloads.
   Never claim scalability or performance that has not been measured.
4. **Maintainable:** Make ownership, interfaces, data flow, and failure behavior easy to understand
   and test. Reuse existing boundaries and shared definitions; avoid hidden coupling and changes
   that require unrelated components to know implementation details.
5. **Built for the long run:** Preserve correctness, durable state, recovery, compatibility, and
   reproducible verification across restarts, upgrades, and sustained use. Prefer durable fixes at
   the responsible component over shortcuts that transfer complexity or risk to future work.

Evaluate every proposed implementation against all five principles before editing and again before
delivery. If an approach violates any principle, revise the approach; do not weaken the principle or
its checks. Existing violations are not permission to introduce or extend them. Report unresolved
violations and missing verification explicitly, and never describe them as satisfied or complete.

## Branch and Git workflow

- Work on `main` only. Do not create, switch to, or leave work on feature/continuity/scratch branches
  unless the maintainer explicitly requests a branch for that task.
- Start every task with `git status --short --branch` and confirm the repo is on `main`.
- Preserve unrelated work already present in the worktree. Never reset, checkout over, stash, or
  delete changes you do not own.
- Prefer one coherent implementation batch. Commit only after the relevant checks pass.
- Use the existing `type(scope): outcome` commit style and push verified work directly to `origin/main`.
- Never add agent or harness attribution to commits (no `Co-authored-by`, `Generated-by`, or mentions of Droid, Claude, Cursor, or any other coding agent in commit messages or trailers).
- Never force-push or use destructive Git commands.

## How to work

1. Read the real call path before editing. Identify the component that owns the behavior.
2. Make the smallest complete fix at that ownership boundary. Do not add duplicate state,
   compatibility layers, speculative abstractions, or extra services to avoid changing the owner.
3. Reuse existing code, `std`, platform facilities, and pinned dependencies before adding anything.
4. Add deterministic regression coverage for concrete bugs when practical.
5. Run focused checks while iterating, then broader gates before delivery or release.
6. For behavior that depends on persistence, provider lifecycle, rendering, updates, or the installed
   app, exercise the real path; compilation alone is not proof.
7. Edit files with the agent's native file tools (read, edit, write) so every change is a visible,
   reviewable diff. Do not write Python, `sed`, heredoc, or other scripts to modify source files.
   A script is allowed only for a genuinely bulk mechanical change that native tools cannot do
   practically; say so before running it, and make it fail when any intended replacement does not apply.

## Architecture invariants

- `MarketEngine` is the single market-demand owner inside `market_runtime`. Provider sessions are
  created and owned only by that in-process runtime; never create a session/runtime per chart or UI surface.
- Instrument quote interests and series interests share `MarketEngine`'s one bounded
  `DataLeaseRegistry`; do not add a second market-demand owner for trading or alerts.
  Provider-neutral feed contracts live in `aeris_trading::market` and depend only on
  `aeris_instruments`; `market_engine` may depend on `aeris_instruments` but never on
  `aeris_trading` or a provider adapter.
- `trading_runtime` is the single in-process owner of broker accounts, orders, order events, fills,
  positions, PnL, simulated execution and the embedded user-record store. The desktop holds command/view
  handles only; `account_runtime`, charts and UI surfaces never own or duplicate broker trading state.
- The desktop never constructs `SimulatedMarketObservation` or relays order-book prices for execution.
  `trading_runtime` consumes the market feed through its bounded inbox and its owner `QuoteBook`;
  desktop trading actions identify instruments and explicit order instructions, never authoritative quotes.
- Production `TradingService` has one owner-priced entry per placement and modification kind.
  It stamps instructions from the current verified owner quote; no public caller-priced
  duplicate or public direct market-observation command is allowed. In-crate legacy
  characterization tests may use only a `#[cfg(test)]` crate-private caller-priced seam.
- Simulated order admission and fills use the trading owner's verified session and current-generation Live quote.
  Crypto quotes may be at most 10 seconds old and other classes at most 60 seconds old by default;
  owner-local `eligible_after` fences resting orders after acceptance and across process restarts.
- Practice-account admission has its own 10,000-account bound; keep this value so stores
  written by released versions continue opening.
- Recurring contract-hour session projection lives in `aeris_instruments::calendar::weekly_hours`;
  market status and trading DAY recovery use that one IANA-zone/DST calculation. A historical
  DAY deadline is derived only from a verified route and the acceptance-time open session;
  unresolved legacy rows stay non-executable rather than receiving a guessed deadline.
- The trading service has one named owner thread whose loop is in `service/owner.rs`; it waits for bounded commands
  or a due deadline and drives synchronous `TradingCore::handle`, `on_market`, and `on_timer`.
- Desktop trading command handles come from the instance's `DesktopServices`, not a
  process-wide `TRADING_SERVICE` or `trading::handle()`/`trading::install()` lookup.
- Desktop trading command feedback belongs to the instance's `DesktopServices`; command
  writers and pane readers use its shared handle, never a process-global
  `TRADING_FEEDBACK` or `trading::latest_feedback()` lookup.
- Desktop pane command feedback reserves one of 64 in-flight or undrained
  request slots before the trading owner call. A full map refuses the command
  with one notice and no owner call. Completed requests use a pane-lifetime
  marker so a retired pane's result reaches the notice center, never a
  replacement pane that inherited its layout identifier.
- Pane trading notifications follow displayed dependencies: account balance,
  P&L and risk changes notify every pane selecting that account, even on
  another instrument; instrument-level changes notify only matching selections.
  Other accounts are not notified. Truly global trading notices use a bounded
  64-entry FIFO, dropping oldest with one counted loss notice per drain.
- Desktop trading instruction and pane alert timestamps use the instance's
  `DesktopServices` clock; do not restore `trading::now()` or a process-global
  desktop clock.
- Desktop account clients and refresh quiesce use the instance's `DesktopServices` account
  owner; no process-wide `DesktopAccount::shared()` or account-service singleton lookup.
- Desktop market consumers, bridge wakes and shutdown use the instance's `DesktopServices` market
  owner; no process-wide market runtime or `shared_market_runtime()` lookup.
- Desktop workspace persistence paths come from the instance's `DesktopServices` persistence
  owner, and chart chrome is one `WorkspaceState` section saved through the window's one
  workspace writer; the legacy chart-chrome file is read-only fold input to the migration
  chain and is never written again. The writer thread owns the restored document in memory:
  UI section updates arrive tagged with their layout generation, the writer replaces those
  sections and saves, and a save never re-reads the file (no read-modify-write). The
  readiness gate runs the full decode + migration + sanitize path as a write-free dry run.
  No separate chart-chrome writer, save receipt, process-global chart-chrome save state or
  implicit process-wide workspace path lookup may return.
- Saved layouts are owned by the desktop `LayoutLibrary` (bounded at 100; creating past the
  bound is refused, never evicts a user layout). Every workspace tab shows one saved layout, and
  a layout's chart drawings are one per-instrument `LayoutDrawings` set shared by every pane of
  every tab showing it. Panes publish committed edits into that set and import its newer
  revisions; panes never own or remember drawings, and no pane-to-pane drawing sync may return.
  Pane chart state persists no drawings. A layout with autosave off changes only on an explicit
  save and reopens at its last saved version after a restart.
- Trading orders and their client, broker, open-scope and executable indexes are owned together by
  `book::OrderBook`; inserts, status replacements and removals must update those indexes through that owner.
- Simulated fill price and quantity policies belong only to `trading_runtime::execution`;
  the current `TouchPrice` and `TouchLiquidity` implementations use the touch and full order quantity.
- Broker order submissions and transitions reserve capacity in the bounded venue request queue before
  committing their SQLite facts; the relay sees the request only after a successful commit. A full
  or disconnected queue rejects without a database write or compensating undo.
- Trading owner timestamps come from the injected `trading_runtime::clock::Clock`, with wall time clamped
  monotonically per process. Direct system time in trading production code belongs only in `SystemClock`.
- Symbol, timeframe, viewport, tab, and layout changes must not tear down a healthy provider session.
- `market_runtime` is the single owner that merges on-demand history and live state. Preserve one
  canonical forming candle, contiguous completed history, exact generation fencing, and explicit recovery.
- Retired clients, generations, selections, sessions, and publications must never mutate current state.
- `desktop_market_fences_stay_in_accept` scans desktop production for epoch/generation
  comparisons. Pane selection and publication acceptance belongs to `ChartPaneController`;
  unrelated release, app-command and receipt comparisons have exact reasoned, count-checked
  exceptions in `tools/naming_check`. An added comparison or a stale exception fails the gate.
  Failed pane replacements re-adopt the prior instrument and interval through the controller's
  restore target, not a provider-named workspace field. The independent market-summary worker
  checks its initial descriptor generation separately, at the exact reasoned `desktop.rs` exception.
- Production queues, caches, retries, and background work remain bounded with explicit overflow and
  cancellation behavior.
- The desktop must stay provider-neutral. It does not own provider adapters, storage implementations,
  or canonical candle state.
- Desktop provider identities are opaque runtime IDs looked up in
  `market_runtime::published_provider_descriptors()` and
  `built_in_provider_presentations()`. The runtime test
  `every_registered_provider_has_a_complete_published_descriptor` covers every registration;
  `tools/naming_check` rejects a desktop production `enum TerminalProvider` or
  `FALLBACK_TERMINAL_PROVIDER`. Only descriptors with `symbol_browser` enabled enter the
  symbol menu. An unregistered stored provider shows `UnsupportedProvider`, never another
  provider's market.
- `desktop_source_is_provider_neutral` scans production desktop and chart-integration Rust source
  for provider names and provider-specific ID formats; `Logarithmic` and `logarithm` are not
  provider identities. Exact fixture module paths compiled only under `cfg(test)` or
  `cfg(any(test, feature = "diagnostics"))` are outside the production scan, and the rule
  checks their gated declarations; moving the same identity to an ungated path fails.
  The only exception is the `assets.rs` logo mapping in `tools/naming_check`;
  no desktop provider logic may gain an exception. Replay conversion and chart legend projection
  use the bound instrument descriptor and published provider name.
  The bridge files `market_worker.rs`, `engine_market_worker.rs` and its `runtime.rs`
  need no provider-identity exceptions after M6.7; the deleted connection shell
  has no exception either. The final production exception is the logo mapping in `assets.rs`.
- Provider attribution marks and About notice lines come from published presentation
  descriptors only while the matching provider session is held, including recovery.
  Only `assets.rs` maps their logo keys to bundled light/dark artwork. Broker
  ticket venue names come from the matching broker venue's provider presentation.
- A fresh workspace persists one chart pane without a provider or instrument.
  The bridge sends `InstrumentRequest::Default`, adopts the runtime's resolved
  selection, and shows "Choose an instrument" without losing the workspace
  when the runtime has no default. Explicit saved instruments restore as
  `InstrumentRequest::Ref`; wire fields retain their existing numbers.
  `desktop_control_flow_does_not_match_display_text` rejects desktop
  production `if`/`match` conditions comparing known notice literals and
  message constants with `starts_with`, `contains` or `==`. Use typed outcomes
  rather than branching on display text.
- `FixtureMarketWorker` lives only under `apps/desktop/src/test_support/`; the module is
  compiled only for tests or the explicit `diagnostics` feature. Default desktop builds cannot
  construct fixture market publications. The diagnostic conformance command is gated likewise.
- The embedded demo replay (`aeris_application::EmbeddedReplaySource`) and the demo chart
  constructor (`AerisChartView::new` and `Default`) compile only for tests or the `test-support`
  features of `aeris_application` and `aeris_chart_integration`. Only dev-dependencies, the chart
  integration's own `test-support` feature and the desktop `diagnostics` feature may enable those
  features; `embedded_demo_replay_is_test_support_only` checks every workspace manifest.
- The desktop stores `ChartInterval` and must not construct `SeriesCadence`; interval-to-cadence mapping
  belongs to `market_runtime`.
- Market history is loaded on demand and bounded; do not restore market-history persistence or a
  second desktop-owned market-state model.
- `aeris_contracts` holds only the persisted workspace schema: the prost messages in
  `messages.rs`, including the `InstallProviderInstrument` and `SeriesKey` identities a saved
  pane stores. In-process catalog, status and provider-presentation types belong to
  `market_runtime` (`catalog.rs`, `status.rs`, `provider_presentation.rs`) and account views to
  `account_runtime::view`. Only the desktop and `market_runtime` may depend on `contracts`;
  `contracts_hold_only_the_persisted_workspace_schema` checks the files, the prost derives and
  every manifest.
- Do not restore `apps/engine`, `local_engine_client`, a secondary market process, local process transport, or market autostart.
- UI-thread code performs no blocking network, disk, process, or shutdown work. Background workers do
  not mutate GPUI state directly.
- `unsafe` is isolated to target-specific FFI under `crates/platform_sys/src/windows/` and
  `crates/platform_sys/src/macos/`, whose blocks require documented, single-operation safety
  justifications. Every other crate inherits the workspace `unsafe_code = "forbid"`; only
  `platform_sys` may lower it in its manifest. The one exception is the pinned GPUI component
  base `third_party/gpui_base` (a workspace member with its own `unsafe_code = "deny"` and
  `clippy::all` lints), whose macOS Objective-C bridges `src/macos_accessibility.rs` and
  `src/input/base/native.rs` alone allow it. `platform_runtime` keeps the safe event streams
  over the `platform_sys` power, network and session-shutdown registrations.
- `platform_sys` is a leaf crate. Only `platform_runtime`, `trading_runtime`, and `observability`
  may depend on it directly.
- Normal and build dependencies follow the layer order that
  `crate_dependencies_follow_the_direction_rules` reads from every `Cargo.toml`. Leaf crates
  depend on no workspace crate; the domain crate only on leaf and domain crates; platform crates
  and adapters only on leaf, domain and platform crates; runtimes also on adapters and runtimes;
  presentation crates (`application`, `chart_integration`, `terminal_ui`) never on a runtime or
  adapter; and nothing depends on a composition root (`desktop`, `aeris_probes`,
  `release_packager`, `naming_check`). Every workspace package needs a layer there.
  `trading_runtime` never depends on `market_runtime`, `market_engine`, `platform_runtime` or an
  adapter; `market_runtime` never depends on `trading_runtime` or `study_sdk`; only a composition
  root depends on both runtimes; and `rusqlite` belongs to `trading_runtime` plus the probes'
  read-only store check. `study_sdk -> market_runtime` is the accepted published-API facade.
- `release_lifecycle` (`crates/release_lifecycle`) owns the signed install, update, rollback and
  uninstall lifecycle, release delivery plans, rollout policy, the compiled release identity, the
  release origin layout (`origin_layout.rs`, shared with the packager) and the launcher
  (`launcher.rs`, run by the thin stable `aeris_launcher` binary). The launcher reads the origin
  only through the `ReleaseSource` seam (`HttpsReleaseSource` in production); `platform_runtime`
  carries none of it and no `ed25519-dalek`,
  `same-file` or `sysinfo`. Only the desktop and `tools/release_packager` may depend on
  it. The manifest and block-plan signers and their `UnsignedReleaseManifest` and `UnsignedBlockPlan`
  (which hold the exact signing message until a signature is attached) compile only under
  `cfg(test)` or its `producer` feature,
  which only `tools/release_packager` and the crate's own dev-dependency (for the launcher tests)
  enable. `platform_runtime::data_root` owns the owned-root safety check and legacy data-root
  migration that the lifecycle reuses. The launcher parses its command before loading the release
  verifying key: `--launcher-identity`, `--remove-application` and `--remove-all-local-data` never
  load it, and `ReleaseUninstaller` holds no key. The lifecycle installer (`tools/windows/aeris_setup.iss`)
  asks once before uninstalling, with "Remove my Aeris data" unchecked: the installed-apps
  uninstall command it registers is `/SILENT /INTERACTIVE`, which skips Inno's own confirmation
  box but still shows that choice. By default (and always for a silent uninstall without
  `/INTERACTIVE`) it runs `--remove-application`, which keeps every data, cache, log and legacy
  root and vault entry; only a ticked box or a pending full uninstall runs
  `--remove-all-local-data`. It carries no Axiusflow cleanup. Its launcher `--install` runs in the
  last file's `AfterInstall`, before any shortcut or registry entry. Inno Setup discards
  exceptions raised there and cannot roll back from it, so a failed install restores the launcher
  Setup replaced, skips the shortcuts and launch entry (`Check: BundledReleaseActive`) and exits
  with the custom code 20; a failed upgrade of a direct 1.x installation keeps its
  `aeris_desktop.exe` and 1.x shortcuts. The shortcuts share the 1.x names and point at the launcher. The desktop spawns launcher promotion only on a plain start of
  an installed release (install generation above 0) and reaps that child. While a retained
  known-good release exists, a release generation stays on probation until a desktop it started
  records the install-root `.startup-complete.json` marker (generation, release identity, process
  ID; written off the UI thread once its first sign-in or workspace window opens, never on a
  validation root). The launcher waits up to 60 seconds for its own child's marker; an
  unsuccessful exit or no marker in time (the child is stopped) quarantines the release and rolls
  back. A marker for the active generation from an earlier launch ends probation; the launcher test
  `probation_ends_on_the_child_marker_an_exit_or_the_bound` covers that bound. The launcher waits on and
  detects processes through `platform_sys::process` (re-exported by `platform_runtime`), never
  sysinfo, and uninstall renames the running launcher to `aeris_launcher.retired-<pid>` beside the
  install root; later launcher starts and the uninstaller delete it by that exact name, never
  through a generated batch file. An update runs readiness on a copy of the user's data and
  journals `ReadinessPassed` before the active pointer moves, so recovery never runs candidate
  code; recovery also moves entries no release record owns into `orphaned/<ms>/` (newest 8
  batches kept). The `--update-and-restart` helper audits the active release once and checks the
  prepared release and the current channel offer before it prints READY; after READY it only
  waits for COMMIT and the desktop's exit. The desktop waits for READY up to
  `UPDATE_RESTART_VERIFICATION_TIMEOUT` (2 minutes). The launcher's `RestartHandshake` seam tests
  (`restart_verifies_everything_before_ready_then_only_waits` and its siblings) prove that order.
- `tools/release_packager` (`aeris_release_packager`) packages one signed release into an
  origin-shaped output tree (payload executables, rollback-compatibility data, signed manifest and,
  against a predecessor, a signed block plan), writes the channel pointer and prints the Inno Setup
  defines. It signs through one `ReleaseSigner`: an Ed25519 key read only from a key file through
  `read_signing_key` (zeroized), never the environment, standard input or the command line, or
  the AWS KMS key named by `--kms-key <key ARN>` (`alias/aeris-release-signing` in the website's
  `AerisReleases` stack), whose private half never leaves KMS. Its only process is `aws kms`
  (`get-public-key`, `sign` with `ED25519_SHA_512` over the raw message) in `signer.rs`, and
  every KMS signature is verified against the key before use. It has no network dependency and
  publishes nothing; `tools/naming_check` enforces this. The rollback-compatibility data
  records the payload's `max_store_schema` (`aeris_trading_runtime::SCHEMA_VERSION`). Before a
  crash rollback the launcher reads the live trading store's `user_version` from its SQLite header
  (a write-ahead log holding frames counts as the failed release's schema) and leaves the failed
  release active instead of restoring one that cannot open that store. Its `lifecycle_e2e` test runs
  `run_launcher` against a local `DirectorySource` through install, update, readiness rejection,
  block-plan delivery, activation, probation, crash and hang rollback, quarantine and a rollback
  refused past a migrated store. The installed desktop and launcher
  there are copies of the `aeris_lifecycle_fixture` binary; never copy a test executable into a
  release, because antivirus heuristics quarantine a program that copies and runs its own image.
- The release packaging variables (`AERIS_RELEASE_VERIFYING_KEY`, `AERIS_RELEASE_BASE_URL`,
  `AERIS_BOOTSTRAP_MIN_GENERATION`, `AERIS_RELEASE_IDENTITY`, `AERIS_INSTALL_GENERATION` and the
  optional, Windows-only `AERIS_AUTHENTICODE_CERT_SHA1`) are read only by
  `crates/release_lifecycle/build.rs`, which validates them with `release_config_rules.rs` and
  generates the static behind `release_lifecycle::release_config()`. None set is a development
  build; any set requires every other one valid, or the build fails. Code reads the
  configuration through `release_config()`, never `option_env!`, and no workflow may set them.
  Releases ship without Authenticode until a code-signing certificate exists: a Windows release
  without the thumbprint audits installed executables by their signed manifest digests alone (as
  macOS does), and `verify_windows_publisher_signature` fails closed, so a stable launcher that
  differs from the active release is not trusted. With the thumbprint set, Windows Authenticode
  audits call `platform_sys::trust::verify_publisher` (in-process `WinVerifyTrust` on the
  embedded signature, the configured signer SHA-1 and a timestamp countersignature; no revocation
  check or network fetch), never a spawned PowerShell.
- Secret-bearing types whose names contain `Token`, `Secret`, `Password`, or `Credential` must not
  derive `Debug`, and those values must not use debug formatting. Only the exact reviewed,
  non-secret or transitively redacting wrappers in `tools/naming_check` are exceptions.
- Durable file replacement is owned by `platform_sys::fs`: staging uses a unique temporary in the
  destination directory, synchronizes file contents before replacement, and synchronizes the
  parent directory on Unix. Production code must use this writer instead of raw `fs::write` or
  `fs::rename` except at the exact move/build/platform implementation sites allowlisted by
  `tools/naming_check`. Do not add another atomic-file FFI implementation.
- The workspace file (`workspace-state.pb`) persists only through
  `platform_runtime::DurableContainer`, the one container implementation, built on
  `StagedFile`: `magic | envelope_version | schema_version | len | sha256 | payload` with a
  12 MB payload bound. A write first copies a valid current file to `<name>.bak`; loads try
  the primary, then at most four exact staged temporaries newer than the backup (from a
  bounded 1,024-entry directory scan), then the backup. A recovering load moves an unusable
  primary to `<name>.corrupt-1`, keeping the newest three; the readiness gate inspects
  read-only. The magic starts with NUL, which no released payload (protobuf, `key=value`
  text) can, so a file without it opens unchanged as a legacy payload and is enveloped on the
  next save. Workspace documents migrate through the ordered chain in
  `desktop/workspace_migrations.rs`: the legacy `workspace-state.frame` is document schema 0,
  a released 1.x build's bare payload carries its document schema in the payload
  `schema_revision` (never past 2, so a bare payload past that is rejected as
  unrecognizable), and the envelope schema is authoritative once present. The chain runs
  before the sanitizer on every load and is persisted at the first current-schema write,
  which preserves the pre-migration payload as `workspace-state.pb.pre-v3` exactly once (the
  one raw `fs::write` allowlist entry in `local_state.rs`) and keeps the frame file. The
  final chain step folds the legacy chart-chrome `key=value` file into the document's
  `chart_chrome` section: the file is read once through `DurableContainer::inspect` (never
  quarantined, moved or rewritten, and left in place afterwards), a missing or unusable file
  leaves the section at its defaults, and a document that already has chrome keeps it. An
  envelope whose schema is newer than the build's newest supported schema is a
  newer-version document, not corruption: a load reports it without quarantining the file or
  offering an older copy, a write never replaces it, the workspace opens read-only for the
  session with a persistent notice, and the readiness gate refuses it. Restore outcomes the
  user must see (fallback restore, defaulted layout, newer-version read-only) are persistent
  `UserNotice`s with an Open folder action through
  `platform_runtime::reveal_in_file_manager`.
  `the_workspace_persists_through_the_durable_container` rejects a
  second container or a direct writer, `.bak` or `read_to_string` path in the workspace
  file's owner and any container or file writer in `chart_chrome.rs`.
- Trading SQLite persistence is organized under `trading_runtime/src/store/` by table family;
  `schema.rs` retains the V1–V30 migration sequence. V20 adds nullable verified market routes;
  migrated legacy instruments remain route-less until verified Terms arrive. V21 backfills
  `fill_accounting` and `trade_cycles`; V22 adds lifecycle columns and V23 adds
  `order_groups` and `order_group_legs`. V24 drops the legacy `managed_brackets` and
  `protective_orders` tables; it is applied by the gated `finalize_group_migration` step (after
  the group backfill and the runtime conversion of awaiting brackets), never by the migration
  chain, so a failed conversion keeps its rows and retries on the next start. V25 adds the
  `ledger_entries` cash ledger, per-account `fee_schedules`, the exact position cost-basis
  columns and the denormalized instrument class/price-grid columns; its backfill runs once per
  store after the group finalize as part of `complete_post_group_migrations`. A simulated
  account's balance is the exact sum of its cash-ledger entries; only a gated pre-group store
  awaiting conversion keeps the legacy starting-equity math until its backfill. V26 adds
  `position_checkpoints`, `ledger_checkpoints` and the `equity_samples` history; retention
  folds each pruned fill prefix and its ledger entries into those checkpoints inside the
  deletion transaction, and recovery rebuilds a checkpointed position as checkpoint plus the
  retained fill suffix. Each simulated fill carries one equity sample inside its commit
  transaction, the 60-second owner timer adds a sample only when equity changed plus one at
  each risk session boundary, and a 1,440-entry in-memory ring per account is rebuilt from
  the durable history at open; retention downsamples minute samples older than seven days
  into last-of-hour rows and drops hourly rows older than a year, with both windows trailing
  the newest sample so a backward clock correction cannot mass-expire history. V27 adds
  `risk_sessions` (seeded per registered risk profile from the registration baseline) and the
  `risk_locks.kind` column; the owner timer rolls each account's session at its configured
  boundary (UTC midnight by default, 17:00 America/Chicago for futures) in one transaction that
  persists the rolled row with the realized baseline, resets the daily meter, releases only
  daily-meter locks (manual locks survive) and writes the boundary equity sample. V28 makes broker
  accounts netted futures accounts: broker fills run through the same `positions` ledger as
  practice fills, so it drops the hedged `broker_positions` table and
  `broker_deals.broker_position_id` and adds `broker_bracket_levels`, the server-held target and
  stop levels kept with their entry order. A venue position report sets only the open P&L; a
  reconcile snapshot that disagrees with the ledger rebases the position in one transaction.
  Broker accounts write no cash-ledger entries (their balance is the venue's), and a broker venue
  declares its `VenueCapabilities` (held bracket levels, client order id length) when attached.
  V29 adds `broker_unresolved_contracts` and `broker_pending_reverses`. A broker account admits no
  new risk-increasing order until the current venue session has reconciled it, nor while it holds an
  unresolved contract (a fill or position the owner could not account, which also lowers the
  reconcile replay floor); cancels, exits and flatten stay allowed. Broker mirror order IDs come from
  the owner sequence, a fill before its ack binds the unbound order by client id, and a reverse sends
  only its exit until the position is flat; a reverse still waiting at restart is dropped with a
  recovery notice. Venue refusals arrive as `VenueUpdate::OperationFailed` and become bounded owner
  `VenueNotice`s (32) shown on the desktop feedback line; every relay command has a 30-second
  deadline, and an unanswered command forces a reconcile.
  V30 indexes account, execution time and fill ID for read-only closed-trade
  keyset pages; migrated exits without recorded P&L are identified by a
  reduction of the position's prior signed quantity and keep unknown P&L.
  Old P&L tables and per-fill columns remain unread until their deferred removal migration.
  Store-open preflight scratch cleanup may remove only exact preflight directories containing
  solely the expected database copy and SQLite sidecars, and exact readiness directories
  containing solely that copy, its migration backups and its preflight scratch; preserve
  unknown contents and report cleanup failures through observability.
- Candidate readiness (`aeris_desktop --desktop-readiness`) starts the trading owner only on a
  `trading_runtime::ReadinessDatabaseCopy`: the database file and its sidecars are copied
  beside it under the instance lock without opening the original through SQLite (even a
  read-only connection creates WAL sidecars), so the user's `trading.sqlite3` keeps its bytes
  and `user_version` and a fresh install never creates it. The desktop writes, and the launcher
  accepts, the one `aeris_release_lifecycle::ReadinessReport` (schema 6, durable write);
  `provider_count` counts registered market providers, needs no sign-in or network, and must
  be positive. `tools/naming_check` checks the copy and the shared type.
- Simulated quote-only marks update unrealized P&L in owner memory without cloning all trading
  state or writing SQLite. Retention runs only from the owner timer at startup and hourly; each
  run writes position and ledger checkpoints in the same transaction before pruning fills or
  ledger entries (a crash leaves neither), never deletes rows referenced by an open order or a
  live order group, and deletes at most 5,000 rows from each table per run. Commands, quotes
  and store open never perform retention.
- Trading consumers subscribe through `TradingService::subscribe` instead of polling snapshots:
  the owner publishes a typed `TradingEvent` carrying the bounded `WorkingSet` after every
  revision-changing step, coalesces quote-only mark changes to one publication per 100 ms with
  a trailing flush, and stays silent while idle. At most eight live subscriptions exist, each
  with a 1,024-event slot that overflows into a `lagged` resync with a fresh working set; paged
  order and closed-trade history are served by the `aeris-trading-history` thread on a
  read-only SQLite connection with at most 500-row keyset pages. Trade pages are scoped
  to one account and use exit time and exit fill ID as the stable cursor; they project
  exact per-exit P&L through the same round-trip projection as the owner, using at most
  500 fills per historical cycle prefix. The working set projects registered terms only for
  open orders, open positions, its fills window and ticket interests; its newest event and fill windows each
  have a 500-row limit, and round trips use at most 500 fills plus open positions. Every
  closed round trip carries its durable exit fill ID, distinct even for partial exits of
  one cycle; an open round trip has no exit fill ID. Active
  bracket/protection views come from a live group-id index maintained with group transitions,
  never a scan of terminal group history. Broker connection, risk profile/meter, session plan
  and market observation error projections remain owner-owned.
- `TradingSnapshot`, `TradingService::snapshot`, its command and ledger construction are
  available only under `cfg(test)` or the explicit `trading_runtime/test-support` feature;
  only dev-dependencies may enable that feature (`trading_runtime_test_features_are_dev_only`
  checks every manifest, including `[features]` forwards). Desktop production and probes use
  subscription working sets and paged history, never the snapshot API.
- Desktop production trading reads use the subscription-backed `TradingStore`, never
  `service.snapshot()`, `trading_service.snapshot()` or `trading().snapshot()` on the owner.
  `desktop_trading_reads_come_from_trading_store` checks these calls under `apps/desktop/src`.
  Trading updates are applied on subscription wakes, not a render-path or 250 ms timer.
- The desktop bottom panel keeps recent fill-based round trips as its live trade head.
  It requests owner-projected historical closed-trade pages per account through the
  read-only history worker on a background thread, merges accounts newest-first and
  deduplicates by exit fill ID against the live head. Historical P&L is shown as received
  from the owner, including unknown values. At most four 500-trade pages are retained,
  with a visible cap; account changes and lagged subscription resync fence old completions
  and reload the first page.
- The `trading_runtime/test-observability` feature is enabled only by the desktop's
  dev-dependency (the same `naming_check` rule) to count calls to the instance's real `TradingService::history` API
  in idle regressions. Default production builds carry no history-read counter.
- Validation runs use `platform_runtime::data_root` isolation and an exclusive per-root instance
  lock. They must not overlap the production data root, probe production vault entries, migrate
  legacy state, write installed-app identity, promote launchers, or run updates.

Architecture assertions in `tools/naming_check` are authoritative when they are stricter than prose.
Its source scans strip only balanced `#[cfg(test)]` module bodies, so production code after a test
module is still scanned, and every allowlist entry must carry a written reason; stale entries fail
the check.

## Market-data and provider work

- Keep the four coordinator command/event characterization scenarios under
  `market_service/tests/characterization.rs` and the deterministic market replay scenarios under
  `market_service/replay/mod.rs`; lifecycle refactors change their expectations deliberately and
  never delete a scenario to make a gate pass.
- Live market probes run only from `tools/aeris_probes` on an isolated validation root with an
  exclusive instance lock. The optional `probe` feature's bounded Hyperliquid fault control
  interrupts only its WebSocket; never use a process-wide proxy that blocks catalog HTTP.
  `paper_trading_probe` exercises the practice trading lifecycle on the public Hyperliquid feed.
- Live market gate evidence (`aeris_platform_runtime::LiveMarketGateRecorder`) compiles only under
  `cfg(test)` or the `aeris_platform_runtime/live-gates` feature. Only dev-dependencies, the Rithmic
  adapter's `live-gates` feature (required by `rithmic_test_smoke`) and the desktop `diagnostics`
  feature may enable it; `live_market_gate_stays_out_of_production_builds` enforces this.
- The trading owner's quote-interest revision is durable store metadata: the market coordinator
  fences stale revisions for its whole lifetime, so a trading-owner restart must resume above the
  fence, and a provider reconnect republishes trading sessions before its `Feed::Live`.
- Coordinator-owned watchdog, history retry and trade-handoff timestamps use the injected
  `market_service::clock::Clock`; direct system time belongs only in `SystemClock`.
- Provider reconnect scheduling uses the bounded `market_service::reconnect::Backoff` policy;
  the Rithmic adapter alone retries transport, while its non-transport session replacements
  wait at least one second. Never stack a coordinator transport retry on that adapter.
- Every Hyperliquid info request spends from the adapter's one process-wide weight budget
  (`hyperliquid_market/src/budget.rs`: 1,000 of the documented 1,200 weight per minute per address);
  never send an info request around it.
- Generic market-service lifecycle, history, calendar, quote, outbox, coordinator, realtime,
  runtime and publication code must not contain provider identity literals. Provider-specific
  capabilities and user-facing presentation metadata are declared with provider registration;
  provider wire aggregation belongs to its adapter, not `ChartInterval`.
- Market session and cadence decisions use the provider-neutral market calendar. Exchange
  codes share one normalization table; generic calendar code contains no provider identity
  literals and does not import an adapter-owned exchange calendar. Unknown week/month
  boundaries reject demand explicitly rather than installing a silent live series.
- Dated provider calendar updates enter the coordinator behind the current provider-generation
  fence and expire into the instrument's recurring calendar or Unknown. Session, week and month
  trade-built bars and tape trading dates use that same calendar, not fixed daily seconds.
- Provider events and history completions carry registry-minted `Epoch<ProviderTag>` identities;
  the coordinator rejects retired epochs by equality and counts their drops. Adapter wire
  generation numbers never serve as market event identities.
- Every runtime-to-consumer market event variant leaves the coordinator inside a
  `Fenced<ConsumerTag, MarketRuntimeEvent>` envelope for that registered consumer lifetime;
  selection-bound events are rejected against the exact current demand before delivery.
- Trades, aggregate depth and order levels are separate additive streams. Aggregate depth comes only
  from a provider's price-level feed, never from order-level data. `MarketStream::OrderLevels` is
  explicit consumer demand (depth demand alone never subscribes it); the coordinator admits at most
  the provider's `maximum_order_level_instruments` and reports the rest as `BudgetExhausted` without
  evicting an admitted instrument. A gap or bound violation empties the bounded `MarketOrderLevelView`
  until a covering provider image arrives, and order levels are never persisted.
- Trade-tape sweep classification lives in `market_runtime`
  (`market_service/trade_sweeps.rs`): it runs once per ingested trade (and on every retention
  eviction or session reset) over the trailing 512 retained prints with the 90th-percentile
  size threshold and 100 ms grouping, so consumers never reclassify the tape. The coordinator
  publishes fenced `TradeTapeUpdate` events carrying
  `MarketTradeTapeOperation::Reset { trades }` / `Append { from_ordinal, trades }` and the
  current sweeps; a consumer's bounded 64-entry update queue overflows into one covering
  `Reset`, and a new selection generation, provider session, prefix rewrite or eviction gap
  restarts the stream with a `Reset`. The desktop's `OrderFlowPresenter` applies the stream
  into a ring bounded at the runtime's `MAXIMUM_RECENT_LADDER_TRADES` and projects only the
  covering window or the appended suffix into the chart.
- Provider catalog controls and responses pass through one bounded `ProviderCatalogChannel`
  boundary; provider-specific command translation stays with each registered channel.
- Catalog callbacks pass through coordinator-owned `CommandFences`, keyed by consumer, provider
  and command kind. Startup resolution uses its own kind, not a numeric generation sentinel;
  consumer retirement clears every pending command through `ConsumerRegistry::remove_consumer`.
  Every search and selection carries a 45-second response deadline after which the coordinator
  rejects it as timed out, and a catalog rejection names its search or selection lane explicitly;
  consumers never infer the lane from the rejection reason.
- Symbol search keeps one catalog-search slot per consumer and provider. The Rithmic catalog runs at
  most one search at a time, keeps only each consumer's latest queued search (rejecting the replaced one
  as superseded) and caches its product listing once per session generation (at most 128 products).
- Chart selections, including startup and restored instruments, resolve and install demand through
  the coordinator's `select_and_demand` command; desktop code never owns startup catalog resolution
  or interprets provider display text as a readiness signal.
- `HistoryTracker` alone owns per-series current/backfill lanes, retry deadlines, empty coverage,
  detached windows and cancellation. History requests and completions carry the same exact
  provider/demand/sequence ticket; retirement prunes the tracker, never parallel coordinator maps.
  When a trade-built series' covering request exhausts its bounded retry budget, the runtime opens
  a live-only series from the trade stream with the provider-neutral `HistoryUnavailable`
  presentation and keeps one slow probe (`HISTORY_UNAVAILABLE_RETRY_DELAY`) armed instead of
  restarting the fast retry budget; a later covering snapshot merges under the trade-built bars and
  reseeds the live handoff from canonical. An exhausted backfill lane keeps its failed cooldown
  until fresh demand.
- Candle series control and the single forming bar belong to `market_service::lifecycle::SeriesLifecycles`.
  Retired candle handlers must not return, and lifecycle transitions use the injected clock rather than system time.
- Stall recovery considers every live candle and trade series while its calendar is open;
  closed sessions never recover for silence, and unknown sessions use a longer threshold.
  A trade-built series counts a newer quote on its instrument as progress, and its threshold
  doubles after each stall recovery that no trade followed, up to eight minutes.
  Practice session checks use the same calendar for every provider.
- Verify protocol fields, precision, timestamp meaning, ordering, snapshot semantics, and recovery
  against current provider documentation or representative observed responses before depending on them.
- Keep provider identity, local ingestion order, and continuity evidence separate. Never infer sequence
  semantics from an opaque identifier or hash.
- Use fixed-point values with explicit scales and provenance. Do not round provider data merely to fit
  an existing assumption.
- Required wire fields must be validated, not silently defaulted.
- Tests should use minimal sanitized fixtures faithful to real shapes. Never store credentials, account
  data, or raw private payload dumps.
- Rithmic production credentials never enter source, chat, logs, CI, or environment files. Native vault
  storage is the credential boundary. The market runtime publishes provider-owned static login fields
  through `ConnectMethod::VaultCredentials` and exposes provider-id-keyed saved summary, sign-in,
  forget and server/system discovery; the Rithmic implementation alone verifies and writes its
  native-vault entry. Submitted field values never derive `Debug`. Live/test provider sessions must
  respect provider concurrency limits.
- Desktop connection rows come from `published_provider_descriptors()` exactly once per registered
  provider. `desktop/provider_login.rs` renders the published vault-login field schema and
  calls only provider-id-keyed market-runtime login methods; the desktop never imports a
  provider-specific login summary/server or writes a credential outside the runtime's vault seam.
  `tools/naming_check` keeps this component in the native-vault and rendering-boundary checks;
  its former Accounts-panel, login-form and workspace-surface identity exceptions are removed.
- Broker orders reach Rithmic only through the venue relay in `market_runtime::rithmic_venue`, started
  once by `MarketService::attach_broker_venue` on its `aeris-broker-venue` worker. It reads the same
  vault login, routes only demo logins (the Rithmic Test gateway and Rithmic Paper Trading) and refuses a
  live login before opening an order plant. Each connection attaches a new venue session generation with
  Rithmic's `VenueCapabilities`, so the owner reconciles every account after a reconnect; the desktop
  only connects the trading owner's attach and inbox handles to it.

The licensed Rithmic proto kit (the R|Protocol `proto` schema folder) remains outside Git. Its
canonical local copy is `C:\rithmic_proto_kit\proto`; the repository reaches it through the ignored
`rithmic_proto_kit` junction, which the adapter's build script reads as `rithmic_proto_kit/proto`.
Do not vendor or modify that permanent copy.

## Operating constraints

1. **Local-first, no server in the market-data path.** No Aeris server receives, stores, processes
   or relays market data, orders or account state, and no feature may depend on one.
2. **Exchange data stays on the user's machine.** Broker data is processed only in the desktop
   process. Aeris must not redistribute exchange data or its derivatives.
3. **Public non-exchange data is fetched directly from its official source**, with the user's own
   free API key where required.
4. **User-owned data is stored locally.** Optional sync uses a folder the user controls.
5. **Provider access is gated.** Features that need a gated provider (Rithmic) are designed now and
   qualified after onboarding; anything buildable on public data or the simulated venue proceeds
   first.

### Data licensing checklist

A release gate, not legal advice. Before a feature that stores, derives or transmits exchange data
ships, confirm against current CME policies, the Rithmic agreement and any broker agreements:

- Local storage of recorded trades and depth for the subscriber's personal replay and review.
- Display of derived values (queue position, detections, footprint and profile statistics).
- Alerts leaving the machine carry event descriptions, not exchange prices or quantities, unless
  explicitly allowed.
- Professional versus non-professional subscriber status.

Record the reviewed policy versions in the feature's release notes.

## Aeris Charts

Aeris Charts is a separate pinned Git dependency. Do not clone or vendor it into this repository.
Host integration belongs in `crates/ui/chart_integration`. When intentionally updating Aeris Charts, change
only the `aeris_charts_*` revisions required by that update; do not update GPUI incidentally.
`aeris_charts_revisions_are_identical_and_locked` in `tools/naming_check` requires every
`aeris_charts_*` workspace dependency to carry one git source and revision and `Cargo.lock` to
resolve every `aeris_charts_*` package to it, so a bump changes the revisions and the lockfile together.

Chart behavior is owned by Aeris Charts, never re-implemented here:

- `crates/ui/chart_integration` is product glue only: the market-data bridge, trading, alert, study, and
  order-flow projections from the runtimes, design tokens, legend and menu UI, and workspace persistence.
- Pointer, wheel, and keyboard input reach the chart through `aeris_charts_render_gpui::input` and the
  engine's `input_*` controller, one adapter call per GPUI listener. Never add gesture routing, hover or
  cursor arbitration, key bindings, drawing-tool mirrors, host-side drawing state (locks, selection,
  creation), brush styling, or other engine-owned state to Terminal. `tools/naming_check` rejects the
  engine's low-level interaction primitives in this crate.
- Chart requests to the host (pane activation, context menu, settings, study removal) leave
  `AerisChartView` as `ChartHostEvent`s (`view/host_events.rs`) that the desktop subscribes to per
  chart. `chart_host_requests_are_emitted_events_not_pending_polls` rejects chart-side `pending_*`
  request fields and `take_*_request` drains in chart integration and desktop production source.
- Hollow candle bodies are the engine's `CandleBody` series style: Terminal selects it through
  `set_candle_body` and supplies only theme colors through the appearance APIs; a hollow direction
  leaves its body unfilled while the frame and wick carry the direction color.
  `chart_integration_candle_bodies_are_engine_owned` rejects the retired host transform names
  `hollow_appearance` and `hollow_base` in chart integration and desktop production source.
- Order-flow sweep classification and tape continuity are runtime-owned: chart integration's
  `apply_order_flow_trades` takes the runtime's `Reset`/`Append` decision as a
  `OrderFlowTapeChange` and never reclassifies the tape or diffs ordinals.
  `chart_integration_has_no_order_flow_analytics` rejects the retired host names
  `classify_order_flow_sweeps`, `order_flow_resume_ordinal` and `invalidate_order_flow_prefix`
  in chart integration and desktop production source.
- When a chart feature needs more from Terminal than data, product policy, or a platform-only effect, the
  missing operation belongs in Aeris Charts. Add it there first (a coordinated two-repository task), push
  it, then bump the pinned revision here; never compensate with Terminal-side wiring.
- After bumping Aeris Charts, check for engine capabilities Terminal is not yet using (for example a new
  controller behavior or host request) instead of assuming the pin bump alone delivered them.

## Authentication and website coordination

Desktop sign-in is mandatory and uses the Aeris Cognito user pool (`us-east-1_05pNG4EOF`, public
native client `tlnlg2amke4k2v9v2k84vvg87`, hosted sign-in at `https://auth.aeristerminal.com`). These
values are compile-time constants in `account_runtime` (`AccountServiceConfig::production()`); no
environment variable selects an issuer. The runtime discovers endpoints from the exact issuer, uses
PKCE with `scope=openid email profile` and a loopback redirect `http://localhost:<port>/callback` on
the fixed ports 47210, 47211 or 47212 (bound on both `127.0.0.1` and `[::1]`), and verifies the
RS256 ID token with `ring` against the issuer JWKS (issuer, audience, `token_use=id`, expiry, nonce).
Only the refresh token and the cached profile (`sub`, email, name, picture) are stored, in the native
vault; there is no lease, device key or entitlement, and every signed-in account shows the
"Unlimited" plan. The account runtime downloads the profile picture itself on a background thread
(`https` only, at most 1 MiB, a 15-second timeout, PNG/JPEG/GIF/WebP by signature, one retry per
address after five minutes) and publishes the bytes in `AccountView`; the desktop renders them as a
GPUI image and installs no GPUI HTTP client. An unreachable provider keeps a saved session signed in (Offline) with bounded backoff;
only a rejected refresh grant signs out. Sign-out revokes the refresh token, deletes the vault entries
and returns to the sign-in gate. `apps/desktop/src/account/mod.rs::sign_in_required` is the one policy:
every process except a validation data root installs the account session. Before the first usable
session (signed in or the cached Offline one) only the local trading store, the account session and
persistence paths exist: the desktop entrypoint shows the sign-in window, and
`start_signed_in_runtime` (market runtime and provider sessions, context sources, the
market-demanding workspace, the trading market feed and the broker venue relay) runs only when the
`services::SignedInStart` gate releases it, exactly once per process; a validation root runs it
directly. After a later sign-out the workspace window renders `desktop/sign_in_gate.rs` (no
workspace handlers or trading hotkeys attach behind it), but provider sessions and the venue stay up
until exit because `MarketService::shutdown` is final and cannot restart in-process; the next sign-in
starts nothing new. `tools/naming_check` keeps these owners out of the entrypoint. Tests must not call `start_restore` or touch
the native vault; use injected vaults and `AccountService` test seams. Manage Profile opens
`https://aeristerminal.com/account/`.

The desktop `DesktopAccount` session paces its own status fetches on its poller thread and runs
weakly held change wakes (at most 8) after each presentation change; the sign-in window and
workspace windows re-render on those wakes and never poll it. Plan limits come only from
`account::plan_policy()` (unrestricted for every account); workspace code never defines product
limits, and its 8-tab and 64-pane bounds are resource bounds. `tools/naming_check` rejects window
account polling and the removed plan placeholders.

The sibling website is at `C:\Users\devraj\Downloads\Softwares\aeris-website`. Inspect both repositories for future auth/profile/subscription changes, and preserve unrelated edits there. Keep tokens and credentials out of UI and logs.

## Design system coordination

`crates/ui/design_system/platform.css` and the Rust tokens in `crates/ui/design_system/src/lib.rs` are
the native platform design-system source. The website keeps a mirrored portable stylesheet at
`src/styles/platform.css` in the sibling repo.

When changing shared platform tokens such as colors, typography, radii, or interaction states:

1. update the native source and Rust token mapping together;
2. update the website mirror in the same coordinated task;
3. run the website sync/tests so auth receives the same stylesheet;
4. do not reintroduce one-off duplicate platform tokens in landing/auth/account CSS.

Theme presets are the `[data-theme="<id>"]` blocks in `platform.css`, compiled into `ThemePreset` and
`AerisTheme::resolve`; the desktop opens in `AerisTheme::initial()` (High Contrast, dark), while Aeris
stays the stylesheet's `:root` palette; `themes.md` is their palette source and the preset blocks must match it.
The popover layer (`--surface-popover`, `--border-popover`, `--border-popover-highlight`) is
Aeris-only: it lives solely in `:root:not([data-theme])` and `.dark:not([data-theme])`, the
design-system build rejects it in any other block, and presets resolve it to the CSS fallbacks
(`surface-secondary`, `border-secondary`, transparent). They are platform-only: `crates/ui/chart_integration` projects a non-default
preset onto Aeris Charts through its existing appearance and options APIs, and Aeris Charts never
receives a preset identity and keeps its own default theme.
The color choices (`--swatch-*` picker swatches, `--swatch-default` and `--session-plan-level`) are
Aeris-owned, not Theme System palette entries. A user's choice is stored as its value, so they are
declared once in their own `:root` block and the build rejects them in `.dark` and every preset.
`colors_come_from_tokens` in `tools/naming_check` rejects quoted CSS hex values and all-numeric
`rgb`/`rgba`/`hsl`/`hsla`/`from_rgb8`/`from_rgba8` constructors in production `apps/desktop`,
`chart_integration` and `terminal_ui` source; files compiled only through a `#[cfg(test)]` module
declaration are outside the scan. Its only counted exceptions are the color picker's spectrum stops
and the legacy series fallbacks that layout restore recognizes.

`CSS.md` holds the exact theme CSS from the Theme System and the rules for using it. Read it before
any UI work and always follow it: every color, radius and border width comes from a token, never a
hardcoded value or an ad-hoc alpha tint. Keep `CSS.md`, `platform.css` and the Theme System identical;
the Aeris-owned color choices live only in `CSS.md`, `platform.css` and the website mirror.

## Verification

Use focused checks first:

```text
cargo check -p <crate> --locked
cargo test -p <crate> --locked
cargo clippy -p <crate> --all-targets --all-features --locked -- -D warnings
```

Before a completed native delivery, run the relevant broad gates; for release-quality changes use:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Do not suppress a new failure just to make a gate green. Fix failures caused by the change and report
independent baseline failures precisely.

CI runs the same gates on GitHub-hosted runners. `.github/workflows/ci.yml` triggers on `push` and
`pull_request` to `main` and on `workflow_dispatch`: `windows-latest` runs fmt, clippy, the workspace
tests and `aeris_naming_check`, and `ubuntu-latest` runs fmt and naming_check.
Windows workspace tests use the default parallel `cargo test --workspace --all-features --locked`.
Desktop bridge wake listeners must unregister their GPUI task waker when a notification
completes or the listener is dropped, so a late publication cannot wake a retired test
scheduler. GPUI interaction fixtures use an injected empty trading store, not a live
subscription thread that can wake GPUI's deterministic test scheduler from outside it;
the real subscription bridge has separate owner-level tests. The workflow keeps
`permissions: contents: read`, concurrency cancellation and `AERIS_RITHMIC_KIT_DISABLED=1` (hosted
runners have no Rithmic kit; kit-gated code is skipped there). `pull_request_target`, `secrets.`
references and self-hosted runners are forbidden on the pull-request path, and any workflow that uses
a self-hosted runner must stay `workflow_dispatch`-only in every trigger form; live, credentialed and
performance lanes stay `workflow_dispatch`-only. The manual `macos_preview.yml` and
`windows_preview.yml` tester builds are the only hosted lanes that may receive the Rithmic kit: only
when a lane's `rithmic_kit` input is set does it unpack the kit's `proto` folder from the
`AERIS_RITHMIC_KIT_TAR_XZ_B64` repository secret (its only secret), and it deletes the kit right
after the build and fails if the kit was not compiled in. The Windows lane packages the release binary
with Inno Setup from `apps/desktop/packaging/windows/aeris_terminal.iss` (a per-user installer that
installs the desktop directly, shares the lifecycle installer's AppId and folder, and never deletes
user data on uninstall) and smoke-tests a silent install, launch and uninstall (a launch whose
window could not open fails it); the macOS lane
smoke-launches the app bundle and ships a disk image with an Applications link. Both keep their
output only as a 14-day workflow artifact.
`tools/naming_check` enforces this shape across every workflow.

## Build output hygiene

Cargo never removes superseded artifacts. Every distinct command, profile and feature set adds a full
new copy under `target/`, and GPUI debug builds have grown it past 70 GB in a few hours. Keep it bounded:

- Before building a new desktop binary (`cargo build` or `cargo run`, debug or release), run
  `cargo clean` so the old `target/` is removed completely and only the new build remains.
- Before each verification batch, check the size of `target/`; above 25 GB, run `cargo clean` first.
- Never run `cargo clean` while another Cargo process is building in this workspace.
- Delete each validation root under `%LOCALAPPDATA%\Temp\aeris_validation\` once its run is finished
  and its evidence is recorded; do not leave per-attempt roots behind.
- The maintainer's machine also runs an hourly `Aeris Target Cleanup` scheduled task that enforces the
  25 GB cap and removes validation roots untouched for two days. It is a backstop, not a substitute.

## Release workflow

Do not publish, install, or point clients at the former Axiusflow endpoint, and the retired
publication workflow must not be restored. The launcher and desktop update-restart handshake uses
the `AERIS_UPDATE_RESTART_READY_V1` and `AERIS_UPDATE_RESTART_COMMIT_V1` lines from
`aeris_release_lifecycle`. Block plans sign `AERIS_RELEASE_BLOCK_PLAN_V2\0` followed by the
SHA-256 of the plan's JSON. Release manifests ship their exact bytes and sign
`AERIS_RELEASE_MANIFEST_V2\0` followed by the SHA-256 of those bytes, so every signed message
fits a KMS raw-message signature; they are verified before any field is read; a manifest's `minimum_launcher_protocol` is checked against
`LAUNCHER_PROTOCOL_VERSION`, never the product version. A failed block-plan delivery returns a
typed `BlockDeliveryError` and is logged before the launcher falls back to full downloads;
`tools/naming_check` rejects any `AXIUSFLOW` token in Rust sources under `apps/` and `crates/`. Releases reach users through the website download page:
after the fix is pushed to `main`, run both preview lanes with `-f rithmic_kit=true`, check that
each smoke test passed, download `Aeris-Terminal-<version>.dmg` from the
`aeris-terminal-rithmic-macos-arm64-preview` artifact and `Aeris-Terminal-Setup-<version>.exe` from
the `aeris-terminal-rithmic-windows-x64-preview` artifact, and publish them from the sibling website
with `bun run publish:desktop -- <dmg> <setup exe>` (it uploads them under `/downloads/` and
rewrites `/downloads/latest.json`, which `https://aeristerminal.com/download/` reads). Public file
names carry only the product and the workspace version (`[workspace.package] version`, semantic
versioning from 1.0.0); never put a provider name or "preview" in them. CI never holds AWS
credentials; publication runs on the maintainer's machine. Builds are ad-hoc signed on macOS and
unsigned on Windows, so Gatekeeper and SmartScreen warn on first launch. Windows builds link the C
runtime statically through `.cargo/config.toml`, so the shipped executables start on a clean Windows
without the Visual C++ Redistributable; no installer carries `VCRUNTIME140.dll`.

When the maintainer says "release the new version", run this whole path without asking again:
confirm `main` is clean and pushed; bump `[workspace.package] version` (minor when the release adds a
user-visible capability, patch when it only fixes) and run `cargo update --workspace --offline` so the
lockfile changes only workspace versions (published installer names are versioned and served with
immutable caching, so a version is never republished); commit `feat(release): release <version>` with
a release-record row naming the version and what it ships since the previous release; push;
wait for CI on that commit; run both preview lanes; check their smoke tests and the Windows checksum;
publish with `publish:desktop`; and confirm that `https://aeristerminal.com/downloads/latest.json` names
the version and the public files hash to the built artifacts. The release-record table in
`plan/stabilization/stabilization_work.md` is the release record.

The desktop starts its update client (`desktop/update.rs`) only in a release build, one that
carries `release_config()`; development builds and the current preview-lane installers have
none and run the desktop directly. Such a build runs the download offer
(`desktop/download_offer.rs`) instead: one worker reads `https://aeristerminal.com/downloads/latest.json`
through `aeris_release_lifecycle::newer_published_download` (HTTPS only, at most 16 KiB, at start
and every 6 hours, 30 minutes after a failure) and, when this platform's published version is
newer, shows one notice per version, a profile-menu row and an avatar dot that open the download
page; it never downloads or installs anything. A launcher update that is ready to restart shows
the same notice (once per version), row and dot with "Restart to update". Neither runs on a
validation root. Do not claim installed-app or update behavior was verified unless exercised on
that path.

Windows release builds with updates come only from `tools/release/publish_update.ps1`, run on the
maintainer's machine (never in CI). It refuses a dirty worktree, a missing Rithmic kit or release
variables already set in the shell (production also needs `HEAD` at the pushed `origin/main`),
takes the next install generation from the channel's current pointer, builds the packager without
a release configuration, then the desktop and launcher with one (verifying key read from the KMS
key), checks the launcher's `--launcher-identity`, packages and signs through `--kms-key` against
the current release (block plan) and builds `Aeris-Setup.exe` with ISCC. Only `-Publish` uploads:
the immutable release folder first, the channel pointer last, then a CloudFront invalidation and
an HTTPS read-back. `-Channel staging` (the default) publishes below
`https://releases.aeristerminal.com/staging`; production is the base URL itself.

If something fails, the user sends back `~/Library/Application Support/Aeris/logs/aeris.log` on macOS or `%LOCALAPPDATA%\Aeris\logs\aeris.log` on Windows.

## Rust and documentation conventions

- Rust edition/toolchain is repository-pinned; dependencies stay pinned to intentional versions.
- Avoid `unwrap`/`expect` outside tests and unavoidable startup invariants.
- Propagate errors intentionally; do not silently discard failures.
- Desktop library and binary deny `clippy::let_underscore_must_use` and
  `clippy::let_underscore_future`. Desktop production code must not use `let _ =`;
  propagate failures, log them through observability, or explicitly handle a typed
  retired/absent outcome. Test-only discards do not count toward the production scan.
- `tools/naming_check/ratchets.toml` holds each package's count of production `Result<_, String>`
  types and `let _ =` discards (`production_string_errors_and_discards_stay_within_their_ratchets`).
  A count above its ratchet fails; a change that lowers a count lowers the ratchet in the same
  commit; an unlisted package is held at zero, so new crates start with typed errors.
- Keep file/directory names snake_case.
- Every file under `tools/` outside its Cargo packages is named by a workflow, `README.md` or this
  file; `every_tool_script_is_referenced_by_ci_or_the_docs` in `tools/naming_check` enforces it.
  Delete a script that loses its last reference instead of keeping it.
- Desktop modules live where their `mod` declaration resolves: the library's modules directly
  under `apps/desktop/src/`, the binary's `desktop` children under `apps/desktop/src/desktop/`.
  `desktop_modules_follow_the_directory_tree` rejects a `#[path]` attribute anywhere in
  `apps/desktop/src`.
- Fix Clippy findings rather than adding broad lint suppression.
- Do not create planning documents, architecture diaries, nested agent files, or duplicate README files.
  Durable architecture rules belong in code/tests/`tools/naming_check`; implementation notes belong near
  the code they describe. `repository_markdown_inventory_is_exact` walks the whole tree (vendored and
  asset folders included) and allows only the reasoned `MARKDOWN_FILES` in `tools/naming_check`
  (`README.md`, this file, `CSS.md`, `themes.md`, the brand-asset `USAGE.md`, the Study SDK guide and
  the pinned `gpui_base` notes) plus the two planning directories below; a stale entry fails too.
- `plan/longterm_plan/` (the long-term product plans) and `plan/stabilization/` are the two sanctioned
  exceptions to the rule above. `plan/stabilization/` holds the stabilization tracking document
  `stabilization_work.md` and the audit/design report copies under `plan/stabilization/reports/`
  (redacted; the repo is public). Every stabilization commit updates its
  item's line in `plan/stabilization/stabilization_work.md` in the same commit. A box is checked only
  when the item's code has landed and every validation its spec requires (tests, probes, guarded GUI
  checks) has passed; until then the line says exactly what is left. Git history is the progress log.
  Stabilization work runs focused per-crate checks while iterating and the full workspace gate once
  per milestone, before the milestone closeout; CI runs the full gate on every push.
- Every commit that introduces or changes an architecture restriction (a `tools/naming_check` rule, a
  crate boundary, a writer or CI rule) must also update this `AGENTS.md` in the same commit, so the
  rule lives where future agents read it.

## Business outreach CRM

Outreach to prop firms, brokers and providers is tracked in a self-hosted Twenty CRM on the
maintainer's machine. Twenty is the source of truth for outreach status, not this repository.

- Twenty runs in Docker from `C:\Users\devraj\twenty-crm` (compose project `twenty`, image tag pinned
  in `.env`) at `http://localhost:4747`, bound to loopback only (`127.0.0.1` and `[::1]`; Windows
  resolves `localhost` to `::1` first, so dropping the IPv6 binding adds ~2s to every connection).
  Containers restart automatically. The REST API allows 100 requests per 60s per key; batch and retry.
- Use the REST API at `http://localhost:4747/rest/` with the bearer key stored in
  `C:\Users\devraj\twenty-crm\api_key.txt`. Never print, log or commit that key, the `.env` file, or
  any contact data. Contact names, emails and deal notes never enter this public repository.
- Data model: a Company per firm, a Person per contact linked to the company, and one Opportunity per
  outreach relationship, named after the firm (chart-engine deals end in ` - Chart engine`).
  Company `companyType` says what the firm is (prop firm, futures FCM or IB, stock, FX/CFD or
  multi-asset broker, crypto exchange, data feed / infrastructure, trading platform, broker tech
  vendor, exchange, professional trading firm, fintech/media). Opportunity custom fields:
  `aerisRole` (supported in Aeris, pitch the Aeris platform, broker connection, data/infra supplier,
  competing platform, chart engine licensing, not a target), `fit` (strong, good, check connection
  first, good fit via NinjaTrader request, later, not a fit) with a one-line `fitReason`, plus `outreachStatus`, `priority`, `platforms`, `sentAs`,
  `firstContactAt`, `lastActivityAt`, `whatHappened` and `nextAction`; the standard `amount` holds
  any quoted annual price.
- After sending outreach or reading a reply, update the matching Opportunity in the same task: set
  `outreachStatus` and `lastActivityAt`, append a dated line to `whatHappened`, set `nextAction`, and
  revise `fit` and `fitReason` when the reply changes them. Create the Company (with `companyType`),
  Person and Opportunity (with `aerisRole`, `fit` and `fitReason`) first for a firm that is not in
  the CRM yet.
- Supported in Aeris is only for connections that are approved and working (Rithmic). A firm that
  has closed gets `outreachStatus` out of business and `fit` not a fit, with the closure date and
  source in `fitReason`. A firm we dropped ourselves gets `outreachStatus` rejected by us,
  `aerisRole` not a target, `fit` not a fit, the decision date in `fitReason` and no `nextAction`. Saved Opportunity views split the list into Supported
  in Aeris, Active pipeline, Later (needs new work) and Dead ends; keep a deal's role and fit accurate
  so it lands in the right view.
- Tradovate/NinjaTrader-only firms are not out of reach: Tradovate gives no API access for prop or
  evaluation accounts, so approval comes from NinjaTrader, and prop-firm requests are the lever. Pitch
  them and ask them to request Aeris support from NinjaTrader/Tradovate.
- Email only addresses confirmed from the firm's own site or an existing thread, and never claim live
  order routing or performance that has not been verified.

## Delivery

Report exactly what changed, what was verified, what was deployed/published/installed if applicable,
and what still requires maintainer-only credentials or visual/manual confirmation. Do not present an
untested assumption as a completed result.
