use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, BodyDataStream, HttpBody};
use axum::extract::Request;
use futures_util::StreamExt;
use tokio::sync::Mutex;

const REJECT_DRAIN_BYTES: usize = 64 * 1024;
const REJECT_DRAIN_TIME: Duration = Duration::from_millis(100);

pub(crate) struct PendingBody {
    source: Arc<Mutex<Option<BodyDataStream>>>,
    drain_allowed: bool,
}

impl PendingBody {
    pub fn retain(request: Request) -> (Request, Self) {
        let (parts, body) = request.into_parts();
        let drain_allowed = parts
            .headers
            .get("Content-Length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length <= REJECT_DRAIN_BYTES);
        let source = Arc::new(Mutex::new(
            (!body.is_end_stream()).then(|| body.into_data_stream()),
        ));
        let body = Body::from_stream(futures_util::stream::unfold(
            source.clone(),
            |source| async move {
                let chunk = {
                    let mut stream = source.lock().await;
                    match stream.as_mut() {
                        Some(body) => {
                            let chunk = body.next().await;
                            if chunk.is_none() {
                                *stream = None;
                            }
                            chunk
                        }
                        None => None,
                    }
                };
                chunk.map(|chunk| (chunk, source))
            },
        ));
        (
            Request::from_parts(parts, body),
            Self {
                source,
                drain_allowed,
            },
        )
    }

    pub async fn finish(self) -> bool {
        let drain = async {
            let mut source = self.source.lock().await;
            let Some(body) = source.as_mut() else {
                return true;
            };
            if !self.drain_allowed {
                eprintln!(
                    "Unread request body has unknown or oversized length; closing connection."
                );
                return false;
            }
            let mut count = 0;
            while let Some(chunk) = body.next().await {
                match chunk {
                    Ok(chunk) if chunk.len() <= REJECT_DRAIN_BYTES - count => {
                        count += chunk.len();
                    }
                    Ok(_) => {
                        eprintln!("Unread request body exceeds bounded connection drain.");
                        return false;
                    }
                    Err(_) => {
                        eprintln!("Unread request body failed during bounded connection drain.");
                        return false;
                    }
                }
            }
            true
        };
        match tokio::time::timeout(REJECT_DRAIN_TIME, drain).await {
            Ok(complete) => complete,
            Err(_) => {
                eprintln!("Unread request body exceeded connection drain deadline.");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn consumed_or_small_rejected_body_is_drained_without_changing_request_metadata() {
        for consumed in [false, true] {
            let request = Request::builder()
                .uri("/synthetic")
                .header("X-Synthetic", "retained")
                .header("Content-Length", "14")
                .body(Body::from("synthetic body"))
                .unwrap();
            let (request, pending) = PendingBody::retain(request);
            assert_eq!(request.uri(), "/synthetic");
            assert_eq!(request.headers()["X-Synthetic"], "retained");
            if consumed {
                assert_eq!(
                    axum::body::to_bytes(request.into_body(), 1024)
                        .await
                        .unwrap(),
                    "synthetic body"
                );
            } else {
                drop(request);
            }
            assert!(pending.finish().await);
        }
    }

    #[tokio::test]
    async fn stalled_or_oversized_unread_bodies_close_instead_of_waiting_or_allocating_unboundedly()
    {
        let (request, pending) = PendingBody::retain(
            Request::builder()
                .header("Content-Length", "1")
                .body(Body::from(vec![0; REJECT_DRAIN_BYTES + 1]))
                .unwrap(),
        );
        drop(request);
        assert!(!pending.finish().await);
        let stream = futures_util::stream::pending::<Result<Vec<u8>, std::io::Error>>();
        let (request, pending) = PendingBody::retain(
            Request::builder()
                .header("Content-Length", "1")
                .body(Body::from_stream(stream))
                .unwrap(),
        );
        drop(request);
        let started = tokio::time::Instant::now();
        assert!(!pending.finish().await);
        assert!(started.elapsed() >= REJECT_DRAIN_TIME);
    }
}
