# Aeris Terminal Audit 04: Platform, Infrastructure, Persistence, Tooling

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal` (branch `main`, 4 commits ahead of origin, 1189 commits).
Scope: crate graph, platform_runtime, launcher/lifecycle/update, account_runtime, credential vault, persistence,
observability, panic/lint posture, dependencies, vendored GPUI, CI, `tools/`, `plan/`.
Method: read-only (git, ripgrep, file reads). No build, test, or file modification in the repository.

## Summary

The product core (domain crates, market_runtime, trading_runtime) has a reasonably clean dependency shape, but the
platform layer carries large dormant subsystems and weak operational foundations:

- About 20k lines of launcher, signed release lifecycle, rollout, and updater code are dormant: the launcher cannot
  start without packaging-time environment variables, update checks are compiled off, and the desktop still spawns
  the launcher on every start if the binary exists.
- `account_runtime` (about 5.3k production lines plus 3k test lines) is dormant (`AUTH_BACKEND_CONFIGURED = cfg!(test)`)
  but still drags reqwest/tokio/hyper/quinn into the desktop, and contains latent defects (token `Debug`,
  a vault payload limit above the Windows credential blob limit, a thread that parks forever on exit failure).
- The workspace claims `unsafe_code = forbid`, but `platform_runtime` downgrades it to `deny` and contains 68 unsafe
  blocks behind 8 `#[allow(unsafe_code)]`.
- Windows release builds use `windows_subsystem = "windows"`, and the only diagnostic channel is stderr. There is no
  log file, no panic hook, and no timestamps, so field diagnostics are effectively lost.
- There is no single-instance lock, so two desktop processes can share the same SQLite store, workspace state file,
  and provider sessions/credentials.
- The trading SQLite store has no `busy_timeout`, no integrity check, and no corruption recovery; a damaged file
  blocks startup.
- At least five separate atomic-write implementations exist; one named `write_atomic` is not atomic.
- Every CI workflow is `workflow_dispatch` only. Nothing runs on push or pull request.
- `tools/naming_check` (2748 lines) asserts source text with `.contains(`/`.find(` across 48 files, enforces
  presence of dormant code, and is co-edited in 196 of 768 recent commits, so it behaves as a change-detector
  rather than an independent architecture gate.
- `plan/` (8 tracked files, 206 KB, touched in 230 recent commits) contradicts the AGENTS rule against planning documents.

## Crate dependency graph

Derived from the `Cargo.toml` manifests (normal dependencies only; dev-dependencies noted where they matter).

```text
Domain (leaf)
  account            (no internal deps)
  instruments        (no internal deps)
  market_data        (no internal deps)
  trading         -> instruments
  observability      (leaf)
  provider_history   (leaf)
  contracts          (prost; leaf)

Platform
  platform_runtime -> observability
                      [contains hosted_broker product protocol, display_timing, power, network,
                       session_shutdown, credential_vault, lifecycle, launcher, update, rollout]

Adapters
  rithmic_adapter     -> platform_runtime, market_data, observability, provider_history
  hyperliquid_adapter -> platform_runtime, market_data

Runtimes
  market_engine   -> market_data
  market_runtime  -> contracts, market_data, market_engine, observability, platform_runtime,
                     provider_history, instruments, trading, all 4 adapters
                     (dev-dep: study_sdk)
  study_sdk       -> market_data, market_runtime, aeris_charts_indicators      <-- inverted
  account_runtime -> account, contracts, observability, platform_runtime (+ reqwest stack)
  context_runtime -> platform_runtime
  trading_runtime -> instruments, trading, rusqlite
  application     -> instruments, market_data, sha2

UI
  design_system      (leaf)
  terminal_ui       -> domain crates, design_system
  chart_integration -> application, design_system, observability, terminal_ui, aeris_charts_*

App
  desktop -> nearly every crate above, gpui_base (vendored, excluded from workspace)
```

### Direction and ownership violations

