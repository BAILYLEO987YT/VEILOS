#!/bin/sh
# Build the W27 (G5d, G5e) Direct3D 9 tests (MSVC, /MD) against the real import
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

# d3d2's shaders, compiled by the SDK's fxc into headers (the profile is in
# each entry point's name: vs11_ is vs_1_1, vs2a_ vs_2_a).
FXC="$KITS/bin/$KIT_VER/x64/fxc.exe"
for e in vs11_lit vs20_loop vs20_skin vs20_wave vs11_fog vs11_points vs20_texgen vs2a_dyn vs20_plain vs20_misc vs20_call; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/d3d2_$e.h /Vn $e d3d2.hlsl >/dev/null
done

# d3d3's: vertex and pixel shaders.
for e in vs20_main vs20_proj vs20_fog vs30_main ps20_modulate ps20_light ps20_mix ps20_kill ps20_dependent ps20_proj ps20_bias ps20_math ps20_depth ps20_fog ps30_main ps30_branch; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/d3d3_$e.h /Vn $e d3d3.hlsl >/dev/null
done

# d3d5's.
for e in vs20_main ps20_cube ps30_lod; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/d3d5_$e.h /Vn $e d3d5.hlsl >/dev/null
done

# d3d4's ps_1_x shaders, assembled (fxc no longer emits ps_1_x).
python psasm.py d3d4.psa build/d3d4_shaders.h

for t in d3d1 d3d2 d3d3 d3d4 d3d5; do
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
for t in d3d1 d3d2 d3d3 d3d4 d3d5; do
    (cd build && ./$t.exe record ../expected/$t.ref >/dev/null)
    cp expected/$t.ref build/$t.ref
    cp expected/$t.ref "$OUT/$t.ref"
    (cd build && ./$t.exe >../expected/$t.out 2>&1; echo "exit $?" >>../expected/$t.out) || true
done
echo "built: d3d1.exe d3d2.exe d3d3.exe d3d4.exe d3d5.exe"
