# AGENTS.md

Instructions for coding agents working in the Axiusflow native repository.

Axiusflow is a local-first Rust/GPUI trading terminal. The desktop is the single application process.
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

## Architecture invariants

- `MarketEngine` is the single market-demand owner inside `market_runtime`. Provider sessions are
  created and owned only by that in-process runtime; never create a session/runtime per chart or UI surface.
- Symbol, timeframe, viewport, tab, and layout changes must not tear down a healthy provider session.
- `market_runtime` is the single owner that merges on-demand history and live state. Preserve one
  canonical forming candle, contiguous completed history, exact generation fencing, and explicit recovery.
- Retired clients, generations, selections, sessions, and publications must never mutate current state.
- Production queues, caches, retries, and background work remain bounded with explicit overflow and
  cancellation behavior.
- The desktop must stay provider-neutral. It does not own provider adapters, storage implementations,
  or canonical candle state.
- Market history is loaded on demand and bounded; do not restore market-history persistence or a
  second desktop-owned market-state model.
- Do not restore `apps/engine`, `local_engine_client`, a secondary market process, local process transport, or market autostart.
- UI-thread code performs no blocking network, disk, process, or shutdown work. Background workers do
  not mutate GPUI state directly.
- Workspace-wide `unsafe_code` remains forbidden.

Architecture assertions in `tools/naming_check` are authoritative when they are stricter than prose.

## Market-data and provider work

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

## Nucleus Charts

Nucleus Charts is a separate pinned Git dependency. Do not clone or vendor it into this repository.
Host integration belongs in `crates/ui/chart_integration`. When intentionally updating Nucleus, change
only the `nucleuscharts_*` revisions required by that update; do not update GPUI incidentally.

## Authentication and website coordination

Authentication is an end-to-end system shared with:
`C:\Users\devraj\Downloads\Devlopment\axiusflow-website`.

- Native ownership: desktop account presentation plus in-process `account_runtime` PKCE, loopback
  callback, token exchange/verification, native vault material, account linking, and lease validation.
- Website ownership: `workers/auth` Better Auth/OIDC, browser sign-in/account pages, D1 account and
  billing state, OAuth/email entry points, native link/lease endpoints, checkout/portal, and webhooks.
- Any auth/profile/subscription/entitlement change must inspect both repositories and verify the full
  browser -> callback -> account runtime -> vault -> desktop path when relevant.
- Browser success alone is not native authentication success.
- Keep OAuth tokens, provider cookies, refresh material, and credentials out of desktop UI and logs.

## Design system coordination

`crates/ui/design_system/platform.css` and the Rust tokens in `crates/ui/design_system/src/lib.rs` are
the native platform design-system source. The website keeps a mirrored portable stylesheet at
`src/styles/platform.css` in the sibling repo.

When changing shared platform tokens such as colors, typography, radii, or interaction states:

1. update the native source and Rust token mapping together;
2. update the website mirror in the same coordinated task;
3. run the website sync/tests so auth receives the same stylesheet;
4. do not reintroduce one-off duplicate platform tokens in landing/auth/account CSS.

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

## Release workflow

Only publish/install when the maintainer asks for a release or end-to-end installed validation.

- Before release, signer-provisioning, or release-tooling work, read the maintainer-machine checkpoint in
  the `Readme` update-pipeline section. It records the current self-hosted runner identities, protected
  tool paths, recovered trust root, missing production trust material, and the exact rule for when a trust
  reset is allowed. Verify volatile runner/tool state against the machine before acting, and update that
  checkpoint when provisioning materially changes.
- Release from a clean, pushed `main` worktree only.
- Read the current public stable channel first and choose the next install generation.
- Production publication is CI-authoritative: dispatch `.github/workflows/release.yml` from `main`. Both
  release jobs run on maintainer-owned Windows runners on the maintainer's machine; GitHub is the
  trigger/audit layer and no GitHub-hosted runner, protected environment, or Actions binary artifact is
  part of the release path. The `release-build` identity owns qualification only. A distinct
  `release-signing` Windows identity owns the production Authenticode certificate, Ed25519 key file, and
  authenticated Wrangler session; production signing/deployment secrets are not stored in GitHub. The
  signer receives candidate binaries only after their hashes have been frozen and handed across the local shared directory. The
  signing job must use an independently provisioned publisher whose SHA-256 is anchored outside the build
  runner, and every external signing/deployment tool must be pinned by SHA-256 and live outside build-runner
  write authority. Do not publish the stable channel from an ordinary shell or invoke the release publisher
  outside that workflow.
- `tools/publish_release.ps1 -Generation <N> -PackageOnly ...` is the local qualification/package path.
  The production workflow uses `-PrebuildOnly` on the build runner, validates the locally handed-off binaries
  against GitHub-controlled qualification hashes, and then uses the independently provisioned publisher with
  `-SkipQualification -SkipBuild -PublisherPath -PublisherSha256`; do not bypass qualification,
  trusted-publisher digest verification, public-key binding, Authenticode/RFC 3161 signing,
  manifest/provenance signing, immutable upload, rollout policy, or public-channel verification.
- After publishing, verify the live stable channel and installer hash before installing.
- For installed-app validation, verify the active lifecycle pointer, signed manifest, installed binary
  hashes and Authenticode publisher/timestamp, rollback-compatibility asset, stable/versioned launcher byte
  equality, and that the running desktop path points at the intended immutable generation with no secondary
  market process or retired market autostart registration.
- Never claim live provider, account, or visual behavior was tested unless that exact path was exercised.

## Rust and documentation conventions

- Rust edition/toolchain is repository-pinned; dependencies stay pinned to intentional versions.
- Avoid `unwrap`/`expect` outside tests and unavoidable startup invariants.
- Propagate errors intentionally; do not silently discard failures.
- Keep file/directory names snake_case.
- Fix Clippy findings rather than adding broad lint suppression.
- Do not create planning documents, architecture diaries, nested agent files, or duplicate README files.
  Durable architecture rules belong in code/tests/`tools/naming_check`; implementation notes belong near
  the code they describe.

## Delivery

Report exactly what changed, what was verified, what was deployed/published/installed if applicable,
and what still requires maintainer-only credentials or visual/manual confirmation. Do not present an
untested assumption as a completed result.
