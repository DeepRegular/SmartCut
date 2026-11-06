#!/usr/bin/env bash
set -eu
cd "$(dirname "$0")/.."
[ -x rust/target/release/examples/conformdiag ] || (cd rust && cargo build --release --locked --example conformdiag)
python3 tests/sar_tests.py "$@"
