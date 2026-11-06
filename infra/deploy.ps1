#requires -Version 5.1
<#
.SYNOPSIS
    Repeatable two-phase deployment of the cot-voice-recorder Azure backend.

.DESCRIPTION
    Phase 1  : provision all infrastructure (VNet, private Storage, Web PubSub,
               Log Analytics, user-assigned identity, RBAC, Container Apps env) with
               deployApps=false.
    Image    : use an already published immutable public GHCR digest. Image
               publishing belongs to the trusted-main GitHub Actions workflow.
    Phase 2  : deploy the API + worker Container Apps and the cleanup Job with
               deployApps=true, pointing at the pushed image.

    The whole flow is idempotent: re-running converges to the declared state.

.NOTES
    Requires: Azure CLI (az), logged in, with the target subscription selected.
    Storage is never exposed publicly; the API only becomes healthy once RBAC and
    private DNS have propagated (allow a few minutes on first deploy).
#>
[CmdletBinding()]
param(
    [string] $SubscriptionId = '673af34d-6b28-41dc-bc7b-f507418045e6',
    [string] $Location = 'swedencentral',
    [string] $SpeechLocation = 'northeurope',
    [string] $EnvironmentName = 'dev',
    [string] $ProjectName = 'cot-voice-recorder',
    [string] $ContainerImage = '',
    # Rust is the source default; executing this script still deploys real resources.
    [ValidateSet('Rust')]
    [string] $BackendRuntime = 'Rust',
    # Optional git-ignored parameters file with real OAuth audiences / owner email.
    [string] $LocalParametersFile = '',
    [switch] $InfraOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Keep Azure CLI output encoding stable on Windows.
$env:PYTHONUTF8 = '1'
$env:PYTHONIOENCODING = 'utf-8'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

$InfraDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$MainBicep = Join-Path $InfraDir 'main.bicep'
$BaseParams = Join-Path $InfraDir 'main.parameters.json'

function Invoke-Az {
    param([Parameter(Mandatory)][string[]] $Args, [string] $What = 'az command')
    Write-Host "az $($Args -join ' ')" -ForegroundColor DarkGray
    $output = & az @Args --subscription $SubscriptionId
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed (exit $LASTEXITCODE)."
    }
    return $output
}

if (-not $InfraOnly -and $ContainerImage -notmatch '^ghcr\.io/[a-z0-9._/-]+@sha256:[a-f0-9]{64}$') {
    throw 'Publish with GitHub Actions first and supply -ContainerImage as an immutable public GHCR digest. This script never builds or pushes an ACR image.'
}

Write-Host "=== cot-voice-recorder deploy ===" -ForegroundColor Cyan
Write-Host "Subscription : $SubscriptionId"
Write-Host "Location     : $Location"
Write-Host "Speech region: $SpeechLocation"
Write-Host "Environment  : $EnvironmentName"
Write-Host "Image        : $ContainerImage"
Write-Host ""

# Build the common parameter argument list (base file + optional local override).
$paramArgs = @('--parameters', "@$BaseParams")
if ($LocalParametersFile) {
    if (-not (Test-Path $LocalParametersFile)) { throw "LocalParametersFile not found: $LocalParametersFile" }
    $paramArgs += @('--parameters', "@$LocalParametersFile")
    Write-Host "Using local override parameters: $LocalParametersFile" -ForegroundColor Yellow
}

# --------------------------------------------------------------- Phase 1
Write-Host "`n--- Phase 1: provision infrastructure (deployApps=false) ---" -ForegroundColor Cyan
$phase1Args = @(
    'deployment','sub','create',
    '--name','cotvr-infra',
    '--location', $Location,
    '--template-file', $MainBicep
) + $paramArgs + @(
    '--parameters','deployApps=false',
    "--parameters","location=$Location",
    "--parameters","speechLocation=$SpeechLocation",
    "--parameters","environmentName=$EnvironmentName",
    "--parameters","projectName=$ProjectName",
    '--output','json'
)
$phase1Raw = Invoke-Az $phase1Args 'Phase 1 deployment'
$phase1 = ($phase1Raw | Out-String | ConvertFrom-Json)
$o = $phase1.properties.outputs

$storageAccountName = $o.storageAccountName.value
Write-Host "Storage      : $storageAccountName" -ForegroundColor Green

$imageRef = $ContainerImage

# ----------------------------------------------------------------- Build


if ($InfraOnly) {
    Write-Host "`nInfraOnly set - skipping app deployment." -ForegroundColor Yellow
    return
}

# --------------------------------------------------------------- Phase 2
Write-Host "`n--- Phase 2: deploy API / worker / cleanup job (deployApps=true) ---" -ForegroundColor Cyan
$phase2Args = @(
    'deployment','sub','create',
    '--name','cotvr-apps',
    '--location', $Location,
    '--template-file', $MainBicep
) + $paramArgs + @(
    '--parameters','deployApps=true',
    "--parameters","containerImage=$imageRef",
    "--parameters","location=$Location",
    "--parameters","speechLocation=$SpeechLocation",
    "--parameters","environmentName=$EnvironmentName",
    "--parameters","projectName=$ProjectName",
    '--output','json'
)
$phase2Raw = Invoke-Az $phase2Args 'Phase 2 deployment'
$phase2 = ($phase2Raw | Out-String | ConvertFrom-Json)
$out = $phase2.properties.outputs

# ---------------------------------------------------------------- Summary
Write-Host "`n=== Deployment outputs ===" -ForegroundColor Cyan
[pscustomobject]@{
    ResourceGroup       = $out.resourceGroupNameOut.value
    ApiBaseUrl          = $out.apiBaseUrl.value
    ApiFqdn             = $out.apiFqdn.value
    StorageAccount      = $out.storageAccountName.value
    BlobEndpoint        = $out.blobEndpoint.value
    QueueEndpoint       = $out.queueEndpoint.value
    TableEndpoint       = $out.tableEndpoint.value
    ManagedIdentityId   = $out.userAssignedIdentityId.value
    ManagedIdentityGuid = $out.userAssignedClientId.value
    WebPubSubName       = $out.webPubSubName.value
    WebPubSubEndpoint   = $out.webPubSubEndpoint.value
    SpeechAccount       = $out.speechAccountName.value
    SpeechEndpoint      = $out.speechEndpoint.value
    ContainerEnv        = $out.managedEnvironmentName.value
    LogAnalytics        = $out.logAnalyticsName.value
    Image               = $imageRef
} | Format-List

$rg = $out.resourceGroupNameOut.value
Write-Host "`n=== Useful commands ===" -ForegroundColor Cyan
Write-Host "API logs   : az containerapp logs show -g $rg -n ca-api --follow"
Write-Host "Worker logs: az containerapp logs show -g $rg -n ca-worker --follow"
Write-Host "Run cleanup: az containerapp job start -g $rg -n caj-cleanup"
Write-Host "Update Google settings (after OAuth registration):"
Write-Host "  az containerapp update -g $rg -n ca-api --set-env-vars VR_GOOGLE_ALLOWED_AUDIENCES=<ids> VR_ALLOWLISTED_EMAIL=<email>"
Write-Host "  (also update ca-worker; or re-run deploy.ps1 -LocalParametersFile main.parameters.local.json)"
Write-Host "`nDone." -ForegroundColor Green