| ID | Edge / placement | Problem |
|----|------------------|---------|
| G1 | `study_sdk -> market_runtime` | An SDK meant for study authors depends on the full runtime and therefore every provider adapter. `market_runtime` dev-depends back on `study_sdk`. The SDK should depend on `market_data` contracts only. |
| G2 | `platform_runtime::hosted_broker` | Product protocol with hardcoded `https://app.aeristerminal.com` (`crates/platform/platform_runtime/src/hosted_broker.rs:20`) lives in the platform crate; adapters depend on `platform_runtime` to reach it. Product policy sits in a platform crate. Closed 2026-10-21: the module was deleted with the removed broker integrations. |
| G3 | `platform_runtime` as catch-all | Contains OS facilities (power, network, session shutdown, credential vault, atomic files), product release lifecycle (launcher, update, rollout, release identity), test-only gates (`live_market_gate`), and a display-timing module used only by stale tools. No cohesive responsibility. |
| G4 | `contracts` | Mixes in-process prost DTOs (a leftover of the removed process boundary) with the persisted workspace schema. Unused RPC messages remain (`GetAccountStatus`, `RefreshAccountProfile`). |
| G5 | `application` | `EmbeddedReplaySource` demo data is used only by `AerisChartView::new`/`Default`, which only tests call. Demo data ships in the production graph. |
| G6 | `desktop -> account_runtime` | Desktop builds `AccountService` only in the `--desktop-readiness` path (`apps/desktop/src/desktop.rs:4035`), but the reqwest/tokio/hyper/quinn stack is linked unconditionally. |

## Persistence inventory

| Store | Location in code | Format / versioning | Write strategy | Read / recovery | Issues |
|-------|------------------|---------------------|----------------|-----------------|--------|
| Workspace state `workspace-state.pb` | `apps/desktop/src/local_state.rs:175-213` | Protobuf (`contracts`), `schema_revision` fixed at 1; legacy `.frame` v21 migration | Temp file, `.bak` rotation, rename | Falls back to `.bak` | Hand-written rename dance duplicated elsewhere; schema revision never advanced, so no migration path is exercised |
| Chart chrome | `crates/ui/chart_integration/src/chart_chrome.rs:762-815` (write), `:636` (read) | Unversioned `key=value` text | Own coalescing saver | Unbounded `read_to_string`; unknown or invalid values silently default | No version field; corruption silently resets user layout; saver duplicates `workspace_persistence.rs` |
| Trading store `trading.sqlite3` | `crates/runtime/trading_runtime/src/store.rs:357-380` (open), `:382-482` (migrations) | SQLite, `user_version` with 19 migrations in chained `if` blocks | WAL, `synchronous=FULL`, foreign keys on | No `busy_timeout`, no `integrity_check`, no backup; open or migration error is a startup failure | Corruption or a second process (no instance lock) blocks or breaks startup |
| Trading exports | `crates/runtime/trading_runtime/src/store.rs:3373-3383` | CSV/JSON | `write_atomic`: remove target, then rename temp; no fsync | n/a | Not atomic: a crash between remove and rename loses the previous file |
| Release lifecycle JSON | `crates/platform/platform_runtime/src/lifecycle.rs:2179-2211` | JSON | `write_json_atomic` (temp, flush, sync, rename) | Typed parse | Correct but a separate implementation; dormant |
| Stable launcher promotion | `crates/platform/platform_runtime/src/launcher.rs:300-323` | Binary copy | `fs::rename` | n/a | Another implementation; dormant |
| Platform atomic file | `crates/platform/platform_runtime/src/atomic_file.rs` | n/a | `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` via `unsafe` | n/a | `replace_file_atomically` has no external callers |
| Account marker | `crates/runtime/account_runtime/src/mod.rs:321-350` | Text marker | Own write | n/a | Dormant |
| Live market gate evidence | `crates/platform/platform_runtime/src/live_market_gate.rs` | JSON | Temp + rename | n/a | Used only by tests and a smoke binary |
| Readiness report | `apps/desktop/src/desktop.rs:4095` | JSON | `fs::write` | n/a | Non-atomic; diagnostic only |
| Chart PNG export | `crates/ui/chart_integration/src/export.rs:463-466` | PNG | `fs::write` on background executor | n/a | Non-atomic; acceptable for user export, but partial files possible |
| Notification AUMID | `apps/desktop/src/user_notifications.rs:131-140` | Registry | Registry write | n/a | Fine |
| Native credentials | `crates/platform/platform_runtime/src/credential_vault.rs` | Windows Credential Manager via keyring | Global mutex `NATIVE_CREDENTIAL_ACCESS` (`:9`) | Legacy probes for `tradingplot`/`axiusflow` services on every miss (`:286-298`) | Mutex poisoning turns into a permanent `AccessSerialized` error; Asceify brand era not migrated |

