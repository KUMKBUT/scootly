*Scootly — pet project, day 1*

Начал пет\-проект: платформа аренды электросамокатов — от Telegram Mini App до MQTT\-телеметрии в Kubernetes\.

*Стек*
• Rust \+ Axum — микросервисы
• Kafka — события, outbox, DLQ
• MQTT QoS 1 — телеметрия, unlock/lock
• PostgreSQL \+ Redis \+ MinIO
• React Native — приложение оператора
• Prometheus \+ Grafana

*Что уже есть на day 1*
• C4\-модель в Structurizr DSL — 3 workspace
• 10 сервисов, 14 ADR: холд/capture в YooKassa, компенсация при неудачном unlock, QoS 1 \+ дедупликация, offline\-first у оператора
• Динамические сценарии: unlock, замена АКБ, тикет в поддержку

*В планах:* нагрузочные тесты через эмулятор самокатов и бота\-арендатора

Диаграммы: Structurizr DSL → PlantUML → PNG

\#rust \#architecture \#k8s \#c4model \#petproject
