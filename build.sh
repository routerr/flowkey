#!/usr/bin/env bash

# Flowkey Unified Build Script
# Supports: macOS (zsh/bash) and Windows (MSYS2 UCRT64)

set -euo pipefail

# Get the absolute path of the project root
PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

BUILD_TMP_DIR=""

cleanup_temp_dirs() {
    if [ -n "${BUILD_TMP_DIR:-}" ] && [ -d "$BUILD_TMP_DIR" ]; then
        rm -rf "$BUILD_TMP_DIR" 2>/dev/null || true
    fi
}

trap cleanup_temp_dirs EXIT

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
    # Terminate running instances to unlock files
    echo "Step 0: Checking for running Flowkey processes..."
    if taskkill //F //IM flowkey-gui.exe //IM flky.exe //IM flowkey.exe 2>/dev/null; then
        sleep 1
    fi

    # Inject common Windows paths if missing. Prioritize Winget Node.js to avoid MSYS2/Rolldown binding bugs.
    export PATH="/c/Users/user/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.14.0-win-x64:$HOME/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"

    # Use a fresh temp dir for installer artifacts only (not cargo target).
    BUILD_TMP_DIR="$(mktemp -d /tmp/flowkey_build_tmp.XXXXXX)"
    export TMPDIR="$BUILD_TMP_DIR"
    if command -v cygpath >/dev/null 2>&1; then
        BUILD_TMP_WIN="$(cygpath -w "$BUILD_TMP_DIR")"
        export TMP="$BUILD_TMP_WIN"
        export TEMP="$BUILD_TMP_WIN"
    else
        export TMP="$BUILD_TMP_DIR"
        export TEMP="$BUILD_TMP_DIR"
    fi

    # Clean stale bundle artifacts that can cause permission-denied errors
    # under MSYS2, but keep the rest of target/ for incremental compilation.
    rm -rf target/release/bundle 2>/dev/null || true

    NPM="npm.cmd"
    NPX="npx.cmd"
    TAURI_BIN="frontend/node_modules/.bin/tauri.cmd"
    TAURI_BUILD_ARGS=""
else
    NPM="npm"
    NPX="npx"
    TAURI_BIN="frontend/node_modules/.bin/tauri"
    if [ "$PLATFORM" == "macos" ]; then
        TAURI_BUILD_ARGS="--bundles app"
    else
        TAURI_BUILD_ARGS=""
    fi
fi

# 1. Install/Update Frontend Dependencies (skip if up to date)
echo "Step 1: Installing frontend dependencies..."
cd crates/flowkey-gui/frontend
if [ ! -d "node_modules" ] || [ "package.json" -nt "node_modules/.package-lock.json" ]; then
    $NPM install
else
    echo "  node_modules up to date, skipping npm install"
fi
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
    ./"$TAURI_BIN" build $TAURI_BUILD_ARGS
elif command -v $NPX &> /dev/null; then
    echo "Using npx to run Tauri CLI..."
    $NPX @tauri-apps/cli build $TAURI_BUILD_ARGS
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

elif [ "$PLATFORM" == "windows" ]; then
    TARGET_DIR="${CARGO_TARGET_DIR:-target}"

    # On Windows, look for .exe
    if [ -f "$TARGET_DIR/release/flowkey-gui.exe" ]; then
        cp "$TARGET_DIR/release/flowkey-gui.exe" dist/flowkey.exe
        echo "Portable executable created: dist/flowkey.exe"
    fi

    # Look for installer if generated
    SEARCH_DIR="$TARGET_DIR/release/bundle/msi"
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