Count: at least five independent atomic-write implementations (local_state, lifecycle, launcher, atomic_file,
live_market_gate) plus one misnamed non-atomic one (trading exports).

## Lint and panic posture

### Lint suppressions (51 total: 49 `allow`, 2 `expect`)

| Lint | Count | Notes |
|------|-------|-------|
| `clippy::too_many_lines` | 10 | Long functions in desktop and runtimes |
| `clippy::too_many_arguments` | 9 | Constructor/plumbing functions |
| `unsafe_code` | 8 | All in `platform_runtime`; crate sets `unsafe_code = "deny"` (`crates/platform/platform_runtime/Cargo.toml:62`) instead of workspace `forbid` |
| `non_snake_case` | 7 | Win32 FFI names |
| `clippy::struct_excessive_bools` | 4 | State structs |
| `clippy::all` / `clippy::pedantic` | 2 each | Generated protobuf: rithmic `lib.rs:10` and a since-removed broker adapter's `lib.rs:3` (acceptable) |
| `dead_code` | 3 | Hides dormant paths |
| `unused_imports` | 1 | `crates/platform/platform_runtime/src/lifecycle.rs:7` |

### Unsafe blocks

| Module (`platform_runtime`) | Unsafe blocks | Live use |
|-----------------------------|---------------|----------|
| `session_shutdown` | 30 | Yes (Windows session end) |
| `power` | 20 | Yes |
| `network` | 13 | Yes |
| `display_timing` | 3 | No (stale pacing tools only) |
| `atomic_file` | 1 | No external callers |
| `io_cancellation` | 1 | Yes |
| Total | 68 | |

### Panic surface

| Item | Finding |
|------|---------|
| Production `unwrap` | 0 |
| Production `expect` | About 20 real sites (rithmic counts are mostly test fixtures; `trading/scripted.rs` is `cfg(test)`) |
| UI-thread `expect` | `apps/desktop/src/desktop.rs:5619`, `:5642`; `chart_integration` `view.rs:148`, `:226`, `:1238` |
| Panic hook | None installed |
| `panic` profile | Default unwind; no `panic = "abort"` |
| `catch_unwind` | Only study runtime and `session_shutdown` |
| Thread spawn sites | About 85; panics in background threads are not reported anywhere persistent |

## CI coverage

| Workflow | Trigger | Runner | Covers | Gaps |
|----------|---------|--------|--------|------|
| `ci.yml` | `workflow_dispatch` only | Self-hosted Linux + Windows | fmt, clippy, build, test, perf example, Windows PowerShell evidence-verifier scripts | Never runs automatically on push or PR |
| `live_market_gates` (hyperliquid) | `workflow_dispatch` | Self-hosted Linux | Live public market smoke | Manual only |
| `live_market_gates` (rithmic) | `workflow_dispatch` | `rithmic-credentials` runner | Live Rithmic smoke with provider kit copy | Manual only; depends on a single credentialed host |
| `macos.yml` | `workflow_dispatch` | `macos-latest` (hosted) | macOS build | Hosted runner, inconsistent with the self-hosted policy in `ci.yml` |
| `macos_preview.yml` | `workflow_dispatch` | `macos-latest` (hosted) | Preview build | Same as above |
| `tools/naming_check` | Run only through `cargo test` | n/a | About 67 source-text assertions | The `main()` binary is never executed in CI |
| `gpui_base` (vendored) | None | n/a | Not in the workspace | No lints, no forbid-unsafe, no tests |
| Stale tooling | `test_evidence_verifiers.ps1` in `ci.yml` | Windows | Still tests `verify_physical_pacing_matrix.ps1` | Verifies output of a workflow whose driver no longer works |

## Findings

### PL-01 Dormant release, launcher, rollout, and updater stack

