//! Run with a disposable Qdrant only:
//! MCP_DISPATCH_QDRANT_URL=http://127.0.0.1:PORT cargo test --test mcp_dispatch -- --ignored
//!
//! Every name in `DISPATCHED_TOOLS` must reach a real arm of `call_tool`. The
//! Qdrant-free unit tests in `src/mcp/dispatch.rs` already keep the constant
//! equal to the arms; this test catches an arm that falls through to the
//! `Unknown tool` error at run time.
use memex8::{config::AppConfig, engine::Engine, mcp::dispatch};
use serde_json::json;

#[tokio::test]
#[ignore = "requires explicit disposable loopback Qdrant"]
async fn every_dispatched_tool_reaches_an_arm() {
    let url = std::env::var("MCP_DISPATCH_QDRANT_URL").expect("disposable Qdrant URL");
    assert!(url.starts_with("http://127.0.0.1:"));
    for key in [
        "QDRANT_URL",
        "EMBEDDING_PROVIDER",
        "EMBEDDING_MODEL",
        "EMBEDDING_DIMENSIONS",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OLLAMA_URL",
    ] {
        std::env::remove_var(key);
    }
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let embedding_url = format!("http://{}", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/api/embed",
                axum::routing::post(|| async {
                    axum::Json(json!({"embeddings": [[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]}))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut config = AppConfig::default();
    config.qdrant.url = url;
    config.embedding.provider = "ollama".into();
    config.embedding.dimensions = 8;
    config.embedding.ollama.url = embedding_url;
    config.turbovec.index_path = dir.path().join("memories.tv").display().to_string();
    config.turbovec.id_map_path = dir.path().join("ids.json").display().to_string();
    let engine = Engine::new(config).await.unwrap();

    for name in dispatch::DISPATCHED_TOOLS {
        let call = dispatch::call_tool(&engine, name, json!({}));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), call)
            .await
            .unwrap_or_else(|_| panic!("{name} did not return within 30s"));
        if let Err(error) = outcome {
            assert!(
                !error.to_string().contains("Unknown tool"),
                "{name} is in DISPATCHED_TOOLS but call_tool has no arm for it"
            );
        }
    }

    let error = dispatch::call_tool(&engine, "memex8_no_such_tool", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Unknown tool"));
    fixture.abort();
}
