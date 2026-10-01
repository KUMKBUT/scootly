//! DTO HTTP API payment-service — схемы `Payment` / `PaymentMethod` /
//! `YookassaNotification` из docs/api/openapi.yaml.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// openapi `Payment`: чек и история платежей.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentDto {
    pub id: Uuid,
    pub ride_id: Option<Uuid>,
    /// `hold | captured | canceled | refunded`.
    pub status: String,
    pub amount_kopeks: i32,
    pub created_at: DateTime<Utc>,
}

impl From<db::payments::Payment> for PaymentDto {
    fn from(p: db::payments::Payment) -> Self {
        Self {
            id: p.id,
            ride_id: p.rental_id,
            status: p.status,
            amount_kopeks: p.amount_kopeks,
            created_at: p.created_at,
        }
    }
}

/// openapi `PaymentMethod` (MVP — максимум одна карта).
#[derive(Debug, Clone, Serialize)]
pub struct PaymentMethodDto {
    pub id: String,
    pub card_last4: String,
    pub card_network: String,
}

/// openapi listPayments: фильтр по поездке + лимит страницы.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub ride_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// openapi `YookassaNotification`: вебхук эквайринга.
/// Подлинность проверяется повторным запросом в YooKassa (services::payments).
/// Поле `type` игнорируем (не несёт логики), `event` и `object` — рабочие.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookNotification {
    pub event: String,
    pub object: WebhookObject,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookObject {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub paid: Option<bool>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
