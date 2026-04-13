FROM rust:latest AS builder

WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "pub mod config;" > src/lib.rs
RUN cargo build --release 2>/dev/null || true

# Build actual project
COPY . .
RUN touch src/main.rs src/lib.rs
RUN cargo build --release

# Runtime
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/memex8 /usr/local/bin/memex8

# Web UI assets (built separately)
RUN mkdir -p /usr/share/memex8/web
# COPY web/dist/ /usr/share/memex8/web/

EXPOSE 8080 8081

VOLUME ["/var/lib/memex8", "/watch"]

ENTRYPOINT ["memex8"]
CMD ["serve"]
