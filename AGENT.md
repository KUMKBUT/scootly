# Scootly — Agent Guide

Пет-проект **Scootly** — приложение аренды самокатов (аналог Urent).
Пользователь открывает **Telegram Mini App** (или мобильное приложение), видит самокаты рядом на карте,
начинает/завершает аренду, оплачивает картой (YooKassa: холд → capture).

Технологии:
- Backend: **Rust + Axum** (микросервисы), Cargo workspace (`backend/`)
- Frontend: **Telegram Mini App** (React + TS + Vite + Tailwind), **React Native + TS**, общий пакет `shared`
- Инфра: **PostgreSQL, Redis (pipeline), Kafka, Nginx/K8s, Prometheus + Grafana**
- Документация: **Structurizr DSL (C4), OpenAPI, AsyncAPI, ADR**
- Монорепо: **pnpm workspaces** (frontend) + **Cargo workspaces** (backend)

Этот файл — обязательная инструкция для AI-ассистентов (OpenCode, Cursor, Claude Code, Aider и др.).

## 🗺️ Карта репозитория

| Путь | Назначение | Куда смотреть |
|---|---|---|
| `backend/services/*` | Rust-микросервисы на Axum (auth, rental, scooter, operator, support, bot, payment, geo, telemetry, notification) | `backend/services/rental-service/src/main.rs` |
| `backend/crates/*` | Общие библиотеки (common, db, kafka, redis-client, proto) | `backend/crates/db/src/lib.rs` |
| `backend/tools/seeder` | Сидер тестовых данных | `backend/tools/seeder/src/main.rs` |
| `frontend/miniapp` | Telegram Mini App (React + Vite + Tailwind) | `frontend/miniapp/src/` |
| `frontend/mobile` | React Native приложение (Expo) | `frontend/mobile/src/` |
| `frontend/shared` | Общие типы и API-клиент | `frontend/shared/src/` |
| `infra/nginx` | Nginx-конфиги | `infra/nginx/nginx.conf` |
| `infra/k8s` | Kubernetes-манифесты (deployments, services, ingress, kustomize, helm) | `infra/k8s/` |
| `infra/nginx/conf.d/rate-limit.conf` | Rate limiting: public 100 RPS/юзер (fallback 300 RPS/IP), auth 10, bots 500/key | `infra/nginx/conf.d/` |
| `infra/minio/lifecycle.json` | S3 lifecycle: сырые фото 90 дней, thumbnails бессрочно | `infra/minio/` |
| `infra/monitoring` | Prometheus, Grafana, Loki | `infra/monitoring/prometheus/prometheus.yml` |
| `infra/terraform` | Terraform-заготовки | `infra/terraform/main.tf` |
| `docs/architecture/landscape/workspace.dsl` | System Context (юзер + платформа + внешние) | `docs/architecture/landscape/` |
| `docs/architecture/platform/workspace.dsl` | Контейнеры, компоненты, dynamic, deployment | `docs/architecture/platform/` |
| `docs/architecture/operator-app/workspace.dsl` | RN-приложение оператора | `docs/architecture/operator-app/` |
| `docs/architecture/adr` | ADR 0001–0014 (axum, mqtt, hold-capture, operator-rn, kafka-retention, unlock-fail, auth-boundary, schema-versioning, mqtt-dedup, dlq, geo-slo, offline-idempotency, telegram-risk, resilience) | `docs/architecture/adr/` |
| `docs/api` | OpenAPI/AsyncAPI | `docs/api/openapi.yaml`, `docs/api/asyncapi.yaml` |
| `docs/db/schema.sql` | Эталонная схема БД | `docs/db/schema.sql` |
| `scripts/` | Обёртки dev/migrate/seed/gen-openapi | `scripts/dev.sh` |
| `.github/workflows` | CI (backend, frontend, deploy) | `.github/workflows/` |
| `tests/` | e2e / load / integration заглушки | `tests/` |

