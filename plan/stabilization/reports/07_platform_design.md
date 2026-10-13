# 07 - Platform stabilization: launcher, lifecycle, account runtime, and foundations

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal` (branch `main`, toolchain 1.99.0).
Read-only audit. No repository files were modified and no cargo commands were run.
Builds on `04_platform_infra.md` (PL-01..PL-19) and `05_cross_cutting.md`; findings already
recorded there are referenced, not repeated.

Maintainer decision honored throughout: the launcher, signed lifecycle, updater, and account runtime
are kept for the future AWS backend. Every proposal below stabilizes or trims them; none deletes them.

## 0. Corrections to earlier audit files

| Earlier claim | Correction (evidence) |
|---|---|
| 04 places the crate at `crates/platform/platform_runtime` | Actual path is `crates/platform_runtime`. |
| 04 PL-07: account runtime pulls in reqwest/tokio/hyper | `account_runtime` uses `ureq`. The reqwest stack comes from the desktop installing Zed's `reqwest_client` as the GPUI `HttpClient` (`apps/desktop/src/desktop.rs:295-300`), used only to load the avatar image `img(photo_url)` (`platform_menu.rs:135-136`). |
| 04: `replace_file_atomically` has no callers | It has no callers outside the crate, but it is used by the launcher (`aeris_launcher.rs:599`, `:949`) and lifecycle (`lifecycle.rs:2216`). |
| Implicit assumption that diagnostics reach the user | The release desktop is built with `windows_subsystem = "windows"` (`apps/desktop/src/main.rs:1-4`), so stderr goes nowhere. The `diagnostic!` macro (`crates/observability/src/diagnostic_output.rs:13-23`) writes only to stderr. There are 194 call sites in 36 files. In an installed build, Aeris currently records no diagnostics at all. |

---

# Part A - Flaw review

## A1. Flow map (as implemented)

```
INSTALL (Inno, aeris_setup.iss)
  setup.exe -> copies payload -> aeris_launcher.exe --install <args>
     run() [aeris_launcher.rs:98] -> embedded_verifying_key() [:103, :1852]   (fails if no build key)
     -> install_remote_update / bootstrap path -> lifecycle stage+activate -> desktop launch

NORMAL LAUNCH  (Start menu -> aeris_launcher.exe, no args)
  run() -> LifecycleLock(.Aeris-lifecycle.lock, File::try_lock) [lifecycle.rs:2308]
     -> recover journal (update-0..6 / rollback-0..3 per-file states)
     -> audit_active_release (exact counts of versions/manifests) [lifecycle.rs:725-729]
     -> active pointer = highest generation in active/ [lifecycle.rs:581-615]
     -> spawn desktop; probation watch 5 s if a retained release exists [launcher :51]
          early exit -> rollback states -> quarantine file -> relaunch previous
  desktop run() [desktop.rs:5422]
     -> --desktop-readiness branch (launcher health probe)              [:5424]
     -> migrate_legacy_native_data_root() every start                   [:5431]
     -> schedule_versioned_launcher_promotion() on no-arg start         [:5434, :5509-5532]
     -> trading -> context -> market -> cTrader venue -> GPUI

UPDATE  (dormant: UPDATE_BACKEND_CONFIGURED=false, update.rs:40)
  desktop update.rs: check channel -> trust check launcher_is_trusted [update.rs:415]
     -> spawn launcher update_and_restart [launcher :648]
        launcher preflight: channel fetch (1 MiB bound, 15 s), manifest verify (ed25519 over
        re-serialized JSON, lifecycle.rs:388), download (10 min global timeout, https only,
        <=3 redirects, size-bounded by signed manifest, hash before rename), optional block
        reconstruction (release_delivery.rs; failures swallowed launcher :543), Authenticode audit
        via powershell.exe per exe (15 s each, lifecycle.rs:46, :2024-2096)
        -> READY token (AXIUSFLOW_UPDATE_RESTART_READY_V2) on stdout
     desktop waits <= 20 s (update.rs:28) -> COMMIT token on stdin -> desktop exits
        launcher: journal update-0..6 -> stage -> run candidate `--desktop-readiness`
        (60 s, launcher :33) -> readiness opens the REAL trading SQLite and migrates it
        (desktop.rs:4024-4103) -> report via fs::write (desktop.rs:4095)
        -> activate (new generation dir) -> relaunch -> probation -> rollback on early exit

UNINSTALL  (Inno [UninstallRun])
  aeris_launcher.exe --remove-all-local-data (needs verifying key, launcher :1848)
     -> wipes data roots: trading DB, workspace, vault credentials. No prompt.

