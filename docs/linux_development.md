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

The desktop connects to the resident local engine over authenticated local IPC. A healthy launch
starts or attaches to that engine, restores cached workspace state, and opens the native GPUI
terminal.

AF_XDP, DPDK, and the superseded socket/conformance harnesses have been deleted from `main`.
The annotated retirement tag and git history retain the historical engineering evidence; none is
a supported development or release target.
