.PHONY: help up down logs migrate seed fmt lint test structurizr structurizr-platform structurizr-operator backup

STRUCTURZR_IMAGE := structurizr/structurizr@sha256:721136283c2f9cf1ba69037bc9de136c579d66fdcb2d771cb60546ec68def1a5
STRUCTURZR_USER := $(shell id -u):$(shell id -g)
STRUCTURZR_TMP := /tmp/scootly-structurizr

help: ## Показать доступные цели
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

up: ## Поднять полный стек (infra + все сервисы + outbox-relay)
	docker compose up -d --build

down: ## Остановить локальный стек
	docker compose down

logs: ## Показать логи всех сервисов
	docker compose logs -f

migrate: ## Применить миграции БД
	./scripts/migrate.sh

seed: ## Заполнить БД тестовыми данными
	./scripts/seed.sh

fmt: ## Форматирование (Rust + frontend)
	cargo fmt --all -- --check
	cd frontend && pnpm format || true

lint: ## Линт (clippy + eslint)
	cargo clippy --workspace -- -D warnings
	cd frontend && pnpm lint || true

test: ## Тесты backend + frontend
	cargo test --workspace
	cd frontend && pnpm test || true

structurizr: ## Structurizr (landscape) на :8081
	mkdir -p $(STRUCTURZR_TMP)/landscape && cp docs/architecture/landscape/workspace.dsl docs/architecture/landscape/structurizr.properties $(STRUCTURZR_TMP)/landscape/
	docker run --rm -p 8081:8080 --user $(STRUCTURZR_USER) -v "$(STRUCTURZR_TMP)/landscape:/usr/local/structurizr" $(STRUCTURZR_IMAGE) local

structurizr-platform: ## Structurizr (platform) на :8083
	mkdir -p $(STRUCTURZR_TMP)/platform && cp docs/architecture/platform/workspace.dsl docs/architecture/platform/structurizr.properties $(STRUCTURZR_TMP)/platform/
	docker run --rm -p 8083:8080 --user $(STRUCTURZR_USER) -v "$(STRUCTURZR_TMP)/platform:/usr/local/structurizr" $(STRUCTURZR_IMAGE) local

structurizr-operator: ## Structurizr (operator-app) на :8084
	mkdir -p $(STRUCTURZR_TMP)/operator-app && cp docs/architecture/operator-app/workspace.dsl docs/architecture/operator-app/structurizr.properties $(STRUCTURZR_TMP)/operator-app/
	docker run --rm -p 8084:8080 --user $(STRUCTURZR_USER) -v "$(STRUCTURZR_TMP)/operator-app:/usr/local/structurizr" $(STRUCTURZR_IMAGE) local

backup: ## Суточный бэкап PostgreSQL в S3
	./scripts/backup.sh
