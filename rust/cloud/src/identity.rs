use crate::problem::Problem;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;

struct Token {
    value: String,
    expires: i64,
}

pub struct ManagedIdentity {
    endpoint: url::Url,
    header: String,
    client_id: String,
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, Token>>,
}

impl ManagedIdentity {
    #[cfg(test)]
    pub(crate) fn fixture(endpoint: &str) -> Self {
        Self::new(endpoint, "synthetic-header".into(), "synthetic-uami".into()).unwrap()
    }

    pub fn from_environment() -> Result<Self, Problem> {
        let endpoint = std::env::var("IDENTITY_ENDPOINT")
            .map_err(|_| Problem::validation("ACA IDENTITY_ENDPOINT is required."))?;
        let header = std::env::var("IDENTITY_HEADER")
            .map_err(|_| Problem::validation("ACA IDENTITY_HEADER is required."))?;
        let client_id = std::env::var("AZURE_CLIENT_ID")
            .map_err(|_| Problem::validation("User-assigned AZURE_CLIENT_ID is required."))?;
        Self::new(&endpoint, header, client_id)
    }
    fn new(endpoint: &str, header: String, client_id: String) -> Result<Self, Problem> {
        let endpoint = url::Url::parse(endpoint)
            .map_err(|_| Problem::validation("Invalid managed identity endpoint."))?;
        let local = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host.parse::<std::net::Ipv4Addr>().is_ok_and(|address| {
                    address.is_loopback() || address.is_private() || address.is_link_local()
                })
        });
        if endpoint.scheme() != "http"
            || !local
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || header.is_empty()
            || client_id.trim().is_empty()
        {
            return Err(Problem::validation(
                "Managed identity requires the injected private ACA endpoint/header and a user-assigned client ID.",
            ));
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        Ok(Self {
            endpoint,
            header,
            client_id,
            http,
            tokens: Mutex::new(HashMap::new()),
        })
    }
    pub async fn token(&self, resource: &str) -> Result<String, Problem> {
        if !matches!(
            resource,
            "https://storage.azure.com/"
                | "https://cognitiveservices.azure.com/"
                | "https://webpubsub.azure.com/"
        ) {
            return Err(Problem::validation(
                "Managed identity resource is not a server-approved data-plane scope.",
            ));
        }
        let mut tokens = self.tokens.lock().await;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if let Some(token) = tokens
            .get(resource)
            .filter(|token| token.expires > now + 300)
        {
            return Ok(token.value.clone());
        }
        let mut uri = self.endpoint.clone();
        uri.query_pairs_mut()
            .append_pair("api-version", "2019-08-01")
            .append_pair("resource", resource)
            .append_pair("client_id", &self.client_id);
        let mut response = self
            .http
            .get(uri)
            .header("X-IDENTITY-HEADER", &self.header)
            .send()
            .await
            .map_err(|_| Problem::unavailable("Managed identity token acquisition failed."))?;
        if !response.status().is_success() {
            return Err(Problem::unavailable(
                "Managed identity endpoint rejected the token request.",
            ));
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Problem::unavailable("Managed identity response interrupted."))?
        {
            if chunk.len() > 1024 * 1024 - data.len() {
                return Err(Problem::unavailable(
                    "Managed identity response exceeded supported bounds.",
                ));
            }
            data.extend_from_slice(&chunk);
        }
        let payload: serde_json::Value = serde_json::from_slice(&data)
            .map_err(|_| Problem::unavailable("Invalid managed identity token response."))?;
        let token = payload
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Problem::unavailable("Managed identity returned no access token."))?
            .to_owned();
        let expiry = match payload.get("expires_on") {
            Some(serde_json::Value::String(value)) => value.parse::<i64>().ok(),
            Some(value) => value.as_i64(),
            None => None,
        }
        .filter(|expiry| *expiry > now)
        .ok_or_else(|| Problem::unavailable("Managed identity token expiry is invalid."))?;
        tokens.insert(
            resource.into(),
            Token {
                value: token.clone(),
                expires: expiry,
            },
        );
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_local_identity_request_selects_uami_and_caches_only_approved_resource_tokens() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = serde_json::json!({"access_token":"synthetic-token","expires_on":(time::OffsetDateTime::now_utc().unix_timestamp()+3600).to_string()}).to_string();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let count = stream.read(&mut buffer).await.unwrap();
            let request = std::str::from_utf8(&buffer[..count]).unwrap();
            assert!(request.contains("client_id=synthetic-uami"));
            assert!(request.contains("api-version=2019-08-01"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-identity-header: synthetic-header")
            );
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let credential = ManagedIdentity::new(
            &format!("http://127.0.0.1:{port}/token"),
            "synthetic-header".into(),
            "synthetic-uami".into(),
        )
        .unwrap();
        assert_eq!(
            credential
                .token("https://storage.azure.com/")
                .await
                .unwrap(),
            "synthetic-token"
        );
        assert_eq!(
            credential
                .token("https://storage.azure.com/")
                .await
                .unwrap(),
            "synthetic-token"
        );
        assert!(
            credential
                .token("https://attacker.example.invalid/")
                .await
                .is_err()
        );
        server.await.unwrap();
    }
    #[test]
    fn credential_cannot_send_identity_header_to_a_public_or_user_supplied_endpoint() {
        for endpoint in [
            "http://attacker.example.invalid/",
            "https://127.0.0.1:1234/",
            "http://user:pass@127.0.0.1/",
            "http://127.0.0.1/?resource=attacker",
        ] {
            assert!(
                ManagedIdentity::new(endpoint, "synthetic-header".into(), "fixture".into())
                    .is_err()
            );
        }
    }
}
