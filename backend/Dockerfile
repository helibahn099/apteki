# Сборка
FROM rust:latest AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY seeds ./seeds
RUN cargo build --release --bin savvy-meds-backend

# Запуск
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 app
COPY --from=builder /app/target/release/savvy-meds-backend /usr/local/bin/savvy-meds-backend
USER app
ENV HTTP_HOST=0.0.0.0 HTTP_PORT=8080 APP_ENV=production LOG_FORMAT=json
EXPOSE 8080
CMD ["savvy-meds-backend"]
