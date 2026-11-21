//! The compressed (TurboVec) vector index: a persisted file pair plus the copy
//! this process has loaded, kept honest about the corpus it describes.
//!
//! Qdrant is authoritative. The index only speeds search up, so any doubt
//! (stale files, a racing write, a changed sidecar) means "search Qdrant".
//!
//! Freshness has three parts:
//! - the persisted pair carries a corpus fingerprint and file digests
//!   ([`CompressedIndexMeta`]) that are re-checked against the live corpus
//!   before an index is ever installed;
//! - a generation counter rejects a reload that raced a write (every write
//!   invalidates after its mutation is durable);
//! - the sidecar stamp recorded at load is re-checked before each use, so a
//!   write or slumber run in another process (the CLI) that removes or
//!   rewrites the pair stops this process from serving the old map.
//!
//! Callers keep the index honest through [`CompressedIndex::around_write`] for a
//! single corpus write, or [`CompressedIndex::invalidate`] around a longer
//! maintenance operation.
use crate::config::AppConfig;
use crate::engine::{dedup_removal, quantizer};
use crate::storage::qdrant::QdrantStore;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{RwLock, RwLockReadGuard};

/// Whether a persisted compressed index may be trusted for the live corpus.
///
/// Counting is not enough. TurboVec is preferred over Qdrant in the search path,
/// so a stale index does not merely return old results — it returns *fewer*
/// results. Slumber's consolidation deletes source memories and inserts
/// summaries, which keeps the vector count roughly stable while changing every
/// ID in the affected region: a count-only check accepted exactly that index and
/// left the summaries unsearchable after a restart. Compare the ID sets instead.
/// Any disagreement rejects the index and search falls back to Qdrant, which is
/// always authoritative.
pub fn turbovec_index_matches_corpus(indexed: &[String], live: &HashSet<String>) -> bool {
    indexed.len() == live.len() && indexed.iter().all(|id| live.contains(id))
}

/// Configured compressed-index file pair, including the defaults used when the
/// config leaves the paths empty.
pub(crate) fn compressed_index_paths(config: &AppConfig) -> (String, String) {
    let index_path = if config.turbovec.index_path.is_empty() {
        "data/memories.tv".to_string()
    } else {
        config.turbovec.index_path.clone()
    };
    let id_map_path = if config.turbovec.id_map_path.is_empty() {
        "data/memories_ids.json".to_string()
    } else {
        config.turbovec.id_map_path.clone()
    };
    (index_path, id_map_path)
}

/// Sidecar that records which corpus a persisted index pair was built from.
///
/// The pair alone is not self-describing: the `.tv` holds quantized codes and the
/// id map holds slot→UUID, so stale *vectors* with unchanged IDs are invisible to
/// an ID-set check. The sidecar carries a fingerprint of the corpus content (not
/// `source_hash`, which `edit_memory` never updates) plus byte digests of both
/// files, so a load can prove the pair describes this corpus.
pub(crate) fn compressed_index_meta_path(index_path: &str) -> String {
    format!("{index_path}.meta.json")
}

/// Marker written when an invalidation could not unlink the pair. Its existence
/// alone rejects the pair at load; a successful rebuild removes it.
pub(crate) fn compressed_index_stale_marker(index_path: &str) -> String {
    format!("{index_path}.stale")
}

const COMPRESSED_INDEX_META_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CompressedIndexMeta {
    pub version: u32,
    /// Number of vectors in the `.tv` at the time it was written.
    pub vectors: usize,
    /// Number of IDs in the companion map; must equal `vectors`.
    pub id_map_len: usize,
    pub dimensions: usize,
    pub bit_width: usize,
    /// SHA-256 over the (id, content) pairs of the corpus this pair was built for.
    pub fingerprint: String,
    /// SHA-256 of the `.tv` bytes: catches a file swapped in from elsewhere.
    pub index_digest: String,
    /// SHA-256 of the id-map bytes.
    pub id_map_digest: String,
}

