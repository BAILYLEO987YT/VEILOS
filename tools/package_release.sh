#!/usr/bin/env bash
# package_release.sh [STORAGE_DIR] -- build VEILOS into dist/VEILOS.zip
#
# The zip holds what someone needs to try VEILOS without Rust: one disk with
# the bootloader, the kernel and a FAT32 VEILOS volume made from storage/
# (veilos.img boots through the BIOS, for QEMU's SeaBIOS; veilos-uefi.img is
# the same for UEFI machines), the launch scripts in tools/release/, and
# VERSION.txt. GitHub Actions runs this on every push (.github/workflows/release.yml).
# STORAGE_DIR defaults to storage/; pass a clean copy to leave out local files.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
storage="${1:-storage}"
python="${PYTHON:-python3}"
out=dist/VEILOS

rm -rf dist
mkdir -p "$out"

sh tools/build.sh
# Room to work, and still small once zipped (empty space compresses to nothing).
for kind in bios uefi; do
    "$python" tools/mkdisk.py "$out/veilos-$kind.img" --size 512M --from "$storage" \
        --boot target/x86_64-unknown-none/debug/veilos-$kind.img
done
mv "$out/veilos-bios.img" "$out/veilos.img"

cp tools/release/README.txt tools/release/run-windows.bat tools/release/run.sh "$out/"
{
    echo "VEILOS $(git rev-parse --short HEAD)"
    git log -1 --format='%s%n%cd' --date=iso
} > "$out/VERSION.txt"

"$python" - <<'EOF'
import os, zipfile
with zipfile.ZipFile("dist/VEILOS.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for root, _, files in os.walk("dist/VEILOS"):
        for f in sorted(files):
            path = os.path.join(root, f)
            info = zipfile.ZipInfo.from_file(path, os.path.relpath(path, "dist"))
            if f.endswith(".sh"):
                info.external_attr = (0o100755 << 16)
            info.compress_type = zipfile.ZIP_DEFLATED
            with open(path, "rb") as src:
                z.writestr(info, src.read(), compresslevel=9)
EOF
ls -l dist/VEILOS.zip
