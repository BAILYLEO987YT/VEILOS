#!/bin/sh
# Build K3, the display-only (KMDOD) test driver for VEILOS's kernel-driver host
# (WDDM_DESIGN.md, W3): k3dod.sys (MSVC against the WDK and displib.lib, as vendors build
# theirs) and its .inf. Copies them to
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
"$VC/bin/Hostx64/x64/cl.exe" /nologo /kernel /GS- /O2 /W3 /Zl /D_AMD64_ /D_WIN64 /DAMD64 /DNTDDI_VERSION=0x0A000010 /D_WIN32_WINNT=0x0A00 /c k3dod.c /Fobuild/k3dod.obj
"$LLVM/lld-link.exe" /nologo /DRIVER /SUBSYSTEM:NATIVE /ENTRY:DriverEntry /NODEFAULTLIB /MACHINE:X64 \
    build/k3dod.obj "/LIBPATH:$KITS/Lib/$VER/km/x64" ntoskrnl.lib hal.lib displib.lib BufferOverflowFastFailK.lib /OUT:build/k3dod.sys
cp build/k3dod.sys k3dod.inf "$OUT/"
echo "built: k3dod.sys"