PRODUCER SIDE: none. release.yml, release_handoff_cleanup.yml, tools/publish_release.ps1 were
deleted in f820339e. Nothing generates manifests, block plans, channel pointers,
rollback-compatibility.json, or invokes Inno. sign_release_manifest / sign_block_plan are only
called from tests.
```

## A2. Launcher and lifecycle findings

Severity scale: Critical (blocks a safe release), High (data loss or bricking risk once enabled),
Medium (reliability or security weakness), Low (cleanup).

| ID | Sev | Finding (evidence) | Fix |
|---|---|---|---|
| LC-01 | High | `run()` loads the embedded verifying key before dispatching any command (`aeris_launcher.rs:98-103`, `:1852`). In a build without `AERIS_RELEASE_VERIFYING_KEY`, every command fails, including `--launcher-identity`, `--promote-stable-launcher`, `--remove-all-local-data`, and uninstall (`uninstall_from_root`, `:1848`). An uninstall cannot depend on a signing key. | Parse the command first. Load the key only in the commands that verify signatures (install, update, normal launch audit). Add a unit test per command for the case where no key is present. |
| LC-02 | Critical | The producer side is missing (see A1). The consumer has 43 lifecycle, 20 launcher, and 6 block-plan tests, but the full pipeline (manifest, channel, block plan, rollback-compatibility, installer) cannot be produced or tested end-to-end. | Build a `tools/release_packager` binary plus an end-to-end harness (F13). |
| LC-03 | High | The manifest signature is ed25519 over `serde_json::to_vec(manifest)` re-serialization (`lifecycle.rs:388`). This ties canonical bytes to Rust struct field order and serde behavior, and there is no domain-separation prefix. The block plan does have one (`release_delivery.rs:39`). A field reorder or a serde upgrade silently invalidates all published signatures, and unknown fields are dropped and not signed. | Sign the raw manifest bytes as `b"AERIS_RELEASE_MANIFEST_V1\0" \|\| bytes`. Verify the raw bytes first, then parse with `#[serde(deny_unknown_fields)]`. Ship both the signature and the exact bytes. |
| LC-04 | Medium | Wire tokens are still Axiusflow-branded: `AXIUSFLOW_UPDATE_RESTART_READY_V2`, `..._COMMIT_V1`, and `TRANSITIONAL` Aeris aliases (`aeris_launcher.rs:40-42`, `update.rs:35-37`). The block-plan domain is `AXIUSFLOW_RELEASE_BLOCK_PLAN_V1` (`release_delivery.rs:39`). No Aeris release has shipped, so no compatibility is owed. | Rename to `AERIS_*_V1` in one commit. Delete the transitional aliases. Add a naming_check rule that `AXIUSFLOW` does not appear in production code. |
| LC-05 | High | The readiness probe runs the candidate build against the user's real trading SQLite and applies migrations (`desktop.rs:4024-4103`). If health fails or a later rollback happens, the older release finds a newer `user_version` and refuses to open it (`store.rs:387`). The rollback is then bricked. The rollback-compatibility epoch is a manual constant (`lifecycle.rs:37`) and is not linked to `SCHEMA_VERSION` (19). | The readiness probe migrates a copy made with `VACUUM INTO` in a temp dir and never touches the real file (F11). The manifest carries `max_store_schema`, and lifecycle refuses to roll back to a release whose `max_store_schema` is below the live DB `user_version` (F14). Take a pre-migration backup on the first real open after an upgrade (F10). |
| LC-06 | Medium | The restart handshake budget on the desktop side is 20 s (`update.rs:28`). Launcher preflight can include the channel fetch (15 s), a full bundle hash, and a PowerShell Authenticode audit per executable (15 s each). This can exceed 20 s and cause restart failures that look random. Not measured. | Do all verification before emitting READY, so the 20 s budget covers only READY to COMMIT. Replace PowerShell with WinVerifyTrust (LC-07). Add a timing test with a fake slow verifier. |
| LC-07 | Medium | Authenticode is checked by spawning `powershell.exe Get-AuthenticodeSignature` (`lifecycle.rs:2024-2096`), which is slow, depends on execution policy, and parses text. If `AERIS_AUTHENTICODE_CERT_SHA1` is absent at build time, the check is silently skipped (`lifecycle.rs:1982-1993`). | Call `WinVerifyTrust` plus a signer thumbprint check from `platform_sys` (F15). The build script fails a release-configured build (verifying key present) when the thumbprint is missing. |
| LC-08 | Medium | Block-reconstruction errors are swallowed: `download_release_bundle` uses `.is_ok()` (`aeris_launcher.rs:543`), and `verified_block_delivery` uses `.ok()?` throughout. The fallback is correct, but there is no record of why delta delivery failed. | Keep the fallback. Return a typed error and log it through the diagnostics sink (F03). |
| LC-09 | Medium | Probation only detects an early exit within 5 s (`aeris_launcher.rs:51`), and only when a retained release exists. A desktop that hangs or parks (AC-05) counts as healthy, which defeats crash-loop detection. | The desktop writes a durable `startup-complete` marker (generation + pid) after the workspace loads. The launcher accepts the generation only after it sees the marker within a bound (for example 60 s). Otherwise it quarantines and rolls back (F14). |
| LC-10 | Medium | Some publishes are not durable. Download artifacts are renamed with a plain `fs::rename` after `sync_all` but without write-through (`aeris_launcher.rs:1425`, `:1595`, `:1640`). `persist_stable_launcher` uses a plain rename (`:300-323`). | Use `StagedFile::commit` from F01 everywhere (MoveFileExW with WRITE_THROUGH, or rename plus parent fsync on Unix). |
| LC-11 | Medium | The readiness report is written non-atomically (`desktop.rs:4095`, and also `readiness_conformance.rs:756`). The report schema is declared twice, in desktop (`desktop.rs:4105-4129`) and launcher (`aeris_launcher.rs:2068-2092`, schema_version 4), with no shared type or contract test. Readiness also requires `provider_count > 0`. | Put one `ReadinessReport` type in `release_lifecycle` and have both sides use it. Write it durably. Add a round-trip contract test. Re-check whether `provider_count > 0` is meaningful for a fresh install. |
| LC-12 | High | Uninstall runs `--remove-all-local-data` with no prompt (`aeris_setup.iss` UninstallRun). It deletes the trading journal, workspace, and vault entries. Axiusflow cleanup lines remain (`aeris_setup.iss:56-64`, `:118-150`, `:194-199`). | Add an Inno `[Code]` uninstall page with "Remove my Aeris data" unchecked by default. Pass the flag only when the user checks it. Delete the Axiusflow cruft (F18). |
| LC-13 | Low | `remove_relocated_binary` writes a `.bat` that deletes the old binary after a `ping` delay, with paths interpolated unescaped (`aeris_launcher.rs:1793-1831`). A `%` in a profile path breaks it. | Drop the batch file. Rename the old binary to `*.retired-<pid>`, and have the launcher remove `*.retired-*` at the start of its next run (owned-path removal already exists in lifecycle). |
| LC-14 | Low | `show_user_launch_error` spawns PowerShell to show a message box (`aeris_launcher.rs:66`). | Call `MessageBoxW` through `platform_sys`. |
| LC-15 | Low | `process_is_running` scans every process with sysinfo (`aeris_launcher.rs:2249`), called in 20 ms polling loops (`:359`, `:873`, `:2141`). sysinfo appears twice in the lock (0.31 and 0.39). | Keep the `Child` handle, or `OpenProcess` plus `WaitForSingleObject` with a timeout in `platform_sys`. Remove sysinfo from the launcher, and from the lock if no other user remains (verify with `cargo tree -i sysinfo`). |
| LC-16 | Medium | The desktop spawns launcher promotion on every no-arg start (`desktop.rs:5434`, `:5509-5532`), including dev builds where `target/debug/aeris_launcher.exe` exists. The launcher then fails on the missing key (LC-01). The `Child` is dropped without being waited on or logged. | Only spawn when `current_release_identity().generation > 0` (installed build). Reap or log the outcome. |
| LC-17 | Low | `migrate_legacy_native_data_root` runs on every start (`desktop.rs:5431`), and the credential vault probes legacy `tradingplot`/`axiusflow` names on every miss (`credential_vault.rs:286-298`). | Keep until the maintainer's machine is confirmed migrated, then delete both (F19). |
| LC-18 | Medium | Recovery from the `Activated` journal state re-runs the health check, which executes candidate code during recovery. `audit_active_release` requires exact directory counts (`lifecycle.rs:725-729`), so one stray directory results in `UpdatePendingCleanup` and blocks launch. | Persist the readiness outcome in the journal before `Activated`, so recovery is deterministic (finish or roll back) without executing code. Move unknown entries to `orphaned/<ts>/` and continue, instead of blocking launch. |
| LC-19 | Low | `minimum_version` compares against the launcher's own `CARGO_PKG_VERSION` (`lifecycle.rs:343-349`), which mixes product semver with generation numbering. | Compare against an explicit `LAUNCHER_PROTOCOL_VERSION: u32`. |
| LC-20 | Low | Six build-time environment variables are read in five places: `AERIS_RELEASE_VERIFYING_KEY`, `AERIS_RELEASE_BASE_URL` (`:1144`), `AERIS_BOOTSTRAP_MIN_GENERATION` (`:1122`), `AERIS_RELEASE_IDENTITY`/`AERIS_INSTALL_GENERATION` (`release_identity.rs:17-22`), and `AERIS_AUTHENTICODE_CERT_SHA1`. A partially configured release build compiles silently. | Read them in one `build.rs`-validated `release_config` module. If any release variable is set, all must be set, otherwise compilation fails. |
| LC-21 | Medium | Test gaps: no end-to-end download, readiness-schema contract, producer, or durable-write crash tests. | Add a `ReleaseSource` transport seam (HTTPS via ureq in production, a directory source in tests). Keep the Range/size validation unit tests on the HTTP adapter (F13). |

