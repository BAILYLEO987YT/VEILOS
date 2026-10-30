#!/bin/sh
# Build the W3 tests with clang-cl + lld-link (no C runtime) and copy it
# to storage/tests/. Run from anywhere: sh win32/tests/w2/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
SDK="C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x64"
OUT=../../../storage/tests
mkdir -p build "$OUT"
for t in w3count w3echo; do
    "$LLVM/clang-cl.exe" /nologo /c /O1 /GS- /Zl /W3 $t.c /Fobuild/$t.obj
    "$LLVM/lld-link.exe" /nologo /NODEFAULTLIB /MACHINE:X64 /ENTRY:start /SUBSYSTEM:CONSOLE build/$t.obj "/LIBPATH:$SDK" kernel32.lib /OUT:build/$t.exe
    cp build/$t.exe "$OUT/"
done
echo "built: w3count.exe w3echo.exe"