Пример дерева:

```text
backend/services/rental-service/{Cargo.toml,Dockerfile,src/main.rs}
backend/crates/db/{Cargo.toml,src/lib.rs,migrations/}
frontend/miniapp/src/{main.tsx,App.tsx,features/,components/,pages/}
```

## 📐 Правила архитектуры

- **Никаких прямых SQL-запросов вне `crates/db`** — только через репозитории.
  Пример: сервис вызывает `db::rentals::create(...)`, а не `sqlx::query!` напрямую.
- **Сервисы общаются через Kafka** (события, см. `docs/api/asyncapi.yaml`) или gRPC (синхронно),
  **не через общую БД**. Чужие таблицы не читаем.
- **Публикация событий — паттерн Outbox**: запись в таблицу `outbox` в одной транзакции
  с бизнес-данными, отдельный воркер публикует в Kafka. Схема — `docs/db/schema.sql`.
- **Redis pipeline** — используем `redis::pipe()` при батчевых операциях.
  Пример:
  ```rust
  let mut pipe = redis::pipe();
  pipe.cmd("GEOADD").arg("scooters").arg(lon).arg(lat).arg(id);
  pipe.cmd("EXPIRE").arg(format!("scooter:{id}")).arg(60);
  pipe.query_async(&mut conn).await?;
  ```
- **Секреты — только через env**, никогда в коде. Пример: `std::env::var("JWT_SECRET")`, локально — `.env` (см. `.env.example`).
- **Каждый сервис** имеет `Dockerfile` и health-check `/health`:
  ```rust
  let app = Router::new().route("/health", get(health));
  ```
- **Ошибки** — через `thiserror`, доменные ошибки в `common::AppError`.
- **PostgreSQL — source of truth, в т.ч. для брони.** Redis — только TTL-триггер
  автоснятия + GEO-кэш + сессии. Расхождение закрывает фоновый джоб сверки (см. ADR-0003).
- **Бронирование без race:** только `UPDATE ... WHERE status='available' RETURNING id`,
  никаких SELECT-then-UPDATE. Вторая линия — UNIQUE-ограничения в `docs/db/schema.sql`.
- **Идемпотентность платежей:** capture с ключом `ride:{rental_id}` (`UNIQUE idempotency_key` в БД,
  ADR-0003 в ред. ADR-0014), hold — `hold:{rental_id}`, сверка раз в 5 мин, неудачи — в DLQ
  (retry 3x backoff, ADR-0010).
- **Kafka-топики версионируются:** `<name>.v1`, Avro-схемы в `backend/crates/proto/avro/`,
  backward-compatible only (ADR-0008). У consumer-топика — парный `<topic>.v1.dlq` (ADR-0010).
- **MQTT QoS 1 — дедуп на консюмере:** `msg_id` от устройства → `SETNX dedup:{msg_id}`
  в Redis, TTL 5 мин (ADR-0009).
- **Graceful shutdown обязателен:** `axum::serve().with_graceful_shutdown()` + добить батч
  в воркерах (ADR-0014). Бизнес-метрики: `rides_started_total`, `unlock_failed_total`,
  `battery_low_total`.
- **Офлайн-операции оператора** — только с `client_op_id` (UUID), `UNIQUE` на бэке;
  presigned URL фото — TTL 24 ч (ADR-0012).
- **PII — через `common::telemetry`**: телефоны (`mask_phone`) и координаты (`mask_coord`)
  маскируются до `tracing!`. Сырой `phone` в логах запрещён (ловит CI).
- **Workspace делим при >40 элементах модели.** Сейчас: `landscape/` (контекст),
  `platform/` (контейнеры/компоненты), `operator-app/` (RN). IoT-эмуляция помечена
  тегом `Emulated` в DSL.
  Пример:
  ```rust
  use common::AppError;
  fn unlock(id: &str) -> Result<(), AppError> {
      if id.is_empty() { return Err(AppError::Validation("empty id".into())); }
      Ok(())
  }
  ```

