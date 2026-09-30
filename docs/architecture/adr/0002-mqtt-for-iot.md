# 0002. MQTT для IoT-телеметрии и команд замку (draft, финал после MVP)

- Status: proposed
- Date: 2026-09-29
- Deciders: scootly team

## Context

Самокат должен слать телеметрию (GPS, батарея, замок) и принимать команды unlock/lock
с целевым latency разблокировки p95 < 2 сек. Кандидаты: MQTT, WebSocket, gRPC.
Вся IoT-сторона сейчас эмулирована (контейнер MQTT Gateway с тегом `Emulated`).

## Decision (предварительное, на MVP)

**MQTT, QoS 1**: лёгкий протокол, last-will для детекта офлайна, retained-сообщения
для последнего статуса, зрелые Rust-клиенты (`rumqttc`). Топики:
`scooter/{id}/telemetry`, `scooter/{id}/status`, `scooter/{id}/cmd`.

## Alternatives considered

- WebSocket: проще для эмулятора в браузере, но нет QoS/last-will из коробки.
- gRPC: хорош для команд, избыточен для потока телеметрии 6300 устройств.

## Consequences

- Плюсы: надёжная доставка, разрыв/переподключение из коробки, масштаб под 6300 устройств.
- Минусы: нужен брокер (эмулятор сейчас, реальный позже); финальный выбор — после MVP,
  отдельным ADR с бенчмарками (hold YooKassa vs MQTT round-trip).