- Severity: High
- Category: Bloat / maintainability
- Evidence:
  - `crates/platform/platform_runtime/src/lifecycle.rs` 4233 lines (production 1-2352), `apps/desktop/src/bin/aeris_launcher.rs` 3025 lines (production 1-1950), plus `release_delivery.rs`, `rollout.rs`, `release_identity`, `update.rs` (1299 lines), `aeris_setup.iss`. About 20k lines total.
  - Launcher fails in `embedded_verifying_key()` (`launcher.rs:103`, `:1852`) unless `AERIS_RELEASE_VERIFYING_KEY`, `AERIS_RELEASE_BASE_URL`, and `AERIS_BOOTSTRAP_MIN_GENERATION` are set at build time.
  - `UPDATE_BACKEND_CONFIGURED = false` (`update.rs:40`, `:114`).
  - Desktop `run()` (`apps/desktop/src/desktop.rs:5434-5438`) still spawns `aeris_launcher --promote-stable-launcher` if the binary exists (`desktop.rs:5509-5532`).
  - Legacy `AXIUSFLOW` wire tokens and `TRANSITIONAL` Aeris tokens (`update.rs:35-37`, `launcher.rs:40-42`).
  - Only live pieces: `native_data_root`, `migrate_legacy_native_data_root` (`desktop.rs:5431`), `native_install_root`, `verify_windows_publisher_signature`, `current_release_identity`.
- Root cause: The retired Axiusflow publication path was disabled by flags instead of removed; AGENTS explicitly keeps it "for future integration", and `naming_check` enforces its presence.
- Fix: Move the few live functions (data root, legacy data-root migration, install root, signature check, release identity) into a small `platform_runtime::install` module. Delete launcher/update/rollout/release delivery and the Inno script, or move them to an unbuilt `release` crate behind a Cargo feature that is off by default. Remove the startup launcher spawn. Drop `AXIUSFLOW`/transitional tokens. Update `naming_check` and the AGENTS release section in the same change.
- Verification: `cargo tree -p desktop` no longer lists launcher-only deps; `rg "promote-stable-launcher|UPDATE_BACKEND_CONFIGURED|AXIUSFLOW" crates apps` returns nothing; desktop starts with no child process (Process Monitor or `Get-CimInstance Win32_Process`).

### PL-02 `unsafe_code` forbid is not enforced in platform_runtime

- Severity: High
- Category: Architecture / safety
- Evidence: Workspace lints set `unsafe_code = "forbid"`; `crates/platform/platform_runtime/Cargo.toml:62` overrides with `deny`; 8 `#[allow(unsafe_code)]` and 68 unsafe blocks (session_shutdown 30, power 20, network 13, display_timing 3, atomic_file 1, io_cancellation 1). AGENTS: "Workspace-wide `unsafe_code` remains forbidden."
- Root cause: Win32 FFI was added directly instead of through safe wrappers (`windows` crate safe APIs or a dedicated FFI crate).
- Fix: Either (a) acknowledge the exception in AGENTS and isolate all FFI into one `platform_sys` crate with `deny` + `#![warn(clippy::undocumented_unsafe_blocks)]` and `// SAFETY:` comments, keeping `forbid` everywhere else; or (b) replace with safe abstractions. Delete `display_timing` (PL-10) and `atomic_file` unsafe (PL-06) immediately; `MoveFileExW` only adds `WRITE_THROUGH` over `std::fs::rename` plus `sync_all`.
- Verification: `rg -c "unsafe \{" crates apps` limited to the FFI crate; `cargo clippy -p platform_sys -- -D clippy::undocumented_unsafe_blocks`.

### PL-03 Diagnostics are lost in Windows release builds; no panic hook or log file

- Severity: High
- Category: Maturity / observability
- Evidence: `crates/platform/observability/src/diagnostic_output.rs` writes only to stderr via `diagnostic!`; `apps/desktop/src/main.rs` sets `windows_subsystem = "windows"` for release, so stderr is detached. No log file, rotation, timestamps, or levels. No `std::panic::set_hook`. About 85 thread spawn sites; `catch_unwind` only in the study runtime and `session_shutdown`.
- Root cause: Observability was built for development consoles; no production sink was designed.
- Fix: Add a bounded rotating file sink in `observability` under `native_data_root()/logs` (size-capped ring, timestamps, level, thread name), with token redaction. Install a panic hook at startup that writes the panic message, location, thread, and backtrace to that sink before the default hook. Add a named-thread spawn helper that reports panics.
- Verification: Release build, force a panic on a background thread through a debug flag; confirm a log entry with backtrace appears and file size stays capped across many restarts.

