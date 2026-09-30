//! WebSocket-сессии: JWT через query (Mini App не умеет заголовки на WS),
//! ping/pong, подписки на карту. Один коннект на юзера: повторный — close 4409.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use common::auth::{decode_token, Claims, TokenType};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::protocol::{self, ClientMsg};
use crate::registry::Registry;
use crate::AppState;

/// Тишина дольше этого → шлюз рвёт соединение (websocket.md §1).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(serde::Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

pub async fn health() -> &'static str {
    "ok"
}

/// JWT из `?token=` → claims в extensions. Иначе 401 до апгрейда.
pub async fn auth_middleware(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(token) = query.token else {
        return common::AppError::Unauthorized("missing token".into()).into_response();
    };
    match decode_token(&state.jwt, &token, TokenType::Access) {
        Ok(claims) => {
            request.extensions_mut().insert(claims);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    axum::Extension(claims): axum::Extension<Claims>,
) -> Response {
    ws.on_upgrade(move |socket| session(socket, state, claims))
}

async fn session(socket: WebSocket, state: AppState, claims: Claims) {
    let user_id = claims.sub;
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    if let Some(old_tx) = state.sessions.register(user_id, tx.clone()) {
        // Один коннект на юзера: старый рвём (websocket.md §1).
        let _ = old_tx.send(replaced_close());
    }
    tracing::info!(%user_id, "ws session opened");

    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(Message::Close(frame)) => {
                    let _ = sender.send(Message::Close(frame)).await;
                    break;
                }
                Some(message) => {
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inbound = receiver.next() => match inbound {
                Some(Ok(message)) => {
                    state.sessions.touch(user_id);
                    if handle_inbound(&state.sessions, user_id, &mut sender, message).await.is_err() {
                        break;
                    }
                }
                Some(Err(error)) => {
                    tracing::debug!(%user_id, %error, "ws receive error");
                    break;
                }
                None => break,
            },
        }
    }
    state.sessions.remove(user_id, &tx);
    tracing::info!(%user_id, "ws session closed");
}

fn replaced_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: 4409,
        reason: "replaced by new connection".into(),
    }))
}

/// Возвращает Err только на ошибку отправки (соединение мертво).
async fn handle_inbound(
    registry: &Registry,
    user_id: uuid::Uuid,
    sender: &mut (impl futures_util::Sink<Message> + Unpin),
    message: Message,
) -> Result<(), ()> {
    match message {
        // Прикладной heartbeat из websocket.md §1.
        Message::Text(raw) => match protocol::parse_client_message(&raw) {
            Ok(ClientMsg::Ping) => {
                send(sender, protocol::envelope("pong", serde_json::json!({}))).await
            }
            Ok(ClientMsg::Subscribe { lat, lon, radius_m }) => {
                registry
                    .set_subscription(user_id, Some(protocol::Subscription { lat, lon, radius_m }));
                tracing::debug!(%user_id, lat, lon, radius_m, "scooters subscription");
                Ok(())
            }
            Ok(ClientMsg::Unsubscribe) => {
                registry.set_subscription(user_id, None);
                Ok(())
            }
            Ok(ClientMsg::TrackRide { ride_id }) => {
                // События поездки придут в этой же сессии (MVP #3/#4); здесь фиксируем интерес.
                registry.track_ride(user_id, ride_id);
                Ok(())
            }
            Err(bad) => {
                send(
                    sender,
                    protocol::envelope(
                        "error",
                        serde_json::json!({ "code": "bad_message", "message": bad }),
                    ),
                )
                .await
            }
        },
        // Транспортный heartbeat (ws-ping): отвечаем понгом.
        Message::Ping(payload) => sender.send(Message::Pong(payload)).await.map_err(|_| ()),
        Message::Close(_) => sender.send(Message::Close(None)).await.map_err(|_| ()),
        Message::Binary(_) | Message::Pong(_) => Ok(()),
    }
}

async fn send(
    sender: &mut (impl futures_util::Sink<Message> + Unpin),
    text: String,
) -> Result<(), ()> {
    sender.send(Message::Text(text)).await.map_err(|_| ())
}
