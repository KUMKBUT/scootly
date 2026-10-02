//! Шлюз замков: команды unlock/lock по MQTT (ADR-0002, MVP #10).
//!
//! Команды публикуются в `scooter/{id}/cmd` (QoS 1), ack ждётся из
//! `scooter/{id}/ack`; ответа нет за [`UNLOCK_ACK_TIMEOUT_SECS`] — ошибка
//! `unlock_timeout`, сервис выполняет компенсацию (void холда, самокат
//! offline, ADR-0006). Дубликаты ack (redelivery QoS 1) снимаются дедупом
//! `dedup:{msg_id}` в Redis, TTL 5 мин (ADR-0009, метрика
//! `mqtt_dedup_dropped_total`). Варианты [`Locks`]: `Mqtt` (брокер из env),
//! `Emulated` (всегда ack), `Silent` (не отвечает — тесты компенсации).

use std::time::Duration;

use common::{AppError, AppResult};
use redis_client::LazyConnection;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Publish, QoS};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Ожидание ack от замка — 10 секунд (таймаут команды `scooter/{id}/cmd`,
/// ADR-0006; DoD MVP: unlock-fail за 10 c).
pub const UNLOCK_ACK_TIMEOUT_SECS: u64 = 10;

/// TTL дедуп-ключа `dedup:{msg_id}` — окно redelivery QoS 1 (ADR-0009).
pub const MQTT_DEDUP_TTL_SECS: u64 = 5 * 60;

/// Подтверждение замка: ack в течение таймаута.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockAck {
    pub scooter_id: Uuid,
}

/// Точка интеграции с замками. Асинхронный контракт — под MQTT с QoS 1
/// (ADR-0009: дедуп на консюмере).
pub trait LockGateway: Send + Sync {
    fn unlock(
        &self,
        scooter_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<LockAck>> + Send;

    fn lock(
        &self,
        scooter_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<LockAck>> + Send;
}

/// Эмуляция: команда доходит всегда (устройство «на столе»).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmulatedLocks;

impl LockGateway for EmulatedLocks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        tracing::debug!(%scooter_id, "emulated unlock ack");
        Ok(LockAck { scooter_id })
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        tracing::debug!(%scooter_id, "emulated lock ack");
        Ok(LockAck { scooter_id })
    }
}

/// Замок не отвечает никогда (устройство офлайн/разряжено) — контракт
/// компенсации unlock-fail в тестах (ADR-0006).
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentLocks;

impl LockGateway for SilentLocks {
    async fn unlock(&self, _scooter_id: Uuid) -> AppResult<LockAck> {
        std::future::pending().await
    }

    async fn lock(&self, _scooter_id: Uuid) -> AppResult<LockAck> {
        std::future::pending().await
    }
}

/// Команда замку — топик `scooter/{id}/cmd` (ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Unlock,
    Lock,
}

impl CommandKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unlock => "unlock",
            Self::Lock => "lock",
        }
    }
}

/// Тело команды: `msg_id` нужен устройству для дедупа и для сопоставления
/// ack с командой (QoS 1 at-least-once, ADR-0009).
#[derive(Serialize)]
struct LockCommand {
    msg_id: Uuid,
    command: &'static str,
}

/// Тело ack из `scooter/{id}/ack`: устройство эхом возвращает `msg_id`.
#[derive(Deserialize)]
struct LockAckMessage {
    msg_id: Uuid,
    command: String,
    ok: bool,
}

/// Ack, разосланный ждущим командам (сопоставление по `msg_id`).
#[derive(Debug, Clone)]
struct AckMessage {
    msg_id: Uuid,
    scooter_id: Uuid,
    command: CommandKind,
}

/// MQTT-шлюз замков (MVP #10). Фоновая задача прокачивает eventloop,
/// восстанавливает подписку на ack после переподключений и рассылает ack
/// ждущим; брокер может быть недоступен на момент старта — poll ретраит.
#[derive(Debug, Clone)]
pub struct MqttLocks {
    client: AsyncClient,
    acks: broadcast::Sender<AckMessage>,
}

impl MqttLocks {
    /// Подключается к брокеру (`tcp://host:port`, `mqtt://host:port` или
    /// `host[:port]`, порт по умолчанию 1883) и запускает pump-задачу
    /// (дедуп ack — через `redis`, ADR-0009).
    pub fn connect(broker_url: &str, redis: LazyConnection) -> AppResult<Self> {
        let (host, port) = parse_broker_url(broker_url)?;
        let options = MqttOptions::new(format!("rental-service-{}", Uuid::new_v4()), host, port);
        let (client, eventloop) = AsyncClient::new(options, 64);
        let (acks, _) = broadcast::channel(256);
        tokio::spawn(pump(eventloop, client.clone(), acks.clone(), redis));
        tracing::info!(broker = broker_url, "mqtt lock gateway started");
        Ok(Self { client, acks })
    }

