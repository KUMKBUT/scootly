FROM rust:1-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p ws-gateway

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/ws-gateway /app/ws-gateway
EXPOSE 9000
CMD ["/app/ws-gateway"]
