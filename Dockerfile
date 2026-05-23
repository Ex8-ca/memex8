# syntax=docker/dockerfile:1
FROM rust:slim-bookworm AS builder

WORKDIR /app

# Cache dependencies layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && echo "pub mod config; pub mod api; pub mod engine; pub mod integrations; pub mod mcp; pub mod storage; pub mod web;" > src/lib.rs \
    && cargo build --release 2>/dev/null || true \
    && rm -rf src

# Build actual project
COPY . .
RUN cargo build --release

# Runtime — minimal Debian slim
FROM debian:trixie-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/memex8 /usr/local/bin/memex8

RUN mkdir -p /usr/share/memex8/web

EXPOSE 8080 8081

VOLUME ["/var/lib/memex8", "/watch"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/api/v1/stats || exit 1

ENTRYPOINT ["memex8"]
CMD ["serve"]
