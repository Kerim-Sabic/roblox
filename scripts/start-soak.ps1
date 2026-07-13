[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('beta', 'stable')]
    [string]$Channel,

    [Parameter(Mandatory)]
    [ValidateSet('fixture', 'live')]
    [string]$Mode,

    [Parameter(Mandatory)]
    [string]$ProfileJson,

    [string]$Database,
    [string]$DaemonPath,
    [int]$RobloxPid,
    [ValidateRange(10, 3600)]
    [int]$HeartbeatSeconds = 60,
    [string]$OutputDirectory,
    [string]$BuildCommit,
    [switch]$AllowLiveInput
)

<#
.SYNOPSIS
Starts an evidence-producing fixture or controlled-live soak monitor.

.DESCRIPTION
This script deliberately does not create a passing release result. It samples
the approved daemon-owned profile throughout a run, records exact Roblox and
daemon process observations, and leaves completion to complete-soak.ps1 after
the elapsed gate and human review. Live mode requires an explicit adopted
RobloxPlayerBeta PID and an explicit acknowledgement because it monitors an
automation session that the operator starts from the NectarPilot UI.
#>

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $root 'artifacts\soaks'
}
if (-not $BuildCommit) {
    $BuildCommit = (git -C $root rev-parse HEAD).Trim()
}
if ($BuildCommit -notmatch '^[0-9a-f]{7,64}$') {
    throw 'BuildCommit must be a Git commit SHA.'
}

function Read-ProfileDocument {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Profile JSON was not found: $Path"
    }
    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Assert-SafeProfile {
    param([pscustomobject]$Profile)

    if (-not $Profile.id) { throw 'Safe test profile has no id.' }
    if (-not $Profile.safety) { throw 'Safe test profile has no safety section.' }
    foreach ($property in 'purchases_enabled', 'donations_enabled', 'trades_enabled', 'allow_system_power') {
        if ([bool]$Profile.safety.$property) {
            throw "Safe test profile must keep safety.$property disabled."
        }
    }
    $budgets = $Profile.safety.item_budgets
    if (-not $budgets) { throw 'Safe test profile has no item budget section.' }
    foreach ($property in 'dice', 'glitter', 'eggs', 'stickers', 'vouchers', 'shrine_donations') {
        if ([int]$budgets.$property -ne 0) {
            throw "Safe test profile must keep item budget '$property' at zero."
        }
    }
    if ($budgets.other) {
        foreach ($property in $budgets.other.PSObject.Properties) {
            if ([int]$property.Value -ne 0) {
                throw "Safe test profile must keep custom item budget '$($property.Name)' at zero."
            }
        }
    }
    if ($Profile.discord -and [bool]$Profile.discord.enabled) {
        throw 'Safe test profile must keep Discord disabled during soak testing.'
    }
    if ($Profile.automation -and $Profile.automation.features) {
        foreach ($property in 'shrine', 'stickers', 'mutations_and_auto_jelly') {
            if ([bool]$Profile.automation.features.$property) {
                throw "Safe test profile must keep automation.features.$property disabled."
            }
        }
    }
}

function Get-ProfileDigest {
    param([pscustomobject]$Profile)

    $canonical = $Profile | ConvertTo-Json -Depth 32 -Compress
    $bytes = [Text.Encoding]::UTF8.GetBytes($canonical)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $hash = $hasher.ComputeHash($bytes)
        return ([BitConverter]::ToString($hash)).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $hasher.Dispose()
    }
}

function Read-DaemonProfile {
    param([string]$Executable, [string]$Store, [string]$ProfileId)

    $json = & $Executable export-profile --database $Store $ProfileId
    if ($LASTEXITCODE -ne 0) { throw 'Daemon could not export the live soak profile.' }
    return $json | ConvertFrom-Json
}

$profile = Read-ProfileDocument -Path $ProfileJson
Assert-SafeProfile -Profile $profile