What is sound and should be kept: monotonic generation and downgrade rejection (`aeris_launcher.rs:997-1012`,
`lifecycle.rs:763-768`), signed size bounds on every download (`write_bounded_download`, `:1732`),
hash-before-rename, path traversal guards (`safe_relative_path`, `lifecycle.rs:453`), the per-file journal states,
symlink-safe `remove_owned_path`, the deterministic rollout bucket (`rollout.rs`), and the
transaction-boundary unit tests.

## A3. Unsafe inventory

The workspace sets `unsafe_code = "forbid"` (`Cargo.toml:103`). `platform_runtime` lowers this to `deny`
(`crates/platform_runtime/Cargo.toml:62`) and allows unsafe per item.

| Location | FFI surface | Production callers | Verdict |
|---|---|---|---|
| `atomic_file.rs:17`, `:30` | `MoveFileExW(REPLACE_EXISTING \| WRITE_THROUGH)` | launcher `:599`, `:949`; lifecycle `:2216` | Keep. Move to `platform_sys::fs`, as the base of the durable writer. |
| `io_cancellation.rs:10`, `:19` | `CancelIoEx` | rithmic `network.rs:75` (abort, alongside `shutdown`) | Likely redundant: reads already poll at 100 ms and `shutdown(Both)` unblocks them. Delete after an abort-latency test proves the latency stays within about 100 ms. Otherwise move to `platform_sys`. |
| `session_shutdown.rs:361` allow; blocks `:385-612` | Message-only window, `RegisterClassW`, `GetMessageW`, window proc, `WM_QUERYENDSESSION` | `NativeSessionShutdownMonitor`: no callers. `NativeSessionShutdownGuard`: readiness path only (`desktop.rs:353-368`, `:4041`) | GPUI already handles `WM_QUERYENDSESSION`/`WM_ENDSESSION` and routes them to quit (`gpui_windows events.rs:112-113`). Delete the Monitor now. Delete the Guard after verifying that the GPUI quit path flushes the workspace and trading store on logoff (manual logoff test). The readiness process is killed by the launcher on timeout anyway. |
| `power_notifications.rs:135` (Windows `:171-222`), `:252` (macOS IOKit `:269-484`) | `PowerRegisterSuspendResumeNotification`; IOKit | `market_runtime` `rithmic_realtime.rs:1649-1653` | Keep. Move the FFI to `platform_sys`. The safe `PowerEvent` stream stays in `platform_runtime`. |
| `network_notifications.rs:191` (macOS `:226-482`), `:505` (Windows `:542-583`) | `SCNetworkReachability`; `NotifyIpInterfaceChange`/`CancelMibChangeNotify2` | `rithmic_realtime.rs:1649-1653` | Keep. Move the FFI to `platform_sys`. |
| `display_timing.rs:407-456` | QPC, `DwmGetCompositionTimingInfo`, `DwmFlush` | Only stale pacing tools | Delete the module (877 lines). Also drop the `Win32_Graphics_Dwm` and `Win32_System_Performance` windows-sys features. |

