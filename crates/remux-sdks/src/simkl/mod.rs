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

fn deserialize_opt_string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Deserialize;
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    match opt {
        Some(serde_json::Value::String(s)) => {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(s))
            }
        }
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        _ => Ok(None),
    }
}

fn deserialize_opt_i64_flexible<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Deserialize;
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    match opt {
        Some(serde_json::Value::Number(n)) => Ok(n.as_i64()),
        Some(serde_json::Value::String(s)) => Ok(s.parse::<i64>().ok()),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklIds {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_opt_i64_flexible")]
    pub simkl: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_opt_string_or_number")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_opt_string_or_number")]
    pub tmdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_opt_string_or_number")]
    pub tvdb: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklMovie {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default)]
    pub ids: SimklIds,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklShow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default)]
    pub ids: SimklIds,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklEpisode {
    #[serde(default)]
    pub season: i64,
    #[serde(default, alias = "episode")]
    pub number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<SimklIds>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SimklPlaybackItem {
    pub id: Option<i64>,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub paused_at: Option<String>,
    #[serde(default)]
    pub watched_at: Option<String>,
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    #[serde(default)]
    pub movie: Option<SimklMovie>,
    #[serde(default)]
    pub show: Option<SimklShow>,
    #[serde(default)]
    pub anime: Option<SimklShow>,
    #[serde(default)]
    pub episode: Option<SimklEpisode>,
}

impl SimklPlaybackItem {
    pub fn timestamp(&self) -> Option<&str> {
        self.paused_at.as_deref().or(self.watched_at.as_deref())
    }
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

#[derive(Debug, Clone, Default)]
pub struct GetPlaybackEndpoint {
    pub hide_watched: bool,
}

impl Endpoint for GetPlaybackEndpoint {
    type Output = Vec<SimklPlaybackItem>;

    fn path(&self) -> String {
        format!("sync/playback?hide_watched={}", self.hide_watched)
    }

    fn method(&self) -> Method {
        Method::GET
    }
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

    #[test]
    fn test_deserialize_playback_items_flexible_ids() {
        let json = r#"[
            {
                "id": 101,
                "progress": 45.5,
                "paused_at": "2026-09-20T10:30:00Z",
                "type": "movie",
                "movie": {
                    "title": "Gladiator II",
                    "year": 2024,
                    "ids": {
                        "simkl": 558449,
                        "imdb": "tt2104996",
                        "tmdb": 558449
                    }
                }
            },
            {
                "id": 102,
                "progress": 62.0,
                "watched_at": "2026-09-20T11:00:00Z",
                "type": "episode",
                "show": {
                    "title": "Stranger Things",
                    "year": 2016,
                    "ids": {
                        "simkl": "39687",
                        "imdb": "tt4574334",
                        "tvdb": 305288
                    }
                },
                "episode": {
                    "season": 2,
                    "episode": 5
                }
            }
        ]"#;

        let items: Vec<SimklPlaybackItem> = serde_json::from_str(json).unwrap();
        assert_eq!(items.len(), 2);

        let m = &items[0];
        assert_eq!(m.progress, 45.5);
        assert_eq!(m.timestamp(), Some("2026-09-20T10:30:00Z"));
        assert_eq!(m.item_type.as_deref(), Some("movie"));
        let movie = m.movie.as_ref().unwrap();
        assert_eq!(movie.title.as_deref(), Some("Gladiator II"));
        assert_eq!(movie.ids.imdb.as_deref(), Some("tt2104996"));
        assert_eq!(movie.ids.tmdb.as_deref(), Some("558449"));

        let ep = &items[1];
        assert_eq!(ep.progress, 62.0);
        assert_eq!(ep.timestamp(), Some("2026-09-20T11:00:00Z"));
        assert_eq!(ep.item_type.as_deref(), Some("episode"));
        let show = ep.show.as_ref().unwrap();
        assert_eq!(show.ids.simkl, Some(39687));
        assert_eq!(show.ids.imdb.as_deref(), Some("tt4574334"));
        assert_eq!(show.ids.tvdb.as_deref(), Some("305288"));
        let episode = ep.episode.as_ref().unwrap();
        assert_eq!(episode.season, 2);
        assert_eq!(episode.number, 5);
    }
}

