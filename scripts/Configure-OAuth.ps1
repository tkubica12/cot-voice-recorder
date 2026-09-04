#requires -Version 5.1
<#
.SYNOPSIS
    Discover the Google OAuth client JSON files in .secrets/ (by JSON *shape*, not
    filename) and write only git-ignored local configuration for every component.

.DESCRIPTION
    Three Google OAuth clients live in a single Google Cloud project. This helper
    classifies the credential files by shape and wires each component without ever
    printing, logging, or committing a client ID or the Desktop client secret:

      * Android client   : root `installed`, NO `client_secret`.
      * Desktop client   : root `installed`, HAS `client_secret`, redirect `http://localhost`.
      * Web/server client: root `web`. Its client_id is the Android Credential
                           Manager `serverClientId` and the ID-token audience.

    Outputs (all git-ignored):
      * android/keystore.properties  -> googleWebClientId = <Web client id>
                                        (only that line is touched; signing values kept).
      * infra/main.parameters.local.json -> googleAllowedAudiences = Web + Desktop,
                                        allowlistedEmail = <owner email>.
      * windows/installer/staging/google-desktop-client.json -> a copy of the Desktop
                                        client JSON for the installer to bundle locally.

    The Desktop JSON is sensitive operational config: it is only ever copied into an
    ignored path, never committed and never echoed.

.PARAMETER SecretsDir
    Directory holding the downloaded Google OAuth client JSON files. Default: .secrets

.PARAMETER OwnerEmail
    The single allowlisted Google account. Not defaulted in source (the owner's address is
    personal data and must never be committed): pass -OwnerEmail, set VR_OWNER_EMAIL, or
    keep the value in the existing ignored infra/main.parameters.local.json.

.PARAMETER SkipDesktopStage
    Do not copy the Desktop JSON into the Windows installer staging path.
#>
[CmdletBinding()]
param(
    [string] $SecretsDir = '',
    [string] $OwnerEmail = '',
    [switch] $SkipDesktopStage
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $SecretsDir) { $SecretsDir = Join-Path $RepoRoot '.secrets' }
if (-not (Test-Path $SecretsDir)) { throw "Secrets directory not found: $SecretsDir" }

# Resolve the allowlisted address without ever hard-coding it in tracked source.
if (-not $OwnerEmail) { $OwnerEmail = $env:VR_OWNER_EMAIL }
if (-not $OwnerEmail) {
    $existingParams = Join-Path $RepoRoot 'infra/main.parameters.local.json'
    if (Test-Path $existingParams) {
        try {
            $prev = Get-Content $existingParams -Raw | ConvertFrom-Json
            $OwnerEmail = $prev.parameters.allowlistedEmail.value
        } catch { $OwnerEmail = '' }
    }
}
if (-not $OwnerEmail) {
    throw 'Owner email not provided. Pass -OwnerEmail, set VR_OWNER_EMAIL, or populate infra/main.parameters.local.json.'
}

function Get-ClientType {
    param([Parameter(Mandatory)] $Json)
    $root = @($Json.PSObject.Properties.Name)[0]
    $node = $Json.$root
    $props = @($node.PSObject.Properties.Name)
    $hasSecret = $props -contains 'client_secret'
    if ($root -eq 'web') { return @{ Type = 'web'; Node = $node } }
    if ($root -eq 'installed' -and $hasSecret) { return @{ Type = 'desktop'; Node = $node } }
    if ($root -eq 'installed' -and -not $hasSecret) { return @{ Type = 'android'; Node = $node } }
    return @{ Type = 'unknown'; Node = $node }
}

$web = $null; $desktop = $null; $android = $null; $desktopFile = $null
foreach ($f in Get-ChildItem -Path $SecretsDir -Filter *.json -File) {
    try { $json = Get-Content $f.FullName -Raw | ConvertFrom-Json } catch { continue }
    $c = Get-ClientType -Json $json
    switch ($c.Type) {
        'web'     { $web = $c.Node }
        'desktop' { $desktop = $c.Node; $desktopFile = $f.FullName }
        'android' { $android = $c.Node }
    }
}

if (-not $web) { throw 'Web/server OAuth client JSON (root "web") not found in .secrets.' }
if (-not $desktop) { throw 'Desktop OAuth client JSON (root "installed" with client_secret) not found in .secrets.' }

$found = @()
if ($android) { $found += 'Android' }
$found += 'Desktop'
$found += 'Web'
Write-Host ("Discovered OAuth clients by shape: {0}" -f ($found -join ', ')) -ForegroundColor Green

# --------------------------------------------------------- Android keystore.properties
$ksPath = Join-Path $RepoRoot 'android/keystore.properties'
if (Test-Path $ksPath) {
    $lines = Get-Content $ksPath
    $webId = $web.client_id
    $set = $false
    $out = foreach ($line in $lines) {
        if ($line -match '^\s*googleWebClientId\s*=') { $set = $true; "googleWebClientId=$webId" }
        else { $line }
    }
    if (-not $set) { $out += "googleWebClientId=$webId" }
    Set-Content -Path $ksPath -Value $out -Encoding ASCII
    Write-Host 'Updated android/keystore.properties: googleWebClientId (Web client id).' -ForegroundColor Green
} else {
    Write-Host 'android/keystore.properties not found; skipped Android wiring.' -ForegroundColor Yellow
}

# --------------------------------------------------------- infra local parameters
$audiences = @($web.client_id, $desktop.client_id) -join ','
$localParams = [ordered]@{
    '$schema'       = 'https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#'
    contentVersion  = '1.0.0.0'
    parameters      = [ordered]@{
        googleAllowedAudiences = [ordered]@{ value = $audiences }
        allowlistedEmail       = [ordered]@{ value = $OwnerEmail }
    }
}
$infraLocal = Join-Path $RepoRoot 'infra/main.parameters.local.json'
($localParams | ConvertTo-Json -Depth 6) | Set-Content -Path $infraLocal -Encoding UTF8
Write-Host 'Wrote infra/main.parameters.local.json (Web + Desktop audiences, owner email).' -ForegroundColor Green

# --------------------------------------------------------- Windows desktop runtime config
if (-not $SkipDesktopStage) {
    $stageDir = Join-Path $RepoRoot 'windows/installer/staging'
    New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
    $dest = Join-Path $stageDir 'google-desktop-client.json'
    Copy-Item -Path $desktopFile -Destination $dest -Force
    Write-Host 'Staged Desktop OAuth client JSON for the installer (git-ignored).' -ForegroundColor Green
}

Write-Host "`nDone. No client IDs or secrets were printed. All outputs are git-ignored." -ForegroundColor Cyan
