//! Redis-клиент. Батчевые операции — только через `redis::pipe()`.

use redis::aio::ConnectionManager;
use std::time::Duration;
use tokio::sync::Mutex;

/// Потолок попытки коннекта: redis 0.25 сам не фейлится быстро
/// (reconnect-цикл при ECONNREFUSED), поэтому режем снаружи.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);

pub mod bookings;
pub mod geo;

/// Ленивое мультиплексированное соединение: первый запрос коннектит,
/// обрыв лечится переподключением на следующем `get()`.
#[derive(Clone)]
pub struct LazyConnection {
    client: redis::Client,
    conn: std::sync::Arc<Mutex<Option<ConnectionManager>>>,
}

impl LazyConnection {
    pub fn new(redis_url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(redis_url)?,
            conn: std::sync::Arc::new(Mutex::new(None)),
        })
    }

    /// Возвращает клон `ConnectionManager` (мультиплекс, переподключение внутри крейта).
    pub async fn get(&self) -> anyhow::Result<ConnectionManager> {
        {
            let guard = self.conn.lock().await;
            if let Some(conn) = guard.as_ref() {
                return Ok(conn.clone());
            }
        }
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.client.get_connection_manager())
            .await
            .map_err(|_| anyhow::anyhow!("redis connect timed out after {CONNECT_TIMEOUT:?}"))??;
        *self.conn.lock().await = Some(conn.clone());
        Ok(conn)
    }

    /// Сбрасывает кэш соединения после ошибки (следующий `get()` переподключится).
    pub async fn invalidate(&self) {
        self.conn.lock().await.take();
    }
}

pub async fn connect(redis_url: &str) -> anyhow::Result<redis::Client> {
    let client = redis::Client::open(redis_url)?;
    Ok(client)
}
