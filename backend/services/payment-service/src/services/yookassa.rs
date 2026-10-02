//! Шлюз YooKassa (ADR-0003, MVP #11): payment-service — единственная точка
//! интеграции с эквайрингом. Варианты [`YooKassa`]: `Http` (боевые/staging-ключи
//! из env, настоящий REST API `/v3`), `Emulated` (платежи «на столе», ключи не
//! нужны), `Failing` (всегда отказ — тесты retry-очереди и 402-контракта).
//!
//! Деньги — только в копейках (`INT`): строка `value` API («490.00»)
//! конвертируется целочисленно, без float. Идемпотентность — заголовок
//! `Idempotency-Key` (`hold:{rental_id}` / `ride:{rental_id}`, ADR-0014).
//!
//! ADR-0014: при недоступности шлюза capture/void не роняют запрос юзера —
//! неудача уходит в retry-очередь джоба сверки.

use db::payments;
use uuid::Uuid;

/// Статус платежа в YooKassa (GET /v3/payments/{id}).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YkStatus {
    /// Создан, подтверждения не требует/ждёт (в холде — редкий транзитный).
    Pending,
    /// Холд заморожен, ждёт capture — норма для активной поездки.
    WaitingForCapture,
    /// Списано.
    Succeeded,
    /// Отменён (истёк холд / отклонён).
    Canceled,
    /// Статус неизвестен (эмуляция / незнакомое значение API).
    #[default]
    Unknown,
}

impl YkStatus {
    fn from_api(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "waiting_for_capture" => Self::WaitingForCapture,
            "succeeded" => Self::Succeeded,
            "canceled" => Self::Canceled,
            _ => Self::Unknown,
        }
    }
}

/// Платёж в терминах YooKassa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YkPayment {
    pub yookassa_id: String,
    pub status: YkStatus,
    /// Сумма платежа у эквайринга в копейках (0 — не разобрали).
    pub amount_kopeks: i32,
    /// `confirmation.confirmation_url` — нужен только при привязке карты.
    pub confirmation_url: Option<String>,
}

