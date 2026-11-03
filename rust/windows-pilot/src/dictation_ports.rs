use std::future::Future;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use voiceprompt_client_protocol::api::{ApiClient, ApiError, DictationRefinement};
use voiceprompt_client_protocol::polishing::RefinementService;
use voiceprompt_client_protocol::state::AuthState;

use crate::microphone::{CaptureBuffer, CaptureError, Microphone};
use crate::{BoundBatchBackend, EncryptedTokenStore, NativeError, StreamingBackend};

pub(crate) trait DictationPorts: Clone + Send + Sync + 'static {
    type Backend: StreamingBackend;
    type Refiner: RefinementService;
    type Capture: Send + 'static;

    fn auth_state(&self) -> watch::Receiver<AuthState>;
    fn recovery_context(&self, backend: &str) -> impl Future<Output = Option<String>> + Send;
    fn backend(&self, label: String) -> Result<Arc<Self::Backend>, NativeError>;
    fn refiner(&self, email: String) -> Arc<Self::Refiner>;
    fn start_capture(&self)
    -> Result<(Self::Capture, mpsc::Receiver<CaptureBuffer>), CaptureError>;
    fn stop_capture(capture: Self::Capture) -> Result<(), CaptureError>;
}

#[derive(Clone)]
pub(crate) struct NativeDictationPorts {
    api: Arc<ApiClient<EncryptedTokenStore>>,
}

impl NativeDictationPorts {
    pub fn new(api: Arc<ApiClient<EncryptedTokenStore>>) -> Self {
        Self { api }
    }
}

pub(crate) struct NativeRefiner {
    api: Arc<ApiClient<EncryptedTokenStore>>,
    email: String,
}

impl RefinementService for NativeRefiner {
    async fn refine(&self, text: &str, context: &str) -> Result<DictationRefinement, ApiError> {
        self.api
            .refine_bound_dictation(text, context, &self.email)
            .await
    }
}

impl DictationPorts for NativeDictationPorts {
    type Backend = BoundBatchBackend<EncryptedTokenStore>;
    type Refiner = NativeRefiner;
    type Capture = Microphone;

    fn auth_state(&self) -> watch::Receiver<AuthState> {
        self.api.auth_state()
    }

    async fn recovery_context(&self, backend: &str) -> Option<String> {
        self.api.recovery_context(backend).await
    }

    fn backend(&self, label: String) -> Result<Arc<Self::Backend>, NativeError> {
        BoundBatchBackend::new(self.api.clone(), label).map(Arc::new)
    }

    fn refiner(&self, email: String) -> Arc<Self::Refiner> {
        Arc::new(NativeRefiner {
            api: self.api.clone(),
            email,
        })
    }

    fn start_capture(&self) -> Result<(Microphone, mpsc::Receiver<CaptureBuffer>), CaptureError> {
        Microphone::start()
    }

    fn stop_capture(capture: Microphone) -> Result<(), CaptureError> {
        capture.finish()
    }
}
