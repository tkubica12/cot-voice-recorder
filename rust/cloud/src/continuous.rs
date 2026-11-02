use crate::ports::QueueReceipt;
use crate::services::{Services, now, preview, uuid};
use crate::stream_worker::{StreamLeases, StreamWorkError, pcm_for_chunk};
use crate::streaming::{MaiStreamingProvider, Upstream, UpstreamSender};
use base64::Engine;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;

const PREFIX: &str = "conversation.item.input_audio_transcription.";
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

struct Progress {
    cursor: u64,
    delta: String,
    intermediate: String,
    published: Option<tokio::time::Instant>,
}
struct Session {
    services: Arc<Services>,
    leases: Arc<StreamLeases>,
    receipt: Arc<QueueReceipt>,
    id: String,
    owner: String,
    stop: watch::Receiver<bool>,
    cancel: watch::Receiver<bool>,
    progress: Mutex<Progress>,
    eof: AtomicBool,
}
enum Completed {
    Sent,
    Received(String),
}
fn stopping() -> StreamWorkError {
    StreamWorkError::Pending("Worker shutdown.")
}
fn provider_failed() -> StreamWorkError {
    StreamWorkError::Failure(crate::problem::Problem::unavailable(
        "Continuous MAI stream failed.",
    ))
}
impl Session {
    fn check_stop(&self) -> Result<(), StreamWorkError> {
        if *self.stop.borrow()
            || *self.cancel.borrow()
            || self.stop.has_changed().is_err()
            || self.cancel.has_changed().is_err()
        {
            Err(stopping())
        } else {
            Ok(())
        }
    }
    async fn pause(&self, duration: Duration) -> Result<(), StreamWorkError> {
        self.check_stop()?;
        let mut stop = self.stop.clone();
        let mut cancel = self.cancel.clone();
        tokio::select! {
            _ = tokio::time::sleep(duration) => self.check_stop(),
            _ = stop.changed() => Err(stopping()),
            _ = cancel.changed() => Err(stopping()),
        }
    }
    async fn publish_progress(&self) -> Result<(), StreamWorkError> {
        let (cursor, text) = {
            let mut progress = self.progress.lock().await;
            let current = tokio::time::Instant::now();
            if progress
                .published
                .is_some_and(|previous| current.duration_since(previous) < Duration::from_secs(5))
            {
                return Ok(());
            }
            progress.published = Some(current);
            let mut tail: Vec<_> = progress
                .delta
                .chars()
                .chain(progress.intermediate.chars())
                .rev()
                .take(140)
                .collect();
            tail.reverse();
            (
                progress.cursor,
                preview(
                    &tail.into_iter().collect::<String>(),
                    self.services.config.preview_max_chars,
                ),
            )
        };
        self.leases
            .update_owned(&self.id, &self.owner, |record| {
                record.streamed_samples = cursor;
                record.stream_preview = text.clone();
            })
            .await?;
        Ok(())
    }
    async fn send_audio(
        self: Arc<Self>,
        sender: UpstreamSender,
    ) -> Result<Completed, StreamWorkError> {
        let mut index = 0;
        let mut waiting = tokio::time::Instant::now();
        loop {
            self.check_stop()?;
            let record = self.services.recording(&self.id).await?;
            if record.stream_lease_id.as_deref() != Some(self.owner.as_str())
                || record.state.terminal()
            {
                return Err(StreamWorkError::Pending("Recording ownership changed."));
            }
            if now() - record.created_at >= time::Duration::hours(48) {
                return Err(StreamWorkError::Terminal(
                    "Streaming recording exceeded retention.",
                ));
            }
            let cursor = self.progress.lock().await.cursor;
            if record.expected_chunk_count == Some(index) {
                if cursor == 0 {
                    return Err(StreamWorkError::Terminal("Empty streaming recording."));
                }
                self.eof.store(true, Ordering::Release);
                sender
                    .send(serde_json::json!({"type":"input_audio_buffer.commit"}))
                    .await?;
                return Ok(Completed::Sent);
            }
            let Some(pcm) = pcm_for_chunk(&self.services, &self.id, index, cursor).await? else {
                self.publish_progress().await?;
                if waiting.elapsed().as_secs_f64()
                    >= self.services.config.recording_stream_idle_seconds
                {
                    return Err(StreamWorkError::Pending("Waiting for durable audio."));
                }
                self.pause(Duration::from_secs_f64(
                    self.services.config.recording_stream_poll_seconds,
                ))
                .await?;
                continue;
            };
            let began = tokio::time::Instant::now();
            let mut sent = 0;
            for packet in pcm.chunks(640) {
                self.check_stop()?;
                sender
                    .send(serde_json::json!({"type":"input_audio_buffer.append",
                    "audio":base64::engine::general_purpose::STANDARD.encode(packet)}))
                    .await?;
                sent += packet.len();
                self.progress.lock().await.cursor += (packet.len() / 2) as u64;
                let target =
                    sent as f64 / 32000.0 / self.services.config.recording_stream_replay_speed;
                self.pause(Duration::from_secs_f64(
                    (target - began.elapsed().as_secs_f64()).max(0.0),
                ))
                .await?;
            }
            index += 1;
            waiting = tokio::time::Instant::now();
            self.publish_progress().await?;
        }
    }
    async fn receive(self: Arc<Self>, mut stream: Upstream) -> Result<Completed, StreamWorkError> {
        let mut stop = self.stop.clone();
        let mut cancel = self.cancel.clone();
        loop {
            self.check_stop()?;
            let event = tokio::select! {
                result = stream.recv() => result?,
                _ = stop.changed() => return Err(stopping()),
                _ = cancel.changed() => return Err(stopping()),
            };
            let kind = event["type"].as_str().unwrap_or("");
            if kind == "error" || kind == format!("{PREFIX}failed") {
                return Err(provider_failed());
            }
            if kind == format!("{PREFIX}completed") {
                if !self.eof.load(Ordering::Acquire) {
                    return Err(provider_failed());
                }
                let text = event["transcript"].as_str().ok_or_else(provider_failed)?;
                if text.len() > MAX_TEXT_BYTES {
                    return Err(StreamWorkError::Terminal(
                        "Streaming transcript exceeds supported bounds.",
                    ));
                }
                return Ok(Completed::Received(text.into()));
            }
            {
                let mut progress = self.progress.lock().await;
                if kind == format!("{PREFIX}delta") {
                    let text = event["delta"].as_str().ok_or_else(provider_failed)?;
                    if text.len() > MAX_TEXT_BYTES - progress.delta.len() {
                        return Err(StreamWorkError::Terminal(
                            "Streaming hypothesis exceeds supported bounds.",
                        ));
                    }
                    progress.delta.push_str(text);
                    progress.intermediate.clear();
                } else if kind == format!("{PREFIX}intermediate") {
                    let text = event["intermediate"].as_str().ok_or_else(provider_failed)?;
                    if text.len() > MAX_TEXT_BYTES - progress.delta.len() {
                        return Err(StreamWorkError::Terminal(
                            "Streaming hypothesis exceeds supported bounds.",
                        ));
                    }
                    progress.intermediate = text.into();
                }
            }
            self.publish_progress().await?;
        }
    }
    async fn heartbeat(self: Arc<Self>) -> Result<Completed, StreamWorkError> {
        loop {
            self.check_stop()?;
            self.leases.renew(&self.id, &self.owner).await?;
            self.receipt
                .renew(self.services.config.queue_visibility_seconds)
                .await?;
            self.pause(Duration::from_secs_f64(
                self.services.config.recording_stream_heartbeat_seconds,
            ))
            .await?;
        }
    }
}