After F08 the target is zero unsafe code outside `crates/platform_sys`.

## A4. Trim table (keep the pipeline, cut the weight)

| Item | Evidence | Action | Size | Risk |
|---|---|---|---|---|
| `display_timing.rs` | Only stale tools use it | Delete | -877 | Low |
| `session_shutdown.rs` | Monitor unused; Guard only in readiness; GPUI handles end-session | Delete Monitor now; delete Guard after the logoff test | -682 | Medium (verify flush) |
| `io_cancellation.rs` | Duplicate of shutdown plus poll | Delete after the latency test | -25 | Low |
| `live_market_gate.rs` | Evidence recorder that copies the executable binary; used by a test (`hyperliquid_realtime.rs:1956`) and the `rithmic_test_smoke` bin | Move out of the production library into a dev/test-support crate or the smoke bin; update naming_check `:2276`, `:2286` | -285 from production | Low |
| Lifecycle in `platform_runtime` | ed25519, semver, sysinfo, and same-file are only used by lifecycle, launcher, and release_delivery, but every adapter depends on `platform_runtime` | Split into a `crates/release_lifecycle` crate (F06) | Move only | Low |
| `sign_release_manifest`, `sign_block_plan` in production | Test-only callers | Move behind a `producer` feature used by `release_packager` and tests | Small | Low |
| PowerShell launch-error popup, `.bat` relocation, sysinfo polling | LC-13/14/15 | Replace with `platform_sys` calls | Net negative | Low |
| Desktop `reqwest_client` | Avatar only | Fetch avatar bytes with ureq (AC-07). Expect reqwest and hyper to leave the lock; tokio/quinn only if nothing else needs them (verify with `cargo tree -i`) | Large deps reduction | Medium (GPUI image path) |
| Duplicate readiness schema | LC-11 | One shared type | -30 | Low |
| Axiusflow residue | Wire tokens, Inno lines, vault legacy probes, `AXIUSFLOW_AUTH_ISSUER` tests (`account_runtime tests.rs:31-41`), legacy data-root migration | Remove (F18/F19) | Small | Low once migrated |
| Dormant-code text pins in naming_check | `tools/naming_check/src/main.rs` tests around `:1658`, `:1703`, `:1747`, `:1788`, `:1857`, `:1887`, `:1924`, `:2311`, `:2427` | Re-evaluate each when the pinned code moves. Keep real invariants and replace pins on implementation text with behavior tests in the owning crate | Medium | Medium (they are authoritative; change deliberately) |
| Workspace and chart-chrome `.bak` dance | `local_state.rs:200-209`, `chart_chrome.rs:790-809` | Replace with the durable writer, which never removes the live file | -60 | Low |
| `plan/` directory | AGENTS.md forbids planning documents | Not inspected in this pass. Check its contents before deleting | ? | ? |

## A5. Account runtime findings

