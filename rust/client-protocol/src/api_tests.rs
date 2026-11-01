use super::*;
use crate::auth::{CachedTokens, DesktopClientConfig};
use crate::network::GoogleTokenClient;
use base64::Engine;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

#[derive(Clone, Default)]
pub(crate) struct Store(Arc<Mutex<Option<CachedTokens>>>);

impl TokenPersistence for Store {
    type Error = std::io::Error;
    fn load(&self) -> Result<Option<CachedTokens>, Self::Error> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, value: &CachedTokens) -> Result<(), Self::Error> {
        *self.0.lock().unwrap() = Some(value.clone());
        Ok(())
    }
    fn clear(&self) -> Result<(), Self::Error> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

fn jwt() -> String {
    let data = format!(
        "{{\"exp\":{}}}",
        OffsetDateTime::now_utc().unix_timestamp() + 3600
    );
    format!(
        "e30.{}.",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    )
}

pub(crate) fn server(
    replies: Vec<(u16, String)>,
) -> (url::Url, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut bytes = [0; 1024];
            loop {
                let count = stream.read(&mut bytes).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
                if request.windows(4).any(|value| value == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
        }
        requests
    });
    (
        url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
        task,
    )
}

pub(crate) async fn make_client(base: url::Url) -> (ApiClient<Store>, String) {
    let store = Store::default();
    let token = jwt();
    store
        .save(&CachedTokens {
            refresh_token: Some("fixture".into()),
            id_token: Some(token.clone()),
            ..Default::default()
        })
        .unwrap();
    let config = DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"fixture","client_secret":"fixture"}}"#,
    )
    .unwrap();
    let auth = Arc::new(
        AuthRuntime::initialize(Some(config), GoogleTokenClient::new().unwrap(), store)
            .await
            .unwrap(),
    );
    (
        ApiClient {
            client: reqwest::Client::new(),
            auth,
            base,
            max_retries: 3,
            retry_delay: Duration::ZERO,
        },
        token,
    )
}

pub(crate) fn transcript() -> String {
    let now = OffsetDateTime::now_utc();
    serde_json::json!({
        "transcript_id":"t","recording_id":"r","body":"synthetic transcript","preview":"synthetic",
        "completed_at":now.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "expires_at":(now + time::Duration::hours(48)).format(&time::format_description::well_known::Rfc3339).unwrap(),
        "character_count":20,"source":"recording","raw_body":null,
        "language":"en","refine_model":"none","transcribe_model":"fixture"
    })
    .to_string()
}

#[tokio::test]
async fn real_authenticated_fetch_uses_id_token_and_keeps_body_out_of_debug() {
    let (base, task) = server(vec![(200, transcript())]);
    let (client, token) = make_client(base).await;
    let result = client.transcript("t").await.unwrap();
    assert_eq!(result.body, "synthetic transcript");
    assert!(!format!("{result:?}").contains("synthetic transcript"));
    let requests = task.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/transcripts/t "));
    assert!(requests[0].to_ascii_lowercase().contains(&format!(
        "authorization: bearer {}",
        token.to_ascii_lowercase()
    )));
}

#[tokio::test]
async fn forbidden_is_terminal_and_problem_details_remain_structured() {
    let (base, task) = server(vec![(
        403,
        r#"{"type":"about:blank","title":"fixture forbidden","status":403}"#.into(),
    )]);
    let (client, _) = make_client(base).await;
    let error = client.transcript("t").await.unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::Forbidden);
    assert_eq!(
        error.problem.unwrap().title.as_deref(),
        Some("fixture forbidden")
    );
    assert_eq!(task.join().unwrap().len(), 1);
}

#[tokio::test]
async fn server_failures_retry_boundedly_without_losing_success_shape() {
    let (base, task) = server(vec![(503, "{}".into()), (200, transcript())]);
    let (client, _) = make_client(base).await;
    assert_eq!(client.transcript("t").await.unwrap().transcript_id, "t");
    assert_eq!(task.join().unwrap().len(), 2);
    let (base, task) = server(vec![(200, "{}".into())]);
    let (client, _) = make_client(base).await;
    assert!(client.transcript("t").await.unwrap_err().response_invalid);
    task.join().unwrap();
}

#[tokio::test]
async fn unauthorized_refreshes_exactly_once_before_retrying() {
    let (base, api_server) = server(vec![(401, "{}".into()), (401, "{}".into())]);
    let updated = jwt();
    let response = serde_json::json!({"id_token":updated}).to_string();
    let (token_endpoint, token_server) =
        crate::network::tests::endpoint("200 OK", response.as_bytes());
    let mut config = DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"fixture","client_secret":"fixture"}}"#,
    )
    .unwrap();
    config.token_uri = url::Url::parse(&token_endpoint).unwrap();
    let store = Store::default();
    store
        .save(&CachedTokens {
            refresh_token: Some("fixture".into()),
            id_token: Some(jwt()),
            ..Default::default()
        })
        .unwrap();
    let auth = Arc::new(
        AuthRuntime::initialize(Some(config), GoogleTokenClient::for_loopback_tests(), store)
            .await
            .unwrap(),
    );
    let client = ApiClient {
        client: reqwest::Client::new(),
        auth,
        base,
        max_retries: 3,
        retry_delay: Duration::ZERO,
    };
    let error = client.transcript("t").await.unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::Unauthorized);
    assert_eq!(api_server.join().unwrap().len(), 2);
    assert!(
        token_server
            .join()
            .unwrap()
            .contains("grant_type=refresh_token")
    );
}
