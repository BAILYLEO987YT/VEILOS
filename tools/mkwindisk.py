#!/usr/bin/env python3
"""mkwindisk.py OUT

A test disk shaped like a PC's Windows install, for checking that VEILOS
never writes to a disk that isn't its own (src/block.rs):

    GPT
      1  EFI System Partition   FAT32, 300 MiB   \\EFI\\Microsoft\\Boot\\..., \\EFI\\Boot\\BOOTX64.EFI
      2  Microsoft Reserved     16 MiB, no filesystem
      3  Basic data             NTFS, 64 MiB     \\bootmgr, \\Windows\\System32, \\Users

Attach it before the VEILOS disk, as a real PC would order them:

    -drive id=win,file=OUT,if=none,format=raw -device ide-hd,drive=win,bus=ide.0

and compare a checksum of OUT before and after the run. The files are
placeholders made here (no Microsoft binaries); the NTFS volume is the
smallest one VEILOS's NTFS driver reads: a boot sector and an MFT whose
root and folders use resident $INDEX_ROOT indexes.
"""
import os
import struct
import subprocess
import sys
import tempfile
import uuid
import zlib

SECTOR = 512
MIB = 1 << 20
ESP_TYPE = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
MSR_TYPE = "E3C9E316-0B5C-4DB8-817D-F92DF00215AE"
DATA_TYPE = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"
PLACEHOLDER = b"VEILOS test fixture: a placeholder, not a Microsoft file.\r\n"

# -----| NTFS |-----

CLUSTER = 4096
SPC = CLUSTER // SECTOR
RECORD = 1024
MFT_LCN = 4
MFT_RECORDS = 128
FILETIME = 133_000_000_000_000_000  # 2022, a fixed time so images repeat


def align8(b):
    return b + b"\0" * (-len(b) % 8)


def resident(atype, value, name="", attr_id=0, indexed=0):
    name16 = name.encode("utf-16le")
    value_off = (24 + len(name16) + 7) // 8 * 8
    hdr = struct.pack("<IIBBHHHIHBB", atype, 0, 0, len(name), 24 if name else 0, 0, attr_id, len(value), value_off, indexed, 0)
    body = hdr + name16
    body += b"\0" * (value_off - len(body)) + value
    body = align8(body)
    return body[:4] + struct.pack("<I", len(body)) + body[8:]


def nonresident_data(lcn, clusters, size, attr_id=0):
    runs = bytes([0x11, clusters, lcn, 0])
    hdr = struct.pack("<IIBBHHHQQHHIQQQ", 0x80, 0, 1, 0, 0, 0, attr_id, 0, clusters - 1, 64, 0, 0, clusters * CLUSTER, size, size)
    body = align8(hdr + runs)
    return body[:4] + struct.pack("<I", len(body)) + body[8:]


def std_info(flags):
    return struct.pack("<QQQQI", FILETIME, FILETIME, FILETIME, FILETIME, flags) + b"\0" * 12


def file_name(parent, name, is_dir, size=0):
    flags = 0x10000000 if is_dir else 0x20
    n16 = name.encode("utf-16le")
    return struct.pack("<QQQQQQQIIBB", parent | (1 << 48), FILETIME, FILETIME, FILETIME, FILETIME, size, size, flags, 0, len(name), 3) + n16


def index_root(children, parent=5):
    """children: (record, name, is_dir, size), in collation order."""
    entries = b""
    for rec, name, is_dir, size in children:
        key = file_name(parent, name, is_dir, size)
        e = struct.pack("<QHHHH", rec | (1 << 48), 0, len(key), 0, 0) + key
        e = align8(e)
        entries += e[:8] + struct.pack("<H", len(e)) + e[10:]
    entries += struct.pack("<QHHHH", 0, 16, 0, 2, 0)  # the last entry
    node = struct.pack("<IIIB3x", 16, 16 + len(entries), 16 + len(entries), 0) + entries
    return struct.pack("<IIIB3x", 0x30, 1, CLUSTER, 1) + node


