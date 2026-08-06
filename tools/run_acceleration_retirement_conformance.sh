#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mkdir -p .cache
cargo metadata --locked --no-deps --format-version 1 > .cache/acceleration-retirement-metadata.json

python3 - <<'PY'
import json
from pathlib import Path

metadata = json.loads(Path(".cache/acceleration-retirement-metadata.json").read_text())
members = {member.rsplit("#", 1)[-1].split("@", 1)[0] for member in metadata["workspace_members"]}
retired_packages = {
    "axiusflow_linux_af_xdp_adapter",
    "axiusflow_linux_dpdk_adapter",
    "axiusflow_market_data_plane",
}
assert members.isdisjoint(retired_packages), (
    f"retired or quarantined workspace members found: {members & retired_packages}"
)

retired_paths = (
    "config/libxdp_patch_provenance.json",
    "crates/adapters/linux_af_xdp",
    "crates/adapters/linux_dpdk",
    "fuzz",
    "third_party/libxdp-sys",
    "tools/af_xdp_conformance_container",
    "tools/dpdk_conformance_container",
    "tools/run_af_xdp_copy_conformance.sh",
    "tools/run_af_xdp_copy_fuzz.sh",
    "tools/run_af_xdp_safe_boundary_fuzz.sh",
    "tools/run_dpdk_vdev_lifecycle.sh",
    "tools/verify_libxdp_patch.py",
)
present = [path for path in retired_paths if Path(path).exists()]
assert not present, f"retired acceleration paths remain: {present}"

manifest = json.loads(Path("config/ingest_readiness.json").read_text())
profiles = {profile["profile"] for profile in manifest["profiles"]}
assert profiles == {"portable_socket", "tuned_linux_socket"}, profiles

setup = Path("tools/setup_linux_desktop.sh").read_text()
assert "acceleration" not in setup.lower()

workflow = Path(".github/workflows/stage_1.yml").read_text()
for forbidden in (
    "axiusflow_linux_af_xdp_adapter",
    "axiusflow_linux_dpdk_adapter",
    "linux_af_xdp_generic_copy_veth_conformance:",
    "linux_dpdk_virtual_device_eal_lifecycle:",
):
    assert forbidden not in workflow, forbidden
PY

cargo test --locked --package axiusflow_transport retired_acceleration_profiles_are_not_advertised
echo "acceleration_retirement_conformance=passed"