/// Точка интеграции с эквайрингом.
pub trait YooKassaGateway: Send + Sync {
    /// Холд: замораживает `amount_kopeks`, возвращает платёж.
    /// Ключ идемпотентности — `hold:{rental_id}` (ADR-0014, в БД не хранится).
    fn create_hold(
        &self,
        idempotency_key: &str,
        amount_kopeks: i32,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;

    /// Capture холда на финальную сумму (частичный capture допустим).
    fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;

    /// Снятие холда (unlock-fail, отмена).
    fn cancel_hold(
        &self,
        yookassa_id: &str,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;

    /// Повторный запрос состояния платежа: вебхуки и сверка доверяют только
    /// данным YooKassa, а не входящему телу (openapi paymentWebhook, ADR-0003).
    fn get(
        &self,
        yookassa_id: &str,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;
}

// ── Деньги: копейки ↔ строка API, строго целочисленно ──────────────────────

/// Копейки → строка `amount.value` API: `4950` → `"49.50"`.
pub fn format_amount(amount_kopeks: i32) -> String {
    format!("{}.{:02}", amount_kopeks / 100, (amount_kopeks % 100).abs())
}

/// Строка `amount.value` API → копейки: `"49.50"` → `4950`. Без float.
pub fn parse_amount(value: &str) -> i32 {
    let (rubles, kopecks) = match value.split_once('.') {
        Some((r, k)) => (r, k),
        None => (value, ""),
    };
    let rubles: i32 = rubles.trim().parse().unwrap_or(0);
    // "5" → 50 копеек, "50" → 50, длиннее двух знаков — режем (API шлёт 2).
    let digits: String = kopecks
        .chars()
        .filter(char::is_ascii_digit)
        .take(2)
        .collect();
    let kopecks: i32 = match digits.len() {
        1 => digits.parse::<i32>().unwrap_or(0) * 10,
        _ => digits.parse::<i32>().unwrap_or(0),
    };
    rubles * 100 + kopecks
}

// ── HTTP-шлюз (MVP #11): staging/боевые ключи ──────────────────────────────

/// Базовый URL API YooKassa; переопределяется env `YOOKASSA_API_URL` (тесты).
pub const DEFAULT_API_URL: &str = "https://api.yookassa.ru/v3";
/// Таймаут запроса к эквайрингу (ADR-0014: деградация не должна держать юзера).
const HTTP_TIMEOUT_SECS: u64 = 10;

/// Настоящий REST API YooKassa (`/v3`): Basic-auth `shop_id:secret_key`,
/// `Idempotency-Key` на мутирующих запросах.
#[derive(Clone)]
pub struct HttpYooKassa {
    http: reqwest::Client,
    base_url: String,
    credentials: reqwest::header::HeaderValue,
}

impl std::fmt::Debug for HttpYooKassa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Секрет не печатаем никогда: только факт наличия шлюза.
        f.debug_struct("HttpYooKassa")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl HttpYooKassa {
    pub fn new(base_url: &str, shop_id: &str, secret_key: &str) -> common::AppResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| common::AppError::Internal(e.into()))?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            credentials: reqwest::header::HeaderValue::from_str(&format!(
                "Basic {}",
                basic_auth_credentials(shop_id, secret_key)
            ))
            .map_err(|e| common::AppError::Internal(e.into()))?,
        })
    }

    /// Из env: `YOOKASSA_SHOP_ID` + `YOOKASSA_SECRET_KEY` (секреты — только env,
    /// ADR-0014), опционально `YOOKASSA_API_URL`.
    pub fn from_env(shop_id: &str, secret_key: &str) -> common::AppResult<Self> {
        let base_url =
            std::env::var("YOOKASSA_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_owned());
        Self::new(&base_url, shop_id, secret_key)
    }

    /// Все ответы /v3/payments имеют общие поля `id`, `status`, `amount`.
    async fn read_payment(&self, response: reqwest::Response) -> common::AppResult<YkPayment> {
        let status = response.status();
        let body: serde_json::Value =
            response
                .json()
                .await
                .map_err(|error| common::AppError::Upstream {
                    code: "yookassa_bad_response",
                    message: format!("payment api returned unparseable body: {error}"),
                })?;
        if !status.is_success() {
            return Err(common::AppError::Upstream {
                code: "yookassa_http",
                message: format!(
                    "payment api returned {status}: {}",
                    body.get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("no description")
                ),
            });
        }
        Ok(YkPayment {
            yookassa_id: body
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            status: body
                .get("status")
                .and_then(|v| v.as_str())
                .map(YkStatus::from_api)
                .unwrap_or_default(),
            amount_kopeks: body
                .get("amount")
                .and_then(|a| a.get("value"))
                .and_then(|v| v.as_str())
                .map(parse_amount)
                .unwrap_or(0),
            confirmation_url: body
                .pointer("/confirmation/confirmation_url")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    }

    fn post(&self, path: &str, idempotency_key: Option<&str>) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(format!("{}{path}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, self.credentials.clone());
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        request
    }

    fn get_request(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, self.credentials.clone())
    }
}

impl YooKassaGateway for HttpYooKassa {
    /// `POST /v3/payments` с `capture: false` — холд (ADR-0003).
    async fn create_hold(
        &self,
        idempotency_key: &str,
        amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        let response = self
            .post("/payments", Some(idempotency_key))
            .json(&serde_json::json!({
                "amount": { "value": format_amount(amount_kopeks), "currency": "RUB" },
                "capture": false,
                "description": format!("Scootly: {idempotency_key}"),
            }))
            .send()
            .await
            .map_err(|error| common::AppError::Upstream {
                code: "yookassa_unreachable",
                message: format!("create hold failed: {error}"),
            })?;
        self.read_payment(response).await
    }

    /// `POST /v3/payments/{id}/capture` на финальную сумму.
    async fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> common::AppResult<YkPayment> {
        let response = self
            .post(
                &format!("/payments/{yookassa_id}/capture"),
                Some(idempotency_key),
            )
            .json(&serde_json::json!({
                "amount": { "value": format_amount(amount_kopeks), "currency": "RUB" },
            }))
            .send()
            .await
            .map_err(|error| common::AppError::Upstream {
                code: "yookassa_unreachable",
                message: format!("capture failed: {error}"),
            })?;
        self.read_payment(response).await
    }

    /// `POST /v3/payments/{id}/cancel` — снятие холда.
    async fn cancel_hold(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        let response = self
            .post(&format!("/payments/{yookassa_id}/cancel"), None)
            .send()
            .await
            .map_err(|error| common::AppError::Upstream {
                code: "yookassa_unreachable",
                message: format!("cancel failed: {error}"),
            })?;
        self.read_payment(response).await
    }

    /// `GET /v3/payments/{id}` — источник истины для вебхуков и сверки.
    async fn get(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        let response = self
            .get_request(&format!("/payments/{yookassa_id}"))
            .send()
            .await
            .map_err(|error| common::AppError::Upstream {
                code: "yookassa_unreachable",
                message: format!("get payment failed: {error}"),
            })?;
        self.read_payment(response).await
    }
}

/// `shop_id:secret_key` в base64 (RFC 7617), без внешних крейтов.
fn basic_auth_credentials(shop_id: &str, secret_key: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{shop_id}:{secret_key}").into_bytes();
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, byte) in [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63]
            .into_iter()
            .enumerate()
        {
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(ALPHABET[byte as usize] as char);
            }
        }
    }
    out
}

