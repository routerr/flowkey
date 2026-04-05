#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -p flowkey-cli --release

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
esac

stage_dir="dist/flky-${os}-${arch}"
archive_path="dist/flky-${os}-${arch}.tar.gz"
rm -rf "$stage_dir" "$archive_path" "${archive_path}.sha256"
mkdir -p "$stage_dir"

cp "target/release/flky" "$stage_dir/flky"
cp README.md "$stage_dir/README.md"
cp docs/protocol.md "$stage_dir/protocol.md"
cp docs/architecture.md "$stage_dir/architecture.md"
cp scripts/install.sh "$stage_dir/install.sh"
chmod +x "$stage_dir/install.sh"

cat > "$stage_dir/INSTALL.txt" <<'EOF'
Run the `flky` binary from this folder or move it onto your PATH.
For a Cargo-based install, run `./install.sh`.
The binary reads config from the platform-specific application data directory
unless `FLKY_CONFIG` is set.
EOF

tar -C dist -czf "$archive_path" "$(basename "$stage_dir")"
shasum -a 256 "$archive_path" | awk '{print $1 "  " $2}' > "${archive_path}.sha256"

echo "created $archive_path"
