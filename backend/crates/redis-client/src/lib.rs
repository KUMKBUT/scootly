//! Redis-клиент. Батчевые операции — только через `redis::pipe()`.

pub async fn connect(redis_url: &str) -> anyhow::Result<redis::Client> {
    let client = redis::Client::open(redis_url)?;
    Ok(client)
}
