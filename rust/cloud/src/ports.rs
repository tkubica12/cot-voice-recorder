use crate::storage::{AzureStorage, StorageError};

pub type BlobData = HashMap<(String, String), Vec<u8>>;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub enum Blobs {
    Azure(Arc<AzureStorage>),
    Memory(Arc<Mutex<BlobData>>),
}
impl Blobs {
    pub fn memory() -> Self {
        Self::Memory(Arc::new(Mutex::new(HashMap::new())))
    }
    pub async fn put(
        &self,
        container: &str,
        name: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<(), StorageError> {
        match self {
            Self::Azure(storage) => storage.put_blob(container, name, data, content_type).await,
            Self::Memory(memory) => {
                memory
                    .lock()
                    .await
                    .insert((container.into(), name.into()), data);
                Ok(())
            }
        }
    }
    pub async fn get(
        &self,
        container: &str,
        name: &str,
        maximum: usize,
    ) -> Result<Vec<u8>, StorageError> {
        match self {
            Self::Azure(storage) => storage.get_blob(container, name, maximum).await,
            Self::Memory(memory) => {
                let memory = memory.lock().await;
                let value = memory
                    .get(&(container.into(), name.into()))
                    .ok_or(StorageError::NotFound)?;
                if value.len() > maximum {
                    return Err(StorageError::Failure(crate::problem::Problem::unavailable(
                        "Blob exceeds supported read bounds.",
                    )));
                }
                Ok(value.clone())
            }
        }
    }
    pub async fn delete(&self, container: &str, name: &str) -> Result<bool, StorageError> {
        match self {
            Self::Azure(storage) => storage.delete_blob(container, name).await,
            Self::Memory(memory) => Ok(memory
                .lock()
                .await
                .remove(&(container.into(), name.into()))
                .is_some()),
        }
    }
}

#[derive(Clone)]
pub enum Queue {
    Azure {
        storage: Arc<AzureStorage>,
        name: String,
    },
    Memory(Arc<MemoryQueue>),
}
pub struct MemoryQueue {
    pub messages: Mutex<VecDeque<(serde_json::Value, u32)>>,
    pub failed: std::sync::atomic::AtomicBool,
}
impl Queue {
    pub fn memory() -> Self {
        Self::Memory(Arc::new(MemoryQueue {
            messages: Mutex::new(VecDeque::new()),
            failed: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub async fn send(&self, message: serde_json::Value, delay: u32) -> Result<(), StorageError> {
        match self {
            Self::Azure { storage, name } => storage.send_queue(name, &message, delay).await,
            Self::Memory(memory) => {
                if memory.failed.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(StorageError::Failure(crate::problem::Problem::unavailable(
                        "Synthetic queue outage.",
                    )));
                }
                memory.messages.lock().await.push_back((message, delay));
                Ok(())
            }
        }
    }
}