impl CompressedIndexMeta {
    /// Prove the sidecar describes exactly these two files.
    fn validate_pair(
        &self,
        index_path: &str,
        id_map_path: &str,
        index: &quantizer::TurboQuantVectorIndex,
        dimensions: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == COMPRESSED_INDEX_META_VERSION,
            "unsupported sidecar version {}",
            self.version
        );
        anyhow::ensure!(
            index.slots() == index.ids().len()
                && self.vectors == index.slots()
                && self.id_map_len == index.ids().len(),
            "sidecar describes {} vectors / {} ids but the files hold {} vectors / {} ids",
            self.vectors,
            self.id_map_len,
            index.slots(),
            index.ids().len()
        );
        anyhow::ensure!(
            self.dimensions == dimensions && index.dimensions() == dimensions,
            "index is {}-dimensional but this engine embeds {dimensions}d",
            index.dimensions()
        );
        anyhow::ensure!(
            self.bit_width == index.bit_width(),
            "sidecar says {}-bit but the index file is {}-bit",
            self.bit_width,
            index.bit_width()
        );
        anyhow::ensure!(
            self.index_digest == file_digest(index_path)?,
            "{index_path} was replaced after the sidecar was written"
        );
        anyhow::ensure!(
            self.id_map_digest == file_digest(id_map_path)?,
            "{id_map_path} was replaced after the sidecar was written"
        );
        Ok(())
    }
}

fn file_digest(path: &str) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Fingerprint of the corpus content an index was built for.
///
/// Covers every memory's ID and stored content, order-independently. Content is
/// what `edit_memory` rewrites and what a rebuild re-embeds, so an edit invalidates
/// the pair even when `source_hash` is left alone. A vector changed without a
/// content change is not representable here; Qdrant stays authoritative for the
/// payload, so that only costs ranking fidelity, never visibility.
pub(crate) fn corpus_fingerprint(memories: &[crate::storage::qdrant::MemoryPoint]) -> String {
    fingerprint_pairs(memories.iter().map(|m| (m.id.as_str(), m.content.as_str())))
}

fn fingerprint_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    use sha2::{Digest, Sha256};
    let mut pairs: Vec<(&str, &str)> = pairs.collect();
    pairs.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"memex8-compressed-index-v1");
    for (id, content) in pairs {
        hasher.update((id.len() as u64).to_le_bytes());
        hasher.update(id.as_bytes());
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn read_compressed_index_meta(index_path: &str) -> anyhow::Result<Option<CompressedIndexMeta>> {
    let path = compressed_index_meta_path(index_path);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(serde_json::from_str(&text)?))
}

/// Build the compressed search index from the live corpus and persist it.
///
/// This is the *only* writer of the index files, and every caller runs it after
/// its last corpus mutation: an index written before a consolidation would point
/// at deleted sources and miss the summaries that replaced them.
///
/// The pair is written to temporary siblings and renamed, so a crash mid-write
/// leaves either the previous pair or nothing — never a new index beside an old
/// ID map. An empty corpus, or a failed build, removes the previous files rather
/// than leaving an index that no longer describes the corpus; search falls back
/// to Qdrant, which is complete.
pub(crate) async fn rebuild(store: &QdrantStore, config: &AppConfig) -> anyhow::Result<usize> {
    let (index_path, id_map_path) = compressed_index_paths(config);
    // The fingerprint covers the whole point set (including memories without a
    // usable vector, which stay out of the index) so build-time and load-time
    // fingerprints are computed from the same view.
    let corpus = match store.scroll_all_memories().await {
        Ok(corpus) => corpus,
        Err(e) => {
            let _ = dedup_removal::invalidate_index_files(&index_path, &id_map_path);
            return Err(e);
        }
    };
    let built = build_compressed_index(store, config.turbovec.bit_width).await;
    let index = match built {
        Ok(Some(index)) => index,
        Ok(None) => {
            dedup_removal::invalidate_index_files(&index_path, &id_map_path)?;
            tracing::info!("  No memories to index; removed the compressed index");
            return Ok(0);
        }
        Err(e) => {
            dedup_removal::invalidate_index_files(&index_path, &id_map_path)?;
            return Err(e);
        }
    };
    if let Err(e) = save_compressed_index(
        &index,
        &index_path,
        &id_map_path,
        &corpus_fingerprint(&corpus),
    ) {
        let _ = dedup_removal::invalidate_index_files(&index_path, &id_map_path);
        return Err(e);
    }
    tracing::info!(
        "  Saved TurboVec index: {} vectors → {} ({:.1}x compression)",
        index.vector_count(),
        index_path,
        index.compression_ratio()
    );
    Ok(index.vector_count())
}

