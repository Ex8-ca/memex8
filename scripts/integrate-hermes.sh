#!/usr/bin/env bash
# Configure Hermes-Agent to use memex8 via webhooks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

MEMEX8_URL="${MEMEX8_URL:-http://localhost:8080}"
API_KEY="${MEMEX8_API_KEY:-$(grep MEMEX8_API_KEY .env 2>/dev/null | cut -d= -f2 || echo 'memex8-dev-key')}"
HERMES_CONFIG="${HERMES_CONFIG:-$HOME/.hermes/config.yaml}"

echo "🧠 Configuring Hermes-Agent → memex8 webhooks"
echo "   memex8 URL: $MEMEX8_URL"
echo "   Hermes config: $HERMES_CONFIG"
echo ""

# Create Hermes config directory
mkdir -p "$(dirname "$HERMES_CONFIG")"

# Check if webhooks section exists
if grep -q "webhooks:" "$HERMES_CONFIG" 2>/dev/null || grep -q "hooks:" "$HERMES_CONFIG" 2>/dev/null; then
    echo "⚠️  hooks/webhooks section already exists in $HERMES_CONFIG"
    echo "   Add the following manually:"
    echo ""
    echo "webhooks:"
    echo "  on_conversation_end:"
    echo "    - url: ${MEMEX8_URL}/api/v1/webhooks/conversation"
    echo "      method: POST"
    echo "      headers:"
    echo "        Authorization: Bearer ${API_KEY}"
    echo ""
else
    # Add webhooks section
    cat >> "$HERMES_CONFIG" << EOF

webhooks:
  on_conversation_end:
    - url: ${MEMEX8_URL}/api/v1/webhooks/conversation
      method: POST
      headers:
        Authorization: Bearer ${API_KEY}
EOF
    echo "✅ Added webhooks section to $HERMES_CONFIG"
fi

echo ""
echo "📥 Hermes will now send conversation summaries to memex8"
echo ""
echo "   Restart Hermes for changes to take effect."
