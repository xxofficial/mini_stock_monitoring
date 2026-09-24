param(
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$appName = [string]::Concat([char]0x5FAE, [char]0x884C, [char]0x60C5)

function Resolve-InnoCompiler {
    $command = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }

    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return $candidate
        }
    }

    throw 'Inno Setup 6 was not found. Install it with: winget install --id JRSoftware.InnoSetup --exact'
}

Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipTests) {
        cargo test --locked --all-targets
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    }

    $iconPath = Join-Path $projectRoot 'assets\mini-stock-monitor.ico'
    if (-not (Test-Path -LiteralPath $iconPath)) {
        & (Join-Path $PSScriptRoot 'generate-icon.ps1') | Out-Null
    }
    cargo build --locked --release --bin mini-stock-monitor
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

    $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read package metadata.' }
    $package = $metadata.packages | Where-Object { $_.name -eq 'mini-stock-monitor' } | Select-Object -First 1
    if ($null -eq $package) { throw 'Unable to find the mini-stock-monitor package.' }
    $version = $package.version

    $distDirectory = Join-Path $projectRoot 'dist'
    $releaseDirectory = Join-Path $distDirectory 'MiniStockMonitor'
    if (Test-Path -LiteralPath $releaseDirectory) {
        Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
    }
    $null = New-Item -ItemType Directory -Path $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'target\release\mini-stock-monitor.exe') -Destination (Join-Path $releaseDirectory "$appName.exe") -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $releaseDirectory -Force

    $archivePath = Join-Path $distDirectory 'MiniStockMonitor-windows-x64.zip'
    Compress-Archive -LiteralPath $releaseDirectory -DestinationPath $archivePath -Force

    $innoCompiler = Resolve-InnoCompiler
    $installerScript = Join-Path $projectRoot 'installer\mini-stock-monitor.iss'
    & $innoCompiler "/DMyAppVersion=$version" $installerScript
    if ($LASTEXITCODE -ne 0) { throw 'Installer build failed.' }
    $installerPath = Join-Path $distDirectory "$appName-$version-windows-x64-setup.exe"
    if (-not (Test-Path -LiteralPath $installerPath)) { throw 'The installer output was not created.' }

    $releaseArtifacts = @($archivePath, $installerPath)
    $checksums = foreach ($artifact in $releaseArtifacts) {
        $checksum = Get-FileHash -LiteralPath $artifact -Algorithm SHA256
        $checksum.Hash.ToLowerInvariant() + '  ' + (Split-Path -Leaf $artifact)
    }
    $checksumPath = Join-Path $distDirectory 'SHA256SUMS.txt'
    [System.IO.File]::WriteAllLines($checksumPath, $checksums, [System.Text.UTF8Encoding]::new($false))

    Write-Output $installerPath
    Write-Output $archivePath
    Write-Output $checksumPath
} finally {
    Pop-Location
}
