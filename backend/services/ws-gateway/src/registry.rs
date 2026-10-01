//! Реестр WS-сессий: подписки на карту, heartbeat, один коннект на юзера.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message};
use tokio::sync::mpsc;

use crate::protocol::Subscription;

#[derive(Debug)]
pub struct Session {
    tx: mpsc::UnboundedSender<Message>,
    sub: Option<Subscription>,
    tracked_ride: Option<uuid::Uuid>,
    last_seen: Instant,
}

/// Потокобезопасный реестр активных сессий шлюза (один под — один реестр;
/// межподовый fan-out делает Redis pub/sub, см. fanout.rs).
#[derive(Default)]
pub struct Registry {
    sessions: Mutex<HashMap<uuid::Uuid, Session>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует сессию; возвращает канал предыдущей сессии юзера,
    /// если та была (её нужно закрыть с 4409).
    pub fn register(
        &self,
        user: uuid::Uuid,
        tx: mpsc::UnboundedSender<Message>,
    ) -> Option<mpsc::UnboundedSender<Message>> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .insert(
                user,
                Session {
                    tx,
                    sub: None,
                    tracked_ride: None,
                    last_seen: Instant::now(),
                },
            )
            .map(|old| old.tx)
    }

    /// Убирает сессию, только если канал всё ещё её (южер мог переконнектиться).
    pub fn remove(&self, user: uuid::Uuid, tx: &mpsc::UnboundedSender<Message>) {
        let mut sessions = self.sessions.lock().unwrap();
        let same = sessions
            .get(&user)
            .is_some_and(|session| session.tx.same_channel(tx));
        if same {
            sessions.remove(&user);
        }
    }

    pub fn set_subscription(&self, user: uuid::Uuid, sub: Option<Subscription>) {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&user) {
            session.sub = sub;
        }
    }

    pub fn track_ride(&self, user: uuid::Uuid, ride: uuid::Uuid) {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&user) {
            session.tracked_ride = Some(ride);
        }
    }

    pub fn touch(&self, user: uuid::Uuid) {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&user) {
            session.last_seen = Instant::now();
        }
    }

    /// `scooter.updated`: только сессиям, чья подписка покрывает точку.
    pub fn broadcast_updated(&self, lat: f64, lon: f64, text: String) {
        let sessions = self.sessions.lock().unwrap();
        for session in sessions.values() {
            if session.sub.is_some_and(|sub| sub.contains(lat, lon)) {
                let _ = session.tx.send(Message::Text(text.clone()));
            }
        }
    }

    /// `scooter.removed`: без координат — всем подписанным на карту.
    pub fn broadcast_removed(&self, text: String) {
        let sessions = self.sessions.lock().unwrap();
        for session in sessions.values() {
            if session.sub.is_some() {
                let _ = session.tx.send(Message::Text(text.clone()));
            }
        }
    }

    /// Приватное событие юзера (ride.*/payment.*/reservation.*, MVP #8):
    /// готовый конверт уходит только его сессии; нет сессии — некуда (§6).
    pub fn send_to_user(&self, user: uuid::Uuid, text: String) {
        if let Some(session) = self.sessions.lock().unwrap().get(&user) {
            let _ = session.tx.send(Message::Text(text));
        }
    }

    /// Рвёт соединения, молчащие дольше `max_idle` (websocket.md §1: 60 c).
    pub fn kick_idle(&self, max_idle: Duration) {
        let sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        for session in sessions.values() {
            if now.duration_since(session.last_seen) >= max_idle {
                let _ = session.tx.send(Message::Close(Some(CloseFrame {
                    code: 1000,
                    reason: "idle timeout".into(),
                })));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{envelope, Subscription};
    use std::time::Duration;

    fn sub_at(lat: f64, lon: f64, radius_m: f64) -> Subscription {
        Subscription { lat, lon, radius_m }
    }

    #[test]
    fn register_replaces_old_session_and_kicks_it() {
        let registry = Registry::new();
        let user = uuid::Uuid::new_v4();
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        assert!(registry.register(user, tx1).is_none());
        let old = registry.register(user, tx2).expect("old session");
        // Старому уходит close 4409.
        let _ = old.send(Message::Close(Some(CloseFrame {
            code: 4409,
            reason: "replaced".into(),
        })));
        match rx1.try_recv().unwrap() {
            Message::Close(frame) => assert_eq!(frame.unwrap().code, 4409),
            other => panic!("expected close, got {other:?}"),
        }
        assert!(rx2.try_recv().is_err(), "new session untouched");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn remove_only_own_channel() {
        let registry = Registry::new();
        let user = uuid::Uuid::new_v4();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        registry.register(user, tx1.clone());

        registry.remove(user, &tx2); // чужой канал — не трогаем
        assert_eq!(registry.len(), 1);
        registry.remove(user, &tx1); // свой — убираем
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn broadcast_filters_by_subscription() {
        let registry = Registry::new();
        let (tx_in, mut rx_in) = mpsc::unbounded_channel();
        let (tx_out, mut rx_out) = mpsc::unbounded_channel();
        let in_user = uuid::Uuid::new_v4();
        let out_user = uuid::Uuid::new_v4();
        registry.register(in_user, tx_in);
        registry.register(out_user, tx_out);
        registry.set_subscription(in_user, Some(sub_at(43.238, 76.889, 500.0)));
        registry.set_subscription(out_user, Some(sub_at(55.0, 37.0, 500.0)));

        let text = envelope(
            "scooter.updated",
            serde_json::json!({ "id": uuid::Uuid::new_v4(), "lat": 43.2385, "lon": 76.8895 }),
        );
        registry.broadcast_updated(43.2385, 76.8895, text);

        assert!(rx_in.try_recv().is_ok(), "subscribed session got event");
        assert!(rx_out.try_recv().is_err(), "far session got nothing");
    }

    #[tokio::test]
    async fn broadcast_removed_reaches_all_map_subscribers() {
        let registry = Registry::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let user = uuid::Uuid::new_v4();
        registry.register(user, tx);
        registry.set_subscription(user, Some(sub_at(1.0, 1.0, 100.0)));

        registry.broadcast_removed(envelope(
            "scooter.removed",
            serde_json::json!({ "id": uuid::Uuid::new_v4(), "reason": "offline" }),
        ));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn kick_idle_closes_silent_sessions() {
        let registry = Registry::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let user = uuid::Uuid::new_v4();
        registry.register(user, tx);

        registry.kick_idle(Duration::from_secs(60));
        assert!(rx.try_recv().is_err(), "fresh session must stay");

        // Симулируем тишину: touch не звали, deadline истёк — сдвигаем часы вручную.
        // (Instant нельзя отмотать, поэтому проверяем через маленький max_idle.)
        registry.kick_idle(Duration::from_millis(0));
        match rx.try_recv().unwrap() {
            Message::Close(_) => {}
            other => panic!("expected close, got {other:?}"),
        }
    }
}
