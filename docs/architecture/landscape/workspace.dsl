workspace "Scootly Landscape" "Контекст: пользователи, платформа, Telegram, YooKassa, IoT" {

    model {
        user = person "Пользователь" "Арендует самокат через Telegram Mini App"
        operator = person "Оператор" "Замена АКБ, задачи, закрытие смены"
        renterBot = person "Бот-арендатор" "Эмулирует пользователей" "Bot"
        supportAdmin = person "Админ поддержки" "Тикеты, ручные возвраты" "Support"

        rentalPlatform = softwareSystem "Scooter Rental Platform" "Бэкенд аренды: Rust + Axum в Kubernetes. Детали — platform/workspace.dsl"
        telegramPlatform = softwareSystem "Telegram Platform" "Хостинг Mini App, Bot API" "External"
        yooKassa = softwareSystem "YooKassa" "Эквайринг: холд/capture, рекурренты" "External"
        scooterIoT = softwareSystem "Scooter IoT Device" "Эмулированные самокаты" "External,IoT,Emulated"

        user -> rentalPlatform "Арендует самокаты, бронирует, платит" "Telegram Mini App (HTTPS)"
        user -> telegramPlatform "Открывает бота и Mini App" "Telegram"
        operator -> rentalPlatform "Обслуживает парк: АКБ, задачи, смены" "Operator App (HTTPS)"
        renterBot -> rentalPlatform "Эмулирует аренду и ошибки" "HTTPS + API Key"
        supportAdmin -> rentalPlatform "Обрабатывает тикеты" "HTTPS"
        rentalPlatform -> telegramPlatform "Mini App, уведомления через Bot API" "HTTPS"
        rentalPlatform -> yooKassa "Холд/capture, рекурренты, возвраты" "HTTPS/REST"
        scooterIoT -> rentalPlatform "Телеметрия: GPS, батарея, замок" "MQTT (QoS 1)"
        rentalPlatform -> scooterIoT "Команды unlock/lock" "MQTT (QoS 1)"
    }

    views {
        systemContext rentalPlatform "SystemContext" "Scootly: платформа и её окружение" {
            include *
            autoLayout
        }

        styles {
            element "Element" {
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
            relationship "Relationship" {
                color #8b949e
                thickness 2
            }
        }
    }
}
