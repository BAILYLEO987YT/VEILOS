#!/usr/bin/env python3
"""addmarker.py IMAGE

Add VEILOS.VOL to the root of a FAT32 disk image made before VEILOS needed
it (src/fs/block.rs: without it the volume is mounted read-only and VEILOS
stops at boot). Leaves everything else as it is. The image must be a
whole-disk FAT32 volume, as tools/mkdisk.py makes. Does nothing if the
marker is already there.
"""
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkdisk import MARKER, fat_time  # noqa: E402

SHORT = b"VEILOS  VOL"


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    with open(sys.argv[1], "r+b") as f:
        b = f.read(512)
        if b[82:90] != b"FAT32   " or b[510:512] != b"\x55\xAA":
            sys.exit("%s: not a whole-disk FAT32 volume" % sys.argv[1])
        bps, spc, res, fats = struct.unpack_from("<HBHB", b, 11)
        spf, root = struct.unpack_from("<I", b, 36)[0], struct.unpack_from("<I", b, 44)[0]
        total = struct.unpack_from("<I", b, 32)[0]
        if bps != 512:
            sys.exit("only 512-byte sectors")
        csize = spc * 512
        data = res + fats * spf
        clusters = (total - data) // spc + 2
        f.seek(res * 512)
        fat = bytearray(f.read(spf * 512))

        def entry(c):
            return struct.unpack_from("<I", fat, c * 4)[0] & 0x0FFFFFFF

        def off(c):
            return (data + (c - 2) * spc) * 512

        # The root directory: is the marker there, and where's a free slot?
        free_slot = None
        c = root
        while 2 <= c < 0x0FFFFFF7:
            f.seek(off(c))
            raw = f.read(csize)
            for i in range(0, csize, 32):
                e = raw[i:i + 32]
                if e[0] == 0 or e[0] == 0xE5:
                    if free_slot is None:
                        free_slot = off(c) + i
                    if e[0] == 0:
                        break
                elif e[:11] == SHORT:
                    print("%s: VEILOS.VOL is already there" % sys.argv[1])
                    return
            else:
                c = entry(c)
                continue
            break
        if free_slot is None:
            sys.exit("the root directory is full")

        # One free cluster for the contents (they fit in one).
        target = next((n for n in range(3, clusters) if entry(n) == 0), None)
        if target is None:
            sys.exit("the disk is full")
        struct.pack_into("<I", fat, target * 4, 0x0FFFFFFF)
        for k in range(fats):
            f.seek((res + k * spf) * 512 + target * 4)
            f.write(fat[target * 4:target * 4 + 4])
        f.seek(off(target))
        f.write(MARKER.ljust(csize, b"\0"))
        date, tm = fat_time(time.time())
        f.seek(free_slot)
        f.write(SHORT + struct.pack("<BBBHHHHHHHI", 0x20, 0, 0, tm, date, date, target >> 16, tm, date, target & 0xFFFF, len(MARKER)))
    print("%s: VEILOS.VOL added" % sys.argv[1])


if __name__ == "__main__":
    main()