### PL-04 No single-instance lock

- Severity: High
- Category: Bug / data integrity
- Evidence: No named mutex, lock file, or `CreateMutexW` usage in `apps/desktop`. Two desktop processes would both open `trading.sqlite3`, both write `workspace-state.pb` (last writer wins via rename), both open provider sessions (Rithmic has provider concurrency limits per AGENTS), and both use the credential vault.
- Root cause: The single-process architecture assumes one process but never enforces it.
- Fix: Acquire an exclusive lock on `native_data_root()/instance.lock` (`std::fs::File::try_lock`, stable in the pinned toolchain if 1.89+, otherwise `fs4`) before opening any store; on contention, focus the existing window (or show a message) and exit.
- Verification: Start two instances; the second exits without touching the database (check `trading.sqlite3-wal` modification times and provider session count).

### PL-05 Trading SQLite store lacks busy timeout, integrity check, and corruption recovery

- Severity: Medium-High
- Category: Durability / recovery
- Evidence: `crates/runtime/trading_runtime/src/store.rs:357-380` sets WAL, `synchronous=FULL`, foreign keys, but no `busy_timeout`; 19 migrations implemented as chained `if user_version < N` blocks (`:382-482`); no `PRAGMA quick_check`; open or migration failure exits startup.
- Root cause: The store was built for the happy path; recovery was not designed.
- Fix: Set `busy_timeout`; convert migrations to a table of `(version, sql)` executed in a transaction per step; run `quick_check` on open; on failure, rename the file to `trading.corrupt-<timestamp>.sqlite3`, start fresh, and surface a user-visible notice. Back up before each migration.
- Verification: Unit tests that open a truncated database file and a future `user_version`; check the quarantine file exists and startup proceeds.

### PL-06 Five atomic-write implementations; trading `write_atomic` is not atomic

- Severity: Medium
- Category: Patch-on-patch / durability
- Evidence: `apps/desktop/src/local_state.rs:175-213`, `lifecycle.rs:2179-2211`, `launcher.rs:300-323`, `atomic_file.rs` (unused externally), `live_market_gate.rs`, and `trading_runtime/src/store.rs:3373-3383` (removes the target and then renames, with no fsync). Coalescing savers duplicated between `chart_integration` `workspace_persistence.rs` and `chart_chrome.rs`.
- Root cause: Each feature added its own file writer.
- Fix: One `platform_runtime::durable_file::replace(path, bytes)` (temp in the same directory, write, `sync_all`, rename, which replaces atomically on Windows in std, then directory sync on Unix) and optional `.bak` rotation. Replace all call sites; delete the rest. One generic coalescing saver.
- Verification: `rg "fn write_atomic|fn write_json_atomic|replace_file_atomically|\.tmp\"" crates apps` shows one definition; crash-injection test between write and rename keeps the previous file intact.

### PL-07 Dormant account_runtime with latent defects and a heavy dependency stack

- Severity: Medium
- Category: Bloat / latent bugs / security
- Evidence:
  - `DEFAULT_AUTH_ISSUER = ""` (`crates/runtime/account_runtime/src/mod.rs:51`); `AUTH_BACKEND_CONFIGURED = cfg!(test)` (`apps/desktop/src/account.rs:6`).
  - `retain_account_refresh_quiesce_for_exit` parks a thread forever on failure (`apps/desktop/src/desktop.rs:302-342`), so exit can hang.
  - `VerifiedTokens` derives `Debug` and `Clone` with plain `String` tokens and no zeroization (`account_runtime/src/oidc.rs:225-235`).
  - Profile vault payload limit 4096 bytes (`mod.rs:61`) exceeds the Windows `CRED_MAX_CREDENTIAL_BLOB_SIZE` of 2560 (windows-native-keyring-store `utils.rs:84-100`).
  - Vault store performed while holding the state mutex (`mod.rs:2121-2140`).
  - reqwest/tokio/hyper/quinn linked only for an avatar `img(photo_url)` (`platform_menu.rs:135`) that is gated off (`platform_menu.rs:370`).
  - Legacy `AXIUSFLOW_AUTH_ISSUER` tests remain.
