#!/bin/sh
# Start VEILOS in QEMU on Linux or macOS:  sh run.sh
# Needs QEMU:  Linux   sudo apt install qemu-system-x86   (or your distro's package)
#              macOS   brew install qemu
cd "$(dirname "$0")" || exit 1

QEMU=$(command -v qemu-system-x86_64) || {
    echo "QEMU isn't installed. Linux: sudo apt install qemu-system-x86   macOS: brew install qemu"
    exit 1
}

# Emulated by default: VEILOS currently hangs before the kernel starts under
# KVM (as it sometimes does under Windows' WHPX). To try KVM on Linux anyway:
#   VEILOS_ACCEL=kvm sh run.sh
ACCEL="-accel tcg,thread=multi"
if [ "${VEILOS_ACCEL:-}" = kvm ] && [ "$(uname -s)" = Linux ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL="-accel kvm $ACCEL"
fi

echo "Starting VEILOS. The first boot can take a minute or two."
echo 'Log in as "admin" with password "veilos" (or "root" with no password).'
# shellcheck disable=SC2086
exec "$QEMU" -cpu qemu64 -smp 4 -m 1G $ACCEL \
    -drive format=raw,file=veilos-boot.img \
    -vga none -device bochs-display \
    -device usb-ehci,id=usb -device usb-tablet,bus=usb.0 \
    -drive id=disk,file=veilos-storage.img,if=none,format=raw \
    -device ahci,id=ahci -device ide-hd,drive=disk,bus=ahci.0 \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    -serial file:serial.log \
    "$@"
