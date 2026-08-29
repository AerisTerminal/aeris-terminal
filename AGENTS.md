# AGENTS.md

Instructions for any coding agent working in this repository. Read this before you write code here.

Axiusflow is a local-first native trading terminal: a Rust/GPUI desktop plus a resident local engine
that owns provider sessions, canonical series, and local history. Charting comes from
[Nucleus Charts](https://github.com/NucleusCharts/financial-charts) as a pinned Git dependency.

---

## Rule 0 — stop over-engineering this codebase

Measured on 2026-08-29: **104,592 lines of Rust across 22 workspace crates**, 709 `#[test]`
functions, and 24 shell/PowerShell scripts under `tools/` — for a chart terminal that still ships
visible layout, data, and lifecycle bugs. The single largest file, `apps/desktop/src/main.rs`, is
**12,122 lines**. `apps/engine/src/market_service.rs` is **11,931**. History alone is split across
three crates (`local_storage`, `local_history`, `provider_history`).

None of that volume is an asset. It is the reason features take days and bugs hide. Every line you
add is paid for again on every read, every build, and every future fix.

**So: the smallest change that fully fixes the reported problem is the correct change.** Not the
most general one, not the most extensible one, not the one that anticipates next quarter.

### Do not do these unless the maintainer explicitly asks

- Add a new crate. Twenty-two is already too many.
- Add a trait, generic, or `dyn` boundary for a single implementation.
- Add a wrapper, manager, registry, factory, builder, coordinator, or "service" layer around code
  that already works.
- Add a Cargo feature, config knob, or environment variable "for flexibility".
- Add a parallel code path, compatibility shim, or second implementation of something that exists.
  Change the one owner instead.
- Add a script to `tools/`, a benchmark harness, or a conformance runner.
- Add a Markdown file. See [Documentation](#documentation).
- Build for a requirement nobody stated.

### Do these instead

- Read the real call path first — entry point, owner, threads, generations — then edit the owner.
- Delete the code your change replaces, in the same commit. A refactor that leaves the old path
  alive is not a refactor.
- Prefer fewer lines. If your diff is net-positive by a lot, be able to say in one sentence why.
- Reuse what is here, then `std`, then the platform, then an already-vendored dependency. A new
  third-party crate is a last resort with a stated cost.
- When you touch a 5,000-line file, leave it smaller than you found it if you reasonably can.

**"Simple" means the least total complexity that fully meets the requirement — not the least typing
and not a weaker feature.** Do not substitute a fragile shortcut, a fake implementation, or a
reduced product for what was asked. Hard-but-correct beats easy-but-broken. Both beat clever.

---

## Never simplify these away

Simplification stops at correctness. These are load-bearing and are not "extra complexity":

- Trading and market-data correctness: fixed-point values, snapshot/delta ordering, provenance,
  sequence numbers, generation guards, history/live handoff.
- Stale-generation rejection. A retired client, series, selection, or publication must never mutate
  current state.
- Bounded work: queues, caches, retries, history requests, memory, background tasks. Overload and
  cancellation behavior must stay defined.
- Error handling and recovery. Loading states must terminally resolve to data, a retry state, or an
  actionable error.
- Security: never log credentials, tokens, or raw provider payloads. Keep native credential storage
  and zeroizing memory where already established.
- `unsafe_code` is `forbid`den workspace-wide. Keep it that way.
- The UI thread does no network, disk, process, or shutdown work; background workers do not mutate
  GPUI state directly.

## Architecture boundaries

These are not folklore. Roughly two dozen tests in `tools/naming_check` assert them against the
real manifests and sources, so they fail loudly when they stop being true. Read the test before you
argue with the rule.

- `MarketEngine` is the single market-demand owner — one `DemandRegistry`, one
  `begin_provider_session`, and `crates/market_engine` is reachable only from `apps/engine`.
  Never create a provider session or a runtime per chart.
  (`market_engine_has_one_lock_free_mutable_owner`, `market_engine_remains_one_cohesive_crate`)
- The desktop depends on no adapter, storage, or `market_engine` crate. Provider-specific types
  stop at adapter boundaries; the UI consumes application/domain models and provider-neutral
  protocol messages. (`desktop_presentation_layers_exclude_provider_and_storage_ownership`,
  `cargo_dependency_direction_excludes_ui_from_backend_layers`,
  `provider_wire_and_nucleus_boundaries_remain_isolated`)
- `local_history` is the engine's storage boundary. (`local_history_is_the_engine_consumed_storage_boundary`)
- Two applications only: desktop and engine. One IPC path, no shared-memory IPC, no distributed
  systems dependencies. (`workspace_has_only_desktop_and_engine_applications`,
  `production_excludes_shared_memory_ipc`, `workspace_excludes_distributed_system_dependencies`)
- Production queues are bounded and production traits are justified boundaries.
  (`production_market_queues_exclude_unbounded_channels`, `production_traits_remain_justified_boundaries`)

Two rules that no test covers, so they are on you: symbol, timeframe, viewport, tab, and layout
changes are presentation changes and must not tear down a provider session; and stale generations
never mutate current state.

Nucleus Charts is a **separate repository**. Never clone or vendor it into this workspace; Cargo
fetches the pinned revision. The GPUI host lives in `crates/ui/chart_integration` — Nucleus's own
example hosts are not linked, so host-side behavior (pointer cadence, legend layout, chrome) is
ours to fix, not a pin bump away. When bumping Nucleus, change only the `nucleuscharts_*`
revisions; do not `cargo update` GPUI along with it.

## Repository map

| Path | Owns |
| --- | --- |
| `apps/desktop` | GPUI terminal: windows, workspace tabs, chart chrome, DOM, engine client wiring |
| `apps/engine` | Resident local engine process: provider sessions, publication, persistence |
| `crates/ui/chart_integration` | Nucleus host: chart view, legends, drawings, indicator panes |
| `crates/ui/design_system`, `crates/ui/terminal_ui` | Theme tokens and GPUI primitives |
| `crates/application` | Transport- and provider-independent use-case contracts |
| `crates/market_engine` | Headless single-owner market state |
| `crates/domain/*` | Provider-neutral instruments and fixed-point market-data values |
| `crates/adapters/*` | Coinbase and Rithmic wire protocols — provider types stop here |
| `crates/local_storage`, `crates/local_history`, `crates/provider_history` | History persistence, engine-side mechanics, provider scheduling |
| `crates/engine_protocol`, `crates/local_engine_client`, `crates/transport` | Versioned IPC and framing |
| `crates/platform_runtime`, `crates/observability` | OS capability boundary; latency vocabulary |
| `tools/naming_check` | The workspace's architecture assertions, run as tests in CI |
| `tools/*` (rest) | Benchmarks and a large pile of conformance scripts |

## Commands

```text
cargo run --release --package axiusflow_desktop     # run the app (--workspace-tabs, --multi-chart)
cargo check -p <crate>                              # while iterating
cargo test -p <crate>                               # focused tests
```

Before committing code, all four must pass with zero warnings — this is exactly what CI runs:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Compilation is not proof. If you changed streaming, persistence, IPC, window lifecycle, or chart
rendering, run the desktop and verify the real path. If you cannot, say precisely what is
unverified and how to check it.

## Conventions

- Rust edition 2024, toolchain pinned to 1.97.1 in `rust-toolchain.toml`.
- Dependencies are pinned with `=` exact versions. Keep it that way.
- `clippy::all` and `clippy::pedantic` are warn at the workspace level and `-D warnings` in CI.
  Fix the code; do not sprinkle `#[allow]`.
- No `unwrap`/`expect` outside tests and unavoidable startup invariants. Do not discard errors or
  weaken a test to get a gate green.
- File and directory names are snake_case, enforced by `tools/naming_check`.
- Measure before optimizing. Performance claims need release-build evidence.

## Documentation

Root Markdown files are limited to `Readme` and `AGENTS.md`. Do not create plans, reports, reviews,
design diaries, per-crate READMEs, or nested agent files. If a tool writes one during your work,
delete it before committing. Explain code in the code.

There is deliberately no `Architecture.md`. This repository used to carry one, plus a migration
specification and an earlier agents file, and the maintainer deleted all three because they had
drifted into describing a system that did not exist. Do not recreate them, and do not treat a
deleted document as a source of truth. **If you want to assert an architectural rule, write it as
an assertion in `tools/naming_check` where it can fail** — that is the one form of architecture
documentation here that cannot quietly go wrong. Prose about the design belongs next to the code it
describes.

## Git and delivery

- Work directly on `main`. No branches or PRs unless the maintainer asks for them.
- Inspect `git status` before and after editing. Stage only files your task owns; never sweep up
  someone else's in-flight work.
- No destructive Git commands, no force-push, no broad path deletes.
- One commit per completed batch, message in the existing `type(scope): outcome` style
  (`fix(ui): wrap chart legends inside their pane`). Push to `origin` without force.
- Report what changed, what you verified, and what the maintainer still needs to test.

## Working with the maintainer

The maintainer is a product owner, not a developer. Take the requested outcome seriously; do not
blindly implement the proposed mechanism.

If an approach would damage correctness, safety, performance, or maintainability: say so plainly in
a sentence or two, name the concrete failure, recommend the better path, and then build the better
path when it preserves the intent. Ask only when the alternatives change behavior, risk, or scope
materially. Object with evidence — source, measurement, platform behavior — never with taste. If
the maintainer reaffirms the request, that is the decision: build it.

## Known broken

CI is red on `main` today, on two of the four gates. Neither failure is new work; both predate this
file. Do not add to them, and do not silence them.

`cargo clippy --workspace --all-targets --all-features -- -D warnings`:

- `crates/ui/chart_integration/src/view.rs` — `too_many_lines` on `legend_rows` (116/100) and
  `used_underscore_binding` on `_mutation` in the `diagnostics`-gated rebuild logging.

`cargo test --workspace --all-features` — two tests in `tools/naming_check`:

- `provider_kit_remains_vendor_only` — asserts a `provider_kit/` vendor tree that is gitignored and
  not present in a clean checkout.
- `platform_rust_source_upper_bound_stays_within_soft_budget` — platform Rust source is 73,638
  lines against a 72,200-line budget. The budget is not the problem.

Most of `naming_check` earns its place — the boundary assertions listed above are the only
architecture enforcement this repo has. Its inventory-style assertions do not: they police vendor
trees and line budgets that reality has moved past. When you next touch it, delete those rather
than teaching them new exceptions, and shrink the codebase back under the budget instead of raising
the number. Do not paper over a failure to make a gate pass.
