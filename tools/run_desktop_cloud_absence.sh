#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

evidence_dir=".cache/evidence"
metadata_path="$evidence_dir/desktop-cloud-absence-metadata.json"
report_path="$evidence_dir/stage_2_desktop_cloud_absence_evidence.json"
mkdir -p "$evidence_dir"

cargo metadata --locked --format-version 1 > "$metadata_path"
cargo build --locked --release --package axiusflow_desktop

python3 - "$metadata_path" "$report_path" <<'PY'
import json
import hashlib
import subprocess
import sys
from pathlib import Path

metadata_path = Path(sys.argv[1])
report_path = Path(sys.argv[2])
metadata = json.loads(metadata_path.read_text(encoding="utf-8"))

packages = {package["id"]: package for package in metadata["packages"]}
desktop_ids = [
    package_id
    for package_id, package in packages.items()
    if package["name"] == "axiusflow_desktop"
]
assert len(desktop_ids) == 1, desktop_ids

nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
pending = desktop_ids.copy()
closure = set()
while pending:
    package_id = pending.pop()
    if package_id in closure:
        continue
    closure.add(package_id)
    pending.extend(
        dependency["pkg"]
        for dependency in nodes[package_id]["deps"]
        if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"])
    )

closure_names = sorted({packages[package_id]["name"] for package_id in closure})
forbidden_packages = {
    "axiusflow_market_data_plane",
    "axiusflow_streaming",
    "rdkafka",
    "rdkafka-sys",
    "tungstenite",
}
present_forbidden_packages = sorted(forbidden_packages.intersection(closure_names))
assert not present_forbidden_packages, present_forbidden_packages

forbidden_literals = {
    "AXIUSFLOW_LIVE_ENDPOINT",
    "AXIUSFLOW_AUTH_TOKEN",
    "?token=",
    "axiusflow_market_data_plane",
    "axiusflow_streaming",
    "tungstenite",
}
desktop_root = Path("apps/desktop")
shipping_files = [desktop_root / "Cargo.toml", *sorted((desktop_root / "src").rglob("*.rs"))]
source_hits = []
for source_path in shipping_files:
    source = source_path.read_text(encoding="utf-8")
    for literal in forbidden_literals:
        if literal in source:
            source_hits.append(f"{source_path}:{literal}")
assert not source_hits, source_hits

binary_path = Path(metadata["target_directory"]) / "release" / "axiusflow_desktop"
if sys.platform == "win32":
    binary_path = binary_path.with_suffix(".exe")
assert binary_path.is_file(), binary_path
binary_strings = subprocess.run(
    ["strings", str(binary_path)],
    check=True,
    capture_output=True,
    text=True,
).stdout
binary_hits = sorted(literal for literal in forbidden_literals if literal in binary_strings)
assert not binary_hits, binary_hits

head_revision = subprocess.run(
    ["git", "rev-parse", "HEAD"],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
tracked_diff_bytes = subprocess.run(
    ["git", "diff", "--binary", "HEAD", "--", "."],
    check=True,
    capture_output=True,
).stdout
untracked_paths = subprocess.run(
    ["git", "ls-files", "--others", "--exclude-standard"],
    check=True,
    capture_output=True,
    text=True,
).stdout.splitlines()
workspace_digest = hashlib.sha256()
assert isinstance(tracked_diff_bytes, bytes)
workspace_digest.update(tracked_diff_bytes)
for untracked_path in sorted(untracked_paths):
    workspace_digest.update(untracked_path.encode("utf-8"))
    workspace_digest.update(b"\0")
    workspace_digest.update(Path(untracked_path).read_bytes())
source_tree_clean = not tracked_diff_bytes and not untracked_paths
source_revision = (
    head_revision
    if source_tree_clean
    else f"{head_revision}+workspace.{workspace_digest.hexdigest()}"
)
report = {
    "schema_version": 1,
    "evidence_scope": "stage_2_desktop_cloud_absence",
    "source_revision": source_revision,
    "source_tree_clean": source_tree_clean,
    "shipping_topology": "disconnected_fixture",
    "desktop_dependency_packages": closure_names,
    "forbidden_cloud_packages_present": present_forbidden_packages,
    "forbidden_shipping_source_literals_present": source_hits,
    "forbidden_release_binary_literals_present": binary_hits,
    "market_data_cloud_route": "absent",
    "provider_credential_cloud_route": "absent",
    "direct_provider_network_integration": "not_exercised",
    "claim_readiness": "fixture_validated",
}
report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
print(
    "desktop_cloud_absence=passed "
    f"shipping_topology={report['shipping_topology']} "
    f"dependency_packages={len(closure_names)} "
    "direct_provider_network_integration=not_exercised "
    f"report={report_path}"
)
PY
