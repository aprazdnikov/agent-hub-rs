"""Draws the agent-hub icon (green disc, white ring) into assets/agent-hub.ico.

Run once after changing the design: python scripts/make-icon.py
"""
import math
import struct
import zlib
from pathlib import Path

GREEN = (46, 160, 67)
WHITE = (255, 255, 255)


def pixel(x: float, y: float, size: int) -> tuple[int, int, int, int]:
    radius = size / 2
    distance = math.hypot(x - radius, y - radius)
    if distance > radius - 0.5:
        return (0, 0, 0, 0)
    ring = abs(distance - radius * 0.55) < radius * 0.09
    r, g, b = WHITE if ring else GREEN
    return (r, g, b, 255)


def png(size: int) -> bytes:
    rows = b"".join(
        b"\x00" + b"".join(bytes(pixel(x + 0.5, y + 0.5, size)) for x in range(size))
        for y in range(size)
    )

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def ico(sizes: list[int]) -> bytes:
    images = [png(size) for size in sizes]
    offset = 6 + 16 * len(images)
    entries = b""
    for size, image in zip(sizes, images, strict=True):
        side = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(image), offset)
        offset += len(image)
    return struct.pack("<HHH", 0, 1, len(images)) + entries + b"".join(images)


if __name__ == "__main__":
    target = Path(__file__).resolve().parent.parent / "assets" / "agent-hub.ico"
    target.parent.mkdir(exist_ok=True)
    target.write_bytes(ico([16, 32, 48, 256]))
    print(f"wrote {target}")
