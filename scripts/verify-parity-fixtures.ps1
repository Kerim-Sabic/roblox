[CmdletBinding()]
param(
    [string]$Index,
    [switch]$RequireCaptured
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Index) {
    $Index = Join-Path $root 'tests\fixtures\parity-index.json'
}
if (-not (Test-Path -LiteralPath $Index -PathType Leaf)) { throw "Fixture index was not found: $Index" }
$document = Get-Content -LiteralPath $Index -Raw | ConvertFrom-Json
if ([int]$document.schema_version -ne 1) { throw 'Unsupported fixture index schema version.' }

$fixtures = @($document.fixtures)
$scenarios = @($document.scenarios)
if ($fixtures.Count -lt 20) { throw 'Fixture index does not cover the required visual state groups.' }
if ($scenarios.Count -ne 28) { throw "Fixture index must map all 28 parity scenarios; found $($scenarios.Count)." }
$fixtureIds = @{}
foreach ($fixture in $fixtures) {
    if ([string]::IsNullOrWhiteSpace([string]$fixture.id) -or $fixtureIds.ContainsKey($fixture.id)) {
        throw 'Fixture ids must be unique and non-empty.'
    }
    $fixtureIds[$fixture.id] = $true
    if (@($fixture.client_sizes).Count -eq 0 -or @($fixture.dpi_scales).Count -eq 0 -or @($fixture.detectors).Count -eq 0) {
        throw "Fixture '$($fixture.id)' is missing size, DPI, or detector metadata."
    }
    if ($RequireCaptured -and $fixture.capture_status -ne 'captured') {
        throw "Fixture '$($fixture.id)' is not a reviewed captured fixture."
    }
}

$scenarioIds = @{}
foreach ($scenario in $scenarios) {
    if ([string]::IsNullOrWhiteSpace([string]$scenario.id) -or $scenarioIds.ContainsKey($scenario.id)) {
        throw 'Scenario ids must be unique and non-empty.'
    }
    $scenarioIds[$scenario.id] = $true
    if ([string]::IsNullOrWhiteSpace([string]$scenario.area) -or [string]::IsNullOrWhiteSpace([string]$scenario.live_test)) {
        throw "Scenario '$($scenario.id)' needs an area and an attended live-test protocol."
    }
    if (@($scenario.regressions).Count -eq 0) { throw "Scenario '$($scenario.id)' has no regression assertion." }
    foreach ($fixtureId in @($scenario.fixtures)) {
        if (-not $fixtureIds.ContainsKey($fixtureId)) {
            throw "Scenario '$($scenario.id)' references missing fixture '$fixtureId'."
        }
    }
}

$routeCatalog = Get-Content -LiteralPath (Join-Path $root 'assets\routes\_legacy-manifest.yaml') -Raw
$patternCatalog = Get-Content -LiteralPath (Join-Path $root 'assets\patterns\_legacy-manifest.yaml') -Raw
if ($routeCatalog -notmatch '(?m)^total_files:\s+91\s*$' -or $routeCatalog -notmatch '(?m)^legacy_bridge_files:\s+91\s*$') {
    throw 'Route catalog does not prove all 91 legacy routes are represented.'
}
if ($patternCatalog -notmatch '(?m)^total_files:\s+12\s*$' -or $patternCatalog -notmatch '(?m)^legacy_bridge_files:\s+11\s*$') {
    throw 'Pattern catalog does not prove 12 patterns with 11 bridge-required assets.'
}

Write-Host "Validated $($fixtures.Count) fixture contracts and $($scenarios.Count) parity scenarios."