- Root cause: Auth was disabled by constants rather than compiled out.
- Fix: Put `account_runtime` and avatar loading behind a `hosted-account` Cargo feature, off by default. Before re-enabling: redact `Debug` for tokens (`secrecy`/`zeroize`), cap the vault payload at 2560 bytes or split it, perform vault I/O outside the lock, and replace the infinite park with a bounded join plus logged abandonment.
- Verification: `cargo tree -p desktop -e normal | rg "reqwest|quinn|hyper"` is empty with default features; a unit test asserts `format!("{:?}", tokens)` contains no token text.

### PL-08 `naming_check` pins source text and is co-edited with the code it checks

- Severity: Medium
- Category: Patch-on-patch / test quality
- Evidence: `tools/naming_check` 2748 lines, about 67 tests, 243 `.contains(` and 40 `.find(` calls over 48 pinned files; tests at lines 1657-1854, 1857-1884, 1887-1922, 2311, 2427 require dormant launcher/account code to exist. Touched in 196 of 768 commits since 2026-09-01 (54 of them `fix`). The markdown inventory checks only the repository root and misses `plan/` and `study_sdk/readme.md`. The binary `main()` never runs in CI.
- Root cause: Architecture rules were encoded as string matches against implementation text, so every refactor must edit the checker.
- Fix: Keep structural checks that are robust (crate dependency direction via `cargo metadata`, forbidden crate names, file naming, forbidden imports by path). Replace text pins of function bodies with behavior tests in the owning crates. Delete assertions that require dormant code. Make the markdown inventory recursive.
- Verification: `naming_check` under about 800 lines; a pure rename refactor in `desktop.rs` does not require editing `naming_check`.

### PL-09 CI is manual-only

- Severity: High
- Category: Maturity / process
- Evidence: Every workflow in `.github/workflows` has only `workflow_dispatch`. `ci.yml` runs fmt, clippy, build, test on self-hosted Linux and Windows only when triggered by hand. macOS workflows use hosted `macos-latest`.
- Root cause: Self-hosted runners are not always online, so triggers were removed instead of gating on availability.
- Fix: Add `push` to `main` and `pull_request` triggers for `ci.yml` (with `concurrency` cancellation); keep live market gates manual or scheduled. If self-hosted availability is the issue, run fmt/clippy/test on hosted `windows-latest` and keep self-hosted for evidence scripts.
- Verification: A push to `main` produces a CI run; a deliberately failing test blocks the PR.

### PL-10 Stale physical-pacing tooling and display_timing module

- Severity: Medium
- Category: Bloat / broken tooling
- Evidence: `tools/run_physical_pacing_profile.ps1:80` passes `--windowed-benchmark`, which was removed from the desktop in commit `53a4ef3c`. `verify_physical_pacing_matrix.ps1` is still exercised by `test_evidence_verifiers.ps1` in `ci.yml`. `platform_runtime::display_timing` (951 lines, 3 unsafe) is used only by these tools. Unreferenced scripts: `prepare_physical_pacing_capture`, `run_rithmic_authorized_silence_recovery`, `run_rithmic_provider_observed_silence`, `test_native_transition_capture_verifier`, `test_physical_pacing_workflow`, `test_window_controls`, `verify_platform_css_mirror`, and several `.sh` scripts.
- Root cause: The benchmark flag was removed without removing its tooling.
- Fix: Delete the pacing scripts, their verifier tests, and `display_timing`; delete other unreferenced scripts or wire them into CI.
- Verification: `rg -- "--windowed-benchmark" tools apps` empty; every remaining `tools/*.ps1` is referenced by CI or documentation.

### PL-11 `plan/` directory contradicts the AGENTS documentation rule

