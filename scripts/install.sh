#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo install --path crates/flowkey-cli --locked --force

echo "flky installed with cargo install."
echo "If Cargo's bin directory is not on your PATH, add ~/.cargo/bin."
