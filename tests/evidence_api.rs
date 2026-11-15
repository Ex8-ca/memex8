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
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let pending_capture: memex8::storage::evidence::CaptureRequest =
        serde_json::from_value(request["evidence"].clone()).unwrap();
    fail_embedding.store(true, std::sync::atomic::Ordering::SeqCst);
    let pending_request = json!({"content":"pending source", "tags":[], "idempotency_key":"backup-pending", "evidence":request["evidence"]});
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&pending_request)
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    fail_embedding.store(false, std::sync::atomic::Ordering::SeqCst);
    let pending =
        memex8::storage::evidence::EvidenceStore::open(&dir.path().join("evidence.sqlite3"))
            .unwrap()
            .prepare(
                "backup-pending",
                "pending source",
                &pending_capture,
                &[],
                "private",
            )
            .unwrap();
    engine
        .store()
        .store_realm(
            &uuid::Uuid::new_v4().to_string(),
            &[1., 0., 0., 0., 0., 0., 0., 0.],
            "fixture-realm",
            Some("retained description"),
            true,
        )
        .await
        .unwrap();
    let journal = memex8::storage::qdrant::ConsolidationOperation {
        version: 1,
        kind: "consolidation".into(),
        dedup_winner: None,
        summary_id: uuid::Uuid::new_v4().to_string(),
        summary_hash: "fixture".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        state: "complete".into(),
        preserve_originals: true,
        embedding_provider: "ollama".into(),
        embedding_model: "fixture".into(),
        embedding_dimensions: 8,
        sources: vec![engine
            .store()
            .get_memory_snapshot(legacy_id)
            .await
            .unwrap()
            .unwrap()],
        cooldown_until: None,
    };
    engine
        .store()
        .archive_consolidation(&journal)
        .await
        .unwrap();
    let expected_realms = engine.store().list_realms().await.unwrap();
    let graph_from = legacy_id.to_owned();
    let graph_to = id.to_owned();
    engine
        .store()
        .store_graph_edge(&memex8::storage::qdrant::GraphEdge {
            from_memory_id: graph_from.clone(),
            to_memory_id: graph_to.clone(),
            relation_type: "references".into(),
            weight: 0.5,
        })
        .await
        .unwrap();
    assert_eq!(engine.store().get_all_graph_edges().await.unwrap().len(), 1);
    std::env::remove_var("MEMEX8_BACKUP_WRITERS_STOPPED");
    assert!(engine
        .backup(Some(dir.path().join("refused-backup").to_str().unwrap()))
        .await
        .unwrap_err()
        .to_string()
        .contains("stopped writers"));
    // Fixture has no requests in flight; operator attestation is explicit.
    std::env::set_var("MEMEX8_BACKUP_WRITERS_STOPPED", "1");
    let paired_backup = engine
        .backup(Some(dir.path().join("backup").to_str().unwrap()))
        .await
        .unwrap();
    assert!(std::path::Path::new(&paired_backup).exists());
    let target_dir = tempfile::tempdir().unwrap();
    let mut target_config = engine.config().clone();
    target_config.qdrant.url =
        std::env::var("EVIDENCE_RESTORE_QDRANT_URL").expect("separate disposable restore Qdrant");
    target_config.turbovec.index_path = target_dir.path().join("memories.tv").display().to_string();
    target_config.turbovec.id_map_path = target_dir.path().join("ids.json").display().to_string();
    let target = Arc::new(Engine::new(target_config.clone()).await.unwrap());
    let legacy_archive = target_dir.path().join("legacy.tar.gz");
    let legacy_snapshot = engine
        .store()
        .get_memory_snapshot(legacy_id)
        .await
        .unwrap()
        .unwrap();
    let mut output = tar::Builder::new(flate2::write::GzEncoder::new(
        std::fs::File::create(&legacy_archive).unwrap(),
        flate2::Compression::default(),
    ));
    for (name, bytes) in [
        (
            "memories.json",
            serde_json::to_vec(&vec![legacy_snapshot.clone()]).unwrap(),
        ),
        ("realms.json", b"[]".to_vec()),
        ("graph_edges.json", b"[]".to_vec()),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o600);
        h.set_size(bytes.len() as u64);
        h.set_cksum();
        output.append_data(&mut h, name, &bytes[..]).unwrap();
    }
    output.into_inner().unwrap().finish().unwrap();
    assert_eq!(
        target
            .restore(legacy_archive.to_str().unwrap(), true, false)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        target.get_memory(legacy_id).await.unwrap().content,
        legacy_snapshot.memory.content
    );
    target.delete_memory(legacy_id).await.unwrap();
    for corrupt in [false, true] {
        let broken = target_dir.path().join(if corrupt {
            "corrupt.tar.gz"
        } else {
            "missing.tar.gz"
        });
        let input = std::fs::File::open(&paired_backup).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(input));
        let mut output = tar::Builder::new(flate2::write::GzEncoder::new(
            std::fs::File::create(&broken).unwrap(),
            flate2::Compression::default(),
        ));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().into_owned();
            if name == std::path::Path::new("evidence.sqlite3") {
                if !corrupt {
                    continue;
                }
                let bytes = b"corrupt database";
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o600);
                header.set_cksum();
                output.append_data(&mut header, &name, &bytes[..]).unwrap();
            } else {
                output
                    .append_data(&mut entry.header().clone(), &name, &mut entry)
                    .unwrap();
            }
        }
        output.into_inner().unwrap().finish().unwrap();
        assert!(target
            .restore(broken.to_str().unwrap(), true, false)
            .await
            .is_err());
        assert!(target
            .store()
            .scroll_all_memories()
            .await
            .unwrap()
            .is_empty());
        assert!(target.store().list_realms().await.unwrap().is_empty());
        assert!(target
            .store()
            .get_all_graph_edges()
            .await
            .unwrap()
            .is_empty());
        assert!(target
            .store()
            .list_consolidations()
            .await
            .unwrap()
            .is_empty());
        assert!(!target_dir.path().join("evidence.sqlite3").exists());
    }
    let pairs = [
        ("scope", false),
        ("foreign", false),
        ("span", false),
        ("schema", false),
        ("realm-id", false),
        ("realm-dimensions", false),
        ("graph-id", false),
        ("journal-id", false),
        ("journal-dimensions", false),
    ];
    for (kind, _) in pairs {
        let broken = target_dir.path().join(format!("invalid-{kind}.tar.gz"));
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(
            std::fs::File::open(&paired_backup).unwrap(),
        ));
        let mut components = std::collections::BTreeMap::<String, Vec<u8>>::new();
        use std::io::Read;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_str().unwrap().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            components.insert(name, bytes);
        }
        let mut memories: Value = serde_json::from_slice(&components["memories.json"]).unwrap();
        let linked = memories
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|m| m["id"] == id)
            .unwrap();
        match kind {
            "scope" => linked["tags"] = json!(["foreign"]),
            "foreign" => {
                linked["evidence_refs"][0]["source_id"] = json!(uuid::Uuid::new_v4().to_string())
            }
            "span" => linked["evidence_refs"][0]["end_byte"] = json!(99999),
            _ => linked["evidence_refs"][0]["schema_version"] = json!(99),
        }
        let component = match kind {
            "realm-id" | "realm-dimensions" => "realms.json",
            "graph-id" => "graph_edges.json",
            "journal-id" | "journal-dimensions" => "consolidations.json",
            _ => "memories.json",
        };
        let mut document = if component == "memories.json" {
            memories
        } else {
            serde_json::from_slice(&components[component]).unwrap()
        };
        match kind {
            "realm-id" => document[0]["id"] = json!("bad"),
            "realm-dimensions" => document[0]["centroid"] = json!([1.0]),
            "graph-id" => document[0]["to_memory_id"] = json!("bad"),
            "journal-id" => document[0]["summary_id"] = json!("bad"),
            "journal-dimensions" => document[0]["sources"][0]["vector"] = json!([1.0]),
            _ => {}
        }
        let bytes = serde_json::to_vec(&document).unwrap();
        let mut manifest: Value = serde_json::from_slice(&components["manifest.json"]).unwrap();
        use sha2::Digest;
        manifest["files"][component] = json!(format!("{:x}", sha2::Sha256::digest(&bytes)));
        components.insert(component.into(), bytes);
        components.insert(
            "manifest.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        );
        let mut output = tar::Builder::new(flate2::write::GzEncoder::new(
            std::fs::File::create(&broken).unwrap(),
            flate2::Compression::default(),
        ));
        for (name, bytes) in components {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o600);
            h.set_cksum();
            output.append_data(&mut h, name, &bytes[..]).unwrap();
        }
        output.into_inner().unwrap().finish().unwrap();
        assert!(
            target
                .restore(broken.to_str().unwrap(), true, false)
                .await
                .is_err(),
            "{kind}"
        );
        assert!(target
            .store()
            .scroll_all_memories()
            .await
            .unwrap()
            .is_empty());
        assert!(!target_dir.path().join("evidence.sqlite3").exists());
    }
    // Real TCP transport failure after SQLite has been installed, not a mocked restore.
    let failure_dir = tempfile::tempdir().unwrap();
    let backend_url = target_config.qdrant.url.clone();
    assert!(backend_url.starts_with("http://127.0.0.1:"));
    let backend_addr = backend_url.trim_start_matches("http://").to_owned();
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cut = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let a = armed.clone();
    let c = cut.clone();
    let ledger_path = failure_dir.path().join("evidence.sqlite3");
    let proxy_task = tokio::spawn(async move {
        loop {
            let (mut incoming, _) = proxy.accept().await.unwrap();
            let mut outgoing = tokio::net::TcpStream::connect(&backend_addr).await.unwrap();
            let (a, c, path) = (a.clone(), c.clone(), ledger_path.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut reader, mut writer) = incoming.split();
                let (mut backend_reader, mut backend_writer) = outgoing.split();
                let forward = async {
                    let mut bytes = [0u8; 16384];
                    loop {
                        let n = reader.read(&mut bytes).await?;
                        if n == 0 {
                            break;
                        }
                        if a.load(std::sync::atomic::Ordering::SeqCst) && path.exists() {
                            c.store(true, std::sync::atomic::Ordering::SeqCst);
                            break;
                        }
                        backend_writer.write_all(&bytes[..n]).await?;
                    }
                    Ok::<_, std::io::Error>(())
                };
                tokio::select! {
                    _ = forward => {},
                    _ = tokio::io::copy(&mut backend_reader, &mut writer) => {}
                }
            });
        }
    });
    let mut failure_config = target_config.clone();
    failure_config.qdrant.url = format!("http://{proxy_addr}");
    failure_config.turbovec.index_path =
        failure_dir.path().join("memories.tv").display().to_string();
    failure_config.turbovec.id_map_path = failure_dir.path().join("ids.json").display().to_string();
    let failing = Engine::new(failure_config).await.unwrap();
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = failing
        .restore(&paired_backup, true, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        cut.load(std::sync::atomic::Ordering::SeqCst),
        "transport was not cut"
    );
    assert!(
        error.contains("restore write phase failed") && error.contains("discard BOTH"),
        "{error}"
    );
    let conn = rusqlite::Connection::open(failure_dir.path().join("evidence.sqlite3")).unwrap();
    assert!(
        conn.query_row("SELECT COUNT(*) FROM sources", [], |r| r.get::<_, i64>(0))
            .unwrap()
            > 0
    );
    assert!(failing
        .restore(&paired_backup, true, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("previous restore may be partial"));
    proxy_task.abort();
    // No Qdrant projection is guaranteed before transport loss; reset any that arrived.
    for memory in target.store().scroll_all_memories().await.unwrap() {
        target.store().delete_memory(&memory.id).await.unwrap();
    }
    assert!(target.restore(&paired_backup, true, false).await.unwrap() >= 2);
    assert_eq!(target.store().get_all_graph_edges().await.unwrap().len(), 1);
    let restored_realms = target.store().list_realms().await.unwrap();
    assert_eq!(restored_realms.len(), expected_realms.len());
    for expected in &expected_realms {
        let actual = restored_realms
            .iter()
            .find(|r| r.id == expected.id)
            .unwrap();
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.centroid, expected.centroid);
        assert_eq!(actual.description, expected.description);
        assert_eq!(actual.is_user_pinned, expected.is_user_pinned);
    }
    assert_eq!(
        serde_json::to_value(
            target
                .store()
                .get_consolidation(&journal.summary_id)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&journal).unwrap()
    );
    assert_eq!(
        target.read_evidence(id, source).await.unwrap().content,
        "exact\nexcerpt"
    );
    let retry_pending =
        memex8::storage::evidence::EvidenceStore::open(&target_dir.path().join("evidence.sqlite3"))
            .unwrap()
            .prepare(
                "backup-pending",
                "pending source",
                &pending_capture,
                &[],
                "private",
            )
            .unwrap();
    assert_eq!(retry_pending.memory_id, pending.memory_id);
    assert_eq!(retry_pending.source_id, pending.source_id);
    assert!(target
        .read_evidence(&pending.memory_id, &pending.source_id)
        .await
        .is_err());
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = socket.local_addr().unwrap().port();
    drop(socket);
    let restored_engine = target.clone();
    let target_server = tokio::spawn(async move {
        run_with_engine(target_config, restored_engine, "127.0.0.1", target_port)
            .await
            .unwrap()
    });
    let restored_endpoint =
        format!("http://127.0.0.1:{target_port}/api/v1/memories/{id}/evidence/{source}");
    for _ in 0..100 {
        if client.get(&restored_endpoint).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        client
            .get(&restored_endpoint)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let readback: Value = client
        .get(&restored_endpoint)
        .bearer_auth("fixture-only")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(readback, evidence);
    target_server.abort();
    assert!(target
        .restore(&paired_backup, true, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("empty"));
    assert!(engine
        .export(dir.path().join("export.json").to_str().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("evidence"));
    assert!(engine
        .restore("missing.tar.gz", true, false)
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
    // Cross-store deletion: API auth, projection, source, replay and search paths.
    assert_eq!(
        client
            .delete(format!("{base}/memories/{id}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/memories/{id}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert!(engine.store().get_memory(id).await.unwrap().is_none());
    assert!(engine.read_evidence(id, source).await.is_err());
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .delete(format!("{base}/memories/{id}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(engine
        .search("exact excerpt", None, None, 20, 0, 0.0)
        .await
        .unwrap()
        .iter()
        .all(|m| m.id != id));
    let ledger =
        memex8::storage::evidence::EvidenceStore::open(&dir.path().join("evidence.sqlite3"))
            .unwrap();
    assert!(!ledger.contains_memory(id).unwrap());
    // Prepared-only deletion and interrupted intent are reconciled before Engine::new returns.
    assert_eq!(
        client
            .delete(format!("{base}/memories/{memory_pending}"))
            .bearer_auth("fixture-only")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{base}/memories"))
            .bearer_auth("fixture-only")
            .json(&interrupted)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // A legacy memory has no ledger row: use a new evidence capture for the crash fixture.
    let crash_request = json!({"content":"crash fixture unique phrase", "idempotency_key":"crash-fixture", "evidence":request["evidence"]});
    let crash: Value = client
        .post(format!("{base}/memories"))
        .bearer_auth("fixture-only")
        .json(&crash_request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let crash_id = crash["id"].as_str().unwrap();
    assert_ne!(crash_id, legacy_id);
    // Persist a stale TurboVec index containing the doomed point; startup must
    // still reconcile and searches must not return its Qdrant payload.
    let snapshot = engine
        .store()
        .get_memory_snapshot(crash_id)
        .await
        .unwrap()
        .unwrap();
    let mut stale = memex8::engine::quantizer::TurboQuantVectorIndex::new(8, 4);
    stale.add_batch(&[crash_id.to_owned()], &[snapshot.vector]);
    stale
        .save(
            &engine.config().turbovec.index_path,
            &engine.config().turbovec.id_map_path,
        )
        .unwrap();
    ledger.begin_erase(crash_id).unwrap();
    assert!(engine.store().get_memory(crash_id).await.unwrap().is_some());
    server.abort();
    let restarted = Engine::new(engine.config().clone()).await.unwrap();
    assert!(restarted
        .store()
        .get_memory(crash_id)
        .await
        .unwrap()
        .is_none());
    assert!(ledger.pending_erasures().unwrap().is_empty());
    assert!(restarted
        .search("crash fixture unique phrase", None, None, 20, 0, 0.0)
        .await
        .unwrap()
        .iter()
        .all(|m| m.id != crash_id));
    assert!(restarted
        .store_with_evidence(
            "crash fixture unique phrase",
            vec![],
            None,
            None,
            None,
            Some("crash-fixture"),
            &pending_capture
        )
        .await
        .is_err());
    fixture.abort();
}