async fn process(
    session: Arc<Session>,
    provider: &MaiStreamingProvider,
    cancel: watch::Sender<bool>,
) -> Result<(String, u64), StreamWorkError> {
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs_f64(session.services.config.recording_stream_session_seconds);
    let record = session.services.recording(&session.id).await?;
    let stream = tokio::time::timeout_at(deadline, provider.session(&record.language))
        .await
        .map_err(|_| StreamWorkError::Pending("Model session rotation."))??;
    let sender = stream.sender();
    let mut tasks = JoinSet::new();
    tasks.spawn(session.clone().send_audio(sender.clone()));
    tasks.spawn(session.clone().receive(stream));
    tasks.spawn(session.clone().heartbeat());
    let mut final_deadline = deadline;
    let mut sent = false;
    let mut final_text = None;
    let mut stop = session.stop.clone();
    let mut result = loop {
        if sent && final_text.is_some() {
            break Ok(());
        }
        tokio::select! {
            _ = stop.changed() => break Err(stopping()),
            _ = tokio::time::sleep_until(deadline) => break Err(StreamWorkError::Pending("Model session rotation.")),
            _ = tokio::time::sleep_until(final_deadline), if sent => break Err(provider_failed()),
            completed = tasks.join_next() => {
                match completed {
                    Some(Ok(Ok(Completed::Sent))) => { sent = true; final_deadline = tokio::time::Instant::now() + Duration::from_secs(45); }
                    Some(Ok(Ok(Completed::Received(text)))) => {
                        if !session.eof.load(Ordering::Acquire) { break Err(provider_failed()); }
                        final_text = Some(text);
                    }
                    Some(Ok(Err(error))) => break Err(error),
                    Some(Err(_)) | None => break Err(provider_failed()),
                }
            }
        }
    };
    cancel.send_replace(true);
    while let Some(completed) = tasks.join_next().await {
        match completed {
            Ok(Err(StreamWorkError::Pending("Worker shutdown."))) => {}
            Ok(Err(error)) => {
                eprintln!("Continuous stream task settled with an error: {error}");
                if result.is_ok() {
                    result = Err(error);
                }
            }
            Err(_) => {
                eprintln!("Continuous stream task failed.");
                if result.is_ok() {
                    result = Err(provider_failed());
                }
            }
            Ok(Ok(_)) => {}
        }
    }
    if sender.close().await.is_err() {
        eprintln!("Continuous MAI close was interrupted.");
    }
    result?;
    let text = final_text.ok_or_else(provider_failed)?;
    Ok((text, session.progress.lock().await.cursor))
}

