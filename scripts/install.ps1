Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot

Write-Host "Stopping any running flowkey processes..."
Stop-Process -Name "flky" -ErrorAction SilentlyContinue
Stop-Process -Name "flowkey-gui" -ErrorAction SilentlyContinue

Write-Host "Installing flowkey-cli..."
& cargo install --path crates/flowkey-cli --locked --force
if ($LASTEXITCODE -ne 0) {
    throw "cargo install for flowkey-cli failed with exit code $LASTEXITCODE"
}

Write-Host "Building and installing flowkey-gui..."
# Ensure frontend dependencies are installed and assets are built
Push-Location crates/flowkey-gui/frontend
& npm install
if ($LASTEXITCODE -ne 0) { throw "npm install failed" }
& npm run build
if ($LASTEXITCODE -ne 0) { throw "npm run build failed" }
Pop-Location

& cargo install --path crates/flowkey-gui --locked --force
if ($LASTEXITCODE -ne 0) {
    throw "cargo install for flowkey-gui failed with exit code $LASTEXITCODE"
}

Write-Host "flowkey installed successfully."
Write-Host "If Cargo's bin directory is not on your PATH, add `$env:USERPROFILE\\.cargo\\bin`."
