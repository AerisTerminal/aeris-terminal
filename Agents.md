# Agents.md

You are an expert software engineering agent responsible for the outcome of work in this repository. Read `Architecture.md` before architectural or cross-cutting changes.

## Product context

Axiusflow is a local-first professional trading platform comparable in product category to MotiveWave and ATAS. It is a native Rust desktop terminal with a resident local engine, live provider connectivity, local history, professional charting and market-depth workflows, and a path to safe execution.

The target is best-in-class performance while remaining lightweight. Architecture is part of the competitive advantage: clear ownership, bounded work, low latency, fast local startup, low resource use, deterministic recovery, and native cross-platform behavior.

Nucleus Charts is a separate repository consumed through the Git dependencies pinned in `Cargo.toml`. Never clone or copy its repository inside the Axiusflow workspace: the additional codebase overwhelms repository searches and agent context, and the platform build does not use a local clone. Let Cargo fetch the pinned revision into its external cache. If the user explicitly requests Nucleus Charts development, work from a separate checkout outside this workspace and follow that repository's instructions.

A Nucleus pin bump does not copy example-host behavior. Axiusflow's GPUI host is `crates/ui/chart_integration`; Nucleus's `gpui_probe` is not linked. If a chart visual or interaction still looks wrong after pinning the latest Nucleus commit, check whether the Nucleus change lives in an example host or in the engine/renderer crates. Brush smoothness in particular is host-owned pointer cadence: keep only the newest sample per painted frame and flush it in rebuild (and on pointer-up). Do not call `brush_create_add` on every Wayland/HID motion event. When bumping Nucleus, update only the `nucleuscharts_*` revisions in `Cargo.toml` / `Cargo.lock` / `Architecture.md` / `tools/naming_check`; do not unscoped `cargo update` Git GPUI. Keep Zed at the `Cargo.lock` commit (`1c9cbd3b24d47e0cfab5f1673574f96d307c8b3e`) unless a GPUI bump is explicitly requested and verified on Linux clipboard.

## Working with the maintainer

The primary maintainer is a product owner, not a technical developer. Treat requested product outcomes seriously, but do not blindly implement the proposed technical mechanism.

When a request would materially harm correctness, trading safety, security, performance, portability, maintainability, or the architecture:

1. Say plainly that the proposed approach is not good for Axiusflow.
2. Explain the concrete failure mode in product terms.
3. Recommend the stronger approach and why it better serves the requested outcome.
4. Implement the stronger approach when it preserves the user's intent and stays within scope. Ask only when the alternatives change product behavior, cost, risk, or scope materially.

Do not object based on taste. Use source evidence, measurements, platform behavior, official documentation, or established engineering constraints. Never patronize the maintainer or hide a technical decision behind jargon.

Difficulty is not a reason to weaken the solution. If the robust design is harder but materially safer, faster, or more durable, implement it. Do not silently substitute a fragile shortcut, immature dependency, fake behavior, or reduced product for the requested result. Simplicity means the least complexity that fully meets the requirement, not the easiest code to type.

## Ponytail workflow

Use the matching Ponytail skill when it is available:

- `ponytail` for implementation, refactoring, bug fixes, and design: understand the real path first, then choose the smallest robust solution.
- `ponytail-review` for diff-level over-engineering reviews.
- `ponytail-audit` for whole-repository deletion and simplification audits.
- `ponytail-debt` to collect deliberate `ponytail:` deferrals into a debt ledger.
- `ponytail-gain` for the standard Ponytail impact scoreboard.
- `ponytail-help` when the user asks how the Ponytail workflows work.

Ponytail governs unnecessary complexity. It must never simplify away trading correctness, data integrity, recovery, security, accessibility, platform-native behavior, error handling, or explicit requirements. Hard-but-correct beats easy-but-fragile.

## Engineering rules

- Read before editing. Trace entry points, callers, ownership, threads, and execution paths. Search the complete workspace, not only the Git diff.
- Fix root causes at the narrowest shared boundary. Do not stack compensating workarounds around a broken owner.
- Prefer deletion and direct code. Reuse existing code, then the standard library, native platform facilities, and already-installed dependencies before adding anything.
- No speculative crates, traits, wrappers, factories, generic systems, feature flags, configuration, or compatibility layers.
- A single implementation does not need an abstraction unless it is a real provider/platform boundary or a necessary test seam.
- Keep the UI thread free of blocking network, disk, process, and shutdown work. Keep background workers from mutating GPUI state directly.
- Bound queues, caches, retries, history requests, memory, and background work. Define overload and cancellation behavior.
- Preserve generation, provenance, sequence, fixed-point value, snapshot/delta, and history/live handoff invariants.
- Never log credentials, tokens, raw secrets, or sensitive provider payloads. Use native credential storage and zeroizing memory where already established.
- Prefer official platform and dependency examples for behavior at external boundaries. Verify the pinned version's source when APIs or behavior may differ.
- Measure before optimizing. Performance claims require release-build evidence on a representative workload.
- Avoid `unsafe`. If a native boundary makes it unavoidable, isolate it behind a small safe API and state/test its invariants.
- Do not add a dependency without checking the workspace first and justifying its runtime, binary-size, maintenance, and supply-chain cost.
- Do not weaken tests, silence lints, discard errors, or use `unwrap`/`expect` outside tests and unavoidable process-startup invariants merely to pass a gate.

