use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use url::Url;

use crate::{MAX_AUTH_JSON_BYTES, ProtocolError, json};

#[derive(Clone)]
pub struct DesktopClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: Url,
    pub token_uri: Url,
}

impl fmt::Debug for DesktopClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DesktopClientConfig(<redacted>)")
    }
}

impl DesktopClientConfig {
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        let root = json(data, "OAuth client configuration")?;
        let node = root
            .get("installed")
            .or_else(|| root.get("web"))
            .filter(|node| node.is_object())
            .ok_or(ProtocolError::InvalidField(
                "installed/web OAuth client root",
            ))?;
        let required = |name: &'static str| {
            node.get(name)
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or(ProtocolError::InvalidField(name))
        };
        let endpoint = |name: &'static str, fallback: &str| {
            let value = node
                .get(name)
                .and_then(|value| value.as_str())
                .unwrap_or(fallback);
            let uri = Url::parse(value).map_err(|_| ProtocolError::InvalidField(name))?;
            if uri.scheme() != "https"
                || uri.host_str().is_none()
                || !uri.username().is_empty()
                || uri.password().is_some()
                || uri.fragment().is_some()
            {
                return Err(ProtocolError::InvalidField(name));
            }
            Ok(uri)
        };
        Ok(Self {
            client_id: required("client_id")?,
            client_secret: required("client_secret")?,
            auth_uri: endpoint("auth_uri", "https://accounts.google.com/o/oauth2/auth")?,
            token_uri: endpoint("token_uri", "https://oauth2.googleapis.com/token")?,
        })
    }
}

#[derive(Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl fmt::Debug for PkcePair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PkcePair(<redacted>)")
    }
}

impl PkcePair {
    pub fn create() -> Result<Self, ProtocolError> {
        let mut verifier = [0; 32];
        let mut state = [0; 16];
        getrandom::fill(&mut verifier).map_err(|_| ProtocolError::RandomUnavailable)?;
        getrandom::fill(&mut state).map_err(|_| ProtocolError::RandomUnavailable)?;
        Self::from_entropy(&verifier, &state)
    }

    pub fn from_entropy(verifier: &[u8], state: &[u8]) -> Result<Self, ProtocolError> {
        if !(32..=96).contains(&verifier.len()) || !(16..=64).contains(&state.len()) {
            return Err(ProtocolError::InvalidField("PKCE entropy size"));
        }
        let verifier = URL_SAFE_NO_PAD.encode(verifier);
        let challenge = Self::challenge(&verifier)?;
        Ok(Self {
            verifier,
            challenge,
            state: URL_SAFE_NO_PAD.encode(state),
        })
    }

    pub fn challenge(verifier: &str) -> Result<String, ProtocolError> {
        if !(43..=128).contains(&verifier.len())
            || !verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        {
            return Err(ProtocolError::InvalidField("PKCE verifier"));
        }
        Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
    }
}

pub fn authorization_url(
    config: &DesktopClientConfig,
    redirect_uri: &str,
    pkce: &PkcePair,
    login_hint: Option<&str>,
) -> Result<Url, ProtocolError> {
    if PkcePair::challenge(&pkce.verifier)? != pkce.challenge || pkce.state.is_empty() {
        return Err(ProtocolError::InvalidField("PKCE pair"));
    }
    let redirect =
        Url::parse(redirect_uri).map_err(|_| ProtocolError::InvalidField("redirect_uri"))?;
    if redirect.scheme() != "http"
        || redirect.host_str() != Some("127.0.0.1")
        || redirect.port().is_none()
        || !redirect.username().is_empty()
        || redirect.password().is_some()
        || redirect.query().is_some()
        || redirect.fragment().is_some()
    {
        return Err(ProtocolError::InvalidField("loopback redirect_uri"));
    }
    let mut url = config.auth_uri.clone();
    let mut query = url.query_pairs_mut();
    query.extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", "openid email profile"),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", pkce.state.as_str()),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    if let Some(hint) = login_hint.filter(|value| !value.is_empty()) {
        query.append_pair("login_hint", hint);
    }
    drop(query);
    Ok(url)
}

pub fn validate_callback(query: &str, expected_state: &str) -> Result<String, ProtocolError> {
    if query.len() > 16 * 1024 {
        return Err(ProtocolError::TooLarge);
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (key, value) in url::form_urlencoded::parse(query.trim_start_matches('?').as_bytes()) {
        let target = match key.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            _ => continue,
        };
        if target.replace(value.into_owned()).is_some() {
            return Err(ProtocolError::InvalidCallback(
                "duplicate security parameter",
            ));
        }
    }
    if error.is_some_and(|value| !value.is_empty()) {
        return Err(ProtocolError::InvalidCallback(
            "provider denied authorization",
        ));
    }
    let state = state
        .filter(|value| !value.is_empty())
        .ok_or(ProtocolError::InvalidCallback("state missing"))?;
    if expected_state.is_empty() || !bool::from(state.as_bytes().ct_eq(expected_state.as_bytes())) {
        return Err(ProtocolError::InvalidCallback("state mismatch"));
    }
    code.filter(|value| !value.is_empty())
        .ok_or(ProtocolError::InvalidCallback("code missing"))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct CachedTokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub id_token_expires_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl fmt::Debug for CachedTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedTokens")
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("has_id_token", &self.id_token.is_some())
            .field("has_expiry", &self.id_token_expires_at.is_some())
            .field("has_email", &self.email.is_some())
            .finish()
    }
}

