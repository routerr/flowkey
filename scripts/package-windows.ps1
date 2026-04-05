Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot

& cargo build -p flowkey-cli --release
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$os = "windows"
$arch = $env:PROCESSOR_ARCHITECTURE
switch -Regex ($arch) {
    "ARM64" { $arch = "arm64" }
    "AMD64" { $arch = "amd64" }
    default { $arch = $arch.ToLowerInvariant() }
}

$stageDir = Join-Path "dist" "flky-$os-$arch"
$archivePath = Join-Path "dist" "flky-$os-$arch.zip"

if (Test-Path $stageDir) {
    Remove-Item $stageDir -Recurse -Force
}
if (Test-Path $archivePath) {
    Remove-Item $archivePath -Force
}
if (Test-Path "$archivePath.sha256") {
    Remove-Item "$archivePath.sha256" -Force
}

New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
Copy-Item "target\release\flky.exe" (Join-Path $stageDir "flky.exe")
Copy-Item README.md (Join-Path $stageDir "README.md")
Copy-Item docs\protocol.md (Join-Path $stageDir "protocol.md")
Copy-Item docs\architecture.md (Join-Path $stageDir "architecture.md")
Copy-Item scripts\install.ps1 (Join-Path $stageDir "install.ps1")

@"
Run the `flky.exe` binary from this folder or move it onto your PATH.
For a Cargo-based install, run `install.ps1`.
The binary reads config from the platform-specific application data directory
unless `$env:FLKY_CONFIG` is set.
"@ | Set-Content (Join-Path $stageDir "INSTALL.txt")

Compress-Archive -Path (Join-Path $stageDir "*") -DestinationPath $archivePath -Force

$hash = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant()
"$hash  $(Split-Path $archivePath -Leaf)" | Set-Content "$archivePath.sha256"

Write-Host "created $archivePath"
