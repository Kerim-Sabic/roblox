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
    $configName = if ($Channel -eq 'beta') { 'tauri.beta.conf.json' } else { 'tauri.conf.json' }
    $TauriConfig = Join-Path -Path $PSScriptRoot -ChildPath "..\apps\desktop\src-tauri\$configName"
}

$matrix = Get-Content -LiteralPath $ParityMatrix -Raw -Encoding utf8
if ($matrix.Contains([string][char]0x2610)) {
    throw 'Feature parity matrix still contains incomplete rows.'
}

$tauri = Get-Content -LiteralPath $TauriConfig -Raw | ConvertFrom-Json
$updater = $tauri.plugins.updater
if (-not $updater -or [string]::IsNullOrWhiteSpace([string]$updater.pubkey) -or @($updater.endpoints).Count -eq 0) {
    throw 'Signed updater public key and HTTPS channel endpoint are not configured.'
}
$endpoint = [string]@($updater.endpoints)[0]
if (-not $endpoint.StartsWith('https://', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Updater endpoint must use HTTPS.'
}
if ($Channel -eq 'beta' -and $endpoint.IndexOf('/releases/download/beta/latest.json', [StringComparison]::OrdinalIgnoreCase) -lt 0) {
    throw 'Beta channel must target the dedicated beta updater manifest.'
}
if ($Channel -eq 'stable' -and $endpoint.IndexOf('/releases/latest/download/latest.json', [StringComparison]::OrdinalIgnoreCase) -lt 0) {
    throw 'Stable channel must target the latest stable updater manifest.'
}
if (-not $SoakResult) { throw 'A signed-off soak result JSON path is required.' }

$result = Get-Content -LiteralPath $SoakResult -Raw | ConvertFrom-Json
$requiredFields = @(
    'schema_version', 'run_id', 'mode', 'build_commit', 'profile_digest',
    'heartbeat_sha256', 'heartbeat_count', 'safe_profile_validated',
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
if ([int]$result.schema_version -ne 1) { throw 'Unsupported soak result schema version.' }
if ($result.mode -ne 'live') { throw 'Release gates require a controlled live soak, not a fixture-only run.' }
if ([string]::IsNullOrWhiteSpace([string]$result.approved_by)) { throw 'Soak result requires an approver.' }
if ([string]::IsNullOrWhiteSpace([string]$result.run_id) -or [string]::IsNullOrWhiteSpace([string]$result.build_commit)) {
    throw 'Soak result must identify the run and tested build.'
}
if ([string]$result.profile_digest -notmatch '^[a-f0-9]{64}$' -or [string]$result.heartbeat_sha256 -notmatch '^[a-f0-9]{64}$') {
    throw 'Soak result must include lowercase SHA-256 evidence digests.'
}
if ([int]$result.heartbeat_count -lt 2 -or -not [bool]$result.safe_profile_validated) {
    throw 'Soak result lacks repeated safe-profile validation evidence.'
}

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
