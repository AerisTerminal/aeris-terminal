# AGENTS.md

Instructions for coding agents working in Axiusflow. Read this file before changing the repository.

Axiusflow is a local-first native trading terminal built with Rust and GPUI. It consists of a
desktop application and a resident local engine that owns provider sessions, canonical market
series, and history. Charting uses [Nucleus Charts](https://github.com/NucleusCharts/financial-charts)
as a pinned Git dependency.

## Engineering priorities

- Build the requested product outcome, not speculative infrastructure.
- Read the real call path and edit the component that owns the behavior.
- Prefer the smallest complete change that preserves correctness and recovery.
- Replace obsolete paths instead of adding compatibility layers or duplicate state.
- Reuse existing code, `std`, platform facilities, and pinned dependencies before adding anything.
- Add no crate, framework, service layer, feature flag, tool script, or third-party dependency unless
  the requirement genuinely needs it.
- Do not weaken behavior, validation, or tests to make a change easier.

## Verify contracts before implementation

- Before editing, trace the real call path, identify the owner of the behavior, and inspect existing
  reusable code. Briefly state the intended change and evidence supporting it in the conversation;
  do not create a planning document.
- For external integrations, verify request/response shapes and field semantics against current
  official documentation and representative public responses when available. Do not invent protocol
  behavior from naming conventions or another provider's implementation.
- Resolve correctness-critical unknowns before writing dependent code: identity, precision,
  timestamp meaning, ordering, continuity, snapshot semantics, and recovery. If evidence is
  unavailable, state the uncertainty and continue only independent work.
- Never treat an identifier or hash as an ordered sequence without an explicit provider guarantee.
  Keep provider identity, local ingestion order, and evidence of continuity distinct.
- Choose fixed-point scales from verified field requirements. Prices, quantities, rates, and
  notional values may require different scales. Do not reject valid provider values merely to
  simplify normalization.
- Preserve required wire fields and validate them explicitly. Do not default missing required
  fields to plausible values or support undocumented alternative payload shapes speculatively.
- Base protocol tests on minimal sanitized fixtures faithful to official examples or observed public
  responses. Preserve real nesting, field names, namespaces, indexes, and precision. Record the
  source beside the fixture; never store credentials, private account data, or raw production
  payload dumps.
- Test assumptions with counterexamples: nonconsecutive IDs, already-prefixed symbols, explicit
  indexes differing from array positions, high-precision decimals, missing fields, and actual stream
  envelopes. Expected results must follow the verified contract, not the implementation.
- Build and verify one coherent slice before expanding dependent integration. Run focused
  compilation and behavioral tests as soon as the slice is testable; do not accumulate an entire
  uncompiled adapter.
- Before handoff, remove unused state, redundant collections, speculative wrappers, and comments
  that claim behavior the code does not provide. Preserve meaningful regression coverage when
  removing old implementations.

## Correctness invariants

These constraints are load-bearing:

- Market data uses fixed-point values with explicit provenance, sequence, timestamp, and generation
  validation.
- History/live handoff maintains one canonical forming candle and contiguous completed history.
- Retired clients, selections, sessions, and publications never mutate current state.
- Queues, caches, retries, history requests, and background work remain bounded with explicit
  overflow and cancellation behavior.
- Loading always resolves to data, recovery/retry, or an actionable terminal error.
- Never log credentials, tokens, or raw provider payloads. Preserve native credential storage and
  zeroizing memory where established.
- Provider credentials live only in the native vault (DPAPI/Keychain/Secret Service) and are
  provisioned only through the interactive terminal prompter, never pasted into chat, email,
  environment, CI secrets, or the repo. CI and live gates use test/paper credentials exclusively;
  production credentials never enter a scheduled job, and the single live session belongs to the
  resident engine or one designated probe, never both at once.
- Workspace-wide `unsafe_code` remains forbidden.
- The UI thread performs no network, disk, process, or shutdown work. Background workers do not
  mutate GPUI state directly.

## Architecture boundaries

Architecture assertions live in `tools/naming_check` and should be read alongside the code they
protect.

- `MarketEngine` is the single market-demand owner. Provider sessions are created only by
  `apps/engine`; never create a session or runtime per chart.
- Symbol, timeframe, viewport, tab, and layout changes are presentation changes and must not tear
  down a provider session.
- The desktop depends on no provider adapter, storage implementation, or `market_engine` crate.
  Provider wire types stop at adapter boundaries; UI and IPC models remain provider-neutral.
- `local_history` is the engine-facing storage boundary.
- The workspace has two applications: desktop and engine. They use one bounded IPC path, with no
  shared-memory or distributed-systems layer.
- Production queues are bounded, and production traits must represent real multi-implementation or
  platform boundaries.

Nucleus Charts is a separate repository fetched by Cargo. Do not clone or vendor it here. The host
integration lives in `crates/ui/chart_integration`; host chrome, pointer behavior, legends, and
layout belong there. When updating Nucleus, change only the `nucleuscharts_*` revisions and do not
update GPUI incidentally.

## Authentication control plane (cross-repository)

Authentication is one end-to-end system split across this repository and the sibling website
repository at `C:\Users\devraj\Downloads\axiusflow-website`. Any authentication, signup, account,
profile, subscription, entitlement, checkout, portal, email OTP, OAuth/OIDC, or browser-callback
change must inspect and verify both repositories. Do not conclude that authentication is fixed from
only the desktop or only the browser page.

- This repository owns the native side: `apps/desktop/src/account.rs` presents account state and
  initiates IPC; `apps/engine/src/account_service` owns PKCE, the loopback callback, token exchange
  and verification, refresh material, native vault storage, account linking, and entitlement leases;
  `crates/engine_protocol/src/account.rs` is the bounded sanitized IPC contract.
- The website repository owns the Cloudflare control plane under `workers/auth`: Better Auth and its
  OAuth/OIDC provider, browser sign-in and account pages, D1 identity/account/billing state, Google
  and email-OTP entry points, the Axiusflow link and lease routes, checkout/portal routes, and Dodo
  webhook reconciliation. The marketing site only links into that Worker.
- The deployed issuer is `https://auth.axiusflow.com/api/auth`; the registered native public client
  is `axiusflow-desktop`, using Authorization Code with S256 PKCE and an ephemeral literal
  `127.0.0.1` callback. Keep provider tokens and cookies out of desktop UI and logs.
- Transactional OTP mail uses Cloudflare Email Service through the Worker's `EMAIL` `send_email`
  binding. Do not request or reintroduce a Resend key or another mail provider unless the maintainer
  explicitly changes that decision. Verify the Cloudflare sending domain and real delivery before
  claiming email signup works.
- Browser success is not native success. End-to-end proof requires the callback, code exchange,
  issuer/audience/signature/nonce checks, canonical account link, vault commit, sanitized IPC update,
  and the correct profile in the running desktop. Also verify full engine restart restoration,
  account switching, cancellation, timeout recovery, and sign-out during refresh.
- Billing and entitlement changes must keep the website account view, desktop plan, subscription
  status, and signed lease consistent. Test duplicate, delayed, interrupted, and out-of-order
  webhooks before production.

The licensed Rithmic Provider Kits are kept permanently outside Git. The maintainer's
canonical copy currently lives at `C:\axiusflow-deps\provider-kit` (the Rithmic live gate
restores `proto/` from there, overridable per runner via `AXIUSFLOW_RITHMIC_KIT_ROOT`). If
`provider_kit/current/proto` is missing, restore it from the canonical copy before building
or concluding that Rithmic is unavailable, and never treat a kitless build as live-path
evidence: without the kit the adapter cannot speak R|Protocol. Keep the permanent copy
unchanged.

## Repository map

| Path | Responsibility |
| --- | --- |
| `apps/desktop` | GPUI windows, workspaces, chart chrome, DOM, and engine-client wiring |
| `apps/engine` | Resident provider sessions, canonical publication, and persistence |
| `crates/ui/chart_integration` | Nucleus host, legends, drawings, and indicator panes |
| `crates/ui/design_system`, `crates/ui/terminal_ui` | Theme tokens and native UI primitives |
| `crates/application` | Transport- and provider-neutral use-case contracts |
| `crates/market_engine` | Headless canonical market state |
| `crates/domain/*` | Instruments and fixed-point market-data models |
| `crates/adapters/*` | Rithmic wire boundaries |
| `crates/local_storage`, `crates/local_history`, `crates/provider_history` | Engine-side history |
| `crates/engine_protocol`, `crates/local_engine_client`, `crates/transport` | Versioned local IPC |
| `crates/platform_runtime`, `crates/observability` | OS capabilities and diagnostics |
| `tools/naming_check` | Enforced repository and dependency boundaries |

## Workflow and verification

Work on `development` by default. Do not work on or push directly to `main`. Open a PR to `main`
only when the maintainer requests release review; merging requires maintainer acceptance.
Inspect `git status` before and after edits, preserve unrelated work, and stage only files owned
by the task.

Use focused checks while iterating:

```text
cargo check -p <crate>
cargo test -p <crate>
cargo clippy -p <crate> --all-targets --all-features -- -D warnings
```

Before delivery, run the workspace gates:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Do not hide or suppress a failure. Fix failures caused by the change; report any independently
reproducible baseline failure precisely.

Compilation is not proof for streaming, persistence, IPC, lifecycle, or rendering changes. Build
and run the release desktop, exercise the real path, and verify that the running desktop and engine
are the intended binaries. If live verification is impossible, state exactly what remains untested.

## Self-hosted CI (manual, zero-cost; GitHub-hosted runners are forbidden)

The account carries no paid Actions quota, so every lane runs on maintainer hardware. CI is a final
qualification step after local implementation and verification, not a feedback loop for partial
work. Both workflows are manual-only (`workflow_dispatch`) and currently disabled in GitHub; enable
only the workflow needed for one deliberate run, then disable it again after completion. The
`*-latest` ban, the two supported-target lanes, and the job-scoped credential rule are pinned in
`tools/naming_check`; the full saga lives in `plan/plan.md`. What a new session must know:

- Windows lane: runner `axiusflow-windows` in `C:\actions-runner`, labels `axiusflow,windows`,
  run interactively via `run.cmd` (never as a service; vault parity). Requires PowerShell 7
  (`shell: pwsh` steps fail without it). Relaunch after every reboot or logoff.
- Linux lane: Hyper-V VM `axiusflow-linux` (Ubuntu 24.04, static 16 GB RAM, 8 GB swap,
  `jobs = 4` in the runner user's `~/.cargo/config.toml`), labels `axiusflow,linux`, running as
  a systemd service that auto-starts with the VM. Key-only SSH with `~/.ssh/axiusflow_linux`;
  the IP is DHCP-assigned, so resolve it per session (ARP scan for the `00-15-5d` NIC).
  Hyper-V automatic checkpoints stay off. The VM consumes 16 GB of the maintainer's workstation:
  keep it shut down during development, start it only for an intentional Linux qualification run,
  and shut it down gracefully immediately afterward.
- macOS native qualification is explicitly deferred because no Apple hardware exists. Keep
  macOS-specific code guarded, warning-clean where cross-target tooling permits, and free of known
  source defects, but do not add an unserviceable required CI lane or claim native support until an
  Apple runner completes the same release gates.
- Runner registration tokens expire after one hour and registration is case-sensitive on the
  repo path; the maintainer issues them from Settings, Actions, Runners.
- Validate workflow YAML with a real parser before push: a run with zero jobs is a parse
  failure. Never `git config --global` in a workflow; scope credentials per job.
- Never change CI or live gates back to automatic `push`, `pull_request`, or `schedule` triggers
  without the maintainer's explicit request. Do not start runners, enable workflows, or dispatch a
  run while implementation is still changing.
- Finish focused checks, the local workspace gates, release-binary verification, and the real
  behavior path first. Then batch the completed commits, push once, start only the required runners,
  enable and dispatch one CI run, wait for it to finish, and shut the runners down. Run live-market
  gates separately only when their deterministic prerequisites pass and live evidence is required.
- Do not push while lanes run: same-ref concurrency cancels them. Avoid repeated pushes; a complete
  two-runner qualification costs roughly 40 minutes of full-machine load.
- Rithmic Test allows one concurrent session: drive the engine path or the smoke binary,
  never both at once. The test feed publishes no prints, so tick bars cannot form there;
  do not chase that absence as an adapter defect.
- Secrets travel by environment only, never enter the repo, logs, or binaries; delete
  throwaway provisioning helpers immediately after use.

## Rust conventions

- Rust edition 2024; toolchain is pinned in `rust-toolchain.toml`.
- Keep dependencies pinned to exact versions.
- Fix clippy findings instead of adding broad `allow` attributes.
- Avoid `unwrap` and `expect` outside tests and unavoidable startup invariants.
- Propagate errors intentionally; do not discard failures.
- Keep file and directory names snake_case.
- Measure release builds before making performance claims.

## Documentation

Root Markdown files are limited to the existing Readme and `AGENTS.md`. Do not create architecture
reports, plans, design diaries, per-crate READMEs, or nested agent files. Explain implementation
details beside the code. Enforce durable architecture rules in `tools/naming_check`, not in a
document that can drift.

## Git and delivery

- Never use destructive Git commands, force-push, or broad path deletion.
- Commit one completed batch using the existing `type(scope): outcome` style.
- Push a locally verified completed batch to `development` once, without force; do not use pushes as CI
  probes or push after every intermediate commit.
- Report the outcome, verification performed, running binary state when relevant, and any remaining
  maintainer validation.

## Working with the maintainer

The maintainer owns product decisions. Make reasonable implementation assumptions when they do not
change behavior or scope materially. Ask only when alternatives materially change behavior, risk,
or authority.

If a requested mechanism would damage correctness, security, performance, or maintainability,
explain the concrete failure briefly and implement the safer approach when it preserves the desired
outcome. If the maintainer reaffirms the requirement, build that decision without reopening it.
