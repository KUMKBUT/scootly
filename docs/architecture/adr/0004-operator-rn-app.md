# 0004. Отдельное React Native приложение оператора

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Оператору нужны: работа без сети (паркинги/подвалы), камера (QR + фото АКБ),
фоновый GPS, отдельная авторизация (не Telegram). Mini App этого не даёт.

## Decision

Отдельное приложение в `frontend/mobile` (React Native, offline-first):
очередь операций + SQLite, фото — в MinIO (S3), путь — в задаче;
подтверждение замены — QR + фото + проверка радиуса 10 м (через geo-service);
конец смены — 3 экрана (ShiftReport, BatteryInventory, ScooterIssues),
событие `shift.closed` → Kafka. Консоль поддержки — роль в этом же приложении
(отдельного фронта нет). Детали — `operator-app/workspace.dsl`.

## Consequences

- Плюсы: offline-first, нативная камера/GPS, своя модель авторизации.
- Минусы: второй mobile-контур (сборки, signing, стора) — в бэклоге CI/CD для RN.
