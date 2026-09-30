//! Верификация Telegram WebApp `initData` (ADR-0013).
//!
//! Алгоритм Telegram:
//! 1. `data_check_string` — все поля, кроме `hash`, отсортированные по ключу,
//!    в формате `key=<urldecoded value>`, разделитель `\n`;
//! 2. `secret_key = HMAC_SHA256(key="WebAppData", msg=bot_token)`;
//! 3. `hash = hex(HMAC_SHA256(key=secret_key, msg=data_check_string))`.
//!
//! Дополнительно проверяем свежесть `auth_date` — защита от replay (accepted
//! risk ADR-0013 митигируется окном валидности).

use std::collections::BTreeMap;

use common::{AppError, AppResult};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

/// Максимальный возраст initData: 24 часа.
pub const MAX_AUTH_AGE_SECS: i64 = 24 * 60 * 60;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub photo_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InitData {
    pub user: TelegramUser,
    pub auth_date: i64,
}

/// Собирает подписанную строку initData (для сидера и e2e-эмуляции Telegram).
/// Значения на вход — как их шлёт Telegram (percent-encoded); hash считается
/// по декодированным значениям.
pub fn build_signed_init_data(bot_token: &str, params: &[(&str, &str)]) -> String {
    let mut fields: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let decoded: BTreeMap<String, String> = fields
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                percent_decode(v).expect("valid percent-encoding"),
            )
        })
        .collect();
    let check_string = data_check_string(&decoded);
    fields.insert("hash".into(), compute_hash(bot_token, &check_string));
    fields
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn parse_pairs(raw: &str) -> AppResult<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for pair in raw.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| AppError::Validation("initData: malformed pair".into()))?;
        if key.is_empty() {
            return Err(AppError::Validation("initData: empty key".into()));
        }
        fields.insert(key.to_owned(), percent_decode(value)?);
    }
    Ok(fields)
}

fn data_check_string(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .filter(|(k, _)| k.as_str() != "hash")
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn compute_hash(bot_token: &str, check_string: &str) -> String {
    let secret = secret_key(bot_token);
    let mut mac = HmacSha256::new_from_slice(&secret).expect("hmac accepts any key length");
    mac.update(check_string.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn secret_key(bot_token: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(b"WebAppData").expect("hmac accepts any key length");
    mac.update(bot_token.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Telegram кодирует значения через `encodeURIComponent`: только `%XX`,
/// символ `+` — литеральный плюс, а не пробел.
fn percent_decode(value: &str) -> AppResult<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex_pair = bytes
                .get(i + 1..i + 3)
                .and_then(|slice| std::str::from_utf8(slice).ok())
                .and_then(|slice| u8::from_str_radix(slice, 16).ok())
                .ok_or_else(|| AppError::Validation("initData: bad percent-encoding".into()))?;
            out.push(hex_pair);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| AppError::Validation("initData: not utf-8".into()))
}

/// Проверяет подпись и свежесть `initData`.
#[tracing::instrument(skip_all, fields(bytes = raw.len()))]
pub fn verify(raw: &str, bot_token: &str) -> AppResult<InitData> {
    let fields = parse_pairs(raw)?;

    let provided_hash = fields
        .get("hash")
        .ok_or_else(|| AppError::Unauthorized("initData: hash missing".into()))?;
    let provided = hex::decode(provided_hash)
        .map_err(|_| AppError::Unauthorized("initData: hash is not hex".into()))?;
    // verify_slice — сравнение в постоянном времени.
    let mut mac = HmacSha256::new_from_slice(&secret_key(bot_token))
        .map_err(|e| AppError::Internal(e.into()))?;
    mac.update(data_check_string(&fields).as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| AppError::Unauthorized("initData: bad signature".into()))?;

    let auth_date: i64 = fields
        .get("auth_date")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AppError::Unauthorized("initData: auth_date missing".into()))?;
    let now = chrono::Utc::now().timestamp();
    if (now - auth_date).abs() > MAX_AUTH_AGE_SECS {
        return Err(AppError::Unauthorized("initData: auth_date expired".into()));
    }

    let user: TelegramUser = serde_json::from_str(
        fields
            .get("user")
            .ok_or_else(|| AppError::Unauthorized("initData: user missing".into()))?,
    )
    .map_err(|_| AppError::Unauthorized("initData: bad user payload".into()))?;

    Ok(InitData { user, auth_date })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT_TOKEN: &str = "123456:TEST-TOKEN";

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn signed(auth_date: i64) -> String {
        build_signed_init_data(
            BOT_TOKEN,
            &[
                ("auth_date", &auth_date.to_string()),
                (
                    "user",
                    r#"{"id":777,"first_name":"Test","username":"tester"}"#,
                ),
            ],
        )
    }

    #[test]
    fn valid_init_data_passes() {
        let data = verify(&signed(now()), BOT_TOKEN).unwrap();
        assert_eq!(data.user.id, 777);
        assert_eq!(data.user.username.as_deref(), Some("tester"));
    }

    #[test]
    fn percent_encoded_value_decodes() {
        let raw = build_signed_init_data(
            BOT_TOKEN,
            &[
                ("auth_date", &now().to_string()),
                ("first_name", "%D0%98%D0%B2%D0%B0%D0%BD"), // "Иван"
                ("user", r#"{"id":1}"#),
            ],
        );
        let fields = parse_pairs(&raw).unwrap();
        assert_eq!(fields.get("first_name").unwrap(), "Иван");
        verify(&raw, BOT_TOKEN).unwrap();
    }

    #[test]
    fn tampered_value_fails() {
        let raw = signed(now());
        let tampered = raw.replace("777", "778");
        assert!(verify(&tampered, BOT_TOKEN).is_err());
    }

    #[test]
    fn wrong_bot_token_fails() {
        assert!(verify(&signed(now()), "1:OTHER").is_err());
    }

    #[test]
    fn stale_auth_date_fails() {
        let stale = signed(now() - MAX_AUTH_AGE_SECS - 60);
        assert!(verify(&stale, BOT_TOKEN).is_err());
    }

    #[test]
    fn missing_hash_fails() {
        let raw = signed(now());
        let no_hash = raw
            .split('&')
            .filter(|pair| !pair.starts_with("hash="))
            .collect::<Vec<_>>()
            .join("&");
        assert!(verify(&no_hash, BOT_TOKEN).is_err());
    }

    #[test]
    fn missing_user_fails() {
        let raw = build_signed_init_data(BOT_TOKEN, &[("auth_date", &now().to_string())]);
        assert!(verify(&raw, BOT_TOKEN).is_err());
    }

    #[test]
    fn plus_is_not_space() {
        let fields = parse_pairs("a=b+c%20d").unwrap();
        assert_eq!(fields.get("a").unwrap(), "b+c d");
    }
}
