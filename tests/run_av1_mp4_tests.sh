#!/usr/bin/env bash
set -eu
cd "$(dirname "$0")/.."
python3 tests/av1_mp4_tests.py "$@"
