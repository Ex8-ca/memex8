#!/bin/bash
# Rebuild memex8 from source and restart the container in place.
# - `--no-cache` forces a fresh compile (slow ~3-5min but ensures the binary matches the working tree)
# - `--force-recreate --no-deps` recreates only the memex8 container (qdrant stays put, no data loss)
set -euo pipefail
cd /home/marc/memex8
echo ">>> Building memex8 (no-cache, ~3-5 min)..."
docker compose build --no-cache memex8
echo ">>> Recreating memex8 container..."
docker compose up -d --force-recreate --no-deps memex8
echo ">>> Done. Check: docker logs memex8-memex8-1 --tail 10"
echo "EXIT:$?"
