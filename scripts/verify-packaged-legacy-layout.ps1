[CmdletBinding()]
param(
    [string]$Root,
    [string]$LayoutRoot
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($Root)) {
    $Root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
} else {
    $Root = (Resolve-Path -LiteralPath $Root).Path
}

function Require-Path {
    param(
        [string]$Path,
        [string]$Description,
        [switch]$Directory
    )

    $pathType = if ($Directory) { 'Container' } else { 'Leaf' }
    if (-not (Test-Path -LiteralPath $Path -PathType $pathType)) {
        throw "$Description is missing: $Path"
    }
}

function Read-SupportManifest {
    param([string]$Path)

    $entries = @()
    $entry = $null
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match '^\s*-\s+path:\s*(?<path>.+?)\s*$') {
            if ($null -ne $entry) { $entries += [pscustomobject]$entry }
            $entry = @{ path = $Matches.path; sha256 = $null; bytes = $null }
            continue
        }
        if ($null -eq $entry) { continue }
        if ($line -match '^\s+sha256:\s*(?<sha256>[a-f0-9]{64})\s*$') {
            $entry.sha256 = $Matches.sha256
            continue
        }
        if ($line -match '^\s+bytes:\s*(?<bytes>\d+)\s*$') {
            $entry.bytes = [uint64]$Matches.bytes
        }
    }
    if ($null -ne $entry) { $entries += [pscustomobject]$entry }

    if ($entries.Count -ne 9) {
        throw "The legacy support manifest must pin exactly 9 harness files; found $($entries.Count)."
    }
    foreach ($candidate in $entries) {
        if ([string]::IsNullOrWhiteSpace($candidate.path) -or
            [string]::IsNullOrWhiteSpace($candidate.sha256) -or
            $null -eq $candidate.bytes) {
            throw 'A legacy support manifest entry is incomplete.'
        }
        if ($candidate.path -notmatch '^(lib|nm_image_assets)/') {
            throw "Legacy support entry is outside the packaged compatibility roots: $($candidate.path)"
        }
    }
    return $entries
}

function Assert-LegacyLayout {
    param(
        [string]$RootPath,
        [object[]]$SupportEntries,
        [string]$PinnedAutoHotkeySha256
    )

    $resolved = (Resolve-Path -LiteralPath $RootPath).Path
    foreach ($directory in @('lib', 'nm_image_assets', 'paths', 'patterns', 'assets')) {
        Require-Path -Path (Join-Path $resolved $directory) -Description "Packaged compatibility directory '$directory'" -Directory
    }

    foreach ($entry in $SupportEntries) {
        $relative = $entry.path -replace '/', [IO.Path]::DirectorySeparatorChar
        $path = Join-Path $resolved $relative
        Require-Path -Path $path -Description "Pinned legacy support file '$($entry.path)'"
        $item = Get-Item -LiteralPath $path
        if ([uint64]$item.Length -ne [uint64]$entry.bytes) {
            throw "Pinned legacy support file '$($entry.path)' has an unexpected size."
        }
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $entry.sha256) {
            throw "Pinned legacy support file '$($entry.path)' does not match its manifest digest."
        }
    }

    $interpreters = @(
        @(
            (Join-Path $resolved 'AutoHotkey64.exe'),
            (Join-Path $resolved 'submacros\AutoHotkey64.exe')
        ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
    )
    if ($interpreters.Count -eq 0) {
        throw 'The pinned AutoHotkey64.exe interpreter is absent from the compatibility layout.'
    }
    $actualInterpreterHash = (Get-FileHash -LiteralPath $interpreters[0] -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualInterpreterHash -ne $PinnedAutoHotkeySha256) {
        throw 'The packaged AutoHotkey64.exe interpreter does not match the daemon trust pin.'
    }
}

$configPath = Join-Path $Root 'apps\desktop\src-tauri\tauri.conf.json'
Require-Path -Path $configPath -Description 'Tauri configuration'
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$resources = @{}
foreach ($property in $config.bundle.resources.PSObject.Properties) {
    $resources[$property.Name] = [string]$property.Value
}

$requiredResources = [ordered]@{
    '../../../assets/' = 'assets/'
    '../../../lib/' = 'lib/'
    '../../../nm_image_assets/' = 'nm_image_assets/'
    '../../../paths/' = 'paths/'
    '../../../patterns/' = 'patterns/'
    '../../../submacros/AutoHotkey64.exe' = 'AutoHotkey64.exe'
}
foreach ($source in $requiredResources.Keys) {
    if (-not $resources.ContainsKey($source) -or $resources[$source] -ne $requiredResources[$source]) {
        throw "Tauri must bundle '$source' at '$($requiredResources[$source])' for the legacy compatibility layout."
    }
}

$supportManifest = Join-Path $Root 'assets\legacy-support\_legacy-manifest.yaml'
Require-Path -Path $supportManifest -Description 'Legacy support manifest'
$supportEntries = Read-SupportManifest -Path $supportManifest

$legacyService = Get-Content -LiteralPath (Join-Path $Root 'apps\daemon\src\legacy_service.rs') -Raw
$pin = [regex]::Match($legacyService, '(?s)PINNED_AUTOHOTKEY64_SHA256.*?"(?<hash>[a-f0-9]{64})"')
if (-not $pin.Success) { throw 'Could not find the pinned AutoHotkey64.exe digest in legacy_service.rs.' }

Assert-LegacyLayout -RootPath $Root -SupportEntries $supportEntries -PinnedAutoHotkeySha256 $pin.Groups['hash'].Value
if (-not [string]::IsNullOrWhiteSpace($LayoutRoot)) {
    Assert-LegacyLayout -RootPath $LayoutRoot -SupportEntries $supportEntries -PinnedAutoHotkeySha256 $pin.Groups['hash'].Value
}

$layoutDescription = if ($LayoutRoot) { " and staged layout '$LayoutRoot'" } else { '' }
Write-Host "Validated Tauri legacy resource mappings and $($supportEntries.Count) pinned support files$layoutDescription."
