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

To compile the optional AF_XDP copy-mode path, install the native acceleration toolchain:

```sh
tools/setup_linux_desktop.sh --system-acceleration
```

This mode includes the desktop packages, so it is sufficient on a fresh checkout.

Without sudo, stage Clang (including its builtin headers) and `m4` plus the `libelf`, zlib, and
zstd development files locally. The host's GCC builds vendored `libbpf`; Clang compiles libxdp's
BPF programs.

```sh
tools/setup_linux_desktop.sh --user-acceleration
source .cache/linux-dev-env.sh
cargo check --locked -p axiusflow_linux_af_xdp_adapter --all-targets --features native-copy
```

The user-local acceleration mode also includes the desktop runtime and development libraries.

## Build and launch

```sh
cargo build --locked -p axiusflow_desktop
cargo run --locked -p axiusflow_desktop
```

The current desktop is intentionally a disconnected binary-fixture path. A healthy launch
opens a native GPUI window containing the Origin chart and keeps the event loop running without
a backend-selection panic.

## Low-latency checks

The unprivileged Linux conformance path validates portable and tuned socket contracts without
claiming AF_XDP or DPDK hardware readiness:

```sh
cargo run --locked --package axiusflow_ingest_conformance
```

AF_XDP copy-mode validation additionally requires root privileges, a veth pair, BPF filesystem,
and the packages listed in `.github/workflows/stage_1.yml`. It does not establish zero-copy,
qualified-NIC, provider, or production readiness.

The local privileged harness runs in a disposable Docker network namespace. It drops all
capabilities before adding only the BPF, network, memory-lock/resource, and `SYS_ADMIN`
capabilities required by libxdp. The repository and container root are read-only; only
`.cache/evidence` and a bounded `/tmp` tmpfs are writable.

```sh
source .cache/linux-dev-env.sh
cargo build --locked --package axiusflow_ingest_conformance --features af-xdp-copy
tools/run_af_xdp_copy_conformance.sh
```

`SYS_ADMIN` is broad even inside a container. Review the script before running it. The container
uses `--network none`, creates its veth pair only inside its private namespace, and is removed on
exit.
