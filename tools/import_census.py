#!/usr/bin/env python3
"""import_census.py [--veilos DIR] [--md OUT] PATH... -- what a driver package imports.

For every 64-bit PE file (.sys, .dll, .exe) under each PATH (a file or a
folder), list the functions it imports (normal and delay-load), grouped by the
DLL they come from, and say how many of them VEILOS provides: a function
counts as provided when a DLL of that name in --veilos (storage/system by
default) exports it. API-set names (api-ms-win-*, ext-ms-*) count as provided
when any VEILOS DLL exports the function. Kernel modules VEILOS doesn't have
yet (ntoskrnl.exe, hal.dll, dxgkrnl.sys, ...) show everything as missing:
that list is the work for WDDM_DESIGN.md stage W1.

Reads the files only; nothing is run. Used on vendor GPU driver packages
(the user's own download or driver store), which stay out of the repository.
"""
import argparse
import os
import struct
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_VEILOS = os.path.join(HERE, "..", "storage", "system")


class Pe:
    def __init__(self, path):
        self.data = open(path, "rb").read()
        d = self.data
        if d[:2] != b"MZ":
            raise ValueError("not MZ")
        pe = struct.unpack_from("<I", d, 0x3C)[0]
        if d[pe:pe + 4] != b"PE\0\0":
            raise ValueError("not PE")
        self.machine, nsec = struct.unpack_from("<HH", d, pe + 4)
        opt_size = struct.unpack_from("<H", d, pe + 20)[0]
        opt = pe + 24
        self.magic = struct.unpack_from("<H", d, opt)[0]
        if self.magic != 0x20B:
            raise ValueError("not PE32+")
        ndirs = struct.unpack_from("<I", d, opt + 108)[0]
        self.dirs = [struct.unpack_from("<II", d, opt + 112 + 8 * i) for i in range(min(ndirs, 16))]
        self.sections = []
        s = opt + opt_size
        for i in range(nsec):
            name = d[s + 40 * i:s + 40 * i + 8].rstrip(b"\0")
            vsize, va, raw_size, raw = struct.unpack_from("<IIII", d, s + 40 * i + 8)
            self.sections.append((va, max(vsize, raw_size), raw, raw_size, name))

    def off(self, rva):
        for va, size, raw, raw_size, _ in self.sections:
            if va <= rva < va + size and rva - va < raw_size:
                return raw + rva - va
        return None

    def cstr(self, rva):
        o = self.off(rva)
        if o is None:
            return None
        end = self.data.index(b"\0", o)
        return self.data[o:end].decode("latin1")

    def _thunks(self, rva):
        out = []
        o = self.off(rva)
        if o is None:
            return out
        while True:
            v = struct.unpack_from("<Q", self.data, o)[0]
            if v == 0:
                break
            if v & (1 << 63):
                out.append("#%d" % (v & 0xFFFF))
            else:
                n = self.cstr((v & 0x7FFFFFFF) + 2)
                out.append(n if n is not None else "?")
            o += 8
        return out

    def imports(self):
        """{dll (lower case): set of names}, normal and delay-load."""
        res = defaultdict(set)
        if len(self.dirs) > 1 and self.dirs[1][0]:
            o = self.off(self.dirs[1][0])
            while o is not None:
                oft, _, _, name, ft = struct.unpack_from("<IIIII", self.data, o)
                if name == 0:
                    break
                dll = (self.cstr(name) or "?").lower()
                res[dll].update(self._thunks(oft or ft))
                o += 20
        if len(self.dirs) > 13 and self.dirs[13][0]:
            o = self.off(self.dirs[13][0])
            while o is not None:
                attrs, name, _, _, nt = struct.unpack_from("<IIIII", self.data, o)
                if name == 0:
                    break
                dll = (self.cstr(name) or "?").lower()
                res[dll].update(self._thunks(nt))
                o += 32
        return res

    def exports(self):
        names = set()
        if not self.dirs or not self.dirs[0][0]:
            return names
        o = self.off(self.dirs[0][0])
        if o is None:
            return names
        n_names = struct.unpack_from("<I", self.data, o + 24)[0]
        names_rva = struct.unpack_from("<I", self.data, o + 32)[0]
        no = self.off(names_rva)
        for i in range(n_names):
            n = self.cstr(struct.unpack_from("<I", self.data, no + 4 * i)[0])
            if n:
                names.add(n)
        return names


def pe_files(paths):
    for p in paths:
        if os.path.isdir(p):
            for root, _, files in os.walk(p):
                for f in sorted(files):
                    if f.lower().endswith((".sys", ".dll", ".exe")):
                        yield os.path.join(root, f)
        else:
            yield p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--veilos", default=DEFAULT_VEILOS)
    ap.add_argument("--md", help="write a Markdown report here")
    ap.add_argument("--list-missing", type=int, default=0, help="print up to N missing names per DLL")
    a = ap.parse_args()

    provided = {}
    for f in pe_files([a.veilos]):
        try:
            provided[os.path.basename(f).lower()] = Pe(f).exports()
        except Exception:
            pass
    every = set().union(*provided.values()) if provided else set()

    per_file = {}
    total = defaultdict(set)
    for f in pe_files(a.paths):
        try:
            pe = Pe(f)
        except Exception:
            continue  # 32-bit or not a PE
        imps = pe.imports()
        per_file[f] = imps
        for dll, names in imps.items():
            total[dll] |= names

    def have(dll, name):
        if dll.startswith(("api-ms-win-", "ext-ms-")):
            return name in every
        return name in provided.get(dll, ())

    lines = []
    lines.append("# Import census\n")
    lines.append("%d 64-bit PE files under %s\n" % (len(per_file), ", ".join(a.paths)))
    lines.append("## By file\n")
    lines.append("| File | DLLs | Functions | VEILOS has |")
    lines.append("|---|---|---|---|")
    for f, imps in sorted(per_file.items(), key=lambda kv: -sum(len(v) for v in kv[1].values())):
        n = sum(len(v) for v in imps.values())
        h = sum(1 for d, v in imps.items() for x in v if have(d, x))
        lines.append("| %s | %d | %d | %d |" % (os.path.basename(f), len(imps), n, h))
    lines.append("\n## By imported DLL (all files together)\n")
    lines.append("| DLL | Functions | VEILOS has | Missing |")
    lines.append("|---|---|---|---|")
    for dll, names in sorted(total.items(), key=lambda kv: -len(kv[1])):
        h = sum(1 for x in names if have(dll, x))
        lines.append("| %s | %d | %d | %d |" % (dll, len(names), h, len(names) - h))
    if a.list_missing:
        lines.append("\n## Missing functions\n")
        for dll, names in sorted(total.items(), key=lambda kv: -len(kv[1])):
            miss = sorted(x for x in names if not have(dll, x))
            if miss:
                shown = miss[: a.list_missing]
                lines.append("**%s** (%d): %s%s\n" % (dll, len(miss), ", ".join(shown), " ..." if len(miss) > len(shown) else ""))
    text = "\n".join(lines) + "\n"
    if a.md:
        open(a.md, "w", encoding="utf-8").write(text)
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