if ($Mode -eq 'live') {
    if (-not $AllowLiveInput) {
        throw 'Live mode requires -AllowLiveInput after the operator has reviewed docs/LIVE_TEST_PROTOCOL.md.'
    }
    if (-not $Database -or -not $DaemonPath) {
        throw 'Live mode requires -Database and -DaemonPath so every heartbeat revalidates the daemon-owned profile.'
    }
    if (-not (Test-Path -LiteralPath $Database -PathType Leaf)) { throw "Database was not found: $Database" }
    if (-not (Test-Path -LiteralPath $DaemonPath -PathType Leaf)) { throw "Daemon executable was not found: $DaemonPath" }
    if ($RobloxPid -le 0) { throw 'Live mode requires the adopted RobloxPlayerBeta -RobloxPid.' }
    $roblox = Get-Process -Id $RobloxPid -ErrorAction Stop
    if ($roblox.ProcessName -ne 'RobloxPlayerBeta') {
        throw "Refusing live soak: PID $RobloxPid is '$($roblox.ProcessName)', not RobloxPlayerBeta."
    }
}

$runId = [Guid]::NewGuid().ToString()
$runDirectory = Join-Path (Join-Path $OutputDirectory $Channel) $runId
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
$manifestPath = Join-Path $runDirectory 'manifest.json'
$heartbeatPath = Join-Path $runDirectory 'heartbeats.ndjson'
$requiredHours = if ($Channel -eq 'stable') { 72 } else { 24 }
$startedAt = [DateTimeOffset]::UtcNow

$manifest = [ordered]@{
    schema_version = 1
    run_id = $runId
    channel = $Channel
    mode = $Mode
    started_at = $startedAt.ToString('O')
    required_duration_hours = $requiredHours
    build_commit = $BuildCommit.ToLowerInvariant()
    profile_id = [string]$profile.id
    profile_digest = Get-ProfileDigest -Profile $profile
    safe_profile_validated = $true
    live_input_acknowledged = [bool]$AllowLiveInput
    adopted_roblox_pid = if ($Mode -eq 'live') { $RobloxPid } else { $null }
    heartbeat_seconds = $HeartbeatSeconds
    machine = $env:COMPUTERNAME
}
$manifest | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath $manifestPath -Encoding utf8

Write-Host "Started $Channel $Mode soak $runId"
Write-Host "Evidence directory: $runDirectory"
Write-Host 'Do not change the profile to enable spending, donations, trades, Discord, shrine, stickers, or Auto-Jelly.'
Write-Host 'Stop the monitor with Ctrl+C only after recording an operator stop; complete-soak.ps1 never marks an interrupted run as passing.'

$heartbeatCount = 0
try {
    while ($true) {
        $currentProfile = if ($Mode -eq 'live') {
            Read-DaemonProfile -Executable $DaemonPath -Store $Database -ProfileId ([string]$profile.id)
        } else {
            Read-ProfileDocument -Path $ProfileJson
        }
        Assert-SafeProfile -Profile $currentProfile
        $profileDigest = Get-ProfileDigest -Profile $currentProfile
        $robloxObservation = if ($Mode -eq 'live') {
            $process = Get-Process -Id $RobloxPid -ErrorAction Stop
            if ($process.ProcessName -ne 'RobloxPlayerBeta') {
                throw "Adopted Roblox process identity changed to '$($process.ProcessName)'."
            }
            [ordered]@{ pid = $process.Id; name = $process.ProcessName; responding = $process.Responding }
        } else {
            $null
        }
        $daemonProcesses = @(Get-Process -Name 'nectarpilot-daemon' -ErrorAction SilentlyContinue | ForEach-Object {
            [ordered]@{ pid = $_.Id; responding = $_.Responding }
        })
        $heartbeat = [ordered]@{
            timestamp = [DateTimeOffset]::UtcNow.ToString('O')
            profile_digest = $profileDigest
            safe_profile_validated = $true
            adopted_roblox = $robloxObservation
            daemon_processes = $daemonProcesses
        }
        $heartbeat | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $heartbeatPath -Encoding utf8
        $heartbeatCount++
        Start-Sleep -Seconds $HeartbeatSeconds
    }
}
finally {
    $stopped = [ordered]@{
        timestamp = [DateTimeOffset]::UtcNow.ToString('O')
        event = 'monitor_stopped'
        heartbeat_count = $heartbeatCount
    }
    $stopped | ConvertTo-Json -Compress | Add-Content -LiteralPath $heartbeatPath -Encoding utf8
    Write-Host "Monitor stopped. Review evidence, then use scripts/complete-soak.ps1 -Manifest $manifestPath."
}
