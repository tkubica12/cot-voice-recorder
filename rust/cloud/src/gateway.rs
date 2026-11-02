use crate::config::Config;
use crate::identity::ManagedIdentity;
use crate::models;
use crate::problem::Problem;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;

pub struct WebPubSub {
    config: Arc<Config>,
    identity: Arc<ManagedIdentity>,
    http: reqwest::Client,
}

impl WebPubSub {
    pub fn new(config: Arc<Config>, identity: Arc<ManagedIdentity>) -> Result<Self, Problem> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        Ok(Self {
            config,
            identity,
            http,
        })
    }
    fn url(&self, segments: &[&str]) -> Result<url::Url, Problem> {
        let mut url = self.config.webpubsub.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| Problem::validation("Invalid Web PubSub endpoint."))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }
    async fn bounded(mut response: reqwest::Response) -> Result<Vec<u8>, Problem> {
        if !response.status().is_success() {
            return Err(Problem::unavailable("Web PubSub rejected the request."));
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Problem::unavailable("Web PubSub response was interrupted."))?
        {
            if chunk.len() > 1024 * 1024 - data.len() {
                return Err(Problem::unavailable("Web PubSub response exceeded bounds."));
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }
    pub async fn negotiate(&self) -> Result<models::Negotiate, Problem> {
        let mut url = self.url(&["api", "hubs", &self.config.hub, ":generateToken"])?;
        url.query_pairs_mut()
            .append_pair("userId", &self.config.group)
            .append_pair(
                "role",
                &format!("webpubsub.joinLeaveGroup.{}", self.config.group),
            )
            .append_pair(
                "minutesToExpire",
                &self.config.webpubsub_ttl_minutes.to_string(),
            )
            .append_pair("api-version", "2024-12-01")
            .append_pair("group", &self.config.group)
            .append_pair("clientType", "Default");
        let token = self.identity.token("https://webpubsub.azure.com/").await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .header("Accept", "application/json, text/json")
            .send()
            .await
            .map_err(|_| Problem::unavailable("Web PubSub negotiation failed."))?;
        #[derive(Deserialize)]
        struct Access {
            token: String,
        }
        let access: Access = serde_json::from_slice(&Self::bounded(response).await?)
            .map_err(|_| Problem::unavailable("Invalid Web PubSub negotiation payload."))?;
        if access.token.is_empty() {
            return Err(Problem::unavailable("Web PubSub returned no client token."));
        }
        let mut client = self.url(&["client", "hubs", &self.config.hub])?;
        client.set_scheme("wss").map_err(|_| Problem::internal())?;
        client
            .query_pairs_mut()
            .append_pair("access_token", &access.token);
        Ok(models::Negotiate {
            url: client.into(),
            hub: self.config.hub.clone(),
            group: self.config.group.clone(),
            expires_at: crate::services::now()
                + time::Duration::minutes(i64::from(self.config.webpubsub_ttl_minutes)),
        })
    }
    pub async fn notify(&self, transcript: &crate::domain::Transcript) -> Result<(), Problem> {
        if transcript.source == "windows_dictation" {
            return Err(Problem::validation(
                "Audit entries must not emit automatic completion notifications.",
            ));
        }
        let mut url = self.url(&[
            "api",
            "hubs",
            &self.config.hub,
            "users",
            &self.config.group,
            ":send",
        ])?;
        url.query_pairs_mut()
            .append_pair("api-version", "2024-12-01");
        let event = serde_json::json!({
            "event":"transcript.completed","event_id":crate::pipeline_text::event_id(&transcript.recording_id),
            "transcript_id":transcript.transcript_id,"recording_id":transcript.recording_id,
            "completed_at":crate::services::python_datetime(transcript.completed_at, true),
            "preview":transcript.preview,
        });
        let token = self.identity.token("https://webpubsub.azure.com/").await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .header("Content-Type", "application/json")
            .json(&event)
            .send()
            .await
            .map_err(|_| Problem::unavailable("Web PubSub notification failed."))?;
        Self::bounded(response).await?;
        Ok(())
    }
}
