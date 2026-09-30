FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p operator-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/operator-service /app/operator-service
EXPOSE 9000
CMD ["/app/operator-service"]