    /// Публикация команды (QoS 1) + ожидание её ack.
    async fn command(&self, scooter_id: Uuid, command: CommandKind) -> AppResult<LockAck> {
        let msg_id = Uuid::new_v4();
        let payload = serde_json::to_vec(&LockCommand {
            msg_id,
            command: command.as_str(),
        })
        .map_err(|error| AppError::Internal(anyhow::Error::new(error)))?;
        self.client
            .publish(
                format!("scooter/{scooter_id}/cmd"),
                QoS::AtLeastOnce,
                false,
                payload,
            )
            .await
            .map_err(|error| AppError::Upstream {
                code: "lock_publish_failed",
                message: format!("mqtt publish failed: {error}"),
            })?;
        tracing::info!(
            %scooter_id,
            %msg_id,
            command = command.as_str(),
            "lock command published"
        );
        self.wait_ack(msg_id, scooter_id, command).await
    }

    /// Ждёт ack с данным `msg_id` и командой (чужие ack пропускает); общий
    /// таймаут 10 c накладывает [`Locks`] (ADR-0006).
    async fn wait_ack(
        &self,
        msg_id: Uuid,
        scooter_id: Uuid,
        command: CommandKind,
    ) -> AppResult<LockAck> {
        let mut rx = self.acks.subscribe();
        loop {
            match rx.recv().await {
                Ok(ack) if ack.msg_id == msg_id && ack.command == command => {
                    tracing::debug!(%scooter_id, %msg_id, "lock ack received");
                    return Ok(LockAck {
                        scooter_id: ack.scooter_id,
                    });
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(%scooter_id, skipped, "lock ack queue lagged");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(AppError::Upstream {
                        code: "lock_ack_lost",
                        message: "mqtt ack pump is down".into(),
                    });
                }
            }
        }
    }
}

impl LockGateway for MqttLocks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        self.command(scooter_id, CommandKind::Unlock).await
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        self.command(scooter_id, CommandKind::Lock).await
    }
}

/// Прокачка eventloop: подписка на ack (после каждого (пере)подключения —
/// clean session), дедуп и broadcast. Ошибки соединения — ретрай через 1 c.
async fn pump(
    mut eventloop: rumqttc::EventLoop,
    client: AsyncClient,
    acks: broadcast::Sender<AckMessage>,
    redis: LazyConnection,
) {
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                if let Err(error) = client.subscribe("scooter/+/ack", QoS::AtLeastOnce).await {
                    tracing::error!(%error, "mqtt resubscribe failed");
                }
            }
            Ok(Event::Incoming(Incoming::Publish(publish))) => {
                handle_ack(&publish, &acks, &redis).await;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "mqtt connection lost, retrying in 1s");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Один входящий ack: парсинг → дедуп `msg_id` (ADR-0009) → broadcast.
async fn handle_ack(
    publish: &Publish,
    acks: &broadcast::Sender<AckMessage>,
    redis: &LazyConnection,
) {
    let Some((ack, ok)) = parse_ack(&publish.topic, &publish.payload) else {
        tracing::warn!(topic = %publish.topic, "unparseable lock ack");
        return;
    };
    if is_duplicate(redis, ack.msg_id).await {
        metrics::counter!("mqtt_dedup_dropped_total").increment(1);
        return;
    }
    if !ok {
        // Устройство отказало: ack не засчитываем — команда уйдёт в таймаут
        // и компенсацию (ADR-0006).
        tracing::warn!(
            scooter_id = %ack.scooter_id,
            msg_id = %ack.msg_id,
            "lock rejected command"
        );
        return;
    }
    let _ = acks.send(ack);
}

/// `SET dedup:{msg_id} 1 NX EX 300` (ADR-0009). `true` — дубль (уже
/// обрабатывали). Redis недоступен/ошибка → не дропаем: повторный ack
/// идемпотентен, а потеря ack стоит юзеру 10 c ожидания.
async fn is_duplicate(redis: &LazyConnection, msg_id: Uuid) -> bool {
    let Ok(mut conn) = redis.get().await else {
        tracing::warn!("redis unavailable, mqtt dedup skipped");
        return false;
    };
    let created: Result<Option<String>, _> = redis::cmd("SET")
        .arg(format!("dedup:{msg_id}"))
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(MQTT_DEDUP_TTL_SECS)
        .query_async(&mut conn)
        .await;
    match created {
        Ok(None) => false,
        Ok(Some(_)) => true,
        Err(error) => {
            tracing::warn!(%error, "mqtt dedup check failed, treating as first");
            false
        }
    }
}

