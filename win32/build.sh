#!/bin/sh
# Build VEILOS's Windows system DLLs and install them into storage/system.
# Run from anywhere: sh win32/build.sh
set -e
cd "$(dirname "$0")"
cargo build --release
mkdir -p ../storage/system
for dll in ntdll kernel32; do
    cp "target/x86_64-pc-windows-msvc/release/$dll.dll" "../storage/system/$dll.dll"
done
echo "installed: $(ls ../storage/system | tr '\n' ' ')"
