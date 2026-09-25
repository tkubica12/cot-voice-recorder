# infra/ — Infrastructure

**Owner:** infrastructure/deployment.
**Tech:** Azure — Container Apps, Storage (Blob/Queue/Table), Web PubSub, ACR, Log
Analytics, Azure Speech, AI Foundry (existing). IaC: **Bicep**, orchestrated by **`infra/deploy.ps1`**
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
├── crcotvr<token>         ACR (Standard, admin disabled, MI pull)
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
system-assigned) makes RBAC, ACR pull and the KEDA queue authorization **deterministic
and pre-created** before the apps start — no chicken-and-egg on first deploy.

Least-privilege data-plane roles assigned to the identity (no keys anywhere):

| Scope | Role |
|-------|------|
| Storage account | Storage Blob Data Contributor |
| Storage account | Storage Queue Data Contributor |
| Storage account | Storage Table Data Contributor |
| ACR | AcrPull |
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

- **ACR** is Standard with `publicNetworkAccess: Enabled`. ACR Private Link requires the
  **Premium** SKU; Standard keeps `az acr build` and managed-identity pulls working over
  the public endpoint while admin user is disabled and there are no registry credentials.
- **Web PubSub** keeps public client connectivity **by design** — the Windows tray client
  connects directly from the Internet using a short-lived, user-scoped client URL that the
  backend mints with its managed identity (`disableLocalAuth: true`, so no access keys).

Neither exception touches Storage, which remains fully private.

## Prerequisites

- Azure CLI (`az`) logged in, with the target subscription selected.
- Bicep CLI (bundled with `az`). Docker is **not** required — the image is built in the
  cloud by `az acr build`.
- Permission to create resource groups and role assignments in the subscription.

## Deploy

`infra/deploy.ps1` is the authoritative, idempotent, **two-phase** orchestrator:

1. **Phase 1** (`deployApps=false`) — provision all infrastructure + identity + RBAC.
2. **Build** — `az acr build` publishes `backend/Dockerfile` to the new ACR.
3. **Phase 2** (`deployApps=true`) — deploy `ca-api`, `ca-worker`, `caj-cleanup`.

```powershell
cd infra
# Full provision + build + deploy (idempotent; re-run any time):
./deploy.ps1

# Common options:
./deploy.ps1 -ImageTag v3                 # explicit image tag (default: git-<sha>)
./deploy.ps1 -SkipBuild -ImageTag v3      # redeploy apps with an existing image
./deploy.ps1 -InfraOnly                   # provision + build, skip the apps
```

The script prints the API base URL and all resource outputs at the end.

> **Package feed proxy.** Direct PyPI TLS is broken on the build workstation, so the
> committed `backend/uv.lock` references the public, unauthenticated
> `https://packagefeedproxy.microsoft.io/pypi/simple/`. `deploy.ps1` passes it to the
> cloud build as `--build-arg UV_DEFAULT_INDEX=...`. No credentials are involved.

### Parameters and secrets

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

`azure.yaml` maps the `api` service for `azd` familiarity. `azd provision` can create the
infrastructure, but use `deploy.ps1` for the full flow: the Container Apps pull the image
from the ACR created in phase 1, and Storage is private, so a single pass cannot both
build the image and start healthy apps.

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
- ACR **Standard** avoids Premium's Private Link cost; the tradeoff is a public (but
  credential-less, MI-only) registry endpoint.
