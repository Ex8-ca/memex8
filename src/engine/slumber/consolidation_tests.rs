//! Behavioral tests against an explicitly supplied, disposable Qdrant.
use super::*;
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Fixture {
    engine: SlumberEngine,
    prompts: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(vector: Vec<f32>) -> Self {
        let url = std::env::var("MEMEX8_TEST_QDRANT_URL")
            .expect("provide a disposable Qdrant URL; tests erase its memories collection");
        let store = QdrantStore::new(&url).await.unwrap();
        store.ensure_collections(3).await.unwrap();
        for memory in store.scroll_all_memories().await.unwrap() {
            store.delete_memory(&memory.id).await.unwrap();
        }
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let captured = prompts.clone();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(move |Json(body): Json<Value>| {
                    let captured = captured.clone();
                    async move {
                        captured
                            .lock()
                            .unwrap()
                            .push(body["messages"][1]["content"].as_str().unwrap().to_owned());
                        Json(json!({"choices":[{"message":{"content":"A derived summary."}}]}))
                    }
                }),
            )
            .route(
                "/api/embed",
                post(move || {
                    let vector = vector.clone();
                    async move { Json(json!({"embeddings":[vector]})) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // These tests run serially in an isolated process, never against real providers.
        std::env::set_var("LOCAL_LLM_URL", &http_url);
        std::env::remove_var("LOCAL_LLM_API_KEY");
        std::env::set_var("EMBEDDING_PROVIDER", "ollama");
        let mut config = AppConfig::default();
        config.embedding.ollama.url = http_url;
        config.embedding.dimensions = 3;
        config.slumber.consolidation.backend = "local".into();
        config.slumber.consolidation.enabled = true;
        config.slumber.summarize.max_cluster_size = 10;
        config.slumber.summarize.preserve_originals = true;
        Self {
            engine: SlumberEngine::new(config, store),
            prompts,
            server,
        }
    }

    async fn seed(&self, count: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..count {
            let id = uuid::Uuid::new_v4().to_string();
            self.engine
                .store
                .store_memory(
                    &id,
                    &[1.0, 0.0, 0.0],
                    &format!("Original fact {i}"),
                    None,
                    None,
                    "realm-a",
                    "same-name",
                    "",
                    "fact",
                    "fact",
                    0.0,
                    "private",
                )
                .await
                .unwrap();
            ids.push(id);
        }
        ids
    }

    async fn assert_originals(&self, ids: &[String]) {
        for id in ids {
            assert!(
                self.engine.store.get_memory(id).await.unwrap().is_some(),
                "consolidation deleted original {id}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_preserves_unrepresented_sources() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(12).await;
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 1);
    let prompts = f.prompts.lock().unwrap();
    assert_eq!(
        ids.iter()
            .filter(|id| prompts[0].contains(id.as_str()))
            .count(),
        10
    );
    drop(prompts);
    f.assert_originals(&ids).await;
    let all = f.engine.store.scroll_all_memories().await.unwrap();
    let summary = all.iter().find(|m| m.chunk_type == "consolidated").unwrap();
    assert_eq!(summary.realm_id.as_deref(), Some("realm-a"));
    assert_eq!(summary.visibility, "private");
    let mut expected: Vec<_> = ids
        .iter()
        .filter(|id| f.prompts.lock().unwrap()[0].contains(id.as_str()))
        .cloned()
        .collect();
    expected.sort();
    let mut actual = summary.consolidated_from.clone();
    actual.sort();
    assert_eq!(
        actual, expected,
        "lineage must identify exactly the prompt inputs"
    );
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_failed_storage_preserves_sources() {
    let f = Fixture::new(vec![1.0, 0.0]).await; // wrong dimensions: Qdrant rejects save
    let ids = f.seed(2).await;
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 0);
    f.assert_originals(&ids).await;
}

#[test]
fn consolidation_requires_explicit_opt_in() {
    let config = crate::config::ConsolidationConfig::default();
    assert_eq!(serde_json::to_value(config).unwrap()["enabled"], false);
    let legacy: crate::config::ConsolidationConfig =
        serde_json::from_value(json!({"backend":"local"})).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap()["enabled"], false);
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_skips_incompatible_boundaries() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    let mut second = f.engine.store.get_memory(&ids[1]).await.unwrap().unwrap();
    for (realm_id, visibility, tags) in [
        ("realm-a", "public", vec![]),
        ("realm-b", "private", vec![]),
        ("realm-a", "private", vec!["project:excluded".to_owned()]),
        ("realm-a", "private", vec!["scope:unknown".to_owned()]),
    ] {
        second.realm_id = Some(realm_id.into());
        second.visibility = visibility.into();
        second.tags = tags;
        f.engine
            .store
            .restore_memory(&crate::storage::qdrant::MemoryWithVector {
                memory: second.clone(),
                vector: vec![1.0, 0.0, 0.0],
            })
            .await
            .unwrap();
        assert_eq!(
            f.engine.llm_consolidate().await.unwrap(),
            0,
            "must skip incompatible or unknown scope"
        );
    }
    assert!(f.prompts.lock().unwrap().is_empty());
    f.assert_originals(&ids).await;
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_excludes_legacy_summaries() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    let mut memory = f.engine.store.get_memory(&ids[1]).await.unwrap().unwrap();
    memory.chunk_type = "consolidated".into();
    f.engine
        .store
        .restore_memory(&crate::storage::qdrant::MemoryWithVector {
            memory,
            vector: vec![1.0, 0.0, 0.0],
        })
        .await
        .unwrap();
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 0);
    assert!(f.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_does_not_repeat_identical_inputs() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(10).await;
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 1);
    // Ranking changes within the same ten inputs must not regenerate a summary.
    let all = f.engine.store.scroll_all_memories().await.unwrap();
    let summary = all.iter().find(|m| m.chunk_type == "consolidated").unwrap();
    let input = summary.consolidated_from.last().unwrap();
    f.engine
        .store
        .set_memory_importance(input, 100.0)
        .await
        .unwrap();
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 0);
    f.assert_originals(&ids).await;
    assert_eq!(f.prompts.lock().unwrap().len(), 1);
    assert_eq!(
        f.engine.store.scroll_all_memories().await.unwrap().len(),
        11
    );
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_generation_failure_preserves_sources() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    f.server.abort();
    tokio::task::yield_now().await;
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 0);
    f.assert_originals(&ids).await;
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_embedding_failure_preserves_sources() {
    let mut f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    f.engine.config.embedding.ollama.url = "invalid://no-embedding-service".into();
    assert!(f.engine.llm_consolidate().await.is_err());
    assert_eq!(f.prompts.lock().unwrap().len(), 1);
    f.assert_originals(&ids).await;
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_public_output_keeps_visibility() {
    let f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    for id in &ids {
        let mut memory = f.engine.store.get_memory(id).await.unwrap().unwrap();
        memory.visibility = "public".into();
        f.engine
            .store
            .restore_memory(&crate::storage::qdrant::MemoryWithVector {
                memory,
                vector: vec![1.0, 0.0, 0.0],
            })
            .await
            .unwrap();
    }
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 1);
    let all = f.engine.store.scroll_all_memories().await.unwrap();
    assert_eq!(
        all.iter()
            .find(|m| m.chunk_type == "consolidated")
            .unwrap()
            .visibility,
        "public"
    );
    f.assert_originals(&ids).await;
}

#[tokio::test]
#[ignore = "requires disposable Qdrant; run serially with MEMEX8_TEST_QDRANT_URL"]
async fn consolidation_disabled_does_not_contact_providers() {
    let mut f = Fixture::new(vec![1.0, 0.0, 0.0]).await;
    let ids = f.seed(2).await;
    f.engine.config.slumber.consolidation.enabled = false;
    assert_eq!(f.engine.llm_consolidate().await.unwrap(), 0);
    assert!(f.prompts.lock().unwrap().is_empty());
    f.assert_originals(&ids).await;
}
