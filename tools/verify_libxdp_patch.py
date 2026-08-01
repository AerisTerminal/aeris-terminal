#!/usr/bin/env python3
import hashlib
import json
import sys
from pathlib import Path

PATCH_PATH = Path("xdp-tools/lib/libxdp/xsk.c")
PATCHED_BLOCK = b"""out:\n\tif (ctx->refcnt_map_fd >= 0)\n\t\tclose(ctx->refcnt_map_fd);\n\tctx->refcnt_map_fd = -ENOENT;\n\txdp_program__close(ctx->xdp_prog);\n"""
UPSTREAM_BLOCK = b"""out:\n\txdp_program__close(ctx->xdp_prog);\n"""


def tree_hash(root: Path, replacement: bytes | None = None) -> str:
    digest = hashlib.sha256()
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        relative = path.relative_to(root).as_posix().encode()
        data = (
            replacement
            if replacement is not None and path.relative_to(root) == PATCH_PATH
            else path.read_bytes()
        )
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(data).to_bytes(8, "big"))
        digest.update(data)
    return digest.hexdigest()


def fail(message: str) -> None:
    print(f"libxdp patch verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    repository = Path(__file__).resolve().parents[1]
    source = repository / "third_party/libxdp-sys"
    manifest_path = repository / "config/libxdp_patch_provenance.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    patch_file = source / PATCH_PATH
    patched_source = patch_file.read_bytes()
    if patched_source.count(PATCHED_BLOCK) != 1:
        fail("normal-release cleanup block is missing or duplicated")
    upstream_source = patched_source.replace(PATCHED_BLOCK, UPSTREAM_BLOCK, 1)
    if PATCHED_BLOCK in upstream_source:
        fail("cleanup patch remains after reconstructing upstream source")

    checks = {
        "patched_tree_sha256": tree_hash(source),
        "reconstructed_upstream_tree_sha256": tree_hash(source, upstream_source),
        "patched_xsk_sha256": hashlib.sha256(patched_source).hexdigest(),
        "upstream_xsk_sha256": hashlib.sha256(upstream_source).hexdigest(),
    }
    for field, actual in checks.items():
        expected = manifest.get(field)
        if actual != expected:
            fail(f"{field} expected {expected}, got {actual}")

    print(
        "libxdp_patch_provenance=verified "
        f"crate={manifest['crate']} version={manifest['version']} "
        f"registry_checksum={manifest['registry_checksum']}"
    )


if __name__ == "__main__":
    main()
