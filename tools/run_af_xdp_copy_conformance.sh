#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="axiusflow-af-xdp-conformance:ubuntu-26.04"
evidence_dir="$repo_root/.cache/evidence"
sdk_lib="$repo_root/.cache/linux-dev-root/usr/lib/x86_64-linux-gnu"

if [[ ! -d "$sdk_lib" ]]; then
    echo "run tools/setup_linux_desktop.sh --user-acceleration first" >&2
    exit 1
fi

mkdir -p "$evidence_dir"

# Some sandboxes mount the default `~/.docker` read-only, which fails buildx bookkeeping.
# Keep client state in a writable scratch directory and skip the rebuild when the pinned
# image is already present.
if [[ -z "${DOCKER_CONFIG:-}" && ! -w "${HOME}/.docker" ]]; then
    DOCKER_CONFIG="$(mktemp -d)"
    export DOCKER_CONFIG
fi

if [[ -n "${AF_XDP_FORCE_IMAGE_BUILD:-}" ]] || ! docker image inspect "$image" >/dev/null 2>&1; then
    docker build --tag "$image" "$repo_root/tools/af_xdp_conformance_container"
fi

docker run --rm \
    --network none \
    --sysctl net.ipv6.conf.all.disable_ipv6=1 \
    --sysctl net.ipv6.conf.default.disable_ipv6=1 \
    --read-only \
    --tmpfs /run:rw,nosuid,nodev,noexec,size=8m \
    --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
    --cap-drop ALL \
    --cap-add BPF \
    --cap-add IPC_LOCK \
    --cap-add NET_ADMIN \
    --cap-add NET_RAW \
    --cap-add SYS_ADMIN \
    --cap-add SYS_RESOURCE \
    --security-opt no-new-privileges \
    --ulimit memlock=-1:-1 \
    --env "GITHUB_SHA=$(git -C "$repo_root" rev-parse HEAD)" \
    --env LIBXDP_BPFFS=/run/xdp-bpffs \
    --env LIBXDP_BPFFS_AUTOMOUNT=1 \
    --env LD_LIBRARY_PATH=/workspace/.cache/linux-dev-root/usr/lib/x86_64-linux-gnu \
    --mount "type=bind,src=$repo_root,dst=/workspace,readonly" \
    --mount "type=bind,src=$evidence_dir,dst=/evidence" \
    "$image"
