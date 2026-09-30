# 0008. Версионирование схем Kafka без Schema Registry

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Доменные события идут через Kafka (rental.events, payment.events, ...). Нужен
контракт производитель↔потребители. Confluent Schema Registry — отдельный
stateful-компонент и ещё одна точка отказа для пет-проекта.

## Decision

1. **Schema Registry не используем.** Avro-схемы лежат в git:
   `backend/crates/proto/avro/*.avsc`, Rust-типы генерируются на сборке.
2. **Версия — в имени топика**: `rental.events.v1`, `scooter.telemetry.v1` и т.д.
3. **Правило совместимости: backward-compatible only** внутри одной версии:
   новые поля — только optional с дефолтами; переименование/удаление/смена типа =
   новый топик `.v2` и период двойной публикации (старая + новая версия).

## Alternatives considered

- Confluent Schema Registry: проверка совместимости в рантайме, но +1 stateful
  сервис, клиентские библиотеки и операционный overhead.
- JSON без схем: контракт не проверяется вообще, ломается молча.

## Consequences

- Плюсы: контракты в code review, ноль рантайм-зависимостей, версия видна в
  метриках/логах по имени топика.
- Минусы: рассинхрон схемы и кода возможен — закрывается CI-джобой (генерация
  типов из `.avsc` на PR); миграция мажорной версии = двойная публикация.