## 🦀 Rust: стиль и конвенции

- Edition 2021, `rustfmt` + `clippy` (deny warnings). Перед PR:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace -- -D warnings
  ```
- Axum-роуты: `handlers/` → `services/` (бизнес-логика) → `models/` (sqlx).
  Пока в сервисах только `src/main.rs` с `/health` — при росте раскладывать по модулям так.
- Именование: `snake_case` для функций/файлов, `PascalCase` для типов. Пример: `fn start_rental()`, `struct StartRental`.
- Логирование — `tracing` с `#[instrument]`, никаких `println!`:
  ```rust
  #[tracing::instrument]
  async fn start_rental(scooter_id: uuid::Uuid) { tracing::info!(%scooter_id, "rental started"); }
  ```
- Метрики — `metrics` crate, экспортер Prometheus (порт `9000`, scrape — `infra/monitoring/prometheus/prometheus.yml`).
- Тесты: unit в `#[cfg(test)]` внутри модуля, integration в `tests/`.
- Новые крейты добавляем в `backend/Cargo.toml` → `[workspace] members`:
  ```toml
  members = ["crates/common", "services/my-new-service"]
  ```
  и в `[workspace.dependencies]` — общие версии зависимостей.

## ⚛️ Frontend: стиль и конвенции

- **FSD-подход**: `features/` (бизнес-фичи: `auth`, `map`, `rental`, `wallet`, `support`) → `components/` (UI-kit) → `pages/` (роутинг) → `lib/` (утилиты). Плюс `hooks/`, `store/`, `api/`, `styles/`.
- Стилизация — Tailwind, никаких inline-style. Пример:
  ```tsx
  <button className="rounded-xl bg-black px-4 py-2 text-white">Арендовать</button>
  ```
- Иконки — `lucide-react`:
  ```tsx
  import { MapPin } from 'lucide-react';
  ```
- Состояние — Zustand (глобальное) + TanStack Query (серверное). Пример:
  ```tsx
  const { data } = useQuery({ queryKey: ['scooters'], queryFn: fetchNearby });
  ```
- Типы для API — из `shared/src/types`, не дублировать. `miniapp` и `mobile` импортируют из `shared` (`workspace:*`).
- Telegram SDK — `@telegram-apps/sdk-react`. Инициализация только в `miniapp`.
- Компоненты: `PascalCase.tsx` (например, `ScooterCard.tsx`), хуки: `useXxx.ts` (например, `useGeolocation.ts`).
- pnpm для установки пакетов (не npm/yarn):
  ```bash
  cd frontend && pnpm install
  pnpm dev:miniapp
  ```

## 📚 Документация

- **System Design** — три workspace (Structurizr): `landscape/` (контекст),
  `platform/` (контейнеры, компоненты Rental, 3 dynamic, deployment),
  `operator-app/` (RN). Правило: >40 элементов модели → делить.
  Запуск: `make structurizr` (`:8081`), `make structurizr-platform` (`:8083`),
  `make structurizr-operator` (`:8084`).
- **ADR** — `docs/architecture/adr/NNNN-название.md` (0001 axum, 0002 mqtt,
  0003 hold-capture, 0004 operator-rn, 0005 kafka-retention, 0006 unlock-fail,
  0007 auth-boundary, 0008 schema-versioning, 0009 mqtt-dedup, 0010 dlq,
  0011 geo-slo, 0012 offline-idempotency, 0013 telegram-risk, 0014 resilience).
  Нумерация сквозная.
- **OpenAPI** — `docs/api/openapi.yaml`, генерируется из кода (`utoipa`, скрипт `scripts/gen-openapi.sh`).
- **AsyncAPI** — `docs/api/asyncapi.yaml` + `x-kafka-retention` / `x-kafka-dlq` / `x-kafka-durability`.
- **Схема БД** — `docs/db/schema.sql`, синхронизируется с миграциями `backend/crates/db/migrations/`.

