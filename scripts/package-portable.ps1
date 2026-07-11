[CmdletBinding()]
param(
    [string]$Configuration = 'release',
    [string]$Version = '0.1.0'
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$target = Join-Path $root "target\$Configuration"
$distRoot = [IO.Path]::GetFullPath((Join-Path $root 'dist'))
$stage = [IO.Path]::GetFullPath((Join-Path $distRoot "NectarPilot-$Version-windows-x64-portable"))
$archive = "$stage.zip"

if (-not $stage.StartsWith($distRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to stage outside the workspace dist directory: $stage"
}

$required = @(
    (Join-Path $target 'nectarpilot.exe'),
    (Join-Path $target 'nectarpilot-daemon.exe')
)
foreach ($path in $required) {
    if (-not (Test-Path -LiteralPath $path)) { throw "Missing build output: $path" }
}

if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item -LiteralPath $required[0] -Destination (Join-Path $stage 'NectarPilot.exe')
Copy-Item -LiteralPath $required[1] -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'START.bat') -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'LICENSE.md') -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'THIRD_PARTY_NOTICES.md') -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'assets') -Destination $stage -Recurse
$complianceFiles = @(
    (Join-Path $root 'dist\nectarpilot.cdx.json'),
    (Join-Path $root 'dist\npm-licenses.json')
)
foreach ($complianceFile in $complianceFiles) {
    if (Test-Path -LiteralPath $complianceFile) {
        Copy-Item -LiteralPath $complianceFile -Destination $stage
    }
}
New-Item -ItemType File -Force -Path (Join-Path $stage '.portable') | Out-Null

if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
Compress-Archive -LiteralPath $stage -DestinationPath $archive -CompressionLevel Optimal
$hash = Get-FileHash -LiteralPath $archive -Algorithm SHA256
Set-Content -LiteralPath "$archive.sha256" -Encoding ascii -Value "$($hash.Hash.ToLowerInvariant())  $([IO.Path]::GetFileName($archive))"
Write-Host "Created $archive"
