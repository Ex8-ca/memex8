#!/usr/bin/env bash
# Choose which host interface memex8's published ports listen on
# (8080 web UI + REST API, 8081 MCP SSE).
#
#   set-bind.sh                 show the current setting and published ports
#   set-bind.sh local           127.0.0.1  (this machine only)
#   set-bind.sh lan             0.0.0.0    (every interface; the default)
#   set-bind.sh tailscale       this machine's Tailscale IPv4 only
#   set-bind.sh 192.168.1.3     one specific IPv4 address
#
# Add --apply to recreate the memex8 container now (about 15-30 s of
# downtime, data untouched). Without it the new value is only written to
# .env and takes effect at the next recreate.
#
# The app itself always binds 0.0.0.0 inside the container; binding 127.0.0.1
# there would make it unreachable even from the host. Exposure is controlled
# by this host-side publish address (MEMEX8_BIND_ADDR in the compose .env).
set -euo pipefail

DIR="${MEMEX8_COMPOSE_DIR:-$HOME/nvme-data/Documents/myprojs/memex8}"
ENVF="$DIR/.env"
CONTAINER="${MEMEX8_CONTAINER:-memex8-memex8-1}"

mode="${1:-}"
apply=0
[ "${2:-}" = "--apply" ] || [ "$mode" = "--apply" ] && apply=1

current() { grep -E '^MEMEX8_BIND_ADDR=' "$ENVF" 2>/dev/null | tail -1 | cut -d= -f2 || true; }

case "$mode" in
  ""|--apply)
    echo "MEMEX8_BIND_ADDR=$(current)  (empty = 0.0.0.0, all interfaces)"
    docker port "$CONTAINER" 2>/dev/null || echo "container $CONTAINER not running"
    exit 0 ;;
  local)     ADDR=127.0.0.1 ;;
  lan|all)   ADDR=0.0.0.0 ;;
  tailscale) ADDR="$(tailscale ip -4 2>/dev/null | head -1)"
             [ -n "$ADDR" ] || { echo "no Tailscale IPv4 found" >&2; exit 1; } ;;
  *)         ADDR="$mode"
             [[ "$ADDR" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || { echo "not an IPv4 address: $ADDR" >&2; exit 1; } ;;
esac

touch "$ENVF"
grep -v '^MEMEX8_BIND_ADDR=' "$ENVF" > "$ENVF.tmp" || true
echo "MEMEX8_BIND_ADDR=$ADDR" >> "$ENVF.tmp"
mv "$ENVF.tmp" "$ENVF"
echo "wrote MEMEX8_BIND_ADDR=$ADDR to $ENVF"

if [ "$apply" -ne 1 ]; then
  echo "not applied yet: re-run with --apply to recreate the container now"
  exit 0
fi

# Recreate with the exact compose stack the live container was started with.
IFS=',' read -ra FILES < <(docker inspect -f '{{index .Config.Labels "com.docker.compose.project.config_files"}}' "$CONTAINER")
ARGS=(); for f in "${FILES[@]}"; do ARGS+=(-f "$f"); done
cd "$DIR"
docker compose "${ARGS[@]}" up -d --no-deps --no-build --pull never --force-recreate --wait --wait-timeout 120 memex8
docker port "$CONTAINER"
