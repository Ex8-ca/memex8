//! MCP tool dispatch: the one place that turns a tool name and its JSON
//! arguments into Engine calls. The stdio and SSE transports only frame
//! messages and call [`call_tool`]; they do not know which tools exist.
//!
//! Three lists describe the tool set: the schemas in `tools::list_tools`, the
//! read/write classification in `publish::is_read_only_tool`, and the arms of
//! [`call_tool`]. [`DISPATCHED_TOOLS`] names the arms, and the tests below fail
//! when it, the arms, or the advertised schemas disagree.
use crate::engine::Engine;
use serde_json::json;

/// Every tool name [`call_tool`] handles. Kept equal to the match arms by the
/// `dispatched_tools_match_the_match_arms` test.
pub const DISPATCHED_TOOLS: &[&str] = &[
    "memex8_timeline",
    "memex8_search",
    "memex8_store",
    "memex8_remember",
    "memex8_restore_duplicate",
    "memex8_unconsolidate",
    "memex8_certify",
    "memex8_recall",
    "memex8_get",
    "memex8_ingest",
    "memex8_realms_list",
    "memex8_realms_show",
    "memex8_upvote",
    "memex8_stats",
    "memex8_slumber_status",
    "memex8_graph_search",
    "memex8_infer",
    "memex8_graph_traverse",
    "memex8_graph_build",
    "memex8_graph_stats",
];

/// MCP `initialize` result, shared by every transport.
pub fn initialize_result() -> serde_json::Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "memex8",
            "version": "0.1.0"
        }
    })
}

/// Run one tool against the Engine.
///
/// `name` and `arguments` come from the JSON-RPC `params` of a `tools/call`
/// request (see `publish::tool_name` and `publish::tool_arguments`). Unknown
/// names return an `Unknown tool` error. Whether the caller holds the
/// maintenance read gate is the transport's job: `publish::tool_response_line`.
pub async fn call_tool(
    engine: &Engine,
    name: &str,
    arguments: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    tracing::info!("MCP tool call: {}", name);

    match name {
        "memex8_timeline" => {
            let request = serde_json::from_value(arguments.clone())?;
            Ok(serde_json::to_value(
                engine.timeline_request(request).await?,
            )?)
        }
        "memex8_search" => {
            let compact = super::tools::boolean_argument(&arguments, "compact")?;
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());
            let min_score = arguments
                .get("min_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;

            let tags: Option<Vec<String>> = arguments
                .get("tags")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?;
            let visibility = arguments.get("visibility").and_then(|v| v.as_str());
            let outcome = engine
                .search_with_coverage(
                    query,
                    realm,
                    tags.as_deref(),
                    limit,
                    0,
                    min_score,
                    visibility,
                )
                .await?;
            super::tools::with_coverage(format_results(outcome.results, compact), &outcome.coverage)
        }
        "memex8_store" | "memex8_remember" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;
            let tags: Vec<String> = arguments
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let realm_hint = arguments.get("realm_hint").and_then(|v| v.as_str());
            // MCP tool callers can opt in to public explicitly; default
            // to private so an MCP-driven capture session can't leak
            // everything it touches to the A2A bridge.
            let visibility = arguments.get("visibility").and_then(|v| v.as_str());

            let memory_type = arguments.get("memory_type").and_then(|v| v.as_str());
            let source = arguments.get("source").and_then(|v| v.as_str());
            let certified = super::tools::boolean_argument(&arguments, "certified")?;
            let stored = engine
                .write_memory(crate::engine::NewMemory {
                    tags,
                    realm_hint,
                    source: source.or(Some("mcp")),
                    visibility: crate::engine::Visibility::from_option(visibility),
                    memory_type,
                    certified,
                    ..crate::engine::NewMemory::new(content)
                })
                .await?;
            Ok(json!({ "id": stored.id, "status": "stored" }))
        }
        "memex8_restore_duplicate" => {
            let operation_id = arguments
                .get("operation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("operation_id is required"))?;
            let result = engine.restore_duplicate(operation_id).await?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&result)?}]}))
        }
        "memex8_unconsolidate" => {
            let memory_id = arguments
                .get("memory_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("memory_id is required"))?;
            let result = engine.unconsolidate(memory_id).await?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&result)?}]}))
        }
        "memex8_certify" => {
            let memory_id = arguments
                .get("memory_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("memory_id is required"))?;
            let revoke = super::tools::boolean_argument(&arguments, "revoke")?;
            let by = arguments.get("by").and_then(|v| v.as_str());
            let note = arguments.get("note").and_then(|v| v.as_str());
            let memory = engine
                .set_certification(memory_id, revoke, by, note)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Memory not found: {}", memory_id))?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&memory)?}]}))
        }
        "memex8_recall" => {
            let compact = super::tools::boolean_argument(&arguments, "compact")?;
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());

            let results = engine.recall(limit, realm).await?;
            Ok(format_results(results, compact))
        }
        "memex8_get" => super::tools::get_memories(engine, &arguments).await,
        "memex8_ingest" => {
            let path = arguments
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;
            let chunk_by = arguments
                .get("chunk_by")
                .and_then(|v| v.as_str())
                .unwrap_or("section");
            let realm_hint = arguments.get("realm_hint").and_then(|v| v.as_str());

            let report = engine.ingest_path(path, chunk_by, realm_hint).await?;
            // Same receipt shape as POST /api/v1/memories/ingest: a partial or
            // failed ingest must never be reported as a clean success.
            Ok(crate::api::routes::memories::ingest_response(path, &report))
        }
        "memex8_realms_list" => {
            let realms = engine.list_realms().await?;
            let realms: Vec<crate::api::routes::realms::RealmSummary> =
                realms.into_iter().map(Into::into).collect();
            Ok(json!({ "realms": realms }))
        }
        "memex8_realms_show" => {
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;
            let realm = engine.show_realm(name).await?;
            let realm: crate::api::routes::realms::RealmSummary = realm.into();
            Ok(json!({ "realm": realm }))
        }
        "memex8_upvote" => {
            let id = arguments
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
            engine.upvote(id).await?;
            Ok(json!({ "upvoted": id }))
        }
        "memex8_stats" => {
            let stats = engine.stats().await?;
            Ok(json!({ "stats": stats }))
        }
        "memex8_slumber_status" => {
            let status = engine.slumber_status().await;
            Ok(json!({ "status": status }))
        }
        "memex8_graph_search" => {
            let entity = arguments
                .get("entity")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'entity' parameter"))?;
            let relationship = arguments.get("relationship").and_then(|v| v.as_str());
            let depth = arguments.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

            let results = engine.graph_search(entity, relationship, depth).await?;
            Ok(json!({ "results": results }))
        }
        "memex8_infer" => {
            let topic = arguments.get("topic").and_then(|v| v.as_str());
            let memory_id = arguments.get("memory_id").and_then(|v| v.as_str());
            let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

            let suggestions = engine.infer_gaps(topic, memory_id, limit).await?;
            Ok(json!({ "suggestions": suggestions }))
        }
        "memex8_graph_traverse" => {
            let memory_id = arguments
                .get("memory_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'memory_id' parameter"))?;
            let depth = arguments.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

            let results = engine.graph_traverse(memory_id, depth).await?;
            Ok(json!({ "results": results, "count": results.len() }))
        }
        "memex8_graph_build" => {
            let threshold = engine.config().slumber.association_min_strength;
            let count = engine.build_graph(threshold).await?;
            Ok(json!({ "status": "built", "edges_created": count }))
        }
        "memex8_graph_stats" => {
            let stats = engine.graph_stats().await?;
            Ok(json!({ "stats": stats }))
        }
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
}

