// =============================================================================
// Application layer (resource-group scoped): API + worker Container Apps and the
// scheduled cleanup Job. All three share one user-assigned managed identity and
// the same image; configuration is delivered purely through env vars.
// =============================================================================

@description('Azure region.')
param location string

@description('Resource tags.')
param tags object

@description('Backend container image (loginServer/repo:tag).')
param containerImage string

@description('Container Apps managed environment resource id.')
param managedEnvironmentId string

@description('ACR login server (e.g. crxxx.azurecr.io).')
param acrLoginServer string

@description('User-assigned managed identity resource id (shared).')
param userAssignedIdentityId string

@description('User-assigned managed identity client id (for DefaultAzureCredential).')
param userAssignedClientId string

@description('Storage account name.')
param storageAccountName string

@description('Web PubSub endpoint (https://...).')
param webPubSubEndpoint string

@description('Foundry endpoint.')
param foundryEndpoint string

@description('Comma-separated Google OIDC audiences (deny-all placeholder).')
param googleAllowedAudiences string

@description('Allowlisted owner email (deny-all placeholder).')
param allowlistedEmail string

// Configuration is delivered entirely through environment variables. No secrets:
// Storage / Foundry / Web PubSub / ACR all authenticate with the managed identity.
var commonEnv = [
  {
    name: 'AZURE_CLIENT_ID'
    value: userAssignedClientId
  }
  {
    name: 'VR_ENVIRONMENT'
    value: 'production'
  }
  {
    name: 'VR_LOG_LEVEL'
    value: 'INFO'
  }
  {
    name: 'VR_STORAGE_ACCOUNT_NAME'
    value: storageAccountName
  }
  {
    name: 'VR_STORAGE_USE_AZURITE'
    value: 'false'
  }
  {
    name: 'VR_USE_FAKE_STORAGE'
    value: 'false'
  }
  {
    name: 'VR_USE_FAKE_AI'
    value: 'false'
  }
  {
    name: 'VR_USE_FAKE_REALTIME'
    value: 'false'
  }
  {
    name: 'VR_AUTH_DISABLED'
    value: 'false'
  }
  {
    name: 'VR_AUDIO_CONTAINER'
    value: 'audio'
  }
  {
    name: 'VR_TRANSCRIPT_CONTAINER'
    value: 'transcripts'
  }
  {
    name: 'VR_RAW_CONTAINER'
    value: 'raw'
  }
  {
    name: 'VR_WORK_QUEUE'
    value: 'work'
  }
  {
    name: 'VR_RECORDINGS_TABLE'
    value: 'recordings'
  }
  {
    name: 'VR_CHUNKS_TABLE'
    value: 'chunks'
  }
  {
    name: 'VR_TRANSCRIPTS_TABLE'
    value: 'transcripts'
  }
  {
    name: 'VR_FOUNDRY_ENDPOINT'
    value: foundryEndpoint
  }
  {
    name: 'VR_FOUNDRY_API_VERSION'
    value: '2024-10-21'
  }
  {
    name: 'VR_TRANSCRIBE_DEPLOYMENT'
    value: 'gpt-4o-transcribe'
  }
  {
    name: 'VR_REFINE_DEPLOYMENT_DEFAULT'
    value: 'gpt-5.6-luna'
  }
  {
    name: 'VR_REFINE_DEPLOYMENT_ALTERNATIVE'
    value: 'gpt-5.6-terra'
  }
  {
    name: 'VR_WEBPUBSUB_ENDPOINT'
    value: webPubSubEndpoint
  }
  {
    name: 'VR_WEBPUBSUB_HUB'
    value: 'transcripts'
  }
  {
    name: 'VR_WEBPUBSUB_GROUP'
    value: 'user'
  }
  {
    name: 'VR_TRANSCRIBE_LANGUAGE'
    value: 'cs'
  }
  // Queue tuning. One message per receive because the worker processes messages
  // sequentially: a larger batch would let the tail of the batch lose its visibility
  // lease while the head is still calling Foundry, causing duplicate model calls.
  {
    name: 'VR_QUEUE_BATCH_SIZE'
    value: '1'
  }
  // Processing lease per message; must exceed the slowest Foundry transcription.
  {
    name: 'VR_QUEUE_VISIBILITY_SECONDS'
    value: '300'
  }
  // Base of the exponential wait between retry attempts (separate from the lease).
  {
    name: 'VR_QUEUE_RETRY_BASE_SECONDS'
    value: '30'
  }
  {
    name: 'VR_GOOGLE_ALLOWED_AUDIENCES'
    value: googleAllowedAudiences
  }
  {
    name: 'VR_ALLOWLISTED_EMAIL'
    value: allowlistedEmail
  }
]

