# Flowkey Unified Build Script for Windows (PowerShell)
# Supports: Windows (PowerShell 5.1 or 7+)

$ErrorActionPreference = "Stop"

# Get the absolute path of the project root
$PROJECT_ROOT = $PSScriptRoot
if (-not $PROJECT_ROOT) {
    $PROJECT_ROOT = Get-Location
}
Set-Location $PROJECT_ROOT

Write-Host "--- Building Flowkey (Windows) ---" -ForegroundColor Cyan

# 0. Terminate any running Flowkey processes to unlock files
Write-Host "Step 0: Checking for running Flowkey processes..." -ForegroundColor Green
$processes = Get-Process "flowkey-gui", "flky" -ErrorAction SilentlyContinue
if ($processes) {
    Write-Host "Closing running instances..." -ForegroundColor Yellow
    $processes | Stop-Process -Force
    # Give Windows a moment to release the file handles
    Start-Sleep -Seconds 1
}

# 1. Install/Update Frontend Dependencies
Write-Host "Step 1: Installing frontend dependencies..." -ForegroundColor Green
Set-Location "crates/flowkey-gui/frontend"
& npm install
Set-Location $PROJECT_ROOT

# 2. Build Frontend
Write-Host "Step 2: Building frontend..." -ForegroundColor Green
Set-Location "crates/flowkey-gui/frontend"
& npm run build
Set-Location $PROJECT_ROOT

# 3. Build Rust Application (Tauri + CLI)
Write-Host "Step 3: Building Rust applications (Release)..." -ForegroundColor Green

# Build GUI with Tauri
Set-Location "crates/flowkey-gui"
$TAURI_CLI = "frontend/node_modules/.bin/tauri.exe"
if (-not (Test-Path $TAURI_CLI)) {
    $TAURI_CLI = "frontend/node_modules/.bin/tauri" # for some node environments
}

if (Test-Path $TAURI_CLI) {
    Write-Host "Using local Tauri CLI to build..." -ForegroundColor Cyan
    & $TAURI_CLI build
} else {
    Write-Host "Tauri CLI not found locally, trying npx..." -ForegroundColor Yellow
    & npx @tauri-apps/cli build
}
Set-Location $PROJECT_ROOT

# Build CLI separately
Write-Host "Building CLI application..." -ForegroundColor Cyan
& cargo build -p flowkey-cli --release

# 4. Collect Artifacts into dist/
Write-Host "Step 4: Collecting artifacts..." -ForegroundColor Green
if (Test-Path "dist") {
    Remove-Item "dist" -Recurse -Force
}
New-Item -ItemType Directory -Force -Path "dist" | Out-Null

# Copy GUI executable
if (Test-Path "target\release\flowkey-gui.exe") {
    Copy-Item "target\release\flowkey-gui.exe" "dist\flowkey-gui.exe"
    Write-Host "GUI Executable: dist\flowkey-gui.exe"
}

# Copy MSI installer if generated
$MSI_DIR = "target\release\bundle\msi"
if (Test-Path $MSI_DIR) {
    $MSI_PATH = Get-ChildItem -Path $MSI_DIR -Filter "*.msi" | Select-Object -First 1
    if ($MSI_PATH) {
        Copy-Item $MSI_PATH.FullName "dist\"
        Write-Host "MSI Installer: dist\$($MSI_PATH.Name)"
    }
}

# Copy CLI binary
if (Test-Path "target\release\flky.exe") {
    Copy-Item "target\release\flky.exe" "dist\flky.exe"
    Write-Host "CLI Binary: dist\flky.exe"
}

# 5. Create a convenience launcher in the root (batch file)
$LAUNCHER_CONTENT = @"
@echo off
if exist "dist\flowkey-gui.exe" (
    start "" "dist\flowkey-gui.exe"
) else if exist "dist\flky.exe" (
    "dist\flky.exe" %*
) else (
    echo Application not built. Run powershell ./build.ps1 first.
    pause
)
"@
$LAUNCHER_CONTENT | Set-Content "flowkey.bat"

Write-Host "--- Build Complete ---" -ForegroundColor Cyan
Write-Host "Artifacts are in the 'dist\' directory."
Write-Host "You can launch the GUI using: .\flowkey.bat"
