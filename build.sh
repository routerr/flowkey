#!/usr/bin/env bash

# Flowkey Unified Build Script
# Supports: macOS (zsh/bash) and Windows (MSYS2 UCRT64)

set -euo pipefail

# Get the absolute path of the project root
PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

echo "--- Building Flowkey ---"

# Detect OS
OS_NAME="$(uname -s)"
case "$OS_NAME" in
    Darwin*)  PLATFORM="macos" ;;
    MSYS*|MINGW*|CYGWIN*) PLATFORM="windows" ;;
    *)        PLATFORM="linux" ;;
esac

echo "Platform detected: $PLATFORM"

if [ "$PLATFORM" == "windows" ]; then
    # Inject common Windows paths if missing. Prioritize Winget Node.js to avoid MSYS2/Rolldown binding bugs.
    export PATH="/c/Users/user/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.14.0-win-x64:$HOME/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"
    NPM="npm.cmd"
    NPX="npx.cmd"
    TAURI_BIN="frontend/node_modules/.bin/tauri.cmd"
else
    NPM="npm"
    NPX="npx"
    TAURI_BIN="frontend/node_modules/.bin/tauri"
fi

# 1. Install/Update Frontend Dependencies
echo "Step 1: Installing frontend dependencies..."
cd crates/flowkey-gui/frontend
$NPM install
cd ../../..

# 2. Build Frontend
echo "Step 2: Building frontend..."
cd crates/flowkey-gui/frontend
$NPM run build
cd ../../..

# 3. Build Rust Application (Tauri + Core)
echo "Step 3: Building Rust application (Release)..."
# We use the local tauri cli in frontend/node_modules if it exists
cd crates/flowkey-gui

if [ -f "$TAURI_BIN" ]; then
    echo "Using local Tauri CLI to build..."
    ./"$TAURI_BIN" build
elif command -v $NPX &> /dev/null; then
    echo "Using npx to run Tauri CLI..."
    $NPX @tauri-apps/cli build
else
    echo "Tauri CLI not found, falling back to manual cargo build..."
    cargo build --release
fi
cd ../..

# 4. Collect Artifacts into dist/
echo "Step 4: Collecting artifacts..."
rm -rf dist
mkdir -p dist

if [ "$PLATFORM" == "macos" ]; then
    # Find the built .app
    # Tauri 1.x usually puts it in target/release/bundle/macos/
    SEARCH_DIR="target/release/bundle/macos"
    if [ -d "$SEARCH_DIR" ]; then
        APP_PATH=$(find "$SEARCH_DIR" -maxdepth 1 -name "*.app" -type d | head -n 1)
        if [ -n "$APP_PATH" ]; then
            echo "Packaging macOS App: $APP_PATH"
            cp -R "$APP_PATH" dist/
            echo "Portable App: dist/$(basename "$APP_PATH")"
        fi
    fi
    
    # Also copy raw binary as a fallback
    if [ -f "target/release/flowkey-gui" ]; then
        cp "target/release/flowkey-gui" dist/flowkey
        chmod +x dist/flowkey
        echo "Portable Binary: dist/flowkey"
    fi

elif [ "$PLATFORM" == "windows" ]; then
    # On Windows, look for .exe
    if [ -f "target/release/flowkey-gui.exe" ]; then
        cp "target/release/flowkey-gui.exe" dist/flowkey.exe
        echo "Portable executable created: dist/flowkey.exe"
    fi
    
    # Look for installer if generated
    SEARCH_DIR="target/release/bundle/msi"
    if [ -d "$SEARCH_DIR" ]; then
        MSI_PATH=$(find "$SEARCH_DIR" -maxdepth 1 -name "*.msi" | head -n 1)
        if [ -n "$MSI_PATH" ]; then
            cp "$MSI_PATH" dist/
            echo "Installer: dist/$(basename "$MSI_PATH")"
        fi
    fi
fi

# 5. Create a convenience launcher in the root
cat > flowkey <<EOF
#!/usr/bin/env bash
if [ -d "dist/flowkey.app" ]; then
    open dist/flowkey.app
elif [ -f "dist/flowkey" ]; then
    ./dist/flowkey
elif [ -f "dist/flowkey.exe" ]; then
    ./dist/flowkey.exe
else
    echo "Application not built. Run ./build.sh first."
fi
EOF
chmod +x flowkey

echo "--- Build Complete ---"
echo "Artifacts are in the 'dist/' directory."
echo "You can launch the app using: ./flowkey"
