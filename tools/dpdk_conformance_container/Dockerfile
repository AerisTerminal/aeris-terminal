FROM ubuntu:26.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        pkg-config \
        libclang-21-dev \
        libdpdk-dev=25.11-2 \
        librte-argparse26=25.11-2 \
        librte-bus-pci26=25.11-2 \
        librte-bus-platform26=25.11-2 \
        librte-bus-vdev26=25.11-2 \
        librte-eal26=25.11-2 \
        librte-ethdev26=25.11-2 \
        librte-kvargs26=25.11-2 \
        librte-log26=25.11-2 \
        librte-mbuf26=25.11-2 \
        librte-mempool-ring26=25.11-2 \
        librte-mempool26=25.11-2 \
        librte-meter26=25.11-2 \
        librte-net-ring26=25.11-2 \
        librte-net26=25.11-2 \
        librte-pci26=25.11-2 \
        librte-ring26=25.11-2 \
        librte-telemetry26=25.11-2 \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain 1.97.1
ENV PATH="/root/.cargo/bin:${PATH}"
ENV LIBCLANG_PATH="/usr/lib/llvm-21/lib"

WORKDIR /workspace

ENTRYPOINT ["/workspace/tools/dpdk_conformance_container/run.sh"]
