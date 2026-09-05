// =============================================================================
// Core infrastructure (resource-group scoped): networking, private Storage,
// ACR, Web PubSub, Log Analytics, user-assigned identity, RBAC and the
// Container Apps managed environment.
// =============================================================================

@description('Azure region.')
param location string

@description('Azure region for the MAI-Transcribe Speech resource.')
param speechLocation string

@description('Resource tags.')
param tags object

@description('Short unique token for globally-unique resource names.')
param resourceToken string

@description('Existing Foundry endpoint (informational / passed through).')
param foundryEndpoint string

// ------------------------------------------------------------------ names
var storageAccountName = 'stcotvr${resourceToken}'
var acrName = 'crcotvr${resourceToken}'
var webPubSubName = 'wps-cotvr-${resourceToken}'
var identityName = 'id-cotvr-${resourceToken}'
var vnetName = 'vnet-cotvr'
var logAnalyticsName = 'log-cotvr-${resourceToken}'
var managedEnvironmentName = 'cae-cotvr'
var speechAccountName = 'aispeech-cotvr-${resourceToken}'

// Built-in role definition IDs (resolved from Azure).
var roleBlobDataContributor = 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
var roleQueueDataContributor = '974c5e8b-45b9-4653-ba55-5f855dd0fb88'
var roleTableDataContributor = '0a9a7e1f-b9d0-4cc4-a60d-0319b160aaa3'
var roleAcrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'
var roleWebPubSubServiceOwner = '12cf5a90-567b-43ae-8102-96cf46c7d9b4'
var roleCognitiveServicesUser = 'a97b65f3-24c7-4388-baec-2e87135dc908'

var blobDnsZoneName = 'privatelink.blob.core.windows.net'
var queueDnsZoneName = 'privatelink.queue.core.windows.net'
var tableDnsZoneName = 'privatelink.table.core.windows.net'
var cognitiveServicesDnsZoneName = 'privatelink.cognitiveservices.azure.com'

// ------------------------------------------------------------ identity
resource uami 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: identityName
  location: location
  tags: tags
}

// -------------------------------------------------------- log analytics
resource logAnalytics 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: logAnalyticsName
  location: location
  tags: tags
  properties: {
    sku: {
      name: 'PerGB2018'
    }
    retentionInDays: 30
    features: {
      enableLogAccessUsingOnlyResourcePermissions: true
    }
  }
}

// -------------------------------------------------------------- network
resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: vnetName
  location: location
  tags: tags
  properties: {
    addressSpace: {
      addressPrefixes: [
        '10.20.0.0/16'
      ]
    }
    subnets: [
      {
        // Container Apps (workload profile) infrastructure subnet: delegated,
        // sized generously for the environment's internal components.
        name: 'infra'
        properties: {
          addressPrefix: '10.20.0.0/23'
          delegations: [
            {
              name: 'containerapps'
              properties: {
                serviceName: 'Microsoft.App/environments'
              }
            }
          ]
        }
      }
      {
        // Dedicated private-endpoint subnet with PE network policies disabled.
        name: 'private-endpoints'
        properties: {
          addressPrefix: '10.20.2.0/24'
          privateEndpointNetworkPolicies: 'Disabled'
        }
      }
    ]
  }
}

resource infraSubnet 'Microsoft.Network/virtualNetworks/subnets@2024-05-01' existing = {
  parent: vnet
  name: 'infra'
}

resource peSubnet 'Microsoft.Network/virtualNetworks/subnets@2024-05-01' existing = {
  parent: vnet
  name: 'private-endpoints'
}

// ----------------------------------------------------- private DNS zones
resource blobZone 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: blobDnsZoneName
  location: 'global'
  tags: tags
}
resource queueZone 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: queueDnsZoneName
  location: 'global'
  tags: tags
}
resource tableZone 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: tableDnsZoneName
  location: 'global'
  tags: tags
}
resource cognitiveServicesZone 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: cognitiveServicesDnsZoneName
  location: 'global'
  tags: tags
}

resource blobZoneLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: blobZone
  name: 'link-vnet'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: vnet.id
    }
  }
}
resource queueZoneLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: queueZone
  name: 'link-vnet'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: vnet.id
    }
  }
}
resource tableZoneLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: tableZone
  name: 'link-vnet'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: vnet.id
    }
  }
}
resource cognitiveServicesZoneLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: cognitiveServicesZone
  name: 'link-vnet'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: vnet.id
    }
  }
}

