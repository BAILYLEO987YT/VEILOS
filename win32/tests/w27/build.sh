#!/bin/sh
# Build the W27 (G5d) Direct3D 9 test (MSVC, /MD) against the real import
# libraries, copy it to storage/tests/, and record on Windows what this
# machine's GPU draws (expected/d3d1.ref, a PPM) and what the test prints
# against it (expected/d3d1.out). Run from anywhere: sh win32/tests/w27/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
KITS="C:/Program Files (x86)/Windows Kits/10"
KIT_VER=10.0.26100.0
VC="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.51.36231"
OUT=../../../storage/tests
mkdir -p build expected "$OUT"

for t in d3d1; do
    "$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- /D_CRT_SECURE_NO_WARNINGS \
        "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
        $t.c /Fobuild/$t.obj
    "$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/$t.obj "/LIBPATH:$VC/lib/x64" \
        "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" \
        msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib user32.lib d3d9.lib /OUT:build/$t.exe
    cp build/$t.exe "$OUT/"
done

# On Windows: the reference pixels (this machine's GPU), then the run that
# compares with them.
for t in d3d1; do
    (cd build && ./$t.exe record ../expected/$t.ref >/dev/null)
    cp expected/$t.ref build/$t.ref
    cp expected/$t.ref "$OUT/$t.ref"
    (cd build && ./$t.exe >../expected/$t.out 2>&1; echo "exit $?" >>../expected/$t.out) || true
done
echo "built: d3d1.exe"
