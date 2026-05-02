use serde_json::json;

/// Get all MCP tool definitions
pub fn list_tools() -> Vec<serde_json::Value> {
    vec![
        tool(
            "memex8_search",
            "Semantic search across memories",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": { "type": "number", "description": "Max results (default 10)" },
                    "realm": { "type": "string", "description": "Filter to realm name" },
                    "min_score": { "type": "number", "description": "Minimum similarity (0-1, default 0.3)" }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "memex8_store",
            "Store a new memory",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Memory content (markdown)" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "realm_hint": { "type": "string" },
                    "source": { "type": "string" }
                },
                "required": ["content"]
            }),
        ),
        tool(
            "memex8_recall",
            "Get high-importance memories",
            json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "number", "description": "Max memories (default 10)" },
                    "realm": { "type": "string" }
                }
            }),
        ),
        tool(
            "memex8_get",
            "Get memory by ID",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        ),
        tool(
            "memex8_ingest",
            "Ingest file or directory",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "chunk_by": { "type": "string", "enum": ["section", "paragraph", "file"] },
                    "realm_hint": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "memex8_realms_list",
            "List all knowledge realms",
            json!({ "type": "object" }),
        ),
        tool(
            "memex8_realms_show",
            "Show realm details",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "memex8_upvote",
            "Increase memory importance",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        ),
        tool(
            "memex8_stats",
            "System statistics",
            json!({ "type": "object" }),
        ),
        tool(
            "memex8_slumber_status",
            "Slumber pipeline status",
            json!({ "type": "object" }),
        ),
        tool(
            "memex8_graph_search",
            "Graph-based memory retrieval",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "string" },
                    "relationship": { "type": "string" },
                    "depth": { "type": "number" }
                },
                "required": ["entity"]
            }),
        ),
        tool(
            "memex8_infer",
            "Given context about a topic, infer related gaps and suggest follow-up actions",
            json!({
                "type": "object",
                "properties": {
                    "topic": { "type": "string", "description": "Topic to analyze for gaps" },
                    "memory_id": { "type": "string", "description": "Or a specific memory ID to analyze" },
                    "limit": { "type": "number", "description": "Max suggestions (default 5)" }
                }
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> serde_json::Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}
