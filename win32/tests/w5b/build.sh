#!/bin/sh
# Build the W5b exception tests against the real C runtime import libraries
# (/MD), copy them to storage/tests/, and record what they print on Windows
# as the expected output. w5cpp is built twice: by clang-cl (FH3 metadata,
# __CxxFrameHandler3) and by MSVC's cl (FH4, __CxxFrameHandler4).
# Run from anywhere: sh win32/tests/w5b/build.sh
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

link() { # link OBJ EXE
    "$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE "$1" "/LIBPATH:$VC/lib/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" \
        "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib "/OUT:$2"
}

# The same program on Windows: what VEILOS should print (and its exit code).
expect() {
    (cd build && ./$1.exe >../expected/$1.out 2>&1; echo "exit $?" >>../expected/$1.out) || true
}

# MSVC: faults inside a __try body are caught there (clang needs /EHa).
"$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- \
    "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
    w5seh.c /Fobuild/w5seh.obj
link build/w5seh.obj build/w5seh.exe

"$LLVM/clang-cl.exe" /nologo /c /O1 /MD /EHsc /W3 /GS- \
    "/imsvc$VC/include" "/imsvc$KITS/Include/$KIT_VER/ucrt" "/imsvc$KITS/Include/$KIT_VER/um" "/imsvc$KITS/Include/$KIT_VER/shared" \
    /D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH w5cpp.cpp /Fobuild/w5cpp.obj
link build/w5cpp.obj build/w5cpp.exe

"$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /EHsc /W3 /GS- \
    "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
    w5cpp.cpp /Fobuild/w5cpp4.obj
link build/w5cpp4.obj build/w5cpp4.exe

for t in w5seh w5cpp w5cpp4; do
    cp build/$t.exe "$OUT/"
    expect $t
done
echo "built: w5seh.exe w5cpp.exe w5cpp4.exe"
