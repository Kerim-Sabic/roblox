param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$Target
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot

if (-not $Target) {
    $hostLine = rustc -vV | Where-Object { $_ -like "host: *" } | Select-Object -First 1
    if (-not $hostLine) {
        throw "Unable to determine the Rust host target."
    }
    $Target = $hostLine.Substring("host: ".Length).Trim()
}

$metadata = cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $workspace "Cargo.toml") | ConvertFrom-Json
$targetDirectory = [System.IO.Path]::GetFullPath([string]$metadata.target_directory)
$cargoArguments = @(
    "build",
    "--manifest-path", (Join-Path $workspace "Cargo.toml"),
    "--package", "nectarpilot-daemon",
    "--target", $Target
)
if ($Configuration -eq "Release") {
    $cargoArguments += "--release"
}

& cargo @cargoArguments
if ($LASTEXITCODE -ne 0) {
    throw "Building the NectarPilot daemon sidecar failed with exit code $LASTEXITCODE."
}

$profileDirectory = $Configuration.ToLowerInvariant()
$extension = if ($Target -like "*-windows-*") { ".exe" } else { "" }
$source = Join-Path $targetDirectory (Join-Path $Target (Join-Path $profileDirectory "nectarpilot-daemon$extension"))
$destinationDirectory = Join-Path $workspace "apps\desktop\src-tauri\binaries"
$destination = Join-Path $destinationDirectory "nectarpilot-daemon-$Target$extension"

if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "The compiled daemon was not found at $source."
}

New-Item -ItemType Directory -Force -Path $destinationDirectory | Out-Null
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Host "Prepared Rust daemon sidecar: $destination"
