# infra/ — Infrastructure

**Owner:** infrastructure/deployment.
**Tech:** Azure — Container Apps, Storage (Blob/Queue/Table), Web PubSub, Log
Analytics, Azure Speech, AI Foundry (existing); public **GHCR** image publishing.
IaC: **Bicep**, orchestrated by **`infra/deploy.ps1`**
(two-phase) with an `azure.yaml` for Azure Developer CLI familiarity.

Provisions what [`../docs/architecture.md`](../docs/architecture.md) requires. The API
surface is fixed by [`../openapi/voice-recorder.yaml`](../openapi/voice-recorder.yaml).

## Deployed topology

```
rg-cot-voice-recorder (Sweden Central)
├── vnet-cotvr (10.20.0.0/16)
│   ├── infra              10.20.0.0/23  delegated Microsoft.App/environments
│   └── private-endpoints  10.20.2.0/24  PE network policies disabled
├── stcotvr<token>         Storage — public access DISABLED, shared key DISABLED,
│   ├── blob: audio, transcripts, raw       HTTPS-only, TLS1.2, default deny
│   ├── queue: work
│   └── table: recordings, chunks, transcripts
├── pe-*-{blob,queue,table} + privatelink.{blob,queue,table}.core.windows.net zones
├── aispeech-cotvr-<token>  Azure Speech (North Europe) — public access/local auth DISABLED
├── pe-aispeech-* + privatelink.cognitiveservices.azure.com zone
├── wps-cotvr-<token>      Web PubSub (Free_F1, hub `transcripts`, local auth disabled)
├── id-cotvr-<token>       user-assigned managed identity (SHARED by all runtime apps)
├── log-cotvr-<token>      Log Analytics (Container Apps logs)
├── cae-cotvr              Container Apps env — VNet-integrated, EXTERNAL ingress
│   ├── ca-api             API, external HTTPS ingress, HTTP scale 1→2 (warm for dictation)
│   ├── ca-worker          no ingress, KEDA azure-queue (MI) scale 0→2
│   └── caj-cleanup        scheduled Job, hourly cron `0 * * * *`
└── (cross-RG) Cognitive Services User on ai-services/tomaskubica-foundry-resource
```

### Identity model — one shared user-assigned managed identity

All three runtime components (`ca-api`, `ca-worker`, `caj-cleanup`) run as the **same
user-assigned managed identity** (`id-cotvr-<token>`). Its client id is injected as
`AZURE_CLIENT_ID` so `DefaultAzureCredential` selects it. A shared UAMI (vs. per-app
system-assigned) makes RBAC and the KEDA queue authorization **deterministic
and pre-created** before the apps start — no chicken-and-egg on first deploy.

Least-privilege data-plane roles assigned to the identity (no keys anywhere):

| Scope | Role |
|-------|------|
| Storage account | Storage Blob Data Contributor |
| Storage account | Storage Queue Data Contributor |
| Storage account | Storage Table Data Contributor |
| Web PubSub | Web PubSub Service Owner (negotiate + send) |
| Foundry account (RG `ai-services`) | Cognitive Services User (invoke deployments) |
| Azure Speech account | Cognitive Services User (invoke MAI-Transcribe-2) |

### Networking & the storage privacy guarantee

Storage is the hard policy boundary: `publicNetworkAccess: Disabled`,
`allowSharedKeyAccess: false`, `defaultAction: Deny`, HTTPS-only, TLS 1.2. Blob, Queue
and Table are reachable **only** through their private endpoints in the
`private-endpoints` subnet; the three `privatelink.*.core.windows.net` zones are linked
to the VNet so the SDK's normal `*.core.windows.net` host names resolve to the private
IPs (10.20.2.4–6). The Container Apps environment is injected into the `infra` subnet, so
API/worker/job traffic to Storage stays private. Control-plane creation of the
containers/queues/tables works despite disabled public access + shared key because it
goes through ARM, not the data plane.

The Azure Speech resource follows the same closed-network pattern: public access and
local key authentication are disabled, its `account` private endpoint resolves through
`privatelink.cognitiveservices.azure.com`, and the worker authenticates only with the
shared managed identity. It is deployed in North Europe because MAI transcription is not
available in Sweden Central.

### Deliberate network exceptions (Storage never weakened)

- **GHCR** serves the public, credential-free Rust image over HTTPS. The image is
  pulled by its verified digest; no registry PAT, Azure registry or AcrPull role
  is required. The previous Standard ACR was explicitly retired.
- **Web PubSub** keeps public client connectivity **by design** — the Windows tray client
  connects directly from the Internet using a short-lived, user-scoped client URL that the
  backend mints with its managed identity (`disableLocalAuth: true`, so no access keys).

Neither exception touches Storage, which remains fully private.

## Prerequisites

- Azure CLI (`az`) logged in, with the target subscription selected.
- Bicep CLI (bundled with `az`). Local Docker is **not** required — the image is
  built and published by the trusted-main GitHub Actions workflow.
- Permission to create resource groups and role assignments in the subscription.

