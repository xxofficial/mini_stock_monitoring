param(
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $projectRoot 'assets\mini-stock-monitor.ico'
}

function New-IconPixels {
    param([int]$Size)

    [byte[]]$pixels = New-Object byte[] ($Size * $Size * 4)
    $scale = $Size / 32.0

    for ($y = 0; $y -lt $Size; $y++) {
        for ($x = 0; $x -lt $Size; $x++) {
            $sourceX = (($x + 0.5) / $scale) - 0.5
            $sourceY = (($y + 0.5) / $scale) - 0.5
            $cornerX = [Math]::Max(6.0, [Math]::Min(25.0, $sourceX))
            $cornerY = [Math]::Max(6.0, [Math]::Min(25.0, $sourceY))
            if ([Math]::Pow($sourceX - $cornerX, 2) + [Math]::Pow($sourceY - $cornerY, 2) -le 36.0) {
                $offset = ($y * $Size + $x) * 4
                $pixels[$offset] = 20
                $pixels[$offset + 1] = 27
                $pixels[$offset + 2] = 39
                $pixels[$offset + 3] = 255
            }
        }
    }

    $segments = @(
        @(6, 23, 13, 16),
        @(13, 16, 18, 19),
        @(18, 19, 26, 9)
    )
    $steps = [Math]::Max(1, [Math]::Floor(80 * $scale + 0.5))
    $strokeSize = [Math]::Max(1, [Math]::Floor(2 * $scale + 0.5))
    foreach ($segment in $segments) {
        for ($step = 0; $step -le $steps; $step++) {
            $t = $step / [double]$steps
            $pointX = ($segment[0] + ($segment[2] - $segment[0]) * $t) * $scale
            $pointY = ($segment[1] + ($segment[3] - $segment[1]) * $t) * $scale
            $startX = [Math]::Floor($pointX + 0.5)
            $startY = [Math]::Floor($pointY + 0.5)
            for ($dy = 0; $dy -lt $strokeSize; $dy++) {
                for ($dx = 0; $dx -lt $strokeSize; $dx++) {
                    $drawX = $startX + $dx
                    $drawY = $startY + $dy
                    if ($drawX -ge 0 -and $drawX -lt $Size -and $drawY -ge 0 -and $drawY -lt $Size) {
                        $offset = ($drawY * $Size + $drawX) * 4
                        $pixels[$offset] = 98
                        $pixels[$offset + 1] = 218
                        $pixels[$offset + 2] = 180
                        $pixels[$offset + 3] = 255
                    }
                }
            }
        }
    }

    return $pixels
}

function New-IconDib {
    param([int]$Size)

    [byte[]]$pixels = New-IconPixels -Size $Size
    $maskRowBytes = [int]([Math]::Ceiling($Size / 32.0) * 4)
    $xorBytes = $Size * $Size * 4
    $stream = [System.IO.MemoryStream]::new()
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([uint32]40)
        $writer.Write([int32]$Size)
        $writer.Write([int32]($Size * 2))
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]0)
        $writer.Write([uint32]$xorBytes)
        $writer.Write([int32]0)
        $writer.Write([int32]0)
        $writer.Write([uint32]0)
        $writer.Write([uint32]0)

        for ($y = $Size - 1; $y -ge 0; $y--) {
            for ($x = 0; $x -lt $Size; $x++) {
                $offset = ($y * $Size + $x) * 4
                $writer.Write([byte]$pixels[$offset + 2])
                $writer.Write([byte]$pixels[$offset + 1])
                $writer.Write([byte]$pixels[$offset])
                $writer.Write([byte]$pixels[$offset + 3])
            }
        }
        $writer.Write((New-Object byte[] ($maskRowBytes * $Size)))
        return $stream.ToArray()
    } finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$images = foreach ($size in $sizes) {
    [byte[]]$data = New-IconDib -Size $size
    [PSCustomObject]@{ Size = $size; Data = $data }
}

$outputDirectory = Split-Path -Parent $OutputPath
$null = New-Item -ItemType Directory -Path $outputDirectory -Force
$iconStream = [System.IO.MemoryStream]::new()
$iconWriter = [System.IO.BinaryWriter]::new($iconStream)
try {
    $iconWriter.Write([uint16]0)
    $iconWriter.Write([uint16]1)
    $iconWriter.Write([uint16]$images.Count)
    $imageOffset = 6 + 16 * $images.Count
    foreach ($image in $images) {
        $dimension = if ($image.Size -eq 256) { 0 } else { $image.Size }
        $iconWriter.Write([byte]$dimension)
        $iconWriter.Write([byte]$dimension)
        $iconWriter.Write([byte]0)
        $iconWriter.Write([byte]0)
        $iconWriter.Write([uint16]1)
        $iconWriter.Write([uint16]32)
        $iconWriter.Write([uint32]$image.Data.Length)
        $iconWriter.Write([uint32]$imageOffset)
        $imageOffset += $image.Data.Length
    }
    foreach ($image in $images) {
        $iconWriter.Write([byte[]]$image.Data)
    }
    [System.IO.File]::WriteAllBytes($OutputPath, $iconStream.ToArray())
} finally {
    $iconWriter.Dispose()
    $iconStream.Dispose()
}

Write-Output $OutputPath
