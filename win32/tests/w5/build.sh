#!/bin/sh
# Build the W5 tests with clang-cl + lld-link against the real C runtime
# import libraries (/MD: vcruntime140.dll + ucrtbase), copy them to
# storage/tests/, and record what they print on Windows as the expected
# output. Run from anywhere: sh win32/tests/w5/build.sh
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
for t in w5crt; do
    "$LLVM/clang-cl.exe" /nologo /c /O1 /MD /W3 /Zc:inline \
        "/imsvc$VC/include" "/imsvc$KITS/Include/$KIT_VER/ucrt" "/imsvc$KITS/Include/$KIT_VER/um" "/imsvc$KITS/Include/$KIT_VER/shared" \
        $t.c /Fobuild/$t.obj
    "$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/$t.obj \
        "/LIBPATH:$VC/lib/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" \
        msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib /OUT:build/$t.exe
    cp build/$t.exe "$OUT/"
    # The same program on Windows: what VEILOS should print.
    (cd build && ./$t.exe one "two words" >../expected/$t.out 2>../expected/$t.err; echo "exit $?" >>../expected/$t.out) || true
done
echo "built: w5crt.exe"