## Deploy

`infra/deploy.ps1` is the authoritative, idempotent, **two-phase** orchestrator:

1. **Phase 1** (`deployApps=false`) — provision all infrastructure + identity + RBAC.
2. **Image** — supply an already published public GHCR digest from `publish-cloud`.
3. **Phase 2** (`deployApps=true`) — provision `ca-api`, `ca-worker`, `caj-cleanup`.

```powershell
cd infra
# Separate full-infrastructure operation; NOT the existing-service image cutover:
./deploy.ps1 -ContainerImage 'ghcr.io/tkubica12/cot-voice-recorder@sha256:<verified-digest>'

# Common options:
./deploy.ps1 -InfraOnly                   # provision infrastructure only
```

The script prints the API base URL and all resource outputs at the end.

The source default is now Rust (`-BackendRuntime Rust` remains accepted).
Executing this script is still a real infrastructure/RBAC/build/deployment
operation, not an offline check. The November 5 production cutover used only
existing-service image updates, not this full provisioning script.
Independent Python SDK test tools use the approved
Microsoft feed and `scripts/uv.lock`; no Python runtime is packaged in the image.

## Publishing and existing-service image updates

The trusted-main `.github/workflows/publish-cloud.yml` is the only image build/push
route. It uses a tracked-source allowlist and ephemeral `GITHUB_TOKEN` to publish
`ghcr.io/tkubica12/cot-voice-recorder:sha-<full-commit>`. Public package visibility
and anonymous manifest/config access must be verified; public repository visibility
alone is insufficient. Production uses the immutable digest.

Existing deployments need only image/necessary entrypoint updates to `ca-api`,
`ca-worker` and `caj-cleanup`, with original image/commands/args recorded for
rollback. Preserve their UAMI, private network, URL, scaling and hourly schedule;
do not rerun this full provisioning script for that update. An anonymous
public GHCR image needs no PAT or new managed-identity role. The actual rollout
succeeded without adding a GHCR registry entry; the old ACR entry was retained.
Neither the publish workflow nor CI deploys Azure automatically. The former ACR
and old cloud-image rollback were explicitly retired on November 6.

### Current verified deployment

