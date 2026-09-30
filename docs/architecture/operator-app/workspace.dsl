workspace "Scootly Operator App" "RN-приложение оператора: задачи АКБ, offline-first, закрытие смены" {

    model {
        operator = person "Оператор" "Замена АКБ, задачи, закрытие смены"
        supportAdmin = person "Админ поддержки" "Тикеты (роль в приложении оператора)" "Support"

        # ASSUMPTION: детали RN-приложения спроектированы здесь впервые; бэкенд виден как чёрный ящик.
        rentalPlatform = softwareSystem "Scooter Rental Platform" "Бэкенд: задачи, смены, тикеты. Детали — platform/workspace.dsl" "External"
        telegramPlatform = softwareSystem "Telegram Platform" "Алерты диспетчеру через Bot API" "External"

        operatorMobileApp = softwareSystem "Operator Mobile App" "React Native, offline-first, камера, фоновый GPS" {
            shiftUI = container "Shift Screens" "ShiftReport, BatteryInventory, ScooterIssues" "React Native" "Frontend"
            # Идемпотентность офлайна: client_op_id (UUID) на каждой операции → UNIQUE на бэке (ADR-0012).
            syncEngine = container "Sync Engine" "Очередь операций (SQLite), retry, client_op_id (UUID), сверка со сменой" "React Native" "Frontend"
            mediaCapture = container "Media Capture" "QR-идентификация, фото в MinIO" "React Native (Camera)" "Frontend"
            localDb = container "Local Store" "Задачи, черновики отчётов, очередь фото" "SQLite" "DataStore"
        }

        operator -> shiftUI "Задачи, QR, фото, 3 экрана смены" "Touch"
        operator -> mediaCapture "Сканирует QR самоката" "Camera"
        supportAdmin -> shiftUI "Обрабатывает тикеты (роль поддержки)" "Touch"
        shiftUI -> localDb "Читает задачи и отчёты" "SQL"
        shiftUI -> syncEngine "Отправляет операции" "In-process call"
        mediaCapture -> localDb "Кладёт фото в очередь загрузки" "SQL"
        mediaCapture -> syncEngine "Сигнал о новом фото" "In-process call"
        syncEngine -> rentalPlatform "Синхронизация: задачи, смены, фото (presigned S3 TTL 24 ч, client_op_id)" "HTTPS/JSON"
        rentalPlatform -> telegramPlatform "Отчёт смены диспетчеру" "Bot API"

        deploymentEnvironment "Production" {
            deploymentNode "Смартфон оператора" "Android / iOS, offline-first" "React Native (Expo)" {
                shiftInstance = containerInstance shiftUI
                syncInstance = containerInstance syncEngine
                mediaInstance = containerInstance mediaCapture
                dbInstance = containerInstance localDb
            }
        }
    }

    views {
        container operatorMobileApp "AppContainers" "Экраны, синхронизация, камера, локальное хранилище" {
            include *
            autoLayout
        }
        dynamic operatorMobileApp "CloseShift" "Закрытие смены: 3 экрана и shift.closed" {
            operator -> shiftUI "Задачи, QR, фото, 3 экрана смены"
            shiftUI -> localDb "Читает задачи и отчёты"
            shiftUI -> syncEngine "Отправляет операции"
            syncEngine -> rentalPlatform "Синхронизация: задачи, смены, фото (presigned S3)"
            rentalPlatform -> telegramPlatform "Отчёт смены диспетчеру"
            autoLayout
        }
        deployment * Production "AppDeployment" "Смартфон оператора (Android/iOS), offline-first" {
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
            element "Container" {
                background #0d419d
                color #e6edf3
                stroke #58a6ff
            }
            element "Frontend" {
                shape MobileDevicePortrait
            }
            element "DataStore" {
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
