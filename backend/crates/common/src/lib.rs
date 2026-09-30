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
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Единый формат ошибок: `{"error": "..."}` (см. docs/api/openapi.yaml).
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mut response = (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
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
