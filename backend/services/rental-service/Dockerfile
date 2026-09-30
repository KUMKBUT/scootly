FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p rental-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/rental-service /app/rental-service
EXPOSE 9000
CMD ["/app/rental-service"]
