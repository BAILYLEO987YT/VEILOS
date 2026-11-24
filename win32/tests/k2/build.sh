#!/bin/sh
# Build K2, the PnP test driver for VEILOS's kernel-driver host
# (WDDM_DESIGN.md, W2): k2edu.sys (MSVC against the WDK, as vendors build
# theirs) for QEMU's "edu" device, its .inf, and k2test.exe. Copies them to
# storage/tests/. The structure offsets src/nt/ntos uses are checked by
# ../k1/build.sh (layout.c). Needs the WDK (10.0.26100). Run from anywhere.
set -e
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
KITS="C:/Program Files (x86)/Windows Kits/10"
VER=10.0.26100.0
VC="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.51.36231"
OUT=../../../storage/tests
mkdir -p build "$OUT"

export INCLUDE="$KITS/Include/$VER/km;$KITS/Include/$VER/km/crt;$KITS/Include/$VER/shared"
"$VC/bin/Hostx64/x64/cl.exe" /nologo /kernel /GS- /O2 /W3 /Zl /D_AMD64_ /D_WIN64 /DAMD64 /DNTDDI_VERSION=0x0A000010 /D_WIN32_WINNT=0x0A00 /c k2edu.c /Fobuild/k2edu.obj
"$LLVM/lld-link.exe" /nologo /DRIVER /SUBSYSTEM:NATIVE /ENTRY:DriverEntry /NODEFAULTLIB /MACHINE:X64 \
    build/k2edu.obj "/LIBPATH:$KITS/Lib/$VER/km/x64" ntoskrnl.lib hal.lib /OUT:build/k2edu.sys
cp build/k2edu.sys k2edu.inf "$OUT/"
echo "built: k2edu.sys"

unset INCLUDE
"$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- /D_CRT_SECURE_NO_WARNINGS \
    "/I$VC/include" "/I$KITS/Include/$VER/ucrt" "/I$KITS/Include/$VER/um" "/I$KITS/Include/$VER/shared" \
    k2test.c /Fobuild/k2test.obj
"$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/k2test.obj "/LIBPATH:$VC/lib/x64" \
    "/LIBPATH:$KITS/Lib/$VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$VER/um/x64" \
    msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib /OUT:build/k2test.exe
cp build/k2test.exe "$OUT/"
echo "built: k2test.exe"
