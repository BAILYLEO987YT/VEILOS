#!/bin/sh
# Build K1, the WDM test driver for VEILOS's kernel-driver host
# (WDDM_DESIGN.md, W1), with MSVC and lld-link against the Windows Driver
# Kit, and copy it to storage/tests/veiltest.sys. First, layout.c (with
# clang-cl) checks every structure offset src/ntos relies on against the WDK
# headers.
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

# The driver itself with MSVC, as vendors build theirs: clang-cl's __try
# only catches exceptions raised by calls (a fault in the __try body itself
# needs asynchronous SEH, which MSVC has and drivers rely on).
VC="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.51.36231"
"$VC/bin/Hostx64/x64/cl.exe" /nologo /kernel /GS- /O2 /W3 /Zl /D_AMD64_ /D_WIN64 /DAMD64 /DNTDDI_VERSION=0x0A000010 /D_WIN32_WINNT=0x0A00 /c veiltest.c /Fobuild/veiltest.obj
"$LLVM/lld-link.exe" /nologo /DRIVER /SUBSYSTEM:NATIVE /ENTRY:DriverEntry /NODEFAULTLIB /MACHINE:X64 \
    build/veiltest.obj "/LIBPATH:$KITS/Lib/$VER/km/x64" ntoskrnl.lib hal.lib ksecdd.lib /OUT:build/veiltest.sys
cp build/veiltest.sys "$OUT/"
echo "built: veiltest.sys"

# k1test.exe: the Windows program that opens \.\VeilTest (MSVC, /MD, like
# the other win32/tests). It needs the driver, so it has no Windows run to
# compare with.
KIT_VER=10.0.26100.0
unset INCLUDE
"$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- /D_CRT_SECURE_NO_WARNINGS \
    "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
    k1test.c /Fobuild/k1test.obj
"$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/k1test.obj "/LIBPATH:$VC/lib/x64" \
    "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" \
    msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib /OUT:build/k1test.exe
cp build/k1test.exe "$OUT/"
echo "built: k1test.exe"
