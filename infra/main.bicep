// =============================================================================
// cot-voice-recorder — Azure infrastructure (subscription-scoped entry point)
//
// Creates the target resource group and deploys all backend infrastructure into
// it, plus a cross-resource-group data-plane role assignment on the existing
// Azure AI Foundry account.
//
// Two-phase orchestration (see infra/deploy.ps1):
//   Phase 1: deployApps=false  -> provision infra, ACR, identity and RBAC.
//   (build & push the backend image to the new ACR)
//   Phase 2: deployApps=true   -> deploy API / worker container apps + cleanup job.
//
// Idempotent: re-running either phase converges to the declared state.
// =============================================================================

targetScope = 'subscription'

@description('Azure region for all new resources.')
param location string = 'swedencentral'

@description('Short project name used for tagging and resource naming.')
param projectName string = 'cot-voice-recorder'

@description('Environment tag / azd environment name (e.g. dev).')
param environmentName string = 'dev'

@description('Target resource group name (created if absent).')
param resourceGroupName string = 'rg-cot-voice-recorder'

@description('Resource group that holds the existing Azure AI Foundry account.')
param foundryResourceGroup string = 'ai-services'

@description('Existing Azure AI Foundry (Cognitive Services / AIServices) account name.')
param foundryAccountName string = 'tomaskubica-foundry-resource'

@description('Existing Azure AI Foundry endpoint.')
param foundryEndpoint string = 'https://tomaskubica-foundry-resource.cognitiveservices.azure.com/'

@description('Whether to deploy the Container Apps (API + worker) and the cleanup job. False on the first pass (before the image exists).')
param deployApps bool = false

@description('Fully qualified container image reference (registry/repo:tag) for the backend. Required when deployApps=true.')
param containerImage string = ''

@description('Comma-separated Google OIDC audiences. Deny-all placeholder until real client IDs are registered.')
param googleAllowedAudiences string = 'not-configured'

@description('Allowlisted owner email. Deny-all placeholder until the real owner email is configured.')
param allowlistedEmail string = 'nobody@invalid.example'

var tags = {
  project: projectName
  environment: environmentName
  'azd-env-name': environmentName
  managedBy: 'bicep'
}

// Short, stable, globally-unique-ish token for names that must be globally unique.
var resourceToken = take(uniqueString(subscription().id, projectName, environmentName), 8)

resource rg 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: resourceGroupName
  location: location
  tags: tags
}

module resources 'modules/resources.bicep' = {
  name: 'resources'
  scope: rg
  params: {
    location: location
    tags: tags
    resourceToken: resourceToken
    foundryEndpoint: foundryEndpoint
  }
}

module apps 'modules/apps.bicep' = if (deployApps) {
  name: 'apps'
  scope: rg
  params: {
    location: location
    tags: tags
    containerImage: containerImage
    managedEnvironmentId: resources.outputs.managedEnvironmentId
    acrLoginServer: resources.outputs.acrLoginServer
    userAssignedIdentityId: resources.outputs.userAssignedIdentityId
    userAssignedClientId: resources.outputs.userAssignedClientId
    storageAccountName: resources.outputs.storageAccountName
    webPubSubEndpoint: resources.outputs.webPubSubEndpoint
    foundryEndpoint: foundryEndpoint
    googleAllowedAudiences: googleAllowedAudiences
    allowlistedEmail: allowlistedEmail
  }
}

// Cross-RG least-privilege data-plane role on the existing Foundry account so the
// runtime identity can invoke the existing model deployments (no keys).
module foundryRbac 'modules/foundry-rbac.bicep' = {
  name: 'foundry-rbac'
  scope: resourceGroup(foundryResourceGroup)
  params: {
    foundryAccountName: foundryAccountName
    principalId: resources.outputs.userAssignedPrincipalId
  }
}

output resourceGroupNameOut string = rg.name
output location string = location
output acrName string = resources.outputs.acrName
output acrLoginServer string = resources.outputs.acrLoginServer
output storageAccountName string = resources.outputs.storageAccountName
output blobEndpoint string = resources.outputs.blobEndpoint
output queueEndpoint string = resources.outputs.queueEndpoint
output tableEndpoint string = resources.outputs.tableEndpoint
output userAssignedIdentityId string = resources.outputs.userAssignedIdentityId
output userAssignedClientId string = resources.outputs.userAssignedClientId
output userAssignedPrincipalId string = resources.outputs.userAssignedPrincipalId
output webPubSubName string = resources.outputs.webPubSubName
output webPubSubEndpoint string = resources.outputs.webPubSubEndpoint
output managedEnvironmentName string = resources.outputs.managedEnvironmentName
output logAnalyticsName string = resources.outputs.logAnalyticsName
output apiFqdn string = deployApps ? apps.outputs.apiFqdn : ''
output apiBaseUrl string = deployApps ? 'https://${apps.outputs.apiFqdn}' : ''
