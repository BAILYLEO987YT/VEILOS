#!/bin/sh
# tools/run.sh [--uefi] [QEMU ARGS...] -- build VEILOS and boot it in QEMU
# (what `cargo run` did with bootimage). The disk is QEMU's view of storage/
# (fat:rw:, about 500 MB at most); serial output comes to this terminal.
#
# BIOS by default: SeaBIOS needs a VGA card with VBE for the boot stages, so
# the display is QEMU's standard VGA (VEILOS's Bochs driver takes it over).
# --uefi boots veilos-uefi.img with QEMU's own OVMF (edk2-x86_64-code.fd).
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
uefi=
if [ "$1" = --uefi ]; then uefi=1; shift; fi
sh "$repo/tools/build.sh" ${uefi:+--uefi} ${uefi:---bios}
out="$repo/target/x86_64-unknown-none/debug"
cd "$repo"

fw=
image="$out/veilos-bios.img"
display="-vga std"
if [ -n "$uefi" ]; then
  image="$out/veilos-uefi.img"
  display="-vga none -device bochs-display"
  qemu=$(command -v qemu-system-x86_64)
  for f in "$(dirname "$qemu")/share/edk2-x86_64-code.fd" "$(dirname "$qemu")/../share/qemu/edk2-x86_64-code.fd" \
           /usr/share/qemu/edk2-x86_64-code.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
    if [ -f "$f" ]; then fw=$f; break; fi
  done
  [ -n "$fw" ] || { echo "No OVMF firmware found (install QEMU's edk2 files or the ovmf package)." >&2; exit 1; }
  cp "$fw" "$out/ovmf-code.fd"
  fw="-drive if=pflash,format=raw,readonly=on,file=$out/ovmf-code.fd"
fi

# shellcheck disable=SC2086
exec qemu-system-x86_64 -machine q35 -cpu qemu64 -smp 4 -m 1G -accel tcg,thread=multi $fw \
    -drive format=raw,file="$image" \
    $display \
    -device usb-ehci,id=usb -device usb-tablet,bus=usb.0 \
    -drive id=disk,file=fat:rw:storage,if=none,format=raw \
    -device ahci,id=ahci -device ide-hd,drive=disk,bus=ahci.0 \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    -serial stdio "$@"