pub async fn transcribe_recording(
    services: Arc<Services>,
    provider: Arc<MaiStreamingProvider>,
    id: String,
    receipt: Arc<QueueReceipt>,
    stop: watch::Receiver<bool>,
) -> Result<(), StreamWorkError> {
    if *stop.borrow() || stop.has_changed().is_err() {
        return Err(stopping());
    }
    let owner = uuid()?.replace('-', "");
    let record = match services.recording(&id).await {
        Ok(record) => record,
        Err(error) if error.status == 404 => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if record.transcription_mode != "streaming" {
        return Err(StreamWorkError::Terminal(
            "Continuous work cannot run over a legacy recording.",
        ));
    }
    let leases = Arc::new(StreamLeases::new(services.clone()));
    if leases.claim(&id, &owner).await?.is_none() {
        return Ok(());
    }
    let (cancel, cancelled) = watch::channel(false);
    let session = Arc::new(Session {
        services: services.clone(),
        leases: leases.clone(),
        receipt,
        id: id.clone(),
        owner: owner.clone(),
        stop,
        cancel: cancelled,
        progress: Mutex::new(Progress {
            cursor: 0,
            delta: String::new(),
            intermediate: String::new(),
            published: None,
        }),
        eof: AtomicBool::new(false),
    });
    let result = async {
        let (text, cursor) = process(session.clone(), &provider, cancel).await?;
        if *session.stop.borrow() || session.stop.has_changed().is_err() {
            return Err(stopping());
        }
        let path = format!("stream/{id}/{owner}.txt");
        services
            .blobs
            .put(
                &services.config.raw_container,
                &path,
                text.as_bytes().to_vec(),
                "text/plain; charset=utf-8",
            )
            .await?;
        leases
            .update_owned(&id, &owner, |record| {
                record.stream_asr_ready = true;
                record.stream_raw_path = Some(path.clone());
                record.streamed_samples = cursor;
                record.stream_preview = preview(&text, services.config.preview_max_chars);
                record.stream_error = None;
            })
            .await?;
        services.arm_finalize(&id).await?;
        Ok(())
    }
    .await;
    let error = match &result {
        Ok(()) => None,
        Err(StreamWorkError::Pending("Model session rotation.")) => Some("session_rotated"),
        Err(StreamWorkError::Pending(_)) => Some("awaiting_audio_or_restart"),
        Err(_) => Some("stream_failed"),
    };
    if let Err(error) = leases.release(&id, &owner, error).await {
        if let Err(previous) = &result {
            eprintln!("Continuous stream failed before release: {previous}");
        }
        return Err(error);
    }
    result
}

#[cfg(test)]
#[path = "continuous_tests.rs"]
mod tests;
