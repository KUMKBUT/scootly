FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p telemetry-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/telemetry-service /app/telemetry-service
EXPOSE 9000
CMD ["/app/telemetry-service"]
