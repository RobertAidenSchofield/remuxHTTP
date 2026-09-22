use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Datelike;
use remux_sdks::remux::SimklPollResultDto;
use remux_sdks::simkl::{
    ScrobblePayload, SimklDeviceCodeResponse, SimklEpisode, SimklIds, SimklMovie,
    SimklPlaybackItem, SimklShow, SimklTokenErrorResponse, SimklTokenResponse,
    SimklUserSettings,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    AppContext,
    common::{TickUnit, ToRunTimeTicks},
    db,
};

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("remux-server/1.0")
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

const SIMKL_API_BASE: &str = "https://api.simkl.com";

static LAST_PLAYBACK_SYNC: LazyLock<Mutex<HashMap<Uuid, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const PLAYBACK_SYNC_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct PendingDeviceAuth {
    pub device_code: Option<String>,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: Instant,
    pub interval: Duration,
    pub last_polled_at: Option<Instant>,
}

static PENDING_DEVICE_AUTHS: LazyLock<Mutex<HashMap<Uuid, PendingDeviceAuth>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct SimklService;

pub enum ResolvedMedia {
    Movie {
        media: db::Media,
        movie: SimklMovie,
    },
    Episode {
        media: db::Media,
        show: SimklShow,
        episode: SimklEpisode,
    },
}

impl ResolvedMedia {
    pub fn media(&self) -> &db::Media {
        match self {
            Self::Movie { media, .. } => media,
            Self::Episode { media, .. } => media,
        }
    }

    pub fn to_payload(&self, progress: f64) -> ScrobblePayload {
        match self {
            Self::Movie { movie, .. } => ScrobblePayload {
                movie: Some(movie.clone()),
                show: None,
                episode: None,
                progress: Some(progress),
            },
            Self::Episode { show, episode, .. } => ScrobblePayload {
                movie: None,
                show: Some(show.clone()),
                episode: Some(episode.clone()),
                progress: Some(progress),
            },
        }
    }
}

impl SimklService {
    /// Calculate playback progress percentage based on ticks:
    /// `Progress = min(100.0, max(0.0, (PositionTicks / RunTimeTicks) * 100.0))`
    pub fn calculate_progress(position_ticks: i64, run_time_ticks: i64) -> f64 {
        if run_time_ticks <= 0 || position_ticks <= 0 {
            return 0.0;
        }
        ((position_ticks as f64 / run_time_ticks as f64) * 100.0).clamp(0.0, 100.0)
    }

    /// Resolve `db::Media` by ID into Simkl identifiers.
    ///
    /// - Movies: Require `imdb` or `tmdb`.
    /// - Episodes: Require series `imdb`, `tvdb`, or `tmdb`, plus episode `season` and `number`.
    pub async fn resolve_media(
        ctx: &AppContext,
        item_id: &Uuid,
    ) -> Result<Option<ResolvedMedia>> {
        let Some(media) = db::Media::get_by_id(&ctx.db, item_id).await? else {
            return Ok(None);
        };

        match media.kind {
            db::MediaKind::Movie => {
                let imdb = media
                    .external_ids
                    .imdb
                    .as_ref()
                    .map(|s| s.to_string());
                let tmdb = media
                    .external_ids
                    .tmdb
                    .map(|id| id.to_string());

                if imdb.is_none() && tmdb.is_none() {
                    debug!(
                        title = %media.title,
                        "Simkl: skipping movie without IMDb or TMDB ID"
                    );
                    return Ok(None);
                }

                let movie = SimklMovie {
                    title: Some(
                        media
                            .title
                            .clone(),
                    ),
                    year: media
                        .released_at
                        .map(|d| d.year()),
                    ids: SimklIds {
                        imdb,
                        tmdb,
                        ..Default::default()
                    },
                };

                Ok(Some(ResolvedMedia::Movie { media, movie }))
            }
            db::MediaKind::Episode => {
                let episode_idx = match media.idx {
                    Some(idx) => idx,
                    None => {
                        debug!(
                            title = %media.title,
                            "Simkl: skipping episode without episode index"
                        );
                        return Ok(None);
                    }
                };

                let season_idx = media
                    .parent_idx
                    .unwrap_or(1);

                // Find series ancestor
                let ancestors = db::Media::get_ancestors(&ctx.db, &media.id)
                    .await
                    .unwrap_or_default();
                let series = if let Some(series) = ancestors
                    .into_iter()
                    .find(|m| m.kind == db::MediaKind::Series)
                {
                    Some(series)
                } else if let Some(gid) = media.grandparent_id {
                    db::Media::get_by_id(&ctx.db, &gid)
                        .await
                        .ok()
                        .flatten()
                        .filter(|m| m.kind == db::MediaKind::Series)
                } else {
                    None
                };

                let Some(series) = series else {
                    debug!(
                        title = %media.title,
                        "Simkl: skipping episode without parent series"
                    );
                    return Ok(None);
                };

                let imdb = series
                    .external_ids
                    .imdb
                    .as_ref()
                    .map(|s| s.to_string());
                let tvdb = series
                    .external_ids
                    .tvdb
                    .map(|id| id.to_string());
                let tmdb = series
                    .external_ids
                    .tmdb
                    .map(|id| id.to_string());

                if imdb.is_none() && tvdb.is_none() && tmdb.is_none() {
                    debug!(
                        series_title = %series.title,
                        "Simkl: skipping episode series without IMDb, TVDB, or TMDB ID"
                    );
                    return Ok(None);
                }

                let show = SimklShow {
                    title: Some(
                        series
                            .title
                            .clone(),
                    ),
                    year: series
                        .released_at
                        .map(|d| d.year()),
                    ids: SimklIds {
                        imdb,
                        tvdb,
                        tmdb,
                        ..Default::default()
                    },
                };

                let episode = SimklEpisode {
                    season: season_idx,
                    number: episode_idx,
                    ids: None,
                };

                Ok(Some(ResolvedMedia::Episode {
                    media,
                    show,
                    episode,
                }))
            }
            _ => {
                debug!(kind = ?media.kind, "Simkl: skipping unsupported media kind");
                Ok(None)
            }
        }
    }

