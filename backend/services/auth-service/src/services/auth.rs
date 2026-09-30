//! Бизнес-логика auth-service: вход через Telegram и ротация JWT-пары.

use common::auth::{self, TokenType};
use common::AppResult;
use db::users;

use crate::dto::{AuthResponse, RefreshRequest, TelegramLoginRequest};
use crate::services::telegram;
use crate::AppState;

#[tracing::instrument(skip_all)]
pub async fn login_telegram(
    state: &AppState,
    req: &TelegramLoginRequest,
) -> AppResult<AuthResponse> {
    let data = telegram::verify(&req.init_data, &state.bot_token)?;
    let user =
        users::upsert_by_telegram_id(&state.pool, data.user.id, req.phone.as_deref()).await?;
    let pair = auth::issue_pair(&state.jwt, user.id, user.telegram_id)?;
    tracing::info!(user_id = %user.id, telegram_id = user.telegram_id, "telegram login");
    Ok(AuthResponse::new(pair, user))
}

#[tracing::instrument(skip_all)]
pub async fn refresh(state: &AppState, req: &RefreshRequest) -> AppResult<AuthResponse> {
    let claims = auth::decode_token(&state.jwt, &req.refresh_token, TokenType::Refresh)?;
    let user = users::find_by_id(&state.pool, claims.sub).await?;
    let pair = auth::issue_pair(&state.jwt, user.id, user.telegram_id)?;
    tracing::info!(user_id = %user.id, "token refreshed");
    Ok(AuthResponse::new(pair, user))
}
