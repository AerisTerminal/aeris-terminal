# AGENTS.md

Instructions for coding agents working in the Aeris native repository.

Aeris is a local-first Rust/GPUI trading terminal. The desktop is the single application process.
In-process `market_runtime` and `account_runtime` own provider sessions, canonical market/account state,
and bounded background work; GPUI owns presentation scheduling only. A secondary local market process must not return.

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
- `trading_runtime` is the single in-process owner of broker accounts, orders, order events, fills,
  positions, PnL, simulated execution and the embedded user-record store. The desktop holds command/view
  handles only; `account_runtime`, charts and UI surfaces never own or duplicate broker trading state.
- cTrader trading travels over the `market_runtime`-owned cTrader session for that host through the
  bounded venue boundary. `market_runtime` relays venue requests and events but never holds order,
  fill, position or account state; never open a second cTrader connection for trading.
- Symbol, timeframe, viewport, tab, and layout changes must not tear down a healthy provider session.
- `market_runtime` is the single owner that merges on-demand history and live state. Preserve one
  canonical forming candle, contiguous completed history, exact generation fencing, and explicit recovery.
- Retired clients, generations, selections, sessions, and publications must never mutate current state.
- Production queues, caches, retries, and background work remain bounded with explicit overflow and
  cancellation behavior.
- The desktop must stay provider-neutral. It does not own provider adapters, storage implementations,
  or canonical candle state.
- The desktop stores `ChartInterval` and must not construct `SeriesCadence`; interval-to-cadence mapping
  belongs to `market_runtime`.
- Market history is loaded on demand and bounded; do not restore market-history persistence or a
  second desktop-owned market-state model.
- Do not restore `apps/engine`, `local_engine_client`, a secondary market process, local process transport, or market autostart.
- UI-thread code performs no blocking network, disk, process, or shutdown work. Background workers do
  not mutate GPUI state directly.
- `unsafe` is isolated to target-specific FFI under `crates/platform_sys/src/windows/` and
  `crates/platform_sys/src/macos/`, whose blocks require documented, single-operation safety
  justifications. Until the M8.3 FFI consolidation, only the exact `platform_runtime` FFI files
  allowlisted by `tools/naming_check` are additional exceptions.
- `platform_sys` is a leaf crate. Only `platform_runtime`, `trading_runtime`, and `observability`
  may depend on it directly.
- Secret-bearing types whose names contain `Token`, `Secret`, `Password`, or `Credential` must not
  derive `Debug`, and those values must not use debug formatting. Only the exact reviewed,
  non-secret or transitively redacting wrappers in `tools/naming_check` are exceptions.
- Durable file replacement is owned by `platform_sys::fs`: staging uses a unique temporary in the
  destination directory, synchronizes file contents before replacement, and synchronizes the
  parent directory on Unix. Production code must use this writer instead of raw `fs::write` or
  `fs::rename` except at the exact move/build/platform implementation sites allowlisted by
  `tools/naming_check`. Do not add another atomic-file FFI implementation.
- Validation runs use `platform_runtime::data_root` isolation and an exclusive per-root instance
  lock. They must not overlap the production data root, probe production vault entries, migrate
  legacy state, write installed-app identity, promote launchers, or run updates. Only the explicit
  cTrader borrowing flag may route the exact cTrader broker-connection vault key to production.

Architecture assertions in `tools/naming_check` are authoritative when they are stricter than prose.
Its source scans strip only balanced `#[cfg(test)]` module bodies, so production code after a test
module is still scanned, and every allowlist entry must carry a written reason; stale entries fail
the check.

## Market-data and provider work

- Keep the four coordinator command/event characterization scenarios under
  `market_service/tests/characterization.rs`; `tools/naming_check` requires their exact names so
  lifecycle refactors cannot silently drop the baseline.
- Coordinator-owned watchdog, history retry and trade-handoff timestamps use the injected
  `market_service::clock::Clock`; direct system time belongs only in `SystemClock`.
- Provider reconnect scheduling uses the bounded `market_service::reconnect::Backoff` policy;
  the Rithmic adapter alone retries transport, while its non-transport session replacements
  wait at least one second. Never stack a coordinator transport retry on that adapter.
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
- Catalog callbacks pass through coordinator-owned `CommandFences`, keyed by consumer, provider
  and command kind. Startup resolution uses its own kind, not a numeric generation sentinel;
  consumer retirement clears every pending command through `ConsumerRegistry::remove_consumer`.
- `HistoryTracker` alone owns per-series current/backfill lanes, retry deadlines, empty coverage,
  detached windows and cancellation. History requests and completions carry the same exact
  provider/demand/sequence ticket; retirement prunes the tracker, never parallel coordinator maps.
