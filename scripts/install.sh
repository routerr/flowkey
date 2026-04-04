#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo install --path crates/kms-cli --locked --force

echo "kms installed with cargo install."
echo "If Cargo's bin directory is not on your PATH, add ~/.cargo/bin."
