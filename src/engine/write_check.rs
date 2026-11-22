//! Detect a store that silently ignores writes.
//!
//! Qdrant can answer "completed" to an update and not apply it. That happened
//! to 98% of a live corpus: edits, deletes and access counts on old memories
//! all reported success and changed nothing, and nothing in memex8 noticed.
//!
//! [`check_writes`] writes a marker to a sample of memories and reads it back.
//! A memory that does not show its marker is ignoring writes. The check talks
//! to a [`WriteProbeTarget`] so its logic is tested against fakes; the
//! production adapter is the Qdrant store.
use super::Engine;
use crate::storage::qdrant::QdrantStore;
use async_trait::async_trait;

/// The few operations the check needs from a store.
#[async_trait]
pub trait WriteProbeTarget: Send + Sync {
    async fn all_ids(&self) -> anyhow::Result<Vec<String>>;
    async fn set_marker(&self, id: &str, nonce: &str) -> anyhow::Result<()>;
    async fn read_marker(&self, id: &str) -> anyhow::Result<Option<String>>;
    async fn clear_marker(&self, id: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct WriteCheck {
    /// Memories probed.
    pub checked: usize,
    /// Memories whose marker did not read back: they ignore writes.
    pub ignored: Vec<String>,
    /// Memories the probe could not complete (the store returned an error).
    pub errors: usize,
}

impl WriteCheck {
    pub fn is_healthy(&self) -> bool {
        self.ignored.is_empty() && self.errors == 0
    }
}

/// Probe up to `max` memories, spread evenly across the corpus so old and new
/// memories are both covered.
pub async fn check_writes(target: &dyn WriteProbeTarget, max: usize) -> anyhow::Result<WriteCheck> {
    let ids = target.all_ids().await?;
    let step = ids.len().div_ceil(max.max(1)).max(1);
    let mut result = WriteCheck::default();
    for id in ids.iter().step_by(step) {
        result.checked += 1;
        let nonce = uuid::Uuid::new_v4().to_string();
        if target.set_marker(id, &nonce).await.is_err() {
            result.errors += 1;
            continue;
        }
        match target.read_marker(id).await {
            Ok(Some(seen)) if seen == nonce => {
                // Best effort: a marker left behind is harmless.
                let _ = target.clear_marker(id).await;
            }
            Ok(_) => result.ignored.push(id.clone()),
            Err(_) => result.errors += 1,
        }
    }
    Ok(result)
}

#[async_trait]
impl WriteProbeTarget for QdrantStore {
    async fn all_ids(&self) -> anyhow::Result<Vec<String>> {
        self.all_memory_ids().await
    }
    async fn set_marker(&self, id: &str, nonce: &str) -> anyhow::Result<()> {
        self.set_write_probe(id, nonce).await
    }
    async fn read_marker(&self, id: &str) -> anyhow::Result<Option<String>> {
        self.read_write_probe(id).await
    }
    async fn clear_marker(&self, id: &str) -> anyhow::Result<()> {
        self.clear_write_probe(id).await
    }
}

impl Engine {
    /// Probe up to `max` memories for silently ignored writes.
    pub async fn check_storage_writes(&self, max: usize) -> anyhow::Result<WriteCheck> {
        check_writes(&self.store, max).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// An in-memory store. Ids in `ignoring` accept every write and apply none.
    #[derive(Default)]
    struct Fake {
        markers: Mutex<HashMap<String, String>>,
        ids: Vec<String>,
        ignoring: HashSet<String>,
        failing: HashSet<String>,
    }

    impl Fake {
        fn with(n: usize) -> Self {
            Self {
                ids: (0..n).map(|i| format!("m{i:03}")).collect(),
                ..Default::default()
            }
        }
        fn leftover_markers(&self) -> usize {
            self.markers.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl WriteProbeTarget for Fake {
        async fn all_ids(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.ids.clone())
        }
        async fn set_marker(&self, id: &str, nonce: &str) -> anyhow::Result<()> {
            anyhow::ensure!(!self.failing.contains(id), "store error");
            if !self.ignoring.contains(id) {
                self.markers.lock().unwrap().insert(id.into(), nonce.into());
            }
            Ok(())
        }
        async fn read_marker(&self, id: &str) -> anyhow::Result<Option<String>> {
            Ok(self.markers.lock().unwrap().get(id).cloned())
        }
        async fn clear_marker(&self, id: &str) -> anyhow::Result<()> {
            if !self.ignoring.contains(id) {
                self.markers.lock().unwrap().remove(id);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_healthy_store_passes_and_leaves_no_markers() {
        let store = Fake::with(20);
        let check = check_writes(&store, 100).await.unwrap();
        assert_eq!(
            check,
            WriteCheck {
                checked: 20,
                ..Default::default()
            }
        );
        assert!(check.is_healthy());
        assert_eq!(store.leftover_markers(), 0);
    }

    #[tokio::test]
    async fn memories_that_ignore_writes_are_named() {
        let mut store = Fake::with(10);
        store.ignoring = ["m002".to_string(), "m007".to_string()].into();
        let check = check_writes(&store, 100).await.unwrap();
        assert_eq!(check.checked, 10);
        assert_eq!(check.ignored, vec!["m002".to_string(), "m007".to_string()]);
        assert!(!check.is_healthy());
    }

    #[tokio::test]
    async fn a_store_that_ignores_everything_is_reported_for_every_sample() {
        let mut store = Fake::with(8);
        store.ignoring = store.ids.iter().cloned().collect();
        let check = check_writes(&store, 100).await.unwrap();
        assert_eq!(check.ignored.len(), 8);
    }

    #[tokio::test]
    async fn store_errors_are_counted_not_mistaken_for_ignored_writes() {
        let mut store = Fake::with(5);
        store.failing = ["m001".to_string()].into();
        let check = check_writes(&store, 100).await.unwrap();
        assert_eq!(
            (check.checked, check.errors, check.ignored.len()),
            (5, 1, 0)
        );
        assert!(!check.is_healthy());
    }

    #[tokio::test]
    async fn the_sample_is_capped_and_spread_across_the_corpus() {
        let mut store = Fake::with(1000);
        // Only the last memories ignore writes: a sample of the first N would miss them.
        store.ignoring = (900..1000).map(|i| format!("m{i:03}")).collect();
        let check = check_writes(&store, 50).await.unwrap();
        assert!(check.checked <= 50, "checked {}", check.checked);
        assert!(
            !check.ignored.is_empty(),
            "a spread sample must reach the tail"
        );
    }

    #[tokio::test]
    async fn an_empty_store_is_healthy() {
        let check = check_writes(&Fake::with(0), 10).await.unwrap();
        assert_eq!(check, WriteCheck::default());
    }
}
