//! JWT-аутентификация (ADR-0007): выпуск пары access/refresh в auth-service,
//! локальная проверка подписи и `exp` в каждом сервисе без сетевого хопа.
//!
//! Секрет — только из env (`JWT_SECRET`), в код не зашивается.

use axum::extract::FromRef;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppError, AppResult};

/// TTL access-токена: 15 минут.
pub const ACCESS_TTL_SECS: i64 = 15 * 60;
/// TTL refresh-токена: 30 дней.
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Access,
    Refresh,
}

impl TokenType {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenType::Access => "access",
            TokenType::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// user id (UUID).
    pub sub: Uuid,
    /// telegram id.
    pub tid: i64,
    /// тип токена: `access` | `refresh`.
    pub typ: String,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

fn build_claims(user_id: Uuid, telegram_id: i64, typ: TokenType, now: i64, ttl: i64) -> Claims {
    Claims {
        sub: user_id,
        tid: telegram_id,
        typ: typ.as_str().to_owned(),
        iat: now,
        exp: now + ttl,
    }
}

fn sign(secret: &str, claims: &Claims) -> AppResult<String> {
    encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(e.into()))
}

/// Выпускает пару access + refresh для пользователя.
pub fn issue_pair(secret: &str, user_id: Uuid, telegram_id: i64) -> AppResult<TokenPair> {
    let now = chrono::Utc::now().timestamp();
    Ok(TokenPair {
        access_token: sign(
            secret,
            &build_claims(
                user_id,
                telegram_id,
                TokenType::Access,
                now,
                ACCESS_TTL_SECS,
            ),
        )?,
        refresh_token: sign(
            secret,
            &build_claims(
                user_id,
                telegram_id,
                TokenType::Refresh,
                now,
                REFRESH_TTL_SECS,
            ),
        )?,
        expires_in: ACCESS_TTL_SECS,
    })
}

/// Проверяет подпись, `exp` и тип токена. Ошибки -> 401.
pub fn decode_token(secret: &str, token: &str, expected: TokenType) -> AppResult<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized("invalid or expired token".into()))?;

    let claims = data.claims;
    if claims.typ != expected.as_str() {
        return Err(AppError::Unauthorized("wrong token type".into()));
    }
    Ok(claims)
}

/// Секрет JWT как часть состояния роутера (достаётся через `FromRef`).
#[derive(Debug, Clone)]
pub struct JwtState(pub std::sync::Arc<String>);

impl std::ops::Deref for JwtState {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// Экстрактор текущего пользователя: `Authorization: Bearer <access_jwt>`.
/// Валидация локальная (ADR-0007), кладёт [`Claims`] в расширения запроса.
#[derive(Debug, Clone)]
pub struct AuthUser(pub Claims);

#[axum::async_trait]
impl<S> axum::extract::FromRequestParts<S> for AuthUser
where
    JwtState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let jwt = JwtState::from_ref(state);

        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;

        Ok(AuthUser(decode_token(&jwt, token, TokenType::Access)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";

    #[test]
    fn pair_roundtrip() {
        let user_id = Uuid::new_v4();
        let pair = issue_pair(SECRET, user_id, 42).unwrap();

        let access = decode_token(SECRET, &pair.access_token, TokenType::Access).unwrap();
        assert_eq!(access.sub, user_id);
        assert_eq!(access.tid, 42);
        assert_eq!(access.typ, "access");
        assert_eq!(pair.expires_in, ACCESS_TTL_SECS);

        let refresh = decode_token(SECRET, &pair.refresh_token, TokenType::Refresh).unwrap();
        assert_eq!(refresh.sub, user_id);
        assert!(refresh.exp - refresh.iat == REFRESH_TTL_SECS);
    }

    #[test]
    fn wrong_type_rejected() {
        let pair = issue_pair(SECRET, Uuid::new_v4(), 1).unwrap();
        assert!(decode_token(SECRET, &pair.refresh_token, TokenType::Access).is_err());
        assert!(decode_token(SECRET, &pair.access_token, TokenType::Refresh).is_err());
    }

    #[test]
    fn wrong_secret_rejected() {
        let pair = issue_pair(SECRET, Uuid::new_v4(), 1).unwrap();
        assert!(decode_token("other-secret", &pair.access_token, TokenType::Access).is_err());
    }

    #[test]
    fn garbage_rejected() {
        assert!(decode_token(SECRET, "not.a.jwt", TokenType::Access).is_err());
    }
}