    /// Trigger playback start scrobble asynchronously.
    pub fn on_start(
        ctx: AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
    ) {
        tokio::spawn(async move {
            if let Err(e) = Self::handle_start(
                &ctx,
                user_id,
                item_id,
                position_ticks,
                run_time_ticks,
            )
            .await
            {
                warn!("[Simkl] Scrobble error on start: {e}");
            }
        });
    }

    /// Trigger playback pause scrobble asynchronously.
    pub fn on_pause(
        ctx: AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
    ) {
        tokio::spawn(async move {
            if let Err(e) = Self::handle_pause(
                &ctx,
                user_id,
                item_id,
                position_ticks,
                run_time_ticks,
            )
            .await
            {
                warn!("[Simkl] Scrobble error on pause: {e}");
            }
        });
    }

    /// Trigger playback stop scrobble asynchronously.
    pub fn on_stop(
        ctx: AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
        played: bool,
    ) {
        tokio::spawn(async move {
            if let Err(e) = Self::handle_stop(
                &ctx,
                user_id,
                item_id,
                position_ticks,
                run_time_ticks,
                played,
            )
            .await
            {
                warn!("[Simkl] Scrobble error on stop: {e}");
            }
        });
    }

    async fn handle_start(
        ctx: &AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
    ) -> Result<()> {
        let (client_id, user_cfg) = Self::get_credentials(ctx, &user_id).await?;
        if client_id.is_empty() {
            debug!(%user_id, "[Simkl] Scrobble start skipped: Simkl Client ID is not configured");
            return Ok(());
        }
        if !user_cfg.enabled {
            debug!(%user_id, "[Simkl] Scrobble start skipped: Simkl scrobbling disabled for user");
            return Ok(());
        }
        if user_cfg
            .user_token
            .is_empty()
        {
            debug!(%user_id, "[Simkl] Scrobble start skipped: no user access token");
            return Ok(());
        }

        let Some(resolved) = Self::resolve_media(ctx, &item_id).await? else {
            return Ok(());
        };

        let runtime = run_time_ticks
            .or_else(|| {
                resolved
                    .media()
                    .runtime
                    .map(|s| s * 10_000_000)
            })
            .unwrap_or(0);
        let pos = position_ticks.unwrap_or(0);
        let progress = Self::calculate_progress(pos, runtime);

        let payload = resolved.to_payload(progress);
        Self::send_scrobble_request("start", &payload, &client_id, &user_cfg.user_token)
            .await
    }

    async fn handle_pause(
        ctx: &AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
    ) -> Result<()> {
        let (client_id, user_cfg) = Self::get_credentials(ctx, &user_id).await?;
        if client_id.is_empty() {
            debug!(%user_id, "[Simkl] Scrobble pause skipped: Simkl Client ID is not configured");
            return Ok(());
        }
        if !user_cfg.enabled {
            debug!(%user_id, "[Simkl] Scrobble pause skipped: Simkl scrobbling disabled for user");
            return Ok(());
        }
        if user_cfg
            .user_token
            .is_empty()
        {
            debug!(%user_id, "[Simkl] Scrobble pause skipped: no user access token");
            return Ok(());
        }

        let Some(resolved) = Self::resolve_media(ctx, &item_id).await? else {
            return Ok(());
        };

        let runtime = run_time_ticks
            .or_else(|| {
                resolved
                    .media()
                    .runtime
                    .map(|s| s * 10_000_000)
            })
            .unwrap_or(0);
        let pos = position_ticks.unwrap_or(0);
        let progress = Self::calculate_progress(pos, runtime);

        let payload = resolved.to_payload(progress);
        Self::send_scrobble_request("pause", &payload, &client_id, &user_cfg.user_token)
            .await
    }