/// Compress every memory that has a usable vector. Memories without one cannot
/// be indexed; they are counted and warned about, and the resulting ID-set
/// mismatch at load time keeps search on Qdrant rather than silently omitting
/// them.
async fn build_compressed_index(
    store: &QdrantStore,
    bit_width: usize,
) -> anyhow::Result<Option<quantizer::TurboQuantVectorIndex>> {
    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    let mut dims = 0usize;
    let mut skipped = 0usize;
    for memory in store.scroll_all_memories_with_vectors().await? {
        if memory.vector.is_empty() {
            skipped += 1;
            continue;
        }
        if dims == 0 {
            dims = memory.vector.len();
        }
        if memory.vector.len() != dims {
            skipped += 1;
            continue;
        }
        rows.push((memory.memory.id, memory.vector));
    }
    if skipped > 0 {
        tracing::warn!(
            "  {} memories have no usable embedding and stay out of the compressed index",
            skipped
        );
    }
    if rows.is_empty() {
        return Ok(None);
    }
    let mut index = quantizer::TurboQuantVectorIndex::new(dims, bit_width);
    for chunk in rows.chunks(256) {
        let ids: Vec<String> = chunk.iter().map(|(id, _)| id.clone()).collect();
        let vectors: Vec<Vec<f32>> = chunk.iter().map(|(_, v)| v.clone()).collect();
        index.add_batch(&ids, &vectors);
    }
    Ok(Some(index))
}