## Market architecture guardrails

1. GPUI performs no provider, persistent-history, blocking network, disk, process, or shutdown work.
2. The desktop does not import provider adapters, provider-history implementations, storage implementations, or `market_engine` directly.
3. Provider-specific types stop at adapter boundaries; desktop presentation consumes application/domain models and provider-neutral engine protocol messages.
4. `MarketEngine` is the single market-demand owner.
5. Never create provider sessions per chart. Shared demand and subscriptions are keyed in the resident engine.
6. Never create runtimes per chart; process and worker ownership stays bounded and explicit.
7. Symbol, timeframe, viewport, tab, or layout changes do not recreate a provider session merely because presentation changed.
8. Valid in-memory visualization data publishes before persistence and remains usable when persistence fails.
9. Stale client, consumer, selection, series, provider, entitlement, or publication generations never mutate current chart state.
10. Loading and recovery states are bounded and terminally resolve to usable data, an explicit retry/recovery state, or an actionable error.
11. There is one production runtime and IPC path. Do not add compatibility or duplicate backend paths.
12. A new crate requires a concrete ownership boundary and documented dependency-direction justification.
13. A new trait requires real polymorphism, a platform/provider boundary, or a necessary test seam.
14. Delete replaced code, dependencies, exports, tests, and tooling in the same change.
15. Compilation is not runtime proof; verify the real streaming, persistence, IPC, lifecycle, and native-window path affected by the change.

## Bug-fix workflow

1. Reproduce the failure or establish an observable failing invariant.
2. Trace every relevant caller and thread/process boundary.
3. Identify the owner and root cause.
4. Add the smallest regression test or deterministic check that would fail before the fix.
5. Implement the robust fix at the owning boundary.
6. Test the real runtime path, especially release-mode streaming and native window behavior when relevant.

A test harness that bypasses the failing path is not proof. If runtime verification is impossible, state exactly what remains unverified and give the precise command or interaction needed.

## Scope and repository safety

- Work directly on `main`. Do not create branches or pull requests unless the user explicitly changes this policy.
- Preserve unrelated worktree changes. Inspect `git status` before and after editing; stage only files owned by the task.
- Never use destructive Git commands, force-push, or delete broad paths. Resolve exact targets first.
- Never commit secrets, credentials, local data, provider entitlements, build outputs, or vendor material accidentally.
- Edit existing files when that is clearer. Create a new module only when it improves a real ownership boundary.
- Keep `Architecture.md` authoritative for implemented behavior. `Axiusflow Local Engine Architecture Migration Specification.md` is the approved target-state migration contract and may intentionally describe behavior not implemented yet. Do not create any other architecture diaries, duplicate plans, or speculative decision-document trees.

## Markdown and architecture consistency

Exactly four Markdown files may exist in the Axiusflow repository:

- `Readme.md`
- `Architecture.md`
- `Agents.md`
- `Axiusflow Local Engine Architecture Migration Specification.md`

Nucleus Charts has its own repository and documentation inventory. Do not clone it into this workspace or count or modify its files as part of Axiusflow work.

Do not create any other `.md` file, including temporary plans, reports, reviews, generated output, package READMEs, or nested agent files. If a tool creates one during work, remove it before committing.

The migration specification defines the approved future engine-owned market-data topology. During implementation, keep its target requirements intact and update `Architecture.md` as each migration slice becomes current behavior. If the approved target itself changes, update both documents in the same commit and keep the distinction between implemented and target behavior explicit.

Keep `Architecture.md` synchronized with the implementation. A change to process topology, crate responsibilities, dependency direction, data flow, mutable ownership, persistence, security, platform support, provider/UI boundaries, or verification gates must update it in the same commit. Before delivery, compare its claims with Cargo manifests, public exports, application entry points, and actual call paths.

## Verification and delivery

Use focused checks while iterating. Before committing a code change, all of these must pass with zero warnings:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Add any relevant provider conformance, persistence, IPC, release-mode performance, or manual native-platform verification. Documentation-only changes skip the Cargo gate; inspect the complete diff and validate paths and links.

When the requested batch is complete:

1. Review the diff for accidental changes, secrets, scope drift, architecture drift, and extra Markdown files.
2. Commit once with a clear, structured message describing the achieved outcome.
3. Push `main` to `origin` without force.
4. Report what changed, the verification performed, and anything the user must test.

Do not stop at a plan when implementation is authorized and safe. Do not claim completion while a required check is failing.
