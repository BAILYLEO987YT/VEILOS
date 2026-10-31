#!/bin/sh
# Build the W10 (G3a) tests (MSVC, /MD) against the real import libraries, copy
# them to storage/tests/, and record what they print on Windows as the
# expected output. Run from anywhere: sh win32/tests/w10/build.sh
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
"$KITS/bin/$KIT_VER/x64/rc.exe" /nologo "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" /fo build/w10dlg.res w10dlg.rc

for t in w10dlg; do
    "$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- /D_CRT_SECURE_NO_WARNINGS \
        "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
        $t.c /Fobuild/$t.obj
    "$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/$t.obj "/LIBPATH:$VC/lib/x64" \
        "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" \
        msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib user32.lib gdi32.lib msimg32.lib build/$t.res /OUT:build/$t.exe
    cp build/$t.exe "$OUT/"
done

# The same programs on Windows: what VEILOS should print.
for t in w10dlg; do
    (cd build && ./$t.exe >../expected/$t.out 2>&1; echo "exit $?" >>../expected/$t.out) || true
done
echo "built: w10dlg.exe"
