#!/bin/sh
# tools/build.sh [--release] [--bios|--uefi] -- build the kernel and its boot
# disk images (bootloader 0.11; this replaces `cargo bootimage`):
#
#   target/x86_64-unknown-none/<profile>/veilos-bios.img   BIOS (SeaBIOS, CSM)
#   target/x86_64-unknown-none/<profile>/veilos-uefi.img   UEFI (OVMF, PCs)
#
# The image builder (tools/mkimage) is a host program run from outside the
# repo, so the kernel's .cargo/config.toml (bare-metal target, build-std)
# doesn't apply to it. It needs a nightly from 2026-07-10 or later: the
# bootloader's own stages pin x86_64 0.15.5 (MKIMAGE_TOOLCHAIN overrides).
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
profile=debug
cargo_profile=
images=
for a in "$@"; do
  case $a in
    --release) profile=release; cargo_profile=--release ;;
    --bios|--uefi) images="$images $a" ;;
    *) echo "usage: tools/build.sh [--release] [--bios|--uefi]" >&2; exit 2 ;;
  esac
done
toolchain=${MKIMAGE_TOOLCHAIN:-nightly-2026-11-01}

(cd "$repo" && cargo build $cargo_profile)
out="$repo/target/x86_64-unknown-none/$profile"
# The firmware reads the kernel through slow disk services: leave the debug
# info (most of a debug build) behind. The unstripped ELF stays for gdb.
objcopy=$(ls "$(cd "$repo" && rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy* | head -1)
kernel="$out/VEIL-OS.boot"
"$objcopy" --strip-debug "$out/VEIL-OS" "$kernel"

cd "${TMPDIR:-/tmp}"
cargo "+$toolchain" run --quiet --release --manifest-path "$repo/tools/mkimage/Cargo.toml" -- "$kernel" "$out" $images
