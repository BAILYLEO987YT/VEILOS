#!/bin/sh
# Build VEILOS's Windows system DLLs and install them into storage/system.
# Run from anywhere: sh win32/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
OUT=target/x86_64-pc-windows-msvc/release
cargo build --release
mkdir -p ../storage/system
for dll in ntdll kernel32 ucrtbase; do
    cp "$OUT/$dll.dll" "../storage/system/$dll.dll"
done

# msvcrt.dll and vcruntime140(_1).dll: forwarders to ucrtbase.dll, made
# from its export list (no code, no entry point).
mkdir -p target/forwarders
python crt_forwarders.py "$(cygpath -w "$LLVM/llvm-objdump.exe")" "$OUT/ucrtbase.dll" target/forwarders
echo "" > target/forwarders/empty.c
"$LLVM/clang-cl.exe" /nologo /c target/forwarders/empty.c /Fotarget/forwarders/empty.obj
for dll in msvcrt vcruntime140 vcruntime140_1; do
    "$LLVM/lld-link.exe" /nologo /DLL /NOENTRY /NODEFAULTLIB /MACHINE:X64 \
        /DEF:target/forwarders/$dll.def target/forwarders/empty.obj /OUT:target/forwarders/$dll.dll
    cp target/forwarders/$dll.dll "../storage/system/$dll.dll"
done
echo "installed: $(ls ../storage/system | tr '\n' ' ')"
