#!/usr/bin/env python3
"""mkdisk.py OUT [--size 8G] [--from storage] [--add HOST=GUEST ...] [--no-marker] [--boot BOOTIMG]

Build a FAT32 disk image holding a folder, for a QEMU run with room to work:

    -drive id=disk,file=OUT,if=none,format=raw

`--from` is the root of the disk (default storage/ beside this script's repo);
each `--add` copies a host file or folder to a path on the disk, for what
shouldn't be in the repository (toolchains and the like). Times are the host's.

The disk gets VEILOS.VOL at its root (storage/ has it; it is added if the
folder doesn't), the marker VEILOS needs before it mounts a volume as `/` and
writes to it (src/fs/block.rs). --no-marker makes a data disk VEILOS only reads.

--boot BOOTIMG makes one bootable disk (a USB stick, or a VM's only disk):
the bootloader's image from tools/build.sh, then this volume as a partition
after it. A GPT image (veilos-uefi.img) gets a GPT entry of VEILOS's own type
(VEILOS_TYPE: Windows leaves such a partition alone), an MBR image
(veilos-bios.img) a FAT32 entry in slot 3. --size is then the volume's size.
QEMU's fat:rw: folder disk is limited to about 500 MB and can't take a rename.
"""
import argparse
import os
import struct
import sys
import time
import uuid
import zlib

SECTOR = 512
RESERVED = 32
FATS = 2
ENTRY = 32
ATTR_DIR = 0x10
ATTR_ARCHIVE = 0x20
ATTR_LFN = 0x0F
EOC = 0x0FFFFFFF
# GPT partition type of a VEILOS system volume (made up for VEILOS; the
# kernel still goes by VEILOS.VOL, this keeps other systems off it).
VEILOS_TYPE = "700EFA60-219D-4B2F-9F39-B2EA24D56442"
MARKER_NAME = "VEILOS.VOL"
MARKER = (
    b"VEILOS SYSTEM VOLUME\r\n"
    b"This file marks the volume VEILOS boots from and writes to. Without it at the\r\n"
    b"root of a volume, VEILOS mounts that volume read-only.\r\n"
)


def parse_size(s):
    units = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30}
    return int(s[:-1]) * units[s[-1].upper()] if s[-1].upper() in units else int(s)