fn save_compressed_index(
    index: &quantizer::TurboQuantVectorIndex,
    index_path: &str,
    id_map_path: &str,
    fingerprint: &str,
) -> anyhow::Result<()> {
    for path in [index_path, id_map_path] {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let temp_index = format!("{}.tmp", index_path);
    let temp_ids = format!("{}.tmp", id_map_path);
    if let Err(e) = index.save(&temp_index, &temp_ids) {
        let _ = std::fs::remove_file(&temp_index);
        let _ = std::fs::remove_file(&temp_ids);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp_index, index_path)
        .and_then(|()| std::fs::rename(&temp_ids, id_map_path))
    {
        let _ = std::fs::remove_file(&temp_index);
        let _ = std::fs::remove_file(&temp_ids);
        return Err(e.into());
    }
    // Written last, and only after the pair is in place: the digests describe
    // exactly the bytes now on disk. A pair without a sidecar is never loaded.
    let meta = CompressedIndexMeta {
        version: COMPRESSED_INDEX_META_VERSION,
        vectors: index.vector_count(),
        id_map_len: index.ids().len(),
        dimensions: index.dimensions(),
        bit_width: index.bit_width(),
        fingerprint: fingerprint.to_string(),
        index_digest: file_digest(index_path)?,
        id_map_digest: file_digest(id_map_path)?,
    };
    let meta_path = compressed_index_meta_path(index_path);
    let temp_meta = format!("{meta_path}.tmp");
    std::fs::write(&temp_meta, serde_json::to_vec(&meta)?)?;
    if let Err(e) = std::fs::rename(&temp_meta, &meta_path) {
        let _ = std::fs::remove_file(&temp_meta);
        return Err(e.into());
    }
    // A successful rebuild supersedes any failure marker from an earlier write.
    let _ = std::fs::remove_file(compressed_index_stale_marker(index_path));
    Ok(())
}

/// A compressed index installed in this process, with the sidecar stamp it was
/// loaded under.
pub(crate) struct LoadedIndex {
    index: quantizer::TurboQuantVectorIndex,
    /// `(len, mtime)` of the sidecar when this index was validated. `None` when
    /// the filesystem reports no mtime, in which case the index is never used.
    stamp: Option<(u64, std::time::SystemTime)>,
}

/// Size and modification time of the pair's sidecar. Every rebuild rewrites the
/// sidecar and every invalidation removes it, so a changed or missing stamp
/// means the files on disk no longer describe the index held in memory.
fn compressed_index_stamp(index_path: &str) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(compressed_index_meta_path(index_path)).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Load the persisted pair and install it only if it provably describes the
/// live corpus (sidecar, file digests, content fingerprint, ID set). Any doubt
/// returns `None` and search stays on Qdrant, which is complete.
async fn load_verified_compressed_index(
    config: &AppConfig,
    store: &QdrantStore,
    dims: usize,
) -> Option<LoadedIndex> {
    let (index_path, id_map_path) = compressed_index_paths(config);
    let bit_width = config.turbovec.bit_width;
    let nothing = None;

    if !std::path::Path::new(&index_path).exists() || !std::path::Path::new(&id_map_path).exists() {
        tracing::debug!(
            "TurboVec index not found at {} (will create on first slumber)",
            index_path
        );
        return nothing;
    }
    // A failed unlink leaves files behind; the marker written at that moment
    // is what keeps them from being trusted.
    let marker = compressed_index_stale_marker(&index_path);
    if std::path::Path::new(&marker).exists() {
        tracing::warn!(
            "⚠️ Compressed index pair at {} was marked stale by an invalidation that \
             could not remove it ({}); ignoring it and serving search from Qdrant \
             until the next slumber rebuilds it",
            index_path,
            marker
        );
        return nothing;
    }
    let index =
        match quantizer::TurboQuantVectorIndex::load(&index_path, &id_map_path, dims, bit_width) {
            Ok(index) => index,
            Err(e) => {
                // NotFound is handled above, so anything here is a corrupt or
                // mixed pair: never silently treat it as "no index yet".
                tracing::warn!(
                    "⚠️ Compressed index at {} is unreadable or inconsistent ({e}); \
                 ignoring it and falling back to Qdrant until the next slumber rebuilds it",
                    index_path
                );
                return nothing;
            }
        };
    // Taken before the sidecar is read: if the pair is rewritten while this
    // load runs, the stamp no longer matches and the index is never used.
    let stamp = compressed_index_stamp(&index_path);
    let meta = match read_compressed_index_meta(&index_path) {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            tracing::warn!(
                "⚠️ Compressed index at {} has no fingerprint sidecar, so it cannot be \
                 proven fresh; ignoring it and falling back to Qdrant",
                index_path
            );
            return nothing;
        }
        Err(e) => {
            tracing::warn!(
                "⚠️ Compressed index sidecar for {} could not be read ({e}); \
                 ignoring the pair and falling back to Qdrant",
                index_path
            );
            return nothing;
        }
    };
    if let Err(e) = meta.validate_pair(&index_path, &id_map_path, &index, dims) {
        tracing::warn!(
            "⚠️ Compressed index at {} does not match its own sidecar ({e}); \
             ignoring it and falling back to Qdrant until the next slumber rebuilds it",
            index_path
        );
        return nothing;
    }
    // Without the live corpus the index cannot be proven fresh, and a stale
    // index hides memories instead of returning old ones.
    let live = match store.scroll_all_memories().await {
        Ok(memories) => memories,
        Err(e) => {
            tracing::warn!(
                "⚠️ Could not read the live corpus to validate {} ({e}); \
                 ignoring it and serving search from Qdrant",
                index_path
            );
            return nothing;
        }
    };
    if meta.fingerprint != corpus_fingerprint(&live) {
        tracing::warn!(
            "⚠️ TurboVec index at {} was built for a different corpus: memory content \
             changed since the rebuild ({} live memories); ignoring it and falling back \
             to Qdrant until the next slumber rebuilds it",
            index_path,
            live.len()
        );
        return nothing;
    }
    let live_ids: HashSet<String> = live.into_iter().map(|m| m.id).collect();
    if turbovec_index_matches_corpus(index.ids(), &live_ids) {
        tracing::info!(
            "📦 Loaded TurboVec index: {} vectors from {} ({}d @ {}-bit, {:.1}x compression)",
            index.vector_count(),
            index_path,
            dims,
            bit_width,
            index.compression_ratio()
        );
        Some(LoadedIndex { index, stamp })
    } else {
        let stale = index
            .ids()
            .iter()
            .filter(|id| !live_ids.contains(*id))
            .count();
        tracing::warn!(
            "⚠️ TurboVec index at {} holds {} vectors for {} live memories \
             ({} indexed IDs are no longer live); ignoring it and falling back \
             to Qdrant until the next slumber rebuilds it",
            index_path,
            index.vector_count(),
            live_ids.len(),
            stale
        );
        nothing
    }
}

