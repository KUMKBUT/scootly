//! Обёртка над rdkafka: продюсер, Outbox-релей (MVP #8, ADR-0008).
//!
//! Публикация — at-least-once: событие уходит в Kafka, только после
//! подтверждения доставки запись outbox помечается опубликованной;
//! сбой оставляет её на следующий проход релея.

pub mod publisher;
pub mod relay;

pub use publisher::Publisher;

/// Адрес брокеров из env; локальный стек — `localhost:29092` (docker-compose).
pub fn brokers_from_env() -> String {
    std::env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:29092".to_string())
}
