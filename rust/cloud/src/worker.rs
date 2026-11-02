use crate::domain::{ChunkState, RecordingState};
use crate::pipeline::Pipeline;
use crate::ports::QueueReceipt;
use crate::storage::{QueueMessage, StorageError};
use crate::stream_worker::StreamWorkError;
use crate::streaming::MaiStreamingProvider;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

pub struct QueueWorker {
    pub pipeline: Arc<Pipeline>,
    pub streaming: Arc<MaiStreamingProvider>,
}
pub fn backoff(count: u32, base: u32, cap: u32) -> u32 {
    base.saturating_mul(
        1u32.checked_shl(count.saturating_sub(1))
            .unwrap_or(u32::MAX),
    )
    .min(cap)
}
impl QueueWorker {
    pub async fn run(&self, mut stop: watch::Receiver<bool>) -> Result<(), StorageError> {
        let mut receive_failures: u32 = 0;
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                return Ok(());
            }
            let wait = match self.run_once(stop.clone()).await {
                Ok(true) => {
                    receive_failures = 0;
                    continue;
                }
                Ok(false) => {
                    receive_failures = 0;
                    Duration::from_secs(1)
                }
                Err(error) => {
                    receive_failures = receive_failures.saturating_add(1);
                    eprintln!("Queue receive failed: {error}");
                    let config = &self.pipeline.services.config;
                    Duration::from_secs_f64(
                        (config.queue_receive_error_backoff_seconds
                            * 2f64.powi(receive_failures.saturating_sub(1).min(63) as i32))
                        .min(config.queue_receive_error_max_backoff_seconds),
                    )
                }
            };
            tokio::select! {_ = tokio::time::sleep(wait)=>{},_ = stop.changed()=>return Ok(())}
        }
    }
    pub async fn run_once(&self, stop: watch::Receiver<bool>) -> Result<bool, StorageError> {
        if *stop.borrow() || stop.has_changed().is_err() {
            return Ok(false);
        }
        let services = &self.pipeline.services;
        let mut messages = services
            .queue
            .receive(services.config.queue_visibility_seconds)
            .await?;
        let Some(message) = messages.pop() else {
            return Ok(false);
        };
        let receipt = Arc::new(QueueReceipt::new(services.queue.clone(), message.clone()));
        let kind = message.content["type"].as_str().unwrap_or("");
        let id = message.content["recording_id"].as_str().unwrap_or("");
        let streaming = if matches!(kind, "stream" | "transcribe") {
            match services.recording(id).await {
                Ok(record) => record.transcription_mode == "streaming",
                Err(error) if error.status == 404 => kind == "stream",
                Err(error) => {
                    eprintln!("Queue recording lookup failed: {error}");
                    self.settle_retry(&receipt, message.dequeue_count).await;
                    return Ok(true);
                }
            }
        } else {
            false
        };
        let result = if id.is_empty() {
            Err(StreamWorkError::Terminal(
                "Queue message is missing recording identity.",
            ))
        } else if kind == "stream" || kind == "transcribe" && streaming {
            crate::continuous::transcribe_recording(
                services.clone(),
                self.streaming.clone(),
                id.into(),
                receipt.clone(),
                stop.clone(),
            )
            .await
        } else if kind == "transcribe" {
            match message.content["index"]
                .as_u64()
                .and_then(|index| u32::try_from(index).ok())
            {
                Some(index) => {
                    self.pipeline
                        .transcribe_chunk(id, index, &receipt, stop.clone())
                        .await
                }
                None => Err(StreamWorkError::Terminal(
                    "Queue message has an invalid chunk index.",
                )),
            }
        } else if kind == "finalize" {
            self.pipeline.finalize(id, &receipt, stop.clone()).await
        } else {
            Err(StreamWorkError::Terminal("Unsupported queue message type."))
        };
        match result {
            Ok(()) => {
                if let Err(error) = receipt.delete().await {
                    eprintln!("Queue delete failed; settled work can be redelivered: {error}");
                }
            }
            Err(StreamWorkError::Pending(reason)) => {
                eprintln!("Queue work remains pending: {reason}");
                if let Err(error) = receipt.renew(30).await {
                    eprintln!("Pending queue renewal failed: {error}");
                }
            }
            Err(error) => {
                eprintln!("Queue processing failed: {error}");
                let terminal = matches!(&error, StreamWorkError::Terminal(_))
                    || matches!(&error,StreamWorkError::Failure(problem) if matches!(problem.status,400|401|403|404|413|415|422|502));
                let attempts = if kind == "transcribe" {
                    services.config.max_transcribe_attempts
                } else {
                    services.config.max_refine_attempts
                };
                let pending_delivery = if kind == "finalize" {
                    match services.recording(id).await {
                        Ok(record) => {
                            record.state == RecordingState::Completed
                                && !record.stream_delivery_completed
                        }
                        Err(error) if error.status == 404 => false,
                        Err(error) => {
                            eprintln!("Completion delivery state lookup failed: {error}");
                            self.settle_retry(&receipt, message.dequeue_count.min(8))
                                .await;
                            return Ok(true);
                        }
                    }
                } else {
                    false
                };
                if pending_delivery {
                    self.settle_retry(&receipt, message.dequeue_count.min(8))
                        .await;
                    return Ok(true);
                }
                if terminal || !streaming && message.dequeue_count >= attempts {
                    match self.fail(&message).await {
                        Ok(()) => {
                            if let Err(error) = receipt.delete().await {
                                eprintln!("Failed-work queue delete could not settle: {error}");
                            }
                        }
                        Err(error) => {
                            eprintln!("Failed-work state could not be persisted: {error}");
                            self.settle_retry(&receipt, message.dequeue_count.min(8))
                                .await;
                        }
                    }
                } else {
                    self.settle_retry(
                        &receipt,
                        if streaming {
                            message.dequeue_count.min(8)
                        } else {
                            message.dequeue_count
                        },
                    )
                    .await;
                }
            }
        }
        Ok(true)
    }
    async fn settle_retry(&self, receipt: &QueueReceipt, count: u32) {
        let delay = backoff(
            count,
            self.pipeline.services.config.queue_retry_base_seconds,
            600,
        );
        if let Err(error) = receipt.renew(delay).await {
            eprintln!("Queue retry renewal failed: {error}");
        }
    }
    async fn fail(&self, message: &QueueMessage) -> Result<(), StreamWorkError> {
        let kind = message.content["type"].as_str().unwrap_or("");
        let id = message.content["recording_id"].as_str().unwrap_or("");
        if id.is_empty() {
            return Ok(());
        }
        if matches!(kind, "transcribe" | "stream") {
            if kind == "transcribe" {
                if let Some(index) = message.content["index"]
                    .as_u64()
                    .and_then(|index| u32::try_from(index).ok())
                {
                    self.pipeline.fail_chunk(id, index).await?;
                }
            }
            self.pipeline
                .fail_recording(id, "transcription_failed")
                .await
        } else if kind == "finalize" {
            let record = match self.pipeline.services.recording(id).await {
                Ok(record) => record,
                Err(error) if error.status == 404 => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if record.state.terminal() {
                return Ok(());
            }
            let chunks = self.pipeline.services.chunks(id).await?;
            let complete = record.expected_chunk_count.is_some_and(|count| {
                chunks.len() >= count as usize
                    && chunks
                        .iter()
                        .take(count as usize)
                        .enumerate()
                        .all(|(index, chunk)| {
                            chunk.index as usize == index
                                && (record.transcription_mode == "streaming"
                                    && record.stream_asr_ready
                                    || chunk.state == ChunkState::Transcribed)
                        })
            });
            self.pipeline
                .fail_recording(
                    id,
                    if record.state == RecordingState::Refining || complete {
                        "refinement_failed"
                    } else {
                        "missing_chunks"
                    },
                )
                .await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
