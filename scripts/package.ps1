$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectRoot
try {
    cargo test --locked --all-targets
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    cargo build --locked --release --bin mini-stock-monitor
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $releaseDirectory = Join-Path $projectRoot 'dist\MiniStockMonitor'
    $null = New-Item -ItemType Directory -Path $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'target\release\mini-stock-monitor.exe') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $releaseDirectory -Force
    $archivePath = Join-Path $projectRoot 'dist\MiniStockMonitor-windows-x64.zip'
    Compress-Archive -LiteralPath $releaseDirectory -DestinationPath $archivePath -Force
    $checksum = Get-FileHash -LiteralPath $archivePath -Algorithm SHA256
    ($checksum.Hash.ToLowerInvariant() + '  ' + (Split-Path -Leaf $archivePath)) | Set-Content -LiteralPath (Join-Path $projectRoot 'dist\SHA256SUMS.txt') -Encoding ascii
    Write-Output $archivePath
} finally {
    Pop-Location
}
