FROM rust:1-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p scooter-service

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/scooter-service /app/scooter-service
EXPOSE 9000 9001
CMD ["/app/scooter-service"]
