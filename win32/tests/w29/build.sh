#!/bin/sh
# Build the W29 (G5f) Direct3D 11 tests (MSVC, /MD) against the real import
# libraries, copy them to storage/tests/, and record on Windows what WARP
# (Windows' software Direct3D 11) makes (expected/dxN.ref, a PPM) and what
# each test prints against it (expected/dxN.out). Each also checks against
# this machine's GPU (`dxN gpu`). Run from anywhere: sh win32/tests/w29/build.sh
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

# dx2's shaders, compiled by the SDK's fxc into headers (the profile is in
# each entry point's name: vs40_ is vs_4_0, ps41_ ps_4_1).
FXC="$KITS/bin/$KIT_VER/x64/fxc.exe"
for e in vs40_main vs40_inst ps40_color ps40_tex ps40_math ps40_loop ps40_pos ps41_shadow ps40_flat ps40_index; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/dx2_$e.h /Vn $e dx2.hlsl >/dev/null
done

# dx3's.
for e in vs40_main ps40_cube ps40_cubelod ps40_vol ps40_volload ps40_arr ps40_arrload ps40_arr1 ps40_dims ps41_cubes; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/dx3_$e.h /Vn $e dx3.hlsl >/dev/null
done

# dx4's (cs50_ is cs_5_0).
for e in cs50_pattern cs50_buffers cs50_reduce cs50_hist vs50_main ps50_show; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/dx4_$e.h /Vn $e dx4.hlsl >/dev/null
done

# dx5's (gs40_ is gs_4_0).
for e in vs40_main vs40_turn ps40_main gs40_sprite gs40_explode gs40_adj; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/dx5_$e.h /Vn $e dx5.hlsl >/dev/null
done

# dx6's (hs50_ is hs_5_0, ds50_ ds_5_0).
for e in vs50_main hs50_tri hs50_trifrac hs50_quad ds50_tri ds50_quad ps50_main ps50_quad; do
    p=$(echo $e | sed 's/^\(..\)\(.\)\(.\)_.*/\1_\2_\3/')
    "$FXC" /nologo /T $p /E $e /Fh build/dx6_$e.h /Vn $e dx6.hlsl >/dev/null
done

TESTS="dx1 dx2 dx3 dx4 dx5 dx6"
for t in $TESTS; do
    "$VC/bin/Hostx64/x64/cl.exe" /nologo /c /O1 /MD /W3 /GS- /D_CRT_SECURE_NO_WARNINGS \
        "/I$VC/include" "/I$KITS/Include/$KIT_VER/ucrt" "/I$KITS/Include/$KIT_VER/um" "/I$KITS/Include/$KIT_VER/shared" \
        $t.c /Fobuild/$t.obj
    "$LLVM/lld-link.exe" /nologo /MACHINE:X64 /SUBSYSTEM:CONSOLE build/$t.obj "/LIBPATH:$VC/lib/x64" \
        "/LIBPATH:$KITS/Lib/$KIT_VER/ucrt/x64" "/LIBPATH:$KITS/Lib/$KIT_VER/um/x64" \
        msvcrt.lib vcruntime.lib ucrt.lib kernel32.lib user32.lib d3d11.lib dxgi.lib dxguid.lib /OUT:build/$t.exe
    cp build/$t.exe "$OUT/"
done

# On Windows: WARP's pixels, the run that compares with them, and the GPU's
# comparison.
for t in $TESTS; do
    (cd build && ./$t.exe record ../expected/$t.ref >/dev/null)
    cp expected/$t.ref build/$t.ref
    cp expected/$t.ref "$OUT/$t.ref"
    (cd build && ./$t.exe >../expected/$t.out 2>&1; echo "exit $?" >>../expected/$t.out) || true
    (cd build && ./$t.exe gpu | grep "panel" | grep -c ok | sed "s/^/$t on the GPU: panels ok: /")
done
echo "built: $TESTS"