| ID | Sev | Finding (evidence) | Fix |
|---|---|---|---|
| AC-01 | High | `VerifiedTokens` derives `Clone + Debug` and holds plain `String` access, id, and refresh tokens (`oidc.rs:225-235`). Any `{:?}` leaks them into logs, which matters once the F03 file sink exists. | Wrap the tokens in `Zeroizing<String>` (zeroize is already used, `mod.rs:2713`) behind a `SecretToken` type with a redacted `Debug` and no `Display`. Remove `Clone`. Must land before or with F03. |
| AC-02 | Medium | `MAXIMUM_PROFILE_VAULT_BYTES = 4096` (`mod.rs:61`) exceeds the Windows `CRED_MAX_CREDENTIAL_BLOB_SIZE` of 2560 bytes. The vault has no size precheck, so large profiles fail opaquely at `CredWriteW`. The lease limit is 2048 (`lease.rs:23`), which is fine. | Add `credential_vault::MAX_SECRET_BYTES` (2560 on Windows). `store()` rejects larger values with an explicit error. Const-assert that the account limits are within it. |
| AC-03 | Medium | Vault I/O runs while holding the account state mutex (`mod.rs:840-848`, `:2042-2053`, `:2121-2140`, `:2413-2424`). Credential Manager calls can stall and block every state reader. | Compute the change under the lock, release it, do the vault I/O, then re-acquire and apply with the existing generation fence. |
| AC-04 | High (when enabled) | The OIDC issuer comes from the runtime environment variable `AERIS_AUTH_ISSUER` (`mod.rs:117-134`, default `""` at `:51`). Any parent process or user environment can redirect sign-in to an attacker's issuer. | Read the issuer at build time (`option_env!`) through `release_config` (LC-20). Allow a runtime override only under `cfg(debug_assertions)`. Delete the `AXIUSFLOW_AUTH_ISSUER` tests. |
| AC-05 | High | `retain_account_refresh_quiesce_for_exit` parks the thread forever on failure, after up to two 20 s waits (`desktop.rs:302-342`, `WAIT_TIMEOUT` at `mod.rs:197`). It is used on readiness and every exit path. The result is an invisible zombie process that still holds the trading DB, defeats probation (LC-09), and would block a single-instance lock (F05). | Bounded: wait up to the timeout, log, then `std::process::exit(code)`. Never park. This is a prerequisite for F05. |
| AC-06 | Medium | The global `NATIVE_CREDENTIAL_ACCESS` mutex in `credential_vault.rs:9` turns poisoning into a permanent `AccessSerialized` error. | The guarded value is `()`, so recover with `lock().unwrap_or_else(PoisonError::into_inner)`. Add a test that poisons the lock. |
| AC-07 | Medium | GPUI `reqwest_client` is installed only for the avatar image (`desktop.rs:295-300`, `platform_menu.rs:135-136`, tested by `http_wiring_tests.rs`). | `account_runtime` fetches avatar bytes with ureq (size bound, timeout, https only, on a background thread) and the desktop renders them from bytes. Remove `reqwest_client` and update `http_wiring_tests.rs`. |
| AC-08 | Info | Auth is disabled per AGENTS.md. I did not verify that refresh and lease threads stay idle when the issuer is empty. | Add a test: with an empty issuer, account runtime start spawns no network work. |

---

# Part B - Foundations design

## B1. Logging and panic capture

Problem: in installed builds, every `diagnostic!` line goes to a discarded stderr. There is no panic
hook. Startup failures in `run()` (`desktop.rs:5439-5504`) exit with code 1 and show nothing.

Design (no new crates; `tracing` is in the lock only transitively):

- Placement: `crates/observability` stays a leaf crate. The desktop and launcher pass the log directory in.
- API:
  ```rust
  pub struct LogSinkConfig { pub directory: PathBuf, pub file_stem: &'static str,
                             pub max_file_bytes: u64, pub max_files: u8 }
  pub fn install_log_sink(config: LogSinkConfig) -> io::Result<LogSinkGuard>; // flushes on drop
  pub fn install_panic_hook();      // chains the previous hook
  ```
- `write_diagnostic` keeps writing to stderr, and also sends `(SystemTime, thread name, line)` into a
  `sync_channel(1024)` read by a writer thread. When the channel is full, it increments an `AtomicU64`
  dropped counter, and the next written line reports the count. This keeps the UI thread free of disk I/O
  (AGENTS.md invariant).
- Rotation: `aeris.log` reaches `max_file_bytes` (4 MiB), then rolls to `aeris.1.log` ... up to `max_files` (5),
  so the worst case is 20 MiB.
- Panic hook: synchronously writes `crash-<unix_ms>.log` (message, location, thread,
  `std::backtrace::Backtrace::force_capture()`, release identity and generation) with `StagedFile`,
  because the writer thread may be the one panicking. Then it calls the previous hook.
- Location: `native_data_root()/logs` for the desktop, `<install_root>/logs/launcher.log` for the launcher.
- Fatal startup: after logging, the release desktop shows a native error dialog (the existing
  `native_ui/dialog.rs` path, or `MessageBoxW` before GPUI is up) that names the log path.
- Redaction: AC-01 must land first. Add a naming_check rule: no `{:?}` of types whose names contain `Token` or `Secret`.
- Verification: unit tests for rotation and overflow. A test re-spawns the test binary with an env
  flag that panics, then asserts the crash file exists. Manual check: a release build started from
  Explorer writes `logs/aeris.log`.

## B2. Single instance

- API (`platform_runtime`): `InstanceLock::acquire(root: &Path) -> Result<InstanceLock, InstanceLockError>`
  using std `File::try_lock` (already used at `lifecycle.rs:2308`) on `<data root>/instance.lock`. Errors
  are `HeldByAnotherProcess` and `Io(io::Error)`.
