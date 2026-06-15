#!/usr/bin/env bash
# Cross-language comparator driver.
#
# Builds + runs the Rust / C / Go harnesses (identical
# timing methodology in each) and prints one combined
# table sorted by op + size.
#
# Usage: ./run.sh
set -euo pipefail
cd "$(dirname "$0")"

OPENSSL_PREFIX="$(brew --prefix openssl@3 2>/dev/null || echo /usr/local/opt/openssl@3)"
CSV="$(mktemp)"
trap 'rm -f "$CSV"' EXIT

echo "== building C (clang -O3 -mcpu=native, OpenSSL at $OPENSSL_PREFIX) ==" >&2
clang -O3 -mcpu=native -Wno-deprecated-declarations \
    -I"$OPENSSL_PREFIX/include" -L"$OPENSSL_PREFIX/lib" \
    c/bench.c -lcrypto -o c/bench

echo "== building Rust (cargo --release, target-cpu=native to match clang's -mcpu=native) ==" >&2
( cd .. && unset RUSTC_WRAPPER && RUSTFLAGS="-C target-cpu=native" cargo build --release -p stones-comparators 2>&1 | tail -1 ) >&2

echo "== running: Rust ==" >&2
../../target/release/bench-stones >> "$CSV"
echo "== running: C / OpenSSL ==" >&2
c/bench >> "$CSV"
echo "== running: Go ==" >&2
( cd go && go run bench.go ) >> "$CSV"

echo ""
echo "impl,op,size,ns_per_op,gib_s"
sort -t, -k2,2 -k3,3n -k5,5nr "$CSV"
