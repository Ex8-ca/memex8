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
                let line = response_line(
                    None,
                    &json!({
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }),
                )?;
                write_line(&mut stdout, &line).await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        tracing::debug!("MCP request: method={}, id={:?}", method, id);

        // Read-only tool calls serialize their response while the maintenance
        // read gate is held, so an erasure cannot commit between fetch and
        // response construction. Delivery over stdout is not covered.
        let reply: anyhow::Result<String> = match method {
            "initialize" => response_line(id.as_ref(), &super::dispatch::initialize_result()),
            "initialized" => response_line(id.as_ref(), &json!({})),
            "tools/list" => response_line(id.as_ref(), &json!({ "tools": tools_list })),
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
                match super::publish::tool_name(&params) {
                    Ok(name) => {
                        super::publish::tool_response_line(
                            e.store().gate(),
                            name,
                            id.as_ref(),
                            super::dispatch::call_tool(
                                e,
                                name,
                                super::publish::tool_arguments(&params),
                            ),
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            "ping" => response_line(id.as_ref(), &json!({})),
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

        match reply {
            Ok(line) => write_line(&mut stdout, &line).await?,
            Err(e) => {
                tracing::error!("MCP error: {:?}", e);
                write_error(&mut stdout, id, -32603, format!("{}", e)).await?;
            }
        }
    }

    tracing::info!("MCP server stopped");
    Ok(())
}

fn response_line(
    id: Option<&serde_json::Value>,
    result: &serde_json::Value,
) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))?)
}

async fn write_line(stdout: &mut tokio::io::Stdout, line: &str) -> std::io::Result<()> {
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