fn format_results(results: Vec<crate::engine::MemoryResult>, compact: bool) -> serde_json::Value {
    if compact {
        let count = results.len();
        return json!({"results": crate::engine::retrieval::RetrievalResults::new(results, true), "count": count});
    }
    json!({
        "results": results.iter().map(|r| json!({
            "id": r.id,
            "content": r.content,
            "heading": r.heading,
            "realm": r.realm_name,
            "importance": r.importance,
            "score": r.score,
        })).collect::<Vec<_>>(),
        "count": results.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool names that appear as match arms of `call_tool` in this file.
    fn arm_names() -> Vec<String> {
        let source = include_str!("dispatch.rs");
        let start = source.find("pub async fn call_tool(").expect("call_tool");
        let end = source[start..].find("\nfn format_results").expect("end") + start;
        source[start..end]
            .lines()
            .filter(|line| line.starts_with("        \"memex8_"))
            .filter_map(|line| line.split_once("=>").map(|(pattern, _)| pattern))
            .flat_map(|pattern| {
                pattern
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    #[test]
    fn dispatched_tools_match_the_match_arms() {
        let mut arms = arm_names();
        arms.sort();
        let mut listed: Vec<String> = DISPATCHED_TOOLS.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(arms, listed);
        let mut unique = listed.clone();
        unique.dedup();
        assert_eq!(unique, listed, "duplicate tool name");
    }

    #[test]
    fn every_advertised_tool_is_dispatched() {
        for tool in crate::mcp::tools::list_tools() {
            let name = tool["name"].as_str().expect("tool name");
            assert!(
                DISPATCHED_TOOLS.contains(&name),
                "{name} is advertised by tools/list but call_tool has no arm for it"
            );
        }
    }
}
