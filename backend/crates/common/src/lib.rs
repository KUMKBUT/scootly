//! Общие типы, ошибки и утилиты для всех сервисов Scootly.

pub mod auth;
pub mod telemetry;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use thiserror::Error;

/// Доменные ошибки всех сервисов.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// 409 с машинным кодом из openapi (`scooter_unavailable`,
    /// `reservation_active_exists`, ...) — для UI Mini App.
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Conflict { .. } => StatusCode::CONFLICT,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Машинный код ошибки (openapi `Error.code`).
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::Validation(_) => "validation_error",
            AppError::Unauthorized(_) => "unauthorized",
            AppError::Conflict { code, .. } => code,
            AppError::Internal(_) => "internal",
        }
    }
}

/// Единый формат ошибок: `{"code": "...", "message": "..."}` (docs/api/openapi.yaml).
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mut response = (
            status,
            Json(serde_json::json!({
                "code": self.code(),
                "message": self.to_string(),
            })),
        )
            .into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                "Bearer".parse().unwrap(),
            );
        }
        response
    }
}

pub type AppResult<T> = Result<T, AppError>;
