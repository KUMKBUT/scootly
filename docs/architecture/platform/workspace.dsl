workspace "Scootly Platform" "Контейнеры и компоненты бэкенда: 10 микросервисов, данные, K8s" {

    model {
        user = person "Пользователь" "Арендует самокат через Telegram Mini App"
        operator = person "Оператор" "Замена АКБ, задачи, закрытие смены (RN-приложение, offline-first)"
        renterBot = person "Бот-арендатор" "Эмулирует пользователей, создаёт обращения в поддержку" "Bot"
        supportAdmin = person "Админ поддержки" "Тикеты, ручные возвраты" "Support"

        telegramPlatform = softwareSystem "Telegram Platform" "Хостинг Mini App, Bot API для уведомлений" "External"
        yooKassa = softwareSystem "YooKassa" "Эквайринг: холд/capture, рекурренты, webhooks" "External"
        scooterIoT = softwareSystem "Scooter IoT Device" "Эмулированные самокаты: GPS, замок, АКБ" "External,IoT" {
            mqttGateway = container "MQTT Gateway" "Приём телеметрии, команды unlock/lock. Эмулятор; в проде — EMQX cluster / HiveMQ (ADR-0009)" "MQTT QoS 1, эмулятор" "IoT,Emulated"
        }

        rentalPlatform = softwareSystem "Scooter Rental Platform" "Бэкенд аренды самокатов: Rust + Axum в Kubernetes" {

            miniapp = container "Telegram Mini App" "Карта, бронь 5 мин, аренда, оплата" "React + TS + Tailwind" "Frontend"
            operatorApp = container "Operator App" "Задачи АКБ, QR + фото, ShiftReport/Inventory/Issues, offline-first" "React Native" "Frontend"
            # Граница авторизации: JWT валидируется в каждом сервисе (crates/common::auth);
            # nginx — только TLS + rate limiting (ADR-0007).
            nginx = container "Ingress (NGINX)" "TLS-терминация, rate limiting, маршрутизация (без авторизации)" "NGINX Ingress" "Infra"

            authService = container "Auth Service" "Telegram init-data в JWT, сессии, выдача/ротация API-ключей ботов" "Rust + Axum" "Microservice"
            rentalService = container "Rental Service" "Бронь, старт/финиш, тарифы, outbox" "Rust + Axum" "Microservice" {
                rentalLifecycle = component "Rental Lifecycle" "start/finish, холд в capture, команды замку" "Rust"
                # Глоссарий: лимит 1+1 = 1 бронь + 1 аренда на юзера.
                bookingManager = component "Booking Manager" "Бронь 5 мин (TTL), лимит 1+1" "Rust"
                tariffEngine = component "Tariff Engine" "Минуты/пакеты, подписка −20%/−15%" "Rust"
                outboxRelay = component "Outbox Relay" "Читает outbox, публикует в Kafka" "Rust"
            }
            scooterService = container "Scooter Service" "Парк, статусы, позиции, батарея" "Rust + Axum" "Microservice"
            geoService = container "Geo Service" "Геозоны, nearby, радиус оператора 10 м" "Rust + Axum" "Microservice"
            paymentService = container "Payment Service" "Холд/capture (idempotency_key = ride_id), circuit breaker на YooKassa, подписка 199₽/мес + trial, webhooks" "Rust + Axum" "Microservice"
            operatorService = container "Operator Service" "Автозадачи при заряде <20%, смены, инвентарь АКБ" "Rust + Axum" "Microservice"
            supportService = container "Support Service" "Тикеты, ручные возвраты" "Rust + Axum" "Microservice"
            botService = container "Bot Service" "100 эмуляторов, случайное поведение, эмуляция платежей" "Rust + Axum" "Microservice"
            telemetryService = container "Telemetry Service" "MQTT-подписка, дедуп msg_id (SETNX Redis TTL 5 мин), GEOADD позиций, поток телеметрии в Kafka" "Rust + Axum" "Microservice"
            notificationService = container "Notification Service" "Уведомления через Telegram Bot API" "Rust + Axum" "Microservice"

            postgres = container "PostgreSQL" "Source of truth (в т.ч. брони), по схеме на сервис" "PostgreSQL 16" "DataStore"
            redis = container "Redis" "Только TTL-триггер брони + GEO-кэш + сессии + rate limiting" "Redis 7 + pipeline" "DataStore"
            # Топики версионируются: <name>.v1, backward-compatible only (ADR-0008).
            # Avro-схемы — в crates/proto/avro.
            # DLQ: у каждого consumer-топика парный <topic>.v1.dlq, retry 3x backoff → DLQ (ADR-0010).
            kafka = container "Kafka" "Доменные события, KRaft-режим" "Kafka" "Messaging"
            objectStorage = container "MinIO (S3)" "Фото замены АКБ: сырые 90 дней, thumbnails бессрочно" "S3-compatible" "DataStore"
            prometheus = container "Prometheus" "Scrape /metrics всех сервисов" "Prometheus" "Monitoring"
            grafana = container "Grafana" "Дашборды, алерты в Telegram-бота" "Grafana" "Monitoring"

            user -> miniapp "Открывает карту, бронирует, оплачивает" "Telegram WebView (HTTPS)"
            user -> telegramPlatform "Открывает бота и Mini App" "Telegram"
            operator -> operatorApp "Берёт задачи, сканирует QR, грузит фото" "HTTPS"
            renterBot -> botService "Эмулирует аренду и ошибки" "HTTPS + API Key"
            # ASSUMPTION: консоль поддержки — роль в Operator App, отдельного фронта нет.
            supportAdmin -> operatorApp "Обрабатывает тикеты (роль поддержки)" "HTTPS"

            miniapp -> nginx "REST API" "HTTPS/JSON"
            operatorApp -> nginx "REST API" "HTTPS/JSON"
            botService -> nginx "Вызывает API как клиент" "HTTPS/JSON + API Key"
            nginx -> authService "Маршрутизация /auth" "HTTPS/JSON"
            nginx -> rentalService "Маршрутизация /rentals" "HTTPS/JSON"
            nginx -> scooterService "Маршрутизация /scooters" "HTTPS/JSON"
            nginx -> geoService "Маршрутизация /geo" "HTTPS/JSON"
            nginx -> paymentService "Маршрутизация /payments" "HTTPS/JSON"
            nginx -> supportService "Маршрутизация /tickets" "HTTPS/JSON"
            nginx -> operatorService "Маршрутизация /tasks" "HTTPS/JSON"

            authService -> telegramPlatform "Проверка init-data" "HTTPS (Bot API)"
            authService -> postgres "Пользователи, refresh-сессии" "SQL"
            authService -> redis "Blacklist токенов, rate limiting, кэш API-ключей ботов (TTL 5 мин)" "Redis"

            # Бронь: PostgreSQL — source of truth, Redis — только TTL-триггер + фоновый джоб сверки.
            rentalService -> redis "Бронь 5 мин (TTL), лимиты 1+1" "Redis"
            rentalService -> postgres "Аренды, брони, outbox" "SQL"
            rentalService -> paymentService "Холд и capture (idempotency key)" "gRPC"
            rentalService -> mqttGateway "Команды unlock/lock" "MQTT (QoS 1)"
            rentalService -> kafka "rental.events.v1, booking.expired.v1" "Kafka"

            rentalLifecycle -> bookingManager "Проверяет бронь и лимиты" "In-process call"
            rentalLifecycle -> tariffEngine "Рассчитывает стоимость" "In-process call"
            rentalLifecycle -> outboxRelay "Сохраняет события в outbox" "In-process call"
            rentalLifecycle -> postgres "Аренды и брони" "SQL"
            rentalLifecycle -> mqttGateway "Unlock/lock" "MQTT (QoS 1)"
            bookingManager -> redis "TTL брони 5 мин" "Redis"
            tariffEngine -> postgres "Читает тарифы, пакеты, подписки" "SQL"
            outboxRelay -> postgres "Читает outbox" "SQL"
            outboxRelay -> kafka "Публикует события (<topic>.v1)" "Kafka"

            scooterService -> postgres "Парк, статусы, АКБ" "SQL"
            scooterService -> redis "Кэш позиций города (GEO, pipeline)" "Redis"
            geoService -> postgres "Полигоны геозон" "SQL"
            geoService -> redis "Проверка зон, nearby" "Redis GEO"
            # Fallback nearby: пустой/недоступный GEO-кэш → last_position через scooter-service
            # (владелец таблицы, database-per-service не нарушаем). SLO: p95 < 500 мс от MQTT до Redis GEO (ADR-0011).
            geoService -> scooterService "Fallback: last_position при пустом GEO-кэше" "gRPC"
            operatorService -> geoService "Проверка радиуса 10 м" "gRPC"

            paymentService -> postgres "Платежи, подписки, trial" "SQL"
            paymentService -> yooKassa "Холд/capture, рекурренты, возвраты" "HTTPS/REST"
            yooKassa -> paymentService "Webhooks (возможен out-of-order, разбор по статусу)" "HTTPS"
            paymentService -> kafka "payment.events.v1" "Kafka"

            telemetryService -> mqttGateway "Подписка на телеметрию" "MQTT"
            telemetryService -> redis "Дедуп msg_id (SETNX, TTL 5 мин, QoS 1), GEOADD позиций (SLO p95 < 500 мс от MQTT)" "Redis"
            telemetryService -> kafka "scooter.telemetry.v1" "Kafka"
            kafka -> scooterService "Обновление позиций и статусов" "Kafka"

            operatorService -> postgres "Задачи, смены, инвентарь АКБ" "SQL"
            operatorService -> objectStorage "Сохраняет пути фото в задачах" "HTTPS (S3)"
            operatorService -> kafka "task.assigned.v1, task.completed.v1, shift.closed.v1" "Kafka"
            kafka -> operatorService "Заряд <20%, задачи диспетчера" "Kafka"
            operatorApp -> objectStorage "Загрузка фото замены" "HTTPS (S3)"

            botService -> kafka "support.ticket.created.v1" "Kafka"
            kafka -> supportService "Тикеты от ботов и пользователей" "Kafka"
            supportService -> postgres "Тикеты, ручные возвраты" "SQL"
            supportService -> paymentService "Ручной возврат" "gRPC"

            kafka -> notificationService "Триггеры: аренда, бронь, задачи" "Kafka"
            notificationService -> telegramPlatform "Сообщения через Bot API" "HTTPS"

            # Бизнес-метрики: rides_started_total, unlock_failed_total, battery_low_total (ADR-0014).
            prometheus -> authService "Scrape /metrics" "HTTP"
            prometheus -> rentalService "Scrape /metrics" "HTTP"
            prometheus -> scooterService "Scrape /metrics" "HTTP"
            prometheus -> geoService "Scrape /metrics" "HTTP"
            prometheus -> paymentService "Scrape /metrics" "HTTP"
            prometheus -> operatorService "Scrape /metrics" "HTTP"
            prometheus -> supportService "Scrape /metrics" "HTTP"
            prometheus -> botService "Scrape /metrics" "HTTP"
            prometheus -> telemetryService "Scrape /metrics" "HTTP"
            prometheus -> notificationService "Scrape /metrics" "HTTP"
            grafana -> prometheus "Дашборды" "PromQL (HTTP)"
        }

        deploymentEnvironment "Production" {
            deploymentNode "Kubernetes Cluster" "Managed K8s, один регион" "Kubernetes" {
                deploymentNode "Ingress Namespace" "Входной трафик, TLS, rate limiting" "NGINX Ingress" {
                    ingressInstance = containerInstance nginx
                }
                deploymentNode "Worker Node Pool" "Пул нод под микросервисы (запас до 50 RPS)" "Kubernetes Deployment" {
                    authPod = containerInstance authService
                    rentalPod = containerInstance rentalService
                    scooterPod = containerInstance scooterService
                    geoPod = containerInstance geoService
                    paymentPod = containerInstance paymentService
                    operatorPod = containerInstance operatorService
                    supportPod = containerInstance supportService
                    botPod = containerInstance botService
                    telemetryPod = containerInstance telemetryService
                    notificationPod = containerInstance notificationService
                }
                deploymentNode "Data Layer (managed)" "Управляемые Postgres/Redis/Kafka/S3" "Managed services" {
                    pgInstance = containerInstance postgres
                    redisInstance = containerInstance redis
                    kafkaInstance = containerInstance kafka
                    minioInstance = containerInstance objectStorage
                }
                deploymentNode "Monitoring Namespace" "Метрики и дашборды" "Prometheus + Grafana" {
                    promInstance = containerInstance prometheus
                    grafanaInstance = containerInstance grafana
                }
            }
        }
    }

    views {
        container rentalPlatform "Containers" "Микросервисы, клиенты и инфраструктура" {
            include *
            autoLayout
        }
        component rentalService "RentalComponents" "Ядро аренды: lifecycle, бронь, тарифы, outbox" {
            include *
            autoLayout
        }
        dynamic rentalPlatform "UnlockScooter" "Разблокировка самоката" {
            user -> miniapp "Открывает карту, бронирует, оплачивает"
            miniapp -> nginx "REST API"
            nginx -> rentalService "Маршрутизация /rentals"
            rentalService -> paymentService "Холд и capture (idempotency key)"
            rentalService -> mqttGateway "Команды unlock/lock"
            rentalService -> postgres "Аренда + INSERT outbox (одна транзакция)"
            rentalService -> kafka "Публикует события через outbox"
            kafka -> notificationService "Триггеры: аренда, бронь, задачи"
            notificationService -> telegramPlatform "Сообщения через Bot API"
            autoLayout
        }
        dynamic rentalPlatform "UnlockFailed" "Замок не подтвердил unlock за 10 сек: void холда (ADR-0006)" {
            user -> miniapp "Жмёт «Начать аренду»"
            miniapp -> nginx "REST API"
            nginx -> rentalService "Маршрутизация /rentals"
            rentalService -> postgres "RideAttempt: pending (статус-машина попытки)"
            rentalService -> paymentService "Холд (idempotency key)"
            rentalService -> mqttGateway "Unlock; ack не пришёл за 10 сек"
            rentalService -> postgres "RideAttempt: pending → failed + INSERT outbox"
            rentalService -> paymentService "Void холда (идемпотентно)"
            rentalService -> kafka "Публикует unlock_failed через outbox"
            kafka -> notificationService "Триггер: аренда не началась"
            notificationService -> telegramPlatform "Сообщение: холд снят"
            autoLayout
        }
        dynamic rentalPlatform "BatteryReplace" "Замена аккумулятора оператором" {
            kafka -> operatorService "Заряд <20%, задачи диспетчера"
            operatorService -> kafka "task.assigned.v1, task.completed.v1, shift.closed.v1"
            operator -> operatorApp "Берёт задачи, сканирует QR, грузит фото"
            operatorApp -> nginx "REST API"
            nginx -> operatorService "Маршрутизация /tasks"
            operatorService -> geoService "Проверка радиуса 10 м"
            operatorService -> postgres "Задачи, смены, инвентарь АКБ"
            operatorApp -> objectStorage "Загрузка фото замены (presigned S3, client_op_id)"
            operatorService -> kafka "task.assigned.v1, task.completed.v1, shift.closed.v1"
            kafka -> notificationService "Триггеры: аренда, бронь, задачи"
            autoLayout
        }
        dynamic rentalPlatform "BotSupportTicket" "Бот создаёт обращение в поддержку" {
            renterBot -> botService "Эмулирует аренду и ошибки"
            botService -> kafka "support.ticket.created.v1"
            kafka -> supportService "Тикеты от ботов и пользователей"
            supportService -> postgres "Тикеты, ручные возвраты"
            supportAdmin -> operatorApp "Обрабатывает тикеты (роль поддержки)"
            operatorApp -> nginx "REST API"
            nginx -> supportService "Маршрутизация /tickets"
            kafka -> notificationService "Триггеры: аренда, бронь, задачи"
            notificationService -> telegramPlatform "Сообщения через Bot API"
            autoLayout
        }
        deployment * Production "ProdK8s" "Production: K8s-кластер, пул воркеров, managed data layer" {
            include *
            autoLayout
        }
        styles {
            element "Element" {
                background #1f2937
                color #e6edf3
                stroke #30363d
            }
            element "Software System" {
                background #1f2937
                color #e6edf3
                stroke #30363d
            }
            element "Person" {
                shape Person
                background #1e3a5f
                color #e6edf3
                stroke #30363d
            }
            element "Bot" {
                shape Robot
                background #1e3a5f
                color #e6edf3
                stroke #30363d
            }
            element "Support" {
                shape Person
                background #1e3a5f
                color #e6edf3
                stroke #30363d
            }
            element "External" {
                shape Box
                background #6e40c9
                color #e6edf3
                stroke #30363d
            }
            element "IoT" {
                shape Cylinder
                background #7d4e00
                color #e6edf3
                stroke #30363d
            }
            element "Container" {
                background #0d419d
                color #e6edf3
                stroke #58a6ff
            }
            element "Frontend" {
                shape WebBrowser
            }
            element "Microservice" {
                shape RoundedBox
            }
            element "Infra" {
                shape RoundedBox
                background #1f2937
                stroke #30363d
            }
            element "Monitoring" {
                shape RoundedBox
                background #1f2937
                stroke #30363d
            }
            element "DataStore" {
                shape Cylinder
                background #7d4e00
                color #e6edf3
                stroke #30363d
            }
            element "Messaging" {
                shape Pipe
                background #7d4e00
                color #e6edf3
                stroke #30363d
            }
            element "Component" {
                shape Component
                background #1a7f37
                color #e6edf3
                stroke #3fb950
            }
            relationship "Relationship" {
                color #8b949e
                thickness 2
            }
        }
    }
}