/// The loaded index and the generation that guards installing a new one.
///
/// Kept apart from [`CompressedIndex`] so the generation race can be tested
/// without Qdrant or files on disk.
struct IndexSlot {
    loaded: RwLock<Option<LoadedIndex>>,
    /// Bumped (under the `loaded` write lock) by every [`IndexSlot::clear`].
    generation: AtomicU64,
}

impl IndexSlot {
    fn new(loaded: Option<LoadedIndex>) -> Self {
        Self {
            loaded: RwLock::new(loaded),
            generation: AtomicU64::new(0),
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    async fn clear(&self) {
        let mut guard = self.loaded.write().await;
        *guard = None;
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Install `loaded` unless a [`clear`](Self::clear) happened since
    /// `started_at` was read. Returns whether it was installed.
    ///
    /// Race-free with concurrent writes because every write clears after its
    /// mutation is durable, and clearing bumps the generation under the same
    /// lock this install takes. A write that landed before the corpus read
    /// makes the fingerprint check fail; one that landed after it bumps the
    /// generation either before the install (rejected here) or after it (the
    /// installed index is dropped again). Either way no stale map survives.
    async fn install_if_current(&self, started_at: u64, loaded: LoadedIndex) -> bool {
        let mut guard = self.loaded.write().await;
        if self.generation() != started_at {
            return false;
        }
        *guard = Some(loaded);
        true
    }
}

/// The compressed index as one process sees it.
pub(crate) struct CompressedIndex {
    slot: IndexSlot,
    config: AppConfig,
    store: QdrantStore,
    dimensions: usize,
}

/// A read hold on the loaded index. Held across the Qdrant fetches that
/// follow a lookup, so an invalidation waits for the search to finish.
pub(crate) struct IndexRead<'a> {
    guard: RwLockReadGuard<'a, Option<LoadedIndex>>,
    index_path: String,
}

impl CompressedIndex {
    /// Load the persisted pair if it provably describes the live corpus;
    /// otherwise start empty and search Qdrant.
    pub(crate) async fn load(config: &AppConfig, store: &QdrantStore, dimensions: usize) -> Self {
        let loaded = load_verified_compressed_index(config, store, dimensions).await;
        Self {
            slot: IndexSlot::new(loaded),
            config: config.clone(),
            store: store.clone_store(),
            dimensions,
        }
    }

    pub(crate) async fn read(&self) -> IndexRead<'_> {
        IndexRead {
            guard: self.slot.loaded.read().await,
            index_path: compressed_index_paths(&self.config).0,
        }
    }

    /// Discard the loaded index and the files behind it after a corpus
    /// mutation. Qdrant stays authoritative while they are rebuilt.
    ///
    /// Lock order is `maintenance gate → compressed index → lexical`. Callers
    /// that already hold the gate call this before releasing it; search takes
    /// the same order (gate, then the index), so no cycle exists. In-memory
    /// state is always dropped; if the files on disk cannot be unlinked, a
    /// stale marker is written so the pair is rejected at the next load
    /// instead of being trusted.
    ///
    /// Every mutation that changes content, a vector, or which IDs are live
    /// must invalidate. Payload-only updates (importance, access counts,
    /// verification stamps) do not: they change neither the vector nor the ID
    /// membership.
    pub(crate) async fn invalidate(&self) {
        self.slot.clear().await;
        let (index_path, id_map_path) = compressed_index_paths(&self.config);
        if let Err(e) = dedup_removal::invalidate_index_files(&index_path, &id_map_path) {
            // A pair that survives a failed unlink must never be accepted again.
            // The marker is the load path's first check; if even that write fails
            // the pair is still rejected, because its stored fingerprint no longer
            // matches the corpus that just changed.
            let marker = compressed_index_stale_marker(&index_path);
            match std::fs::write(&marker, format!("invalidation failed: {e}\n")) {
                Ok(()) => tracing::warn!(
                    "Compressed index files could not be removed ({e}); wrote {marker} \
                     so the pair is rejected at startup"
                ),
                Err(marker_error) => tracing::error!(
                    "Compressed index files could not be removed ({e}) and the stale marker \
                     could not be written ({marker_error}); the leftover pair is still rejected \
                     at startup by its corpus fingerprint"
                ),
            }
        }
    }

    /// Run one corpus write between two invalidations: the first so search
    /// stops using a map that is about to be wrong, the second once the write
    /// is durable, so a reload that verified the corpus before the write landed
    /// cannot keep its map. The second runs even when the write fails, since a
    /// failed write may still have changed the corpus.
    pub(crate) async fn around_write<T>(&self, write: impl Future<Output = T>) -> T {
        self.invalidate().await;
        let out = write.await;
        self.invalidate().await;
        out
    }

    /// Install the pair a rebuild just wrote, so this process searches it
    /// without a restart. Skipped when the corpus changed while it was being
    /// verified (see [`IndexSlot::install_if_current`]).
    pub(crate) async fn reload(&self) {
        let started_at = self.slot.generation();
        let Some(loaded) =
            load_verified_compressed_index(&self.config, &self.store, self.dimensions).await
        else {
            return;
        };
        if !self.slot.install_if_current(started_at, loaded).await {
            tracing::info!(
                "Compressed index reload skipped: the corpus changed while it was \
                 being verified; searching Qdrant until the next rebuild"
            );
        }
    }

    /// Rebuild the persisted pair from the live corpus and install it. Called
    /// after multi-memory maintenance (slumber, import, restore) whose last
    /// mutation is already durable.
    pub(crate) async fn rebuild_and_reload(&self) {
        self.invalidate().await;
        match rebuild(&self.store, &self.config).await {
            Ok(_) => self.reload().await,
            Err(e) => tracing::warn!("Compressed index rebuild failed: {e}"),
        }
    }
}

impl IndexRead<'_> {
    /// Nearest `fetch_limit` memories as `(id, score)`, or `None` when no
    /// trustworthy index is loaded (none installed, or the sidecar on disk no
    /// longer matches the one this index was loaded under).
    ///
    /// The handle is only installed after the persisted pair was validated
    /// against this corpus, and every in-process mutation drops it. A process
    /// sharing the data volume (the CLI) cannot reach this handle, but its
    /// writes remove the sidecar and its rebuilds rewrite it, so the stamp
    /// check catches those.
    pub(crate) fn search(&self, query: &[f32], fetch_limit: usize) -> Option<Vec<(String, f32)>> {
        let loaded = self.guard.as_ref()?;
        let current =
            loaded.stamp.is_some() && loaded.stamp == compressed_index_stamp(&self.index_path);
        if !current {
            tracing::debug!(
                "Compressed index sidecar changed on disk since load; searching Qdrant"
            );
            return None;
        }
        let (scores, indices) = loaded.index.search(query, fetch_limit);
        let ids = loaded.index.resolve_ids(&indices);
        Some(ids.into_iter().map(str::to_string).zip(scores).collect())
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    fn loaded() -> LoadedIndex {
        LoadedIndex {
            index: quantizer::TurboQuantVectorIndex::new(8, 4),
            stamp: None,
        }
    }

    #[tokio::test]
    async fn install_succeeds_when_nothing_was_invalidated() {
        let slot = IndexSlot::new(None);
        let started_at = slot.generation();
        assert!(slot.install_if_current(started_at, loaded()).await);
        assert!(slot.loaded.read().await.is_some());
    }

    #[tokio::test]
    async fn a_reload_that_raced_a_write_is_not_installed() {
        let slot = IndexSlot::new(None);
        let started_at = slot.generation();
        // A write lands and invalidates while the reload verifies the corpus.
        slot.clear().await;
        assert!(!slot.install_if_current(started_at, loaded()).await);
        assert!(slot.loaded.read().await.is_none());
    }

    #[tokio::test]
    async fn invalidating_after_an_install_drops_it() {
        let slot = IndexSlot::new(None);
        assert!(slot.install_if_current(slot.generation(), loaded()).await);
        slot.clear().await;
        assert!(slot.loaded.read().await.is_none());
    }

    #[tokio::test]
    async fn every_clear_bumps_the_generation() {
        let slot = IndexSlot::new(Some(loaded()));
        let before = slot.generation();
        slot.clear().await;
        slot.clear().await;
        assert_eq!(slot.generation(), before + 2);
    }
}

#[cfg(test)]
mod turbovec_freshness_tests {
    use super::{
        compressed_index_paths, fingerprint_pairs, read_compressed_index_meta,
        save_compressed_index, turbovec_index_matches_corpus, CompressedIndexMeta,
    };
    use crate::config::AppConfig;
    use crate::engine::quantizer::TurboQuantVectorIndex;
    use std::collections::HashSet;

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn indexed(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn fingerprint(pairs: &[(&str, &str)]) -> String {
        fingerprint_pairs(pairs.iter().copied())
    }

    fn vector(dims: usize, seed: f32) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dims).map(|i| ((i as f32) + seed).sin()).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memex8-index-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A rebuilt index describes exactly the corpus it was built for.
    #[test]
    fn identical_id_sets_are_accepted() {
        assert!(turbovec_index_matches_corpus(
            &indexed(&["a", "b", "c"]),
            &live(&["a", "b", "c"])
        ));
        assert!(turbovec_index_matches_corpus(&[], &live(&[])));
    }

