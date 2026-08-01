#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop_packages=(libxcb-xkb1 libxkbcommon0 libxkbcommon-x11-0 libxkbcommon-dev libxkbcommon-x11-dev)
acceleration_packages=(clang-21 libclang-common-21-dev libelf1t64 libelf-dev libzstd1 libzstd-dev m4 zlib1g zlib1g-dev)

case "${1:---system}" in
    --system)
        sudo apt-get update
        sudo apt-get install --yes libxkbcommon-dev libxkbcommon-x11-dev
        ;;
    --system-acceleration)
        sudo apt-get update
        sudo apt-get install --yes clang llvm make gcc m4 pkg-config libelf-dev zlib1g-dev iproute2 python3 libxkbcommon-dev libxkbcommon-x11-dev
        ;;
    --user)
        download_dir="$repo_root/.cache/linux-dev-packages"
        sdk_root="$repo_root/.cache/linux-dev-root"
        multiarch="$(dpkg-architecture -qDEB_HOST_MULTIARCH)"
        mkdir -p "$download_dir" "$sdk_root"
        (
            cd "$download_dir"
            apt-get download "${desktop_packages[@]}"
        )
        for package in "$download_dir"/*.deb; do
            dpkg-deb --extract "$package" "$sdk_root"
        done
        cat > "$repo_root/.cache/linux-dev-env.sh" <<EOF
export RUSTFLAGS="\${RUSTFLAGS:-} -L native=$sdk_root/usr/lib/$multiarch"
export LIBRARY_PATH="$sdk_root/usr/lib/$multiarch\${LIBRARY_PATH:+:\$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$sdk_root/usr/lib/$multiarch\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
EOF
        printf 'Run: source %q\n' "$repo_root/.cache/linux-dev-env.sh"
        ;;
    --user-acceleration)
        download_dir="$repo_root/.cache/linux-acceleration-packages"
        sdk_root="$repo_root/.cache/linux-dev-root"
        multiarch="$(dpkg-architecture -qDEB_HOST_MULTIARCH)"
        mkdir -p "$download_dir" "$sdk_root"
        (
            cd "$download_dir"
            apt-get download "${desktop_packages[@]}" "${acceleration_packages[@]}"
        )
        for package in "$download_dir"/*.deb; do
            dpkg-deb --extract "$package" "$sdk_root"
        done
        cat > "$repo_root/.cache/linux-dev-env.sh" <<EOF
export CFLAGS="\${CFLAGS:-} -I$sdk_root/usr/include"
export CLANG="$sdk_root/usr/bin/clang-21"
export PATH="$sdk_root/usr/bin:\$PATH"
export RUSTFLAGS="\${RUSTFLAGS:-} -L native=$sdk_root/usr/lib/$multiarch"
export LIBRARY_PATH="$sdk_root/usr/lib/$multiarch\${LIBRARY_PATH:+:\$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$sdk_root/usr/lib/$multiarch\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
export PKG_CONFIG_PATH="$sdk_root/usr/lib/$multiarch/pkgconfig\${PKG_CONFIG_PATH:+:\$PKG_CONFIG_PATH}"
EOF
        printf 'Run: source %q\n' "$repo_root/.cache/linux-dev-env.sh"
        ;;
    *)
        echo "usage: $0 [--system|--system-acceleration|--user|--user-acceleration]" >&2
        exit 2
        ;;
esac
