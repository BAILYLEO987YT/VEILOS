#!/bin/sh
# Build the W2 console test with clang-cl + lld-link (no C runtime) and copy it
# to storage/tests/. Run from anywhere: sh win32/tests/w2/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
SDK="C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x64"
OUT=../../../storage/tests
mkdir -p build "$OUT"
"$LLVM/clang-cl.exe" /nologo /c /O1 /GS- /Zl /W3 w2hello.c /Fobuild/w2hello.obj
"$LLVM/lld-link.exe" /nologo /NODEFAULTLIB /MACHINE:X64 /ENTRY:start /SUBSYSTEM:CONSOLE build/w2hello.obj "/LIBPATH:$SDK" kernel32.lib /OUT:build/w2hello.exe
cp build/w2hello.exe "$OUT/"
echo "built: w2hello.exe"
