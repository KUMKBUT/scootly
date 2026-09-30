FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p payment-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/payment-service /app/payment-service
EXPOSE 9000
CMD ["/app/payment-service"]
