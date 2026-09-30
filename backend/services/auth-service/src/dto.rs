//! DTO auth-service — контракт docs/api/openapi.yaml (`AuthResponse`, `User`).

use serde::{Deserialize, Serialize};

use common::auth::TokenPair;
use db::users::User;

#[derive(Debug, Deserialize)]
pub struct TelegramLoginRequest {
    pub init_data: String,
    /// Опционально, если запрошен contact-токен.
    pub phone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub user: User,
}

impl AuthResponse {
    pub fn new(pair: TokenPair, user: User) -> Self {
        Self {
            access_token: pair.access_token,
            refresh_token: pair.refresh_token,
            expires_in: pair.expires_in,
            user,
        }
    }
}
