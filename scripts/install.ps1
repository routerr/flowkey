Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot

Write-Host "Stopping any running flky processes..."
Stop-Process -Name "flky" -ErrorAction SilentlyContinue

& cargo install --path crates/flowkey-cli --locked --force
if ($LASTEXITCODE -ne 0) {
    throw "cargo install failed with exit code $LASTEXITCODE"
}

Write-Host "flky installed with cargo install."
Write-Host "If Cargo's bin directory is not on your PATH, add `$env:USERPROFILE\\.cargo\\bin`."