- Placement: in `desktop::run()`, immediately after the `--desktop-readiness` branch
  (`desktop.rs:5424-5430`) and before `migrate_legacy_native_data_root` and `start_trading_service`.
  `DesktopLifecycle` holds the lock, and it is released only after the trading and context stores shut down.
- Readiness: it also acquires the lock, and fails readiness if the lock is held. Combined with B4, this
  means it can never race a running desktop.
- Second instance: native dialog "Aeris is already running", then exit 0. Focusing the existing window
  through a named pipe is deferred, because it needs IPC.
- Hard dependency: AC-05 first. Otherwise a parked zombie holds the lock forever.
- Verification: a test acquires, a second acquire fails, and after drop it succeeds. Manual check: double launch.

## B3. Durable file writer

There are currently seven hand-rolled writers with different guarantees: `local_state.rs:175-213`,
`chart_chrome.rs:765-815`, `trading_runtime store.rs:3373-3383` (no fsync; removes the live file before
renaming), `live_market_gate.rs:263-266`, `desktop.rs:4095` and `readiness_conformance.rs:756` (plain
`fs::write`), `lifecycle.rs:2179-2216`, launcher `:300-323`, `:596-599`, `:946-949`, `:1420-1425`, and
`account_service mod.rs:337-348` (create_new marker, which is correct as is).

- Placement: `platform_sys::fs`. This is a leaf crate, so `trading_runtime`, which does not depend on
  `platform_runtime`, can use it without inverting dependency direction.
- API:
  ```rust
  pub struct StagedFile { /* temp path, File, destination */ }
  impl StagedFile {
      pub fn create(destination: &Path) -> io::Result<Self>; // ".{name}.{pid}-{seq}.tmp", create_new, same dir
      pub fn commit(self) -> io::Result<()>;                // sync_all + replace + (unix) parent fsync
  }                                                          // Drop without commit removes the temp file
  impl Write for StagedFile { .. }
  pub fn write_file_durably(destination: &Path, bytes: &[u8]) -> io::Result<()>;
  pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()>; // MoveFileExW WRITE_THROUGH
  pub fn remove_stale_temporaries(directory: &Path, file_name: &str) -> io::Result<()>;
  ```
- The live file is never removed or renamed away, so the `.bak` dance and its load-time fallback
  (`local_state.rs:117`) become unnecessary. Old `.bak` files are harmless because the main file
  always exists.
- naming_check rule: production code in `apps/desktop`, `trading_runtime`, `account_runtime`, `platform_runtime`,
  and `release_lifecycle` must not call `fs::write(` or `fs::rename(`, except through `platform_sys::fs`
  and an explicit allowlist (directory moves in lifecycle).
- Verification: unit tests for commit, drop cleanup, replacing an existing file, and a pre-existing stale temp.
  A crash test: a child process is killed between write and commit, and the destination must be the old
  content and never empty.

## B4. SQLite durability (`trading_runtime/src/store.rs`)

Current state: open (`:357-380`) sets WAL, `synchronous=FULL`, and `foreign_keys`. There is no
`busy_timeout` and no integrity check. Migration (`:382-482`) is a chain of `if`s up to `SCHEMA_VERSION` 19,
with a transaction per step in `apply_migration` (`:484`). A newer version is a hard error (`:387`).

- `busy_timeout = 5000` at open. This is defense in depth even with the single-instance lock.
- `PRAGMA quick_check` at open, off the UI thread (open already happens before GPUI). Measure its cost on
  a realistic journal size before claiming it is acceptable. On failure, return `StoreError::Corrupt`.
  The desktop shows the error dialog with the path and the newest backup. Never silently recreate
  the user's journal. Optional quarantine: rename db, wal, and shm to `trading.corrupt-<ts>.*` only after
  the user confirms.
- Pre-migration backup: when `user_version < SCHEMA_VERSION`, run `VACUUM INTO
  '<data>/backups/trading-v{old}-{unix}.sqlite'` before the first step. Keep the newest 3.
- Migration table: `const MIGRATIONS: &[Migration { from: u32, apply: fn(&Transaction) -> Result<()> }]`.
  Each step sets `user_version = from + 1` inside its own transaction. A unit test asserts the steps are
  contiguous from the oldest supported version to `SCHEMA_VERSION`, and another migrates every historical
  fixture to head.
- Export `pub const SCHEMA_VERSION` for release tooling. The manifest records `max_store_schema` (B9/F14).
- On clean shutdown, run `PRAGMA wal_checkpoint(TRUNCATE)` so backups and copies are self-contained.
- Readiness (LC-05): `VACUUM INTO` a temp copy, open the copy with migrations, report, then delete it.
  The real database is never written by a candidate build.

## B5. CI

Constraints: zero-cost policy (self-hosted runners only, labels `[self-hosted, axiusflow, linux|windows]`).
`ci.yml` is `workflow_dispatch` only. naming_check `workflows_never_run_fork_pull_requests` (`:2211`)
forbids `pull_request` triggers, which is correct for self-hosted runners.

