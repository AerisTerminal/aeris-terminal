# Linux development

Axiusflow's native desktop uses GPUI with both Wayland and X11 enabled. Ubuntu development
hosts need Fontconfig, FreeType, and XKBCommon development files in addition to the Rust toolchain pinned by
`rust-toolchain.toml`.

## Host setup

Install system packages when sudo is available:

```sh
tools/setup_linux_desktop.sh --system
```

For an unprivileged checkout, stage the same Ubuntu packages under the ignored `.cache`
directory and load the generated build environment:

```sh
tools/setup_linux_desktop.sh --user
source .cache/linux-dev-env.sh
```

The user-local environment must be sourced in each new shell before building or launching.

## Build and launch

```sh
cargo build --locked -p axiusflow_desktop
cargo run --locked -p axiusflow_desktop
```

The current desktop is intentionally a disconnected binary-fixture path. A healthy launch
opens a native GPUI window containing the Origin chart and keeps the event loop running without
a backend-selection panic.

## Low-latency checks

The Linux conformance path validates the active portable and tuned socket contracts:

```sh
cargo run --locked --package axiusflow_ingest_conformance
```

AF_XDP and DPDK are retired from the consumer product, default workspace, installer, readiness
manifest, and active CI. Their isolated adapter source and old harnesses remain only as historical
engineering evidence; they are not supported development or release targets.
