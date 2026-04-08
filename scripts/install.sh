#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Stopping any running flowkey processes..."
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" ]]; then
    # Windows/MSYS2 environment
    taskkill //F //IM flky.exe //T 2>/dev/null || true
    taskkill //F //IM flowkey-gui.exe //T 2>/dev/null || true
else
    pkill flky || true
    pkill flowkey-gui || true
fi

echo "Installing flowkey-cli..."
cargo install --path crates/flowkey-cli --locked --force

echo "Building and installing flowkey-gui..."
# Ensure frontend dependencies are installed and assets are built
(cd crates/flowkey-gui/frontend && npm install && npm run build)
cargo install --path crates/flowkey-gui --locked --force

echo "flowkey installed successfully."
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" ]]; then
    echo "If Cargo's bin directory is not on your PATH, add %USERPROFILE%\\.cargo\\bin or the equivalent MSYS2 path."
else
    echo "If Cargo's bin directory is not on your PATH, add ~/.cargo/bin."
fi
