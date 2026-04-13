#!/usr/bin/env bash
# Configure OpenClaw to send webhooks to memex8 on conversation end and skill execution
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

MEMEX8_URL="${MEMEX8_URL:-http://localhost:8080}"
API_KEY="${MEMEX8_API_KEY:-$(grep MEMEX8_API_KEY .env 2>/dev/null | cut -d= -f2 || echo 'memex8-dev-key')}"
OPENCLAW_CONFIG="${OPENCLAW_CONFIG:-$HOME/.openclaw/config.yaml}"

echo "🦞 Configuring OpenClaw → memex8 webhooks"
echo "   memex8 URL: $MEMEX8_URL"
echo "   OpenClaw config: $OPENCLAW_CONFIG"
echo ""

# Create OpenClaw config directory
mkdir -p "$(dirname "$OPENCLAW_CONFIG")"

# Check if hooks section exists
if grep -q "hooks:" "$OPENCLAW_CONFIG" 2>/dev/null; then
    echo "⚠️  hooks section already exists in $OPENCLAW_CONFIG"
    echo "   Add the following manually:"
    echo ""
    echo "hooks:"
    echo "  on_conversation_end:"
    echo "    - type: webhook"
    echo "      url: ${MEMEX8_URL}/api/v1/webhooks/conversation"
    echo "      method: POST"
    echo "      headers:"
    echo "        Authorization: \"Bearer ${API_KEY}\""
    echo "        Content-Type: application/json"
    echo "      body_template: |"
    echo "        {{"
    echo "          \"summary\": \"{{{{conversation_summary}}}}\","
    echo "          \"source\": \"openclaw\","
    echo "          \"platform\": \"{{{{platform_name}}}}\""
    echo "        }}}"
    echo ""
    echo "  on_skill_executed:"
    echo "    - type: webhook"
    echo "      url: ${MEMEX8_URL}/api/v1/webhooks/skill"
    echo "      method: POST"
    echo "      headers:"
    echo "        Authorization: \"Bearer ${API_KEY}\""
    echo "        Content-Type: application/json"
    echo "      body_template: |"
    echo "        {{"
    echo "          \"skill_name\": \"{{{{skill_name}}}}\","
    echo "          \"skill_category\": \"{{{{skill_category}}}}\","
    echo "          \"status\": \"{{{{skill_status}}}}\","
    echo "          \"input\": {{{{skill_input}}}},"
    echo "          \"output\": {{{{skill_output}}}}"
    echo "        }}}"
else
    # Add hooks section
    cat >> "$OPENCLAW_CONFIG" << EOF

hooks:
  on_conversation_end:
    - type: webhook
      url: ${MEMEX8_URL}/api/v1/webhooks/conversation
      method: POST
      headers:
        Authorization: "Bearer ${API_KEY}"
        Content-Type: application/json
      body_template: |
        {{
          "summary": "{{{{conversation_summary}}}}",
          "source": "openclaw",
          "platform": "{{{{platform_name}}}}"
        }}

  on_skill_executed:
    - type: webhook
      url: ${MEMEX8_URL}/api/v1/webhooks/skill
      method: POST
      headers:
        Authorization: "Bearer ${API_KEY}"
        Content-Type: application/json
      body_template: |
        {{
          "skill_name": "{{{{skill_name}}}}",
          "skill_category": "{{{{skill_category}}}}",
          "status": "{{{{skill_status}}}}",
          "input": {{{{skill_input}}}},
          "output": {{{{skill_output}}}}
        }}
EOF
    echo "✅ Added hooks section to $OPENCLAW_CONFIG"
fi

echo ""
echo "📥 OpenClaw will now send:"
echo "   - Conversation summaries → /api/v1/webhooks/conversation"
echo "   - Skill execution results → /api/v1/webhooks/skill"
echo ""
echo "   Restart OpenClaw for changes to take effect."
