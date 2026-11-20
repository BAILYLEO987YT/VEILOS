#!/bin/sh
# Build the W33 AVX test (Rust, x86_64-pc-windows-msvc), copy it to
# storage/tests/avx1.exe, and record what it prints on Windows.
#
# Four threads each keep their own values in YMM1-YMM5 (all 256 bits) while
# they spin, then check them, so the kernel must save and restore the AVX
# state on every thread switch. Exit code 0: all intact; 1: some changed;
# 100: the OS hasn't enabled AVX (CPUID/XGETBV), nothing tested.
# Run with one CPU (-smp 1) to make the threads share it.
# Run from anywhere: sh win32/tests/w33/build.sh
set -e
cd "$(dirname "$0")"
OUT=../../../storage/tests
mkdir -p expected "$OUT"
CARGO_TARGET_DIR=build cargo build --release --target x86_64-pc-windows-msvc
cp build/x86_64-pc-windows-msvc/release/avx1.exe "$OUT/"
(timeout 60 ./build/x86_64-pc-windows-msvc/release/avx1.exe >expected/avx1.out 2>build/avx1.err; echo "exit $?" >>expected/avx1.out) || true
echo "built: avx1.exe"