- Severity: Low
- Category: Process / maintainability
- Evidence: 8 tracked files, 206 KB, touched in 230 commits since 2026-09-01. AGENTS: "Do not create planning documents, architecture diaries...". Root `Readme` has no extension (a naming exception). Stale `.gitignore` entries (`/financial-charts/`, `/.reference/`, `__pycache__`). Untracked `.cache` is 25 MB.
- Root cause: Planning moved into the repository despite the rule; the root markdown check does not recurse.
- Fix: Move `plan/` out of the repository (issue tracker or wiki) or update the rule deliberately; rename `Readme` to `README.md`; prune `.gitignore`.
- Verification: `git ls-files plan` empty; recursive markdown inventory in `naming_check` passes.

### PL-12 Vendored `gpui_base` is unlinted and oversized

- Severity: Medium
- Category: Bloat / supply chain
- Evidence: `third_party/gpui_base` (about 21.5k lines) is excluded from the workspace, so workspace lints, forbid-unsafe, and CI tests do not apply; its dependencies are unpinned ranges. It includes an LSP-backed editor (about 5.4k lines, `lsp-types`), while the desktop uses only `init` and the text input.
- Root cause: A full component library was vendored to obtain a text input.
- Fix: Trim the vendor to the used modules (or replace with a small input component in `terminal_ui`), add it to the workspace with lints, and pin dependencies through the workspace.
- Verification: `cargo tree -i lsp-types` empty; `gpui_base` appears in `cargo metadata` workspace members.

### PL-13 `study_sdk` depends on `market_runtime`

- Severity: Medium
- Category: Architecture
- Evidence: `crates/runtime/study_sdk/Cargo.toml` depends on `market_runtime`; `market_runtime` dev-depends on `study_sdk`. Any study author compiles every provider adapter.
- Root cause: SDK types were reused from the runtime instead of being defined in a contract crate.
- Fix: Move the shared study types into `market_data` (or a `study_contracts` crate); `market_runtime` depends on the SDK contracts, not the reverse.
- Verification: `cargo tree -p study_sdk | rg adapter` empty.
- Status (2026-10-28, M8.9): accepted rather than moved (stabilization D27). `study_sdk` is the published-API facade above the in-process study runtime, used only by the desktop's built-in study packages; `market_runtime` only dev-depends on it, which naming_check enforces.

### PL-14 `contracts` mixes in-process DTOs with persisted schema

- Severity: Low-Medium
- Category: Architecture / dead code
- Evidence: prost messages for the removed process boundary sit next to the persisted workspace schema; `GetAccountStatus` and `RefreshAccountProfile` are unused.
- Root cause: The process boundary was removed but its message set was kept.
- Fix: Keep only the persisted workspace schema in `contracts` (rename to `workspace_schema`); replace in-process DTOs with plain Rust types in their owning crates.
- Verification: `rg "GetAccountStatus|RefreshAccountProfile"` empty; `contracts` has no consumers outside persistence.
- Status (2026-10-28, M8.9): fixed. `contracts` holds only the prost workspace schema and keeps its crate name; in-process types moved to `market_runtime` and `account_runtime`, and only the desktop and `market_runtime` depend on it.

### PL-15 Dependency hygiene

- Severity: Low
- Category: Bloat
- Evidence: `unicode-segmentation` unused in desktop; `sha2` in desktop only for diagnostics and the `readiness_conformance` test; `rpassword` a production dependency of the rithmic library for one binary; `socket2`, `num-traits`, `signal-hook`, `resvg`, `arboard` pinned per crate rather than in `[workspace.dependencies]`; `resvg 0.48.1` in `chart_integration` duplicates GPUI's `0.46`; duplicate `sysinfo` and `sha2` versions; 921 packages in `Cargo.lock`.
- Root cause: No dependency audit step (`cargo machete`/`cargo udeps`, `cargo deny`).
- Fix: Remove unused crates; move `rpassword` behind a `bin` feature; centralize pins; align `resvg` with GPUI; add `cargo deny check` (bans duplicates with an allowlist) to CI.
- Verification: `cargo machete` clean; `cargo tree -d` duplicates reduced to the allowlist.
- Status (2026-10-28, M8.10): fixed except the CI `cargo deny` step. Crates that a scan of each
  manifest against its crate's sources found unused are removed (`cargo machete` was not run),
  `rpassword` is behind the rithmic `provision` feature, the per-crate pins are centralized, and
  `resvg` uses GPUI's 0.46. The remaining `sha2`, `sysinfo` and `tiny-skia` duplicates come
  from GPUI, `ed25519-dalek` or Aeris Charts.

