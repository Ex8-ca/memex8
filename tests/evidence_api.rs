//! Run with a disposable Qdrant only: EVIDENCE_QDRANT_URL=http://127.0.0.1:PORT cargo test --test evidence_api -- --ignored
use memex8::{api::server::run_with_engine, config::AppConfig, engine::Engine};
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires explicit disposable loopback Qdrant"]
async fn explicit_rest_capture_links_reads_retries_and_fails_closed() {
    let url = std::env::var("EVIDENCE_QDRANT_URL").expect("disposable Qdrant URL");
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
    std::env::set_var("EVIDENCE_FIXTURE_API_KEY", "fixture-only");
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let embedding_url = format!("http://{}", listener.local_addr().unwrap());
    let fail_embedding = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fixture_failure = fail_embedding.clone();
    let fixture = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/api/embed",
                axum::routing::post(move || {
                    let failure = fixture_failure.clone();
                    async move {
                        use axum::response::IntoResponse;
                        if failure.load(std::sync::atomic::Ordering::SeqCst) {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                "fixture embedding failure",
                            )
                                .into_response();
                        }
                        axum::Json(json!({"embeddings": [[1.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0]]}))
                            .into_response()
                    }
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
    config.auth.api_key_env = "EVIDENCE_FIXTURE_API_KEY".into();
    config.turbovec.index_path = dir.path().join("memories.tv").display().to_string();
    config.turbovec.id_map_path = dir.path().join("ids.json").display().to_string();
    config.evidence.enabled = true;
    let engine = Arc::new(Engine::new(config.clone()).await.unwrap());
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let mut detached_config = config.clone();
    detached_config.evidence.enabled = false;
    detached_config.turbovec.index_path = dir
        .path()
        .join("detached/memories.tv")
        .display()
        .to_string();
    let clean = Engine::new(detached_config.clone()).await.unwrap();
    let forged_id = uuid::Uuid::new_v4().to_string();
    let forged = json!([{"vector":[1.,0.,0.,0.,0.,0.,0.,0.], "id":forged_id,
        "content":"forged source", "summary":null, "source_file":null,
        "realm_id":null, "realm_name":"", "importance":1., "upvotes":0,
        "tags":[], "ingested_at":"2026-01-01T00:00:00Z", "last_accessed":"2026-01-01T00:00:00Z",
        "access_count":0, "chunk_type":"manual", "heading":null, "source_hash":"",
        "evidence_refs":[{"schema_version":1,"source_id":uuid::Uuid::new_v4().to_string(),"start_byte":0,"end_byte":13}]}]);
    let forged_path = dir.path().join("forged.json");
    std::fs::write(&forged_path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(clean
        .import(forged_path.to_str().unwrap(), true)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(clean
        .store()
        .get_memory(&forged_id)
        .await
        .unwrap()
        .is_none());
    let e = engine.clone();
    let server =
        tokio::spawn(async move { run_with_engine(config, e, "127.0.0.1", port).await.unwrap() });
    let base = format!("http://127.0.0.1:{port}/api/v1");
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if client.get(format!("{base}/health")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request = json!({"content": "exact\r\nexcerpt", "tags": ["project:one"], "idempotency_key": "event-1", "evidence": {"speaker":"user", "origin":"explicit_rest", "pre_redacted":true, "session":"s1"}});
    let response = client
        .post(format!("{base}/memories"))
        .bearer_auth("fixture-only")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let created: Value = response.json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    let memory: Value = client
        .get(format!("{base}/memories/{id}"))
        .bearer_auth("fixture-only")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let source = memory["evidence_refs"][0]["source_id"]
        .as_str()
        .expect("durable evidence link");
    assert_eq!(memory["evidence_refs"][0]["schema_version"], 1);
    let endpoint = format!("{base}/memories/{id}/evidence/{source}");
    assert_eq!(client.get(&endpoint).send().await.unwrap().status(), 401);
    let evidence: Value = client
        .get(&endpoint)
        .bearer_auth("fixture-only")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(evidence["content"], "exact\nexcerpt");
    assert_eq!(evidence["speaker"], "user");
    assert_eq!(evidence["scope"], json!(["project:one"]));
    assert_eq!(evidence["visibility"], "private");
    let retry: Value = client
        .post(format!("{base}/memories"))
        .bearer_auth("fixture-only")
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["id"], id);
    let mut conflict = request.clone();
    conflict["content"] = json!("changed");
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&conflict)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let legacy: Value = client
        .post(format!("{base}/memories"))
        .bearer_auth("fixture-only")
        .json(&json!({"content":"legacy", "visibility":"nonsense"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let legacy_id = legacy["id"].as_str().unwrap();
    assert_eq!(
        client
            .get(format!("{base}/memories/{legacy_id}/evidence/{source}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .delete(format!("{base}/memories/{id}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(engine
        .backup(Some(dir.path().join("backup").to_str().unwrap()))
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .export(dir.path().join("export.json").to_str().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .restore("missing.tar.gz", true)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .import("missing.json", true)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert_eq!(
        client
            .patch(format!("{base}/memories/{id}"))
            .bearer_auth("fixture-only")
            .json(&json!({"visibility":"public"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(engine
        .trigger_slumber(false)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .edit_memory(id, "replacement")
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .update_memory_payload(id, json!({"visibility":"public"}).try_into().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    let maintenance = memex8::engine::slumber::SlumberEngine::new(
        engine.config().clone(),
        engine.store().clone_store(),
    );
    assert!(maintenance
        .run_full_pipeline(false)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .unconsolidate(id)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .store()
        .set_certification(id, false, None, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .store()
        .delete_unprotected_memory(id)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .store()
        .update_unprotected_payload(id, json!({"tags":[]}).try_into().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .store()
        .delete_consolidation_snapshot(&engine.get_memory(id).await.unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .store()
        .batch_update_payload(&[(id, json!({"tags":[]}).try_into().unwrap())])
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    let snapshot = engine
        .store()
        .get_memory_snapshot(id)
        .await
        .unwrap()
        .unwrap();
    assert!(engine
        .store()
        .restore_memory(&snapshot)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    let mut unsupported = request.clone();
    unsupported["certified"] = json!(true);
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&unsupported)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(engine
        .merge_realms("missing", "missing")
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .split_realm("missing")
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .restore_duplicate(id)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    let stored_before = engine.get_memory(id).await.unwrap();
    assert_eq!(stored_before.content, "exact\nexcerpt");
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let mut interrupted = request.clone();
    interrupted["idempotency_key"] = json!("interrupted-1");
    fail_embedding.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&interrupted)
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    let connection = rusqlite::Connection::open(dir.path().join("evidence.sqlite3")).unwrap();
    let (source_pending, memory_pending): (String, String) = connection
        .query_row(
            "SELECT source_id, memory_id FROM sources WHERE idempotency_key='interrupted-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        client
            .get(format!(
                "{base}/memories/{memory_pending}/evidence/{source_pending}"
            ))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    fail_embedding.store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered: Value = client
        .post(format!("{base}/memories"))
        .bearer_auth("fixture-only")
        .json(&interrupted)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recovered["id"], memory_pending);
    assert_eq!(
        client
            .get(format!(
                "{base}/memories/{memory_pending}/evidence/{source_pending}"
            ))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let detached = Engine::new(detached_config).await.unwrap();
    let mut forged = serde_json::to_value(engine.get_memory(legacy_id).await.unwrap()).unwrap();
    forged["id"] = json!(uuid::Uuid::new_v4().to_string());
    forged["evidence_refs"] = memory["evidence_refs"].clone();
    let incoming = dir.path().join("forged-evidence.json");
    std::fs::write(&incoming, serde_json::to_vec(&json!([forged])).unwrap()).unwrap();
    // Fresh isolated collection is tested separately; this detached engine also
    // must reject incoming links without trusting an unavailable ledger.
    assert!(detached
        .import(incoming.to_str().unwrap(), false)
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(detached
        .backup(Some(dir.path().join("detached-backup").to_str().unwrap()))
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(detached
        .export(dir.path().join("detached-export.json").to_str().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    server.abort();
    fixture.abort();
}
