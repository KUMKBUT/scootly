FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p bot-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/bot-service /app/bot-service
EXPOSE 9000
CMD ["/app/bot-service"]
