use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{MAX_AUTH_JSON_BYTES, ProtocolError};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub backend_base_url: String,
    pub notifications_paused: bool,
    pub auto_start: bool,
    pub dictation_enabled: bool,
    pub dictation_shortcut: String,
    pub dictation_toggle_shortcut: String,
    pub dictation_language: String,
    pub dictation_refinement_enabled: bool,
    pub dictation_streaming_enabled: bool,
    pub dictation_final_wait_milliseconds: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            backend_base_url:
                "https://ca-api.ambitiousdesert-517ec9ed.swedencentral.azurecontainerapps.io".into(),
            notifications_paused: false,
            auto_start: false,
            dictation_enabled: true,
            dictation_shortcut: "Ctrl+Alt+Space".into(),
            dictation_toggle_shortcut: "Ctrl+Alt+Shift+Space".into(),
            dictation_language: "auto".into(),
            dictation_refinement_enabled: false,
            dictation_streaming_enabled: true,
            dictation_final_wait_milliseconds: 1500,
            extra: BTreeMap::new(),
        }
    }
}

impl AppSettings {
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        if data.len() > MAX_AUTH_JSON_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        let settings: Self =
            serde_json::from_slice(data).map_err(|_| ProtocolError::InvalidJson("settings"))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn backend_url(&self) -> Result<Url, ProtocolError> {
        let uri = Url::parse(self.backend_base_url.trim().trim_end_matches('/'))
            .map_err(|_| ProtocolError::InvalidField("backend_base_url"))?;
        if uri.scheme() != "https"
            || uri.host_str().is_none()
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.query().is_some()
            || uri.fragment().is_some()
        {
            return Err(ProtocolError::InvalidField("backend_base_url"));
        }
        Ok(uri)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.backend_url()?;
        if !(500..=5000).contains(&self.dictation_final_wait_milliseconds)
            || self.dictation_final_wait_milliseconds % 500 != 0
        {
            return Err(ProtocolError::InvalidField(
                "dictation_final_wait_milliseconds",
            ));
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|_| ProtocolError::InvalidJson("settings"))?;
        if bytes.len() > MAX_AUTH_JSON_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_preserve_csharp_defaults_and_unknown_rollback_fields() {
        let settings =
            AppSettings::parse(br#"{"notifications_paused":true,"future_option":42}"#).unwrap();
        assert!(settings.notifications_paused);
        assert!(settings.dictation_enabled);
        assert!(settings.dictation_streaming_enabled);
        assert!(!settings.dictation_refinement_enabled);
        assert_eq!(settings.dictation_final_wait_milliseconds, 1500);
        let roundtrip = AppSettings::parse(&settings.encode().unwrap()).unwrap();
        assert_eq!(roundtrip.extra["future_option"], 42);
    }

    #[test]
    fn bad_settings_are_errors_instead_of_silent_production_defaults() {
        for data in [
            br#"{"backend_base_url":"http://127.0.0.1"}"#.as_slice(),
            br#"{"backend_base_url":""}"#,
            br#"{"dictation_final_wait_milliseconds":501}"#,
            br#"{"dictation_final_wait_milliseconds":-1}"#,
            b"corrupt",
            b"null",
        ] {
            assert!(AppSettings::parse(data).is_err());
        }
    }
}