    async fn handle_stop(
        ctx: &AppContext,
        user_id: Uuid,
        item_id: Uuid,
        position_ticks: Option<i64>,
        run_time_ticks: Option<i64>,
        played: bool,
    ) -> Result<()> {
        let (client_id, user_cfg) = Self::get_credentials(ctx, &user_id).await?;
        if client_id.is_empty() {
            debug!(%user_id, "[Simkl] Scrobble stop skipped: Simkl Client ID is not configured");
            return Ok(());
        }
        if !user_cfg.enabled {
            debug!(%user_id, "[Simkl] Scrobble stop skipped: Simkl scrobbling disabled for user");
            return Ok(());
        }
        if user_cfg
            .user_token
            .is_empty()
        {
            debug!(%user_id, "[Simkl] Scrobble stop skipped: no user access token");
            return Ok(());
        }

        let Some(resolved) = Self::resolve_media(ctx, &item_id).await? else {
            return Ok(());
        };

        let runtime = run_time_ticks
            .or_else(|| {
                resolved
                    .media()
                    .runtime
                    .map(|s| s * 10_000_000)
            })
            .unwrap_or(0);
        let pos = position_ticks.unwrap_or(0);
        let mut progress = Self::calculate_progress(pos, runtime);

        let simkl_cfg = db::Settings::get_simkl_config(
            &ctx.db,
            &ctx.config
                .simkl,
        )
        .await?;
        let threshold = simkl_cfg
            .completion_threshold
            .max(1) as f64;

        if played || progress >= threshold {
            // Register as completed watch
            progress = progress.max(threshold);
        }

        let payload = resolved.to_payload(progress);
        Self::send_scrobble_request("stop", &payload, &client_id, &user_cfg.user_token)
            .await
    }

    async fn get_credentials(
        ctx: &AppContext,
        user_id: &Uuid,
    ) -> Result<(String, crate::SimklUserConfig)> {
        let simkl_cfg = db::Settings::get_simkl_config(
            &ctx.db,
            &ctx.config
                .simkl,
        )
        .await?;
        let user_cfg = db::Settings::get_user_simkl_config(
            &ctx.db,
            &ctx.config
                .simkl,
            user_id,
        )
        .await?;
        Ok((simkl_cfg.client_id, user_cfg))
    }

