#requires -Version 5.1
<#
.SYNOPSIS
    Repeatable two-phase deployment of the cot-voice-recorder Azure backend.

.DESCRIPTION
    Phase 1  : provision all infrastructure (VNet, private Storage, ACR, Web PubSub,
               Log Analytics, user-assigned identity, RBAC, Container Apps env) with
               deployApps=false.
    Build    : build & push the backend image to the freshly-created ACR with
               `az acr build` (managed-identity pull; no registry credentials).
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
    [string] $ImageRepository = 'voice-recorder-backend',
    [string] $ImageTag = '',
    # Public, unauthenticated Microsoft package feed proxy (direct PyPI TLS is
    # broken on this workstation; the committed uv.lock references it). Passed as a
    # Docker build-arg to the cloud `az acr build`. No credentials are involved.
    [string] $PackageFeedProxy = 'https://packagefeedproxy.microsoft.io/pypi/simple/',
    # Optional git-ignored parameters file with real OAuth audiences / owner email.
    [string] $LocalParametersFile = '',
    [switch] $SkipBuild,
    [switch] $InfraOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Windows consoles default to cp1252; `az acr build` streams UTF-8 build logs which
# can crash the CLI's log writer ('charmap' codec error). Force UTF-8 for the process.
$env:PYTHONUTF8 = '1'
$env:PYTHONIOENCODING = 'utf-8'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

$InfraDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $InfraDir
$BackendDir = Join-Path $RepoRoot 'backend'
$MainBicep = Join-Path $InfraDir 'main.bicep'
$BaseParams = Join-Path $InfraDir 'main.parameters.json'

function Invoke-Az {
    param([Parameter(Mandatory)][string[]] $Args, [string] $What = 'az command')
    Write-Host "az $($Args -join ' ')" -ForegroundColor DarkGray
    $output = & az @Args
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed (exit $LASTEXITCODE)."
    }
    return $output
}

if (-not $ImageTag) {
    $sha = ''
    try { $sha = (& git -C $RepoRoot rev-parse --short HEAD 2>$null) } catch { $sha = '' }
    if ($LASTEXITCODE -ne 0) { $sha = '' }
    if ($sha) { $ImageTag = "git-$sha" } else { $ImageTag = (Get-Date -Format 'yyyyMMddHHmmss') }
}

Write-Host "=== cot-voice-recorder deploy ===" -ForegroundColor Cyan
Write-Host "Subscription : $SubscriptionId"
Write-Host "Location     : $Location"
Write-Host "Speech region: $SpeechLocation"
Write-Host "Environment  : $EnvironmentName"
Write-Host "Image tag    : $ImageTag"
Write-Host ""

Invoke-Az @('account','set','--subscription', $SubscriptionId) 'account set' | Out-Null

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

$acrName = $o.acrName.value
$acrLoginServer = $o.acrLoginServer.value
$storageAccountName = $o.storageAccountName.value
Write-Host "ACR          : $acrName ($acrLoginServer)" -ForegroundColor Green
Write-Host "Storage      : $storageAccountName" -ForegroundColor Green

$imageRef = "$acrLoginServer/$ImageRepository`:$ImageTag"

# ----------------------------------------------------------------- Build
if (-not $SkipBuild) {
    Write-Host "`n--- Build & push image via az acr build ---" -ForegroundColor Cyan
    Push-Location $BackendDir
    try {
        $buildArgs = @(
            'acr','build',
            '--registry', $acrName,
            '--image', "$ImageRepository`:$ImageTag",
            '--file','Dockerfile'
        )
        if ($PackageFeedProxy) {
            $buildArgs += @('--build-arg', "UV_DEFAULT_INDEX=$PackageFeedProxy")
        }
        $buildArgs += @('.')
        # `az acr build` can crash the local CLI while streaming UTF-8 logs on a
        # Windows cp1252 console even though the server-side build succeeds. Tolerate
        # that specific failure by verifying the run status instead of blind trust.
        Write-Host "az $($buildArgs -join ' ')" -ForegroundColor DarkGray
        & az @buildArgs
        $buildExit = $LASTEXITCODE
        if ($buildExit -ne 0) {
            Write-Warning "az acr build exited $buildExit (likely a Windows log-stream encoding crash). Verifying server-side run status..."
            # The client can die before the server-side run finishes, so poll rather
            # than sample once: a still-running build would otherwise look like a
            # failure and abort an otherwise healthy deployment.
            $status = ''
            $deadline = (Get-Date).AddMinutes(30)
            while ((Get-Date) -lt $deadline) {
                $status = (& az acr task list-runs --registry $acrName --top 20 `
                        --query "[?outputImages[?tag=='$ImageTag' && repository=='$ImageRepository']] | [0].status" -o tsv)
                if (-not $status) {
                    # Output images are only recorded once a run succeeds; fall back to
                    # the newest run so a failed/queued build is still observable.
                    $status = (& az acr task list-runs --registry $acrName --top 1 --query "[0].status" -o tsv)
                }
                if ($status -in @('Succeeded', 'Failed', 'Canceled', 'Error', 'Timeout')) { break }
                Write-Host "  ACR run status: '$status' - waiting..." -ForegroundColor DarkGray
                Start-Sleep -Seconds 15
            }
            if ($status -ne 'Succeeded') { throw "ACR build did not succeed (server status: '$status')." }
            Write-Host "Server-side ACR run status: Succeeded (ignored client log-stream crash)." -ForegroundColor Yellow
        }
    }
    finally {
        Pop-Location
    }
    Write-Host "Image pushed : $imageRef" -ForegroundColor Green
}
else {
    Write-Host "SkipBuild set - assuming $imageRef already exists." -ForegroundColor Yellow
}

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
    Acr                 = $out.acrName.value
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
