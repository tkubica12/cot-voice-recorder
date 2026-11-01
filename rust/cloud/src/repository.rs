use crate::problem::Problem;
use crate::storage::{AzureStorage, Entity, StorageError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

type Key = (String, String, String);

#[derive(Clone)]
pub enum Repository {
    Azure(Arc<AzureStorage>),
    Memory(Arc<MemoryTables>),
}
pub struct MemoryTables {
    rows: Mutex<HashMap<Key, Entity>>,
    version: std::sync::atomic::AtomicU64,
}

impl Repository {
    pub fn memory() -> Self {
        Self::Memory(Arc::new(MemoryTables {
            rows: Mutex::new(HashMap::new()),
            version: std::sync::atomic::AtomicU64::new(1),
        }))
    }
    pub async fn get(
        &self,
        table: &str,
        pk: &str,
        rk: &str,
    ) -> Result<Option<Entity>, StorageError> {
        match self {
            Self::Azure(storage) => storage.entity(table, pk, rk).await,
            Self::Memory(memory) => Ok(memory
                .rows
                .lock()
                .await
                .get(&(table.into(), pk.into(), rk.into()))
                .cloned()),
        }
    }
    pub async fn insert(&self, table: &str, values: Entity) -> Result<Entity, StorageError> {
        let pk = values
            .values
            .get("PartitionKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StorageError::Failure(Problem::internal()))?;
        let rk = values
            .values
            .get("RowKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StorageError::Failure(Problem::internal()))?;
        match self {
            Self::Azure(storage) => {
                let mut values = values;
                values.etag = storage.insert(table, &values.values).await?;
                Ok(values)
            }
            Self::Memory(memory) => {
                let key = (table.into(), pk.into(), rk.into());
                let mut rows = memory.rows.lock().await;
                if rows.contains_key(&key) {
                    return Err(StorageError::Conflict);
                }
                let mut values = values;
                values.etag = format!(
                    "opaque-{}",
                    memory
                        .version
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                );
                rows.insert(key, values.clone());
                Ok(values)
            }
        }
    }
    pub async fn replace(&self, table: &str, values: Entity) -> Result<Entity, StorageError> {
        let pk = values
            .values
            .get("PartitionKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StorageError::Failure(Problem::internal()))?;
        let rk = values
            .values
            .get("RowKey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StorageError::Failure(Problem::internal()))?;
        match self {
            Self::Azure(storage) => {
                let mut values = values;
                values.etag = storage.replace(table, &values).await?;
                Ok(values)
            }
            Self::Memory(memory) => {
                let key = (table.into(), pk.into(), rk.into());
                let mut rows = memory.rows.lock().await;
                let old = rows.get(&key).ok_or(StorageError::NotFound)?;
                if values.etag.is_empty() || values.etag == "*" || old.etag != values.etag {
                    return Err(StorageError::Conflict);
                }
                let mut values = values;
                values.etag = format!(
                    "opaque-{}",
                    memory
                        .version
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                );
                rows.insert(key, values.clone());
                Ok(values)
            }
        }
    }
    pub async fn partition(&self, table: &str, pk: &str) -> Result<Vec<Entity>, StorageError> {
        match self {
            Self::Azure(storage) => storage.query_partition(table, pk).await,
            Self::Memory(memory) => Ok(memory
                .rows
                .lock()
                .await
                .iter()
                .filter(|((stored_table, partition, _), _)| {
                    stored_table == table && partition == pk
                })
                .map(|(_, values)| values.clone())
                .collect()),
        }
    }
    pub async fn delete(
        &self,
        table: &str,
        pk: &str,
        rk: &str,
        etag: &str,
    ) -> Result<(), StorageError> {
        match self {
            Self::Azure(storage) => storage.delete_entity(table, pk, rk, etag).await,
            Self::Memory(memory) => {
                let key = (table.into(), pk.into(), rk.into());
                let mut rows = memory.rows.lock().await;
                let old = rows.get(&key).ok_or(StorageError::NotFound)?;
                if etag.is_empty() || etag == "*" || old.etag != etag {
                    return Err(StorageError::Conflict);
                }
                rows.remove(&key);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn memory_repository_has_the_same_insert_and_compare_replace_delete_conflicts_as_azure() {
        let repository = Repository::memory();
        let values = Entity { values: serde_json::json!({"PartitionKey":"recording","RowKey":"fixture","state":"recording"}).as_object().unwrap().clone(), etag: String::new() };
        let first = repository
            .insert("recordings", values.clone())
            .await
            .unwrap();
        assert!(matches!(
            repository.insert("recordings", values).await,
            Err(StorageError::Conflict)
        ));
        let second = repository
            .replace("recordings", first.clone())
            .await
            .unwrap();
        assert_ne!(first.etag, second.etag);
        assert!(matches!(
            repository.replace("recordings", first.clone()).await,
            Err(StorageError::Conflict)
        ));
        assert!(matches!(
            repository
                .delete("recordings", "recording", "fixture", &first.etag)
                .await,
            Err(StorageError::Conflict)
        ));
        repository
            .delete("recordings", "recording", "fixture", &second.etag)
            .await
            .unwrap();
        assert!(
            repository
                .get("recordings", "recording", "fixture")
                .await
                .unwrap()
                .is_none()
        );
    }
}
