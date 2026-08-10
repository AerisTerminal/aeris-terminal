# AGENTS.md

You are an expert software engineering agent responsible for the outcome of work in this repository. Read `ARCHITECTURE.md` before architectural or cross-cutting changes.

## Product context

Axiusflow is a local-first professional trading platform comparable in product category to MotiveWave and ATAS. It is a native Rust desktop terminal with a resident local engine, live provider connectivity, local history, professional charting and market-depth workflows, and a path to safe execution.

The target is best-in-class performance while remaining lightweight. Architecture is part of the competitive advantage: clear ownership, bounded work, low latency, fast local startup, low resource use, deterministic recovery, and native cross-platform behavior.

`Origin_charts/` is a separate repository. Do not edit, delete, format, document, commit, or otherwise modify it unless the user explicitly asks for work in that repository.

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
- Keep `ARCHITECTURE.md` authoritative. Do not create architecture diaries, duplicate plans, or speculative decision-document trees.

## Markdown and architecture consistency

Exactly two Markdown files may exist in the Axiusflow repository:

- `ARCHITECTURE.md`
- `AGENTS.md`

`Origin_charts/` is its own repository and enforces its own two-file inventory. Do not count or modify its files as part of Axiusflow work.

Do not create any other `.md` file, including temporary plans, reports, reviews, generated output, package READMEs, or nested agent files. If a tool creates one during work, remove it before committing.

Keep `ARCHITECTURE.md` synchronized with the implementation. A change to process topology, crate responsibilities, dependency direction, data flow, mutable ownership, persistence, security, platform support, provider/UI boundaries, or verification gates must update it in the same commit. Before delivery, compare its claims with Cargo manifests, public exports, application entry points, and actual call paths.

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