    async fn send_scrobble_request(
        action: &str,
        payload: &ScrobblePayload,
        client_id: &str,
        user_token: &str,
    ) -> Result<()> {
        let client_id = client_id
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        let user_token = user_token
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        let url = format!(
            "{SIMKL_API_BASE}/scrobble/{action}?client_id={client_id}&app-name=remux&app-version=1.0"
        );
        info!(
            action,
            client_id_prefix = &client_id[..client_id
                .len()
                .min(4)],
            "[Simkl] Sending scrobble request"
        );
        let resp = HTTP_CLIENT
            .post(&url)
            .header("simkl-api-key", client_id)
            .header("Authorization", format!("Bearer {user_token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(payload)
            .send()
            .await
            .context("failed to send request to Simkl API")?;

        let status = resp.status();
        // 200/201 is success, 409 is soft-success (already scrobbled recently)
        if status.is_success() || status.as_u16() == 409 {
            info!(
                action,
                status = status.as_u16(),
                "[Simkl] Scrobble successful"
            );
            Ok(())
        } else {
            let body = resp
                .text()
                .await
                .unwrap_or_default();
            warn!(action, status = status.as_u16(), body = %body, "[Simkl] Scrobble failed");
            anyhow::bail!("Simkl scrobble {action} returned HTTP {status}: {body}");
        }
    }

    /// Test connection with Simkl API using `GET https://api.simkl.com/users/settings`.
    pub async fn test_connection(client_id: &str, user_token: &str) -> Result<String> {
        let client_id = client_id
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        let user_token = user_token
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        let url = format!(
            "{SIMKL_API_BASE}/users/settings?client_id={client_id}&app-name=remux&app-version=1.0"
        );
        info!(
            client_id_prefix = &client_id[..client_id
                .len()
                .min(4)],
            "[Simkl] Testing connection to Simkl API"
        );
        let resp = HTTP_CLIENT
            .get(&url)
            .header("simkl-api-key", client_id)
            .header("Authorization", format!("Bearer {user_token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to reach Simkl API")?;

        let status = resp.status();
        if status.is_success() {
            let settings: SimklUserSettings = resp
                .json()
                .await
                .context("Failed to parse Simkl user settings")?;
            let username = settings
                .user
                .and_then(|u| u.name)
                .unwrap_or_else(|| "User".to_string());
            info!(username = %username, "[Simkl] Connection test successful");
            Ok(format!("Connected successfully as {username}"))
        } else if status.as_u16() == 401 {
            warn!(
                "[Simkl] Connection test failed: Unauthorized (invalid access token)"
            );
            anyhow::bail!("Unauthorized: Invalid Simkl access token")
        } else {
            let body = resp
                .text()
                .await
                .unwrap_or_default();
            warn!(status = status.as_u16(), body = %body, "[Simkl] Connection test failed with error status");
            anyhow::bail!("Simkl API returned HTTP {status}: {body}")
        }
    }

    /// Request a new device PIN or OAuth 2.0 device code for a user.
    /// Prefers OAuth 2.0 Device Flow (V2: POST /oauth2/device) and falls back to legacy V1 (GET /oauth/pin).
    pub async fn start_device_auth(
        client_id: &str,
        user_id: Uuid,
        redirect_uri: Option<&str>,
    ) -> Result<SimklDeviceCodeResponse> {
        let client_id = client_id
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        if client_id.is_empty() {
            anyhow::bail!(
                "Simkl Client ID is empty. Please configure your Client ID in Settings > Simkl."
            );
        }

        info!(
            %user_id,
            client_id_prefix = &client_id[..client_id.len().min(4)],
            redirect = ?redirect_uri,
            "[Simkl] Requesting device authorization from Simkl"
        );

        // 1. Try OAuth 2.0 Device Flow (V2) first (required for all modern Simkl apps)
        let form_params = [
            ("client_id", client_id),
            ("scope", "media:read media:write"),
        ];

        let v2_resp = HTTP_CLIENT
            .post(format!("{SIMKL_API_BASE}/oauth2/device"))
            .header("User-Agent", "remux-server/1.0")
            .header("Accept", "application/json")
            .form(&form_params)
            .send()
            .await
            .context("Failed to connect to Simkl API")?;

        let v2_status = v2_resp.status();
        let device_resp: SimklDeviceCodeResponse = if v2_status.is_success() {
            v2_resp
                .json()
                .await
                .context("Failed to parse Simkl V2 device code response")?
        } else {
            let v2_body_text = v2_resp
                .text()
                .await
                .unwrap_or_default();
            let is_v1_fallback = v2_status.as_u16() == 401
                && (v2_body_text.contains("not enabled for OAuth 2.0")
                    || v2_body_text.contains("invalid_client"));

            if is_v1_fallback {
                info!(%user_id, "[Simkl] Client is not an OAuth 2.0 app; falling back to V1 PIN flow");
                let mut url =
                    format!("{SIMKL_API_BASE}/oauth/pin?client_id={client_id}");
                if let Some(redirect) = redirect_uri.filter(|r| {
                    !r.trim()
                        .is_empty()
                }) {
                    url.push_str(&format!(
                        "&redirect={}",
                        urlencoding::encode(redirect.trim())
                    ));
                }
                let resp = HTTP_CLIENT
                    .get(&url)
                    .header("simkl-api-key", client_id)
                    .header("Accept", "application/json")
                    .send()
                    .await
                    .context("Failed to connect to Simkl API")?;

                let status = resp.status();
                if !status.is_success() {
                    let body = resp
                        .text()
                        .await
                        .unwrap_or_default();
                    warn!(status = status.as_u16(), body = %body, "[Simkl] V1 Device PIN request failed");
                    if let Ok(err_json) =
                        serde_json::from_str::<serde_json::Value>(&body)
                    {
                        if let Some(msg) = err_json
                            .get("message")
                            .and_then(|m| m.as_str())
                        {
                            anyhow::bail!("Simkl API: {msg}");
                        }
                    }
                    anyhow::bail!(
                        "Simkl device authorization failed ({status}): {body}"
                    );
                }

                resp.json()
                    .await
                    .context("Failed to parse Simkl device code response")?
            } else {
                warn!(status = v2_status.as_u16(), body = %v2_body_text, "[Simkl] OAuth 2.0 device authorization failed");
                if let Ok(err_json) =
                    serde_json::from_str::<serde_json::Value>(&v2_body_text)
                {
                    if let Some(desc) = err_json
                        .get("error_description")
                        .and_then(|d| d.as_str())
                    {
                        anyhow::bail!("Simkl API: {desc}");
                    }
                    if let Some(msg) = err_json
                        .get("message")
                        .and_then(|m| m.as_str())
                    {
                        anyhow::bail!("Simkl API: {msg}");
                    }
                }
                anyhow::bail!(
                    "Simkl device authorization failed ({v2_status}): {v2_body_text}"
                );
            }
        };

        let mut device_resp = device_resp;
        if device_resp
            .verification_uri
            .is_empty()
        {
            device_resp.verification_uri = "https://simkl.com/pin".to_string();
        }
        if device_resp
            .verification_uri_complete
            .is_none()
            && !device_resp
                .user_code
                .is_empty()
        {
            device_resp.verification_uri_complete = Some(format!(
                "{}?user_code={}",
                device_resp.verification_uri, device_resp.user_code
            ));
        }

        let expires_at = Instant::now()
            + Duration::from_secs(
                device_resp
                    .expires_in
                    .max(30),
            );
        let interval = Duration::from_secs(
            device_resp
                .interval
                .max(5),
        );

        PENDING_DEVICE_AUTHS
            .lock()
            .unwrap()
            .insert(
                user_id,
                PendingDeviceAuth {
                    device_code: device_resp
                        .device_code
                        .clone(),
                    user_code: device_resp
                        .user_code
                        .clone(),
                    verification_uri: device_resp
                        .verification_uri
                        .clone(),
                    verification_uri_complete: device_resp
                        .verification_uri_complete
                        .clone(),
                    expires_at,
                    interval,
                    last_polled_at: None,
                },
            );

        info!(
            %user_id,
            user_code = %device_resp.user_code,
            is_v2 = device_resp.device_code.is_some(),
            verification_uri = %device_resp.verification_uri,
            "[Simkl] Device authorization initialized successfully"
        );
        Ok(device_resp)
    }

    /// Poll for token approval from Simkl API via OAuth 2.0 V2 (`POST /oauth2/token`) or V1 (`GET /oauth/pin/{user_code}`).
    pub async fn poll_device_auth(
        ctx: &AppContext,
        client_id: &str,
        user_id: Uuid,
    ) -> Result<SimklPollResultDto> {
        let client_id = client_id
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        let pending = {
            let auths = PENDING_DEVICE_AUTHS
                .lock()
                .unwrap();
            auths
                .get(&user_id)
                .cloned()
        };

        let Some(mut pending) = pending else {
            return Ok(SimklPollResultDto {
                status: "expired".to_string(),
                message: Some(
                    "No active login session found. Please start again.".to_string(),
                ),
            });
        };

        if Instant::now() >= pending.expires_at {
            PENDING_DEVICE_AUTHS
                .lock()
                .unwrap()
                .remove(&user_id);
            return Ok(SimklPollResultDto {
                status: "expired".to_string(),
                message: Some(
                    "Authorization code expired. Please start again.".to_string(),
                ),
            });
        }

        if let Some(last) = pending.last_polled_at {
            if last.elapsed() < pending.interval {
                return Ok(SimklPollResultDto {
                    status: "pending".to_string(),
                    message: None,
                });
            }
        }

        pending.last_polled_at = Some(Instant::now());
        if let Some(p) = PENDING_DEVICE_AUTHS
            .lock()
            .unwrap()
            .get_mut(&user_id)
        {
            p.last_polled_at = pending.last_polled_at;
        }

        if let Some(ref device_code) = pending.device_code {
            // OAuth 2.0 Device Flow (V2)
            let form = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code.as_str()),
            ];

            let resp = HTTP_CLIENT
                .post(format!("{SIMKL_API_BASE}/oauth2/token"))
                .header("User-Agent", "remux-server/1.0")
                .header("Accept", "application/json")
                .form(&form)
                .send()
                .await
                .context("Failed to poll Simkl token endpoint")?;

            let status = resp.status();
            if status.is_success() {
                let token_resp: remux_sdks::simkl::SimklTokenResponse = resp
                    .json()
                    .await
                    .context("Failed to parse Simkl token response")?;

                if !token_resp
                    .access_token
                    .is_empty()
                {
                    let mut user_cfg = db::Settings::get_user_simkl_config(
                        &ctx.db,
                        &ctx.config
                            .simkl,
                        &user_id,
                    )
                    .await
                    .unwrap_or_default();
                    user_cfg.enabled = true;
                    user_cfg.user_token = token_resp.access_token;
                    db::Settings::set_user_simkl_config(
                        &ctx.db,
                        &ctx.config
                            .simkl,
                        &user_id,
                        user_cfg,
                    )
                    .await?;

                    PENDING_DEVICE_AUTHS
                        .lock()
                        .unwrap()
                        .remove(&user_id);
                    info!(%user_id, "[Simkl] OAuth 2.0 device authorization approved by user");
                    return Ok(SimklPollResultDto {
                        status: "success".to_string(),
                        message: Some("Connected successfully to Simkl!".to_string()),
                    });
                }
            } else if status.as_u16() == 400 {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_default();
                if let Ok(err) = serde_json::from_str::<
                    remux_sdks::simkl::SimklTokenErrorResponse,
                >(&body)
                {
                    match err
                        .error
                        .as_str()
                    {
                        "authorization_pending" => {
                            return Ok(SimklPollResultDto {
                                status: "pending".to_string(),
                                message: None,
                            });
                        }
                        "slow_down" => {
                            if let Some(p) = PENDING_DEVICE_AUTHS
                                .lock()
                                .unwrap()
                                .get_mut(&user_id)
                            {
                                p.interval += Duration::from_secs(5);
                            }
                            return Ok(SimklPollResultDto {
                                status: "pending".to_string(),
                                message: None,
                            });
                        }
                        "expired_token" => {
                            PENDING_DEVICE_AUTHS
                                .lock()
                                .unwrap()
                                .remove(&user_id);
                            return Ok(SimklPollResultDto {
                                status: "expired".to_string(),
                                message: Some(
                                    "Authorization code expired. Please start again."
                                        .to_string(),
                                ),
                            });
                        }
                        other => {
                            PENDING_DEVICE_AUTHS
                                .lock()
                                .unwrap()
                                .remove(&user_id);
                            let desc = err
                                .error_description
                                .unwrap_or_else(|| other.to_string());
                            return Ok(SimklPollResultDto {
                                status: "error".to_string(),
                                message: Some(desc),
                            });
                        }
                    }
                } else {
                    PENDING_DEVICE_AUTHS
                        .lock()
                        .unwrap()
                        .remove(&user_id);
                    return Ok(SimklPollResultDto {
                        status: "error".to_string(),
                        message: Some(format!("Simkl API error (400): {body}")),
                    });
                }
            } else {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_default();
                PENDING_DEVICE_AUTHS
                    .lock()
                    .unwrap()
                    .remove(&user_id);
                warn!(%user_id, status = status.as_u16(), body = %body, "[Simkl] OAuth 2.0 token polling returned error");
                return Ok(SimklPollResultDto {
                    status: "error".to_string(),
                    message: Some(format!("Simkl API error ({status}): {body}")),
                });
            }
        }

        // V1 legacy PIN flow polling (when device_code is None)
        let url = format!(
            "{SIMKL_API_BASE}/oauth/pin/{}?client_id={client_id}",
            pending.user_code
        );

        let resp = HTTP_CLIENT
            .get(&url)
            .header("simkl-api-key", client_id)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to poll Simkl PIN endpoint")?;

        let status = resp.status();
        if status.is_success() {
            let poll_resp: remux_sdks::simkl::SimklPinPollResponse = resp
                .json()
                .await
                .context("Failed to parse Simkl PIN poll response")?;

            if poll_resp
                .result
                .eq_ignore_ascii_case("OK")
            {
                if let Some(token) = poll_resp
                    .access_token
                    .filter(|t| !t.is_empty())
                {
                    let mut user_cfg = db::Settings::get_user_simkl_config(
                        &ctx.db,
                        &ctx.config
                            .simkl,
                        &user_id,
                    )
                    .await
                    .unwrap_or_default();
                    user_cfg.enabled = true;
                    user_cfg.user_token = token;
                    db::Settings::set_user_simkl_config(
                        &ctx.db,
                        &ctx.config
                            .simkl,
                        &user_id,
                        user_cfg,
                    )
                    .await?;

                    PENDING_DEVICE_AUTHS
                        .lock()
                        .unwrap()
                        .remove(&user_id);
                    info!(%user_id, "[Simkl] PIN authorization approved by user");
                    return Ok(SimklPollResultDto {
                        status: "success".to_string(),
                        message: Some("Connected successfully to Simkl!".to_string()),
                    });
                }
            }

            let msg = poll_resp
                .message
                .unwrap_or_default();
            if msg
                .to_lowercase()
                .contains("pending")
            {
                return Ok(SimklPollResultDto {
                    status: "pending".to_string(),
                    message: None,
                });
            } else if msg
                .to_lowercase()
                .contains("slow")
            {
                if let Some(p) = PENDING_DEVICE_AUTHS
                    .lock()
                    .unwrap()
                    .get_mut(&user_id)
                {
                    p.interval += Duration::from_secs(5);
                }
                return Ok(SimklPollResultDto {
                    status: "pending".to_string(),
                    message: None,
                });
            } else {
                PENDING_DEVICE_AUTHS
                    .lock()
                    .unwrap()
                    .remove(&user_id);
                warn!(%user_id, message = %msg, "[Simkl] PIN authorization ended with non-pending status");
                return Ok(SimklPollResultDto {
                    status: "expired".to_string(),
                    message: Some(if msg.is_empty() {
                        "Authorization code expired or invalid.".to_string()
                    } else {
                        msg
                    }),
                });
            }
        } else if status.as_u16() == 400 || status.as_u16() == 404 {
            let body = resp
                .text()
                .await
                .unwrap_or_default();
            PENDING_DEVICE_AUTHS
                .lock()
                .unwrap()
                .remove(&user_id);
            warn!(%user_id, status = status.as_u16(), body = %body, "[Simkl] PIN polling returned client error");
            Ok(SimklPollResultDto {
                status: "expired".to_string(),
                message: Some(format!("Simkl authorization error ({status}): {body}")),
            })
        } else {
            let body = resp
                .text()
                .await
                .unwrap_or_default();
            PENDING_DEVICE_AUTHS
                .lock()
                .unwrap()
                .remove(&user_id);
            warn!(%user_id, status = status.as_u16(), body = %body, "[Simkl] PIN polling returned server error");
            Ok(SimklPollResultDto {
                status: "error".to_string(),
                message: Some(format!("Simkl API error ({status}): {body}")),
            })
        }
    }

    /// Fetch active playback sessions from Simkl API via `GET /sync/playback?hide_watched=true`.
    pub async fn get_playback(
        client_id: &str,
        user_token: &str,
    ) -> Result<Vec<SimklPlaybackItem>> {
        let client_id = client_id
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        let user_token = user_token
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        let url = format!(
            "{SIMKL_API_BASE}/sync/playback?hide_watched=true&client_id={client_id}&app-name=remux&app-version=1.0"
        );
        info!(
            client_id_prefix = &client_id[..client_id
                .len()
                .min(4)],
            "[Simkl] Fetching playback from Simkl"
        );
        let resp = HTTP_CLIENT
            .get(&url)
            .header("simkl-api-key", client_id)
            .header("Authorization", format!("Bearer {user_token}"))
            .header("Accept", "application/json")
            .send()
            .await
            .context("failed to send request to Simkl Playback API")?;

        let status = resp.status();
        if status.is_success() {
            let items: Vec<SimklPlaybackItem> = resp
                .json()
                .await
                .context("failed to parse Simkl playback items")?;
            Ok(items)
        } else {
            let body = resp
                .text()
                .await
                .unwrap_or_default();
            anyhow::bail!("Simkl GET /sync/playback returned HTTP {status}: {body}");
        }
    }

    /// Pull paused/in-progress playback sessions from Simkl and sync them to Remux's local user media state.
    pub async fn sync_playback(ctx: &AppContext, user_id: Uuid) -> Result<()> {
        let Ok((client_id, user_cfg)) = Self::get_credentials(ctx, &user_id).await
        else {
            return Ok(());
        };

        if client_id.is_empty() {
            debug!(%user_id, "[Simkl] Continue Watching sync skipped: Simkl Client ID is not configured");
            return Ok(());
        }
        if !user_cfg.enabled {
            debug!(%user_id, "[Simkl] Continue Watching sync skipped: Simkl scrobbling disabled for user");
            return Ok(());
        }
        if !user_cfg.sync_continue_watching {
            debug!(%user_id, "[Simkl] Continue Watching sync skipped: continue watching sync disabled for user");
            return Ok(());
        }
        if user_cfg
            .user_token
            .is_empty()
        {
            debug!(%user_id, "[Simkl] Continue Watching sync skipped: no user access token");
            return Ok(());
        }

        // Check rate-limit cooldown
        {
            let mut sync_times = LAST_PLAYBACK_SYNC
                .lock()
                .unwrap();
            if let Some(last) = sync_times.get(&user_id) {
                if last.elapsed() < PLAYBACK_SYNC_COOLDOWN {
                    return Ok(());
                }
            }
            sync_times.insert(user_id, Instant::now());
        }

        let user = match db::User::get_by_id(&ctx.db, &user_id).await? {
            Some(u) => u,
            None => return Ok(()),
        };

        let items = match Self::get_playback(&client_id, &user_cfg.user_token).await {
            Ok(items) => items,
            Err(e) => {
                warn!(%user_id, error = %e, "[Simkl] Failed to fetch playback items for continue watching");
                return Err(e);
            }
        };

        if items.is_empty() {
            return Ok(());
        }

        debug!(%user_id, count = items.len(), "[Simkl] Syncing continue watching items from playback");
        for item in items {
            if let Err(e) = Self::sync_single_playback_item(ctx, &user, &item).await {
                debug!(error = %e, "[Simkl] Skipped playback item during sync");
            }
        }

        Ok(())
    }

    async fn sync_single_playback_item(
        ctx: &AppContext,
        user: &db::User,
        item: &SimklPlaybackItem,
    ) -> Result<()> {
        let is_movie = item
            .item_type
            .as_deref()
            == Some("movie")
            || (item
                .movie
                .is_some()
                && item
                    .episode
                    .is_none());

        let media = if is_movie {
            let movie = match &item.movie {
                Some(m) => m,
                None => anyhow::bail!("missing movie object in playback item"),
            };
            Self::find_or_resolve_movie(ctx, movie).await?
        } else {
            let show = match item
                .show
                .as_ref()
                .or(item
                    .anime
                    .as_ref())
            {
                Some(s) => s,
                None => anyhow::bail!("missing show object in playback item"),
            };
            let episode = match &item.episode {
                Some(ep) => ep,
                None => anyhow::bail!("missing episode object in playback item"),
            };
            Self::find_or_resolve_episode(ctx, show, episode).await?
        };

        let Some(media) = media else {
            return Ok(());
        };

        let paused_at = item
            .timestamp()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.naive_utc())
            .unwrap_or_else(|| chrono::Utc::now().naive_utc());

        let mut state = db::UserMediaState::get_or_new(&ctx.db, user, &media).await?;

        // If local state is newer, don't overwrite
        if let Some(last_local) = state.last_played_at {
            if last_local > paused_at {
                return Ok(());
            }
        }

        let runtime_ticks = media
            .runtime
            .and_then(|runtime| runtime.to_ticks(TickUnit::Seconds))
            .unwrap_or_else(|| {
                if media.kind == db::MediaKind::Movie {
                    120_i64
                        .to_ticks(TickUnit::Minutes)
                        .unwrap()
                } else {
                    45_i64
                        .to_ticks(TickUnit::Minutes)
                        .unwrap()
                }
            });

        let target_ticks = ((item
            .progress
            .clamp(0.0, 100.0)
            / 100.0)
            * runtime_ticks as f64) as i64;

        state.playback_position = target_ticks;
        state.last_played_at = Some(paused_at);
        state.played_at = None;

        state
            .save(&ctx.db)
            .await?;
        debug!(
            %media.id,
            title = %media.title,
            progress = item.progress,
            ticks = target_ticks,
            "[Simkl] Synced continue watching position from Simkl"
        );

        Ok(())
    }