    /// The consolidation regression: sources `a` and `b` are deleted and summary
    /// `s` is inserted. Three vectors were live before and two after, but with a
    /// one-for-one swap the count still matches — so a count-only freshness check
    /// accepted an index that could not answer for `s` and still resolved `a`.
    #[test]
    fn replaced_ids_with_an_equal_count_are_rejected() {
        let index = indexed(&["a", "b"]);
        assert_eq!(index.len(), live(&["s", "b"]).len());
        assert!(!turbovec_index_matches_corpus(&index, &live(&["s", "b"])));
    }

    /// An index that is larger than the corpus (deleted IDs still resident) is
    /// stale even though it "covers" every live memory.
    #[test]
    fn deleted_ids_are_rejected_even_when_every_live_id_is_present() {
        assert!(!turbovec_index_matches_corpus(
            &indexed(&["a", "b", "deleted"]),
            &live(&["a", "b"])
        ));
    }

    /// Memories ingested after the last rebuild are not in the index, which is
    /// exactly the store-then-search failure.
    #[test]
    fn live_ids_missing_from_the_index_are_rejected() {
        assert!(!turbovec_index_matches_corpus(
            &indexed(&["a", "b"]),
            &live(&["a", "b", "new"])
        ));
    }

    /// Empty config paths resolve to the same defaults slumber always used, so
    /// invalidation and rebuild touch the same files.
    #[test]
    fn empty_paths_use_the_data_defaults() {
        let mut config = AppConfig::default();
        config.turbovec.index_path = String::new();
        config.turbovec.id_map_path = String::new();
        assert_eq!(
            compressed_index_paths(&config),
            (
                "data/memories.tv".to_string(),
                "data/memories_ids.json".to_string()
            )
        );
        config.turbovec.index_path = "/tmp/x.tv".into();
        config.turbovec.id_map_path = "/tmp/x-ids.json".into();
        assert_eq!(
            compressed_index_paths(&config),
            ("/tmp/x.tv".to_string(), "/tmp/x-ids.json".to_string())
        );
    }

