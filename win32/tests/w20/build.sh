#!/bin/sh
# Build the W20 test (Rust, rustls with ring; crates from crates.io), copy it to
# storage/tests/, and record what it prints on Windows as the expected output,
# with w20srv.py running on 127.0.0.1 for the time.
# Run from anywhere: sh win32/tests/w20/build.sh
set -e
cd "$(dirname "$0")"
HERE=$(pwd)
OUT=../../../storage/tests
mkdir -p expected build "$OUT"
# Built outside the repository: its .cargo/config.toml (build-std, for the
# kernel) would apply to anything below it.
WORK="${TEMP:-/tmp}/veilos-w20tls"
rm -rf "$WORK/src" "$WORK/certs"
mkdir -p "$WORK"
cp -r w20tls/Cargo.toml w20tls/src "$WORK/"
mkdir -p "$WORK/../certs" && cp certs/ca.der "$WORK/../certs/"
(cd "$WORK" && cargo build --release --target x86_64-pc-windows-msvc)
cp "$WORK/target/x86_64-pc-windows-msvc/release/w20tls.exe" build/
cp build/w20tls.exe "$OUT/"

python w20srv.py > build/w20srv.log 2>&1 &
srv=$!
sleep 2
(cd build && timeout 60 ./w20tls.exe 127.0.0.1 >../expected/w20tls.out 2>&1; echo "exit $?" >>../expected/w20tls.out) || true
kill $srv 2>/dev/null || true
echo "built: w20tls.exe"
