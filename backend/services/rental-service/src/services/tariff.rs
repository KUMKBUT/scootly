//! Тариф MVP (docs/mvp.md §2): один — per_minute, фикс разблокировки + цена
//! минуты. Значения — в конфиге/env, деньги — только в копейках (никогда float).

use chrono::DateTime;
use chrono::Utc;

pub const ENV_UNLOCK_KOPEKS: &str = "TARIFF_UNLOCK_KOPEKS";
pub const ENV_PER_MIN_KOPEKS: &str = "TARIFF_PER_MIN_KOPEKS";
/// На сколько минут подряд берём холд (максимум оценки поездки).
pub const ENV_HOLD_MINUTES: &str = "TARIFF_HOLD_MINUTES";

/// Дефолты, если env не задан: 29 ₽ разблокировка + 8 ₽/мин, холд на 60 мин.
pub const DEFAULT_UNLOCK_KOPEKS: i32 = 2900;
pub const DEFAULT_PER_MIN_KOPEKS: i32 = 800;
pub const DEFAULT_HOLD_MINUTES: i32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tariff {
    pub unlock_kopeks: i32,
    pub per_min_kopeks: i32,
    /// На сколько минут берётся холд (оценка максимума поездки).
    pub hold_minutes: i32,
}

/// Стоимости на момент времени: минуты для UI-тика и сумма.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    pub total_min: i32,
    pub amount_kopeks: i32,
}

impl Tariff {
    pub fn from_env() -> Self {
        let read = |name: &str, default: i32| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
                .filter(|v| *v >= 0)
                .unwrap_or(default)
        };
        Self {
            unlock_kopeks: read(ENV_UNLOCK_KOPEKS, DEFAULT_UNLOCK_KOPEKS),
            per_min_kopeks: read(ENV_PER_MIN_KOPEKS, DEFAULT_PER_MIN_KOPEKS),
            hold_minutes: read(ENV_HOLD_MINUTES, DEFAULT_HOLD_MINUTES).max(1),
        }
    }

    /// Сумма холда на старте (MVP #5, ADR-0003): фикс разблокировки +
    /// цена минуты × hold_minutes — оценка максимума поездки в копейках.
    pub fn hold_amount(&self) -> i32 {
        self.unlock_kopeks
            .saturating_add(self.per_min_kopeks.saturating_mul(self.hold_minutes))
    }

    /// Расчёт на момент `now`: неполная минута считается целой, минимум —
    /// 1 минута. Чистая функция: тик в UI (клиент) и финиш (сервер) считают
    /// одинаково, снапшот GET /rides/{id} ничего не фиксирует.
    pub fn cost(&self, started_at: DateTime<Utc>, now: DateTime<Utc>) -> Cost {
        let elapsed_secs = (now - started_at).num_seconds().max(0);
        let total_min = i32::try_from(elapsed_secs / 60 + i64::from(elapsed_secs % 60 != 0))
            .unwrap_or(i32::MAX)
            .max(1);
        Cost {
            total_min,
            amount_kopeks: self
                .unlock_kopeks
                .saturating_add(self.per_min_kopeks.saturating_mul(total_min)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn tariff() -> Tariff {
        Tariff {
            unlock_kopeks: 2900,
            per_min_kopeks: 800,
            hold_minutes: 60,
        }
    }

    fn at(secs_after_start: i64) -> (DateTime<Utc>, DateTime<Utc>) {
        let start = Utc::now();
        (start, start + Duration::seconds(secs_after_start))
    }

    #[test]
    fn minimum_one_minute_even_instant_finish() {
        let (start, now) = at(0);
        let cost = tariff().cost(start, now);
        assert_eq!(cost.total_min, 1);
        assert_eq!(cost.amount_kopeks, 2900 + 800);
    }

    #[test]
    fn partial_minute_is_billed_whole() {
        for secs in [61, 90, 119] {
            let (start, now) = at(secs);
            let cost = tariff().cost(start, now);
            assert_eq!(cost.total_min, 2, "{secs}s must round up to 2 min");
        }
        // Ровно 2:00 — две минуты; секунда сверху — уже три.
        let (start, now) = at(120);
        assert_eq!(tariff().cost(start, now).total_min, 2);
        let (start, now) = at(121);
        assert_eq!(tariff().cost(start, now).total_min, 3);
    }

    #[test]
    fn amount_is_unlock_plus_per_minute() {
        let (start, now) = at(11 * 60);
        let cost = tariff().cost(start, now);
        assert_eq!(cost.total_min, 11);
        assert_eq!(cost.amount_kopeks, 2900 + 800 * 11);
    }

    #[test]
    fn clock_skew_is_clamped_to_minimum() {
        let (start, now) = at(-300);
        let cost = tariff().cost(start, now);
        assert_eq!(cost.total_min, 1);
    }

    #[test]
    fn saturates_instead_of_overflow() {
        let (start, now) = at(i64::MAX / 10_000_000);
        let cost = tariff().cost(start, now);
        assert!(cost.amount_kopeks > 0);
    }

    #[test]
    fn hold_amount_is_unlock_plus_hold_minutes() {
        assert_eq!(tariff().hold_amount(), 2900 + 800 * 60);
        let small = Tariff {
            unlock_kopeks: 0,
            per_min_kopeks: 1,
            hold_minutes: 1,
        };
        assert_eq!(small.hold_amount(), 1);
    }
}
