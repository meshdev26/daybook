# Build stage
FROM rust:1.98 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./
COPY vendor ./vendor
COPY src ./src
COPY schema.sql ./
# Required by include_str!/include_bytes! in src/web/html.rs (app.css, fonts).
COPY assets ./assets
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/telegram-agent /usr/local/bin/telegram-agent
COPY schema.sql /app/schema.sql
RUN mkdir -p /app/data
EXPOSE 8080
ENV RUST_LOG=info
CMD ["telegram-agent"]