def mft_record(num, attrs, is_dir=False, in_use=True):
    rec = bytearray(RECORD)
    flags = (1 if in_use else 0) | (2 if is_dir else 0)
    body = b"".join(attrs) + struct.pack("<II", 0xFFFFFFFF, 0)
    used = 56 + len(body)
    struct.pack_into("<4sHHQHHHHIIQHHI", rec, 0, b"FILE", 48, 3, 0, 1, 1, 56, flags, used, RECORD, 0, len(attrs), 0, num)
    rec[56:56 + len(body)] = body
    # Update sequence: the last two bytes of each sector move into the array.
    usn = b"\x01\x00"
    rec[48:50] = usn
    for i in (1, 2):
        end = i * SECTOR - 2
        rec[48 + i * 2:50 + i * 2] = rec[end:end + 2]
        rec[end:end + 2] = usn
    return bytes(rec)


def make_ntfs(sectors):
    vol = bytearray(sectors * SECTOR)
    boot = bytearray(SECTOR)
    boot[0:3] = b"\xEB\x52\x90"
    boot[3:11] = b"NTFS    "
    struct.pack_into("<HBHB", boot, 11, SECTOR, SPC, 0, 0)
    boot[0x15] = 0xF8
    struct.pack_into("<HHI", boot, 0x18, 63, 255, 0)
    struct.pack_into("<I", boot, 0x24, 0x00800080)
    struct.pack_into("<QQQ", boot, 0x28, sectors - 1, MFT_LCN, sectors // SPC // 2)
    struct.pack_into("<bxxxbxxxQ", boot, 0x40, -10, 1, 0x1234_5678_9ABC_DEF0)
    boot[510:512] = b"\x55\xAA"
    vol[0:SECTOR] = boot
    vol[(sectors - 1) * SECTOR:] = boot  # the backup boot sector

    mft_clusters = MFT_RECORDS * RECORD // CLUSTER
    system = ["$MFT", "$MFTMirr", "$LogFile", "$Volume", "$AttrDef", ".", "$Bitmap", "$Boot",
              "$BadClus", "$Secure", "$UpCase", "$Extend"]
    records = {}
    for n, name in enumerate(system):
        attrs = [resident(0x10, std_info(6), attr_id=0), resident(0x30, file_name(5, name, n in (5, 11)), attr_id=1, indexed=1)]
        if n == 0:
            attrs.append(nonresident_data(MFT_LCN, mft_clusters, MFT_RECORDS * RECORD, attr_id=2))
        records[n] = (attrs, n in (5, 11))

    bootmgr = PLACEHOLDER
    system32_children = [(68, "config", True, 0)]
    windows_children = [(66, "System32", True, 0)]
    users_children = [(69, "Public", True, 0)]
    root_children = [(64, "bootmgr", False, len(bootmgr)), (67, "Users", True, 0), (65, "Windows", True, 0)]

    def directory(rec, parent, name, children):
        return ([resident(0x10, std_info(0x10)), resident(0x30, file_name(parent, name, True), attr_id=1, indexed=1),
                 resident(0x90, index_root(children, rec), name="$I30", attr_id=2)], True)

    records[5] = ([resident(0x10, std_info(6)), resident(0x30, file_name(5, ".", True), attr_id=1, indexed=1),
                   resident(0x90, index_root(root_children), name="$I30", attr_id=2)], True)
    records[64] = ([resident(0x10, std_info(0x27)), resident(0x30, file_name(5, "bootmgr", False, len(bootmgr)), attr_id=1, indexed=1),
                    resident(0x80, bootmgr, attr_id=2)], False)
    records[65] = directory(65, 5, "Windows", windows_children)
    records[66] = directory(66, 65, "System32", system32_children)
    records[67] = directory(67, 5, "Users", users_children)
    records[68] = directory(68, 66, "config", [])
    records[69] = directory(69, 67, "Public", [])

    mft = MFT_LCN * CLUSTER
    for n in range(MFT_RECORDS):
        if n in records:
            attrs, is_dir = records[n]
            rec = mft_record(n, attrs, is_dir)
        else:
            rec = mft_record(n, [], in_use=False)
        vol[mft + n * RECORD:mft + (n + 1) * RECORD] = rec
    mirror = (sectors // SPC // 2) * CLUSTER
    vol[mirror:mirror + 4 * RECORD] = vol[mft:mft + 4 * RECORD]
    return bytes(vol)

# -----| GPT |-----


def gpt_entry(type_guid, start, end, name):
    return (uuid.UUID(type_guid).bytes_le + uuid.uuid5(uuid.NAMESPACE_URL, "veilos-test/" + name).bytes_le
            + struct.pack("<QQQ", start, end, 0) + name.encode("utf-16le").ljust(72, b"\0"))


def gpt_header(current, backup, first, last, entries_lba, entries_crc):
    disk_guid = uuid.uuid5(uuid.NAMESPACE_URL, "veilos-test/disk").bytes_le
    h = bytearray(struct.pack("<8sIIIIQQQQ16sQIII", b"EFI PART", 0x10000, 92, 0, 0, current, backup, first, last,
                              disk_guid, entries_lba, 128, 128, entries_crc))
    struct.pack_into("<I", h, 16, zlib.crc32(h))
    return bytes(h).ljust(SECTOR, b"\0")


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    here = os.path.dirname(os.path.abspath(__file__))

    esp_start, esp_size = 2048, 300 * MIB // SECTOR
    msr_start, msr_size = esp_start + esp_size, 16 * MIB // SECTOR
    ntfs_start, ntfs_size = msr_start + msr_size, 64 * MIB // SECTOR
    total = ntfs_start + ntfs_size + 2048

    with tempfile.TemporaryDirectory() as tmp:
        tree = os.path.join(tmp, "esp")
        for d in ("EFI/Microsoft/Boot", "EFI/Boot"):
            os.makedirs(os.path.join(tree, d))
        for f in ("EFI/Microsoft/Boot/bootmgfw.efi", "EFI/Microsoft/Boot/BCD", "EFI/Boot/BOOTX64.EFI"):
            with open(os.path.join(tree, f), "wb") as fh:
                fh.write(PLACEHOLDER)
        esp_img = os.path.join(tmp, "esp.img")
        subprocess.run([sys.executable, os.path.join(here, "mkdisk.py"), esp_img, "--size", "300M", "--from", tree, "--no-marker"],
                       check=True, stdout=subprocess.DEVNULL)
        with open(esp_img, "rb") as fh:
            esp = fh.read()

    entries = (gpt_entry(ESP_TYPE, esp_start, esp_start + esp_size - 1, "EFI system partition")
               + gpt_entry(MSR_TYPE, msr_start, msr_start + msr_size - 1, "Microsoft reserved partition")
               + gpt_entry(DATA_TYPE, ntfs_start, ntfs_start + ntfs_size - 1, "Basic data partition")).ljust(128 * 128, b"\0")
    crc = zlib.crc32(entries)

    mbr = bytearray(SECTOR)
    mbr[446:462] = struct.pack("<B3sB3sII", 0, b"\0\x02\0", 0xEE, b"\xff\xff\xff", 1, min(total - 1, 0xFFFFFFFF))
    mbr[510:512] = b"\x55\xAA"

    with open(out, "wb") as f:
        f.truncate(total * SECTOR)
        f.write(mbr)
        f.write(gpt_header(1, total - 1, 34, total - 34, 2, crc))
        f.write(entries)
        f.seek((total - 33) * SECTOR)
        f.write(entries)
        f.write(gpt_header(total - 1, 1, 34, total - 34, total - 33, crc))
        f.seek(esp_start * SECTOR)
        f.write(esp)
        f.seek(ntfs_start * SECTOR)
        f.write(make_ntfs(ntfs_size))
    print("%s: %d MiB, GPT: ESP (FAT32), MSR, NTFS" % (out, total * SECTOR >> 20))


if __name__ == "__main__":
    main()