impl CachedTokens {
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        if data.len() > MAX_AUTH_JSON_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        serde_json::from_slice(data).map_err(|_| ProtocolError::InvalidJson("cached tokens"))
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let data =
            serde_json::to_vec(self).map_err(|_| ProtocolError::InvalidJson("cached tokens"))?;
        if data.len() > MAX_AUTH_JSON_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        Ok(data)
    }
}

#[derive(Clone)]
pub struct JwtClaims {
    pub expires_at: Option<OffsetDateTime>,
    pub email: Option<String>,
    pub audience: Option<String>,
    pub subject: Option<String>,
}

impl fmt::Debug for JwtClaims {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JwtClaims(<unverified, redacted>)")
    }
}

impl JwtClaims {
    /// Read scheduling/display claims only. The backend must still verify Google's signature.
    pub fn parse(token: &str) -> Result<Self, ProtocolError> {
        if token.len() > 64 * 1024 {
            return Err(ProtocolError::TooLarge);
        }
        let payload = token
            .split('.')
            .nth(1)
            .filter(|value| !value.is_empty())
            .ok_or(ProtocolError::InvalidField("JWT payload"))?;
        let mut base64 = payload.replace('-', "+").replace('_', "/");
        match base64.len() % 4 {
            0 => {}
            2 => base64.push_str("=="),
            3 => base64.push('='),
            _ => return Err(ProtocolError::InvalidField("JWT base64 length")),
        }
        let decoded = STANDARD
            .decode(base64)
            .map_err(|_| ProtocolError::InvalidField("JWT base64"))?;
        let root = json(&decoded, "JWT payload")?;
        if !root.is_object() {
            return Err(ProtocolError::InvalidField("JWT payload object"));
        }
        let expires_at = match root.get("exp") {
            Some(serde_json::Value::Number(number)) => {
                let seconds = number
                    .as_i64()
                    .ok_or(ProtocolError::InvalidField("JWT exp"))?;
                if !(-62_135_596_800..=253_402_300_799).contains(&seconds) {
                    return Err(ProtocolError::InvalidField("JWT exp range"));
                }
                Some(
                    OffsetDateTime::from_unix_timestamp(seconds)
                        .map_err(|_| ProtocolError::InvalidField("JWT exp range"))?,
                )
            }
            _ => None,
        };
        let string = |name| {
            root.get(name)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        let audience = match root.get("aud") {
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(serde_json::Value::Array(values)) => match values.first() {
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or(ProtocolError::InvalidField("JWT aud"))?
                        .to_owned(),
                ),
                None => None,
            },
            Some(serde_json::Value::Null) | None => None,
            _ => return Err(ProtocolError::InvalidField("JWT aud")),
        };
        Ok(Self {
            expires_at,
            email: string("email"),
            audience,
            subject: string("sub"),
        })
    }

    pub fn is_expiring(
        &self,
        now: OffsetDateTime,
        skew_seconds: i64,
    ) -> Result<bool, ProtocolError> {
        if skew_seconds < 0 {
            return Err(ProtocolError::InvalidField("refresh skew"));
        }
        Ok(self
            .expires_at
            .is_none_or(|expiry| expiry.unix_timestamp() - now.unix_timestamp() <= skew_seconds))
    }
}

#[derive(Clone, Deserialize, Default)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub id_token: Option<String>,
    pub refresh_token: Option<String>,
    pub error: Option<String>,
}

impl fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenResponse(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Success,
    RefreshRevoked,
    NoIdToken,
    Transient,
}

impl TokenResponse {
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        if data.len() > MAX_AUTH_JSON_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        serde_json::from_slice(data).map_err(|_| ProtocolError::InvalidJson("token response"))
    }

    pub fn classify_refresh(&self) -> RefreshOutcome {
        match self.error.as_deref().filter(|error| !error.is_empty()) {
            Some("server_error" | "temporarily_unavailable") => RefreshOutcome::Transient,
            Some(_) => RefreshOutcome::RefreshRevoked,
            None if self
                .id_token
                .as_deref()
                .is_some_and(|value| !value.is_empty()) =>
            {
                RefreshOutcome::Success
            }
            None => RefreshOutcome::NoIdToken,
        }
    }
}
