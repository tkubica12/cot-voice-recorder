use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) struct Request {
    pub target: url::Url,
    pub headers: String,
    pub body: Vec<u8>,
}
pub(crate) struct Fixture {
    pub endpoint: url::Url,
    requests: tokio::sync::mpsc::Receiver<Request>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub async fn next(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn finish(self) {
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}
pub(crate) fn token() -> (u16, String) {
    (200, serde_json::json!({"access_token":"fixture-data-plane", "expires_on":(crate::services::now().unix_timestamp()+3600).to_string()}).to_string())
}
pub(crate) async fn serve(responses: Vec<(u16, String)>) -> Fixture {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint: url::Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let (sender, requests) = tokio::sync::mpsc::channel(32);
    let base = endpoint.clone();
    let task = tokio::spawn(async move {
        for (status, body) in responses {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut data = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 8192];
                let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(count > 0);
                data.extend_from_slice(&chunk[..count]);
                assert!(data.len() < 4 * 1024 * 1024);
                if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(data[..header_end].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 4 * 1024 * 1024);
            while data.len() < header_end + length {
                let mut chunk = [0; 8192];
                let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(count > 0);
                data.extend_from_slice(&chunk[..count]);
            }
            let target = base
                .join(
                    headers
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap(),
                )
                .unwrap();
            sender
                .send(Request {
                    target,
                    headers,
                    body: data[header_end..header_end + length].to_vec(),
                })
                .await
                .unwrap();
            stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    Fixture {
        endpoint,
        requests,
        task,
    }
}
pub(crate) fn config(endpoint: &url::Url) -> crate::config::Config {
    let mut config = (*crate::services::tests::fixture().config).clone();
    config.foundry = endpoint.clone();
    config.speech = Some(endpoint.clone());
    config.webpubsub = endpoint.clone();
    config
}
pub(crate) fn part<'a>(body: &'a [u8], name: &str) -> &'a [u8] {
    let marker = format!("name=\"{name}\"");
    let start = body
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .unwrap();
    let rest = &body[start..];
    let start = rest
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let rest = &rest[start..];
    let end = rest
        .windows(4)
        .position(|window| window == b"\r\n--")
        .unwrap();
    &rest[..end]
}
