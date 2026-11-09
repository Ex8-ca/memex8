use serde_json::json;

/// Get all MCP tool definitions
pub fn list_tools() -> Vec<serde_json::Value> {
    vec![
        tool(
            "memex8_timeline",
            "Chronological compact neighbors around exactly one memory id, query, or RFC3339 at timestamp. Includes ID/query anchor; time mode has no synthetic row. score=0; truncated=true means the 10,000-row scan budget was reached. Use memex8_get for full bodies.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "format": "uuid"},
                    "query": {"type": "string", "minLength": 1},
                    "at": {"type": "string", "format": "date-time"},
                    "before": {"type": "integer", "minimum": 0, "maximum": 50, "default": 5},
                    "after": {"type": "integer", "minimum": 0, "maximum": 50, "default": 5},
                    "realm": {"type": "string"},
                    "visibility": {"type": "string", "enum": ["private", "public"]}
                },
                "oneOf": [{"required": ["id"]}, {"required": ["query"]}, {"required": ["at"]}]
            }),
        ),
        tool(
            "memex8_search",
            "Semantic search across memories with a retrieval-strength coverage heuristic. Use compact=true, then memex8_get(ids=[...]) for the ones that matter.",
            json!({
                "type": "object",
                "properties": {
                    "compact": { "type": "boolean", "default": false },
                    "query": { "type": "string", "description": "Search query" },
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Require all tags"},
                    "visibility": {"type": "string", "enum": ["private", "public"]},
                    "limit": { "type": "number", "description": "Max results (default 10)" },
                    "realm": { "type": "string", "description": "Filter to realm name" },
                    "min_score": { "type": "number", "description": "Minimum similarity (0-1, default 0.3)" }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "memex8_restore_duplicate",
            "Restore a removed duplicate from its operation journal. Preserves the current winner and newer source edits; applies 30-day dedup and consolidation cooldowns. Safe to retry. Use an operation ID from the slumber report/log.",
            json!({"type": "object", "properties": {
                "operation_id": {"type": "string"}
            }, "required": ["operation_id"]}),
        ),
        tool(
            "memex8_unconsolidate",
            "Undo a consolidated summary using its durable source archive. Restores missing sources, retains newer source edits, then deletes the summary. Restored sources receive a 30-day consolidation cooldown. Safe to retry after a partial failure.",
            json!({"type": "object", "properties": {
                "memory_id": {"type": "string", "description": "Summary ID (also the consolidation journal ID)"}
            }, "required": ["memory_id"]}),
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
            "Get high-importance memories. Use compact=true, then memex8_get(ids=[...]) for the ones that matter.",
            json!({
                "type": "object",
                "properties": {
                    "compact": { "type": "boolean", "default": false },
                    "limit": { "type": "number", "description": "Max memories (default 10)" },
                    "realm": { "type": "string" }
                }
            }),
        ),
        tool(
            "memex8_get",
            "Fetch full memories selected from compact search/recall. Pass memory_id (legacy id also accepted) or ids (1–50). Batch results preserve requested order and report unavailable IDs in not_found.",
            json!({
                "type": "object",
                "properties": {
                    "memory_id": { "type": "string", "minLength": 1 },
                    "id": { "type": "string", "minLength": 1, "description": "Legacy alias for memory_id" },
                    "ids": { "type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 50 }
                },
                "oneOf": [{"required": ["memory_id"]}, {"required": ["id"]}, {"required": ["ids"]}]
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
    tool(name, "Store a durable memory. Text inside <private>...</private> is removed before embedding/storage; an unclosed span hides the remaining text. Set certified=true only when the user explicitly asks to remember/confirm it; ordinary capture remains uncertified.", json!({
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

/// Preserve structured results while supplying standard MCP text content.
pub fn with_coverage(
    mut result: serde_json::Value,
    coverage: &crate::engine::coverage::SearchCoverage,
) -> anyhow::Result<serde_json::Value> {
    result["coverage"] = serde_json::to_value(coverage)?;
    let text = format!(
        "{}\n{}",
        coverage.first_line(),
        serde_json::to_string(&result)?
    );
    result["content"] = json!([{"type": "text", "text": text}]);
    Ok(result)
}

/// Shared dispatch and validation for both MCP transports.
pub async fn get_memories(
    engine: &crate::engine::Engine,
    args: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match get_selection(args)? {
        GetSelection::Single(id) => Ok(json!({"memory": engine.get_memory(&id).await?})),
        GetSelection::Batch(ids) => Ok(serde_json::to_value(engine.batch_get(&ids).await?)?),
    }
}

#[derive(Debug, PartialEq)]
pub enum GetSelection {
    Single(String),
    Batch(Vec<String>),
}

pub fn get_selection(args: &serde_json::Value) -> anyhow::Result<GetSelection> {
    let names = ["memory_id", "id", "ids"];
    anyhow::ensure!(
        names
            .iter()
            .filter(|name| args.get(**name).is_some())
            .count()
            == 1,
        "Pass exactly one of memory_id, id, or ids"
    );
    if let Some(ids) = args.get("ids") {
        let ids: Vec<String> = serde_json::from_value(ids.clone())?;
        crate::engine::retrieval::validate_batch_ids(&ids).map_err(anyhow::Error::msg)?;
        Ok(GetSelection::Batch(ids))
    } else {
        let id = args
            .get("memory_id")
            .or_else(|| args.get("id"))
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("memory_id must be a nonempty string"))?;
        Ok(GetSelection::Single(id.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_requires_one_selection_and_valid_batch_size() {
        assert_eq!(
            get_selection(&json!({"id": "a"})).unwrap(),
            GetSelection::Single("a".into())
        );
        assert_eq!(
            get_selection(&json!({"memory_id": "a"})).unwrap(),
            GetSelection::Single("a".into())
        );
        assert_eq!(
            get_selection(&json!({"ids": ["b", "a", "b"]})).unwrap(),
            GetSelection::Batch(vec!["b".into(), "a".into(), "b".into()])
        );
        for args in [
            json!({}),
            json!({"id": "a", "ids": ["b"]}),
            json!({"id": "a", "memory_id": "a"}),
            json!({"memory_id": null}),
            json!({"ids": []}),
            json!({"ids": [1]}),
            json!({"ids": vec!["a"; 51]}),
        ] {
            assert!(get_selection(&args).is_err(), "{args}");
        }
        assert!(get_selection(&json!({"ids": vec!["a"; 50]})).is_ok());
    }

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