/// Парсинг `scooter/{id}/ack` + тела ack. Неизвестная команда — как мусор.
/// Второй элемент ответа — `ok` устройства (`false` = отказ, не мусор).
fn parse_ack(topic: &str, payload: &[u8]) -> Option<(AckMessage, bool)> {
    let scooter_id = topic
        .strip_prefix("scooter/")?
        .strip_suffix("/ack")?
        .parse()
        .ok()?;
    let message: LockAckMessage = serde_json::from_slice(payload).ok()?;
    let command = match message.command.as_str() {
        "unlock" => CommandKind::Unlock,
        "lock" => CommandKind::Lock,
        _ => return None,
    };
    Some((
        AckMessage {
            msg_id: message.msg_id,
            scooter_id,
            command,
        },
        message.ok,
    ))
}

/// Выбранный шлюз замков (enum вместо `Arc<dyn>` — без async-trait).
#[derive(Debug, Clone, Default)]
pub enum Locks {
    #[default]
    Emulated,
    /// MQTT-брокер из env `MQTT_BROKER_URL` (MVP #10, ADR-0002).
    Mqtt(MqttLocks),
    Silent,
}

impl Locks {
    /// Из env `MQTT_BROKER_URL` (например `tcp://mosquitto:1883`);
    /// без переменной — эмуляция (локальный стенд без брокера/устройств).
    pub fn from_env(redis: LazyConnection) -> Self {
        match std::env::var("MQTT_BROKER_URL") {
            Ok(url) if !url.is_empty() => match MqttLocks::connect(&url, redis) {
                Ok(mqtt) => Self::Mqtt(mqtt),
                Err(error) => {
                    tracing::error!(%error, "bad MQTT_BROKER_URL, falling back to emulated");
                    Self::Emulated
                }
            },
            _ => Self::Emulated,
        }
    }
}

impl LockGateway for Locks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        // Таймаут команды scooter/{id}/cmd — 10 c (ADR-0006).
        let ack = async {
            match self {
                Self::Emulated => EmulatedLocks.unlock(scooter_id).await,
                Self::Mqtt(mqtt) => mqtt.unlock(scooter_id).await,
                Self::Silent => SilentLocks.unlock(scooter_id).await,
            }
        };
        match tokio::time::timeout(Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS), ack).await {
            Ok(result) => result,
            Err(_elapsed) => Err(unlock_timeout(scooter_id)),
        }
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        // Финиш: ack ждали бы так же; таймаут компенсирует юзер ретраем
        // (поездка остаётся активной, MVP #4).
        let ack = async {
            match self {
                Self::Emulated => EmulatedLocks.lock(scooter_id).await,
                Self::Mqtt(mqtt) => mqtt.lock(scooter_id).await,
                Self::Silent => SilentLocks.lock(scooter_id).await,
            }
        };
        match tokio::time::timeout(Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS), ack).await {
            Ok(result) => result,
            Err(_elapsed) => Err(AppError::Upstream {
                code: "lock_ack_timeout",
                message: "scooter did not confirm lock".into(),
            }),
        }
    }
}

fn unlock_timeout(scooter_id: Uuid) -> AppError {
    tracing::warn!(%scooter_id, "unlock ack timeout (10s)");
    AppError::Upstream {
        code: "unlock_timeout",
        message: "scooter did not confirm unlock".into(),
    }
}

