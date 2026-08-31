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
| `crates/adapters/*` | Coinbase and Rithmic wire boundaries |
| `crates/local_storage`, `crates/local_history`, `crates/provider_history` | Engine-side history |
| `crates/engine_protocol`, `crates/local_engine_client`, `crates/transport` | Versioned local IPC |
| `crates/platform_runtime`, `crates/observability` | OS capabilities and diagnostics |
| `tools/naming_check` | Enforced repository and dependency boundaries |

## Workflow and verification

Work directly on `main` unless the maintainer requests a branch or PR. Inspect `git status` before
and after edits, preserve unrelated work, and stage only files owned by the task.

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
- Push `main` to `origin` without force.
- Report the outcome, verification performed, running binary state when relevant, and any remaining
  maintainer validation.

## Working with the maintainer

The maintainer owns product decisions. Make reasonable implementation assumptions when they do not
change behavior or scope materially. Ask only when alternatives materially change behavior, risk,
or authority.

If a requested mechanism would damage correctness, security, performance, or maintainability,
explain the concrete failure briefly and implement the safer approach when it preserves the desired
outcome. If the maintainer reaffirms the requirement, build that decision without reopening it.
