"""Write .def files for msvcrt.dll, vcruntime140.dll and vcruntime140_1.dll:
DLLs that forward every name to ucrtbase.dll (WIN32_DESIGN.md, W5), so a
program sees one C runtime whichever of them it imports.

Usage: crt_forwarders.py <llvm-objdump> <ucrtbase.dll> <out dir>
"""
import os
import re
import subprocess
import sys

objdump, dll, out = sys.argv[1:4]
listing = subprocess.run([objdump, "-p", dll], capture_output=True, text=True, check=True).stdout
names = sorted({m.group(1) for m in re.finditer(r"^\s+\d+\s+0x[0-9a-f]+\s+(\S+)$", listing, re.M)})
if len(names) < 100:
    sys.exit(f"only {len(names)} exports found in {dll}")

# Where msvcrt's name means something other than UCRT's.
MSVCRT_RENAMES = {"wcstok": "_wcstok"}

os.makedirs(out, exist_ok=True)
for target, renames in [("msvcrt", MSVCRT_RENAMES), ("vcruntime140", {}), ("vcruntime140_1", {})]:
    with open(os.path.join(out, target + ".def"), "w", newline="\n") as f:
        f.write(f"LIBRARY {target}.dll\nEXPORTS\n")
        for n in names:
            f.write(f"    {n}=ucrtbase.{renames.get(n, n)}\n")
print(f"{len(names)} forwarded names")
