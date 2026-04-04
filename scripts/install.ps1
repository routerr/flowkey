Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot

cargo install --path crates/kms-cli --locked --force

Write-Host "kms installed with cargo install."
Write-Host "If Cargo's bin directory is not on your PATH, add `$env:USERPROFILE\\.cargo\\bin`."
