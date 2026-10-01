# Scootly — WebSocket-события (app ↔ сервер, MVP)

Канал реального времени между клиентом (Telegram Mini App / RN) и WS-шлюзом.
Шлюз — просто фан-аут: source of truth — PostgreSQL, события рождаются в сервисах
и идут через Kafka (`docs/api/asyncapi.yaml`). WS **не** источник истины — после
реконнекта клиент делает REST-снапшот.

## 1. Соединение и авторизация

```
wss://api.scootly.example/api/v1/ws?token=<access_jwt>
```

- JWT передаётся в query (Mini App не умеет ставить заголовки на WS); nginx терминирует TLS
  и проксирует Upgrade (rate-limit по юзеру, см. `infra/nginx/conf.d/rate-limit.conf`).
- Токен невалиден/просрочен → шлюз отвечает `error` с `code=unauthorized` и закрывает соединение (`4401`).
- Одно соединение на клиента; повторное с тем же юзером заменяет старое (старому — `close 4409`).
- Heartbeat: клиент шлёт `ping` каждые 25 c, шлюз отвечает `pong`; тишина 60 c → шлюз рвёт соединение.

## 2. Конверт сообщения

Все сообщения — JSON с общей обёрткой:

```json
{
  "type": "ride.started",
  "id": "b3c1a1e0-...-uuid",     // id события — для дедупликации на клиенте
  "ts": "2026-09-30T12:00:00Z",
  "payload": { ... }
}
```

- События `ride.*` и `payment.*` могут дублироваться (at-least-once) — клиент дедуплицирует по `id`.
- События карты (`scooter.*`) — at-most-once, дедуп не нужен, свежий снапшот важнее.

## 3. Client → Server

| type | payload | Назначение |
|------|---------|------------|
| `ping` | `{}` | Heartbeat |
| `subscribe.scooters` | `{"lat": 43.238, "lon": 76.889, "radius_m": 500}` | Подписка на поток событий карты вокруг точки (max radius 3000) |
| `unsubscribe.scooters` | `{}` | Отписаться от потока карты (экран карты скрыт) |
| `track.ride` | `{"ride_id": "uuid"}` | Подписка на события своей активной поездки (тики, финиш) |

События своей поездки/брони/платежа приходят автоматически после REST-авторизации —
`track.ride` нужен только после реконнекта, чтобы не пропустить `ride.finished`.

## 4. Server → Client

### Карта

| type | payload | Когда |
|------|---------|-------|
| `scooter.updated` | `{"id", "lat", "lon", "status", "battery_pct"}` | Статус/позиция/батарея изменились (`scooter.status-changed` из Kafka) |
| `scooter.removed` | `{"id", "reason": "offline\|out_of_zone"}` | Самокат исчез из выдачи |

### Бронь

| type | payload | Когда |
|------|---------|-------|
| `reservation.created` | `{"id", "scooter_id", "expires_at"}` | Бронь принята (подтверждение к `POST /reservations`) |
| `reservation.expiring_soon` | `{"id", "expires_at"}` | За 2 мин до TTL — таймер краснеет |
| `reservation.expired` | `{"id", "reason": "ttl\|canceled\|converted"}` | TTL истёк (Redis-триггер, сверка с PG — ADR-0003), отмена юзером или старт поездки |

### Поездка

| type | payload | Когда |
|------|---------|-------|
| `ride.started` | `{"ride_id", "scooter_id", "started_at"}` | Unlock подтверждён |
| `ride.tick` | `{"ride_id", "total_min", "amount_kopeks"}` | Каждые 30 c активной поездки — тикающий счётчик |
| `ride.finished` | `{"ride_id", "total_min", "amount_kopeks", "payment_id"}` | Лок закрыт, capture прошёл |
| `ride.unlock_failed` | `{"ride_id", "reason": "lock_ack_timeout", "hold_canceled": true}` | Замок не ответил за 10 c (ADR-0006) — холд снят, самокат в `offline` |

### Платёж

| type | payload | Когда |
|------|---------|-------|
| `payment.status_changed` | `{"payment_id", "ride_id", "status": "hold\|captured\|canceled\|refunded"}` | Вебхук YooKassa → сервис платежа → Kafka `payment.events.v1` |

### Служебные

| type | payload | Когда |
|------|---------|-------|
| `pong` | `{}` | Ответ на `ping` |
| `error` | `{"code", "message"}` | Ошибки протокола: `unauthorized`, `bad_message`, `rate_limited` |

## 5. Примеры

Клиент:

```json
{"type": "subscribe.scooters", "id": "c1", "ts": "2026-09-30T12:00:00Z",
 "payload": {"lat": 43.238, "lon": 76.889, "radius_m": 500}}
```

Сервер:

```json
{"type": "scooter.updated", "id": "8f2b...", "ts": "2026-09-30T12:00:01Z",
 "payload": {"id": "5a1e...", "lat": 43.2391, "lon": 76.8902,
             "status": "available", "battery_pct": 87}}
```

```json
{"type": "ride.unlock_failed", "id": "77aa...", "ts": "2026-09-30T12:05:10Z",
 "payload": {"ride_id": "d0be...", "reason": "lock_ack_timeout", "hold_canceled": true}}
```

## 6. Реконнект и целостность

1. Клиент рвёт связь → reconnect с backoff 1 c → 2 c → ... → 30 c (макс).
2. После reconnect: `subscribe.scooters` заново + REST-снапшоты (`GET /rides/{id}`, `GET /me`) —
   WS-пропуски не восстанавливаем, REST — истина.
3. Если у юзера была активная поездка/бронь — `track.ride` сразу после реконнекта.
4. Сервер не хранит очередь на отключившегося клиента; критичное (чек поездки) доступно через REST-историю.

## 7. Реализация (заметки для backend)

- Транспорт событий: Kafka `*.v1` → воркер WS-шлюза → Redis pub/sub на поды шлюза → сессии юзеров.
- **Статус (MVP #2):** шлюз `backend/services/ws-gateway` работает; fan-out —
  Redis pub/sub канал `ws:scooters`, сообщения — готовые конверты
  `scooter.updated` / `scooter.removed` (§4). Подписка фильтруется гео
  (haversine, радиус подписки, максимум 3000 м).
- **Статус (MVP #8):** мосты Kafka → Redis работают. Outbox → Kafka — релей
  `backend/tools/outbox-relay` (at-least-once: `published_at` — только после
  подтверждения доставки; внутренние `capture.retry.v1` / `void.retry.v1`
  не публикуются). Мост `backend/services/ws-gateway/src/bridge.rs`
  (consumer-group `ws-gateway`, `auto.offset.reset=latest`) превращает события
  в конверты §4: карта → `ws:scooters`, приватные события юзера → канал
  `ws:users` (`{"user_id", "event"}`) — доставляются только его сессии.
- Auth: `?token=` проверяется **до** upgrade — невалидный токен получает HTTP 401
  (`Error`-конверт), т.е. соединение не открывается. После upgrade действуют §1–§2.
- Публичная идентификация юзера в шлюзе — по `sub` из JWT; приватные события шлются только его сессии.
- События публикуются сервисами через Outbox (ADR-0008/0010), шлюз ничего не пишет в PG.
- Формализация — в `asyncapi.yaml` (protocol: `ws`) при имплементации шлюза.