    async fn find_or_resolve_movie(
        ctx: &AppContext,
        movie: &SimklMovie,
    ) -> Result<Option<db::Media>> {
        let mut ext = db::ExternalIds::default();
        if let Some(imdb) = &movie
            .ids
            .imdb
        {
            ext.imdb = db::NonEmptyString::try_new(imdb.clone()).ok();
        }
        if let Some(tmdb) = &movie
            .ids
            .tmdb
        {
            ext.tmdb = tmdb
                .parse::<i64>()
                .ok();
        }

        if let Some(id) =
            db::Media::find_by_external_ids(&ctx.db, &db::MediaKind::Movie, &ext).await
        {
            return Ok(db::Media::get_by_id(&ctx.db, &id).await?);
        }

        if let Some(imdb) = ext
            .imdb
            .as_ref()
        {
            let custom_id = imdb.to_string();
            let raw = db::MediaIdRaw {
                kind: db::MediaKind::Movie,
                external_ids: db::ExternalIds {
                    imdb: Some(imdb.clone()),
                    custom_stremio_id: Some(custom_id),
                    tmdb: ext.tmdb,
                    ..Default::default()
                },
                season: None,
                episode: None,
            };
            let synth_id = Uuid::from(&raw);
            if let Ok(Some(media)) =
                crate::services::MediaResolveService::resolve_item(synth_id, ctx).await
            {
                return Ok(Some(media));
            }
        }

        Ok(None)
    }

