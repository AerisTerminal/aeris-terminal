#!/usr/bin/env python3
import socket
import struct
import sys
from pathlib import Path


def read_frames(path: Path) -> list[bytes]:
    data = path.read_bytes()
    if len(data) < 24:
        raise ValueError("pcap global header is truncated")
    magic, major, minor, _, _, _, network = struct.unpack_from("<IHHIIII", data)
    if magic != 0xA1B2C3D4 or (major, minor) != (2, 4) or network != 1:
        raise ValueError("pcap header is not little-endian Ethernet v2.4")

    frames: list[bytes] = []
    offset = 24
    while offset < len(data):
        if len(data) - offset < 16:
            raise ValueError("pcap packet header is truncated")
        _, _, captured_length, original_length = struct.unpack_from("<IIII", data, offset)
        offset += 16
        if captured_length != original_length or len(data) - offset < captured_length:
            raise ValueError("pcap packet payload is truncated")
        frames.append(data[offset : offset + captured_length])
        offset += captured_length
    return frames


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} INTERFACE PCAP", file=sys.stderr)
        return 2

    frames = read_frames(Path(sys.argv[2]))
    with socket.socket(socket.AF_PACKET, socket.SOCK_RAW) as sender:
        sender.bind((sys.argv[1], 0))
        for frame in frames:
            sent = sender.send(frame)
            if sent != len(frame):
                raise OSError(f"short AF_PACKET send: {sent} of {len(frame)} bytes")
    print(f"successful_packets={len(frames)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
