use crate::config::AppConfig;
use crate::engine::Engine;
use crate::mcp::tools;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Run MCP server over stdio (for Hermes and other MCP clients)
pub async fn run_stdio(config: AppConfig) -> anyhow::Result<()> {
    tracing::info!("Starting MCP server (stdio transport)");

    // Try to connect to Qdrant; if unavailable, still serve tool metadata
    let engine = match Engine::new(config.clone()).await {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!("Qdrant unavailable, MCP will serve metadata only: {}", e);
            None
        }
    };

    let tools_list = tools::list_tools();

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &mut stdout,
                    None,
                    json!({
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }),
                )
                .await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        tracing::debug!("MCP request: method={}, id={:?}", method, id);

        let result = match method {
            "initialize" => handle_initialize(),
            "initialized" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools_list })),
            "tools/call" => {
                let Some(ref e) = engine else {
                    write_error(
                        &mut stdout,
                        id.clone(),
                        -32603,
                        "Qdrant unavailable — memory operations not accessible".into(),
                    )
                    .await?;
                    continue;
                };
                handle_tool_call(e, &params).await
            }
            "ping" => Ok(json!({})),
            _ => {
                write_error(
                    &mut stdout,
                    id.clone(),
                    -32601,
                    format!("Method not found: {}", method),
                )
                .await?;
                continue;
            }
        };

        match result {
            Ok(data) => write_response(&mut stdout, id, data).await?,
            Err(e) => {
                tracing::error!("MCP error: {:?}", e);
                write_error(&mut stdout, id, -32603, format!("{}", e)).await?;
            }
        }
    }

    tracing::info!("MCP server stopped");
    Ok(())
}

fn handle_initialize() -> anyhow::Result<serde_json::Value> {
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "memex8",
            "version": "0.1.0"
        }
    }))
}

async fn handle_tool_call(
    engine: &Engine,
    params: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    tracing::info!("MCP tool call: {} {:?}", name, arguments);

    match name {
        "memex8_search" => {
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

            let results = engine
                .search(query, realm, None, limit, 0, min_score)
                .await?;
            Ok(format_results(results))
        }
        "memex8_store" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;
            let tags = arguments.get("tags").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });
            let realm_hint = arguments.get("realm_hint").and_then(|v| v.as_str());

            let id = engine
                .store_memory(content, tags, realm_hint, Some("mcp"))
                .await?;
            Ok(json!({ "id": id, "status": "stored" }))
        }
        "memex8_recall" => {
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());

            let results = engine.recall(limit, realm).await?;
            Ok(format_results(results))
        }
        "memex8_get" => {
            let id = arguments
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
            let memory = engine.get_memory(id).await?;
            Ok(json!({ "memory": memory }))
        }
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

            engine.ingest_path(path, chunk_by, realm_hint).await?;
            Ok(json!({ "status": "ingested", "path": path }))
        }
        "memex8_realms_list" => {
            let realms = engine.list_realms().await?;
            Ok(json!({ "realms": realms }))
        }
        "memex8_realms_show" => {
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;
            let realm = engine.show_realm(name).await?;
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
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
}

fn format_results(results: Vec<crate::engine::MemoryResult>) -> serde_json::Value {
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

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    id: Option<serde_json::Value>,
    result: serde_json::Value,
) -> std::io::Result<()> {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    let line = serde_json::to_string(&response).unwrap_or_else(|_| "{}".into());
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn write_error(
    stdout: &mut tokio::io::Stdout,
    id: Option<serde_json::Value>,
    code: i64,
    message: String,
) -> std::io::Result<()> {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let line = serde_json::to_string(&response).unwrap_or_else(|_| "{}".into());
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

/// Run MCP server over SSE (HTTP)
pub async fn run_sse(_config: AppConfig, port: u16) -> anyhow::Result<()> {
    tracing::info!("Starting MCP server (SSE transport) on port {}", port);
    // TODO: implement SSE transport using Axum
    // For now, use the main server with the SSE endpoint
    Ok(())
}
