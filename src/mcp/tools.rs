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
        store_tool("memex8_store"),
        store_tool("memex8_remember"),
        tool(
            "memex8_certify",
            "Protect a user-confirmed memory from automatic rewriting, pruning, decay, and verification. Only certify with explicit user intent; revoke=true returns it to normal lifecycle.",
            json!({
                "type": "object",
                "properties": {
                    "memory_id": {"type": "string"},
                    "revoke": {"type": "boolean", "default": false},
                    "by": {"type": "string", "description": "Optional attribution label, not an authenticated identity"},
                    "note": {"type": "string", "description": "Optional certification note"}
                },
                "required": ["memory_id"]
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
        tool(
            "memex8_graph_traverse",
            "Traverse the knowledge graph from a memory, returning connected memories with relationship paths",
            json!({
                "type": "object",
                "properties": {
                    "memory_id": { "type": "string", "description": "Starting memory ID" },
                    "depth": { "type": "number", "description": "Max traversal depth (default 2)" }
                },
                "required": ["memory_id"]
            }),
        ),
        tool(
            "memex8_graph_build",
            "Build or rebuild the knowledge graph from all stored memories (creates entity-based edges)",
            json!({ "type": "object" }),
        ),
        tool(
            "memex8_graph_stats",
            "Get statistics about the knowledge graph (edge counts, types)",
            json!({ "type": "object" }),
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

fn store_tool(name: &str) -> serde_json::Value {
    tool(name, "Store a durable memory. Set certified=true only when the user explicitly asks to remember/confirm it; ordinary capture remains uncertified.", json!({
        "type": "object",
        "properties": {
            "content": {"type": "string", "description": "Memory content (markdown)"},
            "tags": {"type": "array", "items": {"type": "string"}},
            "realm_hint": {"type": "string"},
            "source": {"type": "string"},
            "memory_type": {"type": "string"},
            "visibility": {"type": "string", "enum": ["private", "public"], "default": "private"},
            "certified": {"type": "boolean", "default": false}
        },
        "required": ["content"]
    }))
}

pub fn boolean_argument(arguments: &serde_json::Value, name: &str) -> anyhow::Result<bool> {
    arguments
        .get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("{} must be a boolean", name))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certification_flags_require_actual_booleans() {
        assert!(!boolean_argument(&json!({}), "certified").unwrap());
        assert!(boolean_argument(&json!({"certified": true}), "certified").unwrap());
        assert!(!boolean_argument(&json!({"certified": false}), "certified").unwrap());
        for value in [json!("false"), json!("true"), json!(1), json!(null)] {
            assert!(boolean_argument(&json!({"certified": value}), "certified").is_err());
        }
    }

    #[test]
    fn legacy_store_and_remember_advertise_identical_parameters() {
        let tools = list_tools();
        let schema =
            |name| tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"].clone();
        assert_eq!(schema("memex8_store"), schema("memex8_remember"));
        assert_eq!(schema("memex8_certify")["required"], json!(["memory_id"]));
        assert_eq!(
            schema("memex8_remember")["properties"]["certified"]["default"],
            false
        );
    }
}
