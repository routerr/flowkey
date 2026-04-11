#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

arch="$(uname -m)"
case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
esac

gui_dir="crates/flowkey-gui"
frontend_dir="$gui_dir/frontend"
tauri_bin="frontend/node_modules/.bin/tauri"
search_dir="target/release/bundle/macos"
bundle_root="dist/flowkey-macos-${arch}"
archive_path="dist/flowkey-macos-${arch}.dmg"

cd "$frontend_dir"
npm install
npm run build
cd ../..

cd "$gui_dir"
if [[ -x "$tauri_bin" ]]; then
    "./$tauri_bin" build --bundles app
else
    npx @tauri-apps/cli build --bundles app
fi
cd ../..

rm -rf "$bundle_root" "$archive_path" "${archive_path}.sha256"
mkdir -p "$bundle_root"

app_source="$(find "$search_dir" -maxdepth 1 -name "*.app" -type d | head -n 1)"
if [[ -z "$app_source" ]]; then
    echo "no macOS app bundle found in $search_dir" >&2
    exit 1
fi

app_bundle="$bundle_root/$(basename "$app_source")"
cp -R "$app_source" "$app_bundle"

sign_identity="${FLKY_MACOS_SIGN_IDENTITY:-}"
notary_apple_id="${FLKY_MACOS_NOTARY_APPLE_ID:-}"
notary_password="${FLKY_MACOS_NOTARY_PASSWORD:-}"
notary_team_id="${FLKY_MACOS_NOTARY_TEAM_ID:-}"

if [[ -n "$sign_identity" ]]; then
    echo "signing macOS bundle with identity: $sign_identity"
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