    async fn find_or_resolve_episode(
        ctx: &AppContext,
        show: &SimklShow,
        episode: &SimklEpisode,
    ) -> Result<Option<db::Media>> {
        let mut ext = db::ExternalIds::default();
        if let Some(imdb) = &show
            .ids
            .imdb
        {
            ext.imdb = db::NonEmptyString::try_new(imdb.clone()).ok();
        }
        if let Some(tmdb) = &show
            .ids
            .tmdb
        {
            ext.tmdb = tmdb
                .parse::<i64>()
                .ok();
        }
        if let Some(tvdb) = &show
            .ids
            .tvdb
        {
            ext.tvdb = tvdb
                .parse::<i64>()
                .ok();
        }

        let series_id = match db::Media::find_by_external_ids(
            &ctx.db,
            &db::MediaKind::Series,
            &ext,
        )
        .await
        {
            Some(id) => Some(id),
            None => {
                if let Some(imdb) = ext
                    .imdb
                    .as_ref()
                {
                    let custom_id = imdb.to_string();
                    let raw = db::MediaIdRaw {
                        kind: db::MediaKind::Series,
                        external_ids: db::ExternalIds {
                            imdb: Some(imdb.clone()),
                            custom_stremio_id: Some(custom_id),
                            tmdb: ext.tmdb,
                            tvdb: ext.tvdb,
                            ..Default::default()
                        },
                        season: None,
                        episode: None,
                    };
                    let synth_id = Uuid::from(&raw);
                    if let Ok(Some(s)) =
                        crate::services::MediaResolveService::resolve_item(
                            synth_id, ctx,
                        )
                        .await
                    {
                        Some(s.id)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        let Some(series_id) = series_id else {
            return Ok(None);
        };

        let ep_row: Option<db::Media> = sqlx::query_as(
            r#"
            SELECT * FROM media
            WHERE kind = 'Episode'
              AND (grandparent_id = ?1 OR parent_id = ?1)
              AND parent_idx = ?2
              AND idx = ?3
            LIMIT 1
            "#,
        )
        .bind(series_id)
        .bind(episode.season)
        .bind(episode.number)
        .fetch_optional(&ctx.db)
        .await?;

        Ok(ep_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_progress() {
        // Zero or negative
        assert_eq!(SimklService::calculate_progress(0, 100), 0.0);
        assert_eq!(SimklService::calculate_progress(-10, 100), 0.0);
        assert_eq!(SimklService::calculate_progress(50, 0), 0.0);

        // Normal percentages
        assert_eq!(SimklService::calculate_progress(50, 100), 50.0);
        assert_eq!(SimklService::calculate_progress(800, 1000), 80.0);

        // Overflow clamping
        assert_eq!(SimklService::calculate_progress(1200, 1000), 100.0);
    }

    #[tokio::test]
    async fn test_sync_single_playback_item_updates_state() {
        let (_s, guard) = crate::integration_test::new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;
        let user = db::User::get_by_username(&ctx.db, "test")
            .await
            .unwrap()
            .unwrap();

        let movie = crate::integration_test::seed_movie(ctx).await;

        let item = SimklPlaybackItem {
            id: Some(123),
            progress: 50.0,
            paused_at: Some("2026-09-20T12:00:00Z".to_string()),
            watched_at: None,
            item_type: Some("movie".to_string()),
            movie: Some(SimklMovie {
                title: Some("Heat".to_string()),
                year: Some(1995),
                ids: SimklIds {
                    imdb: Some("tt0113277".to_string()),
                    tmdb: Some("949".to_string()),
                    ..Default::default()
                },
            }),
            show: None,
            anime: None,
            episode: None,
        };

        SimklService::sync_single_playback_item(ctx, &user, &item)
            .await
            .unwrap();

        let state = db::UserMediaState::get_or_new(&ctx.db, &user, &movie)
            .await
            .unwrap();

        assert!(state.playback_position > 0);
        assert_eq!(state.played_at, None);
        assert!(
            state
                .last_played_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_sync_single_playback_item_preserves_newer_local() {
        let (_s, guard) = crate::integration_test::new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;
        let user = db::User::get_by_username(&ctx.db, "test")
            .await
            .unwrap()
            .unwrap();

        let movie = crate::integration_test::seed_movie(ctx).await;

        let mut local_state = db::UserMediaState::get_or_new(&ctx.db, &user, &movie)
            .await
            .unwrap();
        local_state.playback_position = 999_999;
        local_state.last_played_at = Some(
            chrono::DateTime::parse_from_rfc3339("2026-09-20T15:00:00Z")
                .unwrap()
                .naive_utc(),
        );
        local_state
            .save(&ctx.db)
            .await
            .unwrap();

        let item = SimklPlaybackItem {
            id: Some(123),
            progress: 25.0,
            paused_at: Some("2026-09-20T12:00:00Z".to_string()),
            watched_at: None,
            item_type: Some("movie".to_string()),
            movie: Some(SimklMovie {
                title: Some("Heat".to_string()),
                year: Some(1995),
                ids: SimklIds {
                    imdb: Some("tt0113277".to_string()),
                    ..Default::default()
                },
            }),
            show: None,
            anime: None,
            episode: None,
        };

        SimklService::sync_single_playback_item(ctx, &user, &item)
            .await
            .unwrap();

        let state = db::UserMediaState::get_or_new(&ctx.db, &user, &movie)
            .await
            .unwrap();

        assert_eq!(state.playback_position, 999_999);
    }
}
