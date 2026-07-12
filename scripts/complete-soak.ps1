[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Manifest,

    [Parameter(Mandatory)]
    [string]$ApprovedBy,

    [Parameter(Mandatory)]
    [switch]$Passed,

    [ValidateRange(0, 100000)]
    [int]$UnhandledCrashes = 0,
    [ValidateRange(0, 100000)]
    [int]$StuckInputs = 0,
    [ValidateRange(0, 100000)]
    [int]$UnrelatedProcessTerminations = 0,
    [ValidateRange(0, 100000)]
    [int]$FalseRobloxRestarts = 0,
    [ValidateRange(0, 100000)]
    [int]$ForcedRecoveryScenarios = 0,
    [ValidateRange(0, 100000)]
    [int]$Severity1Open = 0,
    [ValidateRange(0, 100000)]
    [int]$Severity2Open = 0,
    [string]$Notes
)

<#
.SYNOPSIS
Completes an attended soak after evidence review. It cannot shorten time.
#>

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) { throw "Manifest was not found: $Manifest" }
$manifestDocument = Get-Content -LiteralPath $Manifest -Raw | ConvertFrom-Json
if ([int]$manifestDocument.schema_version -ne 1) { throw 'Unsupported soak manifest schema version.' }
if ($manifestDocument.mode -ne 'live') { throw 'Fixture-mode runs cannot be completed as release soaks.' }
if (-not $manifestDocument.safe_profile_validated -or -not $manifestDocument.live_input_acknowledged) {
    throw 'Manifest does not prove a safe-profile, explicitly acknowledged live run.'
}

$heartbeatPath = Join-Path (Split-Path -Parent $Manifest) 'heartbeats.ndjson'
if (-not (Test-Path -LiteralPath $heartbeatPath -PathType Leaf)) { throw 'Heartbeat evidence is missing.' }
$lines = @(Get-Content -LiteralPath $heartbeatPath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
if ($lines.Count -lt 2) { throw 'At least two heartbeat observations are required.' }
$heartbeats = @($lines | ForEach-Object { $_ | ConvertFrom-Json })
if (@($heartbeats | Where-Object { $_.safe_profile_validated -eq $false }).Count -gt 0) {
    throw 'A heartbeat reported an unsafe profile; this soak cannot pass.'
}
$profileObservations = @($heartbeats | Where-Object { $_.profile_digest })
if (@($profileObservations | Where-Object { $_.profile_digest -ne $manifestDocument.profile_digest }).Count -gt 0) {
    throw 'The daemon-owned test profile changed during the soak; review and restart the run.'
}
$startedAt = [DateTimeOffset]::Parse([string]$manifestDocument.started_at)
$finishedAt = [DateTimeOffset]::UtcNow
$durationHours = ($finishedAt - $startedAt).TotalHours
$requiredHours = if ($manifestDocument.channel -eq 'stable') { 72 } else { 24 }
if ($durationHours -lt $requiredHours) {
    throw "The $($manifestDocument.channel) gate requires $requiredHours real hours; this run has $([Math]::Round($durationHours, 3))."
}
if ([string]::IsNullOrWhiteSpace($ApprovedBy)) { throw 'ApprovedBy is required.' }
if ($manifestDocument.channel -eq 'stable' -and $ForcedRecoveryScenarios -lt 50) {
    throw 'Stable requires at least 50 forced recovery scenarios.'
}
if ($UnhandledCrashes -ne 0 -or $StuckInputs -ne 0 -or $UnrelatedProcessTerminations -ne 0 -or $FalseRobloxRestarts -ne 0) {
    throw 'A zero-tolerance safety metric is non-zero; the soak cannot pass.'
}
if ($manifestDocument.channel -eq 'stable' -and ($Severity1Open -ne 0 -or $Severity2Open -ne 0)) {
    throw 'Stable cannot pass with open severity-1 or severity-2 defects.'
}

$heartbeatHash = (Get-FileHash -LiteralPath $heartbeatPath -Algorithm SHA256).Hash.ToLowerInvariant()
$result = [ordered]@{
    schema_version = 1
    run_id = [string]$manifestDocument.run_id
    mode = 'live'
    build_commit = [string]$manifestDocument.build_commit
    profile_digest = [string]$manifestDocument.profile_digest
    heartbeat_sha256 = $heartbeatHash
    heartbeat_count = $lines.Count
    safe_profile_validated = $true
    channel = [string]$manifestDocument.channel
    started_at = $startedAt.ToString('O')
    finished_at = $finishedAt.ToString('O')
    duration_hours = [Math]::Round($durationHours, 6)
    passed = [bool]$Passed
    unhandled_crashes = $UnhandledCrashes
    stuck_inputs = $StuckInputs
    unrelated_process_terminations = $UnrelatedProcessTerminations
    false_roblox_restarts = $FalseRobloxRestarts
    forced_recovery_scenarios = $ForcedRecoveryScenarios
    severity_1_open = $Severity1Open
    severity_2_open = $Severity2Open
    approved_by = $ApprovedBy
    notes = $Notes
}
$resultPath = Join-Path (Split-Path -Parent $Manifest) 'soak-result.json'
$result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $resultPath -Encoding utf8
Write-Host "Created reviewed soak result: $resultPath"
