# Aeris Terminal

Aeris Terminal is a local-first desktop trading terminal built with Rust and GPUI. It brings charts,
order books, market data, practice trading and multi-pane workspaces into one native application
process. Version 1.0.0 ships for Windows x64 and macOS Apple silicon from
<https://aeristerminal.com/download/>.

## Development

Requirements: the Rust toolchain in `rust-toolchain.toml` and the platform dependencies for GPUI.
On Linux, run `tools/setup_linux_desktop.sh`.

```text
cargo run --release -p aeris_desktop -- --workspace-tabs
```

The desktop requires an Aeris account sign-in before it starts market data, provider sessions or
the workspace. Provider data also needs the provider's own credentials, stored in the native vault.

### Validation mode

Pass `--validation-root <absolute directory>` to run against an isolated data folder. A validation
run skips sign-in, takes an exclusive per-root instance lock, and never touches the production data
folder or vault entries, migrates legacy state, writes installed-app identity or runs updates.
Delete the folder when the run is finished.

## Project layout

- `apps/desktop`: the native application and its composition root
- `crates/market_runtime`, `crates/trading_runtime`, `crates/account_runtime`,
  `crates/context_runtime`: in-process runtime owners
- `crates/market_engine`, `crates/provider_history`, `crates/adapters`: market demand, history
  and provider adapters
- `crates/domain`, `crates/contracts`: shared domain types and the persisted workspace schema
- `crates/platform_runtime`, `crates/platform_sys`, `crates/observability`: platform services;
  `platform_sys` holds the target-specific FFI and is the only Aeris crate allowed `unsafe`
- `crates/release_lifecycle`: the signed release launcher and updater, kept for future
  integration; releases currently ship as installers from the website
- `crates/ui`: design system and chart integration
- `third_party/gpui_base`: the pinned GPUI input and control base; only its two macOS
  Objective-C bridges may use `unsafe`
- `tools`: the architecture gate (`naming_check`), probes, the release packager and setup scripts

Aeris Charts is a pinned Git dependency and is fetched by Cargo.

## Checks

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run -p aeris_naming_check --locked
```

CI runs these on GitHub-hosted runners with `AERIS_RITHMIC_KIT_DISABLED=1`: the licensed Rithmic
proto kit is not in the repository, so kit-gated code compiles and runs only on a machine that has it.

See `AGENTS.md` for contributor and architecture rules. This project is proprietary; see `LICENSE`.