- Triggers: `push: branches: [main]` plus `workflow_dispatch`. Only the maintainer can push to main, so
  no fork code reaches the runner. Add `concurrency: { group: ci-main, cancel-in-progress: true }` and
  `permissions: contents: read`. Use no secrets.
- Jobs: a Windows self-hosted job is required (the product is Windows-first): `cargo fmt --check`, clippy with
  `-D warnings`, `cargo test --workspace --all-features --locked`, and naming_check, each with a step timeout. Keep Linux
  as a second job. Perf and evidence scripts stay under `workflow_dispatch` only.
- Local gate: `tools/git_hooks/pre-push` (PowerShell) runs fmt, clippy, and naming_check. It is enabled with
  `git config core.hooksPath tools/git_hooks`, documented with one line in `Readme`.
- naming_check: update the CI assertions (`:2011`, `:2096`, `:2137`) to require the push trigger, concurrency,
  and read-only permissions, while still forbidding `pull_request` and `pull_request_target`.
- Maintainer-only action: re-label the runners from `axiusflow` to `aeris`, then change the labels in the same commit.

## B6. Unsafe isolation

- New leaf crate `crates/platform_sys` with dependencies `windows-sys` (with the features moved from
  `platform_runtime`) and the macOS frameworks currently used. It exposes safe APIs only:
  `fs::{StagedFile, replace_file}`, `power::SuspendResumeRegistration`,
  `network::InterfaceChangeRegistration`, `process::{wait_for_exit, is_running}`, `ui::message_box`,
  `trust::verify_authenticode(path, thumbprint)`, and `net::cancel_socket_io` only if F08 keeps it.
- Lints: `[lints.rust] unsafe_code = "deny"`, with `#![allow(unsafe_code)]` only in `src/windows/*.rs`
  and `src/macos/*.rs`. Clippy `undocumented_unsafe_blocks = "deny"` and
  `multiple_unsafe_ops_per_block = "deny"` for this crate, and each block has a `// SAFETY:` comment.
- `platform_runtime` goes back to the workspace `forbid`.
- naming_check: the `unsafe` keyword and `allow(unsafe_code)` may appear only under `crates/platform_sys/src`.

## B7. naming_check

- Keep it authoritative (AGENTS.md). Change it deliberately, one rule at a time, in the same commit as the
  code it guards.
- New rules: unsafe location (B6), durable writes (B3), no `AXIUSFLOW` in production code (LC-04), a
  dependency-direction check over `Cargo.toml` (B8), release-config completeness (LC-20), no secret `Debug`
  (B1), and CI shape (B5).
- Pins on dormant-code text (A4 list) are re-evaluated when F06 moves the code. A pin that asserts an
  invariant (fencing, rollout determinism, uninstall relocation) moves into a behavior test in
  `release_lifecycle` or `account_runtime`. A pin that only asserts source text is deleted with
  maintainer sign-off.
- Keep the gpui_base retained-module pin (`:2703`).

## B8. Dependency direction (target)

```
platform_sys (leaf, only crate with unsafe)      observability (leaf)
      ^                ^                                ^
      |                +---- trading_runtime (durable export writes)
platform_runtime (vault, sockets, http, dns, browser, notifications, power/network events,
      ^           instance lock, data roots)  forbid(unsafe)
      |  ^-- adapters, market_runtime, context_runtime, account_runtime
release_lifecycle (lifecycle, release_delivery, rollout, release_identity, release_config,
      ^            ReadinessReport, bin aeris_launcher; ed25519/semver/same-file live only here)
      |  ^-- tools/release_packager (feature "producer")
apps/desktop (update.rs, readiness command)
```

The naming_check rule is that only `apps/desktop` and `tools/release_packager` may depend on
`release_lifecycle`, and only `platform_runtime`, `trading_runtime`, and `release_lifecycle` may depend
on `platform_sys`.

## B9. Release producer and end-to-end proof (kept dormant, made testable)

- `tools/release_packager` (binary) takes the built payload directory, the generation, `max_store_schema`
  (from `trading_runtime::SCHEMA_VERSION`), and the signing key (from a file path, never env or CLI text).
  It emits the signed manifest (LC-03 format), block plan, channel pointer, rollback-compatibility data,
  and the Inno invocation. It does not publish anything (AGENTS.md: publishing is disabled).
- An end-to-end test in `release_lifecycle` uses a `DirectorySource` that implements the `ReleaseSource`
  trait, plus a throwaway signing key. It covers install, update, readiness on the DB copy, activation,
  probation marker, forced crash, rollback, and quarantine, and runs in CI on Windows.

---

# Ordered feature breakdown

Each item is one worker commit. Size: S = under 300 changed lines, M = 300 to 800, L = more than 800.
Dependencies refer to earlier IDs.