/// `tcp://host:port` | `mqtt://host:port` | `host:port` | `host` (порт 1883).
fn parse_broker_url(url: &str) -> AppResult<(String, u16)> {
    let rest = url
        .strip_prefix("tcp://")
        .or_else(|| url.strip_prefix("mqtt://"))
        .unwrap_or(url)
        .trim_end_matches('/');
    let bad = || AppError::Validation(format!("invalid MQTT_BROKER_URL: {url}"));
    match rest.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return Err(bad());
            }
            let port = port.parse().map_err(|_| bad())?;
            Ok((host.to_owned(), port))
        }
        None if !rest.is_empty() => Ok((rest.to_owned(), 1883)),
        None => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0006: нет ack за 10 c → `unlock_timeout` (виртуальное время — мгновенно).
    #[tokio::test(start_paused = true)]
    async fn silent_lock_times_out_after_10s() {
        let started = tokio::time::Instant::now();
        let error = Locks::Silent
            .unlock(Uuid::new_v4())
            .await
            .expect_err("silent lock must not ack");
        assert_eq!(error.code(), "unlock_timeout");
        // tokio paused-время: ровно таймаут, реальных секунд нет.
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS)
        );
    }

    #[tokio::test]
    async fn emulated_lock_acks_within_timeout() {
        let scooter_id = Uuid::new_v4();
        let ack = Locks::Emulated
            .unlock(scooter_id)
            .await
            .expect("emulated ack");
        assert_eq!(ack.scooter_id, scooter_id);
    }

    #[test]
    fn parse_broker_url_variants() {
        assert_eq!(
            parse_broker_url("tcp://mosquitto:1883").unwrap(),
            ("mosquitto".into(), 1883)
        );
        assert_eq!(
            parse_broker_url("mqtt://emqx.local:8883/").unwrap(),
            ("emqx.local".into(), 8883)
        );
        assert_eq!(
            parse_broker_url("broker:1884").unwrap(),
            ("broker".into(), 1884)
        );
        assert_eq!(
            parse_broker_url("localhost").unwrap(),
            ("localhost".into(), 1883)
        );
    }

    #[test]
    fn parse_broker_url_rejects_garbage() {
        for url in ["", "tcp://:1883", "host:not-a-port"] {
            assert!(parse_broker_url(url).is_err(), "must reject {url}");
        }
    }

    #[test]
    fn command_payload_carries_msg_id_and_command() {
        let msg_id = Uuid::new_v4();
        let payload = serde_json::to_vec(&LockCommand {
            msg_id,
            command: CommandKind::Unlock.as_str(),
        })
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(value["msg_id"], msg_id.to_string());
        assert_eq!(value["command"], "unlock");
    }

    #[test]
    fn parse_ack_matches_topic_and_payload() {
        let msg_id = Uuid::new_v4();
        let scooter_id = Uuid::new_v4();
        let payload = format!(r#"{{"msg_id":"{msg_id}","command":"lock","ok":true}}"#);
        let (ack, ok) =
            parse_ack(&format!("scooter/{scooter_id}/ack"), payload.as_bytes()).expect("valid ack");
        assert_eq!(ack.msg_id, msg_id);
        assert_eq!(ack.scooter_id, scooter_id);
        assert_eq!(ack.command, CommandKind::Lock);
        assert!(ok);
    }

    #[test]
    fn parse_ack_rejects_malformed() {
        let msg_id = Uuid::new_v4();
        let payload = format!(r#"{{"msg_id":"{msg_id}","command":"unlock","ok":true}}"#);
        // Чужой топик, мусорный JSON, неизвестная команда.
        assert!(parse_ack("other/1/ack", payload.as_bytes()).is_none());
        assert!(parse_ack("scooter/not-a-uuid/ack", payload.as_bytes()).is_none());
        assert!(parse_ack(
            "scooter/01920223-aadb-4c01-ab01-000000000000/ack",
            b"{not json",
        )
        .is_none());
        assert!(parse_ack(
            "scooter/01920223-aadb-4c01-ab01-000000000000/ack",
            br#"{"msg_id":"01920223-aadb-4c01-ab01-000000000009","command":"reboot","ok":true}"#,
        )
        .is_none());
    }

    /// wait_ack резолвится только по своему msg_id; чужие ack пропускает.
    #[tokio::test]
    async fn wait_ack_matches_msg_id() {
        let (client, _eventloop) = AsyncClient::new(MqttOptions::new("test", "127.0.0.1", 1883), 8);
        let (acks, _) = broadcast::channel(16);
        let locks = MqttLocks {
            client,
            acks: acks.clone(),
        };
        let (mine, other, scooter_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let waiter = tokio::spawn({
            let locks = locks.clone();
            async move { locks.wait_ack(mine, scooter_id, CommandKind::Unlock).await }
        });
        // Даём подписаться, потом шлём чужой и свой ack.
        tokio::time::sleep(Duration::from_millis(10)).await;
        acks.send(AckMessage {
            msg_id: other,
            scooter_id,
            command: CommandKind::Unlock,
        })
        .unwrap();
        acks.send(AckMessage {
            msg_id: mine,
            scooter_id,
            command: CommandKind::Unlock,
        })
        .unwrap();
        let ack = waiter.await.unwrap().expect("own ack");
        assert_eq!(ack.scooter_id, scooter_id);
    }
}
