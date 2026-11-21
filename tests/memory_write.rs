//! Run with a disposable Qdrant only:
//! MEMORY_WRITE_QDRANT_URL=http://127.0.0.1:PORT cargo test --test memory_write -- --ignored
//!
//! Behaviour of the one write entry point, `Engine::write_memory`, read back
//! through the public Engine interface. Qdrant-free checks (visibility parsing,
//! defaults) are unit tests in src/engine/write.rs.
use memex8::{
    config::AppConfig,
    engine::{Dedup, Engine, NewMemory, Visibility},
};
use serde_json::json;

async fn engine() -> (Engine, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let url = std::env::var("MEMORY_WRITE_QDRANT_URL").expect("disposable Qdrant URL");
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
    (Engine::new(config).await.unwrap(), dir, fixture)
}

#[tokio::test]
#[ignore = "requires explicit disposable loopback Qdrant"]
async fn write_memory_defaults_redaction_dedup_and_certification() {
    let (engine, _dir, fixture) = engine().await;
    let tag = uuid::Uuid::new_v4().to_string();

    // Defaults: private, `general`, source `manual`, not certified.
    let stored = engine
        .write_memory(NewMemory::new(&format!("alpha {tag}")))
        .await
        .unwrap();
    assert!(stored.created);
    let m = engine.get_memory(&stored.id).await.unwrap();
    assert_eq!(m.visibility, "private");
    assert_eq!(m.memory_type, "general");
    assert_eq!(m.source_file.as_deref(), Some("manual"));
    assert!(!m.is_certified());

    // An explicit visibility and certification are stored; a typo in the
    // memory type is stored as given (and only warned about).
    let m = engine
        .get_memory(
            &engine
                .write_memory(NewMemory {
                    visibility: Visibility::Public,
                    memory_type: Some("prefrence"),
                    certified: true,
                    ..NewMemory::new(&format!("bravo {tag}"))
                })
                .await
                .unwrap()
                .id,
        )
        .await
        .unwrap();
    assert_eq!(m.visibility, "public");
    assert_eq!(m.memory_type, "prefrence");
    assert!(m.is_certified());

    // Private spans are redacted once, in content, tags, source and type.
    let m = engine
        .get_memory(
            &engine
                .write_memory(NewMemory {
                    tags: vec![format!("t<private>x</private>{tag}")],
                    source: Some("s<private>x</private>"),
                    memory_type: Some("fact<private>x</private>"),
                    ..NewMemory::new(&format!("keep <private>secret</private> tail {tag}"))
                })
                .await
                .unwrap()
                .id,
        )
        .await
        .unwrap();
    assert_eq!(m.content, format!("keep [private] tail {tag}"));
    assert_eq!(m.tags, vec![format!("t[private]{tag}")]);
    assert_eq!(m.source_file.as_deref(), Some("s[private]"));
    assert_eq!(m.memory_type, "fact[private]");
    assert!(engine
        .write_memory(NewMemory::new("<private>everything</private>"))
        .await
        .is_err());

    // Dedup::Off always writes; Dedup::Identical answers with the existing memory.
    let content = format!("charlie {tag}");
    let once = |tags: &[&str], visibility| {
        let content = content.clone();
        let tags = tags.iter().map(|t| t.to_string()).collect();
        let engine = &engine;
        async move {
            engine
                .write_memory(NewMemory {
                    tags,
                    visibility,
                    dedup: Dedup::Identical,
                    ..NewMemory::new(&content)
                })
                .await
                .unwrap()
        }
    };
    let first = once(&["a", "b"], Visibility::Private).await;
    assert!(first.created);
    let again = once(&["b", "a", "a"], Visibility::Private).await;
    assert_eq!(
        (again.id.as_str(), again.created),
        (first.id.as_str(), false)
    );
    let other_tags = once(&["a"], Visibility::Private).await;
    assert!(other_tags.created && other_tags.id != first.id);
    let other_visibility = once(&["a", "b"], Visibility::Public).await;
    assert!(other_visibility.created && other_visibility.id != first.id);

    let plain = |n| engine.write_memory(NewMemory::new(n));
    let name = format!("delta {tag}");
    let (x, y) = (plain(&name).await.unwrap(), plain(&name).await.unwrap());
    assert!(x.created && y.created && x.id != y.id);

    fixture.abort();
}
