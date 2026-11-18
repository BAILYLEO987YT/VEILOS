#!/bin/sh
# tools/build.sh [--release] [--bios|--uefi] [--disk SIZE] [--ramdisk SIZE]
#
# Build the kernel and its boot disk images (bootloader 0.11), in
# target/x86_64-unknown-none/<profile>/:
#
#   veilos-bios.img, veilos-uefi.img   the bootloader and kernel only, for a
#                                      VM whose VEILOS volume is another disk
#   --disk SIZE: veilos-bios-disk.img, veilos-uefi-disk.img
#                                      one disk: that, plus a SIZE VEILOS
#                                      volume made from storage/ (a VM, or a
#                                      SATA disk; tools/mkdisk.py --boot)
#   --ramdisk SIZE: veilos-bios-ram.img, veilos-uefi-ram.img
#                                      the volume (SIZE, 34M or more) loaded
#                                      into memory with the kernel: for a USB
#                                      stick, until VEILOS drives USB storage.
#                                      Changes are lost at restart.
#
# --bios or --uefi makes only that kind. The image builder (tools/mkimage) is
# a host program run from outside the repo, so the kernel's .cargo/config.toml
# (bare-metal target, build-std) doesn't apply to it. It needs a nightly from
# 2026-07-10 or later: the bootloader's own stages pin x86_64 0.15.5
# (MKIMAGE_TOOLCHAIN overrides the one used).
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
python=${PYTHON:-python}
command -v "$python" >/dev/null 2>&1 || python=python3
profile=debug
cargo_profile=
kinds=
disk=
ramdisk=
while [ $# -gt 0 ]; do
  case $1 in
    --release) profile=release; cargo_profile=--release ;;
    --bios|--uefi) kinds="$kinds $1" ;;
    --disk) disk=$2; shift ;;
    --ramdisk) ramdisk=$2; shift ;;
    *) echo "usage: tools/build.sh [--release] [--bios|--uefi] [--disk SIZE] [--ramdisk SIZE]" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$kinds" ] || kinds="--bios --uefi"
toolchain=${MKIMAGE_TOOLCHAIN:-nightly-2026-11-01}

(cd "$repo" && cargo build $cargo_profile)
out="$repo/target/x86_64-unknown-none/$profile"
# The firmware reads the kernel through slow disk services: leave the debug
# info (most of a debug build) behind. The unstripped ELF stays for gdb.
objcopy=$(ls "$(cd "$repo" && rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy* | head -1)
kernel="$out/VEIL-OS.boot"
"$objcopy" --strip-debug "$out/VEIL-OS" "$kernel"

mkimage() {
  (cd "${TMPDIR:-/tmp}" && cargo "+$toolchain" run --quiet --release --manifest-path "$repo/tools/mkimage/Cargo.toml" -- "$kernel" "$out" "$@")
}
mkimage $kinds

if [ -n "$disk" ]; then
  for k in $kinds; do
    k=${k#--}
    "$python" "$repo/tools/mkdisk.py" "$out/veilos-$k-disk.img" --size "$disk" --boot "$out/veilos-$k.img"
  done
fi
if [ -n "$ramdisk" ]; then
  "$python" "$repo/tools/mkdisk.py" "$out/ramdisk.img" --size "$ramdisk"
  mkimage $kinds --ramdisk "$out/ramdisk.img"
fi
