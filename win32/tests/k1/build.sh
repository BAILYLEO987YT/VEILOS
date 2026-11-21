#!/bin/sh
# Build K1, the WDM test driver for VEILOS's kernel-driver host
# (WDDM_DESIGN.md, W1), with clang-cl and lld-link against the Windows
# Driver Kit, and copy it to storage/tests/veiltest.sys. First, layout.c
# checks every structure offset src/ntos relies on against the WDK headers.
# Needs the WDK (Microsoft.WindowsWDK.10.0.26100). Run from anywhere.
set -e
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
KITS="C:/Program Files (x86)/Windows Kits/10"
VER=10.0.26100.0
# clang-cl reads INCLUDE like cl.exe does (the paths have spaces).
export INCLUDE="$KITS/Include/$VER/km;$KITS/Include/$VER/km/crt;$KITS/Include/$VER/shared"
FLAGS="/nologo /kernel /GS- /O2 /W3 /Zl /D_AMD64_ /D_WIN64 /DAMD64 /D_KERNEL_MODE /DNTDDI_VERSION=0x0A000010 /D_WIN32_WINNT=0x0A00 -Wno-microsoft -Wno-pragma-pack -Wno-ignored-pragma-intrinsic -Wno-nonportable-include-path"
OUT=../../../storage/tests
mkdir -p build "$OUT"

# shellcheck disable=SC2086
"$LLVM/clang-cl.exe" $FLAGS /c layout.c /Fobuild/layout.obj
echo "layout: every offset matches the WDK"

# shellcheck disable=SC2086
"$LLVM/clang-cl.exe" $FLAGS /c veiltest.c /Fobuild/veiltest.obj
"$LLVM/lld-link.exe" /nologo /DRIVER /SUBSYSTEM:NATIVE /ENTRY:DriverEntry /NODEFAULTLIB /MACHINE:X64 \
    build/veiltest.obj "/LIBPATH:$KITS/Lib/$VER/km/x64" ntoskrnl.lib hal.lib /OUT:build/veiltest.sys
cp build/veiltest.sys "$OUT/"
echo "built: veiltest.sys"
