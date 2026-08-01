#!/usr/bin/env python3
import hashlib
import json
import sys
from pathlib import Path

PATCH_PATH = Path("xdp-tools/lib/libxdp/xsk.c")
SOURCE_EXCLUSIONS = {Path("Cargo.lock")}

# Each reviewed hunk is an exact (patched, upstream) pair. Every patched block must
# appear exactly once, and replacing all of them must reconstruct the registry source
# byte for byte, so no unreviewed vendored change can pass verification.
PATCH_BLOCKS: tuple[tuple[bytes, bytes], ...] = (
    (
        b"""out:\n\tif (ctx->refcnt_map_fd >= 0)\n\t\tclose(ctx->refcnt_map_fd);\n\tctx->refcnt_map_fd = -ENOENT;\n\txdp_program__close(ctx->xdp_prog);\n""",
        b"""out:\n\txdp_program__close(ctx->xdp_prog);\n""",
    ),
    (
        b"""struct xsk_ctx {\n\tstruct xsk_ring_prod *fill;\n\tstruct xsk_ring_cons *comp;\n\tvoid *fill_map;\n\tvoid *comp_map;\n""",
        b"""struct xsk_ctx {\n\tstruct xsk_ring_prod *fill;\n\tstruct xsk_ring_cons *comp;\n""",
    ),
    (
        b"""struct xsk_socket {\n\tstruct xsk_ring_cons *rx;\n\tstruct xsk_ring_prod *tx;\n\tvoid *rx_map;\n\tvoid *tx_map;\n""",
        b"""struct xsk_socket {\n\tstruct xsk_ring_cons *rx;\n\tstruct xsk_ring_prod *tx;\n""",
    ),
    (
        b"""\tif (unmap) {\n\t\terr = xsk_get_mmap_offsets(umem->fd, &off);\n\t\tif (!err) {\n\t\t\tmunmap(ctx->fill_map, off.fr.desc +\n\t\t\t       umem->config.fill_size * sizeof(__u64));\n\t\t\tmunmap(ctx->comp_map, off.cr.desc +\n\t\t\t       umem->config.comp_size * sizeof(__u64));\n\t\t}\n\t}\n\n\tlist_del(&ctx->list);\n""",
        b"""\tif (!unmap)\n\t\tgoto out_free;\n\n\terr = xsk_get_mmap_offsets(umem->fd, &off);\n\tif (err)\n\t\tgoto out_free;\n\n\tmunmap(ctx->fill->ring - off.fr.desc, off.fr.desc + umem->config.fill_size *\n\t       sizeof(__u64));\n\tmunmap(ctx->comp->ring - off.cr.desc, off.cr.desc + umem->config.comp_size *\n\t       sizeof(__u64));\n\nout_free:\n\tlist_del(&ctx->list);\n""",
    ),
    (
        b"""\tstruct xsk_ctx *ctx;\n\tstruct xdp_mmap_offsets off;\n\tint err;\n\n\tctx = calloc(1, sizeof(*ctx));\n""",
        b"""\tstruct xsk_ctx *ctx;\n\tint err;\n\n\tctx = calloc(1, sizeof(*ctx));\n""",
    ),
    (
        b"""\terr = xsk_get_mmap_offsets(umem->fd, &off);\n\tif (err) {\n\t\tfree(ctx);\n\t\treturn NULL;\n\t}\n\n\tctx->netns_cookie = netns_cookie;\n""",
        b"""\tctx->netns_cookie = netns_cookie;\n""",
    ),
    (
        b"""\tctx->fill = fill;\n\tctx->comp = comp;\n\tctx->fill_map = fill->ring - off.fr.desc;\n\tctx->comp_map = comp->ring - off.cr.desc;\n""",
        b"""\tctx->fill = fill;\n\tctx->comp = comp;\n""",
    ),
    (
        b"""\txsk->rx = rx;\n\txsk->rx_map = rx_map;\n""",
        b"""\txsk->rx = rx;\n""",
    ),
    (
        b"""\txsk->tx = tx;\n\txsk->tx_map = tx_map;\n""",
        b"""\txsk->tx = tx;\n""",
    ),
    (
        b"""\t\tif (xsk->rx) {\n\t\t\tmunmap(xsk->rx_map,\n\t\t\t       off.rx.desc + xsk->config.rx_size * desc_sz);\n\t\t}\n\t\tif (xsk->tx) {\n\t\t\tmunmap(xsk->tx_map,\n\t\t\t       off.tx.desc + xsk->config.tx_size * desc_sz);\n\t\t}\n""",
        b"""\t\tif (xsk->rx) {\n\t\t\tmunmap(xsk->rx->ring - off.rx.desc,\n\t\t\t       off.rx.desc + xsk->config.rx_size * desc_sz);\n\t\t}\n\t\tif (xsk->tx) {\n\t\t\tmunmap(xsk->tx->ring - off.tx.desc,\n\t\t\t       off.tx.desc + xsk->config.tx_size * desc_sz);\n\t\t}\n""",
    ),
)


def tree_hash(root: Path, replacement: bytes | None = None) -> str:
    digest = hashlib.sha256()
    for path in sorted(
        candidate
        for candidate in root.rglob("*")
        if candidate.is_file() and candidate.relative_to(root) not in SOURCE_EXCLUSIONS
    ):
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
    upstream_source = patched_source
    for index, (patched_block, upstream_block) in enumerate(PATCH_BLOCKS):
        if patched_source.count(patched_block) != 1:
            fail(f"reviewed hunk {index} is missing or duplicated")
        upstream_source = upstream_source.replace(patched_block, upstream_block, 1)
        if patched_block in upstream_source:
            fail(f"reviewed hunk {index} remains after reconstructing upstream source")

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