- Stall recovery considers every live candle and trade series while its calendar is open;
  closed sessions never recover for silence, and unknown sessions use a longer threshold.
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
  storage is the credential boundary. Live/test provider sessions must respect provider concurrency limits.

The licensed Rithmic Provider Kit remains outside Git. The canonical local copy is
`C:\axiusflow-deps\provider-kit`; do not vendor or modify that permanent copy.

## Operating constraints

1. **Local-first, no server in the market-data path.** No Aeris server receives, stores, processes
   or relays market data, and no feature may depend on one. The only Aeris server is the AWS broker
   for hosted-broker OAuth and application credentials (tastytrade, cTrader); it never sees market
   data, orders or account state.
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

Chart behavior is owned by Aeris Charts, never re-implemented here:

- `crates/ui/chart_integration` is product glue only: the market-data bridge, trading, alert, study, and
  order-flow projections from the runtimes, design tokens, legend and menu UI, and workspace persistence.
- Pointer, wheel, and keyboard input reach the chart through `aeris_charts_render_gpui::input` and the
  engine's `input_*` controller, one adapter call per GPUI listener. Never add gesture routing, hover or
  cursor arbitration, key bindings, drawing-tool mirrors, host-side drawing state (locks, selection,
  creation), brush styling, or other engine-owned state to Terminal. `tools/naming_check` rejects the
  engine's low-level interaction primitives in this crate.
- When a chart feature needs more from Terminal than data, product policy, or a platform-only effect, the
  missing operation belongs in Aeris Charts. Add it there first (a coordinated two-repository task), push
  it, then bump the pinned revision here; never compensate with Terminal-side wiring.
- After bumping Aeris Charts, check for engine capabilities Terminal is not yet using (for example a new
  controller behavior or host request) instead of assuming the pin bump alone delivered them.

## Authentication and website coordination

Authentication is disabled for the current development build because no Aeris account backend is deployed. Desktop startup must reach the workspace without opening a browser or creating an account session. Do not restore sign-in, sign-up, profile, billing, or lease traffic until an Aeris backend is provisioned and the full browser to callback to account runtime to vault to desktop path is verified.

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

`CSS.md` holds the exact theme CSS from the Theme System and the rules for using it. Read it before
any UI work and always follow it: every color, radius and border width comes from a token, never a
hardcoded value or an ad-hoc alpha tint. Keep `CSS.md`, `platform.css` and the Theme System identical.

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
tests and `aeris_naming_check`, and `ubuntu-latest` runs fmt and naming_check. The workflow keeps
`permissions: contents: read`, concurrency cancellation and `AERIS_RITHMIC_KIT_DISABLED=1` (hosted
runners have no Rithmic kit; kit-gated code is skipped there). `pull_request_target`, `secrets.`
references and self-hosted runners are forbidden on the pull-request path, and any workflow that uses
a self-hosted runner must stay `workflow_dispatch`-only in every trigger form; live, credentialed and
performance lanes stay `workflow_dispatch`-only. `tools/naming_check` enforces this shape across
every workflow.

## Release workflow

Production release publication is disabled while the Aeris domain and AWS backend are unconfigured. Do not publish, install, or point clients at the former Axiusflow endpoint. The retired publication workflow must not be restored. Design and verify a new AWS release path when the maintainer requests deployment.

The local launcher and signed lifecycle code remain for future integration; development builds run the desktop directly and automatic update checks are disabled. Do not claim installed-app or update behavior was verified unless exercised on that path.

## Rust and documentation conventions

- Rust edition/toolchain is repository-pinned; dependencies stay pinned to intentional versions.
- Avoid `unwrap`/`expect` outside tests and unavoidable startup invariants.
- Propagate errors intentionally; do not silently discard failures.
- Keep file/directory names snake_case.
- Fix Clippy findings rather than adding broad lint suppression.
- Do not create planning documents, architecture diaries, nested agent files, or duplicate README files.
  Durable architecture rules belong in code/tests/`tools/naming_check`; implementation notes belong near
  the code they describe.
- `plan/stabilization/` is the one sanctioned exception to the rule above: it holds the stabilization
  tracking document `stabilization_work.md` and the audit/design report copies under
  `plan/stabilization/reports/` (redacted; the repo is public). Every feature commit must update
  `plan/stabilization/stabilization_work.md` in the same commit: check the work item, note the findings
  it closes, append a progress-log line, and record any new restriction there.
- Every commit that introduces or changes an architecture restriction (a `tools/naming_check` rule, a
  crate boundary, a writer or CI rule) must also update this `AGENTS.md` in the same commit, so the
  rule lives where future agents read it.

## Delivery

Report exactly what changed, what was verified, what was deployed/published/installed if applicable,
and what still requires maintainer-only credentials or visual/manual confirmation. Do not present an
untested assumption as a completed result.
