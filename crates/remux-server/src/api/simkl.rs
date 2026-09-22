use axum::{
    Json,
    extract::{Path, Query, State},
    response::{Html, IntoResponse},
};
use axum_anyhow::ApiResult as Result;
use http::StatusCode;
use remux_macros::{get, post};
use remux_sdks::remux::{
    SimklDeviceAuthDto, SimklGlobalConfigDto, SimklPollResultDto, SimklTestResultDto,
    SimklUserConfigDto,
};
use uuid::Uuid;

use crate::{
    AppState, IntoApiError,
    db::{self, auth},
    services::SimklService,
};

const MASKED_TOKEN: &str = "••••••••";

fn require_self_or_admin(target_id: Uuid, session: &auth::AuthSession) -> Result<()> {
    if target_id
        != session
            .user
            .id
        && !session
            .user
            .is_admin
    {
        return Err(anyhow::anyhow!("Forbidden").context_unauthorized("forbidden"));
    }
    Ok(())
}

fn mask_token(token: &str) -> String {
    if token.is_empty() {
        String::new()
    } else {
        MASKED_TOKEN.to_string()
    }
}

/// GET /api/settings/simkl: Return global settings (client_id, completion_threshold)
#[get("/api/settings/simkl")]
pub async fn get_simkl_settings(
    State(state): State<AppState>,
    _session: auth::AdminSession,
) -> Result<impl IntoResponse> {
    let cfg = db::Settings::get_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
    )
    .await?;
    let dto = SimklGlobalConfigDto {
        client_id: cfg.client_id,
        completion_threshold: cfg.completion_threshold,
    };
    Ok(Json(dto))
}

/// POST /api/settings/simkl: Save global settings
#[post("/api/settings/simkl")]
pub async fn update_simkl_settings(
    State(state): State<AppState>,
    _session: auth::AdminSession,
    Json(payload): Json<SimklGlobalConfigDto>,
) -> Result<impl IntoResponse> {
    let mut cfg = db::Settings::get_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
    )
    .await?;
    cfg.client_id = payload
        .client_id
        .trim()
        .to_string();
    if payload.completion_threshold > 0 {
        cfg.completion_threshold = payload.completion_threshold;
    }
    db::Settings::set_simkl_config(
        &state
            .ctx
            .db,
        &cfg,
    )
    .await?;
    tracing::info!(
        has_client_id = !cfg
            .client_id
            .is_empty(),
        client_id_prefix = &cfg.client_id[..cfg
            .client_id
            .len()
            .min(4)],
        completion_threshold = cfg.completion_threshold,
        "[Simkl] Saved global Simkl settings in database"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/users/:user_id/simkl: Return user-specific settings (mask user_token)
#[get("/api/users/{user_id}/simkl")]
pub async fn get_user_simkl_settings(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let user_cfg = db::Settings::get_user_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
        &user_id,
    )
    .await?;
    let dto = SimklUserConfigDto {
        enabled: user_cfg.enabled,
        user_token: mask_token(&user_cfg.user_token),
        has_token: !user_cfg
            .user_token
            .is_empty(),
        sync_continue_watching: user_cfg.sync_continue_watching,
    };
    Ok(Json(dto))
}

/// POST /api/users/:user_id/simkl: Save user-specific enabled boolean and user_token
#[post("/api/users/{user_id}/simkl")]
pub async fn update_user_simkl_settings(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<SimklUserConfigDto>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let existing = db::Settings::get_user_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
        &user_id,
    )
    .await?;

    let new_token = if payload
        .user_token
        .is_empty()
        || payload.user_token == MASKED_TOKEN
    {
        existing.user_token
    } else {
        payload.user_token
    };

    let has_token = !new_token.is_empty();

    let user_config = crate::SimklUserConfig {
        enabled: payload.enabled,
        user_token: new_token,
        sync_continue_watching: payload.sync_continue_watching,
    };

    db::Settings::set_user_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
        &user_id,
        user_config,
    )
    .await?;

    tracing::info!(
        %user_id,
        enabled = payload.enabled,
        sync_continue_watching = payload.sync_continue_watching,
        has_token,
        "[Simkl] Updated user Simkl configuration"
    );

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/users/:user_id/simkl/test: Test user token against Simkl API
#[post("/api/users/{user_id}/simkl/test")]
pub async fn test_user_simkl_connection(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
    body: bytes::Bytes,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let global_cfg = db::Settings::get_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
    )
    .await?;
    if global_cfg
        .client_id
        .is_empty()
    {
        tracing::warn!(%user_id, "[Simkl] Connection test requested but Client ID is not configured");
        return Ok(Json(SimklTestResultDto {
            success: false,
            message: "Simkl Client ID is not configured in Server Settings".to_string(),
        }));
    }

    let existing = db::Settings::get_user_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
        &user_id,
    )
    .await?;

    let explicit_token = if !body.is_empty() {
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body) {
            val.get("user_token")
                .or_else(|| val.get("userToken"))
                .and_then(|v| v.as_str())
                .map(|s| {
                    s.trim()
                        .to_string()
                })
        } else {
            None
        }
    } else {
        None
    };

    let token = match explicit_token {
        Some(t) if !t.is_empty() && t != MASKED_TOKEN => t,
        _ => existing.user_token,
    };

    if token.is_empty() {
        return Ok(Json(SimklTestResultDto {
            success: false,
            message: "No Simkl access token provided or configured".to_string(),
        }));
    }

    match SimklService::test_connection(&global_cfg.client_id, &token).await {
        Ok(msg) => Ok(Json(SimklTestResultDto {
            success: true,
            message: msg,
        })),
        Err(e) => Ok(Json(SimklTestResultDto {
            success: false,
            message: e.to_string(),
        })),
    }
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct SimklDeviceStartQuery {
    pub redirect: Option<String>,
}

