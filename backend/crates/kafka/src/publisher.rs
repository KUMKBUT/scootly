//! Продюсер Kafka (rdkafka): идемпотентный продюсер, acks=all
//! (durability — `x-kafka-durability` в docs/api/asyncapi.yaml).

use std::time::Duration;

use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};

/// Потолок ожидания места в очереди продюсера.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
/// Потолок доставки брокеру (message.timeout.ms).
const DELIVERY_TIMEOUT_MS: &str = "10000";

pub struct Publisher {
    producer: FutureProducer,
}

impl Publisher {
    pub fn new(brokers: &str) -> anyhow::Result<Self> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("message.timeout.ms", DELIVERY_TIMEOUT_MS)
            .set("enable.idempotence", "true")
            .create()?;
        Ok(Self { producer })
    }

    /// Публикует JSON-payload; `key` — id сущности (порядок партиций),
    /// `None` — рандомная партиция.
    pub async fn publish(
        &self,
        topic: &str,
        key: Option<&str>,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(payload)?;
        let key_buf = key.map(str::to_owned);
        let mut record = FutureRecord::<String, Vec<u8>>::to(topic).payload(&body);
        if let Some(key) = &key_buf {
            record = record.key(key);
        }
        // send().await возвращает итог доставки: Ok((partition, offset)) или
        // ошибку (queue full / message timeout / брокер недоступен).
        match self.producer.send(record, QUEUE_TIMEOUT).await {
            Ok(_) => Ok(()),
            Err((error, _)) => Err(anyhow::anyhow!("delivery failed: {error}")),
        }
    }

    /// Добивает неотправленное (graceful shutdown, ADR-0014).
    pub fn flush(&self, timeout: Duration) -> anyhow::Result<()> {
        self.producer.flush(timeout)?;
        Ok(())
    }
}
