use crate::ai::AzureAi;
use crate::api::ApiState;
use crate::auth::GoogleVerifier;
use crate::config::Config;
use crate::gateway::WebPubSub;
use crate::identity::ManagedIdentity;
use crate::pipeline::Pipeline;
use crate::ports::{Blobs, Queue};
use crate::problem::Problem;
use crate::repository::Repository;
use crate::services::Services;
use crate::storage::AzureStorage;
use crate::streaming::MaiStreamingProvider;
use crate::worker::QueueWorker;
use std::sync::Arc;

pub struct Application {
    pub config: Arc<Config>,
    pub storage: Arc<AzureStorage>,
    pub services: Arc<Services>,
    pub api: ApiState,
    pub worker: QueueWorker,
}
impl Application {
    pub fn from_environment() -> Result<Self, Problem> {
        let config = Arc::new(Config::from_environment()?);
        if config.use_fakes {
            return Err(Problem::validation(
                "CLI fake adapters are not enabled; synthetic fixtures inject explicit adapters directly.",
            ));
        }
        let identity = Arc::new(ManagedIdentity::from_environment()?);
        let storage = Arc::new(AzureStorage::new(&config, identity.clone())?);
        let services = Arc::new(Services::new(
            config.clone(),
            Repository::Azure(storage.clone()),
            Blobs::Azure(storage.clone()),
            Queue::Azure {
                storage: storage.clone(),
                name: config.queue.clone(),
            },
        )?);
        let ai = Arc::new(AzureAi::new(config.clone(), identity.clone())?);
        let realtime = Arc::new(WebPubSub::new(config.clone(), identity.clone())?);
        let mut api = ApiState::new(
            services.clone(),
            Arc::new(GoogleVerifier::new(config.clone())?),
        );
        api.ai = Some(ai.clone());
        api.realtime = Some(realtime.clone());
        if config.dictation_stream_enabled {
            api.streaming = Some(Arc::new(MaiStreamingProvider::new(
                &config,
                identity.clone(),
                &config.dictation_stream,
            )?));
        }
        let worker = QueueWorker {
            pipeline: Arc::new(Pipeline {
                services: services.clone(),
                ai,
                realtime,
            }),
            streaming: Arc::new(MaiStreamingProvider::new(
                &config,
                identity,
                &config.recording_stream,
            )?),
        };
        Ok(Self {
            config,
            storage,
            services,
            api,
            worker,
        })
    }
}
