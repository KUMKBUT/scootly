*Scootly — pet project, day 5*

За день закрыли задачи MVP 10–12: реальные замки по MQTT, YooKassa в staging и полный Docker Compose стек с healthcheck-зависимостями\.

*Этап 2 — интеграции и приёмка*
✅ 9\. Метрики → Grafana: Prometheus scrape и бизнес\-дашборды
✅ 10\. Реальные замки: MQTT unlock/lock, ack до 10 секунд, дедупликация `msg_id` в Redis
✅ 11\. YooKassa в staging: webhook `payment\.succeeded/canceled`, сверка каждые 5 минут
✅ 12\. Docker Compose: все сервисы \+ `outbox\-relay`, healthcheck\-зависимости, `make up`
⏭ 13\. DoD\-прогон на staging: полный e2e \+ k6 `p95 unlock < 2 c`

Этап 1 — платящий цикл 8/8 ✅ · Этап 2 — интеграции 4/5 ✅

Стек: Rust \+ Axum · Postgres · Redis · Kafka · MQTT · YooKassa · Docker Compose

\#rust \#axum \#postgres \#mqtt \#yookassa \#docker \#petproject
