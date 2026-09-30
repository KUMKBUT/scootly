//! Обёртка над rdkafka: продюсер/консюмер, Outbox-воркер.

pub fn brokers_from_env() -> String {
    std::env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:29092".to_string())
}
