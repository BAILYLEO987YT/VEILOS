#!/usr/bin/env bash
# boot_test.sh DIR [kvm|tcg] -- boot the unzipped release in DIR headless and
# check it reaches the desktop. Used by .github/workflows/release.yml.
# Exit 0 when it booted; otherwise prints why, with the serial log, as a
# GitHub annotation (readable without signing in).
set -u
dir=$1
mode=${2:-kvm}
cd "$dir" || exit 2
rm -f serial.log qemu.out

if [ "$mode" = tcg ]; then export VEILOS_ACCEL=tcg; fi
sh run.sh -display none > qemu.out 2>&1 &
qemu=$!

result=timeout
for i in $(seq 1 60); do
    sleep 5
    if ! kill -0 "$qemu" 2>/dev/null; then result=qemu-exited; break; fi
    if grep -q 'KERNEL PANIC' serial.log 2>/dev/null; then result=panic; break; fi
    if grep -q 'gui_refresh_task started' serial.log 2>/dev/null; then result=booted; break; fi
done
kill "$qemu" 2>/dev/null
wait "$qemu" 2>/dev/null

size=$(wc -c < serial.log 2>/dev/null || echo 0)
echo "[$mode] result=$result after ~$((i * 5))s, serial log $size bytes"
cat qemu.out

if [ "$result" = booted ] && grep -q '^\[FAT\] FAT32' serial.log; then
    echo "::notice::VEILOS booted to the desktop under $mode in about $((i * 5))s."
    exit 0
fi
[ "$result" = booted ] && result=no-storage-disk

# Annotations take one line: encode %, CR and LF.
log=$( { echo "QEMU said:"; tail -5 qemu.out; echo "Serial log, first 15 lines:"; head -15 serial.log 2>/dev/null; echo "...last 25 lines:"; tail -25 serial.log 2>/dev/null; } | cut -c1-200)
log=${log//'%'/'%25'}
log=${log//$'\r'/}
log=${log//$'\n'/'%0A'}
echo "::warning title=Boot test under $mode failed ($result, $size bytes of serial)::$log"
exit 1
