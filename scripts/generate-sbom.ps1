[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$Version = '0.1.0'
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path -Path $PSScriptRoot -ChildPath '..\dist'
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null

$cargoText = (& cargo metadata --format-version 1 --locked) -join [Environment]::NewLine
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed.' }
$cargo = $cargoText | ConvertFrom-Json

$licenseText = (& pnpm licenses list --json --long) -join [Environment]::NewLine
if ($LASTEXITCODE -ne 0) { throw 'pnpm license inventory failed.' }
$npmLicenses = $licenseText | ConvertFrom-Json
$licensePath = Join-Path $output 'npm-licenses.json'
$licenseText | Set-Content -LiteralPath $licensePath -Encoding utf8

$components = @{}
foreach ($package in $cargo.packages) {
    $purl = "pkg:cargo/$($package.name)@$($package.version)"
    $component = [ordered]@{
        type = if ($cargo.workspace_members -contains $package.id) { 'application' } else { 'library' }
        name = $package.name
        version = $package.version
        'bom-ref' = $purl
        purl = $purl
        properties = @([ordered]@{ name = 'nectarpilot:ecosystem'; value = 'cargo' })
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$package.license)) {
        $component.licenses = @([ordered]@{ expression = $package.license })
    }
    $components[$purl] = $component
}

foreach ($licenseGroup in $npmLicenses.PSObject.Properties) {
    foreach ($package in @($licenseGroup.Value)) {
        foreach ($packageVersion in @($package.versions)) {
            $escapedName = [Uri]::EscapeDataString([string]$package.name).Replace('%2F', '/')
            $purl = "pkg:npm/$escapedName@$packageVersion"
            $components[$purl] = [ordered]@{
                type = 'library'
                name = $package.name
                version = $packageVersion
                'bom-ref' = $purl
                purl = $purl
                licenses = @([ordered]@{ expression = [string]$package.license })
                properties = @([ordered]@{ name = 'nectarpilot:ecosystem'; value = 'npm' })
            }
        }
    }
}

$bom = [ordered]@{
    bomFormat = 'CycloneDX'
    specVersion = '1.6'
    serialNumber = "urn:uuid:$([Guid]::NewGuid())"
    version = 1
    metadata = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString('o')
        tools = [ordered]@{
            components = @([ordered]@{
                type = 'application'
                name = 'NectarPilot SBOM generator'
                version = $Version
            })
        }
        component = [ordered]@{
            type = 'application'
            name = 'NectarPilot'
            version = $Version
            'bom-ref' = "pkg:generic/nectarpilot@$Version"
            purl = "pkg:generic/nectarpilot@$Version"
        }
    }
    components = @($components.Values | Sort-Object purl)
}

$bomPath = Join-Path $output 'nectarpilot.cdx.json'
$bom | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $bomPath -Encoding utf8

Write-Host "Created $bomPath with $($components.Count) components."
Write-Host "Created $licensePath."
