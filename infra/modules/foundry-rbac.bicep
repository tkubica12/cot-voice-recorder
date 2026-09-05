// =============================================================================
// Cross-resource-group role assignment: grant the runtime managed identity a
// least-privilege data-plane role on the EXISTING Azure AI Foundry account so it
// can invoke the existing model deployments (transcribe + refine) without keys.
//
// "Cognitive Services User" grants inference data actions across the AIServices
// account (audio transcription + chat completions) — sufficient to call the
// gpt-5.6-luna / gpt-5.6-terra and the gpt-4o-transcribe fallback deployment.
// =============================================================================

@description('Existing Foundry (Cognitive Services / AIServices) account name.')
param foundryAccountName string

@description('Principal id of the runtime managed identity.')
param principalId string

var roleCognitiveServicesUser = 'a97b65f3-24c7-4388-baec-2e87135dc908'

resource foundry 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = {
  name: foundryAccountName
}

resource raFoundry 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(foundry.id, principalId, roleCognitiveServicesUser)
  scope: foundry
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleCognitiveServicesUser)
  }
}

output roleAssignmentId string = raFoundry.id