var registries = [
  {
    server: acrLoginServer
    identity: userAssignedIdentityId
  }
]

// --------------------------------------------------------------- API app
// Public external HTTPS ingress; HTTP scale-to-zero (KEDA http rule).
resource apiApp 'Microsoft.App/containerApps@2025-01-01' = {
  name: 'ca-api'
  location: location
  tags: union(tags, { 'azd-service-name': 'api' })
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${userAssignedIdentityId}': {}
    }
  }
  properties: {
    managedEnvironmentId: managedEnvironmentId
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      ingress: {
        external: true
        targetPort: 8000
        transport: 'auto'
        allowInsecure: false
        traffic: [
          {
            latestRevision: true
            weight: 100
          }
        ]
      }
      registries: registries
    }
    template: {
      containers: [
        {
          name: 'api'
          image: containerImage
          resources: {
            cpu: json('0.5')
            memory: '1.0Gi'
          }
          env: commonEnv
          probes: [
            {
              type: 'Liveness'
              httpGet: {
                path: '/health/live'
                port: 8000
              }
              initialDelaySeconds: 10
              periodSeconds: 30
              failureThreshold: 3
            }
            {
              type: 'Readiness'
              httpGet: {
                path: '/health/ready'
                port: 8000
              }
              initialDelaySeconds: 5
              periodSeconds: 10
              failureThreshold: 6
            }
          ]
        }
      ]
      scale: {
        minReplicas: 0
        maxReplicas: 2
        rules: [
          {
            name: 'http-scale'
            http: {
              metadata: {
                concurrentRequests: '20'
              }
            }
          }
        ]
      }
    }
  }
}

// ------------------------------------------------------------- Worker app
// No ingress. Scales from zero on the private Storage queue via managed identity
// (KEDA azure-queue with workload identity — no connection string).
resource workerApp 'Microsoft.App/containerApps@2025-01-01' = {
  name: 'ca-worker'
  location: location
  tags: union(tags, { 'azd-service-name': 'worker' })
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${userAssignedIdentityId}': {}
    }
  }
  properties: {
    managedEnvironmentId: managedEnvironmentId
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      registries: registries
    }
    template: {
      containers: [
        {
          name: 'worker'
          image: containerImage
          command: []
          args: [
            'worker'
          ]
          resources: {
            cpu: json('0.25')
            memory: '0.5Gi'
          }
          env: commonEnv
        }
      ]
      scale: {
        minReplicas: 0
        maxReplicas: 2
        rules: [
          {
            name: 'queue-scale'
            custom: {
              type: 'azure-queue'
              identity: userAssignedIdentityId
              metadata: {
                accountName: storageAccountName
                queueName: 'work'
                queueLength: '1'
              }
            }
          }
        ]
      }
    }
  }
}

// -------------------------------------------------------------- Cleanup job
// Hourly retention safety-net; runs the same image with the `cleanup` command.
resource cleanupJob 'Microsoft.App/jobs@2025-01-01' = {
  name: 'caj-cleanup'
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${userAssignedIdentityId}': {}
    }
  }
  properties: {
    environmentId: managedEnvironmentId
    workloadProfileName: 'Consumption'
    configuration: {
      triggerType: 'Schedule'
      replicaTimeout: 1800
      replicaRetryLimit: 1
      scheduleTriggerConfig: {
        cronExpression: '0 * * * *'
        parallelism: 1
        replicaCompletionCount: 1
      }
      registries: registries
    }
    template: {
      containers: [
        {
          name: 'cleanup'
          image: containerImage
          command: []
          args: [
            'cleanup'
          ]
          resources: {
            cpu: json('0.25')
            memory: '0.5Gi'
          }
          env: commonEnv
        }
      ]
    }
  }
}

output apiFqdn string = apiApp.properties.configuration.ingress.fqdn
output apiName string = apiApp.name
output workerName string = workerApp.name
output cleanupJobName string = cleanupJob.name
