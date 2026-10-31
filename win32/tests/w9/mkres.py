"""Make w9gfx's resources: test.bmp (8x8, 24bpp: pixel (x, y) is
RGB(32x, 32y, 128)) and test.ico (32x32 32bpp with alpha: left half opaque
red, column 20 half-transparent blue, the rest clear; and 16x16 8bpp
green). Usage: mkres.py OUTDIR"""
import struct, sys, os

out = sys.argv[1]

# test.bmp: bottom-up rows, padded to 4 bytes.
w = h = 8
rows = b""
for y in reversed(range(h)):
    row = b"".join(bytes([128, y * 32, x * 32]) for x in range(w))
    rows += row + b"\0" * ((4 - len(row) % 4) % 4)
info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(rows), 2835, 2835, 0, 0)
bmp = b"BM" + struct.pack("<IHHI", 14 + len(info) + len(rows), 0, 0, 14 + len(info)) + info + rows
open(os.path.join(out, "test.bmp"), "wb").write(bmp)


def icon_image(size, bpp, pixel, palette=None):
    """BITMAPINFOHEADER (height doubled), palette, XOR bits, AND mask."""
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, bpp, 0, 0, 0, 0, len(palette or []), 0)
    pal = b"".join(bytes([b, g, r, 0]) for (r, g, b) in (palette or []))
    xor = b""
    mask = b""
    for y in reversed(range(size)):
        row = b""
        mrow = 0
        for x in range(size):
            v, alpha = pixel(x, y)
            if bpp == 32:
                r, g, b = v
                row += bytes([b, g, r, alpha])
            else:
                row += bytes([v])
            if alpha == 0:
                mrow |= 1 << (size - 1 - x)
        xor += row + b"\0" * ((4 - len(row) % 4) % 4)
        mbytes = mrow.to_bytes((size + 7) // 8, "big")
        mask += mbytes + b"\0" * ((4 - len(mbytes) % 4) % 4)
    return hdr + pal + xor + mask


def big(x, y):
    if x < 16:
        return (255, 0, 0), 255
    if x == 20:
        return (0, 0, 255), 128
    return (0, 0, 0), 0


images = [
    (32, 32, icon_image(32, 32, big)),
    (16, 8, icon_image(16, 8, lambda x, y: (1, 255), [(0, 0, 0), (0, 200, 0)] + [(0, 0, 0)] * 254)),
]
ico = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
for size, bpp, data in images:
    ico += struct.pack("<BBBBHHII", size, size, 0, 0, 1, bpp, len(data), offset)
    offset += len(data)
for _, _, data in images:
    ico += data
open(os.path.join(out, "test.ico"), "wb").write(ico)
