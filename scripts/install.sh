#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Stopping any running flky processes..."
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" ]]; then
    # Windows/MSYS2 environment
    taskkill //F //IM flky.exe //T 2>/dev/null || true
else
    pkill flky || true
fi

cargo install --path crates/flowkey-cli --locked --force

echo "flky installed with cargo install."
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" ]]; then
    echo "If Cargo's bin directory is not on your PATH, add %USERPROFILE%\\.cargo\\bin or the equivalent MSYS2 path."
else
    echo "If Cargo's bin directory is not on your PATH, add ~/.cargo/bin."
fi
