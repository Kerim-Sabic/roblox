[CmdletBinding()]
param(
    [string]$Root
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($Root)) {
    $Root = (Resolve-Path -LiteralPath (Join-Path -Path $PSScriptRoot -ChildPath '..')).Path
}
$patterns = @(
    '(?i)discord(?:app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9._-]+',
    '(?i)privateServerLinkCode=[A-Za-z0-9_-]{16,}',
    '(?i)"(?:bot_?token|webhook|private_?server(?:_link)?)"\s*:\s*"(?!REDACTED\b|example\b|")[^"]{16,}"',
    '(?im)^\s*(?:bot_?token|webhook_url|private_?server(?:_link)?)\s*=\s*(?!REDACTED\s*$|example\s*$)\S+'
)
$excluded = @('.git', '.check', 'node_modules', 'target', 'dist', 'settings')
$violations = @()
$normalizedRoot = (Resolve-Path -LiteralPath $Root).Path.TrimEnd('\') + '\'

Get-ChildItem -LiteralPath $Root -Recurse -File | Where-Object {
    $relative = $_.FullName.Substring($normalizedRoot.Length)
    $segments = $relative -split '[\\/]'
    -not ($excluded | Where-Object { $segments -contains $_ })
} | ForEach-Object {
    $path = $_.FullName
    if ($_.Length -gt 5MB) { return }
    $content = Get-Content -LiteralPath $path -Raw -ErrorAction SilentlyContinue
    foreach ($pattern in $patterns) {
        if ($content -match $pattern) {
            $violations += $path.Substring($normalizedRoot.Length)
            break
        }
    }
}

if ($violations.Count -gt 0) {
    $violations | Sort-Object -Unique | ForEach-Object { Write-Error "Potential secret in $_" }
    exit 1
}

Write-Host 'No known secret patterns found in tracked source candidates.'
