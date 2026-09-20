use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use axum_anyhow::ApiResult as Result;
use http::StatusCode;
use remux_macros::{get, post};
use remux_sdks::remux::{SimklGlobalConfigDto, SimklTestResultDto, SimklUserConfigDto};
use uuid::Uuid;

use crate::{
    AppState, IntoApiError,
    db::{self, auth},
    services::SimklService,
};

const MASKED_TOKEN: &str = "••••••••";

fn require_self_or_admin(target_id: Uuid, session: &auth::AuthSession) -> Result<()> {
    if target_id != session.user.id && !session.user.is_admin {
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
    let cfg = db::Settings::get_simkl_config(&state.ctx.db, &state.ctx.config.simkl).await?;
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
    let mut cfg = db::Settings::get_simkl_config(&state.ctx.db, &state.ctx.config.simkl).await?;
    cfg.client_id = payload.client_id;
    if payload.completion_threshold > 0 {
        cfg.completion_threshold = payload.completion_threshold;
    }
    db::Settings::set_simkl_config(&state.ctx.db, &cfg).await?;
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
    let user_cfg =
        db::Settings::get_user_simkl_config(&state.ctx.db, &state.ctx.config.simkl, &user_id)
            .await?;
    let dto = SimklUserConfigDto {
        enabled: user_cfg.enabled,
        user_token: mask_token(&user_cfg.user_token),
        has_token: !user_cfg.user_token.is_empty(),
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
    let existing =
        db::Settings::get_user_simkl_config(&state.ctx.db, &state.ctx.config.simkl, &user_id)
            .await?;

    let new_token = if payload.user_token.is_empty() || payload.user_token == MASKED_TOKEN {
        existing.user_token
    } else {
        payload.user_token
    };

    let user_config = crate::SimklUserConfig {
        enabled: payload.enabled,
        user_token: new_token,
    };

    db::Settings::set_user_simkl_config(
        &state.ctx.db,
        &state.ctx.config.simkl,
        &user_id,
        user_config,
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/users/:user_id/simkl/test: Test user token against Simkl API
#[post("/api/users/{user_id}/simkl/test")]
pub async fn test_user_simkl_connection(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
    payload: Option<Json<SimklUserConfigDto>>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    let global_cfg =
        db::Settings::get_simkl_config(&state.ctx.db, &state.ctx.config.simkl).await?;
    if global_cfg.client_id.is_empty() {
        return Ok(Json(SimklTestResultDto {
            success: false,
            message: "Simkl Client ID is not configured in Server Settings".to_string(),
        }));
    }

    let existing =
        db::Settings::get_user_simkl_config(&state.ctx.db, &state.ctx.config.simkl, &user_id)
            .await?;

    let token = match payload {
        Some(Json(p)) if !p.user_token.is_empty() && p.user_token != MASKED_TOKEN => p.user_token,
        _ => existing.user_token,
    };

    if token.is_empty() {
        return Ok(Json(SimklTestResultDto {
            success: false,
            message: "No Simkl access token provided".to_string(),
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

