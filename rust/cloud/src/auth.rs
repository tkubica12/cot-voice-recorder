use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::problem::Problem;

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;

#[derive(Clone)]
pub struct User {
    pub email: String,
    pub subject: String,
    pub audience: String,
}

#[derive(Deserialize)]
struct Claims {
    exp: u64,
    iat: u64,
    iss: String,
    aud: serde_json::Value,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<serde_json::Value>,
    #[serde(default)]
    sub: String,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    n: String,
    e: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
}
#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

struct Cache {
    keys: HashMap<String, DecodingKey>,
    expires: tokio::time::Instant,
    fetched: Option<tokio::time::Instant>,
}

pub struct GoogleVerifier {
    config: Arc<Config>,
    client: reqwest::Client,
    cache: Mutex<Cache>,
}

impl GoogleVerifier {
    pub fn new(config: Arc<Config>) -> Result<Self, Problem> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        Ok(Self {
            config,
            client,
            cache: Mutex::new(Cache {
                keys: HashMap::new(),
                expires: tokio::time::Instant::now(),
                fetched: None,
            }),
        })
    }

    async fn key(&self, kid: &str) -> Result<DecodingKey, Problem> {
        let mut cache = self.cache.lock().await;
        if cache.expires > tokio::time::Instant::now() {
            if let Some(key) = cache.keys.get(kid) {
                return Ok(key.clone());
            }
        }
        // Unknown-kid attacks cannot turn every token into a Google/JWKS request.
        if cache
            .fetched
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return Err(Problem::unauthorized());
        }
        cache.fetched = Some(tokio::time::Instant::now());
        let mut response = self
            .client
            .get(self.config.jwks.clone())
            .send()
            .await
            .map_err(|_| Problem::unauthorized())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > 1024 * 1024)
        {
            return Err(Problem::unauthorized());
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Problem::unauthorized())?
        {
            if chunk.len() > 1024 * 1024 - data.len() {
                return Err(Problem::unauthorized());
            }
            data.extend_from_slice(&chunk);
        }
        let jwks: Jwks = serde_json::from_slice(&data).map_err(|_| Problem::unauthorized())?;
        if jwks.keys.is_empty() || jwks.keys.len() > 100 {
            return Err(Problem::unauthorized());
        }
        let mut keys = HashMap::new();
        for key in jwks.keys {
            if key.kty != "RSA"
                || key.kid.is_empty()
                || key.kid.len() > 128
                || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
                || key.usage.as_deref().is_some_and(|usage| usage != "sig")
            {
                continue;
            }
            let decoded = DecodingKey::from_rsa_components(&key.n, &key.e)
                .map_err(|_| Problem::unauthorized())?;
            if keys.insert(key.kid, decoded).is_some() {
                return Err(Problem::unauthorized());
            }
        }
        cache.keys = keys;
        cache.expires = tokio::time::Instant::now() + Duration::from_secs(300);
        cache
            .keys
            .get(kid)
            .cloned()
            .ok_or_else(Problem::unauthorized)
    }

    pub async fn verify(&self, token: &str) -> Result<User, Problem> {
        if token.is_empty() || token.len() > 64 * 1024 {
            return Err(Problem::unauthorized());
        }
        let header = decode_header(token).map_err(|_| Problem::unauthorized())?;
        if header.alg != Algorithm::RS256 {
            return Err(Problem::unauthorized());
        }
        let kid = header
            .kid
            .filter(|kid| !kid.is_empty() && kid.len() <= 128)
            .ok_or_else(Problem::unauthorized)?;
        let key = self.key(&kid).await?;
        self.verify_key(token, &key)
    }

    fn verify_key(&self, token: &str, key: &DecodingKey) -> Result<User, Problem> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.required_spec_claims =
            HashSet::from(["exp", "iat", "aud", "iss"].map(str::to_owned));
        validation.set_audience(&self.config.audiences);
        validation.set_issuer(&self.config.issuers);
        let claims = decode::<Claims>(token, key, &validation)
            .map_err(|_| Problem::unauthorized())?
            .claims;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if now < 0
            || claims.exp <= now as u64
            || claims.iat > now as u64
            || !self.config.issuers.contains(&claims.iss)
        {
            return Err(Problem::unauthorized());
        }
        let email = claims
            .email
            .filter(|email| !email.is_empty())
            .ok_or_else(Problem::unauthorized)?;
        if claims.email_verified != Some(serde_json::Value::Bool(true))
            || email.trim().to_lowercase() != self.config.email
        {
            return Err(Problem::forbidden());
        }
        let audience = match claims.aud {
            serde_json::Value::String(value) => value,
            serde_json::Value::Array(values) => values
                .into_iter()
                .find_map(|value| {
                    value
                        .as_str()
                        .filter(|value| self.config.audiences.iter().any(|aud| aud == value))
                        .map(str::to_owned)
                })
                .ok_or_else(Problem::unauthorized)?,
            _ => return Err(Problem::unauthorized()),
        };
        Ok(User {
            email,
            subject: claims.sub,
            audience,
        })
    }
}
