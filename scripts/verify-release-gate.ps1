[CmdletBinding()]
param(
    [ValidateSet('beta', 'stable')]
    [string]$Channel,
    [string]$ParityMatrix,
    [string]$SoakResult,
    [string]$TauriConfig
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($ParityMatrix)) {
    $ParityMatrix = Join-Path -Path $PSScriptRoot -ChildPath '..\docs\PARITY_MATRIX.md'
}
if ([string]::IsNullOrWhiteSpace($TauriConfig)) {
    $TauriConfig = Join-Path -Path $PSScriptRoot -ChildPath '..\apps\desktop\src-tauri\tauri.conf.json'
}
$matrix = Get-Content -LiteralPath $ParityMatrix -Raw
if ($matrix -match '☐') { throw 'Feature parity matrix still contains incomplete rows.' }
$tauri = Get-Content -LiteralPath $TauriConfig -Raw | ConvertFrom-Json
$updater = $tauri.plugins.updater
if (-not $updater -or [string]::IsNullOrWhiteSpace([string]$updater.pubkey) -or @($updater.endpoints).Count -eq 0) {
    throw 'Signed updater public key and HTTPS channel endpoint are not configured.'
}
if (-not $SoakResult) { throw 'A signed-off soak result JSON path is required.' }

$result = Get-Content -LiteralPath $SoakResult -Raw | ConvertFrom-Json
$requiredFields = @(
    'channel', 'started_at', 'finished_at', 'duration_hours', 'passed',
    'unhandled_crashes', 'stuck_inputs', 'unrelated_process_terminations',
    'false_roblox_restarts', 'forced_recovery_scenarios', 'approved_by'
)
if ($Channel -eq 'stable') {
    $requiredFields += @('severity_1_open', 'severity_2_open')
}
foreach ($field in $requiredFields) {
    if ($result.PSObject.Properties.Name -notcontains $field) { throw "Soak result is missing '$field'." }
}
if ($result.channel -ne $Channel) { throw "Soak result channel '$($result.channel)' does not match '$Channel'." }
if ([string]::IsNullOrWhiteSpace([string]$result.approved_by)) { throw 'Soak result requires an approver.' }
$requiredHours = if ($Channel -eq 'stable') { 72 } else { 24 }
if (-not $result.passed) { throw 'Soak result is marked failed.' }
if ([double]$result.duration_hours -lt $requiredHours) { throw "Channel requires at least $requiredHours soak hours." }
$startedAt = [DateTimeOffset]::Parse([string]$result.started_at)
$finishedAt = [DateTimeOffset]::Parse([string]$result.finished_at)
if ($finishedAt -le $startedAt) { throw 'Soak result finish time must be after its start time.' }
$measuredHours = ($finishedAt - $startedAt).TotalHours
if ($measuredHours + 0.01 -lt [double]$result.duration_hours) {
    throw 'Reported soak duration exceeds the measured timestamp interval.'
}
if ([int]$result.unhandled_crashes -ne 0 -or [int]$result.stuck_inputs -ne 0 -or [int]$result.unrelated_process_terminations -ne 0) {
    throw 'Soak result violates a zero-tolerance safety gate.'
}
if ([int]$result.false_roblox_restarts -ne 0) { throw 'Soak result contains a false Roblox restart.' }
if ($Channel -eq 'stable' -and [int]$result.forced_recovery_scenarios -lt 50) {
    throw 'Stable requires at least 50 forced recovery scenarios.'
}
if ($Channel -eq 'stable' -and ([int]$result.severity_1_open -ne 0 -or [int]$result.severity_2_open -ne 0)) {
    throw 'Stable cannot ship with unresolved severity-1 or severity-2 defects.'
}

Write-Host "$Channel release gate satisfied by $SoakResult"