// ── Эмуляция и отказ (локальный стенд / тесты) ─────────────────────────────

/// Эмуляция: платёж доходит всегда, id выводится детерминированно из ключа,
/// поэтому повторный холд даёт тот же `yookassa_id` (UNIQUE в БД не спорит).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmulatedYooKassa;

fn emulated_id(idempotency_key: &str) -> String {
    // Формат id YooKassa — 22–36 символов; детерминизм важнее похожести.
    let rental = idempotency_key.rsplit(':').next().unwrap_or("x");
    match Uuid::parse_str(rental) {
        Ok(id) => format!("emul-{}", id.simple()),
        Err(_) => format!("emul-{}", Uuid::new_v4().simple()),
    }
}

impl YooKassaGateway for EmulatedYooKassa {
    async fn create_hold(
        &self,
        idempotency_key: &str,
        _amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        let yookassa_id = emulated_id(idempotency_key);
        tracing::debug!(%idempotency_key, "emulated hold created");
        Ok(YkPayment {
            confirmation_url: Some(format!(
                "https://yoomoney.ru/checkout/payments/v2/contract?yk_id={yookassa_id}"
            )),
            yookassa_id,
            status: YkStatus::WaitingForCapture,
            amount_kopeks: 0,
        })
    }

    async fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> common::AppResult<YkPayment> {
        tracing::debug!(%yookassa_id, amount_kopeks, %idempotency_key, "emulated capture ok");
        Ok(YkPayment {
            yookassa_id: yookassa_id.to_owned(),
            status: YkStatus::Succeeded,
            amount_kopeks,
            confirmation_url: None,
        })
    }

    async fn cancel_hold(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        tracing::debug!(%yookassa_id, "emulated hold canceled");
        Ok(YkPayment {
            yookassa_id: yookassa_id.to_owned(),
            status: YkStatus::Canceled,
            amount_kopeks: 0,
            confirmation_url: None,
        })
    }

    /// Статус не отслеживается — вебхуки в эмуляции верифицируются полем `event`.
    async fn get(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        Ok(YkPayment {
            yookassa_id: yookassa_id.to_owned(),
            status: YkStatus::Unknown,
            amount_kopeks: 0,
            confirmation_url: None,
        })
    }
}

/// Всегда отказывающий шлюз — для тестов retry-очереди и 402-контракта.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingYooKassa;

fn upstream() -> common::AppError {
    common::AppError::Upstream {
        code: "capture_failed",
        message: "acquiring is unavailable".into(),
    }
}

impl YooKassaGateway for FailingYooKassa {
    async fn create_hold(
        &self,
        _idempotency_key: &str,
        _amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        Err(upstream())
    }

    async fn capture(
        &self,
        _yookassa_id: &str,
        _amount_kopeks: i32,
        _idempotency_key: &str,
    ) -> common::AppResult<YkPayment> {
        Err(upstream())
    }

    async fn cancel_hold(&self, _yookassa_id: &str) -> common::AppResult<YkPayment> {
        Err(upstream())
    }

    async fn get(&self, _yookassa_id: &str) -> common::AppResult<YkPayment> {
        Err(upstream())
    }
}

/// Выбранный шлюз: `Http` — staging/боевые ключи, `Emulated` — локальный стенд
/// (MVP #11: режим определяется наличием `YOOKASSA_SHOP_ID`/`YOOKASSA_SECRET_KEY`).
#[derive(Debug, Clone, Default)]
pub enum YooKassa {
    #[default]
    Emulated,
    Failing,
    Http(HttpYooKassa),
}