`ca-api`, `ca-worker` and `caj-cleanup` run the same immutable public image
`ghcr.io/tkubica12/cot-voice-recorder@sha256:143f87adcf1f115215e01f7258ec08fb97b3725e9221ef759b3b943a58f40e02`,
published by [the trusted-main workflow](https://github.com/tkubica12/cot-voice-recorder/actions/runs/42093843593)
from `fd746f8625d3f7bf845ea899bba8f66acbe7c3b6`.
Anonymous manifest/config reads verified its digest, source label, Linux/amd64
architecture, native entrypoint and unprivileged user before rollout.

Actual API liveness/readiness returned 200; an unauthenticated transcript request
returned 401 without reading data. Readiness follows successful managed-identity
checks of the three private Blob containers, work queue metadata and three Tables.
Storage public networking/shared keys remain disabled; Speech remains private
with local authentication disabled. Existing public client-facing Web PubSub
access remains unchanged and still has local authentication disabled.
Identity, launch arguments, registry references, environment names/secret
references, ingress URL, scaling and hourly cron matched the pre-update snapshots.
The resource-group `nesahat` lock was restored to its original ReadOnly level,
scope, notes and owners. Worker replicas were zero with an earlier empty-queue
observation; no synthetic work or new model inference was issued.

The next natural cleanup execution, `caj-cleanup-29898540`, ran the new digest
at `2026-11-05T21:00:00Z` and succeeded at `21:00:34Z`. No ad-hoc cleanup was
triggered. Existing data retention policy was not changed.

### Verified ACR retirement

On November 6, the owner explicitly confirmed no other consumers and accepted
the loss of direct rollback to the 15 old image versions. The exact Standard
registry `crcotvrspddxkti` in `rg-cot-voice-recorder` was deleted. Its three unused
registry entries were removed from the API, worker and cleanup Job. The former
AcrPull assignment is absent; the runtime identity's six other assignments
remain. The original ReadOnly lock was restored with the same scope, name, notes
and owners. Registry absence, empty registry arrays, unchanged non-ACR service
configuration and actual API liveness/readiness were independently verified.

The existing Bicep compiler built the pre/post module graphs. They differ only
by the registry and its AcrPull assignment, associated outputs/parameters and
three registry arrays; other resource properties, roles and module wiring are
unchanged. No full Bicep deployment was applied. Future provisioning no longer
recreates an ACR or expects its outputs. The Rust/Docker/publish graph was not
changed and no replacement image was built.

Historical inactive revisions may still describe the old ACR image, but that
image is no longer available for reactivation. No inactive revision, Git
history, private Windows backup, current GHCR image or customer data was deleted
by this retirement. Removing the registry ends its ongoing resource charge;
it does not refund historical usage.

### Parameters and secrets

Android 1.3 additionally requires the updated queue worker. It renews its invisible
queue work and recording lease for an active model session. Queue scaling must count
both visible and invisible messages (`queueLengthStrategy=all`, the KEDA default),
so an active session is not mistaken for an empty queue. Minimum worker replicas
can stay at zero; source audio/attempt results never depend on container memory.
`VR_RECORDING_STREAM_DEPLOYMENT` and `VR_RECORDING_STREAM_REPLAY_SPEED` select the
deployment and bounded replay pacing. Update worker/API/cleanup before the APK.

Windows 1.5 requires the existing Foundry resource to contain a
`MAI-Transcribe-2-Streaming` deployment. The API uses the existing managed identity
and Cognitive Services User grant; no desktop Entra login/key, new public Storage
access, or Speech network-rule change is required. `VR_DICTATION_STREAM_ENABLED`
and `VR_DICTATION_STREAM_DEPLOYMENT` are declared in the app environment.
Deploy the updated **API and cleanup job** image: the live relay is storage-free,
but the separate completed-text audit uses new immutable blob paths and 48-hour
cleanup. The Android worker need not restart for this desktop-only change.
Both `pip` (uv bootstrap) and `uv` use the approved Microsoft package feed during
container builds; an alternate approved project feed can override both.

- `infra/main.parameters.json` holds **non-secret** dev values (region, names, Foundry
  reference) and the **deny-all auth placeholder** (`not-configured` audience,
  `nobody@invalid.example`). The service starts but no real Google token can authenticate.
- **User-specific OAuth/email stay local and git-ignored.** After you register the Google
  OAuth clients, copy `main.parameters.local.json.example` → `main.parameters.local.json`
  (matches the `*.local.json` ignore rule), fill in the real audiences/email, and run:

  ```powershell
  ./deploy.ps1 -SkipBuild -ImageTag <tag> -LocalParametersFile main.parameters.local.json
  ```

  Or update the two apps directly:

  ```powershell
  az containerapp update -g rg-cot-voice-recorder -n ca-api `
    --set-env-vars VR_GOOGLE_ALLOWED_AUDIENCES=<id1>,<id2> VR_ALLOWLISTED_EMAIL=<email>
  az containerapp update -g rg-cot-voice-recorder -n ca-worker `
    --set-env-vars VR_GOOGLE_ALLOWED_AUDIENCES=<id1>,<id2> VR_ALLOWLISTED_EMAIL=<email>
  ```

### Azure Developer CLI (optional)

`azure.yaml` keeps infrastructure-only `azd provision` familiarity; it does not
map an implicit image build/deploy service. Image publishing belongs to GitHub
Actions. `deploy.ps1` uses the already published GHCR digest for an explicitly
authorized full-infrastructure setup. Its two phases establish private networking,
identity and RBAC before starting the applications; existing image updates do not
reprovision that infrastructure.

## Validate / operate

```powershell
$rg='rg-cot-voice-recorder'
# Storage hardening
az storage account show -g $rg -n <storage> --query "{public:publicNetworkAccess, sharedKey:allowSharedKeyAccess, tls:minimumTlsVersion}"
# Private endpoint connection states (Approved)
az storage account show -g $rg -n <storage> --query "privateEndpointConnections[].privateLinkServiceConnectionState.status"
# Private DNS records
az network private-dns record-set a list -g $rg -z privatelink.blob.core.windows.net -o table
# RBAC on the identity
az role assignment list --assignee <uami-principalId> --all -o table
# Revisions / scale
az containerapp revision list -g $rg -n ca-api -o table
# Logs (no secrets are logged)
az containerapp logs show -g $rg -n ca-api --follow
# Run cleanup on demand
az containerapp job start -g $rg -n caj-cleanup
# Health / auth smoke
curl https://<api-fqdn>/health/live      # 200
curl https://<api-fqdn>/health/ready     # 200
curl -i https://<api-fqdn>/v1/recordings/x   # 401 until Google auth is configured
```

## Existing resources (reference, not managed here)

- Resource group `ai-services`, Foundry account `tomaskubica-foundry-resource`
  (Sweden Central), endpoint
  `https://tomaskubica-foundry-resource.cognitiveservices.azure.com/`.
- Deployments: `gpt-6-luna` / `gpt-6-sol` (2026-09-22, GlobalStandard, capacity 500 each),
  `gpt-5.6-luna` / `gpt-5.6-terra` (2026-07-09) and `gpt-4o-transcribe`
  (2025-03-20) as the transcription fallback. This IaC references the existing
  Foundry account and does not manage its model deployments.

## Cost / tradeoffs

- Container Apps **Consumption** profile with min-replicas 0 → near-zero idle cost; cold
  start is absorbed by the async client design.
- Web PubSub **Free_F1** (no cost) is sufficient for one user.
- Four private endpoints + a VNet incur a small hourly PE cost; this is the price of the
  closed Storage firewall the subscription policy requires.
- The old Standard ACR has been removed. GHCR publishing and GitHub Actions usage
  are separate from Azure resource billing; historical ACR charges are not refunded.