### PL-16 Legacy brand migrations and tokens

- Severity: Low
- Category: Patch-on-patch
- Evidence: Brand history Axiusflow, TradingPlot, Asceify, Aeris. Credential vault probes `tradingplot`/`axiusflow` services on every missed load (`credential_vault.rs:286-298`); Asceify is never migrated; `AXIUSFLOW` tokens in update/launcher; legacy data-root migration runs on every start (`desktop.rs:5431`).
- Root cause: Migrations were added per rename without a completion marker or retirement date.
- Fix: Run one-time migrations under a recorded marker in the workspace state; stop probing legacy services after success; set a removal version.
- Verification: A second start performs no legacy credential lookups (trace via the new log sink).

### PL-17 Hardcoded user agent version and unbounded context request wait

- Severity: Low
- Category: Correctness
- Evidence: `crates/runtime/context_runtime/src/sources.rs:12` hardcodes `"Aeris/0.2.3"`; `context_runtime/src/lib.rs:336-344` `request_reply` does a blocking `recv()` with no timeout and is called on the GPUI background executor.
- Root cause: Version string not derived from `CARGO_PKG_VERSION`; no timeout policy.
- Fix: `concat!("Aeris/", env!("CARGO_PKG_VERSION"))`; use `recv_timeout` with an explicit error.
- Verification: Unit test with a stalled worker returns a timeout error.

### PL-18 Chart chrome file is unversioned and read without bounds

- Severity: Low
- Category: Durability
- Evidence: `crates/ui/chart_integration/src/chart_chrome.rs:636` reads the whole file; `:762-815` writes `key=value` with no version; invalid values silently default.
- Root cause: An ad-hoc format grew without a schema.
- Fix: Fold chart chrome into the protobuf workspace state (one store, one writer) or add a version header and size cap; log rejected keys.
- Verification: A test with a 10 MB or malformed file loads defaults with a logged warning and does not stall.

### PL-19 Unused or test-only exports in platform_runtime

- Severity: Low
- Category: Dead code
- Evidence: `NativeSessionShutdownMonitor` and `replace_file_atomically` have no external callers; `live_market_gate` is used only by tests and a smoke binary (`hyperliquid_realtime.rs:1956` in test code); `display_timing` only by stale tools.
- Root cause: Exports were left after their consumers were removed.
- Fix: Delete them or move test gates under `#[cfg(any(test, feature = "live-gates"))]`.
- Verification: `cargo build -p desktop` passes after removal; `rg` shows no references.

## Recommended restructuring (ordered)

1. Operational foundation: rotating log file and panic hook in `observability` (PL-03); single-instance lock before any store opens (PL-04).
2. Durable storage: one `durable_file::replace` and one coalescing saver (PL-06); SQLite busy timeout, integrity check, quarantine, table-driven migrations (PL-05); version or fold chart chrome into workspace state (PL-18).
3. CI triggers: `push`/`pull_request` for fmt, clippy, test; add `cargo deny`/`cargo machete` (PL-09, PL-15).
4. Remove dormant subsystems: delete or feature-gate launcher/update/rollout (PL-01) and `account_runtime` plus the reqwest stack (PL-07); delete `display_timing`, `atomic_file` unsafe, unused exports (PL-10, PL-19); update AGENTS release and auth sections in the same change.
5. Isolate unsafe: one FFI crate with documented `SAFETY` blocks; restore `forbid` elsewhere (PL-02).
6. Slim `naming_check` to structural checks based on `cargo metadata` and paths; delete dormant-code assertions; recurse the markdown inventory (PL-08).
7. Fix dependency direction: `study_sdk` on contracts only (PL-13); split `platform_runtime` into OS facilities and product modules (G3; G2 closed when `hosted_broker` was deleted); reduce `contracts` to the persisted schema (PL-14).
8. Clean the repository: remove `plan/`, stale tools, legacy brand migrations after a marker (PL-11, PL-10, PL-16); trim `gpui_base` (PL-12).