## 🌿 Git workflow

- Ветки: `main` (прод), `dev` (интеграция), `feat/xxx`, `fix/xxx`. Пример: `feat/rental-unlock`, `fix/geo-search`.
- Коммиты — Conventional Commits: `feat(rental): add unlock endpoint`, `fix(auth): refresh telegram init-data`.
- PR — обязательный ревью + зелёный CI (`backend.yml`, `frontend.yml`).
- Не коммитить: `.env`, `target/`, `node_modules/`, секреты K8s (см. `.gitignore`).

## 🛠️ Частые команды

```bash
# Backend
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace

# Frontend
cd frontend && pnpm install
pnpm dev:miniapp
pnpm dev:mobile

# Инфра
make up            # поднять локальный стек
make migrate       # миграции БД
make seed          # тестовые данные
make backup        # pg_dump в S3 (MinIO)
make structurizr           # landscape на :8081
make structurizr-platform  # platform на :8083
make structurizr-operator  # operator-app на :8084
docker compose --profile docs up structurizr  # тот же landscape через compose

# Docker
docker compose up -d
docker compose logs -f rental-service

# Нагрузка (k6): бенчмарк разблокировки, цель p95 < 2 сек
k6 run --vus 50 --duration 2m tests/load/rental-unlock.js
```

Дополнительно: `./scripts/dev.sh`, `./scripts/migrate.sh`, `./scripts/seed.sh`, `./scripts/gen-openapi.sh` (все с `set -euo pipefail`).

## 🚫 Что НЕ делать

- ❌ Не добавлять новые зависимости без обсуждения (список «запрещённых» для фронта — moment.js, lodash full, axios; для бэка — actix-web, diesel).
- ❌ Не дублировать типы между `miniapp` и `mobile` — только через `shared`.
- ❌ Не ходить в БД одного сервиса из другого напрямую.
- ❌ Не логировать PII (телефоны, координаты) без маскирования.
- ❌ Не коммитить секреты, даже тестовые.
- ❌ Не менять DSL без ADR; не склеивать workspace обратно (правило >40 элементов).
- ❌ Не делать SELECT-then-UPDATE для брони/статусов — только `UPDATE..WHERE..RETURNING`.
- ❌ Не логировать сырые телефоны/точные координаты — только через `common::telemetry`.

## 🤖 Как работать AI-агенту в этом репо

Прямые инструкции:

- Перед изменением кода — прочитай соответствующий раздел архитектуры (`docs/architecture/{landscape,platform,operator-app}/workspace.dsl`, ADR, OpenAPI/AsyncAPI).
- Изменения схемы БД → обнови миграцию в `backend/crates/db/migrations/` + `docs/db/schema.sql` + создай ADR.
- Новый эндпоинт → добавь в `docs/api/openapi.yaml` + интеграционный тест в `tests/integration/`.
- Новое Kafka-событие → добавь в `docs/api/asyncapi.yaml` + топик в `docker-compose.yml` (если нужен) + тест Outbox-воркера.
- Перед PR — прогони `make lint && make test` (или `cargo clippy --workspace -- -D warnings` + `cargo test --workspace`).
- Если сомневаешься в архитектурном решении — создай ADR-заготовку `docs/architecture/adr/NNNN-название.md` и опиши варианты (Context / Decision / Consequences).
- Не добавляй бизнес-логику «втихую»: каркас сервисов — только Axum + `/health`, всё остальное — через ADR и тесты.
- Соблюдай FSD на фронте и слои `handlers → services → models` на бэке.

## 📎 Ссылки

- Structurizr: https://structurizr.com/dsl
- C4 model: https://c4model.com
- Axum: https://docs.rs/axum
- sqlx: https://docs.rs/sqlx
- ADR: https://adr.github.io
