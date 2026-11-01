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
    pub messages: Mutex<VecDeque<MemoryMessage>>,
    pub failed: std::sync::atomic::AtomicBool,
}
pub struct MemoryMessage {
    pub message: crate::storage::QueueMessage,
    pub visible_at: tokio::time::Instant,
    pub delay: u32,
}
fn queue_outage() -> StorageError {
    StorageError::Failure(crate::problem::Problem::unavailable(
        "Synthetic queue outage.",
    ))
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
                    return Err(queue_outage());
                }
                memory.messages.lock().await.push_back(MemoryMessage {
                    message: crate::storage::QueueMessage {
                        id: crate::services::uuid().map_err(StorageError::Failure)?,
                        pop_receipt: String::new(),
                        dequeue_count: 0,
                        content: message,
                    },
                    visible_at: tokio::time::Instant::now()
                        + std::time::Duration::from_secs(u64::from(delay)),
                    delay,
                });
                Ok(())
            }
        }
    }
    pub async fn receive(
        &self,
        visibility: u32,
    ) -> Result<Vec<crate::storage::QueueMessage>, StorageError> {
        match self {
            Self::Azure { storage, name } => storage.receive_queue(name, visibility).await,
            Self::Memory(memory) => {
                if memory.failed.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(queue_outage());
                }
                let mut messages = memory.messages.lock().await;
                let current = tokio::time::Instant::now();
                let Some(entry) = messages
                    .iter_mut()
                    .find(|entry| entry.visible_at <= current)
                else {
                    return Ok(Vec::new());
                };
                entry.message.dequeue_count =
                    entry.message.dequeue_count.checked_add(1).ok_or_else(|| {
                        StorageError::Failure(crate::problem::Problem::internal())
                    })?;
                entry.message.pop_receipt =
                    crate::services::uuid().map_err(StorageError::Failure)?;
                entry.visible_at = current + std::time::Duration::from_secs(u64::from(visibility));
                Ok(vec![entry.message.clone()])
            }
        }
    }
    pub async fn renew(
        &self,
        message: &crate::storage::QueueMessage,
        visibility: u32,
    ) -> Result<crate::storage::QueueMessage, StorageError> {
        match self {
            Self::Azure { storage, name } => storage.renew_queue(name, message, visibility).await,
            Self::Memory(memory) => {
                if memory.failed.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(queue_outage());
                }
                let mut messages = memory.messages.lock().await;
                let entry = messages
                    .iter_mut()
                    .find(|entry| {
                        entry.message.id == message.id
                            && entry.message.pop_receipt == message.pop_receipt
                    })
                    .ok_or(StorageError::MessageGone)?;
                entry.message.pop_receipt =
                    crate::services::uuid().map_err(StorageError::Failure)?;
                entry.visible_at = tokio::time::Instant::now()
                    + std::time::Duration::from_secs(u64::from(visibility));
                Ok(entry.message.clone())
            }
        }
    }
    pub async fn delete(&self, message: &crate::storage::QueueMessage) -> Result<(), StorageError> {
        match self {
            Self::Azure { storage, name } => storage.delete_queue(name, message).await,
            Self::Memory(memory) => {
                if memory.failed.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(queue_outage());
                }
                let mut messages = memory.messages.lock().await;
                if let Some(index) = messages.iter().position(|entry| {
                    entry.message.id == message.id
                        && entry.message.pop_receipt == message.pop_receipt
                }) {
                    messages.remove(index);
                } else {
                    eprintln!(
                        "Queue delete receipt was already gone; original message was not removed."
                    );
                }
                Ok(())
            }
        }
    }
}

