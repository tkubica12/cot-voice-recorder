use std::fmt;
use std::time::Duration;

use crate::auth::{DesktopClientConfig, TokenResponse};
use crate::{MAX_AUTH_JSON_BYTES, ProtocolError};

#[derive(Debug)]
pub enum TokenNetworkError {
    InvalidEndpoint,
    Transport,
    Timeout,
    BodyTooLarge,
    Protocol(ProtocolError),
}

impl fmt::Display for TokenNetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("Invalid Google token endpoint."),
            Self::Transport => f.write_str("Google token transport failed."),
            Self::Timeout => f.write_str("Google token request timed out."),
            Self::BodyTooLarge => f.write_str("Google token response exceeds supported bounds."),
            Self::Protocol(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TokenNetworkError {}

pub struct GoogleTokenClient {
    client: reqwest::Client,
    allow_test_loopback: bool,
}

impl GoogleTokenClient {
    #[cfg(test)]
    pub(crate) fn for_loopback_tests() -> Self {
        let mut client = Self::new().expect("test HTTP client");
        client.allow_test_loopback = true;
        client
    }

    pub fn new() -> Result<Self, TokenNetworkError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| TokenNetworkError::Transport)?;
        Ok(Self {
            client,
            allow_test_loopback: false,
        })
    }

    pub async fn exchange_code(
        &self,
        config: &DesktopClientConfig,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse, TokenNetworkError> {
        self.post(
            config,
            &[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("code", code),
                ("code_verifier", verifier),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri),
            ],
        )
        .await
    }

    pub async fn refresh(
        &self,
        config: &DesktopClientConfig,
        refresh_token: &str,
    ) -> Result<TokenResponse, TokenNetworkError> {
        self.post(
            config,
            &[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ],
        )
        .await
    }

    async fn post(
        &self,
        config: &DesktopClientConfig,
        form: &[(&str, &str)],
    ) -> Result<TokenResponse, TokenNetworkError> {
        let endpoint = &config.token_uri;
        let allowed_loopback = self.allow_test_loopback
            && endpoint.scheme() == "http"
            && endpoint.host_str() == Some("127.0.0.1")
            && endpoint.port().is_some();
        if (!allowed_loopback && endpoint.scheme() != "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(TokenNetworkError::InvalidEndpoint);
        }
        let mut response = self
            .client
            .post(endpoint.clone())
            .form(form)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    TokenNetworkError::Timeout
                } else {
                    TokenNetworkError::Transport
                }
            })?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_AUTH_JSON_BYTES as u64)
        {
            return Err(TokenNetworkError::BodyTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                TokenNetworkError::Timeout
            } else {
                TokenNetworkError::Transport
            }
        })? {
            if chunk.len() > MAX_AUTH_JSON_BYTES - body.len() {
                return Err(TokenNetworkError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let parsed = TokenResponse::parse(&body);
        if status.is_success() {
            return parsed.map_err(TokenNetworkError::Protocol);
        }
        if let Ok(response) = parsed {
            if response.error.is_some() {
                return Ok(response);
            }
        }
        Ok(TokenResponse {
            error: Some(
                if status.is_server_error() {
                    "server_error"
                } else {
                    "invalid_request"
                }
                .into(),
            ),
            ..Default::default()
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::auth::RefreshOutcome;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    pub(crate) fn endpoint(status: &str, body: &[u8]) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_owned();
        let body = body.to_vec();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(position) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..position]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= position + 4 + length {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://127.0.0.1:{port}/token"), server)
    }

    fn config(endpoint: &str) -> DesktopClientConfig {
        let mut config = DesktopClientConfig::parse(
            br#"{"installed":{"client_id":"fixture-id","client_secret":"fixture-secret"}}"#,
        )
        .unwrap();
        config.token_uri = url::Url::parse(endpoint).unwrap();
        config
    }

    #[tokio::test]
    async fn real_http_refresh_preserves_form_and_id_token_classification() {
        let (url, server) = endpoint(
            "200 OK",
            br#"{"id_token":"synthetic-id","refresh_token":"rotated"}"#,
        );
        let mut client = GoogleTokenClient::new().unwrap();
        client.allow_test_loopback = true;
        let response = client
            .refresh(&config(&url), "synthetic-refresh")
            .await
            .unwrap();
        assert_eq!(response.classify_refresh(), RefreshOutcome::Success);
        let request = server.join().unwrap();
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let fields: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(fields["grant_type"], "refresh_token");
        assert_eq!(fields["client_id"], "fixture-id");
        assert_eq!(fields["refresh_token"], "synthetic-refresh");
        assert!(!format!("{response:?}").contains("synthetic-id"));
    }

    #[tokio::test]
    async fn real_http_error_classification_is_bounded_and_explicit() {
        for (status, body, outcome) in [
            (
                "400 Bad Request",
                br#"{"error":"invalid_grant"}"#.as_slice(),
                RefreshOutcome::RefreshRevoked,
            ),
            (
                "503 Service Unavailable",
                b"not-json",
                RefreshOutcome::Transient,
            ),
            (
                "200 OK",
                br#"{"access_token":"synthetic-access"}"#,
                RefreshOutcome::NoIdToken,
            ),
        ] {
            let (url, server) = endpoint(status, body);
            let mut client = GoogleTokenClient::new().unwrap();
            client.allow_test_loopback = true;
            assert_eq!(
                client
                    .refresh(&config(&url), "fixture")
                    .await
                    .unwrap()
                    .classify_refresh(),
                outcome
            );
            server.join().unwrap();
        }
        let (url, server) = endpoint("200 OK", b"bad-json");
        let mut client = GoogleTokenClient::new().unwrap();
        client.allow_test_loopback = true;
        assert!(matches!(
            client.refresh(&config(&url), "fixture").await,
            Err(TokenNetworkError::Protocol(_))
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn production_transport_rejects_plain_http() {
        let client = GoogleTokenClient::new().unwrap();
        assert!(matches!(
            client
                .refresh(&config("http://127.0.0.1:1/token"), "fixture")
                .await,
            Err(TokenNetworkError::InvalidEndpoint)
        ));
    }
}
