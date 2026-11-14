#!/usr/bin/env bash
# Throwaway memex8 + Qdrant for benchmarking. Never touches a live stack:
# own compose project, own volumes, own HOME, ports bound to 127.0.0.1 only.
#
#   MEMEX8_BENCH_ENV=/path/to/.env.memex8 instance.sh up <image> [extra.toml]
#   instance.sh down
#
# MEMEX8_BENCH_ENV supplies the embedding settings (OPENAI_API_KEY,
# EMBEDDING_*). Any MEMEX8_* lines in it are dropped and a fresh random
# MEMEX8_API_KEY is generated, so the bench never reuses a real key.
# extra.toml (optional) is appended to the bench config, e.g. a
# [search.hybrid] section for A/B runs.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
inst="$here/instance"
proj=memex8bench
port="${MEMEX8_BENCH_PORT:-18080}"

compose() {
  env HOME="$inst/home" docker compose -p "$proj" \
    -f "$repo/docker-compose.yml" -f "$inst/override.yml" "$@"
}

case "${1:-}" in
  up)
    image="${2:?usage: instance.sh up <image> [extra.toml]}"
    extra="${3:-}"
    envsrc="${MEMEX8_BENCH_ENV:?set MEMEX8_BENCH_ENV to an .env.memex8 with OPENAI_API_KEY}"
    mkdir -p "$inst/home/.memex8"
    umask 077
    grep -v '^MEMEX8_' "$envsrc" > "$inst/.env.memex8"
    echo "MEMEX8_API_KEY=$(python3 -c 'import secrets;print(secrets.token_urlsafe(32))')" >> "$inst/.env.memex8"
    # Slumber effectively off for the whole run: no dedup, verification,
    # re-clustering or consolidation changing rankings mid-benchmark.
    # The scheduler only parses "*/N" intervals, so */9999 is ~once a week.
    sed -e 's/^idle_timeout = .*/idle_timeout = "9999h"/' \
        -e 's/^cron_ingest = .*/cron_ingest = "*\/9999 * * * *"/' \
        -e 's/^consolidation_schedule = .*/consolidation_schedule = ""/' \
        "$repo/config.example.toml" > "$inst/config.toml"
    grep -q '^idle_timeout = "9999h"' "$inst/config.toml"
    grep -q '^consolidation_schedule = ""' "$inst/config.toml"
    if [ -n "$extra" ]; then printf '\n' >> "$inst/config.toml"; cat "$extra" >> "$inst/config.toml"; fi
    chmod 644 "$inst/config.toml"
    cat > "$inst/override.yml" <<EOF
services:
  memex8:
    image: $image
    env_file: !override
      - $inst/.env.memex8
    volumes: !override
      - $inst/config.toml:/config.toml:ro
      - memex8_data:/var/lib/memex8
      - $inst/home/.memex8:/home/memex8/.memex8
    ports: !override
      - "127.0.0.1:$port:8080"
EOF
    compose up -d --no-build >/dev/null 2>&1 || compose up -d --no-build
    for _ in $(seq 1 60); do
      if [ "$(docker inspect -f '{{.State.Health.Status}}' "${proj}-memex8-1" 2>/dev/null)" = healthy ]; then
        echo "bench instance up: http://127.0.0.1:$port ($image)"
        exit 0
      fi
      sleep 3
    done
    echo "bench instance did not become healthy; logs:" >&2
    docker logs --tail 30 "${proj}-memex8-1" >&2 || true
    exit 1 ;;
  down)
    if [ -f "$inst/override.yml" ]; then compose down -v --remove-orphans >/dev/null 2>&1 || true; fi
    # Docker-created files under the instance dir can be root-owned.
    [ -d "$inst" ] && docker run --rm -v "$inst:/w" alpine sh -c 'rm -rf /w/* /w/.[!.]*' >/dev/null 2>&1 || true
    echo "bench instance removed" ;;
  key)
    sed -n 's/^MEMEX8_API_KEY=//p' "$inst/.env.memex8" ;;
  *)
    echo "usage: instance.sh up <image> [extra.toml] | down | key" >&2
    exit 2 ;;
esac