pub struct QueueReceipt {
    queue: Queue,
    message: Mutex<crate::storage::QueueMessage>,
}
impl QueueReceipt {
    pub fn new(queue: Queue, message: crate::storage::QueueMessage) -> Self {
        Self {
            queue,
            message: Mutex::new(message),
        }
    }
    pub async fn renew(&self, visibility: u32) -> Result<(), StorageError> {
        let mut current = self.message.lock().await;
        let renewed = self.queue.renew(&current, visibility).await?;
        *current = renewed;
        Ok(())
    }
    pub async fn delete(&self) -> Result<(), StorageError> {
        let current = self.message.lock().await;
        self.queue.delete(&current).await
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    #[tokio::test]
    async fn renewal_updates_the_held_receipt_and_stale_delete_cannot_remove_the_successor() {
        let queue = Queue::memory();
        queue
            .send(
                serde_json::json!({"type":"stream","recording_id":"fixture"}),
                0,
            )
            .await
            .unwrap();
        let original = queue.receive(300).await.unwrap().remove(0);
        assert_eq!(original.dequeue_count, 1);
        let receipt = QueueReceipt::new(queue.clone(), original.clone());
        receipt.renew(300).await.unwrap();
        assert!(matches!(
            queue.renew(&original, 300).await,
            Err(StorageError::MessageGone)
        ));
        queue.delete(&original).await.unwrap();
        let Queue::Memory(memory) = &queue else {
            panic!("memory fixture");
        };
        assert_eq!(memory.messages.lock().await.len(), 1);
        receipt.delete().await.unwrap();
        assert!(memory.messages.lock().await.is_empty());
        receipt.delete().await.unwrap();
    }
    #[tokio::test]
    async fn delayed_messages_and_visible_redelivery_keep_one_message_per_receive() {
        let queue = Queue::memory();
        queue
            .send(serde_json::json!({"type":"finalize"}), 900)
            .await
            .unwrap();
        assert!(queue.receive(300).await.unwrap().is_empty());
        queue
            .send(serde_json::json!({"type":"stream"}), 0)
            .await
            .unwrap();
        let first = queue.receive(300).await.unwrap().remove(0);
        assert!(queue.receive(300).await.unwrap().is_empty());
        let Queue::Memory(memory) = &queue else {
            panic!("memory fixture");
        };
        memory
            .messages
            .lock()
            .await
            .iter_mut()
            .find(|entry| entry.message.id == first.id)
            .unwrap()
            .visible_at = tokio::time::Instant::now();
        let second = queue.receive(300).await.unwrap().remove(0);
        assert_eq!(first.id, second.id);
        assert_eq!(second.dequeue_count, 2);
        assert_ne!(first.pop_receipt, second.pop_receipt);
        assert!(matches!(
            queue.renew(&first, 300).await,
            Err(StorageError::MessageGone)
        ));
    }
    #[tokio::test]
    async fn concurrent_receivers_do_not_dequeue_two_model_calls_under_one_visibility_lease() {
        let queue = Queue::memory();
        queue
            .send(serde_json::json!({"type":"stream"}), 0)
            .await
            .unwrap();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let queue = queue.clone();
            workers.push(tokio::spawn(async move {
                queue.receive(300).await.unwrap().len()
            }));
        }
        let mut received = 0;
        for worker in workers {
            received += worker.await.unwrap();
        }
        assert_eq!(received, 1);
    }
    #[tokio::test]
    async fn queue_outage_is_explicit_for_receive_renew_and_delete_without_losing_current_receipt()
    {
        let queue = Queue::memory();
        queue
            .send(serde_json::json!({"type":"stream"}), 0)
            .await
            .unwrap();
        let original = queue.receive(300).await.unwrap().remove(0);
        let receipt = QueueReceipt::new(queue.clone(), original);
        let Queue::Memory(memory) = &queue else {
            panic!("memory fixture");
        };
        memory
            .failed
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(queue.receive(300).await.is_err());
        assert!(receipt.renew(300).await.is_err());
        assert!(receipt.delete().await.is_err());
        memory
            .failed
            .store(false, std::sync::atomic::Ordering::Release);
        receipt.renew(300).await.unwrap();
        receipt.delete().await.unwrap();
        assert!(memory.messages.lock().await.is_empty());
    }
}
