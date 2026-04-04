#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -p kms-cli --release

arch="$(uname -m)"
case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
esac

bundle_root="dist/kms-macos-${arch}"
app_bundle="$bundle_root/kms.app"
contents_dir="$app_bundle/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
archive_path="dist/kms-macos-${arch}.dmg"

rm -rf "$bundle_root" "$archive_path" "${archive_path}.sha256"
mkdir -p "$macos_dir" "$resources_dir"

cp "target/release/kms" "$macos_dir/kms"
chmod +x "$macos_dir/kms"

cat > "$contents_dir/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>kms</string>
    <key>CFBundleDisplayName</key>
    <string>kms</string>
    <key>CFBundleIdentifier</key>
    <string>com.key-mouse-sharer.kms</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleExecutable</key>
    <string>kms</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
</dict>
</plist>
EOF

cp README.md "$resources_dir/README.md"
cp docs/protocol.md "$resources_dir/protocol.md"
cp docs/architecture.md "$resources_dir/architecture.md"
cp scripts/install.sh "$resources_dir/install.sh"
chmod +x "$resources_dir/install.sh"

cat > "$resources_dir/INSTALL.txt" <<'EOF'
Open Terminal and run the `kms` binary from this app bundle or move it onto your PATH.
For a Cargo-based install, run the bundled `install.sh`.
The binary reads config from the platform-specific application data directory
unless `KMS_CONFIG` is set.
EOF

hdiutil create -volname kms -srcfolder "$app_bundle" -ov -format UDZO "$archive_path"
shasum -a 256 "$archive_path" | awk '{print $1 "  " $2}' > "${archive_path}.sha256"

echo "created $archive_path"