impl YooKassa {
    /// Из env: есть непустые ключи — настоящий API, нет — эмуляция
    /// (как `Locks::from_env` у замков, MVP #10).
    pub fn from_env() -> Self {
        let shop = std::env::var("YOOKASSA_SHOP_ID").unwrap_or_default();
        let secret = std::env::var("YOOKASSA_SECRET_KEY").unwrap_or_default();
        if shop.is_empty() || secret.is_empty() {
            tracing::info!("YOOKASSA_SHOP_ID/SECRET_KEY not set, using emulated acquiring");
            return Self::Emulated;
        }
        match HttpYooKassa::from_env(&shop, &secret) {
            Ok(http) => {
                tracing::info!("YooKassa http gateway enabled (real acquiring keys)");
                Self::Http(http)
            }
            Err(error) => {
                tracing::error!(%error, "bad YooKassa config, falling back to emulated");
                Self::Emulated
            }
        }
    }
}

impl YooKassaGateway for YooKassa {
    async fn create_hold(
        &self,
        idempotency_key: &str,
        amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => {
                EmulatedYooKassa
                    .create_hold(idempotency_key, amount_kopeks)
                    .await
            }
            Self::Failing => {
                FailingYooKassa
                    .create_hold(idempotency_key, amount_kopeks)
                    .await
            }
            Self::Http(http) => http.create_hold(idempotency_key, amount_kopeks).await,
        }
    }

    async fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => {
                EmulatedYooKassa
                    .capture(yookassa_id, amount_kopeks, idempotency_key)
                    .await
            }
            Self::Failing => {
                FailingYooKassa
                    .capture(yookassa_id, amount_kopeks, idempotency_key)
                    .await
            }
            Self::Http(http) => {
                http.capture(yookassa_id, amount_kopeks, idempotency_key)
                    .await
            }
        }
    }

    async fn cancel_hold(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => EmulatedYooKassa.cancel_hold(yookassa_id).await,
            Self::Failing => FailingYooKassa.cancel_hold(yookassa_id).await,
            Self::Http(http) => http.cancel_hold(yookassa_id).await,
        }
    }

    async fn get(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => EmulatedYooKassa.get(yookassa_id).await,
            Self::Failing => FailingYooKassa.get(yookassa_id).await,
            Self::Http(http) => http.get(yookassa_id).await,
        }
    }
}

/// Ключ холда для вызова шлюза (ADR-0014, см. [`payments::hold_key`]).
pub fn hold_key(rental_id: Uuid) -> String {
    payments::hold_key(rental_id)
}

/// Ключ capture для вызова шлюза (ADR-0014, см. [`payments::ride_key`]).
pub fn ride_key(rental_id: Uuid) -> String {
    payments::ride_key(rental_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_convert_without_float() {
        assert_eq!(format_amount(0), "0.00");
        assert_eq!(format_amount(5), "0.05");
        assert_eq!(format_amount(4950), "49.50");
        assert_eq!(format_amount(100_000), "1000.00");
        assert_eq!(parse_amount("49.50"), 4950);
        assert_eq!(parse_amount("1000.00"), 100_000);
        assert_eq!(parse_amount("7.5"), 750);
        assert_eq!(parse_amount("7"), 700);
        assert_eq!(parse_amount("garbage"), 0);
    }

    #[test]
    fn basic_auth_matches_rfc7617() {
        // Эталон из RFC 7617: base64("Aladdin:open sesame").
        assert_eq!(
            basic_auth_credentials("Aladdin", "open sesame"),
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(basic_auth_credentials("a", "b"), "YTpi");
        assert_eq!(basic_auth_credentials("ab", "cde"), "YWI6Y2Rl");
    }

    #[test]
    fn api_status_maps_to_domain() {
        assert_eq!(
            YkStatus::from_api("waiting_for_capture"),
            YkStatus::WaitingForCapture
        );
        assert_eq!(YkStatus::from_api("succeeded"), YkStatus::Succeeded);
        assert_eq!(YkStatus::from_api("canceled"), YkStatus::Canceled);
        assert_eq!(YkStatus::from_api("who-knows"), YkStatus::Unknown);
    }

    #[test]
    fn from_env_falls_back_to_emulated_without_keys() {
        // Ключи в тестовом окружении не заданы → эмуляция (локальный стенд).
        // Секреты только через env, в коде их нет — asserted by absence.
        assert!(matches!(YooKassa::from_env(), YooKassa::Emulated));
    }

    #[test]
    fn debug_never_prints_credentials() {
        let gateway =
            HttpYooKassa::new(DEFAULT_API_URL, "shop-1", "super-secret").expect("gateway");
        let rendered = format!("{gateway:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains(DEFAULT_API_URL));
    }
}