/// POST /api/users/:user_id/simkl/device/start: Start Device / PIN flow for user
#[post("/api/users/{user_id}/simkl/device/start")]
pub async fn start_user_simkl_device_auth(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
    Query(query): Query<SimklDeviceStartQuery>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let global_cfg = db::Settings::get_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
    )
    .await?;
    if global_cfg
        .client_id
        .is_empty()
    {
        tracing::warn!(%user_id, "[Simkl] Device auth requested but Client ID is not configured");
        return Err(anyhow::anyhow!(
            "Simkl Client ID is not configured in Server Settings"
        )
        .context_bad_request("missing_client_id"));
    }

    tracing::info!(
        %user_id,
        client_id_prefix = &global_cfg.client_id[..global_cfg.client_id.len().min(4)],
        redirect = ?query.redirect,
        "[Simkl] Starting PIN authorization"
    );

    let resp = SimklService::start_device_auth(
        &global_cfg.client_id,
        user_id,
        query
            .redirect
            .as_deref(),
    )
    .await
    .map_err(|e| {
        let detail = format!("{e:#}");
        e.context_bad_request(&detail)
    })?;
    let dto = SimklDeviceAuthDto {
        user_code: resp.user_code,
        verification_uri: resp.verification_uri,
        verification_uri_complete: resp.verification_uri_complete,
        expires_in: resp.expires_in,
        interval: resp.interval,
    };
    Ok(Json(dto))
}

/// POST /api/users/:user_id/simkl/device/poll: Poll Device / PIN flow for user
#[post("/api/users/{user_id}/simkl/device/poll")]
pub async fn poll_user_simkl_device_auth(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let global_cfg = db::Settings::get_simkl_config(
        &state
            .ctx
            .db,
        &state
            .ctx
            .config
            .simkl,
    )
    .await?;
    if global_cfg
        .client_id
        .is_empty()
    {
        tracing::warn!(%user_id, "[Simkl] Device poll requested but Client ID is not configured");
        return Err(anyhow::anyhow!(
            "Simkl Client ID is not configured in Server Settings"
        )
        .context_bad_request("missing_client_id"));
    }

    let res =
        SimklService::poll_device_auth(&state.ctx, &global_cfg.client_id, user_id)
            .await
            .map_err(|e| {
                let detail = format!("{e:#}");
                e.context_bad_request(&detail)
            })?;
    Ok(Json(res))
}

const SIMKL_AUTH_SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Simkl Authorization - Remux</title>
  <style>
    * { box-sizing: border-box; }
    body {
      background-color: #0f172a;
      color: #f8fafc;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      min-height: 100vh;
      margin: 0;
      padding: 24px;
    }
    .card {
      background-color: #1e293b;
      border: 1px solid #334155;
      border-radius: 16px;
      padding: 40px 32px;
      max-width: 440px;
      width: 100%;
      text-align: center;
      box-shadow: 0 20px 25px -5px rgba(0, 0, 0, 0.5), 0 8px 10px -6px rgba(0, 0, 0, 0.5);
    }
    .icon {
      font-size: 56px;
      line-height: 1;
      margin-bottom: 20px;
    }
    h1 {
      font-size: 1.5rem;
      font-weight: 700;
      margin: 0 0 12px;
      color: #f8fafc;
    }
    p {
      font-size: 0.95rem;
      line-height: 1.6;
      color: #94a3b8;
      margin: 0 0 28px;
    }
    .actions {
      display: flex;
      flex-direction: column;
      gap: 12px;
    }
    .btn {
      display: inline-block;
      background-color: #3b82f6;
      color: #ffffff;
      padding: 12px 24px;
      border-radius: 8px;
      font-weight: 600;
      text-decoration: none;
      cursor: pointer;
      border: none;
      font-size: 0.95rem;
      transition: background-color 0.15s ease-in-out;
    }
    .btn:hover {
      background-color: #2563eb;
    }
    .btn-secondary {
      background-color: transparent;
      color: #94a3b8;
      border: 1px solid #334155;
    }
    .btn-secondary:hover {
      background-color: #334155;
      color: #f8fafc;
    }
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">✅</div>
    <h1>Authorization Successful</h1>
    <p>Your Simkl account has been connected to Remux. You can safely close this window and return to your app.</p>
    <div class="actions">
      <button class="btn" onclick="window.close()">Close Window</button>
      <a class="btn btn-secondary" href="/admin">Go to Dashboard</a>
    </div>
  </div>
  <script>
    if (window.opener) {
      try {
        window.opener.postMessage({ type: 'simkl_auth_complete' }, '*');
      } catch (e) {}
    }
  </script>
</body>
</html>"#;

/// GET /auth/simkl: Callback / landing page when Simkl redirects user after PIN authorization
#[get("/auth/simkl")]
pub async fn simkl_auth_redirect() -> impl IntoResponse {
    Html(SIMKL_AUTH_SUCCESS_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_token_flexible() {
        let extract = |body: &[u8]| -> Option<String> {
            if !body.is_empty() {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) {
                    val.get("user_token")
                        .or_else(|| val.get("userToken"))
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            s.trim()
                                .to_string()
                        })
                } else {
                    None
                }
            } else {
                None
            }
        };

        assert_eq!(extract(b""), None);
        assert_eq!(extract(b"{}"), None);
        assert_eq!(
            extract(br#"{"user_token":"tok123"}"#),
            Some("tok123".into())
        );
        assert_eq!(extract(br#"{"userToken":"tok456"}"#), Some("tok456".into()));
        // Ensure duplicate fields from older clients do not error
        assert_eq!(
            extract(br#"{"userToken":"tok789","user_token":"tok789"}"#),
            Some("tok789".into())
        );
    }
}
