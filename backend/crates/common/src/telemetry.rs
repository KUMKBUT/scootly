//! Маскирование PII перед логированием (см. ревизию рисков, п.3).
//!
//! Правила:
//! - телефоны (`+7XXXXXXXXXX`) — показываем только последние 2 цифры;
//! - координаты — огрубляем до ~1 км (1 знак после запятой);
//! - использовать везде, где в `tracing!`-события попадают телефоны/координаты.

/// Маскирует российский номер: `+79150000012` → `+7********12`.
pub fn mask_phone(phone: &str) -> String {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return "*".repeat(digits.len().max(4));
    }
    let tail: String = digits
        .chars()
        .rev()
        .take(2)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("+{}********{}", &digits[..1.min(digits.len())], tail)
}

/// Огрубляет координату до 0.1° (~11 км по широте, достаточно против трекинга).
pub fn mask_coord(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_is_masked() {
        let masked = mask_phone("+79150000012");
        assert!(
            !masked.contains("915000001"),
            "full number leaked: {masked}"
        );
        assert!(masked.ends_with("12"), "tail must be preserved: {masked}");
    }

    #[test]
    fn short_input_does_not_leak() {
        assert_eq!(mask_phone("12"), "****");
    }

    #[test]
    fn coords_are_coarsened() {
        assert_eq!(mask_coord(55.7558), 55.8);
        assert_ne!(mask_coord(55.7558), 55.7558);
    }
}