// -------------------------------------------------------------- storage
resource storage 'Microsoft.Storage/storageAccounts@2024-01-01' = {
  name: storageAccountName
  location: location
  tags: tags
  sku: {
    name: 'Standard_LRS'
  }
  kind: 'StorageV2'
  properties: {
    publicNetworkAccess: 'Disabled'
    allowSharedKeyAccess: false
    allowBlobPublicAccess: false
    supportsHttpsTrafficOnly: true
    minimumTlsVersion: 'TLS1_2'
    defaultToOAuthAuthentication: true
    networkAcls: {
      bypass: 'AzureServices'
      defaultAction: 'Deny'
    }
  }
}

resource blobService 'Microsoft.Storage/storageAccounts/blobServices@2024-01-01' = {
  parent: storage
  name: 'default'
}

resource audioContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2024-01-01' = {
  parent: blobService
  name: 'audio'
}
resource transcriptsContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2024-01-01' = {
  parent: blobService
  name: 'transcripts'
}
resource rawContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2024-01-01' = {
  parent: blobService
  name: 'raw'
}

resource queueService 'Microsoft.Storage/storageAccounts/queueServices@2024-01-01' = {
  parent: storage
  name: 'default'
}
resource workQueue 'Microsoft.Storage/storageAccounts/queueServices/queues@2024-01-01' = {
  parent: queueService
  name: 'work'
}

resource tableService 'Microsoft.Storage/storageAccounts/tableServices@2024-01-01' = {
  parent: storage
  name: 'default'
}
resource recordingsTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2024-01-01' = {
  parent: tableService
  name: 'recordings'
}
resource chunksTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2024-01-01' = {
  parent: tableService
  name: 'chunks'
}
resource transcriptsTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2024-01-01' = {
  parent: tableService
  name: 'transcripts'
}

// ------------------------------------------------ storage private endpoints
resource peBlob 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: 'pe-${storageAccountName}-blob'
  location: location
  tags: tags
  properties: {
    subnet: {
      id: peSubnet.id
    }
    privateLinkServiceConnections: [
      {
        name: 'blob'
        properties: {
          privateLinkServiceId: storage.id
          groupIds: [
            'blob'
          ]
        }
      }
    ]
  }
}
resource peBlobDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: peBlob
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'blob'
        properties: {
          privateDnsZoneId: blobZone.id
        }
      }
    ]
  }
}

resource peQueue 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: 'pe-${storageAccountName}-queue'
  location: location
  tags: tags
  properties: {
    subnet: {
      id: peSubnet.id
    }
    privateLinkServiceConnections: [
      {
        name: 'queue'
        properties: {
          privateLinkServiceId: storage.id
          groupIds: [
            'queue'
          ]
        }
      }
    ]
  }
}
resource peQueueDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: peQueue
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'queue'
        properties: {
          privateDnsZoneId: queueZone.id
        }
      }
    ]
  }
}

resource peTable 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: 'pe-${storageAccountName}-table'
  location: location
  tags: tags
  properties: {
    subnet: {
      id: peSubnet.id
    }
    privateLinkServiceConnections: [
      {
        name: 'table'
        properties: {
          privateLinkServiceId: storage.id
          groupIds: [
            'table'
          ]
        }
      }
    ]
  }
}
resource peTableDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: peTable
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'table'
        properties: {
          privateDnsZoneId: tableZone.id
        }
      }
    ]
  }
}

// --------------------------------------------- MAI transcription resource
// MAI-Transcribe-2 is exposed through Azure Speech Fast Transcription rather than
// an Azure OpenAI model deployment. North Europe is the nearest supported region.
resource speech 'Microsoft.CognitiveServices/accounts@2024-10-01' = {
  name: speechAccountName
  location: speechLocation
  tags: tags
  kind: 'AIServices'
  sku: {
    name: 'S0'
  }
  properties: {
    customSubDomainName: speechAccountName
    publicNetworkAccess: 'Disabled'
    disableLocalAuth: true
    networkAcls: {
      defaultAction: 'Deny'
    }
  }
}

resource peSpeech 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: 'pe-${speechAccountName}'
  location: location
  tags: tags
  properties: {
    subnet: {
      id: peSubnet.id
    }
    privateLinkServiceConnections: [
      {
        name: 'account'
        properties: {
          privateLinkServiceId: speech.id
          groupIds: [
            'account'
          ]
        }
      }
    ]
  }
}

