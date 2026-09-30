FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p notification-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/notification-service /app/notification-service
EXPOSE 9000
CMD ["/app/notification-service"]
