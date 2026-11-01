#!/usr/bin/env bash
set -eu
cd "$(dirname "$0")/.."
python3 tests/container_contract_tests.py