resource peSpeechDns 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: peSpeech
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'account'
        properties: {
          privateDnsZoneId: cognitiveServicesZone.id
        }
      }
    ]
  }
}

// -------------------------------------------------- container registry
// Standard SKU with public network access + managed-identity pull. ACR private
// link requires Premium; Standard keeps `az acr build` automatable while the
// runtime identity authenticates pulls without admin credentials.
resource acr 'Microsoft.ContainerRegistry/registries@2023-11-01-preview' = {
  name: acrName
  location: location
  tags: tags
  sku: {
    name: 'Standard'
  }
  properties: {
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'
    anonymousPullEnabled: false
  }
}

// --------------------------------------------------------- web pubsub
// Free tier is sufficient for a single user. AAD-only (local auth disabled) so
// the backend uses managed identity to mint client URLs and send events.
resource webPubSub 'Microsoft.SignalRService/webPubSub@2024-03-01' = {
  name: webPubSubName
  location: location
  tags: tags
  sku: {
    name: 'Free_F1'
    tier: 'Free'
    capacity: 1
  }
  identity: {
    type: 'None'
  }
  properties: {
    publicNetworkAccess: 'Enabled'
    disableLocalAuth: true
    disableAadAuth: false
  }
}

resource webPubSubHub 'Microsoft.SignalRService/webPubSub/hubs@2024-03-01' = {
  parent: webPubSub
  name: 'transcripts'
  properties: {
    anonymousConnectPolicy: 'deny'
    eventHandlers: []
  }
}

// ------------------------------------------------------------- RBAC
// Least-privilege data-plane roles for the shared runtime identity. Pre-created
// so ACR pull / Storage access / Web PubSub are deterministic before apps start.
resource raBlob 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(storage.id, uami.id, roleBlobDataContributor)
  scope: storage
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleBlobDataContributor)
  }
}
resource raQueue 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(storage.id, uami.id, roleQueueDataContributor)
  scope: storage
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleQueueDataContributor)
  }
}
resource raTable 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(storage.id, uami.id, roleTableDataContributor)
  scope: storage
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleTableDataContributor)
  }
}
resource raAcrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(acr.id, uami.id, roleAcrPull)
  scope: acr
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleAcrPull)
  }
}
resource raWebPubSub 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(webPubSub.id, uami.id, roleWebPubSubServiceOwner)
  scope: webPubSub
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleWebPubSubServiceOwner)
  }
}
resource raSpeech 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(speech.id, uami.id, roleCognitiveServicesUser)
  scope: speech
  properties: {
    principalId: uami.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleCognitiveServicesUser)
  }
}

// --------------------------------------------- container apps environment
// Workload-profile environment injected into the VNet, keeping EXTERNAL ingress
// (internal:false) for the authenticated public API while all Storage traffic
// stays private via the linked private DNS zones.
resource managedEnvironment 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: managedEnvironmentName
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logAnalytics.properties.customerId
        sharedKey: listKeys(logAnalytics.id, '2023-09-01').primarySharedKey
      }
    }
    vnetConfiguration: {
      infrastructureSubnetId: infraSubnet.id
      internal: false
    }
    workloadProfiles: [
      {
        name: 'Consumption'
        workloadProfileType: 'Consumption'
      }
    ]
    zoneRedundant: false
  }
}

// ------------------------------------------------------------- outputs
output storageAccountName string = storageAccountName
output blobEndpoint string = storage.properties.primaryEndpoints.blob
output queueEndpoint string = storage.properties.primaryEndpoints.queue
output tableEndpoint string = storage.properties.primaryEndpoints.table
output acrName string = acr.name
output acrLoginServer string = acr.properties.loginServer
output webPubSubName string = webPubSub.name
output webPubSubEndpoint string = 'https://${webPubSub.properties.hostName}'
output speechAccountName string = speech.name
output speechEndpoint string = 'https://${speech.properties.customSubDomainName}.cognitiveservices.azure.com/'
output userAssignedIdentityId string = uami.id
output userAssignedClientId string = uami.properties.clientId
output userAssignedPrincipalId string = uami.properties.principalId
output managedEnvironmentId string = managedEnvironment.id
output managedEnvironmentName string = managedEnvironment.name
output logAnalyticsName string = logAnalytics.name
output foundryEndpointOut string = foundryEndpoint
