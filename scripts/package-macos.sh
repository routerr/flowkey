#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -p flowkey-cli --release

arch="$(uname -m)"
case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
esac

bundle_root="dist/flky-macos-${arch}"
app_bundle="$bundle_root/flky.app"
contents_dir="$app_bundle/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
archive_path="dist/flky-macos-${arch}.dmg"

rm -rf "$bundle_root" "$archive_path" "${archive_path}.sha256"
mkdir -p "$macos_dir" "$resources_dir"

cp "target/release/flky" "$macos_dir/flky"
chmod +x "$macos_dir/flky"

cat > "$contents_dir/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>flky</string>
    <key>CFBundleDisplayName</key>
    <string>flowkey</string>
    <key>CFBundleIdentifier</key>
    <string>dev.flowkey.flky</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleExecutable</key>
    <string>flky</string>
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
Open Terminal and run the `flky` binary from this app bundle or move it onto your PATH.
For a Cargo-based install, run the bundled `install.sh`.
The binary reads config from the platform-specific application data directory
unless `FLKY_CONFIG` is set.
EOF

sign_identity="${FLKY_MACOS_SIGN_IDENTITY:-}"
notary_apple_id="${FLKY_MACOS_NOTARY_APPLE_ID:-}"
notary_password="${FLKY_MACOS_NOTARY_PASSWORD:-}"
notary_team_id="${FLKY_MACOS_NOTARY_TEAM_ID:-}"

if [[ -n "$sign_identity" ]]; then
    echo "signing macOS bundle with identity: $sign_identity"
    codesign --force --options runtime --timestamp --sign "$sign_identity" "$macos_dir/flky"
    codesign --force --options runtime --timestamp --sign "$sign_identity" "$app_bundle"
fi

hdiutil create -volname flowkey -srcfolder "$app_bundle" -ov -format UDZO "$archive_path"
shasum -a 256 "$archive_path" | awk '{print $1 "  " $2}' > "${archive_path}.sha256"

if [[ -n "$notary_apple_id" || -n "$notary_password" || -n "$notary_team_id" ]]; then
    if [[ -z "$sign_identity" || -z "$notary_apple_id" || -z "$notary_password" || -z "$notary_team_id" ]]; then
        echo "macOS notarization requires FLKY_MACOS_SIGN_IDENTITY, FLKY_MACOS_NOTARY_APPLE_ID, FLKY_MACOS_NOTARY_PASSWORD, and FLKY_MACOS_NOTARY_TEAM_ID" >&2
        exit 1
    fi

    echo "submitting macOS dmg to Apple notarization"
    xcrun notarytool submit "$archive_path" \
        --apple-id "$notary_apple_id" \
        --password "$notary_password" \
        --team-id "$notary_team_id" \
        --wait
    xcrun stapler staple "$archive_path"
fi

echo "created $archive_path"