    /// The ID set can be unchanged while every stored vector is stale — exactly
    /// what `edit_memory` leaves behind, since it rewrites the content and the
    /// vector but never `source_hash`. The fingerprint must follow the content.
    #[test]
    fn fingerprint_changes_when_content_changes_with_unchanged_ids() {
        let before = fingerprint(&[("a", "original text"), ("b", "other text")]);
        let after = fingerprint(&[("a", "edited text"), ("b", "other text")]);
        assert_eq!(live(&["a", "b"]).len(), 2);
        assert_ne!(before, after);
    }

    /// Scroll order is not part of the corpus, so it must not move the hash.
    #[test]
    fn fingerprint_is_order_independent() {
        assert_eq!(
            fingerprint(&[("a", "one"), ("b", "two")]),
            fingerprint(&[("b", "two"), ("a", "one")])
        );
    }

    /// Fields are length-prefixed, so a boundary shift cannot alias two corpora.
    #[test]
    fn fingerprint_separates_field_boundaries() {
        assert_ne!(fingerprint(&[("ab", "c")]), fingerprint(&[("a", "bc")]));
    }

    fn pair_on_disk(tag: &str, rows: &[(&str, Vec<f32>)]) -> (std::path::PathBuf, String, String) {
        let dir = tmp_dir(tag);
        let index_path = dir.join("corpus.tv").to_string_lossy().to_string();
        let id_map_path = dir.join("corpus-ids.json").to_string_lossy().to_string();
        let mut index = TurboQuantVectorIndex::new(8, 4);
        let ids: Vec<String> = rows.iter().map(|(id, _)| id.to_string()).collect();
        let vectors: Vec<Vec<f32>> = rows.iter().map(|(_, v)| v.clone()).collect();
        index.add_batch(&ids, &vectors);
        save_compressed_index(&index, &index_path, &id_map_path, "fingerprint-of-build").unwrap();
        (dir, index_path, id_map_path)
    }