def fat_time(t):
    tm = time.localtime(max(t, 315532800 + 86400))
    return ((tm.tm_year - 1980) << 9) | (tm.tm_mon << 5) | tm.tm_mday, (tm.tm_hour << 11) | (tm.tm_min << 5) | (tm.tm_sec // 2)


def short_name(name, taken):
    """An 8.3 name, and whether `name` needs a long entry beside it."""
    legal = lambda c: c.isalnum() and ord(c) < 128 or c in "$%'-_@~`!(){}^#&"
    base, dot, ext = name.rpartition(".")
    if not dot or not base:
        base, ext = name, ""
    fits = name.upper() == name and len(base) <= 8 and len(ext) <= 3 and all(legal(c) for c in base + ext) and base != ""
    b = "".join(c for c in base.upper() if legal(c)) or "X"
    e = "".join(c for c in ext.upper() if legal(c))[:3]
    if fits:
        s = b.ljust(8) + e.ljust(3)
        if s not in taken:
            return s.encode("ascii"), False
    for n in range(1, 1000000):
        tail = "~%d" % n
        s = (b[: 8 - len(tail)] + tail).ljust(8) + e.ljust(3)
        if s not in taken:
            return s.encode("ascii"), True
    raise ValueError("too many similar names: " + name)


def checksum(short):
    c = 0
    for b in short:
        c = (((c & 1) << 7) + (c >> 1) + b) & 0xFF
    return c


def lfn_entries(name, short):
    units = list(struct.unpack("<%dH" % (len(name.encode("utf-16le")) // 2), name.encode("utf-16le")))
    count = (len(units) + 12) // 13
    if count > 20:
        raise ValueError("name too long: " + name)
    if len(units) % 13:
        units += [0] + [0xFFFF] * (12 - len(units) % 13)
    sum_ = checksum(short)
    out = []
    for seq in range(count, 0, -1):
        part = units[(seq - 1) * 13 : seq * 13]
        e = bytes([seq | (0x40 if seq == count else 0)])
        e += struct.pack("<5H", *part[:5]) + bytes([ATTR_LFN, 0, sum_])
        e += struct.pack("<6H", *part[5:11]) + b"\0\0" + struct.pack("<2H", *part[11:13])
        out.append(e)
    return out


def short_entry(short, attr, cluster, size, mtime):
    date, tm = fat_time(mtime)
    return (
        short
        + struct.pack("<BBBHHHHHHHI", attr, 0, 0, tm, date, date, cluster >> 16, tm, date, cluster & 0xFFFF, size)
    )


class Disk:
    """A FAT32 volume of `size` bytes at byte `base` of the file at `path`
    (0: the whole file, made anew)."""

    def __init__(self, path, size, base=0):
        if base:
            self.f = open(path, "r+b")
            self.f.seek(0, 2)
            if self.f.tell() < base + size:
                self.f.truncate(base + size)
        else:
            self.f = open(path, "w+b")
            self.f.truncate(size)
        self.base = base
        total = size // SECTOR
        # Clusters as Windows formats: 512 bytes below 260 MiB (a small RAM
        # disk), 4 KiB to 8 GiB, larger ones above.
        self.spc = 1 if size < 260 << 20 else 8 if size <= 8 << 30 else 16 if size <= 16 << 30 else 32
        self.cb = self.spc * SECTOR
        clusters = total // self.spc
        self.spf = (clusters + 2) * 4 // SECTOR + 1
        self.data_start = RESERVED + FATS * self.spf
        self.clusters = (total - self.data_start) // self.spc
        if self.clusters < 65525:
            raise SystemExit("too small for FAT32: at least 34M")
        self.total = total
        self.fat = bytearray((self.clusters + 2) * 4)
        struct.pack_into("<II", self.fat, 0, 0x0FFFFFF8, EOC)
        self.next = 2

    def alloc(self, n):
        """`n` clusters in a row, chained: the first one."""
        if n == 0:
            return 0
        first = self.next
        if first + n > self.clusters + 2:
            raise SystemExit("disk is full: use a larger --size")
        for c in range(first, first + n):
            struct.pack_into("<I", self.fat, c * 4, c + 1 if c + 1 < first + n else EOC)
        self.next += n
        return first

    def seek(self, cluster):
        self.f.seek(self.base + (self.data_start + (cluster - 2) * self.spc) * SECTOR)

    def put_file(self, path):
        if isinstance(path, bytes):
            first = self.alloc((len(path) + self.cb - 1) // self.cb)
            if first:
                self.seek(first)
                self.f.write(path)
            return first, len(path)
        size = os.path.getsize(path)
        first = self.alloc((size + self.cb - 1) // self.cb)
        if first:
            self.seek(first)
            with open(path, "rb") as src:
                while True:
                    chunk = src.read(1 << 22)
                    if not chunk:
                        break
                    self.f.write(chunk)
        return first, size

    def finish(self):
        for k in range(FATS):
            self.f.seek(self.base + (RESERVED + k * self.spf) * SECTOR)
            self.f.write(self.fat)
        boot = bytearray(SECTOR)
        boot[0:3] = b"\xEB\x58\x90"
        boot[3:11] = b"VEILOS  "
        struct.pack_into("<HBHBHH", boot, 11, SECTOR, self.spc, RESERVED, FATS, 0, 0)
        boot[21] = 0xF8
        struct.pack_into("<HHHII", boot, 22, 0, 63, 255, self.base // SECTOR, self.total)
        struct.pack_into("<IHHIHH", boot, 36, self.spf, 0, 0, 2, 1, 6)
        boot[64] = 0x80
        boot[66] = 0x29
        struct.pack_into("<I", boot, 67, 0x56454C4F)
        boot[71:82] = b"VEILOS     "
        boot[82:90] = b"FAT32   "
        boot[510:512] = b"\x55\xAA"
        info = bytearray(SECTOR)
        struct.pack_into("<I", info, 0, 0x41615252)
        struct.pack_into("<III", info, 484, 0x61417272, 0xFFFFFFFF, 0xFFFFFFFF)
        struct.pack_into("<I", info, 508, 0xAA550000)
        for at in (0, 6):
            self.f.seek(self.base + at * SECTOR)
            self.f.write(boot)
            self.f.write(info)
        self.f.close()


class Node:
    """A folder on the disk: named children, each a host path or a Node."""

    def __init__(self, host=None):
        self.children = {}
        self.host = host

    def child(self, name):
        key = name.lower()
        if key not in self.children:
            self.children[key] = (name, Node())
        return self.children[key][1]

    def add_tree(self, path):
        for name in sorted(os.listdir(path)):
            p = os.path.join(path, name)
            if os.path.isdir(p):
                self.child(name).add_tree(p)
            else:
                self.children[name.lower()] = (name, p)


def write_dir(disk, node, cluster, parent):
    entries = []
    if parent is not None:
        now = time.time()
        entries.append(short_entry(b".          ", ATTR_DIR, cluster, 0, now))
        entries.append(short_entry(b"..         ", ATTR_DIR, parent, 0, now))
    taken = set()
    subdirs = []
    for name, item in node.children.values():
        short, needs_long = short_name(name, taken)
        taken.add(short.decode("ascii"))
        if needs_long:
            entries.extend(lfn_entries(name, short))
        if isinstance(item, Node):
            at = len(entries)
            entries.append(short)  # Filled in below, when its cluster is known.
            subdirs.append((at, short, item))
        else:
            first, size = disk.put_file(item)
            mtime = time.time() if isinstance(item, bytes) else os.path.getmtime(item)
            entries.append(short_entry(short, ATTR_ARCHIVE, first, size, mtime))
    # Each folder needs its own clusters before its children can name them.
    allocated = {}
    for at, short, item in subdirs:
        count = dir_entry_count(item)
        c = disk.alloc(max(1, (count * ENTRY + disk.cb - 1) // disk.cb))
        allocated[id(item)] = c
        entries[at] = short_entry(short, ATTR_DIR, c, 0, time.time())
    raw = b"".join(entries)
    disk.seek(cluster)
    disk.f.write(raw)
    disk.f.write(b"\0" * ((len(raw) + disk.cb - 1) // disk.cb * disk.cb - len(raw)))
    # ".." holds 0 when the parent is the root.
    for at, short, item in subdirs:
        write_dir(disk, item, allocated[id(item)], cluster if parent is not None else 0)


def dir_entry_count(node):
    n = 2
    for name, _ in node.children.values():
        n += 1 + (len(name) + 12) // 13
    return n


def copy_boot_image(boot, out, size):
    """Copy the bootloader's image to `out` and add a partition for the
    VEILOS volume after its last one. Returns the volume's byte offset."""
    with open(boot, "rb") as f:
        img = bytearray(f.read())
    if img[510:512] != b"\x55\xAA":
        sys.exit("%s: no partition table" % boot)
    gpt = img[512:520] == b"EFI PART"
    if gpt:
        entries_lba, count, esize = struct.unpack_from("<QII", img, 512 + 72)
        table = img[entries_lba * SECTOR:entries_lba * SECTOR + count * esize]
        used = [table[i * esize:(i + 1) * esize] for i in range(count) if any(table[i * esize:i * esize + 16])]
        last = max(struct.unpack_from("<Q", e, 40)[0] for e in used)
    else:
        parts = [struct.unpack_from("<II", img, 446 + i * 16 + 8) for i in range(4) if img[446 + i * 16 + 4]]
        last = max(s + n - 1 for s, n in parts)
        if len(parts) > 3:
            sys.exit("%s: no free MBR slot" % boot)
    start = (last + 1 + 2047) // 2048 * 2048
    sectors = size // SECTOR
    total = start + sectors + (2048 if gpt else 0)

    # The boot image's partitions, as they were; nothing past them.
    img = img[:(last + 1) * SECTOR]
    with open(out, "wb") as f:
        f.truncate(total * SECTOR)
        f.write(img)
        if gpt:
            entry = (uuid.UUID(VEILOS_TYPE).bytes_le + uuid.uuid4().bytes_le
                     + struct.pack("<QQQ", start, start + sectors - 1, 0) + "VEILOS".encode("utf-16le").ljust(72, b"\0"))
            table = (b"".join(used) + entry).ljust(128 * 128, b"\0")
            crc = zlib.crc32(table)
            disk_guid = uuid.uuid4().bytes_le

            def header(cur, alt, at):
                h = bytearray(struct.pack("<8sIIIIQQQQ16sQIII", b"EFI PART", 0x10000, 92, 0, 0, cur, alt,
                                          34, total - 34, disk_guid, at, 128, 128, crc))
                struct.pack_into("<I", h, 16, zlib.crc32(h))
                return bytes(h).ljust(SECTOR, b"\0")

            mbr = bytearray(img[:SECTOR])
            mbr[446:512] = b"\0" * 64 + b"\x55\xAA"
            mbr[446:462] = struct.pack("<B3sB3sII", 0, b"\0\x02\0", 0xEE, b"\xff\xff\xff", 1, min(total - 1, 0xFFFFFFFF))
            f.seek(0)
            f.write(mbr + header(1, total - 1, 2) + table)
            f.seek((total - 33) * SECTOR)
            f.write(table + header(total - 1, 1, total - 33))
        else:
            slot = next(i for i in range(4) if not img[446 + i * 16 + 4])
            f.seek(446 + slot * 16)
            f.write(struct.pack("<B3sB3sII", 0, b"\xfe\xff\xff", 0x0C, b"\xfe\xff\xff", start, sectors))
    return start * SECTOR


def main():
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--size", default="8G")
    ap.add_argument("--from", dest="root", default=os.path.join(repo, "storage"))
    ap.add_argument("--add", action="append", default=[], metavar="HOST=GUEST")
    ap.add_argument("--no-marker", action="store_true", help="a data disk: leave out VEILOS.VOL")
    ap.add_argument("--boot", metavar="BOOTIMG", help="one bootable disk: this image's partitions, then the volume")
    args = ap.parse_args()

    root = Node()
    root.add_tree(args.root)
    for spec in args.add:
        host, _, guest = spec.partition("=")
        parts = [p for p in guest.replace("\\", "/").split("/") if p]
        if not parts:
            sys.exit("--add needs a path on the disk: " + spec)
        if os.path.isdir(host):
            node = root
            for p in parts:
                node = node.child(p)
            node.add_tree(host)
        else:
            node = root
            for p in parts[:-1]:
                node = node.child(p)
            node.children[parts[-1].lower()] = (parts[-1], host)

    if args.no_marker:
        root.children.pop(MARKER_NAME.lower(), None)
    elif MARKER_NAME.lower() not in root.children:
        root.children[MARKER_NAME.lower()] = (MARKER_NAME, MARKER)

    size = parse_size(args.size)
    base = copy_boot_image(args.boot, args.out, size) if args.boot else 0
    disk = Disk(args.out, size, base)
    root_clusters = max(1, (dir_entry_count(root) * ENTRY + disk.cb - 1) // disk.cb)
    write_dir(disk, root, disk.alloc(root_clusters), None)
    used = disk.next - 2
    disk.finish()
    print("%s: %d MiB used of %d MiB" % (args.out, used * disk.cb >> 20, disk.total * SECTOR >> 20))


if __name__ == "__main__":
    main()
