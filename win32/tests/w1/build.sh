#!/bin/sh
# Build the W1 loader tests with clang-cl + lld-link (no C runtime) and copy
# them to storage/tests/. Run from anywhere: sh win32/tests/w1/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
SDK="C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x64"
OUT=../../../storage/tests
mkdir -p build "$OUT"
CFLAGS="/nologo /c /O1 /GS- /Zl /W3"
LFLAGS="/nologo /NODEFAULTLIB /MACHINE:X64"
"$LLVM/clang-cl.exe" $CFLAGS w1dep.c /Fobuild/w1dep.obj
"$LLVM/clang-cl.exe" $CFLAGS w1lib.c /Fobuild/w1lib.obj
"$LLVM/clang-cl.exe" $CFLAGS w1test.c /Fobuild/w1test.obj
"$LLVM/clang-cl.exe" $CFLAGS w1ret.c /Fobuild/w1ret.obj
"$LLVM/llvm-lib.exe" /nologo /MACHINE:X64 /DEF:missing.def /OUT:build/missing.lib
"$LLVM/llvm-lib.exe" /nologo /MACHINE:X64 /DEF:apiset.def /OUT:build/apiset.lib
"$LLVM/lld-link.exe" $LFLAGS /DLL /ENTRY:DllMain build/w1dep.obj "/LIBPATH:$SDK" kernel32.lib /OUT:build/w1dep.dll /IMPLIB:build/w1dep.lib
"$LLVM/lld-link.exe" $LFLAGS /DLL /ENTRY:DllMain /DEF:w1lib.def build/w1lib.obj build/w1dep.lib "/LIBPATH:$SDK" kernel32.lib /OUT:build/w1lib.dll /IMPLIB:build/w1lib.lib
"$LLVM/lld-link.exe" $LFLAGS /ENTRY:start /SUBSYSTEM:CONSOLE build/w1test.obj build/w1lib.lib build/missing.lib build/apiset.lib "/LIBPATH:$SDK" kernel32.lib /OUT:build/w1test.exe
"$LLVM/lld-link.exe" $LFLAGS /ENTRY:start /SUBSYSTEM:CONSOLE build/w1ret.obj /OUT:build/w1ret.exe
cp build/w1dep.dll build/w1lib.dll build/w1test.exe build/w1ret.exe "$OUT/"
echo "built: $(ls "$OUT")"