| # | Feature | Contents | Deps | Size | Verification |
|---|---|---|---|---|---|
| F00 | CI on push to main | B5 workflow, pre-push hook, naming_check CI assertions | - | S | naming_check tests; a dispatch run on the self-hosted runner |
| F01 | `platform_sys` crate + durable writer | Crate scaffold and lints; move `MoveFileExW`; `StagedFile`/`write_file_durably`; unsafe-location rule (allowlisting existing platform_runtime blocks until F08) | - | M | Unit and crash tests (B3) |
| F02 | Adopt durable writer | local_state, chart_chrome, trading export, readiness report (`desktop.rs:4095`, `readiness_conformance.rs:756`), live_market_gate, lifecycle `write_json_atomic`, launcher writes and download commits (LC-10); durable-write rule | F01 | M | Existing persistence tests plus a no-`.bak` test |
| F03 | Log sink + panic hook + fatal dialog | B1; install in desktop `run()` and the launcher; secret-Debug rule | F04 (AC-01 must land first; do F04 before F03) | M | Rotation, overflow, and child-panic tests; manual release launch |
| F04 | Account safety batch 1 | AC-01 (SecretToken), AC-05 (bounded exit), AC-06 (poison recovery) | - | S | Unit tests; a test that the exit path returns within the bound |
| F05 | Single instance | B2 | F03, F04 | S | Lock tests; manual double launch |
| F06 | Split `release_lifecycle` crate | Pure move of lifecycle, release_delivery, rollout, release_identity, and the launcher bin; `producer` feature for the signers; dependency-direction rule | F01 | L (move) | Workspace build and test; `cargo tree` shows ed25519/semver gone from adapters |
| F07 | Launcher hygiene | LC-01 command-first dispatch, LC-16 installed-only promotion, LC-13 retired-binary sweep, LC-14 MessageBoxW, LC-15 process wait and sysinfo removal, `release_config` (LC-20) | F01, F06 | M | Per-command no-key tests; `cargo tree -i sysinfo` |
| F08 | FFI consolidation + trims | Move power and network FFI to `platform_sys`; delete display_timing; delete the session Monitor; Guard and CancelIoEx removal gated on tests; `platform_runtime` forbids unsafe | F01 | L | Rithmic abort-latency test; manual logoff flush test; unsafe-location rule fully enforced |
| F09 | Account safety batch 2 | AC-02 vault size cap, AC-03 I/O outside the lock, AC-04 build-time issuer, AC-08 idle test | F07 (release_config) | M | account_runtime tests |
| F10 | SQLite hardening | B4: busy_timeout, quick_check with error dialog, migration table, `VACUUM INTO` pre-migration backup, exported SCHEMA_VERSION, checkpoint on shutdown | F02 | M | Fixture migration tests from each historical version; corrupt-file test; measured quick_check time |
| F11 | Readiness contract | Shared `ReadinessReport`; readiness runs on a DB copy under the instance lock; durable report (LC-05 part, LC-11) | F05, F06, F10 | M | Round-trip contract test; test that the real DB `user_version` is unchanged after readiness |
| F12 | Wire protocol v1 | LC-03 raw-bytes domain-separated signature + `deny_unknown_fields`; LC-04 Aeris tokens; LC-19 protocol version; LC-08 typed block errors | F06, F03 | M | Signature vectors; tamper and unknown-field tests |
| F13 | Release producer + end-to-end harness | B9 `tools/release_packager`, `ReleaseSource` seam, DirectorySource, full-cycle test (LC-02, LC-21) | F12, F11 | L | The end-to-end test in CI on Windows |
| F14 | Rollback safety | Manifest `max_store_schema` gate (LC-05), startup-complete marker probation (LC-09), deterministic Activated recovery and orphan tolerance (LC-18), preflight-before-READY (LC-06) | F13, F10 | M | End-to-end crash and hang rollback cases; schema-gate test |
| F15 | Authenticode via WinVerifyTrust | LC-07 in `platform_sys::trust`; build fails if a release config lacks the thumbprint | F01, F07 | S | Signed and unsigned fixture tests on Windows |
| F16 | Drop `reqwest_client` | AC-07: ureq avatar fetch; update `http_wiring_tests.rs` | - | S | `cargo tree -i reqwest` is empty; visual avatar check once auth is live |
| F17 | Inno uninstall safety | LC-12 keep-data page, Axiusflow cruft removal | F07 | S | Manual install and uninstall on a VM; data kept by default |
| F18 | Axiusflow residue sweep | `AXIUSFLOW_AUTH_ISSUER` tests, remaining tokens, no-AXIUSFLOW rule | F12, F17 | S | naming_check |
| F19 | Legacy migration removal | Delete `migrate_legacy_native_data_root` and the vault legacy probes (LC-17) | Maintainer confirms migration | S | Workspace tests |
| F20 | naming_check consolidation | B7: re-home or delete dormant-code pins after F06/F08/F12; move live_market_gate to test support | F06, F08, F12 | M | naming_check and workspace tests |

Critical path to a releasable pipeline: F01 -> F02 -> F06 -> F10 -> F11 -> F12 -> F13 -> F14 (+F15, F17).
Can run in parallel early: F00, F03/F04/F05, F08, F09, F16.

## Open items requiring the maintainer or manual verification

- Runner re-label (`axiusflow` to `aeris`) and whether the Linux self-hosted runner stays.
- Logoff flush test on a real Windows session before deleting `NativeSessionShutdownGuard`.
- Confirm that the Axiusflow-era data on the maintainer's machine has been migrated before F19.
- Contents of `plan/` were not inspected in this pass.
- The quick_check cost, the handshake timing (LC-06), and the CancelIoEx redundancy are hypotheses until measured.
- Code-signing certificate, release signing key custody, and the AWS release endpoint are out of scope and remain dormant per AGENTS.md.
