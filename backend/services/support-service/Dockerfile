FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p support-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/support-service /app/support-service
EXPOSE 9000
CMD ["/app/support-service"]
