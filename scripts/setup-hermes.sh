#!/usr/bin/env bash
# setup-hermes.sh — Install memex8 plugin into Hermes and start Docker
set -euo pipefail

MEMEX8_DIR="$(cd "$(dirname "$0")/.." && pwd)"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_AGENT="${HERMES_AGENT:-$HERMES_HOME/hermes-agent}"
HERMES_PLUGINS="$HERMES_AGENT/plugins/memory"
HERMES_CONFIG="$HERMES_HOME/config.yaml"

# ─── Colors ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()  { echo -e "${GREEN}✓${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }
info() { echo -e "${CYAN}→${NC} $1"; }
err()  { echo -e "${RED}✗${NC} $1" >&2; }

echo ""
echo "🧠 memex8 → Hermes Integration Setup"
echo "====================================="
echo ""

# ─── Step 1: Check Docker ────────────────────────────────────────────────────
info "Checking Docker..."
if ! command -v docker &>/dev/null || ! docker compose version &>/dev/null 2>&1; then
    err "Docker and Docker Compose are required."
    echo "   Install: https://docs.docker.com/get-docker/"
    exit 1
fi
log "Docker OK"

# ─── Step 2: Find Hermes ─────────────────────────────────────────────────────
info "Looking for Hermes..."
if [ ! -d "$HERMES_AGENT" ]; then
    err "Hermes not found at $HERMES_AGENT"
    echo "   Set HERMES_AGENT env var to your hermes-agent directory."
    exit 1
fi
log "Hermes found at $HERMES_AGENT"

# ─── Step 3: Check/create .env ────────────────────────────────────────────────
if [ ! -f "$MEMEX8_DIR/.env" ]; then
    info "Creating .env from template..."
    cp "$MEMEX8_DIR/.env.example" "$MEMEX8_DIR/.env"
    echo ""
    warn "Edit .env and set your OPENAI_API_KEY:"
    echo "   nano $MEMEX8_DIR/.env"
    echo ""
    read -p "Press Enter when ready (or Ctrl-C to cancel)..."
fi

API_KEY="${MEMEX8_API_KEY:-$(grep '^MEMEX8_API_KEY=' "$MEMEX8_DIR/.env" 2>/dev/null | cut -d= -f2 || echo 'memex8-dev-key')}"
BASE_URL="${MEMEX8_BASE_URL:-http://localhost:8080}"

# ─── Step 4: Start Docker ────────────────────────────────────────────────────
info "Starting memex8 + Qdrant via Docker Compose..."
cd "$MEMEX8_DIR"

# Check if already running
if docker compose ps --format json 2>/dev/null | grep -q '"Status":"running"' 2>/dev/null; then
    log "memex8 is already running"
else
    docker compose up -d 2>&1 | tail -5

    # Wait for healthy (max 60s)
    info "Waiting for services..."
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8080/health &>/dev/null; then
            break
        fi
        echo -n "."
        sleep 2
    done
    echo ""
fi

if curl -sf http://localhost:8080/health &>/dev/null; then
    log "memex8 is running at $BASE_URL"
else
    warn "memex8 may still be starting. Check with: docker compose logs memex8"
fi

# ─── Step 5: Install plugin ──────────────────────────────────────────────────
info "Installing memex8 plugin into Hermes..."

if [ ! -d "$HERMES_PLUGINS" ]; then
    mkdir -p "$HERMES_PLUGINS"
    log "Created $HERMES_PLUGINS"
fi

# Remove old version if exists
rm -rf "$HERMES_PLUGINS/memex8"

# Copy plugin
cp -r "$MEMEX8_DIR/plugins/memex8" "$HERMES_PLUGINS/"
log "Plugin installed at $HERMES_PLUGINS/memex8"

# ─── Step 6: Configure Hermes ────────────────────────────────────────────────
info "Configuring Hermes..."

# Create config if it doesn't exist
if [ ! -f "$HERMES_CONFIG" ]; then
    touch "$HERMES_CONFIG"
fi

# Set MEMEX8_API_KEY in .env
HERMES_ENV="$HERMES_HOME/.env"
if [ -f "$HERMES_ENV" ]; then
    if grep -q "^MEMEX8_API_KEY=" "$HERMES_ENV" 2>/dev/null; then
        sed -i "s|^MEMEX8_API_KEY=.*|MEMEX8_API_KEY=$API_KEY|" "$HERMES_ENV"
    else
        echo "" >> "$HERMES_ENV"
        echo "MEMEX8_API_KEY=$API_KEY" >> "$HERMES_ENV"
    fi
    if grep -q "^MEMEX8_BASE_URL=" "$HERMES_ENV" 2>/dev/null; then
        sed -i "s|^MEMEX8_BASE_URL=.*|MEMEX8_BASE_URL=$BASE_URL|" "$HERMES_ENV"
    else
        echo "MEMEX8_BASE_URL=$BASE_URL" >> "$HERMES_ENV"
    fi
else
    cat > "$HERMES_ENV" << ENVEOF
MEMEX8_API_KEY=$API_KEY
MEMEX8_BASE_URL=$BASE_URL
ENVEOF
fi
log "Set MEMEX8_API_KEY in $HERMES_ENV"

# Set memory.provider in config.yaml
if command -v python3 &>/dev/null; then
    python3 -c "
import sys
try:
    import yaml
except ImportError:
    sys.exit(1)

config_path = '$HERMES_CONFIG'
try:
    with open(config_path) as f:
        cfg = yaml.safe_load(f) or {}
except Exception:
    cfg = {}

if 'memory' not in cfg:
    cfg['memory'] = {}
cfg['memory']['provider'] = 'memex8'
if 'memory_enabled' not in cfg['memory']:
    cfg['memory']['memory_enabled'] = True

with open(config_path, 'w') as f:
    yaml.dump(cfg, f, default_flow_style=False, sort_keys=False)
print('ok')
" 2>/dev/null
    if [ $? -ne 0 ]; then
        # Fallback: use sed
        if grep -q "^memory:" "$HERMES_CONFIG" 2>/dev/null; then
            # Update existing provider
            sed -i 's/^  provider:.*/  provider: memex8/' "$HERMES_CONFIG"
        else
            echo "" >> "$HERMES_CONFIG"
            echo "memory:" >> "$HERMES_CONFIG"
            echo "  provider: memex8" >> "$HERMES_CONFIG"
            echo "  memory_enabled: true" >> "$HERMES_CONFIG"
        fi
    fi
else
    # No python, use sed
    if grep -q "^memory:" "$HERMES_CONFIG" 2>/dev/null; then
        sed -i 's/^  provider:.*/  provider: memex8/' "$HERMES_CONFIG"
    else
        echo "" >> "$HERMES_CONFIG"
        echo "memory:" >> "$HERMES_CONFIG"
        echo "  provider: memex8" >> "$HERMES_CONFIG"
        echo "  memory_enabled: true" >> "$HERMES_CONFIG"
    fi
fi
log "Set memory.provider: memex8 in $HERMES_CONFIG"

# ─── Done ────────────────────────────────────────────────────────────────────
echo ""
echo "✅ All done!"
echo ""
echo "  🌐 Web UI:    $BASE_URL"
echo "  📂 Plugin:    $HERMES_PLUGINS/memex8"
echo "  ⚙️  Config:    $HERMES_CONFIG (memory.provider: memex8)"
echo ""
echo "Restart Hermes for the plugin to take effect."
echo ""
