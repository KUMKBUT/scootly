# Контекст сборки — ./backend (workspace целиком, см. docker-compose.yml).
FROM rust:1-slim AS builder
WORKDIR /app
# cmake/g++/make — для rdkafka cmake-build (backend/Cargo.toml).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake g++ make pkg-config libssl-dev libcurl4-openssl-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p notification-service

FROM debian:trixie-slim
# ca-certificates — TLS (YooKassa/Telegram), curl — HEALTHCHECK /health.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/notification-service /app/notification-service
EXPOSE 9000
HEALTHCHECK --interval=10s --timeout=3s --start-period=15s --retries=5 \
  CMD curl -fsS http://127.0.0.1:9000/health || exit 1
CMD ["/app/notification-service"]
