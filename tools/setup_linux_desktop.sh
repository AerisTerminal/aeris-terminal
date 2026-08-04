#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop_packages=(libxcb-xkb1 libfontconfig1 libfontconfig-dev libfreetype6 libfreetype-dev libxkbcommon0 libxkbcommon-x11-0 libxkbcommon-dev libxkbcommon-x11-dev)

case "${1:---system}" in
    --system)
        sudo apt-get update
        sudo apt-get install --yes libfontconfig-dev libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev
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
    *)
        echo "usage: $0 [--system|--user]" >&2
        exit 2
        ;;
esac
