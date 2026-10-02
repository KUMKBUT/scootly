# Контекст сборки — ./backend (workspace целиком, см. docker-compose.yml).
FROM rust:1-slim AS builder
WORKDIR /app
# cmake/g++/make — для rdkafka cmake-build (backend/Cargo.toml).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake g++ make pkg-config libssl-dev libcurl4-openssl-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p seeder

# One-shot джоб: отработал и вышел (docker compose --profile tools run --rm seeder).
FROM debian:trixie-slim
WORKDIR /app
COPY --from=builder /app/target/release/seeder /app/seeder
CMD ["/app/seeder"]
