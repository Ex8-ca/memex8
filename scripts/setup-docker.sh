#!/usr/bin/env bash
# memex8 Docker setup — one command to get running
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

echo "🧠 memex8 Docker Setup"
echo "======================"
echo ""

# Check Docker
if ! command -v docker &>/dev/null || ! docker compose version &>/dev/null; then
    echo "❌ Docker and Docker Compose are required."
    echo "   Install: https://docs.docker.com/get-docker/"
    exit 1
fi

# Copy .env if it doesn't exist
if [ ! -f .env ]; then
    echo "📝 Creating .env from template..."
    cp .env.example .env
    echo ""
    echo "⚠️  Edit .env and set your API keys:"
    echo "   nano .env"
    echo ""
    read -p "Press Enter when ready (or Ctrl-C to cancel)..."
fi

# Start everything
echo ""
echo "🚀 Starting memex8 + Qdrant..."
docker compose up -d

# Wait for health
echo ""
echo "⏳ Waiting for services to be healthy..."
sleep 5

if docker compose ps --format json | jq -r '.[] | select(.Health == "healthy") | .Name' | grep -q memex8; then
    echo ""
    echo "✅ memex8 is running!"
    echo ""
    echo "🌐 Web UI:   http://localhost:8080"
    echo "🔑 API Key:  $(grep MEMEX8_API_KEY .env | cut -d= -f2 || echo 'check your .env')"
    echo "📊 REST API: http://localhost:8080/api/v1/stats"
    echo ""
    echo "To stop: docker compose down"
else
    echo ""
    echo "⚠️  Services may still be starting. Check status:"
    echo "   docker compose ps"
    echo "   docker compose logs memex8"
fi