    /// The pair as written validates against its own sidecar.
    #[test]
    fn sidecar_validates_the_pair_it_describes() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-ok", &rows);
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        assert_eq!(meta.vectors, 2);
        assert_eq!(meta.id_map_len, 2);
        assert_eq!(meta.fingerprint, "fingerprint-of-build");
        meta.validate_pair(&index_path, &id_map_path, &loaded, 8)
            .expect("a freshly written pair must validate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `.tv` swapped in from another corpus whose slot count matches the id map
    /// passes the length check, so the sidecar digests are what catch it.
    #[test]
    fn sidecar_rejects_a_foreign_index_of_the_same_length() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-swap", &rows);
        let (_other, other_index, _other_ids) = pair_on_disk(
            "meta-swap-foreign",
            &[("x", vector(8, 3.0)), ("y", vector(8, 4.0))],
        );
        std::fs::copy(&other_index, &index_path).unwrap();
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        assert_eq!(
            loaded.slots(),
            2,
            "same length: the count check cannot help"
        );
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        let error = meta
            .validate_pair(&index_path, &id_map_path, &loaded, 8)
            .unwrap_err()
            .to_string();
        assert!(error.contains("was replaced"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&_other);
    }

    /// A mixed pair whose lengths agree with each other but not with the sidecar
    /// is still refused.
    #[test]
    fn sidecar_rejects_a_pair_of_a_different_size() {
        let rows = [("a", vector(8, 1.0)), ("b", vector(8, 2.0))];
        let (dir, index_path, id_map_path) = pair_on_disk("meta-size", &rows);
        let (_other, other_index, other_ids) = pair_on_disk(
            "meta-size-foreign",
            &[
                ("x", vector(8, 3.0)),
                ("y", vector(8, 4.0)),
                ("z", vector(8, 5.0)),
            ],
        );
        std::fs::copy(&other_index, &index_path).unwrap();
        std::fs::copy(&other_ids, &id_map_path).unwrap();
        let loaded = TurboQuantVectorIndex::load(&index_path, &id_map_path, 8, 4).unwrap();
        assert_eq!(loaded.slots(), 3);
        let meta: CompressedIndexMeta = read_compressed_index_meta(&index_path).unwrap().unwrap();
        let error = meta
            .validate_pair(&index_path, &id_map_path, &loaded, 8)
            .unwrap_err()
            .to_string();
        assert!(error.contains("sidecar describes 2 vectors"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&_other);
    }

    /// The sidecar lives beside the pair and must be named off the index path, so
    /// invalidation and rebuild always hit the same file.
    #[test]
    fn sidecar_and_marker_paths_derive_from_the_index_path() {
        assert_eq!(
            super::compressed_index_meta_path("/data/m.tv"),
            "/data/m.tv.meta.json"
        );
        assert_eq!(
            super::compressed_index_stale_marker("/data/m.tv"),
            "/data/m.tv.stale"
        );
    }
}
