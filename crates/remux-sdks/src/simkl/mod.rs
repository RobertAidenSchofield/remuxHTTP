use crate::{Auth, Body, Endpoint, Method};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct SimklAuth {
    pub client_id: String,
    pub user_token: Option<String>,
}

impl Auth for SimklAuth {
    fn apply(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req = req
            .header("simkl-api-key", &self.client_id)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(ref token) = self.user_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklIds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simkl: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklMovie {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    pub ids: SimklIds,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklShow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    pub ids: SimklIds,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklEpisode {
    pub season: i64,
    pub number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<SimklIds>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ScrobblePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub movie: Option<SimklMovie>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<SimklShow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<SimklEpisode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrobbleResponse {
    pub result: Option<String>,
    pub action: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScrobbleStartEndpoint {
    pub payload: ScrobblePayload,
}

impl Endpoint for ScrobbleStartEndpoint {
    type Output = ScrobbleResponse;

    fn path(&self) -> String {
        "scrobble/start".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.payload).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct ScrobblePauseEndpoint {
    pub payload: ScrobblePayload,
}

impl Endpoint for ScrobblePauseEndpoint {
    type Output = ScrobbleResponse;

    fn path(&self) -> String {
        "scrobble/pause".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.payload).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct ScrobbleStopEndpoint {
    pub payload: ScrobblePayload,
}

impl Endpoint for ScrobbleStopEndpoint {
    type Output = ScrobbleResponse;

    fn path(&self) -> String {
        "scrobble/stop".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.payload).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklUserSettings {
    pub user: Option<SimklUserProfile>,
    pub account: Option<SimklUserAccount>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklUserProfile {
    pub name: Option<String>,
    pub joined_at: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklUserAccount {
    pub id: Option<i64>,
    pub timezone: Option<String>,
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UserSettingsEndpoint;

impl Endpoint for UserSettingsEndpoint {
    type Output = SimklUserSettings;

    fn path(&self) -> String {
        "users/settings".to_string()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklDeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SimklTokenErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_movie_scrobble_payload_serialization() {
        let payload = ScrobblePayload {
            movie: Some(SimklMovie {
                title: Some("Inception".to_string()),
                year: Some(2010),
                ids: SimklIds {
                    imdb: Some("tt1375666".to_string()),
                    tmdb: Some("27205".to_string()),
                    ..Default::default()
                },
            }),
            show: None,
            episode: None,
            progress: Some(12.5),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"title\":\"Inception\""));
        assert!(json.contains("\"imdb\":\"tt1375666\""));
        assert!(json.contains("\"progress\":12.5"));
        assert!(!json.contains("\"show\""));
    }

    #[test]
    fn test_episode_scrobble_payload_serialization() {
        let payload = ScrobblePayload {
            movie: None,
            show: Some(SimklShow {
                title: Some("Stranger Things".to_string()),
                year: Some(2016),
                ids: SimklIds {
                    imdb: Some("tt4574334".to_string()),
                    ..Default::default()
                },
            }),
            episode: Some(SimklEpisode {
                season: 1,
                number: 3,
                ids: None,
            }),
            progress: Some(42.0),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"season\":1"));
        assert!(json.contains("\"number\":3"));
        assert!(json.contains("\"imdb\":\"tt4574334\""));
        assert!(!json.contains("\"movie\""));
    }
}

