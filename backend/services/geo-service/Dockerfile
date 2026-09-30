FROM rust:1-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p geo-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/geo-service /app/geo-service
EXPOSE 9000
CMD ["/app/geo-service"]
